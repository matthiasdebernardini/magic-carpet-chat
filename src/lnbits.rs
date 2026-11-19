//! A wallet on a self-hosted LNbits server, the second wallet service next
//! to Coinos. `MC_WALLET_SERVER` (an `https://host` base URL) picks it when
//! a wallet is created; after that the stored [`Login`]'s `server` routes
//! every call here (see `coinos::wallet_balance`, `coinos::wallet_pay`).
//! Checked against LNbits v1.7.1 with the LNURLp "Pay Links" extension:
//!
//! - `POST /api/v1/account` — a new account with one wallet, no auth. The
//!   answer carries the wallet's `adminkey`, which is all this app keeps.
//! - `POST /lnurlp/api/v1/links` with the admin key — the Lightning address
//!   `<username>@<host>`, with `zaps` on so the server sends receipts.
//! - `GET /.well-known/lnurlp/<username>` — does that address answer.
//! - `GET /api/v1/wallet` — the balance, in millisats.
//! - `POST /api/v1/payments` — pays a BOLT11 invoice from the wallet.
//!
//! The server lives at the root of its host: LUD-16 resolves an address at
//! `https://<host>/.well-known/lnurlp/…`, so a server under a path could
//! never answer for its own addresses.
//!
//! [`Login`]: crate::coinos::Login

use serde_json::Value;

use crate::coinos::{self, CoinosError, PAYMENT_TIMEOUT, REFUSAL_TEXT_CAP};
use crate::diag;
use crate::secrets::Secret;

/// This module's own client. The wallet key travels in `X-Api-Key`, a header
/// reqwest keeps when it follows a redirect to another host or to plain
/// http, so redirects are never followed and only https goes out.
static CLIENT: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .build()
        .expect("reqwest client with the compiled-in rustls backend")
});

/// The variable that puts new wallets on an LNbits server.
pub const SERVER_ENV: &str = "MC_WALLET_SERVER";

/// `MC_WALLET_SERVER` is set but is not a server this app can use. Fixed
/// wording: the value is never echoed.
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
#[error(
    "MC_WALLET_SERVER must be an https:// address with only a host, like https://wallet.example. No wallet was created."
)]
pub struct BadServer;

/// The wallet server new wallets go to: `None` when `MC_WALLET_SERVER` is
/// unset or blank (Coinos), the normalized base URL when it is valid. An
/// invalid value is an error, never a quiet fall back to Coinos.
pub fn server_from_env() -> Result<Option<String>, BadServer> {
    match std::env::var(SERVER_ENV) {
        Ok(raw) if !raw.trim().is_empty() => parse_server(&raw).map(Some),
        _ => Ok(None),
    }
}

/// The host a new wallet will be opened on, for the panel's wording: `None`
/// means Coinos (or a bad `MC_WALLET_SERVER`, which the signup reports).
pub fn signup_host() -> Option<String> {
    server_from_env()
        .ok()
        .flatten()
        .map(|server| host_of(&server))
}

/// `https://host`, from what the person set: https only, the default port, a host, and
/// nothing else (no path, query, fragment or credentials). A trailing slash
/// is dropped.
pub fn parse_server(raw: &str) -> Result<String, BadServer> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| BadServer)?;
    let plain = url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    match authority(&url) {
        Some(authority) if plain => Ok(format!("https://{authority}")),
        _ => Err(BadServer),
    }
}

/// `host` or `host:port`; the default port is left out.
fn authority(url: &reqwest::Url) -> Option<String> {
    let host = url.host_str().filter(|h| !h.is_empty())?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// The host part of a stored server URL: the domain of its Lightning
/// addresses, and the name its errors go by.
pub fn host_of(server: &str) -> String {
    reqwest::Url::parse(server)
        .ok()
        .as_ref()
        .and_then(authority)
        .unwrap_or_else(|| server.to_string())
}

fn http_error(server: &str, error: impl std::fmt::Display) -> CoinosError {
    CoinosError::ServerHttp(host_of(server), error.to_string())
}

fn refused(server: &str, detail: impl Into<String>) -> CoinosError {
    CoinosError::ServerRefused(host_of(server), detail.into())
}

/// LNbits says why in `{"detail": …}`; anything else (a proxy page) is
/// passed through, capped, as the panel shows it.
fn detail_of(body: &str) -> String {
    if let Some(detail) = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
    {
        return detail.trim().to_string();
    }
    let body = body.trim();
    match body.char_indices().nth(REFUSAL_TEXT_CAP) {
        Some((cut, _)) => format!("{}…", &body[..cut]),
        None => body.to_string(),
    }
}

const KEY_REFUSED: &str = "the wallet key was not accepted";

/// Step 1 of a signup: a new LNbits account named `username`, and its
/// wallet's admin key. The caller must already hold the login in the store.
/// A 4xx is `ServerRefused` (sign-ups off): nothing was created.
pub async fn create_account(server: &str, username: &str) -> Result<Secret, CoinosError> {
    let resp = CLIENT
        .post(format!("{server}/api/v1/account"))
        .json(&serde_json::json!({ "name": username }))
        .send()
        .await
        .map_err(|e| http_error(server, e))?;
    let status = resp.status();
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        return Err(refused(server, detail_of(&body)));
    }
    if !status.is_success() {
        return Err(http_error(
            server,
            format!("/api/v1/account answered {status}"),
        ));
    }
    let wallet: Value = resp
        .json()
        .await
        .map_err(|e| http_error(server, format!("unexpected signup response: {e}")))?;
    wallet
        .get("adminkey")
        .and_then(|k| k.as_str())
        .filter(|k| !k.is_empty())
        .map(Secret::new)
        .ok_or_else(|| http_error(server, "unexpected signup response: no admin key"))
}

/// What `POST /lnurlp/api/v1/links` did with a username.
#[derive(Debug, PartialEq, Eq)]
pub enum NewAddress {
    Created,
    /// 409: some link on the server already has that username. It may be
    /// this wallet's own; [`own_usernames`] tells.
    Taken,
}

/// Step 2 of a signup: the Lightning address `username@host`, zaps on.
pub async fn create_address(
    server: &str,
    key: &Secret,
    username: &str,
) -> Result<NewAddress, CoinosError> {
    let resp = CLIENT
        .post(format!("{server}/lnurlp/api/v1/links"))
        .header("X-Api-Key", key.expose())
        .json(&serde_json::json!({
            "description": "Magic Carpet",
            "min": 1,
            "max": 1_000_000,
            "comment_chars": 0,
            "username": username,
            "zaps": true,
        }))
        .send()
        .await
        .map_err(|e| http_error(server, e))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(NewAddress::Created);
    }
    if status == reqwest::StatusCode::CONFLICT {
        return Ok(NewAddress::Taken);
    }
    if matches!(status.as_u16(), 401 | 404) {
        return Err(refused(server, KEY_REFUSED));
    }
    if status.is_client_error() {
        let body = resp.text().await.unwrap_or_default();
        return Err(refused(server, detail_of(&body)));
    }
    Err(http_error(
        server,
        format!("/lnurlp/api/v1/links answered {status}"),
    ))
}

/// The usernames of this wallet's own pay links.
pub async fn own_usernames(server: &str, key: &Secret) -> Result<Vec<String>, CoinosError> {
    let resp = CLIENT
        .get(format!("{server}/lnurlp/api/v1/links"))
        .header("X-Api-Key", key.expose())
        .send()
        .await
        .map_err(|e| http_error(server, e))?;
    let status = resp.status();
    if matches!(status.as_u16(), 401 | 404) {
        return Err(refused(server, KEY_REFUSED));
    }
    if !status.is_success() {
        return Err(http_error(
            server,
            format!("/lnurlp/api/v1/links answered {status}"),
        ));
    }
    let links: Value = resp
        .json()
        .await
        .map_err(|e| http_error(server, format!("unexpected links response: {e}")))?;
    Ok(usernames_of(&links))
}

fn usernames_of(links: &Value) -> Vec<String> {
    links
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|link| link.get("username").and_then(|u| u.as_str()))
        .map(str::to_string)
        .collect()
}

/// Does `username@host` answer with a pay request? Public, no key.
pub async fn address_exists(server: &str, username: &str) -> Result<bool, CoinosError> {
    let resp = CLIENT
        .get(format!("{server}/.well-known/lnurlp/{username}"))
        .send()
        .await
        .map_err(|e| http_error(server, e))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !status.is_success() {
        return Err(http_error(
            server,
            format!("lnurlp probe answered {status}"),
        ));
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|e| http_error(server, format!("unexpected lnurlp response: {e}")))?;
    // 200 with `{"status":"ERROR",…}` for an unknown username.
    Ok(body.get("tag").and_then(|t| t.as_str()) == Some("payRequest"))
}

/// The wallet's balance in sats. An unknown key is `ServerRefused`.
pub async fn balance(server: &str, key: &Secret) -> Result<u64, CoinosError> {
    let resp = CLIENT
        .get(format!("{server}/api/v1/wallet"))
        .header("X-Api-Key", key.expose())
        .send()
        .await
        .map_err(|e| http_error(server, e))?;
    let status = resp.status();
    if matches!(status.as_u16(), 401 | 403 | 404) {
        return Err(refused(server, KEY_REFUSED));
    }
    if !status.is_success() {
        return Err(http_error(
            server,
            format!("/api/v1/wallet answered {status}"),
        ));
    }
    let wallet: Value = resp
        .json()
        .await
        .map_err(|e| http_error(server, format!("unexpected /api/v1/wallet response: {e}")))?;
    sats_of(&wallet)
        .ok_or_else(|| http_error(server, "unexpected /api/v1/wallet response: no balance"))
}

/// LNbits counts in millisats; a part sat cannot be spent, so it is floored.
fn sats_of(wallet: &Value) -> Option<u64> {
    let msat = wallet.get("balance")?;
    msat.as_u64()
        .or_else(|| msat.as_f64().map(|f| f.max(0.) as u64))
        .map(|msat| msat / 1000)
}

/// Pay `bolt11` from the wallet `key` opens, after the same invoice checks
/// as Coinos: exactly `sats`, not about to expire. `ServerHttp` means the
/// request never reached the server; once it has, no answer is
/// [`CoinosError::OutcomeUnknown`].
pub async fn pay_invoice(
    server: &str,
    key: &Secret,
    bolt11: &str,
    sats: u64,
) -> Result<(), CoinosError> {
    let invoice = coinos::payable_invoice(bolt11, sats, coinos::now()).map_err(|e| match e {
        CoinosError::Refused(reason) => refused(server, reason),
        other => other,
    })?;
    let resp = CLIENT
        .post(format!("{server}/api/v1/payments"))
        .header("X-Api-Key", key.expose())
        .timeout(PAYMENT_TIMEOUT)
        .json(&serde_json::json!({ "out": true, "bolt11": invoice.to_string() }))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                http_error(server, e)
            } else {
                diag::note(format!(
                    "wallet server payment gave no answer: {}",
                    e.without_url()
                ));
                CoinosError::OutcomeUnknown
            }
        })?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    payment_answer(server, status, &body)
}

/// The answer to `POST /api/v1/payments`. A body with `"status":"failed"`
/// is a refusal whatever the HTTP status (LNbits sends it with a 520). A
/// 2xx is paid only when it says `"success"`: `"pending"` may still settle.
/// Any other 5xx may come from the proxy in front after the payment left,
/// so it is outcome-unknown; a 4xx is a refusal.
fn payment_answer(
    server: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> Result<(), CoinosError> {
    let state = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string));
    if state.as_deref() == Some("failed") {
        let detail = detail_of(body);
        return Err(refused(
            server,
            if detail.is_empty() {
                "payment failed".to_string()
            } else {
                detail
            },
        ));
    }
    if status.is_success() {
        return match state.as_deref() {
            Some("success") => Ok(()),
            _ => Err(CoinosError::OutcomeUnknown),
        };
    }
    if status.is_server_error() {
        return Err(CoinosError::OutcomeUnknown);
    }
    if matches!(status.as_u16(), 401 | 404) {
        return Err(refused(server, KEY_REFUSED));
    }
    let detail = detail_of(body);
    Err(refused(
        server,
        if detail.is_empty() {
            "payment not accepted".to_string()
        } else {
            detail
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    const SERVER: &str = "https://wallet.example";

    #[test]
    fn the_balance_is_millisats_floored_to_sats() {
        assert_eq!(
            sats_of(&serde_json::json!({"name": "w", "balance": 0})),
            Some(0)
        );
        assert_eq!(sats_of(&serde_json::json!({"balance": 21_999})), Some(21));
        assert_eq!(sats_of(&serde_json::json!({"balance": 250_000})), Some(250));
        assert_eq!(sats_of(&serde_json::json!({"balance": 999})), Some(0));
        assert_eq!(sats_of(&serde_json::json!({"balance": -5})), Some(0));
        assert_eq!(sats_of(&serde_json::json!({"name": "w"})), None);
    }

    #[test]
    fn a_payment_answer_is_paid_refused_or_unknown() {
        assert!(payment_answer(SERVER, StatusCode::CREATED, r#"{"status":"success"}"#).is_ok());
        assert!(matches!(
            payment_answer(SERVER, StatusCode::CREATED, r#"{"status":"pending"}"#),
            Err(CoinosError::OutcomeUnknown)
        ));
        // A 2xx that does not say success has not been shown to be paid.
        assert!(matches!(
            payment_answer(SERVER, StatusCode::CREATED, ""),
            Err(CoinosError::OutcomeUnknown)
        ));
        let failed = r#"{"detail":"Insufficient balance.","status":"failed"}"#;
        match payment_answer(SERVER, StatusCode::from_u16(520).unwrap(), failed) {
            Err(e @ CoinosError::ServerRefused(..)) => {
                assert_eq!(
                    e.to_string(),
                    "wallet.example said no (Insufficient balance)."
                );
                assert!(!e.to_string().contains("Coinos"));
            }
            other => panic!("a failed body is a refusal: {other:?}"),
        }
        assert!(matches!(
            payment_answer(SERVER, StatusCode::BAD_GATEWAY, "<html>Bad Gateway</html>"),
            Err(CoinosError::OutcomeUnknown)
        ));
        assert!(matches!(
            payment_answer(SERVER, StatusCode::from_u16(520).unwrap(), "oops"),
            Err(CoinosError::OutcomeUnknown)
        ));
        for status in [StatusCode::NOT_FOUND, StatusCode::UNAUTHORIZED] {
            match payment_answer(SERVER, status, r#"{"detail":"Wallet not found."}"#) {
                Err(CoinosError::ServerRefused(host, detail)) => {
                    assert_eq!(host, "wallet.example");
                    assert_eq!(detail, KEY_REFUSED);
                }
                other => panic!("an unknown key is a refusal: {other:?}"),
            }
        }
    }

    #[test]
    fn the_address_domain_is_the_servers_host() {
        assert_eq!(host_of("https://wallet.example"), "wallet.example");
        assert_eq!(
            host_of("https://wallet.example:8443"),
            "wallet.example:8443"
        );
        let login = coinos::Login::fresh_on("https://wallet.example".into());
        assert_eq!(
            login.lightning_address(),
            format!("{}@wallet.example", login.username)
        );
        assert_eq!(login.server_host().as_deref(), Some("wallet.example"));
        assert!(login.password.expose().is_empty() && !login.has_token());
        let coinos = coinos::Login::fresh();
        assert_eq!(
            coinos.lightning_address(),
            format!("{}@coinos.io", coinos.username)
        );
        assert_eq!(coinos.server_host(), None);
    }

    #[test]
    fn only_a_bare_https_host_is_a_wallet_server() {
        assert_eq!(
            parse_server("https://wallet.example").as_deref(),
            Ok("https://wallet.example")
        );
        assert_eq!(
            parse_server(" https://wallet.example/ ").as_deref(),
            Ok("https://wallet.example")
        );
        // A port would end up in the Lightning address, which LUD-16 has no
        // room for.
        assert_eq!(parse_server("https://wallet.example:8443/"), Err(BadServer));
        assert_eq!(
            parse_server("https://wallet.example:443").as_deref(),
            Ok("https://wallet.example")
        );
        for bad in [
            "http://wallet.example",
            "wallet.example",
            "https://",
            "https://wallet.example/lnbits",
            "https://wallet.example/?x=1",
            "https://wallet.example/#top",
            "https://user:pw@wallet.example",
            "ftp://wallet.example",
            "",
        ] {
            assert_eq!(parse_server(bad), Err(BadServer), "{bad:?}");
        }
        assert!(!BadServer.to_string().contains("Coinos"));
    }

    #[test]
    fn a_login_without_a_server_is_coinos_and_one_with_it_round_trips() {
        let old = r#"{"username":"carpetab12cd34","password":"pw","token":"tok"}"#;
        let login: coinos::Login = serde_json::from_str(old).unwrap();
        assert_eq!(login.server, None);
        assert_eq!(login.lightning_address(), "carpetab12cd34@coinos.io");
        // A Coinos login writes the same JSON it always did.
        assert!(!serde_json::to_string(&login).unwrap().contains("server"));

        let mut on_server = coinos::Login::fresh_on("https://wallet.example".into());
        on_server.token = Secret::new("adminkey123");
        let json = serde_json::to_string(&on_server).unwrap();
        assert!(
            json.contains(r#""server":"https://wallet.example""#),
            "{json}"
        );
        let decoded: coinos::Login = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, on_server);
        assert!(!format!("{decoded:?}").contains("adminkey123"));
    }

    #[test]
    fn own_links_are_read_by_username() {
        let links = serde_json::json!([
            {"id": "a", "username": "carpetab12cd34"},
            {"id": "b", "username": null},
            {"id": "c"},
        ]);
        assert_eq!(usernames_of(&links), vec!["carpetab12cd34".to_string()]);
        assert!(usernames_of(&serde_json::json!({"detail": "x"})).is_empty());
    }
}
