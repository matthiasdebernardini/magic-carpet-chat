//! Secret storage: every account's nsec and Coinos login in ONE file,
//! `~/Library/Application Support/magic-carpet-chat/accounts.json` (Linux:
//! `~/.config/magic-carpet-chat/accounts.json`), mode 0600,
//! written atomically. Not the keychain: every unsigned build is a new app to
//! macOS, which re-prompts for the login password on each launch, and a plain
//! file survives app updates for every build the same way.
//!
//! Every secret leaves this module wrapped in [`Secret`], which has a `Debug`
//! impl that prints a placeholder. That is the whole point — a stray `{:?}`
//! anywhere in the app, or a secret embedded in an error chain, cannot leak
//! the value into a log line.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::coinos;

const APP_DIR: &str = "magic-carpet-chat";
const FILE_NAME: &str = "accounts.json";

#[derive(thiserror::Error, Debug)]
pub enum SecretError {
    /// The store file could not be read or written. Carries the path and the
    /// io error, never the file's contents.
    #[error("{0}")]
    Store(String),
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

/// A string that refuses to print itself. Serialises as the bare string, so
/// it can sit in the store file without a wrapper.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
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

/// The store file as a whole. `Debug` is safe: every secret inside is a
/// [`Secret`].
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    /// The account the app opens on. `None` only for an empty store.
    pub active: Option<String>,
    /// In the order they were added; the rail draws them in this order.
    pub accounts: Vec<StoredAccount>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredAccount {
    pub pubkey: String,
    /// Private: the runtime gets signing keys through [`keys`], never the text.
    nsec: Secret,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coinos: Option<coinos::Login>,
}

impl StoredAccount {
    pub fn npub(&self) -> String {
        PublicKey::from_hex(&self.pubkey)
            .ok()
            .and_then(|pk| pk.to_bech32().ok())
            .unwrap_or_else(|| self.pubkey.clone())
    }
}

/// What [`add_account`] hands back: public material only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub pubkey: String,
    pub npub: String,
}

/// `MC_STORE_DIR` overrides the directory — the round-trip test writes to a
/// temp dir, and a rehearsal can run on a throwaway store.
fn store_dir() -> Result<PathBuf, SecretError> {
    if let Ok(dir) = std::env::var("MC_STORE_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    // Linux: $XDG_CONFIG_HOME, else ~/.config. Everything else keeps the
    // macOS layout the app shipped with.
    #[cfg(target_os = "linux")]
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir).join(APP_DIR));
    }
    let home = std::env::home_dir()
        .ok_or_else(|| SecretError::Store("no home directory".into()))?;
    let base = if cfg!(target_os = "linux") { ".config" } else { "Library/Application Support" };
    Ok(home.join(base).join(APP_DIR))
}

pub fn store_path() -> Result<PathBuf, SecretError> {
    Ok(store_dir()?.join(FILE_NAME))
}

/// Missing file = empty store.
pub fn load() -> Result<Store, SecretError> {
    let path = store_path()?;
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            SecretError::Store(format!("Could not read {}: {e}", path.display()))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(SecretError::Store(format!(
            "Could not read {}: {e}",
            path.display()
        ))),
    }
}

/// Write to `accounts.json.tmp` (mode 0600), then rename over the real file,
/// so a crash mid-write never leaves a half-written store.
fn save(store: &Store) -> Result<(), SecretError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let path = store_path()?;
    let fail = |e: &dyn std::fmt::Display| {
        SecretError::Store(format!("Could not save to {}: {e}", path.display()))
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| fail(&e))?;
    }
    let json = serde_json::to_vec_pretty(store).map_err(|e| fail(&e))?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| fail(&e))?;
    file.write_all(&json).map_err(|e| fail(&e))?;
    file.sync_all().map_err(|e| fail(&e))?;
    drop(file);
    std::fs::rename(&tmp, &path).map_err(|e| fail(&e))
}

/// Every change is load → modify → save; two runtime tasks can overlap (an
/// add, then its wallet step), so writers take turns.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn modify<T>(change: impl FnOnce(&mut Store) -> Result<T, SecretError>) -> Result<T, SecretError> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut store = load()?;
    let out = change(&mut store)?;
    save(&store)?;
    Ok(out)
}

fn missing(pubkey: &str) -> SecretError {
    SecretError::Store(format!("No stored account for {pubkey}"))
}

/// Parse, dedupe by pubkey, make it the active account, save. The nsec is
/// stored as bech32 whatever was pasted, so the file has one shape.
pub fn add_account(nsec: &Secret) -> Result<AccountRecord, SecretError> {
    let keys = parse_secret(nsec)?;
    let pubkey = keys.public_key().to_hex();
    let npub = keys
        .public_key()
        .to_bech32()
        .map_err(|_| SecretError::NotAKey)?;
    let nsec = Secret::new(
        keys.secret_key()
            .to_bech32()
            .map_err(|_| SecretError::NotAKey)?,
    );
    modify(|store| {
        if !store.accounts.iter().any(|a| a.pubkey == pubkey) {
            store.accounts.push(StoredAccount {
                pubkey: pubkey.clone(),
                nsec,
                coinos: None,
            });
        }
        store.active = Some(pubkey.clone());
        Ok(())
    })?;
    Ok(AccountRecord { pubkey, npub })
}

/// Drops the nsec AND the Coinos login with it. If the removed account was
/// active, the first remaining one is.
pub fn remove_account(pubkey: &str) -> Result<(), SecretError> {
    modify(|store| {
        store.accounts.retain(|a| a.pubkey != pubkey);
        if store.active.as_deref() == Some(pubkey) {
            store.active = store.accounts.first().map(|a| a.pubkey.clone());
        }
        Ok(())
    })
}

pub fn set_active(pubkey: &str) -> Result<(), SecretError> {
    modify(|store| {
        if !store.accounts.iter().any(|a| a.pubkey == pubkey) {
            return Err(missing(pubkey));
        }
        store.active = Some(pubkey.to_string());
        Ok(())
    })
}

/// Write the Coinos login BEFORE `POST /api/register`, never after: a signup
/// that succeeds but never reports back would otherwise leave a funded
/// account nobody can log in to. Delete it only through
/// [`forget_coinos_login`], and only when coinos refused outright.
pub fn store_coinos_login(pubkey: &str, login: &coinos::Login) -> Result<(), SecretError> {
    modify(|store| {
        let account = store
            .accounts
            .iter_mut()
            .find(|a| a.pubkey == pubkey)
            .ok_or_else(|| missing(pubkey))?;
        account.coinos = Some(login.clone());
        Ok(())
    })
}

pub fn load_coinos_login(pubkey: &str) -> Result<Option<coinos::Login>, SecretError> {
    Ok(load()?
        .accounts
        .into_iter()
        .find(|a| a.pubkey == pubkey)
        .and_then(|a| a.coinos))
}

pub fn forget_coinos_login(pubkey: &str) -> Result<(), SecretError> {
    modify(|store| {
        if let Some(account) = store.accounts.iter_mut().find(|a| a.pubkey == pubkey) {
            account.coinos = None;
        }
        Ok(())
    })
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

/// The stored key as signing keys. `Ok(None)` means no such account.
pub fn keys(pubkey: &str) -> Result<Option<Keys>, SecretError> {
    let store = load()?;
    let Some(account) = store.accounts.iter().find(|a| a.pubkey == pubkey) else {
        return Ok(None);
    };
    Keys::parse(account.nsec.expose())
        .map(Some)
        .map_err(|_| SecretError::InvalidKey("A stored key"))
}

/// The stored nsec itself, bech32 (`add_account` normalised it), read
/// fresh from the store. `Ok(None)` means no such account. Only the Identity
/// tab calls this: to copy it to the clipboard or show it for 30 s.
pub fn nsec(pubkey: &str) -> Result<Option<Secret>, SecretError> {
    Ok(load()?
        .accounts
        .into_iter()
        .find(|a| a.pubkey == pubkey)
        .map(|a| a.nsec))
}

/// `MC_NSECS`: comma-separated nsecs, added if not already stored. Called at
/// every launch and by `--import-keys`, so it must be idempotent: a key
/// already in the store is left alone (its active flag included). Returns
/// the npub of every key named — nothing here may ever return key material.
pub fn import_from_env() -> Result<Vec<String>, SecretError> {
    let Ok(raw) = std::env::var("MC_NSECS") else {
        return Ok(Vec::new());
    };
    let known: HashSet<String> = load()?
        .accounts
        .iter()
        .map(|a| a.pubkey.clone())
        .collect();
    let mut npubs = Vec::new();
    for piece in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let secret = Secret::new(piece);
        let keys = parse_secret(&secret).map_err(|_| SecretError::InvalidKey("MC_NSECS"))?;
        let pubkey = keys.public_key().to_hex();
        if known.contains(&pubkey) {
            npubs.push(
                keys.public_key()
                    .to_bech32()
                    .map_err(|_| SecretError::InvalidKey("MC_NSECS"))?,
            );
            continue;
        }
        npubs.push(add_account(&secret)?.npub);
    }
    Ok(npubs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MC_STORE_DIR` is process-wide. nextest gives each test its own
    /// process, but `cargo test` (CI) runs them as threads of one, so the
    /// store tests take turns.
    static STORE_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let err = SecretError::InvalidKey("MC_NSECS");
        assert_eq!(
            err.to_string(),
            "MC_NSECS is not an nsec or 64-char hex secret key"
        );
    }

    #[test]
    fn the_store_round_trips_accounts_wallets_and_the_active_flag() {
        use std::os::unix::fs::PermissionsExt as _;

        let _store = STORE_DIR.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("mc-store-test-{}", std::process::id()));
        unsafe { std::env::set_var("MC_STORE_DIR", &dir) };
        assert!(load().unwrap().accounts.is_empty(), "missing file is an empty store");

        let a = Keys::generate();
        let b = Keys::generate();
        let ra = add_account(&Secret::new(a.secret_key().to_bech32().unwrap())).unwrap();
        let rb = add_account(&Secret::new(b.secret_key().to_secret_hex())).unwrap();
        // The same key again is a no-op on the list, and makes it active.
        let again = add_account(&Secret::new(a.secret_key().to_bech32().unwrap())).unwrap();
        assert_eq!(again, ra);
        let store = load().unwrap();
        assert_eq!(store.accounts.len(), 2);
        assert_eq!(store.accounts[0].pubkey, ra.pubkey);
        assert_eq!(store.accounts[0].npub(), ra.npub);
        assert_eq!(store.active.as_deref(), Some(ra.pubkey.as_str()));
        assert!(!format!("{store:?}").contains("nsec1"), "the store debug-prints a key");

        // Hex in, bech32 out: the signing keys are the same key.
        assert_eq!(keys(&rb.pubkey).unwrap().unwrap().public_key(), b.public_key());
        assert!(keys(&"cc".repeat(32)).unwrap().is_none());

        set_active(&rb.pubkey).unwrap();
        assert_eq!(load().unwrap().active.as_deref(), Some(rb.pubkey.as_str()));
        assert!(set_active(&"cc".repeat(32)).is_err());

        let login = coinos::Login::fresh();
        store_coinos_login(&ra.pubkey, &login).unwrap();
        assert_eq!(load_coinos_login(&ra.pubkey).unwrap(), Some(login.clone()));
        assert_eq!(load_coinos_login(&rb.pubkey).unwrap(), None);
        assert!(store_coinos_login(&"cc".repeat(32), &login).is_err());

        let path = store_path().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "store file mode");
        assert!(!path.with_extension("json.tmp").exists(), "temp file left behind");

        forget_coinos_login(&ra.pubkey).unwrap();
        assert_eq!(load_coinos_login(&ra.pubkey).unwrap(), None);

        // Removing the active account falls back to the first remaining one.
        remove_account(&rb.pubkey).unwrap();
        let store = load().unwrap();
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.active.as_deref(), Some(ra.pubkey.as_str()));
        assert!(keys(&rb.pubkey).unwrap().is_none());
        remove_account(&ra.pubkey).unwrap();
        assert_eq!(load().unwrap().active, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nsec_returns_the_stored_bech32_key() {
        let _store = STORE_DIR.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("mc-nsec-test-{}", std::process::id()));
        unsafe { std::env::set_var("MC_STORE_DIR", &dir) };

        let keys = Keys::generate();
        // Stored from hex, handed back as bech32.
        let record = add_account(&Secret::new(keys.secret_key().to_secret_hex())).unwrap();
        let stored = nsec(&record.pubkey).unwrap().unwrap();
        assert_eq!(stored.expose(), keys.secret_key().to_bech32().unwrap());
        assert!(nsec(&"cc".repeat(32)).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
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
