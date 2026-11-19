//! Live check of a wallet on an LNbits server. `#[ignore]`: it opens a real
//! account and Lightning address on the server in `MC_WALLET_SERVER`, so it
//! runs only on request:
//!
//! ```text
//! MC_WALLET_SERVER=https://wallet.example mbx nextest run --run-ignored only --no-capture -E 'binary(lnbits_live)'
//! ```
//!
//! It goes through `nostr::open_wallet`, the same signup the app runs, on a
//! fresh key in a throwaway store (`MC_STORE_DIR`), then reads the balance
//! through the same dispatch the panel uses. No sats move. The store, with
//! the wallet's admin key, is left in the temp dir and its path printed.

use magic_carpet_chat::coinos;
use magic_carpet_chat::lnbits;
use magic_carpet_chat::nostr;
use magic_carpet_chat::secrets::{self, Secret};
use nostr_sdk::prelude::*;

#[tokio::test]
#[ignore = "opens a real wallet on the server in MC_WALLET_SERVER"]
async fn a_new_wallet_on_the_server_reads_zero_and_its_address_takes_zaps() {
    let server = lnbits::server_from_env()
        .expect("MC_WALLET_SERVER is a valid server")
        .expect("set MC_WALLET_SERVER=https://<your LNbits server>");
    let dir = std::env::temp_dir().join(format!("mc-lnbits-live-{}", std::process::id()));
    // SAFETY: set before any other thread reads the environment; this test
    // binary runs this one test.
    unsafe { std::env::set_var("MC_STORE_DIR", &dir) };
    println!("store: {}", dir.display());

    // rustls refuses to choose between the two providers in the graph.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let keys = Keys::generate();
    let nsec = Secret::new(keys.secret_key().to_bech32().expect("bech32 nsec"));
    let account = secrets::add_account(&nsec, true).expect("add the account");

    let login = nostr::open_wallet(&account.pubkey)
        .await
        .unwrap_or_else(|e| panic!("signup failed: {e}"));
    assert_eq!(login.server.as_deref(), Some(server.as_str()));
    assert!(login.has_token(), "the admin key was stored");
    assert_eq!(
        secrets::load_coinos_login(&account.pubkey).expect("read the store"),
        Some(login.clone()),
        "the store holds the login the signup returned"
    );
    let address = login.lightning_address();
    println!("address: {address}");
    assert_eq!(address, format!("{}@{}", login.username, lnbits::host_of(&server)));

    let sats = coinos::wallet_balance(&login)
        .await
        .unwrap_or_else(|e| panic!("balance read failed: {e}"));
    assert_eq!(sats, 0, "a new wallet is empty");

    coinos::check_address(&address)
        .await
        .unwrap_or_else(|e| panic!("{address} cannot take bounty payouts: {e}"));

    // A second run on the same login finds the address and changes nothing.
    let again = nostr::open_wallet(&account.pubkey)
        .await
        .unwrap_or_else(|e| panic!("retry failed: {e}"));
    assert_eq!(again, login);
}
