//! Secret storage. The two nsecs are bearer credentials: they live in the OS
//! keychain (or an env override for automated runs) and nowhere else.
//!
//! Every secret leaves this module wrapped in [`Secret`], which has a `Debug`
//! impl that prints a placeholder. That is the whole point — a stray `{:?}`
//! anywhere in the app, or a secret embedded in an error chain, cannot leak
//! the value into a log line.

use keyring::Entry;
use nostr_sdk::prelude::*;

const SERVICE: &str = "magic-carpet-chat";

/// The two identities the demo drives. Each has its own keychain entry, its
/// own env override, and (in `api::Api`) its own cookie jar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Account {
    /// The house: posts the DList and the auto-pay bounty.
    Issuer,
    /// Matthias: claims an item and gets paid.
    Claimant,
}

impl Account {
    pub const ALL: [Account; 2] = [Account::Issuer, Account::Claimant];

    fn entry_key(self) -> &'static str {
        match self {
            Account::Issuer => "issuer-nsec",
            Account::Claimant => "claimant-nsec",
        }
    }

    /// Env override, checked before the keychain so automated runs never need
    /// an unlocked credential store.
    pub fn env_var(self) -> &'static str {
        match self {
            Account::Issuer => "MC_ISSUER_NSEC",
            Account::Claimant => "MC_CLAIMANT_NSEC",
        }
    }
}

impl std::fmt::Display for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Account::Issuer => "issuer",
            Account::Claimant => "claimant",
        })
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SecretError {
    #[error("OS keychain unavailable: {0}")]
    Keyring(String),
    /// Deliberately does not carry the parser's error: a bech32 error can echo
    /// pieces of its input, and this message may end up in a log.
    #[error("{0} is not an nsec or 64-char hex secret key")]
    InvalidKey(&'static str),
}

/// A string that refuses to print itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The only way out. Call sites should pass the result straight to a
    /// parser or a network client, never to a formatter.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Surfaces a missing or locked OS credential store at startup instead of at
/// the first save.
pub fn store_available() -> Result<(), SecretError> {
    Entry::store_status()
        .as_ref()
        .copied()
        .map_err(|e| SecretError::Keyring(e.to_string()))
}

fn entry(account: Account) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, account.entry_key()).map_err(|e| SecretError::Keyring(e.to_string()))
}

pub fn store_nsec(account: Account, nsec: &Secret) -> Result<(), SecretError> {
    entry(account)?
        .set_password(nsec.expose())
        .map_err(|e| SecretError::Keyring(e.to_string()))
}

/// Env override first, then the keychain. `Ok(None)` means no key anywhere.
pub fn load_nsec(account: Account) -> Result<Option<Secret>, SecretError> {
    if let Ok(value) = std::env::var(account.env_var()) {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(Some(Secret::new(value)));
        }
    }
    match entry(account)?.get_password() {
        Ok(value) => Ok(Some(Secret::new(value))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretError::Keyring(e.to_string())),
    }
}

/// The stored key as signing keys. `Ok(None)` means no key is stored.
pub fn keys(account: Account) -> Result<Option<Keys>, SecretError> {
    let Some(secret) = load_nsec(account)? else {
        return Ok(None);
    };
    Keys::parse(secret.expose())
        .map(Some)
        .map_err(|_| SecretError::InvalidKey(account.env_var()))
}

/// The `--import-keys` CLI path: read MC_ISSUER_NSEC / MC_CLAIMANT_NSEC,
/// validate, store to the keychain, and return only the derived npubs.
/// Nothing here may ever return or print key material.
pub fn import_keys_from_env() -> Result<Vec<(Account, String)>, SecretError> {
    let mut imported = Vec::new();
    for account in Account::ALL {
        let Ok(raw) = std::env::var(account.env_var()) else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let keys =
            Keys::parse(raw).map_err(|_| SecretError::InvalidKey(account.env_var()))?;
        store_nsec(account, &Secret::new(raw))?;
        let npub = keys
            .public_key()
            .to_bech32()
            .map_err(|_| SecretError::InvalidKey(account.env_var()))?;
        imported.push((account, npub));
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_prints_its_value() {
        let s = Secret::new("nsec1verysecretvalue");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert_eq!(format!("{s}"), "<redacted>");
        // Nested in a struct, the derived Debug still goes through ours.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Holder {
            key: Secret,
        }
        let printed = format!("{:?}", Holder { key: s });
        assert!(!printed.contains("verysecret"), "leaked: {printed}");
    }

    #[test]
    fn key_errors_never_echo_the_input() {
        let err = SecretError::InvalidKey(Account::Issuer.env_var());
        assert_eq!(
            err.to_string(),
            "MC_ISSUER_NSEC is not an nsec or 64-char hex secret key"
        );
    }
}
