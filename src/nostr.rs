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
//! account (per-account cookie jars, so the issuer's and the claimant's
//! sessions never mix).

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
use crate::secrets::{self, Account, Secret, SecretError};

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

/// The instance's house issuer — PUBLIC key material only. Read-only browsing
/// lists this issuer's bounties when no issuer key is imported, so the app
/// works with no key at all. `MC_ISSUER_NPUB` overrides it.
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
    /// Read both account keys (keychain or env), then each account's kind-0
    /// from the instance. Emits `AccountLoaded`/`AccountMissing`, then
    /// `ProfileLoaded` for whichever accounts have a profile.
    LoadAccounts,
    /// GET the issuer's bounty list (status=all). Emits `Bounties` or
    /// `BountiesFailed`.
    FetchBounties,
    PublishDList {
        account: Account,
        singular: String,
        plural: String,
        description: Option<String>,
    },
    CreateBounty {
        account: Account,
        req: CreateBounty,
    },
    PublishClaim {
        account: Account,
        bounty_id: String,
        name: String,
        list_coordinate: String,
    },
    /// Validate a pasted secret, store it in the keychain, and announce the
    /// account: `AccountLoaded` (+ `ProfileLoaded` when a kind-0 exists), then
    /// `ImportReady` with the onboarding summary — or `ImportFailed` with a
    /// message that never echoes the input. The secret rides the channel
    /// inside [`Secret`], so a stray `{:?}` on a `Command` cannot leak it.
    ImportKey { account: Account, secret: Secret },
    /// Delete the account's keychain entry, then re-announce whatever is
    /// still true (an env override survives a forget — env wins).
    ForgetKey { account: Account },
    /// Open a Coinos wallet bound to the account's key and write its
    /// Lightning address into the account's kind-0. Emits `WalletCreated`
    /// (then `ProfileLoaded` once the lud16 is on the relays) or
    /// `WalletFailed`. Idempotent: a login already in the keychain for the
    /// same pubkey is reused, so a retry after a publish failure never opens
    /// a second account.
    CreateCoinosWallet { account: Account },
    /// Follow one bounty: receipts by `#e` on the relay, plus the API's
    /// payment state machine while a claim is pending. Idempotent — a bounty
    /// already being watched is left alone. At most one bounty is watched at
    /// a time; watching a new one stops the old watcher, but its ledger (what
    /// was already reported) survives, so re-watching never replays history.
    WatchBounty { id: String },
    Shutdown,
}

/// Which operation an [`Update::Error`] came from, so the UI can attribute a
/// failure to the form that caused it instead of to whichever form happens to
/// be submitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSource {
    /// The runtime itself (thread, tokio, relay URL).
    Runtime,
    /// LoadAccounts (keychain reads, profile fetches).
    Accounts,
    /// PublishDList.
    DList,
    /// CreateBounty.
    Bounty,
    /// PublishClaim.
    Claim,
    /// Background bounty watching (API polls, receipt subscriptions).
    Watch,
}

/// Runtime → UI. Every variant is a deduplicated fact, safe to render as-is.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Honest relay connectivity, emitted on every change.
    RelayStatus(bool),
    /// An account's key exists; only public material leaves the runtime.
    AccountLoaded {
        account: Account,
        pubkey: String,
        npub: String,
    },
    /// No key stored for this account — the UI offers the paste-a-key import
    /// instead of inventing an identity.
    AccountMissing { account: Account },
    /// The account's kind-0, as far as it goes. Missing fields stay `None`
    /// and the UI falls back to its local labels.
    ProfileLoaded {
        account: Account,
        name: Option<String>,
        picture: Option<String>,
        /// The Lightning address payouts go to. Its absence is a first-class
        /// fact: the onboarding warns, because a payout needs one.
        lud16: Option<String>,
    },
    /// A pasted key was rejected. The message is fixed text from
    /// [`SecretError`] — it never echoes the input.
    ImportFailed { account: Account, message: String },
    /// A pasted key is stored and its profile probe finished. Only public
    /// material: the npub, and whether a kind-0 was found (its fields went
    /// out in `ProfileLoaded` just before).
    ImportReady {
        account: Account,
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
        account: Account,
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
        account: Account,
        created: WalletCreated,
    },
    /// Fixed text from `coinos::CoinosError` or [`SecretError`]; never the
    /// password.
    WalletFailed { account: Account, message: String },
    Error {
        source: ErrorSource,
        message: String,
    },
}

/// What the wallet signup came back with. `publish_error` is set when the
/// kind-0 write did not reach the relays: the wallet is real, but the payer
/// cannot see the address yet, so the UI offers a retry. Public material
/// only — fixed text, and the password stays in the keychain.
#[derive(Debug, Clone, PartialEq)]
pub struct WalletCreated {
    pub username: String,
    pub lightning_address: String,
    pub publish_error: Option<String>,
}

pub struct NostrHandle {
    pub commands: mpsc::UnboundedSender<Command>,
    pub updates: mpsc::UnboundedReceiver<Update>,
}

#[derive(thiserror::Error, Debug)]
enum BridgeError {
    #[error("You need a {0} key for this — paste your nsec in the sidebar first")]
    NoKey(Account),
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

/// One session per account, shareable across spawned command tasks: the outer
/// lock guards the map, the inner lock serialises work per account — so a slow
/// issuer submit never blocks the claimant's publish, and neither blocks the
/// command loop.
type SharedSessions = Arc<TokioMutex<HashMap<Account, Arc<TokioMutex<Session>>>>>;

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
            Command::ImportKey { account, secret } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(import_key(sessions, account, secret, updates));
            }
            Command::ForgetKey { account } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(forget_key(sessions, account, updates));
            }
            Command::CreateCoinosWallet { account } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(create_coinos_wallet(sessions, account, updates));
            }
            Command::PublishDList {
                account,
                singular,
                plural,
                description,
            } => {
                // Spawned: a stalled publish must not block the commands
                // queued behind it (the other account's work, watches).
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(async move {
                    let result = publish_dlist(
                        &sessions,
                        account,
                        &singular,
                        &plural,
                        description.as_deref(),
                    )
                    .await;
                    let update = match result {
                        Ok(coordinate) => Update::DListPublished {
                            account,
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
            Command::CreateBounty { account, req } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(async move {
                    let update = match create_bounty(&sessions, account, &req).await {
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
                account,
                bounty_id,
                name,
                list_coordinate,
            } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(async move {
                    let update =
                        match publish_claim(&sessions, account, &name, &list_coordinate).await {
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
            Command::Shutdown => break,
        }
    }

    status_task.abort();
    for (_, watcher) in watchers {
        watcher.task.abort();
    }
    client.disconnect().await;
}

/// Lazily open one session per account: keys from the keychain (or the env
/// override), a fresh `Api` with its own cookie jar.
async fn session(
    sessions: &SharedSessions,
    account: Account,
) -> Result<Arc<TokioMutex<Session>>, BridgeError> {
    let mut map = sessions.lock().await;
    if let Some(existing) = map.get(&account) {
        return Ok(existing.clone());
    }
    let keys = secrets::keys(account)?.ok_or(BridgeError::NoKey(account))?;
    let api = Api::new()?;
    let session = Arc::new(TokioMutex::new(Session {
        keys,
        api,
        logged_in: false,
    }));
    map.insert(account, session.clone());
    Ok(session)
}

async fn publish_dlist(
    sessions: &SharedSessions,
    account: Account,
    singular: &str,
    plural: &str,
    description: Option<&str>,
) -> Result<String, BridgeError> {
    let session = session(sessions, account).await?;
    let session = session.lock().await;
    let event = events::dlist_header(&session.keys, singular, plural, description)?;
    // Publishing through the instance needs no login; the event is signed.
    session.api.publish_event(&event).await?;
    let dtag = events::header_dtag(singular.trim());
    Ok(events::header_coordinate(&session.keys.public_key(), &dtag))
}

async fn create_bounty(
    sessions: &SharedSessions,
    account: Account,
    req: &CreateBounty,
) -> Result<String, BridgeError> {
    let session = session(sessions, account).await?;
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
    account: Account,
    name: &str,
    list_coordinate: &str,
) -> Result<String, BridgeError> {
    let session = session(sessions, account).await?;
    let session = session.lock().await;
    let event = events::claim(&session.keys, name, list_coordinate)?;
    session.api.publish_event(&event).await?;
    Ok(event.id.to_hex())
}

/// One account's kind-0 content through the instance's public scan endpoint
/// (kind 0 is replaceable, so the relay holds at most the latest). `Ok(None)`:
/// the scan worked and found no profile. `Err(())`: the scan itself failed —
/// the caller decides whether that is silent (startup) or shown (onboarding).
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
fn profile_loaded(account: Account, profile: Profile) -> Update {
    Update::ProfileLoaded {
        account,
        name: profile.name,
        picture: profile.picture,
        lud16: profile.lud16,
    }
}

/// The new kind-0: the existing profile with ONLY `lud16` changed.
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
) -> Result<serde_json::Map<String, serde_json::Value>, BridgeError> {
    let existing = fetch_kind0(api, &keys.public_key().to_hex())
        .await
        .map_err(|_| {
            BridgeError::Profile("Could not read your current profile from the instance")
        })?;
    let profile = merge_lud16(existing.as_deref(), lud16)?;
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

/// The wallet signup, entirely on the runtime thread. Order matters:
///
/// 1. Store the login in the keychain.
/// 2. `POST /api/register`. A refusal deletes the login (nothing was
///    created); any other failure keeps it (the account may exist).
/// 3. Merge the lud16 into the kind-0 and publish it.
///
/// A login already in the keychain for this pubkey (a retry) skips step 1 and
/// probes coinos before deciding whether step 2 is still needed — so a signup
/// that timed out on the way back never becomes two accounts, and one that
/// never happened is not skipped. A login for a different pubkey (the slot
/// was re-imported) is replaced; an entry this app cannot read is left alone
/// and reported, because it may be the only copy of a real password.
async fn create_coinos_wallet(
    sessions: SharedSessions,
    account: Account,
    updates: mpsc::UnboundedSender<Update>,
) {
    let fail = |message: String| {
        let _ = updates.unbounded_send(Update::WalletFailed { account, message });
    };
    let (keys, api) = match session(&sessions, account).await {
        Ok(session) => {
            let session = session.lock().await;
            (session.keys.clone(), session.api.clone())
        }
        Err(e) => return fail(e.to_string()),
    };
    let pubkey = keys.public_key().to_hex();

    let stored = match secrets::load_coinos_login(account) {
        Ok(Some(secret)) => match coinos::Login::decode(&secret) {
            Some(login) => Some(login),
            None => {
                return fail(format!(
                    "The saved Coinos login for this account is unreadable — remove the \
                     {} keychain entry and try again.",
                    account.coinos_entry_key()
                ));
            }
        },
        Ok(None) => None,
        Err(e) => return fail(e.to_string()),
    };
    let (login, fresh) = coinos::Login::reuse_or_fresh(stored, &pubkey);
    if fresh && let Err(e) = secrets::store_coinos_login(account, &login.encode()) {
        return fail(e.to_string());
    }

    let needs_register = if fresh {
        true
    } else {
        match coinos::wallet_exists(&login.username).await {
            Ok(exists) => !exists,
            Err(e) => return fail(e.to_string()),
        }
    };
    if needs_register {
        match coinos::create_wallet(&login.username, &login.password, &pubkey).await {
            Ok(_) => {}
            Err(e) => {
                if e.account_definitely_not_created() {
                    let _ = secrets::forget_coinos_login(account);
                }
                return fail(e.to_string());
            }
        }
    }

    let lightning_address = coinos::lightning_address(&login.username);
    let published = publish_lud16(&keys, &api, &lightning_address).await;
    // The wallet exists either way; a publish failure travels inside
    // `WalletCreated` so the panel can show it next to the retry button.
    let _ = updates.unbounded_send(Update::WalletCreated {
        account,
        created: WalletCreated {
            username: login.username,
            lightning_address,
            publish_error: published.as_ref().err().map(ToString::to_string),
        },
    });
    if let Ok(profile) = published {
        let profile = Profile::from_value(&serde_json::Value::Object(profile));
        let _ = updates.unbounded_send(profile_loaded(account, profile));
    }
}

/// Resolve one account to public material and announce it, then its kind-0.
/// Secrets never leave [`crate::secrets`].
async fn load_one_account(account: Account, updates: &mpsc::UnboundedSender<Update>) {
    let keys = match secrets::keys(account) {
        Ok(Some(keys)) => keys,
        Ok(None) => {
            let _ = updates.unbounded_send(Update::AccountMissing { account });
            return;
        }
        Err(e) => {
            let _ = updates.unbounded_send(Update::Error {
                source: ErrorSource::Accounts,
                message: e.to_string(),
            });
            let _ = updates.unbounded_send(Update::AccountMissing { account });
            return;
        }
    };
    let pubkey = keys.public_key().to_hex();
    let npub = keys
        .public_key()
        .to_bech32()
        .unwrap_or_else(|_| pubkey.clone());
    if updates
        .unbounded_send(Update::AccountLoaded {
            account,
            pubkey: pubkey.clone(),
            npub,
        })
        .is_err()
    {
        return;
    }

    // A scan failure is not an error state: the UI's fallback labels are
    // the defined behaviour when no profile is known.
    let Ok(api) = Api::new() else { return };
    if let Ok(Some(content)) = fetch_kind0(&api, &pubkey).await {
        let _ = updates.unbounded_send(profile_loaded(account, parse_profile(&content)));
    }
}

async fn load_accounts(updates: mpsc::UnboundedSender<Update>) {
    for account in Account::ALL {
        load_one_account(account, &updates).await;
    }
}

/// The pasted-key import, run entirely on the runtime thread so the UI never
/// touches the keychain: validate, store, drop any stale session, then
/// announce the account and probe its profile. Every message that leaves here
/// is public material or fixed text.
async fn import_key(
    sessions: SharedSessions,
    account: Account,
    secret: Secret,
    updates: mpsc::UnboundedSender<Update>,
) {
    let parsed = match secrets::parse_secret(&secret) {
        Ok(keys) => keys,
        Err(e) => {
            let _ = updates.unbounded_send(Update::ImportFailed {
                account,
                message: e.to_string(),
            });
            return;
        }
    };
    // Store the trimmed text, so the keychain never holds stray whitespace.
    let trimmed = Secret::new(secret.expose().trim());
    if let Err(e) = secrets::store_nsec(account, &trimmed) {
        // This message lands on the first-launch screen: a locked keychain
        // gets a recovery step, not the keyring crate's platform detail.
        let message = match e {
            SecretError::Keyring(_) => "This Mac's keychain is locked — unlock it \
                 (or log out and back in) and try again."
                .to_string(),
            other => other.to_string(),
        };
        let _ = updates.unbounded_send(Update::ImportFailed { account, message });
        return;
    }
    // A cached session signs with the old key; drop it.
    sessions.lock().await.remove(&account);

    // Env wins over the keychain, so announce the EFFECTIVE key — normally
    // the one just pasted, but an automated run's env override stays honest.
    let keys = match secrets::keys(account) {
        Ok(Some(keys)) => keys,
        _ => parsed,
    };
    let pubkey = keys.public_key().to_hex();
    let npub = keys
        .public_key()
        .to_bech32()
        .unwrap_or_else(|_| pubkey.clone());
    if updates
        .unbounded_send(Update::AccountLoaded {
            account,
            pubkey: pubkey.clone(),
            npub: npub.clone(),
        })
        .is_err()
    {
        return;
    }

    let probe = match Api::new() {
        Ok(api) => fetch_kind0(&api, &pubkey).await,
        Err(_) => Err(()),
    };
    let (profile, profile_checked) = match probe {
        Ok(profile) => (profile, true),
        Err(()) => (None, false),
    };
    let profile_found = profile.is_some();
    // The profile lands on the account view first, so the onboarding summary
    // that follows can read the name and lud16 from there.
    if let Some(content) = profile {
        let _ = updates.unbounded_send(profile_loaded(account, parse_profile(&content)));
    }
    let _ = updates.unbounded_send(Update::ImportReady {
        account,
        npub,
        profile_checked,
        profile_found,
    });
}

/// Delete the keychain entry, drop the cached session, then re-announce
/// whatever is still true — an env override survives and wins.
async fn forget_key(
    sessions: SharedSessions,
    account: Account,
    updates: mpsc::UnboundedSender<Update>,
) {
    if let Err(e) = secrets::forget_nsec(account) {
        let _ = updates.unbounded_send(Update::Error {
            source: ErrorSource::Accounts,
            message: e.to_string(),
        });
        return;
    }
    sessions.lock().await.remove(&account);
    load_one_account(account, &updates).await;
}

/// The pubkey whose bounties the read paths list: the imported issuer key if
/// any, else `MC_ISSUER_NPUB`, else the instance's house issuer. Public
/// material only — read-only browsing needs no secret.
fn issuer_pubkey_for_reads() -> String {
    if let Ok(Some(keys)) = secrets::keys(Account::Issuer) {
        return keys.public_key().to_hex();
    }
    if let Ok(value) = std::env::var("MC_ISSUER_NPUB")
        && let Ok(pubkey) = PublicKey::parse(value.trim())
    {
        return pubkey.to_hex();
    }
    HOUSE_ISSUER_HEX.to_string()
}

async fn fetch_bounties(updates: mpsc::UnboundedSender<Update>) {
    let result = async {
        let api = Api::new()?;
        Ok::<_, BridgeError>(api.list_bounties(&issuer_pubkey_for_reads()).await?)
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
        let merged = merge_lud16(Some(existing), "carpet1234@coinos.io").unwrap();
        assert_eq!(merged["name"], "Matthias");
        assert_eq!(merged["picture"], "https://x/p.png");
        assert_eq!(merged["about"], "hi");
        assert_eq!(merged["nip05"], "m@x.io");
        assert_eq!(merged["custom"]["deep"], 1);
        assert_eq!(merged["lud16"], "carpet1234@coinos.io");
        // What goes on the wire: every original key plus lud16, nothing else.
        assert_eq!(merged.len(), 6);

        // An existing lud16 is replaced, not duplicated.
        let merged = merge_lud16(Some(r#"{"lud16":"old@strike.me","name":"M"}"#), "new@coinos.io").unwrap();
        assert_eq!(merged["lud16"], "new@coinos.io");
        assert_eq!(merged["name"], "M");
        assert_eq!(merged.len(), 2);

        // A wrong-typed field elsewhere survives as it was.
        let merged = merge_lud16(Some(r#"{"name":"Alice","about":42}"#), "a@coinos.io").unwrap();
        assert_eq!(merged["name"], "Alice");
        assert_eq!(merged["about"], 42);

        // A wrong-typed lud16 is replaced and the rest kept.
        let merged = merge_lud16(Some(r#"{"name":"Alice","lud16":123}"#), "a@coinos.io").unwrap();
        assert_eq!(merged["lud16"], "a@coinos.io");
        assert_eq!(merged["name"], "Alice");

        // A null value keeps its key.
        let merged = merge_lud16(Some(r#"{"about":null}"#), "a@coinos.io").unwrap();
        assert!(merged.contains_key("about"));
        assert!(merged["about"].is_null());

        // Only a confirmed-absent profile may start from blank.
        let blank = serde_json::json!({ "lud16": "new@coinos.io" });
        assert_eq!(merge_lud16(None, "new@coinos.io").unwrap(), *blank.as_object().unwrap());
        assert_eq!(merge_lud16(Some(""), "new@coinos.io").unwrap(), *blank.as_object().unwrap());

        // Content that is not an object is left alone rather than clobbered.
        assert!(merge_lud16(Some("[1,2]"), "x@y.z").is_err());
        assert!(merge_lud16(Some("not json"), "x@y.z").is_err());
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
        // Neither side of the wallet channel may hold the password: the
        // command names an account, the update names public material only.
        let printed = format!(
            "{:?}",
            Command::CreateCoinosWallet {
                account: Account::Claimant
            }
        );
        assert_eq!(printed, "CreateCoinosWallet { account: Claimant }");
        let update = Update::WalletCreated {
            account: Account::Claimant,
            created: WalletCreated {
                username: "carpetab12cd34".into(),
                lightning_address: "carpetab12cd34@coinos.io".into(),
                publish_error: Some("The relays did not accept the profile update".into()),
            },
        };
        assert!(!format!("{update:?}").contains("password"));
    }

    #[test]
    fn an_import_command_debugs_without_its_secret() {
        // The command channel is the one place a pasted key travels outside
        // `secrets`; a `{:?}` on it must stay clean.
        let command = Command::ImportKey {
            account: Account::Claimant,
            secret: Secret::new("nsec1extremelysecretvalue"),
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
