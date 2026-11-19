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
//!
//! `issuer_pays_a_claimant_on_another_lightning_provider` is the second test
//! here: the app's auto-pay zaps a claimant whose Lightning address is not on
//! coinos, and the zap receipt that other provider publishes must reach the
//! instance relay. See its doc comment for the variables it needs. Run one
//! test at a time with `test(<name>)` in the filter.

use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use magic_carpet_chat::api::CreateBounty;
use magic_carpet_chat::coinos;
use magic_carpet_chat::events;
use magic_carpet_chat::nostr::{
    Command, ErrorSource, NostrHandle, Update, public_relays, relay_url, spawn_runtime,
};
use magic_carpet_chat::secrets::{self, Secret};
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
            generated: true,
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
                request: None,
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
                if let Update::Balance { pubkey, sats, .. } = update
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
                message.starts_with("Coinos said no"),
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

/// The reward the external-provider test pays: the smallest amount the
/// first test moves.
const EXTERNAL_REWARD_SATS: u64 = RETURN_SATS;

/// An env var the test cannot run without, trimmed. Missing or blank stops
/// the run before anything is spent.
fn required_env(name: &str, what: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| panic!("{name} is not set: it must hold {what}. Nothing was spent."))
}

/// `AddAccount` for a key the test was handed (no wallet opened, not
/// generated here), resolved to its hex pubkey once the account is stored.
async fn import_account(handle: &mut NostrHandle, name: &'static str, nsec: &str) -> String {
    let pubkey = Keys::parse(nsec)
        .unwrap_or_else(|_| panic!("{name}: the key is not an nsec or hex secret key"))
        .public_key()
        .to_hex();
    handle
        .commands
        .unbounded_send(Command::AddAccount {
            secret: Secret::new(nsec),
            name: None,
            open_wallet: false,
            generated: false,
        })
        .unwrap();
    loop {
        let update = next(&mut handle.updates, Duration::from_secs(60), "AddAccount").await;
        fail_on_error(&update);
        if let Update::AccountAdded { pubkey: p, .. } = update
            && p == pubkey
        {
            return pubkey;
        }
    }
}

/// The signature-checked kind-9735 receipts naming `claim_id` on `relays`.
/// A relay that does not answer within 10 s counts as holding none.
async fn receipts_on(relays: &[String], claim_id: EventId) -> Vec<Event> {
    let client = Client::default();
    for url in relays {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().and_wait(Duration::from_secs(10)).await;
    let filter = Filter::new()
        .kind(Kind::Custom(events::RECEIPT_KIND))
        .event(claim_id);
    let receipts = client
        .fetch_events(filter)
        .timeout(Duration::from_secs(10))
        .await
        .map(|found| found.into_iter().filter(|e| e.verify().is_ok()).collect())
        .unwrap_or_default();
    client.disconnect().await;
    receipts
}

/// The issuer's own auto-pay zaps a claimant whose Lightning address is on
/// another provider (not coinos), and the receipt that provider publishes
/// reaches the instance relay, so the claim reads as paid. Paying a Lightning
/// address is otherwise only proven with coinos on both ends. It moves
/// [`EXTERNAL_REWARD_SATS`] real sats, so it is `#[ignore]`. Run it with:
///
/// ```sh
/// mbx nextest run --run-ignored only --no-capture -E 'binary(e2e) and test(issuer_pays_a_claimant_on_another_lightning_provider)'
/// ```
///
/// - `MC_E2E_ISSUER_NSEC`: the issuer's key. Its own trust scores must rank
///   the claimant at least 2, the auto-pay floor, or the claim is never paid.
/// - `MC_E2E_PAYER_TOKEN`: the coinos token of the wallet the issuer pays
///   from; it must hold the reward.
/// - `MC_E2E_CLAIMANT_NSEC`: the claimant's key. Its kind-0 gets the address
///   below as its lud16, on the instance and the public relays.
/// - `MC_E2E_EXTERNAL_LUD16`: the claimant's Lightning address, on any
///   provider but coinos.io that sends zap receipts.
/// - `MC_E2E_RECEIPT_WAIT_SECS` (default 300): how long the receipt may take
///   to reach the instance relay once the payment left.
///
/// Both keys land in a throwaway store under `MC_STORE_DIR`, printed at the
/// end; the attempt record there is what keeps a rerun from paying twice.
#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn issuer_pays_a_claimant_on_another_lightning_provider() {
    // Every check that can fail runs before a sat moves.
    let issuer_nsec = required_env("MC_E2E_ISSUER_NSEC", "the issuer's nsec");
    let token = required_env("MC_E2E_PAYER_TOKEN", "the coinos token of the issuer's wallet");
    let claimant_nsec = required_env("MC_E2E_CLAIMANT_NSEC", "the claimant's nsec");
    let address = required_env(
        "MC_E2E_EXTERNAL_LUD16",
        "the claimant's Lightning address on a provider other than coinos.io",
    );
    let (_, domain) = coinos::split_address(&address)
        .unwrap_or_else(|| panic!("MC_E2E_EXTERNAL_LUD16 is not name@domain. Nothing was spent."));
    assert!(
        !domain.eq_ignore_ascii_case("coinos.io"),
        "MC_E2E_EXTERNAL_LUD16 is on coinos.io; this test needs another provider. Nothing was spent."
    );
    if let Err(e) = coinos::check_address(&address).await {
        panic!("MC_E2E_EXTERNAL_LUD16 can't take a zap: {e} Nothing was spent.");
    }
    // A plain invoice for the reward proves the server takes that amount;
    // it is never paid.
    if let Err(e) = coinos::lnurl_invoice(&address, EXTERNAL_REWARD_SATS).await {
        panic!("{domain} won't invoice {EXTERNAL_REWARD_SATS} sats: {e} Nothing was spent.");
    }
    let token = Secret::new(token);
    let wallet = coinos::balance(&token)
        .await
        .unwrap_or_else(|e| panic!("MC_E2E_PAYER_TOKEN can't read its wallet: {e} Nothing was spent."));
    assert!(
        wallet >= EXTERNAL_REWARD_SATS,
        "the payer wallet holds {wallet} sats, less than the {EXTERNAL_REWARD_SATS} sat reward. Nothing was spent."
    );

    let store_dir = std::env::temp_dir().join(format!(
        "mc-e2e-external-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&store_dir).expect("create the throwaway store dir");
    // SAFETY: set before spawn_runtime, so no thread of ours reads the
    // environment concurrently; the idle tokio workers do not touch it.
    unsafe {
        std::env::set_var("MC_STORE_DIR", &store_dir);
        std::env::set_var("MC_AUTOPAY_POLL_SECS", "5");
    }

    let mut handle = spawn_runtime();
    handle.commands.unbounded_send(Command::Connect).unwrap();

    // Step 1: both keys in the store; the issuer pays from the token's wallet.
    let issuer = import_account(&mut handle, "issuer", &issuer_nsec).await;
    let claimant = import_account(&mut handle, "claimant", &claimant_nsec).await;
    assert_ne!(issuer, claimant, "the issuer and the claimant are the same key");
    secrets::store_coinos_login(
        &issuer,
        &coinos::Login {
            username: String::new(),
            password: Secret::new(""),
            token,
            server: None,
        },
    )
    .expect("store the payer token as the issuer's wallet");

    // Step 2: the claimant's kind-0 names the external address, through the
    // same command the account screen sends.
    handle
        .commands
        .unbounded_send(Command::SetLightningAddress {
            pubkey: claimant.clone(),
            address: address.clone(),
        })
        .unwrap();
    loop {
        let update = next(&mut handle.updates, Duration::from_secs(90), "SetLightningAddress").await;
        fail_on_error(&update);
        match update {
            Update::LightningAddressSet { pubkey, .. } if pubkey == claimant => break,
            Update::LightningAddressFailed { pubkey, message } if pubkey == claimant => {
                panic!("the claimant's lud16 did not publish: {message} Nothing was spent.")
            }
            _ => {}
        }
    }
    expect_profile(&mut handle.updates, &claimant, &address, None).await;

    // Step 3: a one-slot auto-pay bounty on a fresh list.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    handle
        .commands
        .unbounded_send(Command::PublishDList {
            pubkey: issuer.clone(),
            singular: format!("e2e zap item {stamp}"),
            plural: format!("e2e zap items {stamp}"),
            description: None,
        })
        .unwrap();
    let list_coordinate = loop {
        let update = next(&mut handle.updates, Duration::from_secs(60), "PublishDList").await;
        fail_on_error(&update);
        if let Update::DListPublished { pubkey, coordinate } = update
            && pubkey == issuer
        {
            break coordinate;
        }
    };
    handle
        .commands
        .unbounded_send(Command::CreateBounty {
            pubkey: issuer.clone(),
            req: CreateBounty {
                list_coordinate: list_coordinate.clone(),
                amount_sats: EXTERNAL_REWARD_SATS,
                criteria: "End-to-end check: any one item, paid to a Lightning address off coinos."
                    .into(),
                bounty_cap_sats: Some(EXTERNAL_REWARD_SATS),
                reward_per_item: true,
                max_rewards_per_npub: Some(1),
                auto_pay: true,
                auto_pay_min_rank: None,
            },
        })
        .unwrap();
    let bounty_id = loop {
        let update = next(&mut handle.updates, Duration::from_secs(60), "CreateBounty").await;
        fail_on_error(&update);
        if let Update::BountyCreated { id } = update {
            break id;
        }
    };

    // Step 4: the claimant claims; the issuer's auto-pay picks it up.
    handle
        .commands
        .unbounded_send(Command::PublishClaim {
            pubkey: claimant.clone(),
            bounty_id: bounty_id.clone(),
            name: format!("e2e item {stamp}"),
            list_coordinate: list_coordinate.clone(),
        })
        .unwrap();
    let claim_id = loop {
        let update = next(&mut handle.updates, Duration::from_secs(60), "PublishClaim").await;
        fail_on_error(&update);
        if let Update::ClaimPublished { bounty_id: b, event_id } = update
            && b == bounty_id
        {
            break event_id;
        }
    };
    let claimed_at = Instant::now();

    // Step 5: the payment. No attempt within the wait means the claim never
    // qualified (rank, address, wallet), and nothing was spent.
    let attempt_wait = Duration::from_secs(180);
    let attempt_deadline = claimed_at + attempt_wait;
    loop {
        let left = attempt_deadline.saturating_duration_since(Instant::now());
        let Some(update) = try_next(&mut handle.updates, left).await else {
            panic!(
                "no payment attempt within {attempt_wait:?} of the claim: the claim never \
                 qualified for auto-pay (the issuer's scores must rank the claimant at least 2, \
                 the claimant's newest kind-0 must name the address, the wallet must hold the \
                 reward). Nothing was spent."
            );
        };
        fail_on_error(&update);
        if let Update::PaymentState {
            claim_event_id,
            state,
            reason,
            ..
        } = update
            && claim_event_id == claim_id
        {
            match state.as_str() {
                "attempting" => {}
                "paid" => break,
                "failed" => panic!("coinos refused the payment: {reason:?}"),
                _ => panic!(
                    "the payment's outcome is unknown ({reason:?}): check the payer wallet before rerunning"
                ),
            }
        }
    }
    let paid_after = claimed_at.elapsed();

    // Step 6: the receipt, judged by the rule the app counts a claim paid by:
    // the issuer signed the zap request inside it, for this claim, bounty and
    // reward. Coinos saying "paid" proves nothing here.
    let receipt_wait = Duration::from_secs(
        std::env::var("MC_E2E_RECEIPT_WAIT_SECS")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(300),
    );
    let claim_event_id = EventId::from_hex(&claim_id).expect("the claim id is an event id");
    let instance = [relay_url()];
    let paid_at = Instant::now();
    let receipt = loop {
        let found = receipts_on(&instance, claim_event_id).await;
        if let Some(receipt) = found.iter().find(|receipt| {
            events::receipt_pays_bounty_claim(
                receipt,
                &claim_id,
                &issuer,
                &list_coordinate,
                &bounty_id,
                EXTERNAL_REWARD_SATS,
            )
        }) {
            break receipt.clone();
        }
        if paid_at.elapsed() >= receipt_wait {
            let elsewhere = receipts_on(&public_relays(), claim_event_id).await;
            panic!(
                "the payment left the issuer's wallet, but no receipt the issuer's zap request is \
                 inside reached the instance relay within {receipt_wait:?}. This is a finding, not \
                 a flake: {domain} did not deliver its receipt there. Receipts for the claim on \
                 the instance relay that fail the rule: {}; on the public relays: {}.",
                found.len(),
                elsewhere.len()
            );
        }
        // Drain the runtime's chatter so its failures still end the run.
        if let Some(update) = try_next(&mut handle.updates, POLL_EVERY).await {
            fail_on_error(&update);
        }
    };
    let receipt_after = paid_at.elapsed();

    handle.commands.unbounded_send(Command::Shutdown).unwrap();
    println!("\n  provider                {domain}");
    println!("  bounty                  {bounty_id}");
    println!("  claim                   {claim_id}");
    println!("  receipt                 {}", receipt.id.to_hex());
    println!("  receipt signer          {}", receipt.pubkey.to_hex());
    println!("  paid after claim        {:>7.1} s", paid_after.as_secs_f64());
    println!("  receipt after payment   {:>7.1} s", receipt_after.as_secs_f64());
    println!("  store                   {}\n", store_dir.display());
}
