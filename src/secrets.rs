//! Secret storage. The two nsecs are bearer credentials: they live in the OS
//! keychain (or an env override for automated runs) and nowhere else.
//!
//! Every secret leaves this module wrapped in [`Secret`], which has a `Debug`
//! impl that prints a placeholder. That is the whole point — a stray `{:?}`
//! anywhere in the app, or a secret embedded in an error chain, cannot leak
//! the value into a log line.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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

    /// The Coinos username + password this app opened for the account, as
    /// one JSON string (see `coinos::Login`). Its own entry, so forgetting the
    /// nsec and forgetting the wallet login stay separate decisions. Public
    /// because the UI names the entry when it tells the user where the
    /// password lives.
    pub fn coinos_entry_key(self) -> &'static str {
        match self {
            Account::Issuer => "issuer-coinos-login",
            Account::Claimant => "claimant-coinos-login",
        }
    }

    /// Whether this app opens and shows a wallet for the account. Only the
    /// claimant: the issuer's wallet is the prod payer's, managed elsewhere.
    pub fn has_local_wallet(self) -> bool {
        self == Account::Claimant
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
    /// The pasted-key path. Same rule as `InvalidKey`: no parser detail, no
    /// echo of the input — this string is shown on screen and may be logged.
    #[error("That doesn't look like a key — paste an nsec1… string or 64 hex characters")]
    NotAKey,
    /// An npub — the half every client shows on the profile page. Right key
    /// family, wrong half: say so, and say where the right one lives.
    #[error(
        "That's your public key — the app needs your secret key, which starts \
         with nsec1. Find it in your Nostr app under Settings > Keys."
    )]
    PublicKeyPasted,
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

fn keyring_err(e: impl std::fmt::Display) -> SecretError {
    SecretError::Keyring(e.to_string())
}

/// Surfaces a missing or locked OS credential store at startup instead of at
/// the first save.
pub fn store_available() -> Result<(), SecretError> {
    if DEV_STORE {
        return dev_dir().map(|_| ());
    }
    Entry::store_status()
        .as_ref()
        .copied()
        .map_err(keyring_err)
}

/// Debug builds keep secrets in plain 0600 files instead of the keychain:
/// every unsigned debug binary is a new app to macOS, which re-prompts for the
/// login password on each launch. Release builds always use the keychain.
const DEV_STORE: bool = cfg!(debug_assertions);

/// The dev-store directory, resolved and created once per process. The
/// failure is kept as text because `SecretError` does not clone.
fn dev_dir() -> Result<&'static Path, SecretError> {
    static DEV_DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DEV_DIR
        .get_or_init(|| {
            let dir = std::env::home_dir()
                .ok_or_else(|| "no home directory".to_string())?
                .join("Library/Application Support")
                .join(SERVICE)
                .join("dev-secrets");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            Ok(dir)
        })
        .as_deref()
        .map_err(keyring_err)
}

fn get(key: &str) -> Result<Option<String>, SecretError> {
    if DEV_STORE {
        return match std::fs::read_to_string(dev_dir()?.join(key)) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(keyring_err(e)),
        };
    }
    match entry(key)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(keyring_err(e)),
    }
}

fn set(key: &str, value: &str) -> Result<(), SecretError> {
    if DEV_STORE {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(dev_dir()?.join(key))
            .map_err(keyring_err)?;
        return file.write_all(value.as_bytes()).map_err(keyring_err);
    }
    entry(key)?.set_password(value).map_err(keyring_err)
}

/// An entry that was never stored is already forgotten, so "not found" is success.
fn del(key: &str) -> Result<(), SecretError> {
    if DEV_STORE {
        return match std::fs::remove_file(dev_dir()?.join(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(keyring_err(e)),
        };
    }
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(keyring_err(e)),
    }
}

fn entry(key: &str) -> Result<Entry, SecretError> {
    Entry::new(SERVICE, key).map_err(keyring_err)
}

pub fn store_nsec(account: Account, nsec: &Secret) -> Result<(), SecretError> {
    set(account.entry_key(), nsec.expose())
}

/// "Forget this key": delete the stored entry. An env override is NOT
/// touched — env wins on the next load, and lying about that would be worse
/// than showing the key come back.
pub fn forget_nsec(account: Account) -> Result<(), SecretError> {
    del(account.entry_key())
}

/// Write the Coinos login BEFORE `POST /api/register`, never after: a signup
/// that succeeds but never reports back would otherwise leave a funded
/// account nobody can log in to. Delete it only through
/// [`forget_coinos_login`], and only when coinos refused outright.
pub fn store_coinos_login(account: Account, login: &Secret) -> Result<(), SecretError> {
    set(account.coinos_entry_key(), login.expose())
}

pub fn load_coinos_login(account: Account) -> Result<Option<Secret>, SecretError> {
    Ok(get(account.coinos_entry_key())?.map(Secret::new))
}

pub fn forget_coinos_login(account: Account) -> Result<(), SecretError> {
    del(account.coinos_entry_key())
}

/// Validate a user-pasted secret: `nsec1…` bech32 or 64 hex characters.
/// The shape is checked here, before nostr's parser, so the error can be
/// specific without ever carrying the input.
pub fn parse_secret(secret: &Secret) -> Result<Keys, SecretError> {
    let raw = secret.expose().trim();
    // bech32 is case-insensitive in practice for a paste: normalise once so
    // an uppercased nsec/npub is recognised too.
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("npub1") {
        // The most likely wrong paste from a non-technical user: their
        // public key, copied off any client's profile screen.
        return Err(SecretError::PublicKeyPasted);
    }
    let is_hex = raw.len() == 64 && raw.chars().all(|c| c.is_ascii_hexdigit());
    if !lower.starts_with("nsec1") && !is_hex {
        return Err(SecretError::NotAKey);
    }
    Keys::parse(&lower).map_err(|_| SecretError::NotAKey)
}

/// Env override first, then the keychain. `Ok(None)` means no key anywhere.
pub fn load_nsec(account: Account) -> Result<Option<Secret>, SecretError> {
    if let Ok(value) = std::env::var(account.env_var()) {
        let value = value.trim();
        if !value.is_empty() {
            return Ok(Some(Secret::new(value)));
        }
    }
    Ok(get(account.entry_key())?.map(Secret::new))
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
        let keys = Keys::parse(raw).map_err(|_| SecretError::InvalidKey(account.env_var()))?;
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

    #[test]
    fn parse_secret_accepts_a_valid_nsec() {
        let keys = Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let parsed = parse_secret(&Secret::new(nsec)).unwrap();
        assert_eq!(parsed.public_key(), keys.public_key());
    }

    #[test]
    fn parse_secret_accepts_64_char_hex_in_either_case() {
        let keys = Keys::generate();
        let hex = keys.secret_key().to_secret_hex();
        assert_eq!(hex.len(), 64);
        let parsed = parse_secret(&Secret::new(hex.clone())).unwrap();
        assert_eq!(parsed.public_key(), keys.public_key());
        // Uppercase hex is the same key.
        let parsed = parse_secret(&Secret::new(hex.to_ascii_uppercase())).unwrap();
        assert_eq!(parsed.public_key(), keys.public_key());
    }

    #[test]
    fn parse_secret_trims_surrounding_whitespace() {
        let keys = Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let parsed = parse_secret(&Secret::new(format!("  {nsec}\n"))).unwrap();
        assert_eq!(parsed.public_key(), keys.public_key());
    }

    #[test]
    fn parse_secret_rejects_garbage_without_echoing_it() {
        for bad in [
            "hello world",
            "nsec1qqqqqqqq", // nsec-shaped, bad checksum
            "abc123",        // hex, wrong length
            &"a".repeat(63), // one short of 64
            &"a".repeat(65), // one past 64
            &"g".repeat(64), // 64 chars, not hex
            "",
        ] {
            let err = parse_secret(&Secret::new(bad)).unwrap_err();
            let message = err.to_string();
            assert_eq!(
                message,
                "That doesn't look like a key — paste an nsec1… string or 64 hex characters"
            );
            if !bad.is_empty() {
                assert!(!message.contains(bad), "echoed input: {message}");
            }
        }
    }

    #[test]
    fn parse_secret_tells_a_pasted_npub_apart() {
        // A real npub, an npub-shaped junk string, and an uppercased npub all
        // get the "that's your public key" message — never the generic one,
        // and never an echo.
        let real = Keys::generate().public_key().to_bech32().unwrap();
        for npub in [
            real.clone(),
            "npub1xyz".to_string(),
            real.to_ascii_uppercase(),
        ] {
            let err = parse_secret(&Secret::new(npub.clone())).unwrap_err();
            assert!(matches!(err, SecretError::PublicKeyPasted));
            let message = err.to_string();
            assert!(message.contains("public key"), "wrong message: {message}");
            assert!(
                message.contains("nsec1"),
                "no pointer to the fix: {message}"
            );
            assert!(!message.contains(&npub), "echoed input: {message}");
        }
    }

    #[test]
    fn parse_secret_accepts_an_uppercased_nsec() {
        let keys = Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap().to_ascii_uppercase();
        let parsed = parse_secret(&Secret::new(nsec)).unwrap();
        assert_eq!(parsed.public_key(), keys.public_key());
    }
}
