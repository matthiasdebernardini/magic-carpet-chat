//! The runtime bridge: all Nostr and HTTP work on one dedicated tokio thread.
//!
//! gpui's executor is not tokio, and nostr-sdk/reqwest need tokio, so nothing
//! tokio-dependent may ever be awaited on the gpui side. The bridge is two
//! futures channels — executor-agnostic, so gpui tasks can await the update
//! stream directly:
//!
//!   UI ── Command ──▶ [nostr-runtime thread: tokio + Client + Api] ── Update ──▶ UI
//!
//! The thread owns the nostr-sdk client (instance relay) and one `Api` per
//! account (per-account cookie jars, so two accounts' sessions never mix).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as TokioMutex;

use crate::api::{Api, ApiError, Bounty, BountyDetail, CreateBounty};
use crate::coinos;
use crate::events::{self, EventError};
use crate::secrets::{self, Secret, SecretError};
use crate::trust::{self, Assertion, BrainstormKey, Designation};

pub const INSTANCE_RELAY: &str = "wss://magic-carpet.brainstorm.world/relay";

/// Where a fresh lud16 must also land: Strike, Primal, and the prod payer's
/// own lookups read the public relays, not our instance. Used only by a
/// throwaway client in `publish_lud16` — the runtime's main `client` stays
/// on the instance relay, which feeds the receipt subscriptions and the
/// status dot.
const PUBLIC_RELAYS: [&str; 3] = [
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
];

/// The read set a `FetchTrust` falls back to when the account's kind-10002
/// cannot be read — the mock's "Popular relays" list (app.jsx:113), also the
/// set every trust read unions the outbox relays with.
const TRUST_FALLBACK_RELAYS: [&str; 4] = [
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
    "wss://purplepag.es",
];

/// The instance's house issuer — PUBLIC key material only. The read paths
/// list this issuer's bounties whatever keys are stored, so the app works
/// with no key at all. `MC_ISSUER_NPUB` overrides it.
pub const HOUSE_ISSUER_HEX: &str =
    "853baa94b4b12d23931ade03ceb854a2b36cf1e24b5e3a82e68c8ca3a8ced2ba";

/// How long a claim with no `auto_payments` row keeps the fast poll. On prod
/// the watcher creates the row within one tick, so a rowless claim older than
/// this is a silent gate (rank, lud16, allowlist) — terminal server-side,
/// nothing to poll hot for.
const ROWLESS_GRACE_SECS: u64 = 600;

/// `derivedStatus` values after which a bounty can never pay again.
const TERMINAL_STATUSES: [&str; 3] = ["fulfilled", "expired", "cancelled"];

/// `MC_RELAY_URL` overrides the prod relay, for rehearsals.
pub fn relay_url() -> String {
    std::env::var("MC_RELAY_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| INSTANCE_RELAY.to_string())
}

/// UI → runtime.
#[derive(Debug, Clone)]
pub enum Command {
    /// Open the relay connection. Idempotent.
    Connect,
    /// Read the store, then each account's kind-0 from the instance. Emits
    /// `AccountsLoaded`, then `ProfileLoaded` for whichever accounts have a
    /// profile (a failed scan is silent).
    LoadAccounts,
    /// GET the issuer's bounty list (status=all). Emits `Bounties` or
    /// `BountiesFailed`.
    FetchBounties,
    PublishDList {
        pubkey: String,
        singular: String,
        plural: String,
        description: Option<String>,
    },
    CreateBounty {
        pubkey: String,
        req: CreateBounty,
    },
    PublishClaim {
        pubkey: String,
        bounty_id: String,
        name: String,
        list_coordinate: String,
    },
    /// Validate a secret (pasted or generated — the UI cannot tell the
    /// runtime apart, and need not), store it, and announce the account:
    /// `AccountAdded` (+ `ProfileLoaded` when a kind-0 exists) — or
    /// `AddAccountFailed` with a message that never echoes the input. With
    /// `open_wallet` the same task continues straight into
    /// `CreateCoinosWallet`, so one click does everything; `name` then goes
    /// into the kind-0 that step writes, when the key has none yet. The
    /// secret rides the channel inside [`Secret`], so a stray `{:?}` on a
    /// `Command` cannot leak it.
    AddAccount {
        secret: Secret,
        name: Option<String>,
        open_wallet: bool,
    },
    /// Delete the account (nsec and Coinos login) from the store, drop its
    /// session, re-emit `AccountsLoaded`.
    RemoveAccount { pubkey: String },
    /// Persist which account the app opens on. Nothing is emitted.
    SetActive { pubkey: String },
    /// Open a Coinos wallet bound to the account's key and write its
    /// Lightning address into the account's kind-0. Emits `WalletCreated`
    /// (then `ProfileLoaded` once the lud16 is on the relays) or
    /// `WalletFailed`. Idempotent: a login already stored for the account is
    /// reused, so a retry after a publish failure never opens a second one.
    CreateCoinosWallet { pubkey: String },
    /// Read the wallet balance with the stored token. Emits `Balance`, or
    /// nothing (one feed line per failure streak).
    FetchBalance { pubkey: String },
    /// Pay `sats` from this account's Coinos wallet to the Lightning address
    /// `to` (any domain: another account here, Strike, …). Emits `Sent` and
    /// then a fresh `Balance`, or `SendFailed`.
    Send {
        pubkey: String,
        to: String,
        sats: u64,
    },
    /// Follow one bounty: receipts by `#e` on the relay, plus the API's
    /// payment state machine while a claim is pending. Idempotent — a bounty
    /// already being watched is left alone. At most one bounty is watched at
    /// a time; watching a new one stops the old watcher, but its ledger (what
    /// was already reported) survives, so re-watching never replays history.
    WatchBounty { id: String },
    /// Read everything the Trust tab and the dashboard attention rows need:
    /// the account's kind-10040 designation and kind-3 contact list from its
    /// outbox relays, the Brainstorm setup endpoint, then the kind-30382
    /// assertions the map and the instance name. One `Update::Trust` per
    /// `TrustPart`, as soon as it is known. `generation` lets the UI drop a
    /// late answer from a fetch it already superseded.
    FetchTrust { pubkey: String, generation: u32 },
    Shutdown,
}

/// Which operation an [`Update::Error`] came from, so the UI can attribute a
/// failure to the form that caused it instead of to whichever form happens to
/// be submitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSource {
    /// The runtime itself (thread, tokio, relay URL).
    Runtime,
    /// LoadAccounts / RemoveAccount / SetActive (store reads and writes).
    Accounts,
    /// PublishDList.
    DList,
    /// CreateBounty.
    Bounty,
    /// PublishClaim.
    Claim,
    /// Background bounty watching (API polls, receipt subscriptions).
    Watch,
    /// FetchBalance.
    Wallet,
}

/// Runtime → UI. Every variant is a deduplicated fact, safe to render as-is.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Honest relay connectivity, emitted on every change.
    RelayStatus(bool),
    /// The store, as public material: every account in rail order, and
    /// which one the app opens on. Re-emitted after a remove.
    AccountsLoaded {
        accounts: Vec<AccountInfo>,
        active: Option<String>,
    },
    /// The account's kind-0, as far as it goes. Missing fields stay `None`
    /// and the UI falls back to its local labels.
    ProfileLoaded {
        pubkey: String,
        name: Option<String>,
        picture: Option<String>,
        /// The Lightning address payouts go to. Its absence is a first-class
        /// fact: the account screen warns, because a payout needs one.
        lud16: Option<String>,
    },
    /// A secret was rejected or could not be stored. Fixed text from
    /// [`SecretError`] — it never echoes the input.
    AddAccountFailed { message: String },
    /// A key is stored and its profile probe finished. Only public material:
    /// the npub, and whether a kind-0 was found (its fields follow in
    /// `ProfileLoaded`).
    AccountAdded {
        pubkey: String,
        npub: String,
        /// False when the profile probe itself failed (network, API): the
        /// lud16 line then says "couldn't check" instead of claiming "none".
        profile_checked: bool,
        /// The probe worked but found NO kind-0 at all — the strongest hint
        /// the pasted key is not the one its owner thinks it is (e.g. a hex
        /// PUBLIC key, which parses as a plausible secret key).
        profile_found: bool,
    },
    /// The issuer's bounty list, freshly fetched.
    Bounties(Vec<Bounty>),
    BountiesFailed(String),
    /// A fresh full snapshot of a watched bounty (emitted only on change).
    /// Boxed: the snapshot dwarfs every other variant on the channel.
    BountyDetail(Box<BountyDetail>),
    DListPublished {
        pubkey: String,
        coordinate: String,
    },
    BountyCreated {
        id: String,
    },
    ClaimPublished {
        bounty_id: String,
        event_id: String,
    },
    /// An auto_payments state transition (attempting → paid → settled | …).
    PaymentState {
        bounty_id: String,
        claim_event_id: String,
        state: String,
        reason: Option<String>,
        ts: u64,
    },
    /// A server-validated kind-9735 landed for a watched claim. The headline
    /// demo number. Emitted only from the API's `zapReceipt` — the server
    /// checks the description-tag pubkey against the issuer/delegate; a raw
    /// relay push proves nothing and never reaches this variant.
    ReceiptSeen {
        bounty_id: String,
        receipt_id: String,
        secs_from_claim: u64,
        /// The receipt event's created_at — the honest feed timestamp, so a
        /// receipt learned late never masquerades as an instant payout.
        at: u64,
    },
    /// A Coinos account exists for the account's key; see [`WalletCreated`].
    WalletCreated {
        pubkey: String,
        created: WalletCreated,
    },
    /// Fixed text from `coinos::CoinosError` or [`SecretError`]; never the
    /// password.
    WalletFailed { pubkey: String, message: String },
    /// The Coinos balance, in sats.
    Balance { pubkey: String, sats: u64 },
    /// Coinos accepted the payment: `sats` left this account's wallet for
    /// `to`. A `Balance` follows.
    Sent {
        pubkey: String,
        to: String,
        sats: u64,
    },
    /// Fixed text from `coinos::CoinosError` or the input check; never the
    /// token or the password. `may_have_paid` is the one failure that is not
    /// a "no": the payment request went out and no answer came back, so the
    /// sats may be gone. A `Balance` follows it, and the UI drops the amount
    /// so a reflex retry cannot pay twice.
    SendFailed {
        pubkey: String,
        message: String,
        may_have_paid: bool,
    },
    /// One finished piece of a `FetchTrust` — five of these arrive per fetch,
    /// each the moment its read completes rather than all at the end.
    Trust {
        pubkey: String,
        generation: u32,
        part: TrustPart,
    },
    Error {
        source: ErrorSource,
        message: String,
    },
}

/// One piece of a trust read, in the order the runtime resolves them. `Ok`
/// is a real answer (including "checked, and there is none"); `Err` is a
/// fixed message, never a relay's or endpoint's raw reply.
#[derive(Debug, Clone, PartialEq)]
pub enum TrustPart {
    /// The account's kind-10040: `Err` when every relay failed, `Ok(None…)`
    /// via [`Designation::None`] when relays answered with nothing.
    Designation(Result<Designation, String>),
    /// The Brainstorm setup endpoint's answer for this npub.
    Brainstorm(Result<BrainstormKey, String>),
    /// Distinct `p` tags on the newest valid kind-3; `Ok(None)` = none found.
    Follows(Result<Option<u32>, String>),
    /// The assertion from the rank provider the account's own map names —
    /// omitted entirely when the map has no `30382:rank` row.
    OwnAssertion(Result<Option<Assertion>, String>),
    /// The assertion from the instance's provider, plus the provider that was
    /// resolved (`None` when resolution itself failed, so the card can say
    /// which half broke).
    InstanceAssertion {
        provider: Option<(String, String)>,
        result: Result<Option<Assertion>, String>,
    },
}

/// What the wallet signup came back with. `publish_error` is set when the
/// kind-0 write did not reach the relays: the wallet is real, but the payer
/// cannot see the address yet, so the UI offers a retry. Public material
/// only — fixed text, and the password stays in the store.
#[derive(Debug, Clone, PartialEq)]
pub struct WalletCreated {
    pub username: String,
    pub lightning_address: String,
    pub publish_error: Option<String>,
}

/// One stored account, as `AccountsLoaded` announces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub pubkey: String,
    pub npub: String,
    pub wallet: Option<WalletInfo>,
}

/// The public half of a stored Coinos login. `has_token` is false for a
/// login saved before a register that never answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletInfo {
    pub username: String,
    pub lightning_address: String,
    pub has_token: bool,
}

pub struct NostrHandle {
    pub commands: mpsc::UnboundedSender<Command>,
    pub updates: mpsc::UnboundedReceiver<Update>,
}

#[derive(thiserror::Error, Debug)]
enum BridgeError {
    /// Unreachable while the UI only names stored accounts; kept so a stale
    /// pubkey (removed mid-flight) fails with words, not a panic.
    #[error("This account has no key stored")]
    NoKey,
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error(transparent)]
    Event(#[from] EventError),
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The kind-0 write. Fixed text only: the profile may hold anything its
    /// owner put there, and this string reaches the feed and the wallet panel.
    #[error("{0}")]
    Profile(&'static str),
}

/// Start the named runtime thread. Returns immediately; a thread or runtime
/// failure arrives as `Update::Error`.
pub fn spawn_runtime() -> NostrHandle {
    // The dep graph carries two rustls crypto backends: gpui-kit's http stack
    // pulls ring and aws-lc-rs through rustls's default features, and
    // reqwest's rustls-tls adds aws-lc-rs again. rustls panics at the first
    // TLS handshake rather than guess between them. Pick ring before any
    // client exists; Err just means someone chose first.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (command_tx, command_rx) = mpsc::unbounded();
    let (update_tx, update_rx) = mpsc::unbounded();

    let thread_tx = update_tx.clone();
    let spawned = std::thread::Builder::new()
        .name("nostr-runtime".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = thread_tx.unbounded_send(Update::Error {
                        source: ErrorSource::Runtime,
                        message: format!("tokio runtime failed: {e}"),
                    });
                    return;
                }
            };
            runtime.block_on(run(command_rx, thread_tx));
        });
    if let Err(e) = spawned {
        let _ = update_tx.unbounded_send(Update::Error {
            source: ErrorSource::Runtime,
            message: format!("runtime thread failed: {e}"),
        });
    }

    NostrHandle {
        commands: command_tx,
        updates: update_rx,
    }
}

struct Session {
    keys: Keys,
    api: Api,
    logged_in: bool,
}

/// One session per account (by pubkey hex), shareable across spawned command
/// tasks: the outer lock guards the map, the inner lock serialises work per
/// account — so a slow issuer submit never blocks another account's publish,
/// and neither blocks the command loop.
type SharedSessions = Arc<TokioMutex<HashMap<String, Arc<TokioMutex<Session>>>>>;

/// Accounts whose last balance read failed: the feed gets one line per
/// failure streak, not one per 3-second poll.
type BalanceFailures = Arc<std::sync::Mutex<HashSet<String>>>;

/// A running bounty watcher, as the command loop tracks it. `abort()` kills
/// the task without running its cleanup, so the current relay subscription is
/// shared out here for the command loop to unsubscribe.
struct Watcher {
    task: tokio::task::JoinHandle<()>,
    subscription: Arc<TokioMutex<Option<SubscriptionId>>>,
}

async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: mpsc::UnboundedSender<Update>,
) {
    let relay = relay_url();
    let client = Client::default();
    if let Err(e) = client.add_relay(relay.as_str()).await {
        let _ = updates.unbounded_send(Update::Error {
            source: ErrorSource::Runtime,
            message: format!("bad relay url {relay}: {e}"),
        });
    }
    let status_task = tokio::spawn(watch_relay_status(
        client.clone(),
        relay.clone(),
        updates.clone(),
    ));

    let sessions: SharedSessions = Arc::new(TokioMutex::new(HashMap::new()));
    let balance_failures: BalanceFailures = Arc::new(std::sync::Mutex::new(HashSet::new()));
    // The instance's rank provider `(key, relay)`, resolved once — the
    // issuer's designation changes rarely and resolving it costs a relay
    // round-trip per FetchTrust. Filled only on success: a failed lookup is
    // retried on the next fetch rather than cached as broken.
    let instance_provider: Arc<TokioMutex<Option<(String, String)>>> =
        Arc::new(TokioMutex::new(None));
    let mut watchers: HashMap<String, Watcher> = HashMap::new();
    // Ledgers outlive their watchers: what was already reported for a bounty
    // is permanent state, so stopping and re-watching never replays history
    // into the activity feed. Bounded by the number of bounties ever watched.
    let mut ledgers: HashMap<String, Arc<TokioMutex<WatchLedger>>> = HashMap::new();

    while let Some(command) = commands.next().await {
        match command {
            Command::Connect => {
                client.connect().await;
            }
            Command::LoadAccounts => {
                tokio::spawn(load_accounts(updates.clone()));
            }
            Command::FetchBounties => {
                tokio::spawn(fetch_bounties(updates.clone()));
            }
            Command::AddAccount {
                secret,
                name,
                open_wallet,
            } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(add_account(sessions, secret, name, open_wallet, updates));
            }
            Command::RemoveAccount { pubkey } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(remove_account(sessions, pubkey, updates));
            }
            Command::SetActive { pubkey } => {
                let updates = updates.clone();
                tokio::spawn(async move {
                    if let Err(e) = secrets::set_active(&pubkey) {
                        let _ = updates.unbounded_send(Update::Error {
                            source: ErrorSource::Accounts,
                            message: e.to_string(),
                        });
                    }
                });
            }
            Command::CreateCoinosWallet { pubkey } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(create_coinos_wallet(sessions, pubkey, None, updates));
            }
            Command::FetchBalance { pubkey } => {
                let failures = balance_failures.clone();
                let updates = updates.clone();
                tokio::spawn(fetch_balance(pubkey, failures, updates));
            }
            Command::Send { pubkey, to, sats } => {
                let failures = balance_failures.clone();
                let updates = updates.clone();
                tokio::spawn(send_sats(pubkey, to, sats, failures, updates));
            }
            Command::PublishDList {
                pubkey,
                singular,
                plural,
                description,
            } => {
                // Spawned: a stalled publish must not block the commands
                // queued behind it (another account's work, watches).
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(async move {
                    let result = publish_dlist(
                        &sessions,
                        &pubkey,
                        &singular,
                        &plural,
                        description.as_deref(),
                    )
                    .await;
                    let update = match result {
                        Ok(coordinate) => Update::DListPublished {
                            pubkey,
                            coordinate,
                        },
                        Err(e) => Update::Error {
                            source: ErrorSource::DList,
                            message: e.to_string(),
                        },
                    };
                    let _ = updates.unbounded_send(update);
                });
            }
            Command::CreateBounty { pubkey, req } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(async move {
                    let update = match create_bounty(&sessions, &pubkey, &req).await {
                        Ok(id) => Update::BountyCreated { id },
                        Err(e) => Update::Error {
                            source: ErrorSource::Bounty,
                            message: e.to_string(),
                        },
                    };
                    let _ = updates.unbounded_send(update);
                });
            }
            Command::PublishClaim {
                pubkey,
                bounty_id,
                name,
                list_coordinate,
            } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(async move {
                    let update =
                        match publish_claim(&sessions, &pubkey, &name, &list_coordinate).await {
                            Ok(event_id) => Update::ClaimPublished {
                                bounty_id,
                                event_id,
                            },
                            Err(e) => Update::Error {
                                source: ErrorSource::Claim,
                                message: e.to_string(),
                            },
                        };
                    let _ = updates.unbounded_send(update);
                });
            }
            Command::WatchBounty { id } => {
                if watchers.get(&id).is_some_and(|w| !w.task.is_finished()) {
                    // Already live: restarting it would only cost an extra
                    // poll and an extra snapshot. The ledger already guards
                    // against replays, so there is nothing to refresh.
                    continue;
                }
                // One live watcher at a time: browsing a list of N bounties
                // must not leave N pollers hitting prod. abort() skips the
                // task's own cleanup, so its subscription is closed here.
                for (_, watcher) in watchers.drain() {
                    watcher.task.abort();
                    if let Some(sub) = watcher.subscription.lock().await.take() {
                        let _ = client.unsubscribe(&sub).await;
                    }
                }
                let ledger = ledgers
                    .entry(id.clone())
                    .or_insert_with(|| {
                        Arc::new(TokioMutex::new(WatchLedger::new(&id)))
                    })
                    .clone();
                let subscription = Arc::new(TokioMutex::new(None));
                let task = tokio::spawn(watch_bounty(
                    client.clone(),
                    id.clone(),
                    updates.clone(),
                    ledger,
                    subscription.clone(),
                ));
                watchers.insert(id, Watcher { task, subscription });
            }
            Command::FetchTrust { pubkey, generation } => {
                let provider = instance_provider.clone();
                let updates = updates.clone();
                tokio::spawn(fetch_trust(pubkey, generation, provider, updates));
            }
            Command::Shutdown => break,
        }
    }

    status_task.abort();
    for (_, watcher) in watchers {
        watcher.task.abort();
    }
    client.disconnect().await;
}

/// Lazily open one session per account: keys from the store, a fresh `Api`
/// with its own cookie jar.
async fn session(
    sessions: &SharedSessions,
    pubkey: &str,
) -> Result<Arc<TokioMutex<Session>>, BridgeError> {
    let mut map = sessions.lock().await;
    if let Some(existing) = map.get(pubkey) {
        return Ok(existing.clone());
    }
    let keys = secrets::keys(pubkey)?.ok_or(BridgeError::NoKey)?;
    let api = Api::new()?;
    let session = Arc::new(TokioMutex::new(Session {
        keys,
        api,
        logged_in: false,
    }));
    map.insert(pubkey.to_string(), session.clone());
    Ok(session)
}

async fn publish_dlist(
    sessions: &SharedSessions,
    pubkey: &str,
    singular: &str,
    plural: &str,
    description: Option<&str>,
) -> Result<String, BridgeError> {
    let session = session(sessions, pubkey).await?;
    let session = session.lock().await;
    let event = events::dlist_header(&session.keys, singular, plural, description)?;
    // Publishing through the instance needs no login; the event is signed.
    session.api.publish_event(&event).await?;
    let dtag = events::header_dtag(singular.trim());
    Ok(events::header_coordinate(&session.keys.public_key(), &dtag))
}

async fn create_bounty(
    sessions: &SharedSessions,
    pubkey: &str,
    req: &CreateBounty,
) -> Result<String, BridgeError> {
    let session = session(sessions, pubkey).await?;
    let mut session = session.lock().await;
    // The bounty issuer is always the session pubkey, so log in first.
    if !session.logged_in {
        session.api.login(&session.keys).await?;
        session.logged_in = true;
    }
    let bounty = session.api.create_bounty(req).await?;
    Ok(bounty.id)
}

async fn publish_claim(
    sessions: &SharedSessions,
    pubkey: &str,
    name: &str,
    list_coordinate: &str,
) -> Result<String, BridgeError> {
    let session = session(sessions, pubkey).await?;
    let session = session.lock().await;
    let event = events::claim(&session.keys, name, list_coordinate)?;
    session.api.publish_event(&event).await?;
    Ok(event.id.to_hex())
}

/// One account's kind-0 content through the instance's public scan endpoint
/// (kind 0 is replaceable, so the relay holds at most the latest). `Ok(None)`:
/// the scan worked and found no profile. `Err(())`: the scan itself failed —
/// the caller decides whether that is silent (startup) or shown (the account
/// screen).
async fn fetch_kind0(api: &Api, pubkey: &str) -> Result<Option<String>, ()> {
    let filter = serde_json::json!({ "kinds": [0], "authors": [pubkey], "limit": 1 });
    let events = api.scan(&filter).await.map_err(|_| ())?;
    Ok(events.into_iter().next().map(|event| event.content))
}

/// The kind-0 fields the UI shows, read field by field from the JSON so one
/// odd value elsewhere in the profile hides nothing else. `name` is
/// `display_name` first, then `name`; a non-string or blank value counts as
/// absent, so a profile holding `"name": ""` still falls back to the local
/// label. Unparseable or non-object content is an empty profile: the
/// fallback labels are the defined behaviour when nothing is known.
#[derive(Debug, Default, PartialEq)]
struct Profile {
    name: Option<String>,
    picture: Option<String>,
    lud16: Option<String>,
}

impl Profile {
    fn from_value(value: &serde_json::Value) -> Self {
        let field = |key: &str| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        Self {
            name: field("display_name").or_else(|| field("name")),
            picture: field("picture"),
            lud16: field("lud16"),
        }
    }
}

/// A kind-0's content read field by field; see [`Profile`].
fn parse_profile(content: &str) -> Profile {
    serde_json::from_str(content)
        .map(|value: serde_json::Value| Profile::from_value(&value))
        .unwrap_or_default()
}

/// The `ProfileLoaded` an account's kind-0 amounts to.
fn profile_loaded(pubkey: String, profile: Profile) -> Update {
    Update::ProfileLoaded {
        pubkey,
        name: profile.name,
        picture: profile.picture,
        lud16: profile.lud16,
    }
}

/// The new kind-0: the existing profile with ONLY `lud16` changed — plus
/// `name`, when one was given and the profile has none (the create-account
/// path; a name set elsewhere always wins).
///
/// The same nsec may also live in a phone app or a browser extension, so this
/// app is not the only writer of this profile. Merge, never rebuild — a
/// kind-0 made from nothing wipes the name and picture set elsewhere on every
/// relay that accepts it. The content is handled field by field as plain
/// JSON: every other key is carried through untouched, `null` values and
/// wrong-typed fields included. `None` existing content means "confirmed no
/// profile", and only that may start from a blank one; anything that is not
/// a JSON object is left alone rather than overwritten.
fn merge_lud16(
    existing: Option<&str>,
    lud16: &str,
    name: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, BridgeError> {
    let mut profile = match existing.map(str::trim) {
        None | Some("") => serde_json::Map::new(),
        Some(text) => match serde_json::from_str(text) {
            Ok(serde_json::Value::Object(map)) => map,
            _ => {
                return Err(BridgeError::Profile(
                    "Your current profile could not be read, so it was left alone",
                ));
            }
        },
    };
    profile.insert("lud16".into(), serde_json::Value::String(lud16.to_string()));
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty())
        && Profile::from_value(&serde_json::Value::Object(profile.clone()))
            .name
            .is_none()
    {
        profile.insert("name".into(), serde_json::Value::String(name.to_string()));
    }
    Ok(profile)
}

/// Write `lud16` into the account's kind-0 and push it to the instance AND the
/// public relays. Returns the merged profile so the caller can announce it.
///
/// A scan failure is an error, not an empty profile: a timed-out read used to
/// look exactly like "no profile" in the desktop app, and the merge then
/// published a kind-0 holding nothing but lud16.
async fn publish_lud16(
    keys: &Keys,
    api: &Api,
    lud16: &str,
    name: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, BridgeError> {
    let existing = fetch_kind0(api, &keys.public_key().to_hex())
        .await
        .map_err(|_| {
            BridgeError::Profile("Could not read your current profile from the instance")
        })?;
    let profile = merge_lud16(existing.as_deref(), lud16, name)?;
    let content = serde_json::Value::Object(profile.clone()).to_string();
    let event = EventBuilder::new(Kind::Metadata, content)
        .finalize(keys)
        .map_err(|_| BridgeError::Profile("Could not sign the profile update"))?;

    // The instance first: it is what the prod payer reads.
    api.publish_event(&event)
        .await
        .map_err(|_| BridgeError::Profile("The instance relay did not accept the profile update"))?;

    // Then the public relays, through a client that lives for this one send.
    // A relay that refuses is fine as long as one accepts; none accepting is
    // a publish failure, because Strike-side lookups would still miss it.
    let client = Client::default();
    for url in PUBLIC_RELAYS {
        let _ = client.add_relay(url).await;
    }
    client.connect().and_wait(Duration::from_secs(10)).await;
    let sent = client.send_event(&event).await;
    client.disconnect().await;
    match sent {
        Ok(output) if !output.success.is_empty() => Ok(profile),
        _ => Err(BridgeError::Profile(
            "The relays did not accept the profile update",
        )),
    }
}

/// Fixed text for the one state a retry cannot fix: the login was saved
/// before a register that never answered, the wallet turned out to exist,
/// and the token it answered with is gone. `/api/login` has a captcha, so
/// there is no second way to get one.
const NO_TOKEN: &str = "Wallet exists but this app has no token for it; remove the account and \
     create a new wallet. Your Coinos username and password still work on coinos.io.";

/// The wallet signup, entirely on the runtime thread. Order matters:
///
/// 1. Store the login (empty token) on the account record.
/// 2. `POST /api/register`; store the token it answers with. A refusal
///    deletes the login (nothing was created); any other failure keeps it
///    (the account may exist).
/// 3. Merge the lud16 (and `name`, if the profile has none) into the kind-0
///    and publish it.
///
/// A login already stored with a token (a retry after a publish failure)
/// skips to step 3. One stored WITHOUT a token probes coinos first: no
/// wallet → register with the same login; a wallet → [`NO_TOKEN`], never a
/// second account.
async fn create_coinos_wallet(
    sessions: SharedSessions,
    pubkey: String,
    name: Option<String>,
    updates: mpsc::UnboundedSender<Update>,
) {
    let fail = |message: String| {
        let _ = updates.unbounded_send(Update::WalletFailed {
            pubkey: pubkey.clone(),
            message,
        });
    };
    let (keys, api) = match session(&sessions, &pubkey).await {
        Ok(session) => {
            let session = session.lock().await;
            (session.keys.clone(), session.api.clone())
        }
        Err(e) => return fail(e.to_string()),
    };

    let stored = match secrets::load_coinos_login(&pubkey) {
        Ok(stored) => stored,
        Err(e) => return fail(e.to_string()),
    };
    let login = match stored {
        Some(login) if login.has_token() => login,
        Some(login) => match coinos::wallet_exists(&login.username).await {
            Ok(true) => return fail(NO_TOKEN.into()),
            Ok(false) => match register_wallet(&pubkey, login).await {
                Ok(login) => login,
                Err(message) => return fail(message),
            },
            Err(e) => return fail(e.to_string()),
        },
        None => {
            let login = coinos::Login::fresh();
            if let Err(e) = secrets::store_coinos_login(&pubkey, &login) {
                return fail(e.to_string());
            }
            match register_wallet(&pubkey, login).await {
                Ok(login) => login,
                Err(message) => return fail(message),
            }
        }
    };

    let lightning_address = coinos::lightning_address(&login.username);
    let published = publish_lud16(&keys, &api, &lightning_address, name.as_deref()).await;
    // The wallet exists either way; a publish failure travels inside
    // `WalletCreated` so the panel can show it next to the retry button.
    let _ = updates.unbounded_send(Update::WalletCreated {
        pubkey: pubkey.clone(),
        created: WalletCreated {
            username: login.username,
            lightning_address,
            publish_error: published.as_ref().err().map(ToString::to_string),
        },
    });
    if let Ok(profile) = published {
        let profile = Profile::from_value(&serde_json::Value::Object(profile));
        let _ = updates.unbounded_send(profile_loaded(pubkey, profile));
    }
}

/// Step 2 of the signup: register, then store the token next to the login.
/// The error is the fixed text the panel shows.
async fn register_wallet(pubkey: &str, mut login: coinos::Login) -> Result<coinos::Login, String> {
    match coinos::create_wallet(&login.username, &login.password, pubkey).await {
        Ok(token) => {
            login.token = token;
            secrets::store_coinos_login(pubkey, &login).map_err(|e| e.to_string())?;
            Ok(login)
        }
        Err(e) => {
            if e.account_definitely_not_created() {
                let _ = secrets::forget_coinos_login(pubkey);
            }
            Err(e.to_string())
        }
    }
}

/// The balance read behind the "Waiting for sats…" line. Silent without a
/// token; on failure, one feed line per streak.
/// A wallet opened by another install of this app (or on coinos.io itself)
/// can be funded and shown here, but only the register token pays.
const CANNOT_SEND: &str = "This wallet cannot send from this app; it was opened elsewhere";

/// `Command::Send`: the stored token pays an invoice pulled from the
/// recipient's own LNURL endpoint, then the balance is re-read so the panel
/// drops at once instead of on the next poll.
async fn send_sats(
    pubkey: String,
    to: String,
    sats: u64,
    failures: BalanceFailures,
    updates: mpsc::UnboundedSender<Update>,
) {
    let fail = |message: String| {
        let _ = updates.unbounded_send(Update::SendFailed {
            pubkey: pubkey.clone(),
            message,
            may_have_paid: false,
        });
    };
    let login = match secrets::load_coinos_login(&pubkey) {
        Ok(Some(login)) if login.has_token() => login,
        Ok(_) => return fail(CANNOT_SEND.into()),
        Err(e) => return fail(e.to_string()),
    };
    if sats == 0 {
        return fail("Enter an amount in sats".into());
    }
    let to = to.trim().to_string();
    if coinos::split_address(&to).is_none() {
        return fail("Enter a Lightning address like name@domain.com".into());
    }
    let bolt11 = match coinos::lnurl_invoice(&to, sats).await {
        Ok(bolt11) => bolt11,
        Err(e) => return fail(e.to_string()),
    };
    match coinos::pay_invoice(&login.token, &bolt11, sats).await {
        Ok(()) => {
            let _ = updates.unbounded_send(Update::Sent {
                pubkey: pubkey.clone(),
                to,
                sats,
            });
        }
        Err(e @ coinos::CoinosError::OutcomeUnknown) => {
            let _ = updates.unbounded_send(Update::SendFailed {
                pubkey: pubkey.clone(),
                message: e.to_string(),
                may_have_paid: true,
            });
        }
        Err(e) => return fail(e.to_string()),
    }
    // Either way the wallet may have changed: the panel shows the new
    // balance at once, not on the next poll.
    fetch_balance(pubkey, failures, updates).await;
}

async fn fetch_balance(
    pubkey: String,
    failures: BalanceFailures,
    updates: mpsc::UnboundedSender<Update>,
) {
    let login = match secrets::load_coinos_login(&pubkey) {
        Ok(Some(login)) if login.has_token() => login,
        _ => return,
    };
    let mark = |failing: bool| -> bool {
        let mut set = failures.lock().unwrap_or_else(|p| p.into_inner());
        if failing {
            set.insert(pubkey.clone())
        } else {
            set.remove(&pubkey)
        }
    };
    match coinos::balance(&login.token).await {
        Ok(sats) => {
            mark(false);
            let _ = updates.unbounded_send(Update::Balance { pubkey, sats });
        }
        Err(e) => {
            if mark(true) {
                let _ = updates.unbounded_send(Update::Error {
                    source: ErrorSource::Wallet,
                    message: format!("Balance check failed: {e}"),
                });
            }
        }
    }
}

/// Announce the store as public material, then each account's kind-0.
/// Secrets never leave [`crate::secrets`]. A store that cannot be read is
/// reported and treated as empty — the account screen then opens, and the
/// first save fails with the same message.
async fn load_accounts(updates: mpsc::UnboundedSender<Update>) {
    let store = match secrets::load() {
        Ok(store) => store,
        Err(e) => {
            let _ = updates.unbounded_send(Update::Error {
                source: ErrorSource::Accounts,
                message: e.to_string(),
            });
            secrets::Store::default()
        }
    };
    let accounts = store
        .accounts
        .iter()
        .map(|account| AccountInfo {
            pubkey: account.pubkey.clone(),
            npub: account.npub(),
            wallet: account.coinos.as_ref().map(|login| WalletInfo {
                username: login.username.clone(),
                lightning_address: coinos::lightning_address(&login.username),
                has_token: login.has_token(),
            }),
        })
        .collect();
    if updates
        .unbounded_send(Update::AccountsLoaded {
            accounts,
            active: store.active.clone(),
        })
        .is_err()
    {
        return;
    }

    // A scan failure is not an error state: the UI's fallback labels are
    // the defined behaviour when no profile is known. A CONFIRMED-empty
    // profile is reported, though: the UI pre-fills a stored wallet's address
    // from the store, and only a real profile read can say whether the
    // relays carry that lud16 yet.
    let Ok(api) = Api::new() else { return };
    for account in &store.accounts {
        if let Ok(content) = fetch_kind0(&api, &account.pubkey).await {
            let _ = updates.unbounded_send(profile_loaded(
                account.pubkey.clone(),
                content.map(|c| parse_profile(&c)).unwrap_or_default(),
            ));
        }
    }
}

/// The add-account path, run entirely on the runtime thread so the UI never
/// touches the store: validate, store, drop any stale session, announce the
/// account and probe its profile — then, if asked, open the wallet in the
/// same breath. Every message that leaves here is public material or fixed
/// text.
async fn add_account(
    sessions: SharedSessions,
    secret: Secret,
    name: Option<String>,
    open_wallet: bool,
    updates: mpsc::UnboundedSender<Update>,
) {
    let record = match secrets::add_account(&secret) {
        Ok(record) => record,
        Err(e) => {
            let _ = updates.unbounded_send(Update::AddAccountFailed {
                message: e.to_string(),
            });
            return;
        }
    };
    // A cached session for a re-added key is harmless but stale; drop it.
    sessions.lock().await.remove(&record.pubkey);

    let probe = match Api::new() {
        Ok(api) => fetch_kind0(&api, &record.pubkey).await,
        Err(_) => Err(()),
    };
    let (profile, profile_checked) = match probe {
        Ok(profile) => (profile, true),
        Err(()) => (None, false),
    };
    let profile_found = profile.is_some();
    // The account lands on the UI first, so the profile that follows has a
    // view to land on.
    if updates
        .unbounded_send(Update::AccountAdded {
            pubkey: record.pubkey.clone(),
            npub: record.npub,
            profile_checked,
            profile_found,
        })
        .is_err()
    {
        return;
    }
    if let Some(content) = profile {
        let _ = updates.unbounded_send(profile_loaded(
            record.pubkey.clone(),
            parse_profile(&content),
        ));
    }
    if open_wallet {
        create_coinos_wallet(sessions, record.pubkey, name, updates).await;
    }
}

/// Delete the account from the store, drop the cached session, then
/// re-announce the store.
async fn remove_account(
    sessions: SharedSessions,
    pubkey: String,
    updates: mpsc::UnboundedSender<Update>,
) {
    if let Err(e) = secrets::remove_account(&pubkey) {
        let _ = updates.unbounded_send(Update::Error {
            source: ErrorSource::Accounts,
            message: e.to_string(),
        });
        return;
    }
    sessions.lock().await.remove(&pubkey);
    load_accounts(updates).await;
}

/// The issuer whose bounties the read paths list: `MC_ISSUER_NPUB` (npub or
/// hex), else the instance's house issuer. No stored key changes it — a
/// person who pastes their own nsec must not end up looking at their own
/// (empty) bounty list. Public material only.
pub fn issuer_pubkey() -> String {
    issuer_pubkey_from(std::env::var("MC_ISSUER_NPUB").ok().as_deref())
}

fn issuer_pubkey_from(env_value: Option<&str>) -> String {
    env_value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| PublicKey::parse(v).ok())
        .map(|pk| pk.to_hex())
        .unwrap_or_else(|| HOUSE_ISSUER_HEX.to_string())
}

/// An account IS the issuer when its pubkey is the one the read paths list.
/// That account gets the bounty form; everyone else gets the claim form.
pub fn is_issuer(pubkey: &str) -> bool {
    pubkey == issuer_pubkey()
}

async fn fetch_bounties(updates: mpsc::UnboundedSender<Update>) {
    let result = async {
        let api = Api::new()?;
        Ok::<_, BridgeError>(api.list_bounties(&issuer_pubkey()).await?)
    }
    .await;
    let update = match result {
        Ok(bounties) => Update::Bounties(bounties),
        Err(e) => Update::BountiesFailed(e.to_string()),
    };
    let _ = updates.unbounded_send(update);
}

/// Poll the relay's real status so the UI dot is honest, not hopeful.
async fn watch_relay_status(
    client: Client,
    relay: String,
    updates: mpsc::UnboundedSender<Update>,
) {
    let mut last: Option<bool> = None;
    loop {
        let connected = match client.relay(relay.as_str()).await {
            Ok(Some(relay)) => relay.status() == RelayStatus::Connected,
            _ => false,
        };
        if last != Some(connected) {
            if updates
                .unbounded_send(Update::RelayStatus(connected))
                .is_err()
            {
                return;
            }
            last = Some(connected);
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// The pure state of one bounty watch: which transitions and receipts were
/// already reported. Split from the network loop so the dedup rules are
/// testable offline against the prod fixture. Owned by the command loop and
/// handed to each (re)spawned watcher, so restarts never forget.
struct WatchLedger {
    bounty_id: String,
    claim_created: HashMap<String, u64>,
    last_payment: HashMap<String, (String, Option<String>)>,
    seen_receipts: HashSet<String>,
    /// Relay-pushed receipt ids that already triggered an immediate poll —
    /// each unvalidated push buys at most one extra API round-trip.
    relay_triggered: HashSet<String>,
}

impl WatchLedger {
    fn new(bounty_id: impl Into<String>) -> Self {
        Self {
            bounty_id: bounty_id.into(),
            claim_created: HashMap::new(),
            last_payment: HashMap::new(),
            seen_receipts: HashSet::new(),
            relay_triggered: HashSet::new(),
        }
    }

    /// Fold in one API snapshot. Returns the updates that are new, and
    /// whether any claim is still pending — meaning a payment is genuinely in
    /// flight, so the fast poll is warranted.
    ///
    /// A claim with a payment row is pending while the row is (`attempting`
    /// or `paid`). A claim WITHOUT a row is pending only while the server
    /// could still create one: the bounty pays automatically, is still open,
    /// the server calls the claim `payable`, and the claim is younger than
    /// [`ROWLESS_GRACE_SECS`] — after that a missing row means a silent gate
    /// (rank, lud16, allowlist), which is terminal and never gets a row.
    fn ingest_detail(&mut self, detail: &BountyDetail, now: u64) -> (Vec<Update>, bool) {
        let bounty_can_still_pay = detail.bounty.auto_pay_on()
            && detail.bounty.effective_status() == "open";
        let mut out = Vec::new();
        let mut pending = false;
        for claim in &detail.claims {
            let claim_ts = claim.event.created_at.unwrap_or(0);
            self.claim_created.insert(claim.event.id.clone(), claim_ts);
            match &claim.auto_payment {
                Some(row) => {
                    let state_now = (row.state.clone(), row.reason.clone());
                    if self.last_payment.get(&row.claim_event_id) != Some(&state_now) {
                        out.push(Update::PaymentState {
                            bounty_id: self.bounty_id.clone(),
                            claim_event_id: row.claim_event_id.clone(),
                            state: row.state.clone(),
                            reason: row.reason.clone(),
                            ts: row.updated_at.or(row.created_at).unwrap_or(0),
                        });
                        self.last_payment.insert(row.claim_event_id.clone(), state_now);
                    }
                    if row.is_pending() {
                        pending = true;
                    }
                }
                None => {
                    let payable = claim
                        .payment_status
                        .as_deref()
                        .is_none_or(|status| status == "payable");
                    if bounty_can_still_pay
                        && payable
                        && now.saturating_sub(claim_ts) < ROWLESS_GRACE_SECS
                    {
                        pending = true;
                    }
                }
            }
            if let Some(receipt) = claim.receipt()
                && self.seen_receipts.insert(receipt.receipt_id.clone()) {
                    out.push(Update::ReceiptSeen {
                        bounty_id: self.bounty_id.clone(),
                        receipt_id: receipt.receipt_id,
                        secs_from_claim: receipt.created_at.saturating_sub(claim_ts),
                        at: receipt.created_at,
                    });
                }
        }
        (out, pending)
    }

    /// A kind-9735 pushed by the relay. The push is a poll trigger, never a
    /// fact: the server accepts a receipt only when its description-tag
    /// pubkey is the issuer or the issuer's auto-pay delegate
    /// (magic-carpet-v2/src/api/bounties.js), and this client cannot know the
    /// delegate — so any ordinary zap (or the junk test receipt) on a claim
    /// event must never be announced. Returns true when the receipt names a
    /// claim this watch knows, has not been reported by the API, and has not
    /// already bought its one immediate poll.
    fn note_relay_receipt(&mut self, receipt: &events::Receipt) -> bool {
        let names_known_claim = receipt
            .claim_event_ids
            .iter()
            .any(|id| self.claim_created.contains_key(id));
        if !names_known_claim || self.seen_receipts.contains(&receipt.receipt_id) {
            return false;
        }
        self.relay_triggered.insert(receipt.receipt_id.clone())
    }

    fn claim_ids(&self) -> BTreeSet<String> {
        self.claim_created.keys().cloned().collect()
    }
}

/// Whether a bounty watch may stop for good. `paid_unreceipted`
/// (`receipt_timeout`) is NOT terminal for the watch while the bounty is
/// open: the money already left the wallet and the receipt can still import
/// late, and only the poll notices the server row flip to `settled` (or a
/// `zapReceipt` appear). The watch stops only when the bounty itself can
/// never pay again AND nothing is in flight.
fn watch_done(detail: &BountyDetail, pending: bool) -> bool {
    TERMINAL_STATUSES.contains(&detail.bounty.effective_status()) && !pending
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn watch_bounty(
    client: Client,
    bounty_id: String,
    updates: mpsc::UnboundedSender<Update>,
    ledger: Arc<TokioMutex<WatchLedger>>,
    sub_slot: Arc<TokioMutex<Option<SubscriptionId>>>,
) {
    let api = match Api::new() {
        Ok(api) => api,
        Err(e) => {
            let _ = updates.unbounded_send(Update::Error {
                source: ErrorSource::Watch,
                message: e.to_string(),
            });
            return;
        }
    };
    let mut notifications = client.notifications();
    let mut push_alive = true;
    let mut subscribed: BTreeSet<String> = BTreeSet::new();
    let mut last_error: Option<String> = None;
    let mut error_streak: u32 = 0;
    let mut last_detail: Option<BountyDetail> = None;

    loop {
        // The API snapshot: payment state machine + server-validated receipts.
        let interval = match api.get_bounty(&bounty_id).await {
            Ok(detail) => {
                error_streak = 0;
                last_error = None;
                // The snapshot goes out before the derived facts, so a UI
                // resolving a claim name against it never runs ahead of it.
                if last_detail.as_ref() != Some(&detail) {
                    if updates
                        .unbounded_send(Update::BountyDetail(Box::new(detail.clone())))
                        .is_err()
                    {
                        return;
                    }
                    last_detail = Some(detail.clone());
                }
                let (new_updates, pending) =
                    ledger.lock().await.ingest_detail(&detail, now_unix());
                for update in new_updates {
                    if updates.unbounded_send(update).is_err() {
                        return;
                    }
                }
                // A bounty that can never pay again, with nothing in flight,
                // needs no watcher at all: stop. A later re-watch (the user
                // re-selecting it) does one fresh poll and stops again. An
                // open bounty with a paid_unreceipted claim is NOT done — the
                // slow poll below is what catches a late receipt.
                if watch_done(&detail, pending) {
                    if let Some(sub) = sub_slot.lock().await.take() {
                        let _ = client.unsubscribe(&sub).await;
                    }
                    return;
                }
                if pending {
                    2
                } else if detail.bounty.auto_pay_on() {
                    10
                } else {
                    // Open but manual-pay: nothing moves without the API poll,
                    // and nothing moves fast. Keep the pane alive, slowly.
                    30
                }
            }
            Err(e) => {
                let message = format!("watch {bounty_id}: {e}");
                if last_error.as_deref() != Some(message.as_str()) {
                    if updates
                        .unbounded_send(Update::Error {
                            source: ErrorSource::Watch,
                            message: message.clone(),
                        })
                        .is_err()
                    {
                        return;
                    }
                    last_error = Some(message);
                }
                // Back off while the API is unreachable: 2, 4, … capped at 60.
                error_streak = error_streak.saturating_add(1).min(6);
                (1u64 << error_streak).min(60)
            }
        };

        // Keep the relay subscription pointed at the current claim set, so a
        // receipt push can cut the poll interval short.
        let claim_ids = ledger.lock().await.claim_ids();
        if claim_ids != subscribed && !claim_ids.is_empty() {
            if let Some(old) = sub_slot.lock().await.take() {
                // A failed unsubscribe leaves a stale filter on the relay; the
                // dedup in the ledger makes its extra pushes harmless.
                let _ = client.unsubscribe(&old).await;
            }
            let ids: Vec<EventId> = claim_ids
                .iter()
                .filter_map(|hex| EventId::from_hex(hex).ok())
                .collect();
            let filter = Filter::new()
                .kind(Kind::Custom(events::RECEIPT_KIND))
                .events(ids);
            match client.subscribe(filter).await {
                Ok(output) => {
                    *sub_slot.lock().await = Some(output.value);
                    subscribed = claim_ids;
                }
                Err(e) => {
                    let message = format!("receipt subscription failed: {e}");
                    if last_error.as_deref() != Some(message.as_str()) {
                        if updates
                            .unbounded_send(Update::Error {
                                source: ErrorSource::Watch,
                                message: message.clone(),
                            })
                            .is_err()
                        {
                            return;
                        }
                        last_error = Some(message);
                    }
                }
            }
        }

        // Wait out the poll interval, draining relay pushes as they arrive. A
        // push that plausibly concerns a watched claim ends the wait early:
        // the very next API poll is what validates and announces it.
        let sleep = tokio::time::sleep(Duration::from_secs(interval));
        tokio::pin!(sleep);
        loop {
            if !push_alive {
                (&mut sleep).await;
                break;
            }
            tokio::select! {
                _ = &mut sleep => break,
                notification = notifications.next() => match notification {
                    Some(ClientNotification::Event { event, .. }) => {
                        // Verified first: the relay can serve anything.
                        if event.verify().is_ok()
                            && let Some(receipt) = events::parse_receipt(&event)
                            && ledger.lock().await.note_relay_receipt(&receipt)
                        {
                            break; // Poll now; the API decides what it means.
                        }
                    }
                    Some(ClientNotification::Shutdown) | None => push_alive = false,
                    Some(_) => {}
                },
            }
        }
        if updates.is_closed() {
            return;
        }
    }
}

/// A `FetchTrust`, on a throwaway client: the runtime's connected `client`
/// belongs to the instance relay, and trust reads must not grow its relay
/// set or its subscriptions. Emits each [`TrustPart`] the moment it is
/// known, so a slow relay never holds back the parts that already landed.
async fn fetch_trust(
    pubkey: String,
    generation: u32,
    instance_provider: Arc<TokioMutex<Option<(String, String)>>>,
    updates: mpsc::UnboundedSender<Update>,
) {
    let send = |part: TrustPart| {
        let _ = updates.unbounded_send(Update::Trust {
            pubkey: pubkey.clone(),
            generation,
            part,
        });
    };

    let client = Client::default();
    for url in TRUST_FALLBACK_RELAYS {
        let _ = client.add_relay(url).await;
    }
    client.connect().await;

    // The account's outbox relays (kind-10002), unioned with the fallback
    // set; a failed 10002 read quietly yields just the fallbacks.
    let outbox = outbox_relays(&client, &pubkey).await;
    for url in &outbox {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().await;

    // Independent reads, concurrently: designation, contact list, setup.
    let (designation, follows, brainstorm) = futures::join!(
        read_designation(&client, &pubkey, &outbox),
        read_follows(&client, &pubkey, &outbox),
        fetch_brainstorm_key(&pubkey),
    );
    send(TrustPart::Designation(designation.clone()));
    send(TrustPart::Follows(follows));
    send(TrustPart::Brainstorm(brainstorm));

    // The provider the account's own map names — skipped entirely when the
    // map is missing or has no rank row (there is no provider to read).
    if let Some(row) = designation.as_ref().ok().and_then(Designation::rank_row) {
        send(TrustPart::OwnAssertion(
            read_assertion(&client, &row.key, &row.relay, &pubkey).await,
        ));
    }

    // The provider the instance's issuer designates.
    match resolve_instance_provider(&client, &instance_provider).await {
        Some((key, relay)) => {
            let result = read_assertion(&client, &key, &relay, &pubkey).await;
            send(TrustPart::InstanceAssertion {
                provider: Some((key, relay)),
                result,
            });
        }
        None => send(TrustPart::InstanceAssertion {
            provider: None,
            result: Err("Couldn't determine the instance's rank provider.".into()),
        }),
    }

    client.disconnect().await;
}

/// The relays the account claims to write to: `r` tags of its newest
/// kind-10002 that carry no marker or marker `write`, unioned with
/// [`TRUST_FALLBACK_RELAYS`]. A failed or absent 10002 yields the fallback
/// set on its own — never an error.
async fn outbox_relays(client: &Client, pubkey: &str) -> Vec<String> {
    let mut urls: BTreeSet<String> = TRUST_FALLBACK_RELAYS
        .iter()
        .map(|url| trust::normalize_relay(url))
        .collect();
    let Ok(public) = PublicKey::parse(pubkey) else {
        return urls.into_iter().collect();
    };
    let filter = Filter::new()
        .kind(Kind::Custom(10002))
        .author(public)
        .limit(1);
    let read = client
        .fetch_events(filter)
        .timeout(Duration::from_secs(10))
        .await;
    if let Ok(events) = read
        && let Some(event) = events.iter().max_by_key(|event| event.created_at)
    {
        for tag in event.tags.iter() {
            let parts = tag.as_slice();
            let Some(url) = parts.get(1) else { continue };
            let url = trust::normalize_relay(url);
            let marker = parts.get(2).map(String::as_str);
            if parts.first().map(String::as_str) == Some("r")
                && marker.is_none_or(|m| m == "write")
                && (url.starts_with("wss://") || url.starts_with("ws://"))
            {
                urls.insert(url);
            }
        }
    }
    urls.into_iter().collect()
}

/// Run `filter` against each relay individually and keep the results apart:
/// `Ok` means that relay answered (EOSE), `Err` means it failed — the
/// difference between "no event exists" and "nobody answered".
async fn read_per_relay(
    client: &Client,
    urls: &[String],
    filter: Filter,
) -> Vec<(String, Result<Vec<Event>, ()>)> {
    futures::future::join_all(urls.iter().map(|url| {
        let url = url.clone();
        let filter = filter.clone();
        async move {
            let relay = match client.relay(url.as_str()).await {
                Ok(Some(relay)) => relay,
                _ => return (url, Err(())),
            };
            let events = relay
                .fetch_events(filter)
                .timeout(Duration::from_secs(10))
                .policy(ReqExitPolicy::ExitOnEOSE)
                .await
                .map(|events| events.into_iter().collect())
                .map_err(|_| ());
            (url, events)
        }
    }))
    .await
}

/// `Ok` events from the answering relays, and how many answered.
fn answered(results: Vec<(String, Result<Vec<Event>, ()>)>) -> (usize, Vec<(String, Event)>) {
    let mut answered = 0;
    let mut events = Vec::new();
    for (url, result) in results {
        if let Ok(found) = result {
            answered += 1;
            events.extend(found.into_iter().map(|event| (url.clone(), event)));
        }
    }
    (answered, events)
}

/// kind-10040 per relay. `Err` only when every relay failed; an answered
/// relay set with no valid event is `Ok(Designation::None)`.
async fn read_designation(
    client: &Client,
    pubkey: &str,
    outbox: &[String],
) -> Result<Designation, String> {
    let Ok(public) = PublicKey::parse(pubkey) else {
        return Err("Couldn't read kind-10040 from the relays.".into());
    };
    let filter = Filter::new().kind(Kind::Custom(10040)).author(public);
    let results = read_per_relay(client, outbox, filter).await;
    let total = results.len();
    let (answered, events) = answered(results);
    if answered == 0 {
        return Err(format!("Couldn't read kind-10040 from {total} relays."));
    }
    Ok(trust::parse_designation(&events, pubkey))
}

/// The newest valid kind-3's distinct `p` count. Same failure rule as the
/// designation: every relay failing is `Err`, answered-but-empty is
/// `Ok(None)`.
async fn read_follows(
    client: &Client,
    pubkey: &str,
    outbox: &[String],
) -> Result<Option<u32>, String> {
    let Ok(public) = PublicKey::parse(pubkey) else {
        return Err("Couldn't read kind-3 from the relays.".into());
    };
    let filter = Filter::new()
        .kind(Kind::ContactList)
        .author(public)
        .limit(1);
    let results = read_per_relay(client, outbox, filter).await;
    let total = results.len();
    let (answered, events) = answered(results);
    if answered == 0 {
        return Err(format!("Couldn't read kind-3 from {total} relays."));
    }
    Ok(events
        .iter()
        .filter_map(|(_, event)| {
            trust::follow_count(event, pubkey).map(|n| (event.created_at, n))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, n)| n))
}

/// The newest valid kind-30382 `provider` signed about `subject` on `relay`
/// — the relay is the one the map (or the issuer's map) names, so the read
/// is deliberately single-relay: an assertion anywhere else is not the
/// provider's word.
async fn read_assertion(
    client: &Client,
    provider: &str,
    relay: &str,
    subject: &str,
) -> Result<Option<Assertion>, String> {
    let Ok(provider_key) = PublicKey::parse(provider) else {
        return Err("Couldn't read the provider's assertions.".into());
    };
    let relay = trust::normalize_relay(relay);
    let _ = client.add_relay(relay.as_str()).await;
    client.connect().await;
    let filter = Filter::new()
        .kind(Kind::Custom(30382))
        .author(provider_key)
        .identifier(subject);
    let results = read_per_relay(client, &[relay.clone()], filter).await;
    match results.into_iter().next() {
        Some((_, Ok(events))) => {
            let mut assertion = trust::parse_assertion(&events, provider, subject);
            if let Some(assertion) = &mut assertion {
                assertion.relay = relay.clone();
            }
            Ok(assertion)
        }
        _ => Err(format!("Couldn't read {relay}.")),
    }
}

/// `GET https://api.brainstorm.world/setup/{hex}`: 404 means the npub has no
/// Brainstorm account (`Ok(NoAccount)`); any other failure is `Err` with
/// fixed wording — the endpoint's own error text is never shown.
async fn fetch_brainstorm_key(hex: &str) -> Result<BrainstormKey, String> {
    let http = reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "Couldn't reach the Brainstorm setup service.".to_string())?;
    let response = http
        .get(format!("{}{hex}", trust::BRAINSTORM_SETUP))
        .send()
        .await
        .map_err(|_| "Couldn't reach the Brainstorm setup service.".to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(BrainstormKey::NoAccount);
    }
    if !response.status().is_success() {
        return Err("The Brainstorm setup service returned an error.".into());
    }
    let body = response
        .text()
        .await
        .map_err(|_| "The Brainstorm setup answer could not be read.".to_string())?;
    trust::parse_setup(&body)
        .ok_or_else(|| "The Brainstorm setup answer could not be read.".to_string())
}

/// `(key, relay)` of the provider the instance scores from: the issuer's own
/// kind-10040 rank row, falling back to its Brainstorm setup answer. Cached
/// on first success only — a failure is retried on the next `FetchTrust`.
async fn resolve_instance_provider(
    client: &Client,
    cache: &Arc<TokioMutex<Option<(String, String)>>>,
) -> Option<(String, String)> {
    if let Some(cached) = cache.lock().await.clone() {
        return Some(cached);
    }
    let issuer = issuer_pubkey();
    let outbox = outbox_relays(client, &issuer).await;
    for url in &outbox {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().await;
    let resolved = match read_designation(client, &issuer, &outbox).await {
        Ok(designation) => designation
            .rank_row()
            .map(|row| (row.key.clone(), row.relay.clone())),
        Err(_) => None,
    };
    let resolved = match resolved {
        Some(provider) => Some(provider),
        None => match fetch_brainstorm_key(&issuer).await {
            Ok(BrainstormKey::Assigned { key, relay }) => Some((key, relay)),
            _ => None,
        },
    };
    if let Some(provider) = &resolved {
        *cache.lock().await = Some(provider.clone());
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::parse_detail;

    const FIXTURE: &str = include_str!("../tests/fixtures/bounty-5f44688e.json");
    const CLAIM_ID: &str = "afb4bfe5b6556aa4a65a3c51a82d0e974c234dbe9551963d347208aa5d4ebc85";
    /// Any "now" later than the fixture's timestamps.
    const NOW: u64 = 1_786_100_000;

    #[test]
    fn a_snapshot_reports_each_fact_exactly_once() {
        let detail = parse_detail(FIXTURE).unwrap();
        let mut ledger = WatchLedger::new("5f44688e-fcb3-4c6a-bee6-e287995f7d3e");

        let (updates, pending) = ledger.ingest_detail(&detail, NOW);
        assert!(!pending, "the settled fixture holds nothing pending");
        assert_eq!(
            updates,
            vec![
                Update::PaymentState {
                    bounty_id: "5f44688e-fcb3-4c6a-bee6-e287995f7d3e".into(),
                    claim_event_id: CLAIM_ID.into(),
                    state: "settled".into(),
                    reason: None,
                    ts: 1_786_093_481,
                },
                Update::ReceiptSeen {
                    bounty_id: "5f44688e-fcb3-4c6a-bee6-e287995f7d3e".into(),
                    receipt_id:
                        "b3b3f887aab52d97796777aeac056f72ddaf745fd98329a3fcdb9ac37ba332c7".into(),
                    // Claim 1786093454 → receipt 1786093481.
                    secs_from_claim: 27,
                    at: 1_786_093_481,
                },
            ]
        );

        // The same snapshot again: everything already reported, nothing new.
        let (updates, pending) = ledger.ingest_detail(&detail, NOW);
        assert!(updates.is_empty());
        assert!(!pending);
        assert_eq!(ledger.claim_ids().len(), 1);

        // A watcher restart hands the SAME ledger to the new task, so even a
        // fresh ingest of the same snapshot stays silent (no feed replays).
    }

    #[test]
    fn a_relay_receipt_never_announces_and_triggers_at_most_one_poll() {
        let detail = parse_detail(FIXTURE).unwrap();
        let mut ledger = WatchLedger::new("5f44688e-fcb3-4c6a-bee6-e287995f7d3e");
        let _ = ledger.ingest_detail(&detail, NOW);

        // The relay pushes the receipt the API already validated and
        // reported: stale, no extra poll.
        let receipt = detail.claims[0].receipt().unwrap();
        assert!(!ledger.note_relay_receipt(&receipt));

        // A fresh receipt naming a known claim: worth exactly one immediate
        // poll — and no ReceiptSeen, because only the server-validated copy
        // (claims[].zapReceipt) may announce a payment.
        let fresh = events::Receipt {
            receipt_id: "ff".repeat(32),
            created_at: 1_786_093_454 + 21,
            claim_event_ids: vec![CLAIM_ID.into()],
            payer_pubkey: None,
            sender_pubkey: None,
            amount_msats: Some(100_000),
        };
        assert!(ledger.note_relay_receipt(&fresh));
        assert!(!ledger.note_relay_receipt(&fresh), "one poll per receipt id");

        // A receipt for a claim this watch has never seen (like the junk test
        // receipt on the instance relay) is not even worth a poll.
        let stranger = events::Receipt {
            receipt_id: "ee".repeat(32),
            created_at: 1_786_093_500,
            claim_event_ids: vec!["d3".repeat(32)],
            payer_pubkey: None,
            sender_pubkey: None,
            amount_msats: None,
        };
        assert!(!ledger.note_relay_receipt(&stranger));
    }

    #[test]
    fn a_late_receipt_still_lands_after_paid_unreceipted() {
        // The live failure mode (rehearsal 08-01, "toronto" 08-11): the
        // server times out waiting for the receipt and parks the row at
        // paid_unreceipted — then the receipt imports later anyway.
        let mut early: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        early["bounty"]["derivedStatus"] = "open".into();
        early["claims"][0]["autoPayment"]["state"] = "paid_unreceipted".into();
        early["claims"][0]["autoPayment"]["reason"] = "receipt_timeout".into();
        early["claims"][0].as_object_mut().unwrap().remove("zapReceipt");
        let early = parse_detail(&early.to_string()).unwrap();

        let mut ledger = WatchLedger::new("5f44688e-fcb3-4c6a-bee6-e287995f7d3e");
        let (updates, pending) = ledger.ingest_detail(&early, NOW);
        assert_eq!(
            updates,
            vec![Update::PaymentState {
                bounty_id: "5f44688e-fcb3-4c6a-bee6-e287995f7d3e".into(),
                claim_event_id: CLAIM_ID.into(),
                state: "paid_unreceipted".into(),
                reason: Some("receipt_timeout".into()),
                ts: 1_786_093_481,
            }]
        );
        assert!(!pending, "no receipt decision is in flight — no fast poll");
        // The watch must stay LIVE: the bounty is open, so the slow poll
        // keeps running and can still see the row flip.
        assert!(
            !watch_done(&early, pending),
            "paid_unreceipted on an open bounty must not stop the watch"
        );

        // Later the server row flips to settled and the receipt imports. The
        // pristine fixture IS that snapshot: exactly one PaymentState
        // (settled) and one ReceiptSeen come out — the card and feed update.
        let late = parse_detail(FIXTURE).unwrap();
        let (updates, _) = ledger.ingest_detail(&late, NOW);
        assert_eq!(
            updates,
            vec![
                Update::PaymentState {
                    bounty_id: "5f44688e-fcb3-4c6a-bee6-e287995f7d3e".into(),
                    claim_event_id: CLAIM_ID.into(),
                    state: "settled".into(),
                    reason: None,
                    ts: 1_786_093_481,
                },
                Update::ReceiptSeen {
                    bounty_id: "5f44688e-fcb3-4c6a-bee6-e287995f7d3e".into(),
                    receipt_id:
                        "b3b3f887aab52d97796777aeac056f72ddaf745fd98329a3fcdb9ac37ba332c7".into(),
                    secs_from_claim: 27,
                    at: 1_786_093_481,
                },
            ]
        );

        // And only once: the same settled snapshot again stays silent.
        let (updates, _) = ledger.ingest_detail(&late, NOW);
        assert!(updates.is_empty());
    }

    #[test]
    fn merging_lud16_keeps_every_other_profile_field() {
        let existing = r#"{"name":"Matthias","picture":"https://x/p.png","about":"hi","nip05":"m@x.io","custom":{"deep":1}}"#;
        let merged = merge_lud16(Some(existing), "carpet1234@coinos.io", None).unwrap();
        assert_eq!(merged["name"], "Matthias");
        assert_eq!(merged["picture"], "https://x/p.png");
        assert_eq!(merged["about"], "hi");
        assert_eq!(merged["nip05"], "m@x.io");
        assert_eq!(merged["custom"]["deep"], 1);
        assert_eq!(merged["lud16"], "carpet1234@coinos.io");
        // What goes on the wire: every original key plus lud16, nothing else.
        assert_eq!(merged.len(), 6);

        // An existing lud16 is replaced, not duplicated.
        let merged = merge_lud16(Some(r#"{"lud16":"old@strike.me","name":"M"}"#), "new@coinos.io", None).unwrap();
        assert_eq!(merged["lud16"], "new@coinos.io");
        assert_eq!(merged["name"], "M");
        assert_eq!(merged.len(), 2);

        // A wrong-typed field elsewhere survives as it was.
        let merged = merge_lud16(Some(r#"{"name":"Alice","about":42}"#), "a@coinos.io", None).unwrap();
        assert_eq!(merged["name"], "Alice");
        assert_eq!(merged["about"], 42);

        // A wrong-typed lud16 is replaced and the rest kept.
        let merged = merge_lud16(Some(r#"{"name":"Alice","lud16":123}"#), "a@coinos.io", None).unwrap();
        assert_eq!(merged["lud16"], "a@coinos.io");
        assert_eq!(merged["name"], "Alice");

        // A null value keeps its key.
        let merged = merge_lud16(Some(r#"{"about":null}"#), "a@coinos.io", None).unwrap();
        assert!(merged.contains_key("about"));
        assert!(merged["about"].is_null());

        // Only a confirmed-absent profile may start from blank.
        let blank = serde_json::json!({ "lud16": "new@coinos.io" });
        assert_eq!(merge_lud16(None, "new@coinos.io", None).unwrap(), *blank.as_object().unwrap());
        assert_eq!(merge_lud16(Some(""), "new@coinos.io", None).unwrap(), *blank.as_object().unwrap());

        // Content that is not an object is left alone rather than clobbered.
        assert!(merge_lud16(Some("[1,2]"), "x@y.z", None).is_err());
        assert!(merge_lud16(Some("not json"), "x@y.z", None).is_err());
    }

    #[test]
    fn a_new_accounts_name_only_fills_an_empty_profile() {
        // The create path: no kind-0 yet, so the name goes in with the lud16.
        let merged = merge_lud16(None, "a@coinos.io", Some(" Ada ")).unwrap();
        assert_eq!(merged["name"], "Ada");
        assert_eq!(merged.len(), 2);
        // A blank name is no name.
        assert!(!merge_lud16(None, "a@coinos.io", Some("  ")).unwrap().contains_key("name"));
        // A name set elsewhere wins, whichever field carries it.
        let merged = merge_lud16(Some(r#"{"name":"Alice"}"#), "a@coinos.io", Some("Ada")).unwrap();
        assert_eq!(merged["name"], "Alice");
        let merged =
            merge_lud16(Some(r#"{"display_name":"Alice"}"#), "a@coinos.io", Some("Ada")).unwrap();
        assert!(!merged.contains_key("name"));
        // A profile with an empty name is a profile with no name.
        let merged = merge_lud16(Some(r#"{"name":""}"#), "a@coinos.io", Some("Ada")).unwrap();
        assert_eq!(merged["name"], "Ada");
    }

    #[test]
    fn the_issuer_is_the_env_override_or_the_house_key() {
        let house = PublicKey::from_hex(HOUSE_ISSUER_HEX).unwrap();
        assert_eq!(issuer_pubkey_from(None), HOUSE_ISSUER_HEX);
        assert_eq!(issuer_pubkey_from(Some("")), HOUSE_ISSUER_HEX);
        assert_eq!(issuer_pubkey_from(Some("not a key")), HOUSE_ISSUER_HEX);
        // npub or hex, either way the hex comes out.
        assert_eq!(
            issuer_pubkey_from(Some(&house.to_bech32().unwrap())),
            HOUSE_ISSUER_HEX
        );
        let other = Keys::generate().public_key();
        assert_eq!(
            issuer_pubkey_from(Some(&format!(" {} ", other.to_bech32().unwrap()))),
            other.to_hex()
        );
        assert_eq!(issuer_pubkey_from(Some(&other.to_hex())), other.to_hex());
    }

    #[test]
    fn a_profile_is_read_field_by_field() {
        let profile = parse_profile(r#"{"name":"Alice","about":42}"#);
        assert_eq!(profile.name.as_deref(), Some("Alice"));
        let profile = parse_profile(r#"{"display_name":"","name":"A"}"#);
        assert_eq!(profile.name.as_deref(), Some("A"));
        assert_eq!(parse_profile("[1,2]"), Profile::default());
        assert_eq!(parse_profile("not json"), Profile::default());
    }

    #[test]
    fn a_wallet_command_and_update_carry_no_secret() {
        // Neither side of the wallet channel may hold the password or the
        // token: the command names an account, the update names public
        // material only.
        let printed = format!(
            "{:?}",
            Command::CreateCoinosWallet {
                pubkey: "aa".repeat(32)
            }
        );
        assert_eq!(printed, format!("CreateCoinosWallet {{ pubkey: \"{}\" }}", "aa".repeat(32)));
        let update = Update::WalletCreated {
            pubkey: "aa".repeat(32),
            created: WalletCreated {
                username: "carpetab12cd34".into(),
                lightning_address: "carpetab12cd34@coinos.io".into(),
                publish_error: Some("The relays did not accept the profile update".into()),
            },
        };
        assert!(!format!("{update:?}").contains("password"));
    }

    #[test]
    fn an_add_account_command_debugs_without_its_secret() {
        // The command channel is the one place a pasted key travels outside
        // `secrets`; a `{:?}` on it must stay clean.
        let command = Command::AddAccount {
            secret: Secret::new("nsec1extremelysecretvalue"),
            name: Some("Ada".into()),
            open_wallet: true,
        };
        let printed = format!("{command:?}");
        assert!(!printed.contains("extremelysecret"), "leaked: {printed}");
        assert!(printed.contains("<redacted>"), "not redacted: {printed}");
    }

    /// A one-claim detail with no payment row, shaped like the live API.
    fn rowless_detail(
        auto_pay: u64,
        derived_status: &str,
        payment_status: &str,
        claim_created_at: u64,
    ) -> BountyDetail {
        let json = format!(
            r#"{{
                "success": true,
                "bounty": {{"id":"b1","issuer_pubkey":"aa","list_coordinate":"39998:aa:x",
                            "amount_sats":100,"auto_pay":{auto_pay},
                            "derivedStatus":"{derived_status}"}},
                "claims": [{{"event":{{"id":"c1","pubkey":"bb","kind":39999,
                             "created_at":{claim_created_at},"tags":[],"content":"Nashville"}},
                            "paymentStatus":"{payment_status}"}}]
            }}"#
        );
        parse_detail(&json).unwrap()
    }

    #[test]
    fn a_rowless_claim_is_pending_only_while_the_server_could_still_pay_it() {
        let now = 1_000_000;
        let fresh = now - 30;
        let stale = now - ROWLESS_GRACE_SECS;

        // Auto-pay armed, open, payable, fresh: the row is due any tick.
        let detail = rowless_detail(1, "open", "payable", fresh);
        let (updates, pending) = WatchLedger::new("b1").ingest_detail(&detail, now);
        assert!(updates.is_empty(), "no payment row means nothing to report");
        assert!(pending, "a row is still expected — keep polling fast");

        // Past the grace window: a silent gate (rank, lud16, allowlist)
        // never creates a row. Terminal server-side; poll slow.
        let (_, pending) =
            WatchLedger::new("b1").ingest_detail(&rowless_detail(1, "open", "payable", stale), now);
        assert!(!pending, "a rowless claim past the grace window is terminal");

        // Manual-pay bounties never create rows at all.
        let (_, pending) =
            WatchLedger::new("b1").ingest_detail(&rowless_detail(0, "open", "payable", fresh), now);
        assert!(!pending, "auto-pay off: no row will ever appear");

        // The server already refused (cap or per-npub limit): terminal.
        let (_, pending) =
            WatchLedger::new("b1").ingest_detail(&rowless_detail(1, "open", "closed", fresh), now);
        assert!(!pending, "a closed claim never gets a row");

        // A bounty that can never pay again is terminal regardless of age.
        let (_, pending) = WatchLedger::new("b1")
            .ingest_detail(&rowless_detail(1, "fulfilled", "payable", fresh), now);
        assert!(!pending, "a fulfilled bounty pays nobody new");
    }
}
