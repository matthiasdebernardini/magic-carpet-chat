//! Live check: two brand-new keys open wallets through the runtime the UI
//! drives, one gets funded, and they pay each other. It talks to the real
//! coinos.io and the real instance relay, so it is `#[ignore]` and the
//! offline suite never touches it. Run it with:
//!
//! ```sh
//! mbx nextest run --run-ignored only --no-capture -E 'binary(e2e)'
//! ```
//!
//! Account A's 21 sats come from one of two payers:
//!
//! - `MC_E2E_PAYER_TOKEN=<coinos token>`: the test pays A's LNURL invoice
//!   itself through the same `lnurl_invoice` + `pay_invoice` path the app
//!   sends with, and waits up to 120 s for the balance.
//! - unset: the test prints A's address and its LNURL and waits for a phone
//!   wallet to pay, up to `MC_E2E_ZAP_WAIT_SECS` (default 600).
//!
//! Then A sends B 21 sats, B sends A 10 back, and B is refused a send it
//! cannot afford. Both accounts land in a throwaway store under
//! `MC_STORE_DIR`, printed at the end: A's `coinos.token` can be the payer
//! for the next run. The nsecs, the coinos passwords and every token stay in
//! that file; nothing here prints them.

use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use magic_carpet_chat::coinos;
use magic_carpet_chat::nostr::{Command, ErrorSource, NostrHandle, Update, spawn_runtime};
use magic_carpet_chat::secrets::Secret;
use nostr_sdk::prelude::*;

/// What A is funded with, and what A sends B.
const FUNDING_SATS: u64 = 21;
/// What B sends back.
const RETURN_SATS: u64 = 10;
/// What B cannot afford.
const TOO_MANY_SATS: u64 = 100_000;

/// One update, printed as it arrives. Every variant is public material, so
/// `{:?}` is safe. A silence longer than `deadline` is a failure with the
/// step's name in it, so the log says which stage stalled.
async fn next(updates: &mut UnboundedReceiver<Update>, deadline: Duration, step: &str) -> Update {
    try_next(updates, deadline)
        .await
        .unwrap_or_else(|| panic!("{step}: no update within {deadline:?}"))
}

/// Same, but silence is `None`: the balance poll uses it as its pacing.
async fn try_next(updates: &mut UnboundedReceiver<Update>, deadline: Duration) -> Option<Update> {
    let update = match tokio::time::timeout(deadline, updates.next()).await {
        Err(_) => return None,
        Ok(None) => panic!("runtime channel closed: the runtime thread died"),
        Ok(Some(update)) => update,
    };
    println!("[update] {update:?}");
    Some(update)
}

/// Any failure the runtime reports ends the run with its message, whatever
/// step was waiting: a `WalletFailed` during the profile wait is still a bug.
/// A wallet read that failed once (coinos timeout, 5xx) is the exception:
/// the poll retries, and only its deadline may end the run.
fn fail_on_error(update: &Update) {
    match update {
        Update::AddAccountFailed { message } => panic!("AddAccountFailed: {message}"),
        Update::WalletFailed { message, .. } => panic!("WalletFailed: {message}"),
        Update::Error {
            source: ErrorSource::Wallet,
            ..
        } => {}
        Update::Error { source, message } => panic!("Error from {source:?}: {message}"),
        _ => {}
    }
}

/// One account as the test knows it: the public key and the address the
/// wallet answers at.
struct Account {
    name: &'static str,
    pubkey: String,
    address: String,
    username: String,
    ready: Duration,
}

/// `AddAccount { open_wallet: true }` for a fresh key, with every check the
/// account screen relies on: the profile probe reached the instance and
/// found nothing, the wallet opened, the kind-0 with the lud16 published.
async fn add_account(handle: &mut NostrHandle, name: &'static str) -> Account {
    let keys = Keys::generate();
    let pubkey = keys.public_key().to_hex();
    let nsec = keys.secret_key().to_bech32().expect("nsec bech32");
    let t0 = Instant::now();
    handle
        .commands
        .unbounded_send(Command::AddAccount {
            secret: Secret::new(nsec),
            name: Some(name.into()),
            open_wallet: true,
        })
        .unwrap();

    let wallet_deadline = t0 + Duration::from_secs(90);
    let mut account_added = false;
    let created = loop {
        let left = wallet_deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{name}: no WalletCreated within 90 s");
        let update = next(&mut handle.updates, left, "wallet creation").await;
        fail_on_error(&update);
        match update {
            Update::AccountAdded {
                pubkey: p,
                profile_checked,
                profile_found,
                ..
            } if p == pubkey => {
                assert!(profile_checked, "the profile probe must reach the instance");
                assert!(!profile_found, "a fresh key cannot already have a kind-0");
                account_added = true;
            }
            Update::WalletCreated { pubkey: p, created } if p == pubkey => {
                assert!(account_added, "WalletCreated arrived before AccountAdded");
                break created;
            }
            _ => {}
        }
    };
    let ready = t0.elapsed();
    assert_eq!(created.publish_error, None, "the kind-0 with the lud16 did not publish");
    let address = created.lightning_address.clone();
    assert!(address.ends_with("@coinos.io"), "unexpected address {address}");
    expect_profile(&mut handle.updates, &pubkey, &address, Some(name)).await;
    Account {
        name,
        pubkey,
        username: created.username,
        address,
        ready,
    }
}

/// The next `ProfileLoaded` for `pubkey` must carry the address; `name` is
/// checked only where the spec pins it.
async fn expect_profile(
    updates: &mut UnboundedReceiver<Update>,
    pubkey: &str,
    address: &str,
    name: Option<&str>,
) {
    loop {
        let update = next(updates, Duration::from_secs(60), "ProfileLoaded").await;
        fail_on_error(&update);
        if let Update::ProfileLoaded {
            pubkey: p,
            name: n,
            lud16,
            ..
        } = update
            && p == pubkey
        {
            assert_eq!(lud16.as_deref(), Some(address), "profile lud16 mismatch");
            if let Some(name) = name {
                assert_eq!(n.as_deref(), Some(name), "profile name mismatch");
            }
            return;
        }
    }
}

/// How often the poll asks; the app's own panel poll uses the same cadence.
const POLL_EVERY: Duration = Duration::from_secs(3);

/// `FetchBalance` every 3 s by the wall clock until the account's balance
/// satisfies `enough`, or `wait` runs out. Updates arriving in between are
/// drained as they come, so a chatty runtime (a flapping relay) neither
/// delays the next ask nor hides the deadline. Returns the balance that
/// satisfied it.
async fn poll_balance(
    handle: &mut NostrHandle,
    account: &Account,
    wait: Duration,
    what: &str,
    enough: impl Fn(u64) -> bool,
) -> u64 {
    let deadline = Instant::now() + wait;
    loop {
        handle
            .commands
            .unbounded_send(Command::FetchBalance {
                pubkey: account.pubkey.clone(),
            })
            .unwrap();
        let next_ask = Instant::now() + POLL_EVERY;
        loop {
            let now = Instant::now();
            assert!(
                now < deadline,
                "{}: {what} did not happen within {wait:?}",
                account.name
            );
            let left = next_ask.saturating_duration_since(now);
            if left.is_zero() {
                break;
            }
            if let Some(update) = try_next(&mut handle.updates, left.min(deadline - now)).await {
                fail_on_error(&update);
                if let Update::Balance { pubkey, sats } = update
                    && pubkey == account.pubkey
                    && enough(sats)
                {
                    return sats;
                }
            }
        }
    }
}

/// One `FetchBalance`, answered.
async fn read_balance(handle: &mut NostrHandle, account: &Account) -> u64 {
    poll_balance(handle, account, Duration::from_secs(30), "a balance read", |_| true).await
}

/// A send is up to three HTTP calls (two to the recipient's LNURL server,
/// one 90 s payment POST), so its verdict gets that long.
const SEND_WAIT: Duration = Duration::from_secs(120);

/// `Command::Send` from `from`, resolved to the runtime's verdict.
async fn send(handle: &mut NostrHandle, from: &Account, to: &str, sats: u64) -> Result<(), String> {
    handle
        .commands
        .unbounded_send(Command::Send {
            pubkey: from.pubkey.clone(),
            to: to.to_string(),
            sats,
        })
        .unwrap();
    loop {
        let update = next(&mut handle.updates, SEND_WAIT, "Send").await;
        fail_on_error(&update);
        match update {
            Update::Sent { pubkey, to: t, sats: s } if pubkey == from.pubkey => {
                assert_eq!(t, to, "Sent names the wrong recipient");
                assert_eq!(s, sats, "Sent names the wrong amount");
                return Ok(());
            }
            Update::SendFailed {
                pubkey, message, ..
            } if pubkey == from.pubkey => {
                return Err(message);
            }
            _ => {}
        }
    }
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn two_new_users_open_wallets_and_pay_each_other() {
    let store_dir = std::env::temp_dir().join(format!(
        "mc-e2e-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&store_dir).expect("create the throwaway store dir");
    // SAFETY: set before spawn_runtime, so no thread of ours reads the
    // environment concurrently; the idle tokio workers do not touch it.
    unsafe { std::env::set_var("MC_STORE_DIR", &store_dir) };

    let mut handle = spawn_runtime();
    handle.commands.unbounded_send(Command::Connect).unwrap();

    // Step 1: two accounts, each with a wallet and a published lud16.
    let a = add_account(&mut handle, "Carpet A").await;
    let b = add_account(&mut handle, "Carpet B").await;
    assert_ne!(a.address, b.address, "two accounts got the same wallet");

    // A cold read of the store and the instance agrees on both.
    handle.commands.unbounded_send(Command::LoadAccounts).unwrap();
    loop {
        let update = next(&mut handle.updates, Duration::from_secs(30), "LoadAccounts").await;
        fail_on_error(&update);
        if let Update::AccountsLoaded { accounts, .. } = update {
            assert_eq!(accounts.len(), 2, "two accounts expected: {accounts:?}");
            for account in [&a, &b] {
                let stored = accounts
                    .iter()
                    .find(|s| s.pubkey == account.pubkey)
                    .unwrap_or_else(|| panic!("{} missing from the store", account.name));
                let wallet = stored.wallet.as_ref().expect("a stored wallet");
                assert!(wallet.has_token, "{}: the register token was not stored", account.name);
                assert_eq!(wallet.lightning_address, account.address);
            }
            break;
        }
    }
    // These reads go through the instance API, so they prove the relay
    // carries both addresses, not just our memory of them.
    let mut profiles_seen = 0;
    while profiles_seen < 2 {
        let update = next(&mut handle.updates, Duration::from_secs(60), "ProfileLoaded").await;
        fail_on_error(&update);
        if let Update::ProfileLoaded { pubkey, lud16, .. } = update {
            let account = [&a, &b]
                .into_iter()
                .find(|acc| acc.pubkey == pubkey)
                .expect("a profile for one of our keys");
            assert_eq!(lud16.as_deref(), Some(account.address.as_str()));
            profiles_seen += 1;
        }
    }

    // Step 2: fund A.
    let funding_wait = match std::env::var("MC_E2E_PAYER_TOKEN") {
        Ok(token) => {
            let bolt11 = coinos::lnurl_invoice(&a.address, FUNDING_SATS)
                .await
                .expect("an invoice from A's LNURL endpoint");
            coinos::pay_invoice(&Secret::new(token.trim()), &bolt11, FUNDING_SATS)
                .await
                .expect("the payer token pays A's invoice");
            println!("[payer] paid {FUNDING_SATS} sats to {} from the token wallet", a.address);
            Duration::from_secs(120)
        }
        Err(_) => {
            let secs = std::env::var("MC_E2E_ZAP_WAIT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(600);
            let lnurl = coinos::lnurl_pay(&a.address).expect("an LNURL for A's address");
            println!("\n{}", "=".repeat(72));
            println!("  Pay {FUNDING_SATS} sats to {} from your phone", a.address);
            println!("  {lnurl}");
            println!("{}\n", "=".repeat(72));
            Duration::from_secs(secs)
        }
    };
    let funding_started = Instant::now();
    let a_funded = poll_balance(&mut handle, &a, funding_wait, "the funding", |sats| {
        sats >= FUNDING_SATS
    })
    .await;
    let funding_seen = funding_started.elapsed();

    // Step 3: A pays B.
    let sent_at = Instant::now();
    if let Err(message) = send(&mut handle, &a, &b.address, FUNDING_SATS).await {
        panic!("A -> B failed: {message}");
    }
    poll_balance(&mut handle, &b, Duration::from_secs(60), "B receiving from A", |sats| {
        sats >= FUNDING_SATS
    })
    .await;
    let a_to_b_seen = sent_at.elapsed();
    let a_after_send = read_balance(&mut handle, &a).await;
    assert!(
        a_after_send + FUNDING_SATS <= a_funded,
        "A's balance went {a_funded} -> {a_after_send}, not down by {FUNDING_SATS}"
    );

    // Step 4: B pays A back.
    let sent_at = Instant::now();
    if let Err(message) = send(&mut handle, &b, &a.address, RETURN_SATS).await {
        panic!("B -> A failed: {message}");
    }
    poll_balance(&mut handle, &a, Duration::from_secs(60), "A receiving from B", |sats| {
        sats >= a_after_send + RETURN_SATS
    })
    .await;
    let b_to_a_seen = sent_at.elapsed();
    let b_after_send = read_balance(&mut handle, &b).await;
    assert!(
        b_after_send <= FUNDING_SATS - RETURN_SATS,
        "B kept {b_after_send} sats after sending {RETURN_SATS} of {FUNDING_SATS}"
    );

    // Step 5: B cannot send what it does not have. Coinos itself must be the
    // one saying no — a recipient-side or token failure would prove nothing
    // — and B's balance must not move.
    match send(&mut handle, &b, &a.address, TOO_MANY_SATS).await {
        Ok(()) => panic!("B sent {TOO_MANY_SATS} sats it never had"),
        Err(message) => {
            println!("[refused] B -> A {TOO_MANY_SATS} sats: {message}");
            assert!(
                message.starts_with("coinos refused"),
                "the refusal must come from coinos, not the recipient or the token: {message}"
            );
        }
    }
    let b_after_refusal = read_balance(&mut handle, &b).await;
    assert_eq!(
        b_after_refusal, b_after_send,
        "B's balance moved on a refused send"
    );

    handle.commands.unbounded_send(Command::Shutdown).unwrap();
    println!("\n  A ready                 {:>7.1} s", a.ready.as_secs_f64());
    println!("  B ready                 {:>7.1} s", b.ready.as_secs_f64());
    println!(
        "  A funded after          {:>7.1} s  ({a_funded} sats)",
        funding_seen.as_secs_f64()
    );
    println!("  A -> B seen after       {:>7.1} s", a_to_b_seen.as_secs_f64());
    println!("  B -> A seen after       {:>7.1} s", b_to_a_seen.as_secs_f64());
    println!("  A username              {}", a.username);
    println!("  B username              {}", b.username);
    println!("  store                   {}\n", store_dir.display());
}
