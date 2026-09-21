//! The protocol layer of magic-carpet-chat: everything that talks Nostr or
//! HTTP to the Magic Carpet instance, kept free of gpui so it can run on its
//! own tokio thread and be tested offline.
//!
//! The UI (the binary in `main.rs`) drives this through
//! [`nostr::spawn_runtime`] and never touches a tokio-dependent future on the
//! gpui executor.

/// What every HTTP client this app builds announces itself as.
pub const USER_AGENT: &str = concat!("magic-carpet-chat/", env!("CARGO_PKG_VERSION"));

pub mod api;
pub mod coinos;
pub mod events;
pub mod nostr;
pub mod secrets;
pub mod trust;
