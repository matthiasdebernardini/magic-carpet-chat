//! Two ready-made accounts to click around with in two app windows: a
//! claimant, and an issuer whose own trust scores rank the claimant, with a
//! funded wallet and two bounties posted. Each test drives the runtime the UI
//! drives, in the account store `MC_STORE_DIR` names (one role per store),
//! and skips every step that is already done, so a rerun is safe. They
//! publish public, permanent events and move real sats, so they are
//! `#[ignore]` and the offline suite never touches them. Run them in order:
//!
//! ```sh
//! MC_STORE_DIR=<claimant dir> mbx nextest run --run-ignored only --no-capture -E 'binary(demo_pair) and test(=demo_claimant_account)'
//! MC_STORE_DIR=<issuer dir> MC_DEMO_CLAIMANT_PUBKEY=<hex> mbx nextest run --run-ignored only --no-capture -E 'binary(demo_pair) and test(=demo_issuer_setup)'
//! MC_STORE_DIR=<claimant dir> mbx nextest run --run-ignored only --no-capture -E 'binary(demo_pair) and test(=demo_claimant_sees_bounties)'
//! ```
//!
//! The issuer run also reads:
//!
//! - `MC_DEMO_PAYER_TOKEN`: a coinos token that tops the issuer's wallet up
//!   to `MC_DEMO_FUND_SATS` (default 300). Unset: the run prints the
//!   issuer's address and skips the funding.
//! - `MC_DEMO_TRUST_WAIT_SECS` (default 3600): how long to wait for
//!   Brainstorm's first calculation.
//!
//! The nsecs, the coinos passwords and every token stay in the store files;
//! nothing here prints them.

use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use magic_carpet_chat::api::{Bounty, CreateBounty};
use magic_carpet_chat::brainstorm;
use magic_carpet_chat::coinos;
use magic_carpet_chat::events;
use magic_carpet_chat::nostr::{
    AccountInfo, Command, CommunityPart, ErrorSource, NostrHandle, SetupState, TrustPart, Update,
    relay_url, spawn_runtime, trust_relays,
};
use magic_carpet_chat::secrets::{self, Secret};
use magic_carpet_chat::trust::{self, Designation, INSTANCE_MIN_RANK};
use nostr_sdk::prelude::*;

/// The generation every community command here carries; one flow per run.
const GENERATION: u32 = 1;
/// How often the trust poll asks `/setup`.
const SETUP_POLL: Duration = Duration::from_secs(30);
/// How often the balance poll asks; the app's own panel poll uses the same.
const POLL_EVERY: Duration = Duration::from_secs(3);

/// One bounty the issuer posts, and the list it goes on.
struct DemoBounty {
    singular: &'static str,
    plural: &'static str,
    description: &'static str,
    criteria: &'static str,
    reward_sats: u64,
    cap_sats: u64,
    auto_pay: bool,
    min_rank: Option<u64>,
}

const BOUNTIES: [DemoBounty; 2] = [
    DemoBounty {
        singular: "Coffee shop in Nashville",
        plural: "Coffee shops in Nashville",
        description: "Coffee shops in Nashville, Tennessee worth a visit.",
        criteria: "Name one coffee shop in Nashville, Tennessee that is open today and not \
                   already on the list. One shop per claim.",
        reward_sats: 50,
        cap_sats: 150,
        auto_pay: true,
        min_rank: Some(2),
    },
    DemoBounty {
        singular: "Taco spot in Nashville",
        plural: "Taco spots in Nashville",
        description: "Taco spots in Nashville, Tennessee worth a visit.",
        criteria: "Name one taco spot in Nashville, Tennessee that is open today and not \
                   already on the list. One spot per claim. Paid by hand.",
        reward_sats: 50,
        cap_sats: 100,
        auto_pay: false,
        min_rank: None,
    },
];

/// One update, printed as it arrives. Every variant is public material, so
/// `{:?}` is safe. A silence longer than `deadline` fails with the step's
/// name in it.
async fn next(updates: &mut UnboundedReceiver<Update>, deadline: Duration, step: &str) -> Update {
    try_next(updates, deadline)
        .await
        .unwrap_or_else(|| panic!("{step}: no update within {deadline:?}"))
}

/// Same, but silence is `None`: the polls use it as their pacing.
async fn try_next(updates: &mut UnboundedReceiver<Update>, deadline: Duration) -> Option<Update> {
    let update = match tokio::time::timeout(deadline, updates.next()).await {
        Err(_) => return None,
        Ok(None) => panic!("runtime channel closed: the runtime thread died"),
        Ok(Some(update)) => update,
    };
    println!("[update] {update:?}");
    Some(update)
}

/// Any failure the runtime reports ends the run with its message. A wallet
/// read that failed once is the exception: the balance poll retries.
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

/// Every update for `wait`, each checked for failures.
async fn drain(handle: &mut NostrHandle, wait: Duration) {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        if let Some(update) = try_next(&mut handle.updates, left).await {
            fail_on_error(&update);
        }
    }
}

/// `MC_STORE_DIR`, created if missing. Unset or blank stops the run before
/// the runtime starts, so it can never touch the real store.
fn store_dir() -> String {
    let dir = std::env::var("MC_STORE_DIR")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "MC_STORE_DIR is not set: it must name this role's own account store folder. \
                 Nothing was done."
            )
        });
    std::fs::create_dir_all(&dir).expect("create the MC_STORE_DIR folder");
    dir
}

/// A number from the environment, or `default` when unset.
fn env_u64(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(value) => value
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name} is not a whole number. Nothing was done.")),
        Err(_) => default,
    }
}

/// The runtime, connected.
fn start() -> NostrHandle {
    let handle = spawn_runtime();
    handle.commands.unbounded_send(Command::Connect).unwrap();
    handle
}

/// `LoadAccounts`, answered: the stored accounts and the active one.
async fn load_accounts(handle: &mut NostrHandle) -> (Vec<AccountInfo>, Option<String>) {
    handle.commands.unbounded_send(Command::LoadAccounts).unwrap();
    loop {
        let update = next(&mut handle.updates, Duration::from_secs(30), "LoadAccounts").await;
        fail_on_error(&update);
        if let Update::AccountsLoaded { accounts, active } = update {
            return (accounts, active);
        }
    }
}

/// The account the store opens on, or its first one.
async fn stored_account(handle: &mut NostrHandle) -> Option<AccountInfo> {
    let (accounts, active) = load_accounts(handle).await;
    active
        .as_ref()
        .and_then(|active| accounts.iter().find(|a| &a.pubkey == active))
        .or(accounts.first())
        .cloned()
}

/// `AddAccount { open_wallet: true }` for a fresh key, resolved once the
/// wallet is open and its kind-0 with the lud16 published.
async fn add_account(handle: &mut NostrHandle, name: &str) {
    let keys = Keys::generate();
    let pubkey = keys.public_key().to_hex();
    let nsec = keys.secret_key().to_bech32().expect("nsec bech32");
    handle
        .commands
        .unbounded_send(Command::AddAccount {
            secret: Secret::new(nsec),
            name: Some(name.into()),
            open_wallet: true,
            generated: true,
        })
        .unwrap();
    let created = loop {
        let update = next(&mut handle.updates, Duration::from_secs(90), "AddAccount").await;
        fail_on_error(&update);
        if let Update::WalletCreated { pubkey: p, created } = update
            && p == pubkey
        {
            break created;
        }
    };
    assert_eq!(created.publish_error, None, "the kind-0 with the lud16 did not publish");
}

/// `CreateCoinosWallet` for a stored account that has none.
async fn open_wallet(handle: &mut NostrHandle, pubkey: &str) {
    handle
        .commands
        .unbounded_send(Command::CreateCoinosWallet {
            pubkey: pubkey.to_string(),
        })
        .unwrap();
    let created = loop {
        let update = next(&mut handle.updates, Duration::from_secs(90), "CreateCoinosWallet").await;
        fail_on_error(&update);
        if let Update::WalletCreated { pubkey: p, created } = update
            && p == pubkey
        {
            break created;
        }
    };
    assert_eq!(created.publish_error, None, "the kind-0 with the lud16 did not publish");
}

/// The stored account with a wallet: reused when the store holds one, else
/// a fresh key named `name`.
async fn ensure_account(handle: &mut NostrHandle, name: &str, step: &str) -> AccountInfo {
    match stored_account(handle).await {
        Some(account) if account.wallet.is_some() => {
            println!("[{step}] skip: the store already holds {}", account.npub);
            return account;
        }
        Some(account) => {
            println!("[{step}] the store holds {} without a wallet: opening one", account.npub);
            open_wallet(handle, &account.pubkey).await;
        }
        None => {
            println!("[{step}] the store is empty: creating \"{name}\" with a wallet");
            add_account(handle, name).await;
        }
    }
    stored_account(handle)
        .await
        .expect("the account just stored is in the store")
}

/// The account's Lightning address; every account here has a wallet.
fn address(account: &AccountInfo) -> String {
    account
        .wallet
        .as_ref()
        .map(|w| w.lightning_address.clone())
        .expect("the account has a wallet")
}

/// The next `Community` part for `pubkey` that `pick` takes.
async fn community<T>(
    handle: &mut NostrHandle,
    pubkey: &str,
    wait: Duration,
    step: &str,
    pick: impl Fn(CommunityPart) -> Option<T>,
) -> T {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{step}: no answer within {wait:?}");
        let update = next(&mut handle.updates, left, step).await;
        fail_on_error(&update);
        if let Update::Community { pubkey: p, part, .. } = update
            && p == pubkey
            && let Some(found) = pick(part)
        {
            return found;
        }
    }
}

/// One `CheckSetup`, answered.
async fn check_setup(handle: &mut NostrHandle, pubkey: &str) -> SetupState {
    handle
        .commands
        .unbounded_send(Command::CheckSetup {
            pubkey: pubkey.to_string(),
            generation: GENERATION,
        })
        .unwrap();
    community(handle, pubkey, Duration::from_secs(30), "CheckSetup", |part| match part {
        CommunityPart::SetupChecked(state) => Some(state),
        _ => None,
    })
    .await
}

/// The account's kind-10040, from `FetchTrust`'s `Designation` part.
async fn read_designation(handle: &mut NostrHandle, pubkey: &str, generation: u32) -> Designation {
    handle
        .commands
        .unbounded_send(Command::FetchTrust {
            pubkey: pubkey.to_string(),
            generation,
        })
        .unwrap();
    loop {
        let update = next(&mut handle.updates, Duration::from_secs(60), "FetchTrust").await;
        fail_on_error(&update);
        if let Update::Trust {
            pubkey: p,
            generation: g,
            part: TrustPart::Designation(result),
        } = update
            && p == pubkey
            && g == generation
        {
            return result.unwrap_or_else(|e| panic!("the Treasure Map read failed: {e}"));
        }
    }
}

/// `FetchBalance` every 3 s until the balance satisfies `enough`, or `wait`
/// runs out.
async fn poll_balance(
    handle: &mut NostrHandle,
    pubkey: &str,
    wait: Duration,
    what: &str,
    enough: impl Fn(u64) -> bool,
) -> u64 {
    let deadline = Instant::now() + wait;
    loop {
        handle
            .commands
            .unbounded_send(Command::FetchBalance {
                pubkey: pubkey.to_string(),
                request: None,
            })
            .unwrap();
        let next_ask = Instant::now() + POLL_EVERY;
        loop {
            let now = Instant::now();
            assert!(now < deadline, "{what} did not happen within {wait:?}");
            let left = next_ask.saturating_duration_since(now);
            if left.is_zero() {
                break;
            }
            if let Some(update) = try_next(&mut handle.updates, left.min(deadline - now)).await {
                fail_on_error(&update);
                if let Update::Balance { pubkey: p, sats, .. } = update
                    && p == pubkey
                    && enough(sats)
                {
                    return sats;
                }
            }
        }
    }
}

/// One `FetchBalance`, answered.
async fn read_balance(handle: &mut NostrHandle, pubkey: &str) -> u64 {
    poll_balance(handle, pubkey, Duration::from_secs(30), "a balance read", |_| true).await
}

/// Step c. `/setup` first; `CalculateTrust` only when the scores are not
/// ready and this store never had Brainstorm accept a follow list (the
/// runtime records that before it asks for the calculation, so a rerun
/// after a timeout only polls). Returns whether `CalculateTrust` was sent in
/// this run.
async fn trust_scores(handle: &mut NostrHandle, pubkey: &str) -> bool {
    if let SetupState::Ready { rows } = check_setup(handle, pubkey).await {
        println!("[step c] skip: /setup already shows the scores ready ({rows} rows)");
        return false;
    }
    let requested_before = secrets::follow_list_sent(pubkey)
        .expect("read the store")
        .is_some();
    let calculated_here = if requested_before {
        println!(
            "[step c] a calculation was already requested from this store: polling /setup, \
             not calculating again"
        );
        false
    } else {
        println!("[step c] scores not ready: sending CalculateTrust once");
        handle
            .commands
            .unbounded_send(Command::CalculateTrust {
                pubkey: pubkey.to_string(),
                generation: GENERATION,
            })
            .unwrap();
        let state = community(
            handle,
            pubkey,
            Duration::from_secs(180),
            "CalculateTrust",
            |part| match part {
                CommunityPart::Calculated(result) => Some(result),
                _ => None,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("CalculateTrust failed: {e}"));
        if let SetupState::Ready { rows } = state {
            println!("[step c] Brainstorm took the request and the scores are ready ({rows} rows)");
            return true;
        }
        true
    };

    let wait = Duration::from_secs(env_u64("MC_DEMO_TRUST_WAIT_SECS", 3600));
    let deadline = Instant::now() + wait;
    loop {
        let now = Instant::now();
        assert!(
            now < deadline,
            "Brainstorm's scores were not ready within {wait:?}. It is safe to re-run later: \
             the re-run sees the follow list already sent from this store and only polls \
             /setup, it does not ask Brainstorm to calculate again."
        );
        drain(handle, SETUP_POLL.min(deadline - now)).await;
        if let SetupState::Ready { rows } = check_setup(handle, pubkey).await {
            println!("[step c] the scores are ready ({rows} rows)");
            return calculated_here;
        }
    }
}

/// Step d without `PublishMap`, for a rerun: the runtime only publishes a
/// map in the run that asked Brainstorm to calculate, and asking again
/// would spend the rate limit. Same pieces as the runtime: the `/setup`
/// rows, `trust::map_event`, the trust relays plus the instance relay.
/// Returns how many relays accepted it.
async fn publish_map_directly(pubkey: &str) -> usize {
    let keys = secrets::keys(pubkey)
        .expect("read the store")
        .expect("the issuer's key is stored");
    let rows = brainstorm::get_setup(pubkey)
        .await
        .unwrap_or_else(|e| panic!("the /setup read failed: {e}"))
        .expect("/setup has rows: step c saw them ready");
    let event = trust::map_event(&keys, &rows);
    let client = Client::default();
    let mut urls = trust_relays();
    urls.push(trust::normalize_relay(&relay_url()));
    for url in &urls {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().and_wait(Duration::from_secs(5)).await;
    let sent = client
        .send_event(&event)
        .await
        .expect("the Treasure Map went out");
    client.disconnect().await;
    assert!(
        !sent.success.is_empty(),
        "no relay accepted the Treasure Map (tried {})",
        urls.len()
    );
    sent.success.len()
}

/// The rank `issuer`'s own scores give `subject`, the way the eligible
/// list's `issuerRank` is read: the map's `30382:rank` row names a key and
/// a relay, and that key's kind-30382 with `d` = subject on that relay
/// carries the rank. `None` when there is no such assertion yet.
async fn issuer_rank(provider: &str, relay: &str, subject: &str) -> Option<u8> {
    let provider_key = PublicKey::parse(provider).expect("the map's rank key is a public key");
    let relay = trust::normalize_relay(relay);
    let client = Client::default();
    let _ = client.add_relay(relay.as_str()).await;
    client.connect().and_wait(Duration::from_secs(5)).await;
    let filter = Filter::new()
        .kind(Kind::Custom(30382))
        .author(provider_key)
        .identifier(subject);
    let found: Vec<Event> = client
        .fetch_events(filter)
        .timeout(Duration::from_secs(10))
        .await
        .unwrap_or_else(|e| panic!("couldn't read the scores from {relay}: {e}"))
        .into_iter()
        .collect();
    client.disconnect().await;
    trust::parse_assertion(&found, provider, subject).map(|a| a.rank)
}

/// `FetchBounties`, answered: every issuer's bounties.
async fn all_bounties(handle: &mut NostrHandle, seq: u64) -> Vec<Bounty> {
    handle
        .commands
        .unbounded_send(Command::FetchBounties { seq })
        .unwrap();
    loop {
        let update = next(&mut handle.updates, Duration::from_secs(120), "FetchBounties").await;
        fail_on_error(&update);
        match update {
            Update::Bounties { seq: s, list } if s == seq => return list,
            Update::BountiesFailed { seq: s, message } if s == seq => {
                panic!("FetchBounties failed: {message}")
            }
            _ => {}
        }
    }
}

/// `PublishDList` then `CreateBounty` on the list it returns, the UI's order.
async fn post_bounty(handle: &mut NostrHandle, pubkey: &str, bounty: &DemoBounty) -> String {
    handle
        .commands
        .unbounded_send(Command::PublishDList {
            pubkey: pubkey.to_string(),
            singular: bounty.singular.into(),
            plural: bounty.plural.into(),
            description: Some(bounty.description.into()),
        })
        .unwrap();
    let list_coordinate = loop {
        let update = next(&mut handle.updates, Duration::from_secs(60), "PublishDList").await;
        fail_on_error(&update);
        if let Update::DListPublished { pubkey: p, coordinate } = update
            && p == pubkey
        {
            break coordinate;
        }
    };
    handle
        .commands
        .unbounded_send(Command::CreateBounty {
            pubkey: pubkey.to_string(),
            req: CreateBounty {
                list_coordinate,
                amount_sats: bounty.reward_sats,
                criteria: bounty.criteria.into(),
                bounty_cap_sats: Some(bounty.cap_sats),
                // The UI's shape: per-item rewards, one per npub.
                reward_per_item: true,
                max_rewards_per_npub: Some(1),
                auto_pay: bounty.auto_pay,
                auto_pay_min_rank: bounty.min_rank,
            },
        })
        .unwrap();
    loop {
        let update = next(&mut handle.updates, Duration::from_secs(60), "CreateBounty").await;
        fail_on_error(&update);
        if let Update::BountyCreated { id } = update {
            return id;
        }
    }
}

/// The `:d-tag` a bounty's list coordinate ends with.
fn list_suffix(bounty: &DemoBounty) -> String {
    format!(":{}", events::header_dtag(bounty.singular))
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn demo_claimant_account() {
    store_dir();
    let mut handle = start();
    let account = ensure_account(&mut handle, "Matthias (test)", "account").await;
    handle.commands.unbounded_send(Command::Shutdown).unwrap();
    println!("CLAIMANT_PUBKEY={}", account.pubkey);
    println!("claimant {}  {}", account.npub, address(&account));
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn demo_issuer_setup() {
    store_dir();
    let claimant = std::env::var("MC_DEMO_CLAIMANT_PUBKEY")
        .ok()
        .and_then(|v| PublicKey::parse(v.trim()).ok())
        .unwrap_or_else(|| {
            panic!(
                "MC_DEMO_CLAIMANT_PUBKEY is not set or not a public key: it must hold the \
                 CLAIMANT_PUBKEY demo_claimant_account printed. Nothing was done."
            )
        })
        .to_hex();
    let fund_sats = env_u64("MC_DEMO_FUND_SATS", 300);
    let mut handle = start();

    // a. The issuer account, with a wallet.
    let account = ensure_account(&mut handle, "David (test)", "step a").await;
    let issuer = account.pubkey.clone();
    let issuer_address = address(&account);
    assert_ne!(issuer, claimant, "the issuer store holds the claimant's key");

    // b. Follow the claimant. The runtime re-reads the contact list first
    // and publishes nothing when it already has the key.
    handle
        .commands
        .unbounded_send(Command::Follow {
            pubkey: issuer.clone(),
            generation: GENERATION,
            target: claimant.clone(),
            allow_first_list: true,
        })
        .unwrap();
    let followed = community(&mut handle, &issuer, Duration::from_secs(90), "Follow", |part| {
        match part {
            CommunityPart::Followed(result) => Some(result),
            _ => None,
        }
    })
    .await
    .unwrap_or_else(|e| panic!("Follow failed: {e:?}"));
    if followed.already {
        println!("[step b] skip: the contact list already follows the claimant");
    } else {
        println!(
            "[step b] followed the claimant: {} of {} relays accepted the kind-3",
            followed.accepted.len(),
            followed.asked
        );
    }

    // c. Trust scores.
    let calculated_here = trust_scores(&mut handle, &issuer).await;

    // d. The Treasure Map.
    let mut generation = GENERATION;
    if read_designation(&mut handle, &issuer, generation).await.rank_row().is_some() {
        println!("[step d] skip: a Treasure Map with a rank row is already published");
    } else if calculated_here {
        handle
            .commands
            .unbounded_send(Command::PublishMap {
                pubkey: issuer.clone(),
                generation: GENERATION,
            })
            .unwrap();
        let published = community(&mut handle, &issuer, Duration::from_secs(90), "PublishMap", |part| {
            match part {
                CommunityPart::MapPublished(result) => Some(result),
                _ => None,
            }
        })
        .await
        .unwrap_or_else(|e| panic!("PublishMap failed: {e}"));
        println!(
            "[step d] published the Treasure Map: {} rows, {} of {} relays accepted it",
            published.rows,
            published.accepted.len(),
            published.asked
        );
    } else {
        let accepted = publish_map_directly(&issuer).await;
        println!(
            "[step d] published the Treasure Map from the /setup rows ({accepted} relays accepted \
             it): CalculateTrust ran in an earlier run, so PublishMap would refuse"
        );
    }

    // e. The claimant's rank in the issuer's scores. The relays may lag
    // behind a map published moments ago.
    let row = loop {
        generation += 1;
        if let Some(row) = read_designation(&mut handle, &issuer, generation)
            .await
            .rank_row()
            .cloned()
        {
            break row;
        }
        assert!(
            generation < GENERATION + 6,
            "the Treasure Map has no 30382:rank row on the relays"
        );
        drain(&mut handle, Duration::from_secs(5)).await;
    };
    let rank = issuer_rank(&row.key, &row.relay, &claimant).await;
    match rank {
        Some(rank) => println!("[step e] the issuer's scores rank the claimant {rank}"),
        None => println!("[step e] the issuer's scores do not rank the claimant yet"),
    }
    let rank = rank.unwrap_or_else(|| {
        panic!(
            "no rank for the claimant from {} on {}: Brainstorm may still be publishing \
             scores. Nothing was funded or posted; re-run later.",
            row.key, row.relay
        )
    });
    assert!(
        u64::from(rank) >= INSTANCE_MIN_RANK,
        "the claimant's rank {rank} is below {INSTANCE_MIN_RANK}. Nothing was funded or posted."
    );

    // f. Funding, at most `fund_sats` in this run.
    let balance = read_balance(&mut handle, &issuer).await;
    if balance >= fund_sats {
        println!("[step f] skip: the wallet holds {balance} sats, at least {fund_sats}");
    } else {
        match std::env::var("MC_DEMO_PAYER_TOKEN") {
            Ok(token) if !token.trim().is_empty() => {
                let sats = fund_sats - balance;
                let bolt11 = coinos::lnurl_invoice(&issuer_address, sats)
                    .await
                    .expect("an invoice from the issuer's LNURL endpoint");
                coinos::pay_invoice(&Secret::new(token.trim()), &bolt11, sats)
                    .await
                    .expect("the payer token pays the issuer's invoice");
                println!("[step f] paid {sats} sats to the issuer from the token wallet");
                let funded = poll_balance(
                    &mut handle,
                    &issuer,
                    Duration::from_secs(120),
                    "the funding",
                    |s| s >= fund_sats,
                )
                .await;
                println!("[step f] the wallet holds {funded} sats");
            }
            _ => println!(
                "[step f] skip: MC_DEMO_PAYER_TOKEN is not set. The wallet holds {balance} sats; \
                 pay {} sats to {issuer_address}",
                fund_sats - balance
            ),
        }
    }

    // g. The two bounties, each skipped when its list already has one of
    // the issuer's bounties.
    let existing = all_bounties(&mut handle, 1).await;
    let mut bounty_ids = Vec::new();
    for bounty in &BOUNTIES {
        let suffix = list_suffix(bounty);
        let found = existing
            .iter()
            .find(|b| b.issuer_pubkey == issuer && b.list_coordinate.ends_with(&suffix));
        let id = match found {
            Some(b) => {
                println!("[step g] skip: \"{}\" is already bounty {}", bounty.singular, b.id);
                b.id.clone()
            }
            None => {
                let id = post_bounty(&mut handle, &issuer, bounty).await;
                println!(
                    "[step g] posted \"{}\" as bounty {id} (auto-pay {})",
                    bounty.singular,
                    if bounty.auto_pay { "on" } else { "off" }
                );
                id
            }
        };
        bounty_ids.push(id);
    }

    let balance = read_balance(&mut handle, &issuer).await;
    handle.commands.unbounded_send(Command::Shutdown).unwrap();
    println!("\n  issuer npub             {}", account.npub);
    println!("  Lightning address       {issuer_address}");
    println!("  balance                 {balance} sats");
    println!("  rank of claimant        {rank}");
    for (bounty, id) in BOUNTIES.iter().zip(&bounty_ids) {
        println!("  {:<23} {id}", bounty.singular);
    }
    println!();
}

#[ignore]
#[tokio::test(flavor = "multi_thread")]
async fn demo_claimant_sees_bounties() {
    store_dir();
    let mut handle = start();
    let account = stored_account(&mut handle)
        .await
        .expect("the claimant store is empty: run demo_claimant_account first");
    handle
        .commands
        .unbounded_send(Command::FetchEligible {
            pubkey: account.pubkey.clone(),
            seq: 1,
        })
        .unwrap();
    let list = loop {
        let update = next(&mut handle.updates, Duration::from_secs(120), "FetchEligible").await;
        fail_on_error(&update);
        match update {
            Update::Eligible { pubkey, seq: 1, list } if pubkey == account.pubkey => break list,
            Update::EligibleFailed {
                pubkey,
                seq: 1,
                message,
            } if pubkey == account.pubkey => panic!("FetchEligible failed: {message}"),
            _ => {}
        }
    };
    handle.commands.unbounded_send(Command::Shutdown).unwrap();
    for bounty in &BOUNTIES {
        let suffix = list_suffix(bounty);
        let found = list
            .iter()
            .find(|b| b.list_coordinate.ends_with(&suffix))
            .unwrap_or_else(|| panic!("\"{}\" is not in the claimant's eligible list", bounty.singular));
        let rank = found
            .issuer_rank
            .map_or_else(|| "none".to_string(), |r| r.to_string());
        println!(
            "  {:<23} {}  issuerRank {rank}  issuer {}",
            bounty.singular, found.id, found.issuer_pubkey
        );
    }
}
