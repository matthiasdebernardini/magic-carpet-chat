//! Open a hosted Coinos wallet for a Nostr key, in one call. Ported from
//! magic-carpet-desktop and checked against coinos-server master:
//!
//! - `POST /api/register` — routes/users.ts. The body must nest the
//!   credentials under a `user` key; a flat body is rejected. `pubkey` binds
//!   the account to the nsec's public key (the server keeps a
//!   `user:<pubkey>` mapping), so Coinos nostr-login with the same key works.
//!   No captcha on register; `/login` runs one, which is why the token in the
//!   register response is never traded for a second login.
//! - `GET /.well-known/lnurlp/<username>` — a live LUD-16 endpoint the moment
//!   the account exists. Funding never needs the token: a static
//!   `lightning:LNURL1…` QR is enough for Strike or any Lightning wallet.
//!
//! - `GET /api/me` with the register token — the balance, so the app can
//!   show the sats arrive. The token is the one from register: the server
//!   signs `{id}` with no expiry, and `/api/login` sits behind a captcha, so
//!   that token is the only one this app will ever hold.
//! - `POST /api/payments` with the same token — pays a BOLT11 invoice from
//!   the wallet. The invoice comes from the recipient's own LUD-16 endpoint
//!   (`lnurl_invoice`, any domain), so one account here can pay another, or
//!   a Strike address, straight from the panel.
//!
//! ponytail: one provider, no account recovery, no NWC. Someone who outgrows
//! a custodial wallet sets a different lud16 in any Nostr client.

use std::str::FromStr as _;
use std::sync::LazyLock;

use lightning_invoice::Bolt11Invoice;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::secrets::Secret;

const BASE: &str = "https://coinos.io";

/// One client for every call: a connection pool, and the builder runs once.
/// The TLS backend is chosen at compile time, so the build cannot fail for a
/// reason a retry would fix.
static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client with the compiled-in rustls backend")
});

#[derive(thiserror::Error, Debug)]
pub enum CoinosError {
    #[error("could not reach coinos.io: {0}")]
    Http(String),
    /// Coinos answers registration failures with plain text, not JSON, so the
    /// body is passed through as-is. Also a 401 on the balance read, and a
    /// payment coinos would not make ("Insufficient funds").
    #[error("coinos refused: {0}")]
    Refused(String),
    /// The recipient's side of a send: its LNURL server was unreachable,
    /// answered something other than a pay request, or said no. Named after
    /// the recipient's domain, because coinos had no part in it.
    #[error("{0}")]
    Recipient(String),
    /// `POST /api/payments` went out and no answer came back (timeout,
    /// dropped connection). Coinos may have paid: a retry with a fresh
    /// invoice would pay twice, so the caller must re-read the balance and
    /// let the person decide.
    #[error("Payment outcome unknown; check the balance before sending again")]
    OutcomeUnknown,
}

impl CoinosError {
    /// True only when coinos answered and said no.
    ///
    /// The caller saves the login before registering, because a signup that
    /// succeeds and then fails to report back would otherwise strand an
    /// account nobody can reach. That trade only holds while the outcome is
    /// unknown — once the server has refused outright, the saved login
    /// describes nothing and should go.
    pub fn account_definitely_not_created(&self) -> bool {
        matches!(self, CoinosError::Refused(_))
    }
}

/// One account's Coinos wallet, as the store keeps it: what it takes to log
/// in to coinos.io by hand, plus the API token. The username is public (it
/// is the Lightning address); the password and token are not, so both are
/// [`Secret`]s and a `{:?}` on the whole thing stays clean. It lives on the
/// account record itself, so it can never belong to a different key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    pub username: String,
    pub password: Secret,
    /// The bearer JWT from `POST /api/register`. Empty until register
    /// answers: the login is saved BEFORE the request (see
    /// `nostr::create_coinos_wallet`), so an empty token means "the outcome
    /// of that request is unknown".
    pub token: Secret,
}

impl Login {
    pub fn fresh() -> Self {
        Self {
            username: suggest_username(),
            password: generate_password(),
            token: Secret::new(""),
        }
    }

    pub fn has_token(&self) -> bool {
        !self.token.expose().is_empty()
    }
}

pub fn lightning_address(username: &str) -> String {
    format!("{username}@coinos.io")
}

/// Coinos requires 2-24 letters or digits and lowercases the name itself
/// (lib/register.ts). A random suffix keeps signup from colliding with an
/// existing account, which returns an error rather than a login.
fn suggest_username() -> String {
    let mut rng = rand::rng();
    let suffix: String = (0..8)
        .map(|_| {
            let alphabet = b"abcdefghijkmnpqrstuvwxyz23456789";
            alphabet[rng.random_range(0..alphabet.len())] as char
        })
        .collect();
    format!("carpet{suffix}")
}

fn generate_password() -> Secret {
    let mut rng = rand::rng();
    let alphabet = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    Secret::new(
        (0..32)
            .map(|_| alphabet[rng.random_range(0..alphabet.len())] as char)
            .collect::<String>(),
    )
}

/// Register `username` bound to `pubkey_hex` and return the session token.
/// The caller must already hold the login in the store: an `Http` error here
/// leaves the outcome unknown.
pub async fn create_wallet(
    username: &str,
    password: &Secret,
    pubkey_hex: &str,
) -> Result<Secret, CoinosError> {
    let resp = CLIENT
        .post(format!("{BASE}/api/register"))
        .json(&serde_json::json!({
            "user": {
                "username": username,
                "password": password.expose(),
                "pubkey": pubkey_hex,
            }
        }))
        .send()
        .await
        .map_err(|e| CoinosError::Http(e.to_string()))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(CoinosError::Refused(body));
    }
    let registered: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CoinosError::Http(format!("unexpected signup response: {e}")))?;
    registered
        .get("token")
        .and_then(|t| t.as_str())
        .map(Secret::new)
        .ok_or_else(|| CoinosError::Http("unexpected signup response: no token".into()))
}

/// The wallet's balance in sats. A 401 is `Refused`: the token is not one
/// coinos knows, and no retry fixes that.
pub async fn balance(token: &Secret) -> Result<u64, CoinosError> {
    let resp = CLIENT
        .get(format!("{BASE}/api/me"))
        .bearer_auth(token.expose())
        .send()
        .await
        .map_err(|e| CoinosError::Http(e.to_string()))?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(CoinosError::Refused("the wallet token was not accepted".into()));
    }
    if !resp.status().is_success() {
        return Err(CoinosError::Http(format!("/api/me answered {}", resp.status())));
    }
    let me: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CoinosError::Http(format!("unexpected /api/me response: {e}")))?;
    me.get("balance")
        .and_then(|b| b.as_u64().or_else(|| b.as_f64().map(|f| f.max(0.) as u64)))
        .ok_or_else(|| CoinosError::Http("unexpected /api/me response: no balance".into()))
}

/// Does a Coinos account answer at this username? Public endpoint, no token.
/// Used on retry: a login saved before a signup that never reported back may
/// or may not name a real account, and this settles it before re-registering.
pub async fn wallet_exists(username: &str) -> Result<bool, CoinosError> {
    let resp = CLIENT
        .get(format!("{BASE}/.well-known/lnurlp/{username}"))
        .send()
        .await
        .map_err(|e| CoinosError::Http(e.to_string()))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !resp.status().is_success() {
        return Err(CoinosError::Http(format!(
            "lnurlp probe answered {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CoinosError::Http(format!("unexpected lnurlp response: {e}")))?;
    // Coinos answers 200 with `{"status":"ERROR",…}` for an unknown user.
    Ok(body.get("tag").and_then(|t| t.as_str()) == Some("payRequest"))
}

/// `user@domain`, split. `None` for anything else — an empty half, no `@`,
/// or whitespace inside a half.
pub fn split_address(lightning_address: &str) -> Option<(&str, &str)> {
    let (user, domain) = lightning_address.trim().split_once('@')?;
    if user.is_empty() || domain.is_empty() {
        return None;
    }
    if user.contains(char::is_whitespace) || domain.contains(char::is_whitespace) {
        return None;
    }
    Some((user, domain))
}

/// LUD-16: where `user@domain` publishes its pay request.
fn lnurlp_url(user: &str, domain: &str) -> String {
    format!("https://{domain}/.well-known/lnurlp/{user}")
}

/// The LUD-01 encoding of any Lightning address's pay endpoint (LUD-16 maps
/// `user@domain` to `https://domain/.well-known/lnurlp/user`). Uppercase, so
/// a QR encodes it in alphanumeric mode. `None` when `lightning_address` is
/// not `user@domain`.
pub fn lnurl_pay(lightning_address: &str) -> Option<String> {
    let (user, domain) = split_address(lightning_address)?;
    lnurl_encode(&lnurlp_url(user, domain)).ok()
}

/// LUD-06's callback takes `amount` as one more query parameter; the
/// callback may already carry some.
fn callback_with_amount(callback: &str, msat: u64) -> String {
    let separator = if callback.contains('?') { '&' } else { '?' };
    format!("{callback}{separator}amount={msat}")
}

/// A LNURL server's `{"status":"ERROR","reason":…}`, when that is what
/// `body` is.
fn lnurl_error(body: &serde_json::Value) -> Option<String> {
    if body.get("status").and_then(|s| s.as_str()) != Some("ERROR") {
        return None;
    }
    Some(
        body.get("reason")
            .and_then(|r| r.as_str())
            .filter(|r| !r.trim().is_empty())
            .unwrap_or("no reason given")
            .trim()
            .to_string(),
    )
}

/// A GET to the recipient's LNURL server, decoded. Both LUD-06 steps answer
/// JSON, and both can be `{"status":"ERROR"}` with a 200.
async fn lnurl_get(domain: &str, url: String) -> Result<serde_json::Value, CoinosError> {
    let resp = CLIENT
        .get(url)
        .send()
        .await
        .map_err(|e| CoinosError::Recipient(format!("{domain} could not be reached: {e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.map_err(|_| {
        CoinosError::Recipient(format!("{domain} answered {status} with no LNURL response"))
    })?;
    if let Some(reason) = lnurl_error(&body) {
        return Err(CoinosError::Recipient(format!("{domain} refused: {reason}")));
    }
    if !status.is_success() {
        return Err(CoinosError::Recipient(format!("{domain} answered {status}")));
    }
    Ok(body)
}

/// A BOLT11 invoice for exactly `sats`, from the recipient's own LUD-16
/// endpoint — any domain, not only coinos (Strike, Primal, …). LUD-06: read
/// the pay request, check the amount is inside its range, ask the callback.
/// `Recipient` for everything the recipient's server did wrong; the caller
/// keeps the sats count and re-checks it against the invoice before paying.
pub async fn lnurl_invoice(lightning_address: &str, sats: u64) -> Result<String, CoinosError> {
    let (user, domain) = split_address(lightning_address).ok_or_else(|| {
        CoinosError::Recipient(format!(
            "{} is not a Lightning address (name@domain.com)",
            lightning_address.trim()
        ))
    })?;
    let pay = lnurl_get(domain, lnurlp_url(user, domain)).await?;
    if pay.get("tag").and_then(|t| t.as_str()) != Some("payRequest") {
        return Err(CoinosError::Recipient(format!(
            "{domain} has no Lightning address named {user}"
        )));
    }
    let msat = sats.saturating_mul(1000);
    let min = pay.get("minSendable").and_then(|v| v.as_u64()).unwrap_or(1);
    let max = pay.get("maxSendable").and_then(|v| v.as_u64()).unwrap_or(u64::MAX);
    if msat < min || msat > max {
        return Err(CoinosError::Recipient(format!(
            "{domain} accepts {} to {} sats per payment",
            min.div_ceil(1000),
            max / 1000
        )));
    }
    let callback = pay
        .get("callback")
        .and_then(|c| c.as_str())
        .filter(|c| c.starts_with("https://"))
        .ok_or_else(|| {
            CoinosError::Recipient(format!("{domain} gave no callback for the invoice"))
        })?;
    let invoice = lnurl_get(domain, callback_with_amount(callback, msat)).await?;
    invoice
        .get("pr")
        .and_then(|pr| pr.as_str())
        .map(|pr| pr.trim().to_string())
        .filter(|pr| !pr.is_empty())
        .ok_or_else(|| CoinosError::Recipient(format!("{domain} returned no invoice")))
}

/// `bolt11` parsed, and its amount checked against what was asked for. The
/// invoice came from the recipient's server, so the wallet never trusts its
/// amount: one that asks for more (or is open-ended) is refused here, before
/// coinos ever sees it.
fn invoice_for(bolt11: &str, sats: u64) -> Result<Bolt11Invoice, CoinosError> {
    let invoice = Bolt11Invoice::from_str(bolt11.trim())
        .map_err(|e| CoinosError::Refused(format!("the invoice could not be read: {e}")))?;
    match invoice.amount_milli_satoshis() {
        Some(msat) if msat == sats.saturating_mul(1000) => Ok(invoice),
        Some(msat) => Err(CoinosError::Refused(format!(
            "the invoice asks for {} sats, not {sats}",
            msat.div_ceil(1000)
        ))),
        None => Err(CoinosError::Refused(
            "the invoice names no amount, so it was not paid".into(),
        )),
    }
}

/// Coinos truncates nothing, and a proxy error page can be long; the panel
/// shows this text.
const REFUSAL_TEXT_CAP: usize = 200;

/// A Lightning payment can take a while to settle through the network, and
/// coinos answers only once it has; the client's 30 s default would give up
/// on payments that go through.
const PAYMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Pay `bolt11` from the wallet `token` opens, after checking the invoice
/// asks for exactly `sats`. `Ok` once coinos accepted the payment; a 401 is
/// the token, any other refusal is coinos's own text ("Insufficient funds").
/// `Http` means the request never reached coinos; once it has, no answer is
/// [`CoinosError::OutcomeUnknown`], never a plain failure.
pub async fn pay_invoice(token: &Secret, bolt11: &str, sats: u64) -> Result<(), CoinosError> {
    let invoice = invoice_for(bolt11, sats)?;
    let resp = CLIENT
        .post(format!("{BASE}/api/payments"))
        .bearer_auth(token.expose())
        .timeout(PAYMENT_TIMEOUT)
        .json(&serde_json::json!({ "payreq": invoice.to_string() }))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                CoinosError::Http(e.to_string())
            } else {
                CoinosError::OutcomeUnknown
            }
        })?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    Err(payment_refusal(status, &body))
}

/// A non-2xx answer to `POST /api/payments`. A 5xx is a coinos or proxy
/// failure that may have come after the payment left, so it is
/// [`CoinosError::OutcomeUnknown`]; only a 4xx is a refusal.
fn payment_refusal(status: reqwest::StatusCode, body: &str) -> CoinosError {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return CoinosError::Refused("the wallet token was not accepted".into());
    }
    if status.is_server_error() {
        return CoinosError::OutcomeUnknown;
    }
    let body = body.trim();
    let reason = match body.char_indices().nth(REFUSAL_TEXT_CAP) {
        Some((cut, _)) => format!("{}…", &body[..cut]),
        None if body.is_empty() => "payment not accepted".to_string(),
        None => body.to_string(),
    };
    CoinosError::Refused(reason)
}

/// bech32 with HRP `lnurl`. The original checksum, NOT bech32m: LUD-01
/// predates BIP-350 and every wallet decodes it that way.
fn lnurl_encode(url: &str) -> Result<String, bech32::EncodeError> {
    let hrp = bech32::Hrp::parse_unchecked("lnurl");
    bech32::encode_upper::<bech32::Bech32>(hrp, url.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lnurl_matches_the_lud01_test_vector() {
        // The vector from the LUD-01 spec text.
        let url = "https://service.com/api?q=3fc3645b439ce8e7f2553a69e5267081d96dcd340693afabe04be7b0ccd178df";
        assert_eq!(
            lnurl_encode(url).unwrap(),
            "LNURL1DP68GURN8GHJ7UM9WFMXJCM99E3K7MF0V9CXJ0M385EKVCENXC6R2C35XVUKXEFCV5MKVV34X5EKZD3EV56NYD3HXQURZEPEXEJXXEPNXSCRVWFNV9NXZCN9XQ6XYEFHVGCXXCMYXYMNSERXFQ5FNS"
        );
    }

    #[test]
    fn a_lightning_address_becomes_its_lnurlp_endpoint() {
        let lnurl = lnurl_pay("carpetab12cd34@coinos.io").unwrap();
        assert!(lnurl.starts_with("LNURL1"));
        assert_eq!(lnurl, lnurl.to_ascii_uppercase());
        let expected =
            lnurl_encode("https://coinos.io/.well-known/lnurlp/carpetab12cd34").unwrap();
        assert_eq!(lnurl, expected);

        assert_eq!(lnurl_pay("not-an-address"), None);
        assert_eq!(lnurl_pay("@coinos.io"), None);
        assert_eq!(lnurl_pay("user@"), None);
        assert_eq!(split_address("  m@strike.me "), Some(("m", "strike.me")));
        assert_eq!(split_address("a b@strike.me"), None);
    }

    #[test]
    fn the_callback_gets_amount_as_one_more_query_parameter() {
        assert_eq!(
            callback_with_amount("https://coinos.io/api/lnurlp/carpet", 21_000),
            "https://coinos.io/api/lnurlp/carpet?amount=21000"
        );
        assert_eq!(
            callback_with_amount("https://strike.me/pay?user=m", 21_000),
            "https://strike.me/pay?user=m&amount=21000"
        );
    }

    #[test]
    fn an_invoice_for_a_different_amount_is_not_paid() {
        // BOLT 11's own "coffee beans" vector, as lightning-invoice tests
        // it: 25 mBTC = 2 500 000 sats, with a payment secret so it parses.
        let bolt11 = "lnbc25m1pvjluezpp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5vdhkven9v5sxyetpdeessp5zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zyg3zygs9q5sqqqqqqqqqqqqqqqpqsq67gye39hfg3zd8rgc80k32tvy9xk2xunwm5lzexnvpx6fd77en8qaq424dxgt56cag2dpt359k3ssyhetktkpqh24jqnjyw6uqd08sgptq44qu";
        assert!(invoice_for(bolt11, 2_500_000).is_ok());
        match invoice_for(bolt11, 21) {
            Err(CoinosError::Refused(reason)) => {
                assert_eq!(reason, "the invoice asks for 2500000 sats, not 21")
            }
            other => panic!("a wrong amount must be refused: {other:?}"),
        }
        assert!(matches!(
            invoice_for("lnbc1notaninvoice", 21),
            Err(CoinosError::Refused(_))
        ));
    }

    #[test]
    fn a_payment_5xx_is_outcome_unknown_and_a_4xx_is_a_refusal() {
        use reqwest::StatusCode;
        // A 502 can come from a proxy after coinos already paid.
        assert!(matches!(
            payment_refusal(StatusCode::BAD_GATEWAY, "<html>Bad Gateway</html>"),
            CoinosError::OutcomeUnknown
        ));
        match payment_refusal(StatusCode::BAD_REQUEST, " Insufficient funds ") {
            CoinosError::Refused(reason) => assert_eq!(reason, "Insufficient funds"),
            other => panic!("a 400 must be a refusal: {other:?}"),
        }
        assert!(matches!(
            payment_refusal(StatusCode::UNAUTHORIZED, ""),
            CoinosError::Refused(_)
        ));
    }

    #[test]
    fn a_lnurl_error_body_is_its_reason() {
        let err = serde_json::json!({"status": "ERROR", "reason": "Not found"});
        assert_eq!(lnurl_error(&err).as_deref(), Some("Not found"));
        let bare = serde_json::json!({"status": "ERROR"});
        assert_eq!(lnurl_error(&bare).as_deref(), Some("no reason given"));
        let ok = serde_json::json!({"tag": "payRequest"});
        assert_eq!(lnurl_error(&ok), None);
    }

    #[test]
    fn only_an_outright_refusal_means_there_is_no_account() {
        // The saved login is only safe to delete when we know no account was
        // created. Getting this wrong in either direction either strands an
        // account or throws away the password to a real one.
        assert!(
            CoinosError::Refused("username taken".into()).account_definitely_not_created(),
            "coinos answered and said no, so nothing was created"
        );
        assert!(
            !CoinosError::Http("connection reset".into()).account_definitely_not_created(),
            "a dropped connection may still have created the account"
        );
    }

    #[test]
    fn suggested_username_passes_the_servers_own_rule() {
        // lib/register.ts — letters and digits only, 2 to 24 characters.
        for _ in 0..50 {
            let name = suggest_username();
            assert!(name.len() >= 2 && name.len() <= 24, "bad length: {name}");
            assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric()),
                "bad characters: {name}"
            );
            assert!(!name.contains("undefined"));
        }
    }

    #[test]
    fn a_login_round_trips_through_the_store_shape_without_printing() {
        let login = Login {
            username: "carpetabcd1234".into(),
            password: generate_password(),
            token: Secret::new("eyJhbGciOiJIUzI1NiJ9.payload.signature"),
        };
        let printed = format!("{login:?}");
        assert!(!printed.contains("eyJ"), "token leaked: {printed}");
        assert!(!printed.contains(login.password.expose()), "password leaked");
        assert!(printed.contains("carpetabcd1234"), "the username is public");

        let json = serde_json::to_string(&login).unwrap();
        let decoded: Login = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, login);
        assert!(decoded.has_token());
        assert!(!Login::fresh().has_token(), "a login saved before register has no token yet");
        // Every field is required: a partial entry is unreadable, not half a login.
        assert!(serde_json::from_str::<Login>(r#"{"username":"x","password":"y"}"#).is_err());
    }
}
