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
//! ponytail: one provider, no account recovery, no balance display (that
//! needs the token), no NWC (this app never pays). Someone who outgrows a
//! custodial wallet sets a different lud16 in any Nostr client.

use rand::Rng;

use crate::secrets::Secret;

const BASE: &str = "https://coinos.io";
const USER_AGENT: &str = concat!("magic-carpet-chat/", env!("CARGO_PKG_VERSION"));

#[derive(thiserror::Error, Debug)]
pub enum CoinosError {
    #[error("could not reach coinos.io: {0}")]
    Http(String),
    /// Coinos answers registration failures with plain text, not JSON, so the
    /// body is passed through as-is.
    #[error("coinos refused the signup: {0}")]
    Refused(String),
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

/// The keychain payload: what it takes to log in to coinos.io by hand. The
/// username is public (it is the Lightning address), the password is not, so
/// the whole thing only ever travels as one [`Secret`]. `pubkey` (hex) names
/// the key the account was bound to: the keychain entry is per account slot,
/// not per key, so a re-imported nsec must not inherit the old key's wallet.
pub struct Login {
    pub username: String,
    pub password: Secret,
    pub pubkey: String,
}

impl Login {
    pub fn fresh(pubkey_hex: &str) -> Self {
        Self {
            username: suggest_username(),
            password: generate_password(),
            pubkey: pubkey_hex.to_string(),
        }
    }

    /// The login to use for `pubkey_hex`, and whether it is new. A stored
    /// login for a different key is ignored, not deleted here: the caller
    /// overwrites the entry when it stores the fresh one.
    pub fn reuse_or_fresh(stored: Option<Self>, pubkey_hex: &str) -> (Self, bool) {
        match stored {
            Some(login) if login.pubkey == pubkey_hex => (login, false),
            _ => (Self::fresh(pubkey_hex), true),
        }
    }

    pub fn encode(&self) -> Secret {
        Secret::new(
            serde_json::json!({
                "username": self.username,
                "password": self.password.expose(),
                "pubkey": self.pubkey,
            })
            .to_string(),
        )
    }

    /// `None` for a keychain entry this app did not write.
    pub fn decode(secret: &Secret) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(secret.expose()).ok()?;
        let field = |key: &str| value.get(key)?.as_str().map(str::to_string);
        Some(Self {
            username: field("username")?,
            password: Secret::new(field("password")?),
            pubkey: field("pubkey")?,
        })
    }
}

pub struct NewWallet {
    pub username: String,
    pub lightning_address: String,
}

pub fn lightning_address(username: &str) -> String {
    format!("{username}@coinos.io")
}

/// Coinos requires 2-24 letters or digits and lowercases the name itself
/// (lib/register.ts). A random suffix keeps signup from colliding with an
/// existing account, which returns an error rather than a login.
pub fn suggest_username() -> String {
    let mut rng = rand::rng();
    let suffix: String = (0..8)
        .map(|_| {
            let alphabet = b"abcdefghijkmnpqrstuvwxyz23456789";
            alphabet[rng.random_range(0..alphabet.len())] as char
        })
        .collect();
    format!("carpet{suffix}")
}

pub fn generate_password() -> Secret {
    let mut rng = rand::rng();
    let alphabet = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    Secret::new(
        (0..32)
            .map(|_| alphabet[rng.random_range(0..alphabet.len())] as char)
            .collect::<String>(),
    )
}

fn http() -> Result<reqwest::Client, CoinosError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| CoinosError::Http(e.to_string()))
}

/// Register `username` bound to `pubkey_hex`. The caller must already hold
/// the login in the keychain: an `Http` error here leaves the outcome unknown.
pub async fn create_wallet(
    username: &str,
    password: &Secret,
    pubkey_hex: &str,
) -> Result<NewWallet, CoinosError> {
    let resp = http()?
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
    // The response carries a session token. It is a bearer secret this app
    // has no use for, so it is checked for shape and dropped, never stored.
    let registered: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| CoinosError::Http(format!("unexpected signup response: {e}")))?;
    if registered.get("token").and_then(|t| t.as_str()).is_none() {
        return Err(CoinosError::Http(
            "unexpected signup response: no token".into(),
        ));
    }

    Ok(NewWallet {
        username: username.to_string(),
        lightning_address: lightning_address(username),
    })
}

/// Does a Coinos account answer at this username? Public endpoint, no token.
/// Used on retry: a login saved before a signup that never reported back may
/// or may not name a real account, and this settles it before re-registering.
pub async fn wallet_exists(username: &str) -> Result<bool, CoinosError> {
    let resp = http()?
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

/// The LUD-01 encoding of any Lightning address's pay endpoint (LUD-16 maps
/// `user@domain` to `https://domain/.well-known/lnurlp/user`). Uppercase, so
/// a QR encodes it in alphanumeric mode. `None` when `lightning_address` is
/// not `user@domain`.
pub fn lnurl_pay(lightning_address: &str) -> Option<String> {
    let (user, domain) = lightning_address.trim().split_once('@')?;
    if user.is_empty() || domain.is_empty() {
        return None;
    }
    lnurl_encode(&format!("https://{domain}/.well-known/lnurlp/{user}")).ok()
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
    fn a_login_round_trips_through_the_keychain_shape_without_printing() {
        let login = Login {
            username: "carpetabcd1234".into(),
            password: generate_password(),
            pubkey: "aa".repeat(32),
        };
        let encoded = login.encode();
        assert_eq!(format!("{encoded:?}"), "Secret(<redacted>)");
        let decoded = Login::decode(&encoded).unwrap();
        assert_eq!(decoded.username, login.username);
        assert_eq!(decoded.password, login.password);
        assert_eq!(decoded.pubkey, login.pubkey);
        assert!(Login::decode(&Secret::new("not json")).is_none());
        assert!(Login::decode(&Secret::new(r#"{"username":"x"}"#)).is_none());
        // An entry from before `pubkey` was recorded is unreadable, not
        // silently bound to whatever key is active now.
        assert!(Login::decode(&Secret::new(r#"{"username":"x","password":"y"}"#)).is_none());
    }

    #[test]
    fn a_login_for_one_key_is_never_reused_for_another() {
        // Forget the nsec, paste a different one: the account slot keeps its
        // keychain entry, but that wallet belongs to the old key.
        let key_a = "aa".repeat(32);
        let key_b = "bb".repeat(32);
        let stored = Login::decode(&Login::fresh(&key_a).encode()).unwrap();
        let old_username = stored.username.clone();

        let (login, fresh) = Login::reuse_or_fresh(Some(stored), &key_b);
        assert!(fresh);
        assert_eq!(login.pubkey, key_b);
        assert_ne!(login.username, old_username);

        let stored = Login::decode(&Login::fresh(&key_a).encode()).unwrap();
        let old_username = stored.username.clone();
        let (login, fresh) = Login::reuse_or_fresh(Some(stored), &key_a);
        assert!(!fresh);
        assert_eq!(login.username, old_username);

        let (_, fresh) = Login::reuse_or_fresh(None, &key_a);
        assert!(fresh);
    }
}
