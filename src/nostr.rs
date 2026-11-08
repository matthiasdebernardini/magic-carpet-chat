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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use nostr_sdk::prelude::*;
use tokio::sync::Mutex as TokioMutex;

use crate::api::{Api, ApiError, AutoPaymentRow, Bounty, BountyDetail, Claim, CreateBounty, RawEvent};
use crate::autopay::{self, AttemptState, ClaimFacts, Hold, Policy};
use crate::brainstorm::{self, RunState, SearchHit};
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
pub const TERMINAL_STATUSES: [&str; 3] = ["fulfilled", "expired", "cancelled"];

/// `MC_RELAY_URL` overrides the prod relay, for rehearsals.
pub fn relay_url() -> String {
    std::env::var("MC_RELAY_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| INSTANCE_RELAY.to_string())
}

/// How often a running recalculation is polled: 15 s, or
/// `MC_RECALC_POLL_SECS` (whole seconds, at least 1) so a scripted test need
/// not wait out the real interval.
fn recalc_poll() -> Duration {
    Duration::from_secs(poll_secs_from(
        std::env::var("MC_RECALC_POLL_SECS").ok().as_deref(),
    ))
}

/// How often the auto-pay loop looks for new claims: 30 s, or
/// `MC_AUTOPAY_POLL_SECS` (whole seconds, at least 1) for tests.
fn autopay_poll() -> Duration {
    Duration::from_secs(autopay_secs_from(
        std::env::var("MC_AUTOPAY_POLL_SECS").ok().as_deref(),
    ))
}

fn autopay_secs_from(env: Option<&str>) -> u64 {
    env.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs >= 1)
        .unwrap_or(30)
}

fn poll_secs_from(env: Option<&str>) -> u64 {
    env.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs >= 1)
        .unwrap_or(15)
}

/// `MC_TRUST_RELAYS`, comma-separated: trims each entry, skips empties,
/// normalizes the URL, and drops anything that is not `ws://`/`wss://`.
/// `None` for unset or when nothing valid remains — the defaults apply.
fn relays_from_env() -> Option<Vec<String>> {
    let urls = parse_relay_list(&std::env::var("MC_TRUST_RELAYS").ok()?);
    (!urls.is_empty()).then_some(urls)
}

fn parse_relay_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(trust::normalize_relay)
        .filter(|url| url.starts_with("wss://") || url.starts_with("ws://"))
        .collect()
}

/// The read set every trust read unions the account's outbox relays with —
/// the `MC_TRUST_RELAYS` set when the override is active, else
/// [`TRUST_FALLBACK_RELAYS`].
pub fn trust_relays() -> Vec<String> {
    relays_from_env().unwrap_or_else(|| {
        TRUST_FALLBACK_RELAYS
            .iter()
            .map(|url| trust::normalize_relay(url))
            .collect()
    })
}

/// The set public writes (a new lud16, a kind-3 follow, a kind-10040 map)
/// go to: the `MC_TRUST_RELAYS` set when the override is active, else
/// [`PUBLIC_RELAYS`].
pub fn public_relays() -> Vec<String> {
    relays_from_env().unwrap_or_else(|| {
        PUBLIC_RELAYS
            .iter()
            .map(|url| trust::normalize_relay(url))
            .collect()
    })
}

/// UI → runtime.
#[derive(Debug, Clone)]
pub enum Command {
    /// Open the relay connection. Idempotent.
    Connect,
    /// Read the store, then each account's kind-0 from the instance. Emits
    /// `AccountsLoaded`, then per account `ProfileLoaded` or, when the scan
    /// failed, `ProfileUnavailable`.
    LoadAccounts,
    /// GET every issuer's bounties (status=all), with slot counts from the
    /// relay's claims and receipts for the ones the issuer's app pays. Emits
    /// `Bounties` or `BountiesFailed` carrying the same `seq`, so the UI can
    /// tell a reply to this read from one to an earlier read.
    FetchBounties { seq: u64 },
    /// The open bounties `pubkey` can still claim, worked out here: each
    /// issuer's own scores (Treasure Map, then kind-30382 `d` = pubkey) set
    /// `issuerRank`; full bounties, ones `pubkey` has its rewards from, and
    /// its own are left out. The server's eligible list is not used: it
    /// ranks everyone by the house's scores. Emits `Eligible` or
    /// `EligibleFailed` carrying the same `pubkey` and `seq`, so the UI
    /// drops a reply for an account it already switched away from.
    FetchEligible {
        pubkey: String,
        seq: u64,
    },
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
    /// `Command` cannot leak it. `generated` marks a key the app itself just
    /// minted (`StoredAccount.created_in_app`): such a key having no kind-3
    /// on the relays means "fresh", not "lost", so the first follow write
    /// needs no confirmation.
    AddAccount {
        secret: Secret,
        name: Option<String>,
        open_wallet: bool,
        generated: bool,
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
    /// Read the wallet balance with the stored token. Emits `Balance` or
    /// `BalanceFailed` carrying the same `request`, plus one feed line per
    /// failure streak. `None` for polls; the armed remove sets its own id.
    FetchBalance {
        pubkey: String,
        request: Option<u64>,
    },
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
    /// The instance owner's identity for the community panel's recommended
    /// follow: the issuer's pubkey (the `MC_ISSUER_NPUB`-honouring helper),
    /// its npub, and the display name its kind-0 carries — read through the
    /// instance, then the trust relays. Emits `Update::Owner`. No pubkey or
    /// generation: there is one owner and the answer is never stale.
    FetchOwner,
    /// The follow step's profile search (`/search/pubkeys`; names and photos follow as `ProfileName`s). `seq` lets the
    /// UI drop the reply to a query it already replaced.
    SearchProfiles {
        pubkey: String,
        generation: u32,
        seq: u64,
        query: String,
    },
    /// The name in `target`'s (hex pubkey) kind-0, read from the trust
    /// relays, for a row built from a pasted key. Emits `Community` with
    /// `CommunityPart::ProfileName`; `seq` is the search seq it belongs to.
    FetchProfileName {
        pubkey: String,
        generation: u32,
        seq: u64,
        target: String,
    },
    /// Append `target` (hex pubkey) to the account's kind-3 under the
    /// majority rule: a found list is merged, `allow_first_list` is the
    /// confirmed consent to start one from scratch. Emits `Community` with
    /// `CommunityPart::Followed`.
    Follow {
        pubkey: String,
        generation: u32,
        target: String,
        allow_first_list: bool,
    },
    /// The "Calculate my trust scores" step: Brainstorm login, follow-list
    /// ingest, graperank, then one setup probe. Rate-limited — never re-sent
    /// for a reason. Emits `Community` with `CommunityPart::Calculated`.
    CalculateTrust { pubkey: String, generation: u32 },
    /// The Recalculate button: login, `followList` only when the list
    /// changed since Brainstorm last accepted one, `graperank` once (never
    /// retried), then a `graperankResult` poll until the run is done or
    /// failed, then `/user/history`. Emits `Community` with
    /// `CommunityPart::Recalc` per state and `LastFinished` at the end.
    Recalculate { pubkey: String, generation: u32 },
    /// When the account's last run finished (`/user/history`). Asked only
    /// for an account this app has already sent a follow list for, so
    /// opening a page never logs anyone in to Brainstorm unasked. Emits
    /// `Community` with `CommunityPart::LastFinished`.
    FetchRunHistory { pubkey: String, generation: u32 },
    /// A `/setup` read ONLY — no login, nothing rate-limited. The "Try
    /// again" control. Emits `Community` with `CommunityPart::SetupChecked`.
    CheckSetup { pubkey: String, generation: u32 },
    /// Build the kind-10040 from the current `/setup` rows, re-read the
    /// existing map under the majority rule (an existing one wins — the
    /// publish refuses), then sign locally and publish. Emits `Community`
    /// with `CommunityPart::MapPublished`.
    PublishMap { pubkey: String, generation: u32 },
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
    /// CreateBounty, refused: no bounty was made.
    Bounty,
    /// CreateBounty, unanswered (transport error, timeout, 5xx, unreadable
    /// reply): the server may have made the bounty anyway.
    BountyOutcomeUnknown,
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
    /// The account's kind-0 could not be read. Nothing is known about its
    /// lud16, so the UI must not offer to write one.
    ProfileUnavailable { pubkey: String },
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
    Bounties { seq: u64, list: Vec<Bounty> },
    BountiesFailed { seq: u64, message: String },
    /// The server's eligible list for `pubkey`, as sent: own bounties and the
    /// auto-pay minimum are filtered by the UI ([`crate::api::claimable_by`]).
    Eligible {
        pubkey: String,
        seq: u64,
        list: Vec<Bounty>,
    },
    EligibleFailed {
        pubkey: String,
        seq: u64,
        message: String,
    },
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
    Balance {
        pubkey: String,
        sats: u64,
        request: Option<u64>,
    },
    /// A balance read for this account failed or found no login. Sent on
    /// every failure (the feed's `Error` is sent once per streak), so a
    /// waiting remove confirm always gets an answer.
    BalanceFailed {
        pubkey: String,
        request: Option<u64>,
    },
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
    /// The instance owner's identity, once. No generation: stale is
    /// impossible for a value that never changes in-flight.
    Owner {
        pubkey_hex: String,
        npub: String,
        name: Option<String>,
        picture: Option<String>,
    },
    /// One finished piece of a community flow, dropped by the UI unless its
    /// generation matches the panel's.
    Community {
        pubkey: String,
        generation: u32,
        part: CommunityPart,
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

/// One piece of the trusted-community flow. `Ok` is a real answer;
/// `Err` is fixed text only — Brainstorm's bodies and every token stay on
/// the runtime thread.
#[derive(Debug, Clone, PartialEq)]
pub enum CommunityPart {
    /// A `SearchProfiles` reply; `seq` matches the command's.
    SearchResults {
        seq: u64,
        result: Result<Vec<SearchHit>, String>,
    },
    /// A `FetchProfileName` reply; all are `None` when no kind-0 was found.
    ProfileName {
        seq: u64,
        target: String,
        name: Option<String>,
        picture: Option<String>,
        nip05: Option<String>,
    },
    /// A `Follow` outcome.
    Followed(Result<FollowOk, FollowErr>),
    /// A `CalculateTrust` outcome: `Ok` means Brainstorm took the request;
    /// the `SetupState` is what the `/setup` probe then saw.
    Calculated(Result<SetupState, String>),
    /// A `CheckSetup` reply.
    SetupChecked(SetupState),
    /// A `Recalculate` step: `Ok` is the run's state as the poll last read
    /// it; `Err` is fixed wording for a run that never started (or a poll
    /// that gave up). Nothing is retried after an `Err`.
    Recalc(Result<RunState, String>),
    /// When the last run finished, unix seconds; `None` when none has.
    LastFinished(Option<u64>),
    /// A `PublishMap` outcome.
    MapPublished(Result<PublishOk, String>),
}

/// What a `/setup` read found — whether the map publish may proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    /// Rows ready for a map; the count is display material.
    Ready { rows: usize },
    /// A real 404: calculated-but-not-yet, or nothing was ever calculated.
    Missing,
    /// The read failed or the body was not setup rows.
    Unreadable,
}

/// A follow that landed.
#[derive(Debug, Clone, PartialEq)]
pub struct FollowOk {
    /// The hex pubkey now followed.
    pub target: String,
    /// Normalized URLs of the relays that accepted the kind-3.
    pub accepted: Vec<String>,
    /// Normalized URLs of the asked relays that did not accept it.
    pub rejected: Vec<String>,
    /// How many relays the publish went to.
    pub asked: usize,
    /// The list already had the key: nothing was published.
    pub already: bool,
}

/// A follow that did not land, with everything the retry UI needs.
#[derive(Debug, Clone, PartialEq)]
pub enum FollowErr {
    /// Too few relays answered to say whether a list exists. Counts only.
    Inconclusive { answered: usize, asked: usize },
    /// A strict majority confirmed no list exists AND the key was not
    /// generated here: writing one without consent would clobber a list the
    /// relays cannot see. The UI shows the warning and re-sends with
    /// `allow_first_list`.
    NeedsConfirmation { answered: usize, asked: usize },
    /// Every relay refused the signed kind-3.
    Rejected { asked: usize },
    /// Fixed wording (session, signing).
    Failed(&'static str),
}

/// A map that landed.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishOk {
    /// Normalized URLs of the relays that accepted the kind-10040.
    pub accepted: Vec<String>,
    /// How many relays the publish went to.
    pub asked: usize,
    /// How many `/setup` rows the map carries.
    pub rows: usize,
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
    #[error("This account has no secret key saved. Re-add the account.")]
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
    /// The create request went out and no clear answer came back.
    #[error(transparent)]
    OutcomeUnknown(ApiError),
}

/// A failed create, split by whether the server may still have made it.
fn create_outcome(e: ApiError) -> BridgeError {
    match e {
        ApiError::Http(_) | ApiError::Decode(_) => BridgeError::OutcomeUnknown(e),
        ApiError::Status { status, .. } if status >= 500 => BridgeError::OutcomeUnknown(e),
        e => BridgeError::Api(e),
    }
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
    /// The Brainstorm bearer token, minted by the challenge exchange. Kept
    /// inside [`Secret`] so a `{:?}` cannot leak it; in memory only, never
    /// stored, never in an Update.
    brainstorm: Option<Secret>,
    /// The newest kind-3 this session signed or read — the followList
    /// ingest input, so the calculate step does not re-read the relays.
    /// The id Brainstorm last accepted lives in the account store
    /// (`secrets::follow_list_sent`), so a restart never re-sends it.
    contact_list: Option<Event>,
    /// A graperank run was accepted for this account this session — the
    /// gate `PublishMap` checks, since a new process cannot know.
    graperank_ok: bool,
    /// A Calculate or Recalculate is in flight for this account; held by a
    /// [`RunGuard`] so a second one cannot spend the rate limit beside it.
    run_busy: Arc<AtomicBool>,
}

/// Owns an account's [`Session::run_busy`] flag for one run; clears it on
/// drop, so every exit path frees the account.
struct RunGuard(Arc<AtomicBool>);

impl RunGuard {
    /// `None` when a run is already in flight for the account.
    fn acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| RunGuard(flag.clone()))
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
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
    let ctx = AutoPayCtx::default();
    let autopay_task = tokio::spawn(autopay_loop(client.clone(), ctx.clone(), updates.clone()));
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
            Command::FetchBounties { seq } => {
                tokio::spawn(fetch_bounties(client.clone(), seq, updates.clone()));
            }
            Command::FetchEligible { pubkey, seq } => {
                tokio::spawn(fetch_eligible(
                    client.clone(),
                    ctx.clone(),
                    pubkey,
                    seq,
                    updates.clone(),
                ));
            }
            Command::AddAccount {
                secret,
                name,
                open_wallet,
                generated,
            } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(add_account(sessions, secret, name, open_wallet, generated, updates));
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
            Command::FetchBalance { pubkey, request } => {
                let failures = balance_failures.clone();
                let updates = updates.clone();
                tokio::spawn(fetch_balance(pubkey, request, failures, updates));
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
                    let sent_at = now_unix();
                    let update = match create_bounty(&sessions, &pubkey, &req).await {
                        Ok(id) => {
                            let min_rank = req.auto_pay_min_rank.unwrap_or(trust::INSTANCE_MIN_RANK);
                            if req.auto_pay
                                && let Err(e) = secrets::set_auto_pay(&pubkey, &id, min_rank)
                            {
                                let _ = updates.unbounded_send(Update::Error {
                                    source: ErrorSource::Watch,
                                    message: format!(
                                        "Bounty {id} won't pay automatically: {e} Pay its claims by hand."
                                    ),
                                });
                            }
                            Update::BountyCreated { id }
                        }
                        Err(e) => {
                            let unknown = matches!(e, BridgeError::OutcomeUnknown(_));
                            if unknown && req.auto_pay {
                                tokio::spawn(recover_auto_pay(
                                    pubkey.clone(),
                                    req.clone(),
                                    sent_at,
                                    updates.clone(),
                                ));
                            }
                            Update::Error {
                                source: if unknown {
                                    ErrorSource::BountyOutcomeUnknown
                                } else {
                                    ErrorSource::Bounty
                                },
                                message: e.to_string(),
                            }
                        }
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
                    ctx.clone(),
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
            Command::FetchOwner => {
                tokio::spawn(fetch_owner(updates.clone()));
            }
            Command::SearchProfiles {
                pubkey,
                generation,
                seq,
                query,
            } => {
                let updates = updates.clone();
                tokio::spawn(async move {
                    let result = brainstorm::search_pubkeys(&query)
                        .await
                        .map_err(str::to_string);
                    // The rows show as bare keys first; their names and
                    // photos follow once one relay query has read the lot.
                    let targets: Vec<String> = match &result {
                        Ok(hits) => hits.iter().map(|hit| hit.pubkey.clone()).collect(),
                        Err(_) => Vec::new(),
                    };
                    let _ = updates.unbounded_send(Update::Community {
                        pubkey: pubkey.clone(),
                        generation,
                        part: CommunityPart::SearchResults { seq, result },
                    });
                    let mut profiles = trust_relay_profiles(&targets).await;
                    for target in targets {
                        let Profile {
                            name,
                            picture,
                            nip05,
                            ..
                        } = profiles.remove(&target).unwrap_or_default();
                        let _ = updates.unbounded_send(Update::Community {
                            pubkey: pubkey.clone(),
                            generation,
                            part: CommunityPart::ProfileName {
                                seq,
                                target,
                                name,
                                picture,
                                nip05,
                            },
                        });
                    }
                });
            }
            Command::FetchProfileName {
                pubkey,
                generation,
                seq,
                target,
            } => {
                let updates = updates.clone();
                tokio::spawn(async move {
                    let Profile {
                        name,
                        picture,
                        nip05,
                        ..
                    } = trust_relay_profile(&target).await;
                    let _ = updates.unbounded_send(Update::Community {
                        pubkey,
                        generation,
                        part: CommunityPart::ProfileName {
                            seq,
                            target,
                            name,
                            picture,
                            nip05,
                        },
                    });
                });
            }
            Command::Follow {
                pubkey,
                generation,
                target,
                allow_first_list,
            } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(follow_account(
                    sessions,
                    pubkey,
                    generation,
                    target,
                    allow_first_list,
                    updates,
                ));
            }
            Command::CalculateTrust { pubkey, generation } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(calculate_trust(sessions, pubkey, generation, updates));
            }
            Command::Recalculate { pubkey, generation } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(recalculate(sessions, pubkey, generation, updates));
            }
            Command::FetchRunHistory { pubkey, generation } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(fetch_run_history(sessions, pubkey, generation, updates));
            }
            Command::CheckSetup { pubkey, generation } => {
                let updates = updates.clone();
                tokio::spawn(check_setup(pubkey, generation, updates));
            }
            Command::PublishMap { pubkey, generation } => {
                let sessions = sessions.clone();
                let updates = updates.clone();
                tokio::spawn(publish_map(sessions, pubkey, generation, updates));
            }
            Command::Shutdown => break,
        }
    }

    status_task.abort();
    autopay_task.abort();
    for (_, watcher) in watchers {
        watcher.task.abort();
    }
    client.disconnect().await;
}

/// The bounty a create whose outcome is unknown may have made: this
/// issuer's, on the same list, for the same reward and criteria, created no
/// earlier than a minute before the request went out. `Ok(None)` when there
/// is none yet; `Err(())` when more than one fits, so none can be picked.
fn created_bounty(
    list: &[Bounty],
    issuer: &str,
    req: &CreateBounty,
    sent_at: u64,
) -> Result<Option<String>, ()> {
    let fits: Vec<&Bounty> = list
        .iter()
        .filter(|b| {
            b.issuer_pubkey == issuer
                && b.list_coordinate == req.list_coordinate
                && b.amount_sats == req.amount_sats
                && b.criteria.as_deref().map(str::trim) == Some(req.criteria.trim())
                && b.created_at.is_some_and(|at| at + 60 >= sent_at)
        })
        .collect();
    match fits.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.id.clone())),
        _ => Err(()),
    }
}

/// A create with auto-pay on timed out: the server may have made the
/// bounty, and without its id the auto-pay setting has nothing to attach
/// to. Look for it a few times; save the setting when exactly one bounty
/// fits, else say plainly that auto-pay was not saved.
async fn recover_auto_pay(
    pubkey: String,
    req: CreateBounty,
    sent_at: u64,
    updates: mpsc::UnboundedSender<Update>,
) {
    let not_saved = |why: &str| {
        let _ = updates.unbounded_send(Update::Error {
            source: ErrorSource::Watch,
            message: format!(
                "Auto-pay wasn't saved for the bounty whose create timed out: {why} If it shows up in the list, pay its claims by hand."
            ),
        });
    };
    for wait in [5, 15, 45] {
        tokio::time::sleep(Duration::from_secs(wait)).await;
        let Ok(api) = Api::new() else { continue };
        let Ok(list) = api.list_all_bounties().await else {
            continue;
        };
        match created_bounty(&list, &pubkey, &req, sent_at) {
            Ok(Some(id)) => {
                let min_rank = req.auto_pay_min_rank.unwrap_or(trust::INSTANCE_MIN_RANK);
                if let Err(e) = secrets::set_auto_pay(&pubkey, &id, min_rank) {
                    not_saved(&e.to_string());
                }
                return;
            }
            Ok(None) => continue,
            Err(()) => return not_saved("more than one bounty on that list matches it."),
        }
    }
    not_saved("the server never listed it.");
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
        brainstorm: None,
        contact_list: None,
        graperank_ok: false,
        run_busy: Arc::new(AtomicBool::new(false)),
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

/// Create the bounty with the server's auto-pay OFF: the server pays only
/// for allowlisted issuers, and two payers on one bounty would pay twice.
/// `req.auto_pay` is the issuer's wish; the caller stores it as this app's
/// own auto-pay setting once the bounty exists. The minimum rank always goes
/// to the server (default 2), which keeps it with the bounty.
async fn create_bounty(
    sessions: &SharedSessions,
    pubkey: &str,
    req: &CreateBounty,
) -> Result<String, BridgeError> {
    let req = &CreateBounty {
        auto_pay: false,
        auto_pay_min_rank: Some(
            req.auto_pay_min_rank
                .unwrap_or(trust::INSTANCE_MIN_RANK)
                .max(trust::INSTANCE_MIN_RANK),
        ),
        ..req.clone()
    };
    let session = session(sessions, pubkey).await?;
    let mut session = session.lock().await;
    // The bounty issuer is always the session pubkey, so log in first.
    ensure_login(&mut session).await?;
    // The server's session cookie dies after 24 h with no renewal, while
    // `logged_in` lives as long as the app: a 401 means log in again, once.
    let bounty = match session.api.create_bounty(req).await {
        Err(ApiError::Status { status: 401, .. }) => {
            session.logged_in = false;
            ensure_login(&mut session).await?;
            session.api.create_bounty(req).await.map_err(create_outcome)?
        }
        result => result.map_err(create_outcome)?,
    };
    Ok(bounty.id)
}

/// The kind-22242 login, unless this session already holds one.
async fn ensure_login(session: &mut Session) -> Result<(), ApiError> {
    if !session.logged_in {
        session.api.login(&session.keys).await?;
        session.logged_in = true;
    }
    Ok(())
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
    nip05: Option<String>,
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
            nip05: field("nip05"),
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
                    "Your profile data is in an unexpected format, so this app left it alone.",
                ));
            }
        },
    };
    // A Lightning address set elsewhere is where this person already gets
    // paid; replacing it would reroute their payouts. The same address is a
    // retry and goes through.
    let current = Profile::from_value(&serde_json::Value::Object(profile.clone())).lud16;
    if current.is_some_and(|current| !current.eq_ignore_ascii_case(lud16.trim())) {
        return Err(BridgeError::Profile(
            "Your profile already has a Lightning address, so this app left it alone.",
        ));
    }
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
            BridgeError::Profile(
                "Couldn't read your profile from the Magic Carpet server. Try again.",
            )
        })?;
    let profile = merge_lud16(existing.as_deref(), lud16, name)?;
    let content = serde_json::Value::Object(profile.clone()).to_string();
    let event = EventBuilder::new(Kind::Metadata, content)
        .finalize(keys)
        .map_err(|_| {
            BridgeError::Profile("Couldn't sign the profile update. Restart the app and try again.")
        })?;

    // The instance first: it is what the prod payer reads.
    api.publish_event(&event).await.map_err(|_| {
        BridgeError::Profile("The Magic Carpet server didn't save the profile update.")
    })?;

    // Then the public relays, through a client that lives for this one send.
    // A relay that refuses is fine as long as one accepts; none accepting is
    // a publish failure, because Strike-side lookups would still miss it.
    let client = Client::default();
    for url in public_relays() {
        let _ = client.add_relay(url).await;
    }
    client.connect().and_wait(Duration::from_secs(10)).await;
    let sent = client.send_event(&event).await;
    client.disconnect().await;
    match sent {
        Ok(output) if !output.success.is_empty() => Ok(profile),
        _ => Err(BridgeError::Profile(
            "The Nostr servers didn't accept the profile update.",
        )),
    }
}

/// Fixed text for the one state a retry cannot fix: the login was saved
/// before a register that never answered, the wallet turned out to exist,
/// and the token it answered with is gone. `/api/login` has a captcha, so
/// there is no second way to get one.
const NO_TOKEN: &str = "This app lost access to the wallet. Remove the account and create a new wallet. Your Coinos login still works on coinos.io.";

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
const CANNOT_SEND: &str = "This app can't send from this wallet. It was opened elsewhere.";

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
        return fail("Enter an amount in sats.".into());
    }
    let to = to.trim().to_string();
    if coinos::split_address(&to).is_none() {
        return fail("Enter a Lightning address, like name@domain.com.".into());
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
    fetch_balance(pubkey, None, failures, updates).await;
}

async fn fetch_balance(
    pubkey: String,
    request: Option<u64>,
    failures: BalanceFailures,
    updates: mpsc::UnboundedSender<Update>,
) {
    let login = match secrets::load_coinos_login(&pubkey) {
        Ok(Some(login)) if login.has_token() => login,
        _ => {
            let _ = updates.unbounded_send(Update::BalanceFailed { pubkey, request });
            return;
        }
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
            let _ = updates.unbounded_send(Update::Balance {
                pubkey,
                sats,
                request,
            });
        }
        Err(e) => {
            if mark(true) {
                let _ = updates.unbounded_send(Update::Error {
                    source: ErrorSource::Wallet,
                    message: format!("Balance check failed: {e} Try again."),
                });
            }
            let _ = updates.unbounded_send(Update::BalanceFailed { pubkey, request });
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

    // A CONFIRMED-empty profile is reported: the UI pre-fills a stored
    // wallet's address from the store, and only a real profile read can say
    // whether the relays carry that lud16 yet. A failed scan is reported
    // too, so the UI never mistakes "unknown" for "no Lightning address".
    let api = Api::new();
    for account in &store.accounts {
        let read = match &api {
            Ok(api) => fetch_kind0(api, &account.pubkey).await,
            Err(_) => Err(()),
        };
        let _ = updates.unbounded_send(match read {
            Ok(content) => profile_loaded(
                account.pubkey.clone(),
                content.map(|c| parse_profile(&c)).unwrap_or_default(),
            ),
            Err(()) => Update::ProfileUnavailable {
                pubkey: account.pubkey.clone(),
            },
        });
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
    generated: bool,
    updates: mpsc::UnboundedSender<Update>,
) {
    let record = match secrets::add_account(&secret, generated) {
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

/// The instance's house issuer: `MC_ISSUER_NPUB` (npub or hex), else
/// [`HOUSE_ISSUER_HEX`]. Not who may issue (every account may); it is the
/// instance owner the community panel recommends following and whose map
/// names the instance's score provider. The wallet panel never opens or
/// re-points a wallet for it, but if its key and a Coinos login are stored
/// here and it issues a bounty with auto-pay on, this app pays that bounty's
/// claims from that wallet like any issuer's. Public material only.
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

/// An account IS the house issuer ([`issuer_pubkey`]).
pub fn is_issuer(pubkey: &str) -> bool {
    pubkey == issuer_pubkey()
}

async fn fetch_bounties(client: Client, seq: u64, updates: mpsc::UnboundedSender<Update>) {
    let result = async {
        let api = Api::new()?;
        let mut list = api.list_all_bounties().await?;
        let store = secrets::load().unwrap_or_default();
        fold_relay_usage(&client, &mut list, &store).await;
        Ok::<_, BridgeError>(list)
    }
    .await;
    let update = match result {
        Ok(list) => Update::Bounties { seq, list },
        Err(e) => Update::BountiesFailed {
            seq,
            message: e.to_string(),
        },
    };
    let _ = updates.unbounded_send(update);
}

async fn fetch_eligible(
    client: Client,
    ctx: AutoPayCtx,
    pubkey: String,
    seq: u64,
    updates: mpsc::UnboundedSender<Update>,
) {
    let update = match eligible(&client, &ctx, &pubkey).await {
        Ok(list) => Update::Eligible { pubkey, seq, list },
        Err(e) => Update::EligibleFailed {
            pubkey,
            seq,
            message: e.to_string(),
        },
    };
    let _ = updates.unbounded_send(update);
}

/// See [`Command::FetchEligible`]. Bounties the issuer's scores do not rank
/// `viewer` on come back with no `issuerRank`, and the UI's
/// [`crate::api::claimable_by`] drops them with the ones below the minimum.
async fn eligible(client: &Client, ctx: &AutoPayCtx, viewer: &str) -> Result<Vec<Bounty>, BridgeError> {
    let api = Api::new()?;
    let mut list: Vec<Bounty> = api
        .list_all_bounties()
        .await?
        .into_iter()
        .filter(|b| b.effective_status() == "open" && b.issuer_pubkey != viewer)
        .collect();
    let store = secrets::load().unwrap_or_default();
    let facts = fold_relay_usage(client, &mut list, &store).await;
    list.retain(|b| {
        if b.effective_status() != "open" {
            return false;
        }
        match facts.get(&b.id) {
            Some(facts) => {
                let (slots, per_npub) = autopay::slots_and_limit(b);
                autopay::open_for(slots, per_npub, facts, viewer)
            }
            // The server's own auto-pay bounties keep the server's count:
            // its delegate's receipts are not the issuer's to check here.
            None => b.payment_state.as_ref().is_none_or(|s| s.open_reward_slots > 0),
        }
    });
    let pairs: Vec<(String, String)> = list
        .iter()
        .map(|b| (b.issuer_pubkey.clone(), viewer.to_string()))
        .collect();
    let ranks = issuer_ranks(ctx, &pairs).await;
    for bounty in &mut list {
        bounty.issuer_rank = ranks
            .get(&(bounty.issuer_pubkey.clone(), viewer.to_string()))
            .copied()
            .flatten();
    }
    Ok(list)
}

// ─── Issuer-side auto-pay ──────────────────────────────────────────────────
// The server pays only for allowlisted issuers and ranks every claimant by
// the house's scores, so for everyone else this app does it: claims and
// receipts from the relay (`relay_url`, so a rehearsal relay works), ranks
// from the issuer's own Treasure Map (the trust relays), payments from the
// issuer's own Coinos wallet. The decisions are `crate::autopay`'s.

/// How long a read rank, a Lightning-address check or a Treasure Map stays
/// good. Short: a Recalculate can change a rank at any time.
const RANK_TTL_SECS: u64 = 60;
const MAP_TTL_SECS: u64 = 600;
/// How long a payment without a receipt (or an attempt still marked
/// sending) counts as in flight before the card calls it what it is.
const RECEIPT_GRACE_SECS: u64 = 120;

/// What the watchers, the eligible reads and the auto-pay loop share.
#[derive(Clone, Default)]
struct AutoPayCtx {
    cache: Arc<TokioMutex<TrustCache>>,
    /// Why the loop last held back a claim it would pay (no Lightning
    /// address, wallet empty, no zap support…), by claim address. In memory
    /// only: none of these moved sats, so the next tick tries again.
    notes: Arc<std::sync::Mutex<HashMap<String, String>>>,
}

#[derive(Default)]
struct TrustCache {
    /// Issuer → its map's `30382:rank` row `(key, relay)`, `None` for none.
    maps: HashMap<String, (u64, Option<(String, String)>)>,
    /// `(issuer, subject)` → the subject's rank in the issuer's scores.
    ranks: HashMap<(String, String), (u64, Option<f64>)>,
    /// Pubkey → whether its kind-0 names a Lightning address.
    lud16: HashMap<String, (u64, bool)>,
}

impl AutoPayCtx {
    fn note(&self, address: &str, text: impl Into<String>) {
        self.notes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(address.to_string(), text.into());
    }

    fn clear_note(&self, address: &str) {
        self.notes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(address);
    }

    fn note_for(&self, address: &str) -> Option<String> {
        self.notes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(address)
            .cloned()
    }
}

/// Each `(issuer, subject)`'s rank in the issuer's own scores: the issuer's
/// kind-10040 `30382:rank` row, then that key's kind-30382 with `d` =
/// subject on the row's relay. `None` when the issuer has no map, the key
/// has no assertion about the subject, or the read failed (not cached, so
/// the next call retries).
async fn issuer_ranks(
    ctx: &AutoPayCtx,
    pairs: &[(String, String)],
) -> HashMap<(String, String), Option<f64>> {
    let now = now_unix();
    let mut out = HashMap::new();
    let mut missing: Vec<(String, String)> = Vec::new();
    {
        let cache = ctx.cache.lock().await;
        for pair in pairs {
            if out.contains_key(pair) || missing.contains(pair) {
                continue;
            }
            match cache.ranks.get(pair) {
                Some((at, rank)) if now.saturating_sub(*at) < RANK_TTL_SECS => {
                    out.insert(pair.clone(), *rank);
                }
                _ => missing.push(pair.clone()),
            }
        }
    }
    if missing.is_empty() {
        return out;
    }
    let client = Client::default();
    for url in trust_relays() {
        let _ = client.add_relay(url).await;
    }
    client.connect().and_wait(Duration::from_secs(5)).await;
    for (issuer, subject) in missing {
        let rank = match rank_source(ctx, &client, &issuer, now).await {
            Some((key, relay)) => match read_assertion(&client, &key, &relay, &subject).await {
                Ok(found) => {
                    let rank = found.map(|a| f64::from(a.rank));
                    ctx.cache
                        .lock()
                        .await
                        .ranks
                        .insert((issuer.clone(), subject.clone()), (now, rank));
                    rank
                }
                Err(_) => None,
            },
            None => None,
        };
        out.insert((issuer, subject), rank);
    }
    client.disconnect().await;
    out
}

/// The issuer's `30382:rank` row, from its kind-10040 on its outbox and the
/// trust relays.
async fn rank_source(
    ctx: &AutoPayCtx,
    client: &Client,
    issuer: &str,
    now: u64,
) -> Option<(String, String)> {
    if let Some((at, map)) = ctx.cache.lock().await.maps.get(issuer)
        && now.saturating_sub(*at) < MAP_TTL_SECS
    {
        return map.clone();
    }
    let outbox = outbox_relays(client, issuer).await;
    for url in &outbox {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().await;
    let designation = read_designation(client, issuer, &outbox).await.ok()?;
    let map = designation
        .rank_row()
        .map(|row| (row.key.clone(), row.relay.clone()));
    ctx.cache
        .lock()
        .await
        .maps
        .insert(issuer.to_string(), (now, map.clone()));
    map
}

/// Whether `pubkey`'s kind-0 names a Lightning address. `None` when the
/// profile could not be read.
async fn has_lud16(ctx: &AutoPayCtx, pubkey: &str) -> Option<bool> {
    let now = now_unix();
    if let Some((at, has)) = ctx.cache.lock().await.lud16.get(pubkey)
        && now.saturating_sub(*at) < RANK_TTL_SECS
    {
        return Some(*has);
    }
    let api = Api::new().ok()?;
    let has = claimant_lud16(&api, pubkey).await.ok()?.is_some();
    ctx.cache
        .lock()
        .await
        .lud16
        .insert(pubkey.to_string(), (now, has));
    Some(has)
}

/// The claimant's Lightning address from its kind-0. `Err` when the
/// profile could not be read.
async fn claimant_lud16(api: &Api, pubkey: &str) -> Result<Option<String>, ()> {
    Ok(fetch_kind0(api, pubkey)
        .await?
        .and_then(|content| parse_profile(&content).lud16))
}

/// Kind-39999 claims on any of `coordinates` since `since`, and the
/// kind-9735 receipts that name them, from the instance relay. Signatures
/// are checked here: the relay can serve anything. `None` when a read
/// failed.
async fn relay_claims(
    client: &Client,
    coordinates: &[String],
    since: u64,
) -> Option<(Vec<Event>, Vec<Event>)> {
    if coordinates.is_empty() {
        return Some((Vec::new(), Vec::new()));
    }
    let filter = Filter::new()
        .kind(Kind::Custom(events::CLAIM_KIND))
        .custom_tags(SingleLetterTag::LOWERCASE_Z, coordinates.iter().cloned())
        .since(Timestamp::from(since));
    let claims: Vec<Event> = client
        .fetch_events(filter)
        .timeout(Duration::from_secs(10))
        .await
        .ok()?
        .into_iter()
        .filter(|event| event.verify().is_ok())
        .collect();
    if claims.is_empty() {
        return Some((claims, Vec::new()));
    }
    let filter = Filter::new()
        .kind(Kind::Custom(events::RECEIPT_KIND))
        .events(claims.iter().map(|event| event.id));
    let receipts = client
        .fetch_events(filter)
        .timeout(Duration::from_secs(10))
        .await
        .ok()?
        .into_iter()
        .filter(|event| event.verify().is_ok())
        .collect();
    Some((claims, receipts))
}

fn tag_values<'a>(event: &'a Event, name: &str) -> Vec<&'a str> {
    event
        .tags
        .iter()
        .filter_map(|t| {
            let t = t.as_slice();
            (t.first().map(String::as_str) == Some(name))
                .then(|| t.get(1).map(String::as_str))
                .flatten()
        })
        .collect()
}

/// A claim's stable address, `<pubkey>:<d tag>`, as the server's
/// `stableClaimAddress` keys it. `None` without a d tag.
fn claim_address(event: &Event) -> Option<String> {
    let d = tag_values(event, "d").into_iter().next()?;
    Some(format!("{}:{d}", event.pubkey.to_hex()))
}

/// The current claims on `coordinate`: exactly one `z` tag naming it, made
/// at or after `since`, and the newest event per stable address (a replaced
/// claim is one claim, not two).
fn current_claims<'a>(events: &'a [Event], coordinate: &str, since: u64) -> Vec<&'a Event> {
    let mut current: HashMap<String, &Event> = HashMap::new();
    for event in events {
        if event.kind.as_u16() != events::CLAIM_KIND
            || tag_values(event, "z") != [coordinate]
            || event.created_at.as_secs() < since
        {
            continue;
        }
        let Some(address) = claim_address(event) else {
            continue;
        };
        let newer = current.get(&address).is_none_or(|old| {
            (event.created_at, event.id.to_hex()) > (old.created_at, old.id.to_hex())
        });
        if newer {
            current.insert(address, event);
        }
    }
    let mut out: Vec<&Event> = current.into_values().collect();
    out.sort_by_key(|event| (event.created_at, event.id.to_hex()));
    out
}

/// Whether a relay receipt pays this bounty's claim `claim_event_id`: the
/// issuer signed the zap request, for the bounty's reward, on its list.
fn pays(receipt: &Event, bounty: &Bounty, claim_event_id: &str) -> bool {
    events::receipt_pays_bounty_claim(
        receipt,
        claim_event_id,
        &bounty.issuer_pubkey,
        &bounty.list_coordinate,
        &bounty.id,
        bounty.amount_sats,
    )
}

/// One relay claim joined with the server's copy of it, for a bounty the
/// server does not pay itself. The receipt is only one this app verified
/// ([`pays`]): the server attaches a receipt by the description's pubkey
/// alone, so a forged one could mark the claim paid and use up its slot. The
/// issuer's attempt on this bounty becomes the payment row.
fn merge_claim(
    server: Option<Claim>,
    fact: &ClaimFacts,
    event: &Event,
    receipts: &[Event],
    bounty: &Bounty,
    attempts: &[secrets::PayAttempt],
    now: u64,
) -> Option<Claim> {
    let mut claim = match server {
        Some(claim) => claim,
        None => Claim::from_event(RawEvent::from_event(event)?),
    };
    claim.zap_receipt = receipts
        .iter()
        .find(|r| pays(r, bounty, &fact.event_id))
        .and_then(RawEvent::from_event);
    if claim.auto_payment.is_none()
        && let Some(attempt) = attempts
            .iter()
            .find(|a| a.bounty_id == bounty.id && a.claim_address == fact.address)
    {
        claim.auto_payment = Some(attempt_row(
            attempt,
            &fact.event_id,
            claim.zap_receipt.is_some(),
            now,
        ));
    }
    // Without the issuer's attempt (the claimant's app, anyone else's), a
    // verified receipt is the whole story: the same "settled" row the
    // issuer's app shows. No start time, so no "attempted" beat is invented.
    if claim.auto_payment.is_none()
        && let Some(receipt) = claim.zap_receipt.as_ref()
    {
        claim.auto_payment = Some(AutoPaymentRow {
            id: fact.address.clone(),
            claim_event_id: fact.event_id.clone(),
            bounty_id: bounty.id.clone(),
            issuer_pubkey: Some(bounty.issuer_pubkey.clone()),
            claimant_pubkey: Some(fact.claimant.clone()),
            claim_address: Some(fact.address.clone()),
            amount_sats: Some(bounty.amount_sats),
            state: "settled".into(),
            reason: None,
            created_at: None,
            updated_at: receipt.created_at,
        });
    }
    Some(claim)
}

/// One bounty's claims as decision facts, each with its event: paid when a
/// relay receipt carries the issuer's signed zap request for it, attempted
/// when the issuer's store has an attempt at its address. Ranks are filled
/// in later.
fn facts_for<'a>(
    bounty: &Bounty,
    claims: &'a [Event],
    receipts: &[Event],
    attempts: &[secrets::PayAttempt],
) -> Vec<(ClaimFacts, &'a Event)> {
    let since = bounty.created_at.unwrap_or(0);
    current_claims(claims, &bounty.list_coordinate, since)
        .into_iter()
        .filter_map(|event| {
            let address = claim_address(event)?;
            let id = event.id.to_hex();
            let paid = receipts.iter().any(|r| pays(r, bounty, &id));
            let attempt = attempts
                .iter()
                .find(|a| a.bounty_id == bounty.id && a.claim_address == address)
                .map(|a| a.state);
            Some((
                ClaimFacts {
                    address,
                    event_id: id,
                    claimant: event.pubkey.to_hex(),
                    created_at: event.created_at.as_secs(),
                    paid,
                    attempt,
                    rank: None,
                },
                event,
            ))
        })
        .collect()
}

/// The issuer's attempts on this bounty that no current claim carries (the
/// claim was replaced away or deleted from the relay), as facts that hold
/// their slot: money that may have left must keep counting against the cap.
fn orphan_attempts(
    bounty: &Bounty,
    facts: &[ClaimFacts],
    attempts: &[secrets::PayAttempt],
) -> Vec<ClaimFacts> {
    attempts
        .iter()
        .filter(|a| a.bounty_id == bounty.id && !facts.iter().any(|f| f.address == a.claim_address))
        .map(|a| ClaimFacts {
            address: a.claim_address.clone(),
            event_id: a.claim_event_id.clone(),
            claimant: a.claimant.clone(),
            created_at: a.started_at,
            paid: false,
            attempt: Some(a.state),
            rank: None,
        })
        .collect()
}

/// The bounty's counters and status from the client's own count: paid
/// claims, slots held by attempts, slots left. A bounty every slot of which
/// is paid reads fulfilled even while the server still calls it open.
fn apply_usage(bounty: &mut Bounty, facts: &[ClaimFacts]) {
    let (slots, per_npub) = autopay::slots_and_limit(bounty);
    let (held, _) = autopay::usage(facts);
    let paid = facts.iter().filter(|f| f.paid).count() as u64;
    let mut state = bounty.payment_state.clone().unwrap_or_default();
    state.reward_amount_sats = bounty.amount_sats;
    state.total_reward_slots = slots;
    state.max_rewards_per_npub = per_npub;
    state.paid_reward_count = paid;
    state.held_reward_count = held.saturating_sub(paid);
    state.remaining_reward_slots = slots.saturating_sub(held);
    state.open_reward_slots = slots.saturating_sub(held);
    state.fulfilled = slots > 0 && paid >= slots;
    if state.fulfilled && bounty.effective_status() == "open" {
        bounty.derived_status = Some("fulfilled".into());
    }
    bounty.payment_state = Some(state);
}

/// The store's attempts for `issuer`; empty when it is not stored here.
fn attempts_of<'a>(store: &'a secrets::Store, issuer: &str) -> &'a [secrets::PayAttempt] {
    store
        .accounts
        .iter()
        .find(|a| a.pubkey == issuer)
        .map(|a| a.pay_attempts.as_slice())
        .unwrap_or(&[])
}

/// This app's auto-pay setting for a bounty, if any stored account has one.
fn setting_of<'a>(store: &'a secrets::Store, bounty: &Bounty) -> Option<&'a secrets::AutoPaySetting> {
    store
        .accounts
        .iter()
        .find(|a| a.pubkey == bounty.issuer_pubkey)?
        .auto_pay
        .iter()
        .find(|s| s.bounty_id == bounty.id)
}

/// Fold the store and the relay into a list of bounties: `app_auto_pay` on
/// every bounty with a setting here, and for each open bounty the server
/// does not pay itself, the client's slot count ([`apply_usage`]). Returns
/// those bounties' claim facts by bounty id; a failed relay read leaves the
/// server's numbers and returns nothing.
async fn fold_relay_usage(
    client: &Client,
    bounties: &mut [Bounty],
    store: &secrets::Store,
) -> HashMap<String, Vec<ClaimFacts>> {
    for bounty in bounties.iter_mut() {
        bounty.app_auto_pay = setting_of(store, bounty).is_some();
    }
    let app_paid = |b: &Bounty| !b.server_auto_pay_on() && b.effective_status() == "open";
    let mut coordinates: Vec<String> = bounties
        .iter()
        .filter(|b| app_paid(b))
        .map(|b| b.list_coordinate.clone())
        .collect();
    coordinates.sort();
    coordinates.dedup();
    let since = bounties
        .iter()
        .filter(|b| app_paid(b))
        .filter_map(|b| b.created_at)
        .min()
        .unwrap_or(0);
    let Some((claims, receipts)) = relay_claims(client, &coordinates, since).await else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for bounty in bounties.iter_mut().filter(|b| app_paid(b)) {
        let attempts = attempts_of(store, &bounty.issuer_pubkey);
        let mut facts: Vec<ClaimFacts> = facts_for(bounty, &claims, &receipts, attempts)
            .into_iter()
            .map(|(f, _)| f)
            .collect();
        facts.extend(orphan_attempts(bounty, &facts, attempts));
        apply_usage(bounty, &facts);
        out.insert(bounty.id.clone(), facts);
    }
    out
}

/// A stored attempt as the server's `auto_payments` row shape, so the claim
/// card's timeline and the watch ledger's feed lines read it like the
/// server's own. A `sending` attempt or a payment with no receipt is in
/// flight for [`RECEIPT_GRACE_SECS`]; after that it is `unknown` or
/// `paid_unreceipted`, so the watcher stops its fast poll.
fn attempt_row(
    attempt: &secrets::PayAttempt,
    claim_event_id: &str,
    receipt: bool,
    now: u64,
) -> AutoPaymentRow {
    let last = attempt.updated_at.max(attempt.started_at);
    let fresh = now.saturating_sub(last) < RECEIPT_GRACE_SECS;
    let state = match attempt.state {
        AttemptState::Sending if fresh => "attempting",
        AttemptState::Sending | AttemptState::Unknown => "unknown",
        AttemptState::Paid if receipt => "settled",
        AttemptState::Paid if fresh => "paid",
        AttemptState::Paid => "paid_unreceipted",
        AttemptState::Failed => "failed",
    };
    AutoPaymentRow {
        id: attempt.claim_address.clone(),
        claim_event_id: claim_event_id.to_string(),
        bounty_id: attempt.bounty_id.clone(),
        issuer_pubkey: None,
        claimant_pubkey: Some(attempt.claimant.clone()),
        claim_address: Some(attempt.claim_address.clone()),
        amount_sats: Some(attempt.sats),
        state: state.to_string(),
        reason: attempt.reason.clone(),
        created_at: Some(attempt.started_at),
        updated_at: Some(last),
    }
}

/// A bounty snapshot with this app's view folded in, and the decision for
/// every relay claim.
struct Annotated {
    detail: BountyDetail,
    policy: Option<Policy>,
    facts: Vec<ClaimFacts>,
}

/// The server's snapshot of a bounty the server does not pay itself, made
/// whole from the relay: every current claim (the server hides claimants the
/// house's scores rank below 2), "paid" from the issuer's signed receipts,
/// the issuer's attempts as payment rows, the client's slot count, and each
/// unpaid claim's reason in plain words. The server's own auto-pay bounties
/// come back as the server sent them.
async fn annotate(ctx: &AutoPayCtx, client: &Client, mut detail: BountyDetail) -> Annotated {
    // Even when nothing can be read, a receipt the server attached to a
    // bounty it does not pay is unverified: it never shows as paid.
    let unchanged = |mut detail: BountyDetail| {
        if !detail.bounty.server_auto_pay_on() {
            for claim in &mut detail.claims {
                claim.zap_receipt = None;
            }
        }
        Annotated {
            detail,
            policy: None,
            facts: Vec::new(),
        }
    };
    // An unreadable store is not "no attempts": without the attempts there
    // is no knowing what was paid, so nothing is decided (and nothing paid).
    let Ok(store) = secrets::load() else {
        return unchanged(detail);
    };
    let setting = setting_of(&store, &detail.bounty).cloned();
    detail.bounty.app_auto_pay = setting.is_some();
    if detail.bounty.server_auto_pay_on() {
        return unchanged(detail);
    }
    let coordinate = detail.bounty.list_coordinate.clone();
    let since = detail.bounty.created_at.unwrap_or(0);
    let Some((events, receipts)) =
        relay_claims(client, std::slice::from_ref(&coordinate), since).await
    else {
        return unchanged(detail);
    };
    let issuer = detail.bounty.issuer_pubkey.clone();
    let issuer_here = store.accounts.iter().any(|a| a.pubkey == issuer);
    let attempts = attempts_of(&store, &issuer);
    let mut facts: Vec<ClaimFacts> = Vec::new();
    let mut by_id: HashMap<String, Claim> = detail
        .claims
        .drain(..)
        .map(|claim| (claim.event.id.clone(), claim))
        .collect();
    let now = now_unix();
    let mut claims: Vec<Claim> = Vec::new();
    for (fact, event) in facts_for(&detail.bounty, &events, &receipts, attempts) {
        let server = by_id.remove(&fact.event_id);
        let Some(claim) =
            merge_claim(server, &fact, event, &receipts, &detail.bounty, attempts, now)
        else {
            continue;
        };
        facts.push(fact);
        claims.push(claim);
    }
    // Claims the server lists that the relay no longer serves keep their
    // place, without the server's unverified receipt; attempts whose claim
    // is gone keep their slot.
    claims.extend(by_id.into_values().map(|claim| Claim {
        zap_receipt: None,
        ..claim
    }));
    let orphans = orphan_attempts(&detail.bounty, &facts, attempts);
    facts.extend(orphans);
    claims.sort_by_key(|c| (c.event.created_at.unwrap_or(0), c.event.id.clone()));

    let pairs: Vec<(String, String)> = facts
        .iter()
        .filter(|f| f.claimant != issuer && !f.paid && f.attempt.is_none())
        .map(|f| (issuer.clone(), f.claimant.clone()))
        .collect();
    let ranks = issuer_ranks(ctx, &pairs).await;
    for fact in &mut facts {
        fact.rank = ranks
            .get(&(issuer.clone(), fact.claimant.clone()))
            .copied()
            .flatten();
    }
    // An issuer stored here has its switch here; anyone else's app may be
    // paying, so a claim on its bounty waits on that app.
    let min_rank = setting
        .as_ref()
        .map(|s| s.min_rank.max(trust::INSTANCE_MIN_RANK))
        .unwrap_or_else(|| detail.bounty.claim_min_rank());
    let policy = Policy::of(&detail.bounty, setting.is_some() || !issuer_here, min_rank);
    let decisions: HashMap<String, Result<(), Hold>> =
        autopay::plan(&policy, &facts).into_iter().collect();
    for claim in &mut claims {
        let Some(fact) = facts.iter().find(|f| f.event_id == claim.event.id) else {
            continue;
        };
        claim.unpaid_reason = match decisions.get(&fact.address) {
            Some(Err(Hold::Attempted(AttemptState::Sending)))
                if claim.auto_payment.as_ref().is_some_and(|r| r.state == "attempting") =>
            {
                Some("Paying now…".to_string())
            }
            Some(Err(hold)) => hold.describe(),
            Some(Ok(())) => Some(match ctx.note_for(&fact.address) {
                Some(note) => note,
                None if has_lud16(ctx, &fact.claimant).await == Some(false) => {
                    "No Lightning address in the claimant's profile, so it can't be paid.".to_string()
                }
                None if issuer_here => "Waiting for the next auto-pay check.".to_string(),
                None => "Waiting for the issuer's app to pay it.".to_string(),
            }),
            None => None,
        };
    }
    detail.claims = claims;
    apply_usage(&mut detail.bounty, &facts);
    Annotated {
        detail,
        policy: Some(policy),
        facts,
    }
}

/// Every [`autopay_poll`], each stored account's auto-pay bounties are
/// checked and their qualifying claims paid, while the app runs.
async fn autopay_loop(client: Client, ctx: AutoPayCtx, updates: mpsc::UnboundedSender<Update>) {
    loop {
        autopay_tick(&client, &ctx, &updates).await;
        tokio::time::sleep(autopay_poll()).await;
        if updates.is_closed() {
            return;
        }
    }
}

async fn autopay_tick(client: &Client, ctx: &AutoPayCtx, updates: &mpsc::UnboundedSender<Update>) {
    let Ok(settings) = secrets::auto_pay_settings() else {
        return;
    };
    if settings.is_empty() {
        return;
    }
    let Ok(api) = Api::new() else {
        return;
    };
    for (issuer, setting) in settings {
        let Ok(detail) = api.get_bounty(&setting.bounty_id).await else {
            continue;
        };
        if detail.bounty.issuer_pubkey != issuer
            || detail.bounty.server_auto_pay_on()
            || TERMINAL_STATUSES.contains(&detail.bounty.effective_status())
        {
            continue;
        }
        let annotated = annotate(ctx, client, detail).await;
        let Some(policy) = annotated.policy else {
            continue;
        };
        let bounty = annotated.detail.bounty;
        let mut facts = annotated.facts;
        // Pay one claim at a time and re-plan: a claim that could not be
        // paid this tick (no address, empty wallet) must not hold the slot
        // a later claim could take.
        loop {
            let next = autopay::plan(&policy, &facts)
                .into_iter()
                .find_map(|(address, decision)| decision.is_ok().then_some(address));
            let Some(address) = next else { break };
            let Some(ix) = facts.iter().position(|f| f.address == address) else {
                break;
            };
            let paid: Vec<(String, String)> = facts
                .iter()
                .filter(|f| f.paid)
                .map(|f| (f.address.clone(), f.claimant.clone()))
                .collect();
            match pay_claim(ctx, &api, &bounty, &facts[ix], paid, updates).await {
                PayOutcome::Decided(state) => facts[ix].attempt = Some(state),
                PayOutcome::Retry => {
                    facts.remove(ix);
                }
                // Another instance filled the bounty: nothing else on it
                // can be paid this tick.
                PayOutcome::Full => break,
            }
        }
    }
}

/// What one [`pay_claim`] came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayOutcome {
    /// Nothing recorded, nothing sent (a note says why): try next tick.
    Retry,
    /// The claim is decided for this tick: an attempt exists, by this pass
    /// or another instance, or the per-npub limit refused it.
    Decided(AttemptState),
    /// The cap refused it: the bounty is full.
    Full,
}

/// Record the attempt, then ask for the invoice: the limit recheck runs
/// before any zap request is signed or leaves this app, so a refused one
/// leaks nothing. `invoice` signs the request and fetches the bolt11; when
/// it fails, no payment request was made, so the attempt is taken back and
/// the claim tried again next tick. `Ok` carries the invoice to pay.
async fn record_then_invoice<F, Fut>(
    ctx: &AutoPayCtx,
    issuer: &str,
    attempt: secrets::PayAttempt,
    limits: &secrets::SlotLimits,
    invoice: F,
) -> Result<String, PayOutcome>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let (bounty_id, address) = (attempt.bounty_id.clone(), attempt.claim_address.clone());
    match secrets::begin_attempt_within(issuer, attempt, Some(limits)) {
        Ok(secrets::Begin::Recorded) => {}
        Ok(secrets::Begin::Exists) => return Err(PayOutcome::Decided(AttemptState::Sending)),
        Ok(secrets::Begin::NoSlots) => {
            ctx.note(&address, Hold::NoSlots.describe().unwrap_or_default());
            return Err(PayOutcome::Full);
        }
        Ok(secrets::Begin::OverLimit) => {
            ctx.note(&address, Hold::OverLimit.describe().unwrap_or_default());
            return Err(PayOutcome::Decided(AttemptState::Failed));
        }
        Err(e) => {
            ctx.note(&address, e.to_string());
            return Err(PayOutcome::Retry);
        }
    }
    match invoice().await {
        Ok(bolt11) => Ok(bolt11),
        Err(why) => {
            // If taking it back fails the attempt stays `sending`: shown as
            // unknown, never retried. The safe side.
            let _ = secrets::abandon_attempt_for(issuer, &bounty_id, &address);
            ctx.note(&address, why);
            Err(PayOutcome::Retry)
        }
    }
}

/// Pay one claim from the issuer's Coinos wallet as a zap. The checks that
/// need no signature run first and, on a "no", leave a note ([`PayOutcome::Retry`]).
/// Then [`record_then_invoice`]: the attempt is recorded under the store's
/// limit recheck, and only then is the zap request signed and the invoice
/// fetched. Then the payment goes out, and its end state is recorded.
async fn pay_claim(
    ctx: &AutoPayCtx,
    api: &Api,
    bounty: &Bounty,
    claim: &ClaimFacts,
    paid: Vec<(String, String)>,
    updates: &mpsc::UnboundedSender<Update>,
) -> PayOutcome {
    let issuer = &bounty.issuer_pubkey;
    let sats = bounty.amount_sats;
    let lud16 = match claimant_lud16(api, &claim.claimant).await {
        Ok(Some(lud16)) => lud16,
        Ok(None) => {
            ctx.note(
                &claim.address,
                "No Lightning address in the claimant's profile, so it can't be paid.",
            );
            return PayOutcome::Retry;
        }
        Err(()) => {
            ctx.note(&claim.address, "Couldn't read the claimant's profile. Trying again shortly.");
            return PayOutcome::Retry;
        }
    };
    let login = match secrets::load_coinos_login(issuer) {
        Ok(Some(login)) if login.has_token() => login,
        _ => {
            ctx.note(&claim.address, CANNOT_SEND);
            return PayOutcome::Retry;
        }
    };
    match coinos::balance(&login.token).await {
        Ok(balance) if balance < sats => {
            ctx.note(
                &claim.address,
                format!(
                    "Wallet empty: the issuer's Coinos wallet holds {balance} sats, and the reward is {sats}."
                ),
            );
            return PayOutcome::Retry;
        }
        Ok(_) => {}
        Err(e) => {
            ctx.note(&claim.address, format!("Couldn't read the issuer's wallet: {e}"));
            return PayOutcome::Retry;
        }
    }
    let Ok(Some(keys)) = secrets::keys(issuer) else {
        ctx.note(&claim.address, "The issuer's key isn't stored here.");
        return PayOutcome::Retry;
    };
    let now = now_unix();
    let attempt = secrets::PayAttempt {
        bounty_id: bounty.id.clone(),
        claim_address: claim.address.clone(),
        claim_event_id: claim.event_id.clone(),
        claimant: claim.claimant.clone(),
        sats,
        state: AttemptState::Sending,
        reason: None,
        started_at: now,
        updated_at: now,
    };
    // The plan came from a snapshot; another instance may have paid since.
    // The store re-counts the cap and the per-npub limit under its lock.
    let (slots, per_npub) = autopay::slots_and_limit(bounty);
    let limits = secrets::SlotLimits { slots, per_npub, paid };
    let invoice = || async {
        let mut relays = vec![relay_url()];
        relays.extend(public_relays());
        let request = events::zap_request(
            &keys,
            &claim.claimant,
            &claim.event_id,
            &bounty.list_coordinate,
            &bounty.id,
            sats,
            &relays,
        )
        .map_err(|e| e.to_string())?;
        coinos::lnurl_zap_invoice(&lud16, sats, &request.as_json())
            .await
            .map_err(|e| e.to_string())
    };
    let bolt11 = match record_then_invoice(ctx, issuer, attempt, &limits, invoice).await {
        Ok(bolt11) => bolt11,
        Err(outcome) => return outcome,
    };
    ctx.clear_note(&claim.address);
    let report = |state: &str, reason: Option<String>| {
        let _ = updates.unbounded_send(Update::PaymentState {
            bounty_id: bounty.id.clone(),
            claim_event_id: claim.event_id.clone(),
            state: state.to_string(),
            reason,
            ts: now_unix(),
        });
    };
    report("attempting", None);

    let (state, reason) = match coinos::pay_invoice(&login.token, &bolt11, sats).await {
        Ok(()) => (AttemptState::Paid, None),
        Err(e @ coinos::CoinosError::OutcomeUnknown) => (AttemptState::Unknown, Some(e.to_string())),
        Err(e) => (AttemptState::Failed, Some(e.to_string())),
    };
    // A failed write leaves the attempt `sending`: shown as unknown, never
    // retried. That is the safe side.
    let _ = secrets::finish_attempt_for(
        issuer,
        &bounty.id,
        &claim.address,
        state,
        reason.clone(),
        now_unix(),
    );
    report(
        match state {
            AttemptState::Paid => "paid",
            AttemptState::Failed => "failed",
            AttemptState::Sending | AttemptState::Unknown => "unknown",
        },
        reason,
    );
    PayOutcome::Decided(state)
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
        let bounty_can_still_pay = detail.bounty.server_auto_pay_on()
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
    ctx: AutoPayCtx,
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
                let detail = annotate(&ctx, &client, detail).await.detail;
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
                let message = format!("Bounty {bounty_id} didn't refresh: {e} Retrying.");
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
                    let message = format!("Payment receipt watch failed: {e}. Retrying.");
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
    for url in trust_relays() {
        let _ = client.add_relay(url).await;
    }
    // Reads on a relay still connecting fail; give them a moment first.
    client.connect().and_wait(Duration::from_secs(5)).await;

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
            result: Err("Couldn't find this server's score provider. Try again later.".into()),
        }),
    }

    client.disconnect().await;
}

/// The relays the account claims to write to: `r` tags of its newest
/// kind-10002 that carry no marker or marker `write`, unioned with
/// [`trust_relays`]. A failed or absent 10002 yields the fallback
/// set on its own — never an error.
async fn outbox_relays(client: &Client, pubkey: &str) -> Vec<String> {
    let mut urls: BTreeSet<String> = trust_relays().into_iter().collect();
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
/// `Ok` means that relay answered, `Err` means it failed — the difference
/// between "no event exists" and "nobody answered". See [`relay_answer`].
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
            // No SDK `.timeout()`: it ends the read with `Ok(empty)`, which
            // looks exactly like an answer. Without it the fetch ends at
            // EOSE, CLOSED or disconnect, and this timeout bounds the rest.
            let fetched = tokio::time::timeout(
                Duration::from_secs(10),
                relay.fetch_events(filter).policy(ReqExitPolicy::ExitOnEOSE),
            )
            .await
            .ok()
            .map(|read| {
                read.map(|events| events.into_iter().collect())
                    .map_err(|_| ())
            });
            let answer = relay_answer(fetched, relay.status().is_connected());
            (url, answer)
        }
    }))
    .await
}

/// One relay's read, classified. `fetched` is `None` on the outer timeout.
/// The SDK also returns `Ok(empty)` when the relay dropped (or never came
/// up) mid-read, so an empty answer only counts from a relay still
/// connected — otherwise a dead relay would read as "no event exists".
fn relay_answer(fetched: Option<Result<Vec<Event>, ()>>, connected: bool) -> Result<Vec<Event>, ()> {
    match fetched {
        None => Err(()),
        Some(Ok(events)) if events.is_empty() && !connected => Err(()),
        Some(read) => read,
    }
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
        return Err("The account's public key is invalid, so no Nostr server was asked.".into());
    };
    let filter = Filter::new().kind(Kind::Custom(10040)).author(public);
    let results = read_per_relay(client, outbox, filter).await;
    let total = results.len();
    let (answered, events) = answered(results);
    if answered == 0 {
        return Err(format!(
            "No Nostr server answered (asked {total}). Try again."
        ));
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
        return Err("The account's public key is invalid, so no Nostr server was asked.".into());
    };
    let filter = Filter::new()
        .kind(Kind::ContactList)
        .author(public)
        .limit(1);
    let results = read_per_relay(client, outbox, filter).await;
    let total = results.len();
    let (answered, events) = answered(results);
    if answered == 0 {
        return Err(format!(
            "Couldn't read your follow list: no Nostr server answered (asked {total}). Try again."
        ));
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
        return Err("The score provider's key is invalid, so its scores can't be read.".into());
    };
    let relay = trust::normalize_relay(relay);
    let _ = client.add_relay(relay.as_str()).await;
    client.connect().await;
    let filter = Filter::new()
        .kind(Kind::Custom(30382))
        .author(provider_key)
        .identifier(subject);
    let results = read_per_relay(client, std::slice::from_ref(&relay), filter).await;
    match results.into_iter().next() {
        Some((_, Ok(events))) => {
            let mut assertion = trust::parse_assertion(&events, provider, subject);
            if let Some(assertion) = &mut assertion {
                assertion.relay = relay.clone();
            }
            Ok(assertion)
        }
        _ => Err(format!("Couldn't read from {relay}. Try again.")),
    }
}

/// `GET {brainstorm::base()}/setup/{hex}`: 404 means the npub has no
/// Brainstorm account (`Ok(NoAccount)`); any other failure is `Err` with
/// fixed wording — the endpoint's own error text is never shown.
async fn fetch_brainstorm_key(hex: &str) -> Result<BrainstormKey, String> {
    let http = reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| {
            "Couldn't reach Brainstorm. Check your connection and try again.".to_string()
        })?;
    let response = http
        .get(format!("{}{}/{hex}", brainstorm::base(), trust::BRAINSTORM_SETUP_PATH))
        .send()
        .await
        .map_err(|_| {
            "Couldn't reach Brainstorm. Check your connection and try again.".to_string()
        })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(BrainstormKey::NoAccount);
    }
    if !response.status().is_success() {
        return Err("Brainstorm returned an error. Try again in a few minutes.".into());
    }
    let body = response.text().await.map_err(|_| {
        "Brainstorm sent an answer the app can't read. Try again later.".to_string()
    })?;
    trust::parse_setup(&body)
        .ok_or_else(|| "Brainstorm sent an answer the app can't read. Try again later.".to_string())
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

// ─── The trusted-community flow ────────────────────────────────────────────
// Follow (kind-3 merge), calculate (Brainstorm login → followList →
// graperank → setup probe), publish (kind-10040 from the setup rows). Every
// failure string here is fixed wording — relay replies, Brainstorm bodies
// and the token itself never cross into an Update.

/// The instance owner, for the panel's recommended follow: the issuer's
/// pubkey and npub always, its kind-0 display name when a read finds one —
/// through the instance first, then the trust relays.
async fn fetch_owner(updates: mpsc::UnboundedSender<Update>) {
    let pubkey_hex = issuer_pubkey();
    let npub = PublicKey::from_hex(&pubkey_hex)
        .ok()
        .and_then(|pk| pk.to_bech32().ok())
        .unwrap_or_else(|| pubkey_hex.clone());
    let mut profile = Profile::default();
    if let Ok(api) = Api::new()
        && let Ok(Some(content)) = fetch_kind0(&api, &pubkey_hex).await
    {
        profile = parse_profile(&content);
    }
    if profile.name.is_none() || profile.picture.is_none() {
        let relay = trust_relay_profile(&pubkey_hex).await;
        profile.name = profile.name.or(relay.name);
        profile.picture = profile.picture.or(relay.picture);
    }
    let _ = updates.unbounded_send(Update::Owner {
        pubkey_hex,
        npub,
        name: profile.name,
        picture: profile.picture,
    });
}

/// The newest kind-0 the trust relays hold for `pubkey_hex`, as far as it
/// goes; empty when none answers.
async fn trust_relay_profile(pubkey_hex: &str) -> Profile {
    trust_relay_profiles(&[pubkey_hex.to_string()])
        .await
        .remove(pubkey_hex)
        .unwrap_or_default()
}

/// The newest verified kind-0 per pubkey, from one query to the trust
/// relays. A pubkey no relay answered for is absent from the map.
async fn trust_relay_profiles(pubkeys: &[String]) -> HashMap<String, Profile> {
    let publics: Vec<PublicKey> = pubkeys
        .iter()
        .filter_map(|hex| PublicKey::parse(hex).ok())
        .collect();
    if publics.is_empty() {
        return HashMap::new();
    }
    let urls = trust_relays();
    let client = Client::default();
    for url in &urls {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().and_wait(Duration::from_secs(5)).await;
    let filter = Filter::new()
        .kind(Kind::Metadata)
        .authors(publics.iter().copied())
        .limit(publics.len() * 5);
    let results = read_per_relay(&client, &urls, filter).await;
    client.disconnect().await;
    let (_, events) = answered(results);
    newest_profiles(events.into_iter().map(|(_, event)| event), &publics)
}

/// The newest signature-verified kind-0 per author, parsed; events by
/// anyone outside `wanted` or with a bad signature are dropped.
fn newest_profiles(
    events: impl IntoIterator<Item = Event>,
    wanted: &[PublicKey],
) -> HashMap<String, Profile> {
    let mut newest: HashMap<String, Event> = HashMap::new();
    for event in events {
        let hex = event.pubkey.to_hex();
        if !wanted.contains(&event.pubkey)
            || event.verify().is_err()
            || newest.get(&hex).is_some_and(|kept| kept.created_at >= event.created_at)
        {
            continue;
        }
        newest.insert(hex, event);
    }
    newest
        .into_iter()
        .map(|(hex, event)| (hex, parse_profile(&event.content)))
        .collect()
}

/// What the follow panel's search box holds when it is a key and not a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PastedKey {
    /// An `npub1…`, `nostr:npub1…` or `nprofile1…`; the hex pubkey.
    Public(String),
    /// Text holding an `nsec1…` or `ncryptsec1…` anywhere, or a bare 64-hex
    /// string (which `secrets::parse_secret` accepts as a secret) — never
    /// searched, echoed or sent.
    Private,
}

/// Classify a search-box value; `None` means an ordinary name search.
/// Prefixes match in any case (an uppercase bech32 off a QR code is common).
pub fn parse_pasted_key(input: &str) -> Option<PastedKey> {
    let lower = input.trim().to_ascii_lowercase();
    let text = lower.strip_prefix("nostr:").unwrap_or(&lower);
    let is_hex = text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit());
    // Anywhere in the text, not just a prefix: a line pasted from notes
    // ("backup nsec1…") would otherwise go to Brainstorm search in a URL.
    // ncryptsec1 is a password-encrypted nsec (NIP-49).
    if lower.contains("nsec1") || lower.contains("ncryptsec1") || is_hex {
        return Some(PastedKey::Private);
    }
    let public = if text.starts_with("npub1") {
        PublicKey::from_bech32(text).ok()?
    } else if text.starts_with("nprofile1") {
        Nip19Profile::from_bech32(text).ok()?.public_key
    } else {
        return None;
    };
    Some(PastedKey::Public(public.to_hex()))
}

/// Open a throwaway client on the trust relays, then widen it to the set
/// the community flows read and publish through: the account's outbox
/// relays unioned with [`trust_relays`], plus the instance relay. The set
/// is also exactly the relays `client.send_event` writes to.
async fn community_client(pubkey: &str) -> (Client, Vec<String>) {
    let client = Client::default();
    for url in trust_relays() {
        let _ = client.add_relay(url).await;
    }
    client.connect().and_wait(Duration::from_secs(5)).await;
    let mut urls: BTreeSet<String> = outbox_relays(&client, pubkey).await.into_iter().collect();
    urls.insert(trust::normalize_relay(&relay_url()));
    for url in &urls {
        let _ = client.add_relay(url.as_str()).await;
    }
    client.connect().and_wait(Duration::from_secs(3)).await;
    (client, urls.into_iter().collect())
}

/// The account's kind-3 under the Brainstorm `probeContactList` majority
/// rule — see [`trust::classify_contacts`].
async fn read_contacts(client: &Client, pubkey: &str, relays: &[String]) -> trust::ContactRead {
    let Ok(public) = PublicKey::parse(pubkey) else {
        return trust::ContactRead::Inconclusive {
            answered: 0,
            asked: relays.len(),
        };
    };
    let filter = Filter::new()
        .kind(Kind::ContactList)
        .author(public)
        .limit(5);
    let results = read_per_relay(client, relays, filter).await;
    trust::classify_contacts(&results, pubkey)
}

/// Broadcast `event` to every relay `client` knows (the full `urls` set
/// `community_client` returned). Returns the normalized URLs that accepted
/// it and how many were asked.
async fn publish_event(client: &Client, urls: &[String], event: &Event) -> (Vec<String>, usize) {
    let asked = urls.len();
    let accepted = match client.send_event(event).await {
        Ok(output) => output
            .success
            .keys()
            .map(|url| trust::normalize_relay(url.as_str()))
            .collect(),
        Err(_) => Vec::new(),
    };
    (accepted, asked)
}

/// `Command::Follow`: read the account's kind-3 under the majority rule,
/// merge `target` in (or start the list, with consent or an app-made key),
/// sign and publish — or report why nothing was published.
async fn follow_account(
    sessions: SharedSessions,
    pubkey: String,
    generation: u32,
    target: String,
    allow_first_list: bool,
    updates: mpsc::UnboundedSender<Update>,
) {
    let result = do_follow(&sessions, &pubkey, &target, allow_first_list).await;
    let _ = updates.unbounded_send(Update::Community {
        pubkey,
        generation,
        part: CommunityPart::Followed(result),
    });
}

async fn do_follow(
    sessions: &SharedSessions,
    pubkey: &str,
    target: &str,
    allow_first_list: bool,
) -> Result<FollowOk, FollowErr> {
    let session = session(sessions, pubkey).await.map_err(|_| {
        FollowErr::Failed("This account has no secret key saved. Re-add the account.")
    })?;
    // The session lock is held across the whole read → merge → publish: two
    // Follow commands can never build on the same base list, and the
    // calculate step sees only a settled contact_list.
    let mut session = session.lock().await;
    let keys = session.keys.clone();
    let (client, urls) = community_client(pubkey).await;
    let mut absent = None;
    let relay_found = match read_contacts(&client, pubkey, &urls).await {
        trust::ContactRead::Found(event) => Some(event),
        trust::ContactRead::Absent { answered, asked } => {
            absent = Some((answered, asked));
            None
        }
        trust::ContactRead::Inconclusive { answered, asked } => {
            client.disconnect().await;
            return Err(FollowErr::Inconclusive { answered, asked });
        }
    };
    // The base is the NEWEST list this session knows — the relay's, or the
    // one we signed moments ago (relays lag behind a publish).
    let base = trust::merge_base(relay_found, session.contact_list.clone());
    if base.is_none() {
        // Genuinely no list anywhere this session can see. An app-minted
        // key was never used elsewhere: no list is a fact, not a hazard.
        // Anything else needs the user's word.
        let (answered, asked) = absent.unwrap_or((0, urls.len()));
        let minted = secrets::created_in_app(pubkey).unwrap_or(false);
        if !minted && !allow_first_list {
            client.disconnect().await;
            return Err(FollowErr::NeedsConfirmation { answered, asked });
        }
    }
    // `merge_follow(None)` is the first-list bootstrap: one p tag, "" content.
    let Some((tags, content)) = trust::merge_follow(base.as_ref(), target) else {
        // The newest list already follows this key — nothing to publish,
        // but the list itself is the followList input the calculate step
        // would otherwise re-read.
        session.contact_list = base;
        client.disconnect().await;
        return Ok(FollowOk {
            target: target.to_string(),
            accepted: Vec::new(),
            rejected: Vec::new(),
            asked: urls.len(),
            already: true,
        });
    };
    let signed = EventBuilder::new(Kind::ContactList, content)
        .tags(tags)
        .finalize(&keys)
        .map_err(|_| {
            FollowErr::Failed("Couldn't sign your follow list. Restart the app and try again.")
        });
    let event = match signed {
        Ok(event) => event,
        Err(fail) => {
            client.disconnect().await;
            return Err(fail);
        }
    };
    let (accepted, asked) = publish_event(&client, &urls, &event).await;
    client.disconnect().await;
    if accepted.is_empty() {
        return Err(FollowErr::Rejected { asked });
    }
    let rejected: Vec<String> = urls
        .iter()
        .map(|url| trust::normalize_relay(url))
        .filter(|url| !accepted.contains(url))
        .collect();
    // The newest list this session knows of: the calculate step posts it
    // to followList without another relay round-trip.
    session.contact_list = Some(event);
    Ok(FollowOk {
        target: target.to_string(),
        accepted,
        rejected,
        asked,
        already: false,
    })
}

/// The account's Brainstorm session token: the session's cached one, else
/// the challenge exchange. `force` mints a new one (the 401 retry).
async fn brainstorm_token(
    session: &Arc<TokioMutex<Session>>,
    force: bool,
) -> Result<Secret, String> {
    if !force && let Some(token) = session.lock().await.brainstorm.clone() {
        return Ok(token);
    }
    let (hex, keys) = {
        let session = session.lock().await;
        (session.keys.public_key().to_hex(), session.keys.clone())
    };
    let challenge = brainstorm::auth_challenge(&hex)
        .await
        .map_err(str::to_string)?;
    let login = brainstorm::login_event(&keys, &challenge);
    let token = brainstorm::verify_challenge(&hex, &login)
        .await
        .map_err(str::to_string)?;
    session.lock().await.brainstorm = Some(token.clone());
    Ok(token)
}

/// The account's kind-3 to ingest: the one this session published or
/// read, else a fresh majority-rule read. `None` when there is none.
async fn current_contact_list(session: &Arc<TokioMutex<Session>>, pubkey: &str) -> Option<Event> {
    if let Some(event) = session.lock().await.contact_list.clone() {
        return Some(event);
    }
    let (client, urls) = community_client(pubkey).await;
    let read = read_contacts(&client, pubkey, &urls).await;
    client.disconnect().await;
    match read {
        trust::ContactRead::Found(event) => Some(event),
        _ => None,
    }
}

const LOGIN_REFUSED: &str = "Brainstorm refused the login. Try again, or visit brainstorm.world.";
const RUN_IN_FLIGHT: &str = "A calculation is already running for this account. Wait for it to finish.";
const RUN_UNREADABLE: &str = "Couldn't read the run's progress from Brainstorm. Try again later.";

/// What Calculate and Recalculate share: login, POST the kind-3 unless it
/// is the list Brainstorm last accepted (remembered in the account store),
/// then POST graperank once. A 401 mints one fresh token and retries that
/// one call; a 403 or 429 is never retried. Refused while another run for
/// the account is in flight. `Ok` is the token that worked, for the caller's
/// reads, and the guard that keeps the account busy until it is dropped.
async fn request_run(
    session: &Arc<TokioMutex<Session>>,
    pubkey: &str,
    event: &Event,
) -> Result<(Secret, RunGuard), String> {
    let flag = session.lock().await.run_busy.clone();
    let guard = RunGuard::acquire(&flag).ok_or_else(|| RUN_IN_FLIGHT.to_string())?;
    let mut token = brainstorm_token(session, false).await?;
    let event_id = event.id.to_hex();
    // A store that cannot be read counts as "never sent": one extra POST
    // beats a recalculation that never sees the new follow.
    let sent = secrets::follow_list_sent(pubkey).ok().flatten();
    if sent.as_deref() != Some(event_id.as_str()) {
        let outcome = match brainstorm::post_follow_list(&token, event).await {
            // A token too old to be valid: mint once, retry once.
            Err(brainstorm::Fail::Unauthorized) => {
                token = brainstorm_token(session, true).await?;
                brainstorm::post_follow_list(&token, event).await
            }
            other => other,
        };
        match outcome {
            Ok(()) => {
                // Accepted is accepted: a failed write only costs one
                // re-send later.
                let _ = secrets::store_follow_list_sent(pubkey, &event_id);
            }
            Err(brainstorm::Fail::Unauthorized) => return Err(LOGIN_REFUSED.into()),
            Err(brainstorm::Fail::Text(message)) => return Err(message.into()),
        }
    }

    let outcome = match brainstorm::post_graperank(&token).await {
        Err(brainstorm::Fail::Unauthorized) => {
            token = brainstorm_token(session, true).await?;
            brainstorm::post_graperank(&token).await
        }
        other => other,
    };
    match outcome {
        Ok(()) => {
            session.lock().await.graperank_ok = true;
            Ok((token, guard))
        }
        Err(brainstorm::Fail::Unauthorized) => Err(LOGIN_REFUSED.into()),
        Err(brainstorm::Fail::Text(message)) => Err(message.into()),
    }
}

/// `Command::CalculateTrust`: the Brainstorm equivalent of the mock's "one
/// click" — login, POST the kind-3 (once per event id), POST graperank,
/// then a single `/setup` probe.
async fn calculate_trust(
    sessions: SharedSessions,
    pubkey: String,
    generation: u32,
    updates: mpsc::UnboundedSender<Update>,
) {
    let send = |result: Result<SetupState, String>| {
        let _ = updates.unbounded_send(Update::Community {
            pubkey: pubkey.clone(),
            generation,
            part: CommunityPart::Calculated(result),
        });
    };
    let session = match session(&sessions, &pubkey).await {
        Ok(session) => session,
        Err(e) => return send(Err(e.to_string())),
    };
    let hex = session.lock().await.keys.public_key().to_hex();

    // There is nothing to calculate without a list.
    let Some(event) = current_contact_list(&session, &pubkey).await else {
        return send(Err(
            "Follow at least one account first, then try again.".into()
        ));
    };
    if let Err(message) = request_run(&session, &pubkey, &event).await {
        return send(Err(message));
    }

    // The calculation is queued — the setup answer may still take a while.
    send(Ok(match brainstorm::get_setup(&hex).await {
        Ok(Some(rows)) => SetupState::Ready { rows: rows.len() },
        Ok(None) => SetupState::Missing,
        Err(_) => SetupState::Unreadable,
    }));
}

/// How long a run is polled before the button gives up waiting: a queue
/// that long means the run is better checked later than watched.
const RECALC_PATIENCE: Duration = Duration::from_secs(4 * 3_600);

/// `Command::Recalculate`: [`request_run`], then the `graperankResult` poll
/// every [`recalc_poll`] until done or failed, then `/user/history`. A
/// missed read waits for the next tick; three in a row end the run, as does
/// a second 401 or the account's removal. Nothing is POSTed again.
async fn recalculate(
    sessions: SharedSessions,
    pubkey: String,
    generation: u32,
    updates: mpsc::UnboundedSender<Update>,
) {
    let send = |part: CommunityPart| {
        let _ = updates.unbounded_send(Update::Community {
            pubkey: pubkey.clone(),
            generation,
            part,
        });
    };
    let session = match session(&sessions, &pubkey).await {
        Ok(session) => session,
        Err(e) => return send(CommunityPart::Recalc(Err(e.to_string()))),
    };
    // Brainstorm fails a run with no follows (NO_ELIGIBLE_USERS): refuse it
    // here instead of spending the rate limit on it.
    let event = current_contact_list(&session, &pubkey)
        .await
        .filter(|event| trust::follow_count(event, &pubkey).is_some_and(|n| n > 0));
    let Some(event) = event else {
        return send(CommunityPart::Recalc(Err(brainstorm::FOLLOW_FIRST.into())));
    };
    let (mut token, _guard) = match request_run(&session, &pubkey, &event).await {
        Ok(run) => run,
        Err(message) => return send(CommunityPart::Recalc(Err(message))),
    };

    let started = tokio::time::Instant::now();
    let interval = recalc_poll();
    let mut first = true;
    let mut relogged = false;
    let mut failed_reads = 0;
    loop {
        if !first {
            tokio::time::sleep(interval).await;
        }
        first = false;
        // A removed account (or a re-added one, with a new session) is no
        // longer this run's to watch.
        let current = sessions.lock().await.get(&pubkey).cloned();
        if !current.is_some_and(|current| Arc::ptr_eq(&current, &session)) {
            return;
        }
        let read = match brainstorm::get_graperank_result(&token).await {
            // One fresh login per run; a second 401 ends it.
            Err(brainstorm::Fail::Unauthorized) if !relogged => {
                relogged = true;
                match brainstorm_token(&session, true).await {
                    Ok(fresh) => {
                        token = fresh;
                        brainstorm::get_graperank_result(&token).await
                    }
                    Err(_) => Err(brainstorm::Fail::Unauthorized),
                }
            }
            other => other,
        };
        match read {
            Ok(Some(state)) => {
                failed_reads = 0;
                send(CommunityPart::Recalc(Ok(state)));
                if state.is_final() {
                    break;
                }
            }
            Err(brainstorm::Fail::Unauthorized) if relogged => {
                return send(CommunityPart::Recalc(Err(LOGIN_REFUSED.into())));
            }
            _ => {
                failed_reads += 1;
                if failed_reads >= 3 {
                    return send(CommunityPart::Recalc(Err(RUN_UNREADABLE.into())));
                }
            }
        }
        if started.elapsed() >= RECALC_PATIENCE {
            return send(CommunityPart::Recalc(Err(
                "Brainstorm is still working on it. Check back later.".into(),
            )));
        }
    }
    if let Some(finished) = last_finished(&session).await {
        send(CommunityPart::LastFinished(finished));
    }
}

/// `GET /user/history` with the session's token (one re-mint on a 401).
/// `None` when it could not be read.
async fn last_finished(session: &Arc<TokioMutex<Session>>) -> Option<Option<u64>> {
    let token = brainstorm_token(session, false).await.ok()?;
    match brainstorm::get_history(&token).await {
        Err(brainstorm::Fail::Unauthorized) => {
            let fresh = brainstorm_token(session, true).await.ok()?;
            brainstorm::get_history(&fresh).await.ok()
        }
        other => other.ok(),
    }
}

/// `Command::FetchRunHistory`: the last finished run, only for an account
/// that already sent Brainstorm a follow list from this app.
async fn fetch_run_history(
    sessions: SharedSessions,
    pubkey: String,
    generation: u32,
    updates: mpsc::UnboundedSender<Update>,
) {
    if !matches!(secrets::follow_list_sent(&pubkey), Ok(Some(_))) {
        return;
    }
    let Ok(session) = session(&sessions, &pubkey).await else {
        return;
    };
    if let Some(finished) = last_finished(&session).await {
        let _ = updates.unbounded_send(Update::Community {
            pubkey,
            generation,
            part: CommunityPart::LastFinished(finished),
        });
    }
}

/// `Command::CheckSetup`: the `/setup` read and nothing else. Never a POST —
/// the "Try again" link must not spend the shared rate limit.
async fn check_setup(pubkey: String, generation: u32, updates: mpsc::UnboundedSender<Update>) {
    let state = match brainstorm::get_setup(&pubkey).await {
        Ok(Some(rows)) => SetupState::Ready { rows: rows.len() },
        Ok(None) => SetupState::Missing,
        Err(_) => SetupState::Unreadable,
    };
    let _ = updates.unbounded_send(Update::Community {
        pubkey,
        generation,
        part: CommunityPart::SetupChecked(state),
    });
}

/// `Command::PublishMap`: `/setup` rows → kind-10040, but only after the
/// majority rule says no map already exists (or only a withdrawn one —
/// empty tags — does). An existing map wins; we refuse rather than replace.
async fn publish_map(
    sessions: SharedSessions,
    pubkey: String,
    generation: u32,
    updates: mpsc::UnboundedSender<Update>,
) {
    let send = |result: Result<PublishOk, String>| {
        let _ = updates.unbounded_send(Update::Community {
            pubkey: pubkey.clone(),
            generation,
            part: CommunityPart::MapPublished(result),
        });
    };
    let session = match session(&sessions, &pubkey).await {
        Ok(session) => session,
        Err(e) => return send(Err(e.to_string())),
    };
    if !session.lock().await.graperank_ok {
        return send(Err(
            "Ask Brainstorm to calculate your trust scores first.".into()
        ));
    }
    let keys = session.lock().await.keys.clone();
    let rows = match brainstorm::get_setup(&pubkey).await {
        Ok(Some(rows)) => rows,
        Ok(None) => {
            return send(Err(
                "Brainstorm has no data for this account yet. The first calculation can take \
                 an hour. Try again later."
                    .into(),
            ));
        }
        Err(message) => return send(Err(message.into())),
    };

    let (client, urls) = community_client(&pubkey).await;
    let read = match PublicKey::parse(&pubkey) {
        Ok(public) => {
            let filter = Filter::new().kind(Kind::Custom(10040)).author(public);
            let results = read_per_relay(&client, &urls, filter).await;
            trust::classify_replaceable(&results, &pubkey, Kind::Custom(10040))
        }
        Err(_) => trust::ContactRead::Inconclusive {
            answered: 0,
            asked: urls.len(),
        },
    };
    let outcome = match read {
        // An existing map stands — tell the user to reload instead of
        // replacing it. A map with no tags is a withdrawal: safe to re-add.
        trust::ContactRead::Found(event) if event.tags.iter().next().is_some() => {
            Err("This account already has a Treasure Map. Check again to load it.".into())
        }
        trust::ContactRead::Found(_) | trust::ContactRead::Absent { .. } => {
            let event = trust::map_event(&keys, &rows);
            let (accepted, asked) = publish_event(&client, &urls, &event).await;
            if accepted.is_empty() {
                Err(format!(
                    "No Nostr server accepted your Treasure Map (tried {asked}). Try again."
                ))
            } else {
                Ok(PublishOk {
                    accepted,
                    asked,
                    rows: rows.len(),
                })
            }
        }
        trust::ContactRead::Inconclusive { answered, asked } => Err(format!(
            "Couldn't check for an existing Treasure Map: {answered} of {asked} Nostr servers answered. Nothing was published. Try again."
        )),
    };
    client.disconnect().await;
    send(outcome);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::parse_detail;

    const COORD: &str = "39998:aa:bagels";

    fn claim_at(keys: &Keys, name: &str, z: &[&str], at: u64) -> Event {
        let mut tags = vec![Tag::parse(vec!["d".to_string(), name.to_string()]).unwrap()];
        for coordinate in z {
            tags.push(Tag::parse(vec!["z".to_string(), coordinate.to_string()]).unwrap());
        }
        EventBuilder::new(Kind::Custom(events::CLAIM_KIND), name)
            .tags(tags)
            .custom_created_at(Timestamp::from(at))
            .finalize(keys)
            .unwrap()
    }

    fn bounty_row(extra: serde_json::Value) -> Bounty {
        let mut row = serde_json::json!({
            "id": "b1",
            "issuer_pubkey": "aa",
            "list_coordinate": COORD,
            "amount_sats": 100,
            "status": "open",
            "created_at": 100,
        });
        if let (Some(row), Some(extra)) = (row.as_object_mut(), extra.as_object()) {
            row.extend(extra.clone());
        }
        serde_json::from_value(row).unwrap()
    }

    #[test]
    fn the_auto_pay_poll_defaults_to_30_seconds() {
        assert_eq!(autopay_secs_from(None), 30);
        assert_eq!(autopay_secs_from(Some(" 2 ")), 2);
        assert_eq!(autopay_secs_from(Some("0")), 30);
        assert_eq!(autopay_secs_from(Some("soon")), 30);
    }

    #[test]
    fn a_replaced_claim_is_one_claim_and_stray_events_are_not_claims() {
        let bob = Keys::generate();
        let carol = Keys::generate();
        let events = vec![
            claim_at(&bob, "everything", &[COORD], 200),
            claim_at(&bob, "everything", &[COORD], 300),
            claim_at(&carol, "sesame", &[COORD], 250),
            // Too old for the bounty, another list, two z tags, no d tag.
            claim_at(&carol, "old", &[COORD], 50),
            claim_at(&carol, "other", &["39998:aa:donuts"], 260),
            claim_at(&carol, "both", &[COORD, "39998:aa:donuts"], 270),
        ];
        let current = current_claims(&events, COORD, 100);
        let found: Vec<(String, u64)> = current
            .iter()
            .map(|e| (e.content.clone(), e.created_at.as_secs()))
            .collect();
        assert_eq!(
            found,
            [("sesame".to_string(), 250), ("everything".to_string(), 300)]
        );
        assert_eq!(
            claim_address(current[1]),
            Some(format!("{}:everything", bob.public_key().to_hex()))
        );
    }

    #[test]
    fn the_client_count_fulfils_a_bounty_whose_slots_are_paid() {
        let paid = |n: u64, claimant: &str| ClaimFacts {
            address: format!("{claimant}:{n}"),
            event_id: format!("e{n}"),
            claimant: claimant.into(),
            created_at: n,
            paid: true,
            attempt: None,
            rank: None,
        };
        let mut bounty = bounty_row(serde_json::json!({
            "bounty_cap_sats": 200, "reward_per_item": 1, "max_rewards_per_npub": 1,
            "derivedStatus": "open",
        }));
        apply_usage(&mut bounty, &[paid(1, "bob")]);
        let state = bounty.payment_state.clone().unwrap();
        assert_eq!((state.paid_reward_count, state.open_reward_slots, state.total_reward_slots), (1, 1, 2));
        assert_eq!(bounty.effective_status(), "open");
        apply_usage(&mut bounty, &[paid(1, "bob"), paid(2, "carol")]);
        assert!(bounty.payment_state.as_ref().unwrap().fulfilled);
        assert_eq!(bounty.effective_status(), "fulfilled");
    }

    #[test]
    fn an_attempt_whose_claim_left_the_relay_still_holds_its_slot() {
        let bounty = bounty_row(serde_json::json!({}));
        let attempt = |bounty_id: &str, address: &str| secrets::PayAttempt {
            bounty_id: bounty_id.into(),
            claim_address: address.into(),
            claim_event_id: "gone".into(),
            claimant: "bob".into(),
            sats: 100,
            state: AttemptState::Paid,
            reason: None,
            started_at: 1,
            updated_at: 1,
        };
        let present = ClaimFacts {
            address: "carol:x".into(),
            event_id: "e1".into(),
            claimant: "carol".into(),
            created_at: 1,
            paid: false,
            attempt: Some(AttemptState::Paid),
            rank: None,
        };
        let attempts = [
            attempt("b1", "bob:y"),
            attempt("b1", "carol:x"),
            attempt("other", "dave:z"),
        ];
        let orphans = orphan_attempts(&bounty, std::slice::from_ref(&present), &attempts);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].address, "bob:y");
        assert_eq!(orphans[0].attempt, Some(AttemptState::Paid));
    }

    /// A kind-9735 from some zapper around `request`, naming `claim_id`.
    fn receipt_around(request: &Event, claim_id: &str) -> Event {
        EventBuilder::new(Kind::Custom(events::RECEIPT_KIND), "")
            .tags([
                Tag::parse(vec!["e".to_string(), claim_id.to_string()]).unwrap(),
                Tag::parse(vec![
                    "description".to_string(),
                    serde_json::to_string(request).unwrap(),
                ])
                .unwrap(),
            ])
            .finalize(&Keys::generate())
            .unwrap()
    }

    #[test]
    fn a_server_receipt_the_app_cannot_verify_does_not_pay_the_claim() {
        let issuer = Keys::generate();
        let issuer_hex = issuer.public_key().to_hex();
        let bounty = bounty_row(serde_json::json!({ "issuer_pubkey": issuer_hex }));
        let bob = Keys::generate();
        let event = claim_at(&bob, "everything", &[COORD], 200);
        let id = event.id.to_hex();
        let (fact, _) = facts_for(&bounty, std::slice::from_ref(&event), &[], &[])
            .into_iter()
            .next()
            .unwrap();
        assert!(!fact.paid);

        // What the server attaches: description pubkey = issuer, junk sig.
        let forged = RawEvent {
            id: "f0".into(),
            pubkey: "zz".into(),
            kind: Some(events::RECEIPT_KIND),
            created_at: Some(300),
            tags: vec![
                vec!["e".into(), id.clone()],
                vec![
                    "description".into(),
                    serde_json::json!({ "pubkey": issuer_hex, "kind": 9734, "sig": "00" }).to_string(),
                ],
            ],
            content: String::new(),
            sig: Some("00".into()),
        };
        let server = Claim {
            zap_receipt: Some(forged),
            ..Claim::from_event(RawEvent::from_event(&event).unwrap())
        };
        let merged = merge_claim(Some(server), &fact, &event, &[], &bounty, &[], 300).unwrap();
        assert!(merged.zap_receipt.is_none(), "the forged receipt must not count");

        // A receipt around the issuer's own signed request for the reward does.
        let relays = vec!["wss://nos.lol".to_string()];
        let request = events::zap_request(&issuer, &bob.public_key().to_hex(), &id, COORD, "b1", 100, &relays).unwrap();
        let receipts = vec![receipt_around(&request, &id)];
        let (fact, _) = facts_for(&bounty, std::slice::from_ref(&event), &receipts, &[])
            .into_iter()
            .next()
            .unwrap();
        assert!(fact.paid);
        let merged = merge_claim(None, &fact, &event, &receipts, &bounty, &[], 300).unwrap();
        assert!(merged.zap_receipt.is_some());
    }

    #[test]
    fn facts_count_only_this_bountys_reward_and_attempts() {
        let issuer = Keys::generate();
        let bounty = bounty_row(serde_json::json!({ "issuer_pubkey": issuer.public_key().to_hex() }));
        let bob = Keys::generate();
        let event = claim_at(&bob, "everything", &[COORD], 200);
        let id = event.id.to_hex();
        let relays = vec!["wss://nos.lol".to_string()];
        // A zap for 99 sats is not this bounty's 100-sat reward.
        let short = events::zap_request(&issuer, &bob.public_key().to_hex(), &id, COORD, "b1", 99, &relays).unwrap();
        // An attempt on another bounty at the same address is not this one's.
        let elsewhere = secrets::PayAttempt {
            bounty_id: "other".into(),
            claim_address: format!("{}:everything", bob.public_key().to_hex()),
            claim_event_id: id.clone(),
            claimant: bob.public_key().to_hex(),
            sats: 100,
            state: AttemptState::Paid,
            reason: None,
            started_at: 1,
            updated_at: 1,
        };
        let receipts = vec![receipt_around(&short, &id)];
        let (fact, _) = facts_for(&bounty, std::slice::from_ref(&event), &receipts, &[elsewhere])
            .into_iter()
            .next()
            .unwrap();
        assert!(!fact.paid);
        assert_eq!(fact.attempt, None);
    }

    #[test]
    fn a_timed_out_create_is_matched_to_exactly_one_new_bounty() {
        let req = CreateBounty {
            list_coordinate: COORD.into(),
            amount_sats: 100,
            criteria: "Bagel shops".into(),
            bounty_cap_sats: None,
            reward_per_item: true,
            max_rewards_per_npub: Some(1),
            auto_pay: true,
            auto_pay_min_rank: Some(2),
        };
        let row = |id: &str, issuer: &str, amount: u64, at: u64| {
            bounty_row(serde_json::json!({
                "id": id, "issuer_pubkey": issuer, "amount_sats": amount,
                "criteria": "Bagel shops", "created_at": at,
            }))
        };
        let sent_at = 10_000;
        let list = vec![
            row("old", "aa", 100, 1_000),
            row("theirs", "bb", 100, 10_001),
            row("other-reward", "aa", 50, 10_001),
            row("new", "aa", 100, 10_002),
        ];
        assert_eq!(created_bounty(&list, "aa", &req, sent_at), Ok(Some("new".into())));
        assert_eq!(created_bounty(&list[..3], "aa", &req, sent_at), Ok(None));
        let mut twice = list.clone();
        twice.push(row("new-2", "aa", 100, 10_003));
        assert_eq!(created_bounty(&twice, "aa", &req, sent_at), Err(()));
    }

    /// A store in its own temp dir, with one generated account.
    fn temp_store(name: &str) -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("mc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("MC_STORE_DIR", &dir) };
        let keys = Keys::generate();
        let pubkey = secrets::add_account(&Secret::new(keys.secret_key().to_bech32().unwrap()), true)
            .unwrap()
            .pubkey;
        (dir, pubkey)
    }

    fn sending(bounty: &str, address: &str, claimant: &str) -> secrets::PayAttempt {
        secrets::PayAttempt {
            bounty_id: bounty.into(),
            claim_address: address.into(),
            claim_event_id: format!("e-{address}"),
            claimant: claimant.into(),
            sats: 100,
            state: AttemptState::Sending,
            reason: None,
            started_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn a_refused_recheck_signs_nothing_and_a_failed_invoice_frees_the_claim() {
        let (dir, me) = temp_store("record-first");
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let ctx = AutoPayCtx::default();
        let one = secrets::SlotLimits { slots: 1, per_npub: Some(1), paid: Vec::new() };
        let signed = std::sync::atomic::AtomicUsize::new(0);
        let invoice = |answer: Result<String, String>| {
            let signed = &signed;
            move || async move {
                signed.fetch_add(1, Ordering::SeqCst);
                answer
            }
        };
        runtime.block_on(async {
            // Another instance already took the one slot.
            let x = sending("b1", "x:1", "x");
            assert!(matches!(
                secrets::begin_attempt_within(&me, x, Some(&one)),
                Ok(secrets::Begin::Recorded)
            ));
            let full = record_then_invoice(&ctx, &me, sending("b1", "y:1", "y"), &one, invoice(Ok("lnbc".into()))).await;
            assert_eq!(full, Err(PayOutcome::Full));
            assert_eq!(signed.load(Ordering::SeqCst), 0, "no zap request after a refusal");

            // An invoice that can't be had takes the attempt back.
            let failed = record_then_invoice(&ctx, &me, sending("b2", "y:1", "y"), &one, invoice(Err("no zaps".into()))).await;
            assert_eq!(failed, Err(PayOutcome::Retry));
            assert!(secrets::pay_attempts(&me).unwrap().iter().all(|a| a.bounty_id != "b2"));
            assert_eq!(ctx.note_for("y:1").as_deref(), Some("no zaps"));

            let paid = record_then_invoice(&ctx, &me, sending("b2", "y:1", "y"), &one, invoice(Ok("lnbc".into()))).await;
            assert_eq!(paid, Ok("lnbc".to_string()));
            assert_eq!(signed.load(Ordering::SeqCst), 2);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_claimants_app_reads_a_verified_receipt_as_settled() {
        let issuer = Keys::generate();
        let bounty = bounty_row(serde_json::json!({ "issuer_pubkey": issuer.public_key().to_hex() }));
        let bob = Keys::generate();
        let event = claim_at(&bob, "everything", &[COORD], 200);
        let id = event.id.to_hex();
        let relays = vec!["wss://nos.lol".to_string()];
        let request = events::zap_request(&issuer, &bob.public_key().to_hex(), &id, COORD, "b1", 100, &relays).unwrap();
        let receipts = vec![receipt_around(&request, &id)];
        let (fact, _) = facts_for(&bounty, std::slice::from_ref(&event), &receipts, &[])
            .into_iter()
            .next()
            .unwrap();
        // No attempts: this is not the issuer's app.
        let merged = merge_claim(None, &fact, &event, &receipts, &bounty, &[], 300).unwrap();
        let row = merged.auto_payment.expect("a payment row for the card");
        assert_eq!(row.state, "settled");
        assert_eq!(row.created_at, None, "no invented attempt time");
        assert!(!row.is_pending());
        // Unpaid, nothing is made up.
        let (fact, _) = facts_for(&bounty, std::slice::from_ref(&event), &[], &[])
            .into_iter()
            .next()
            .unwrap();
        let unpaid = merge_claim(None, &fact, &event, &[], &bounty, &[], 300).unwrap();
        assert!(unpaid.auto_payment.is_none());
    }

    #[test]
    fn an_unreadable_store_still_hides_the_servers_unverified_receipt() {
        let dir = std::env::temp_dir().join(format!("mc-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("accounts.json"), "not json").unwrap();
        unsafe { std::env::set_var("MC_STORE_DIR", &dir) };
        let claim: serde_json::Value = serde_json::json!({
            "event": { "id": "c1", "pubkey": "bb", "kind": 39999, "created_at": 200, "tags": [], "content": "x" },
            "zapReceipt": { "id": "r1", "pubkey": "zz", "kind": 9735, "created_at": 300,
                "tags": [["e", "c1"], ["description", "{\"pubkey\":\"aa\"}"]], "content": "" },
        });
        let detail = |auto_pay: u64, claim: &serde_json::Value| {
            parse_detail(
                &serde_json::json!({
                    "success": true,
                    "bounty": { "id": "b1", "issuer_pubkey": "aa", "list_coordinate": COORD,
                        "amount_sats": 100, "status": "open", "auto_pay": auto_pay },
                    "claims": [claim],
                })
                .to_string(),
            )
            .unwrap()
        };
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let ctx = AutoPayCtx::default();
        runtime.block_on(async {
            let client = Client::default();
            let app_paid = annotate(&ctx, &client, detail(0, &claim)).await;
            assert!(app_paid.policy.is_none(), "nothing decided without the store");
            assert!(app_paid.detail.claims[0].zap_receipt.is_none());
            // The server's own auto-pay bounties keep what the server says.
            let server_paid = annotate(&ctx, &client, detail(1, &claim)).await;
            assert!(server_paid.detail.claims[0].zap_receipt.is_some());
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_claim_paid_on_one_bounty_is_still_payable_on_another_on_the_same_list() {
        let issuer = Keys::generate();
        let issuer_hex = issuer.public_key().to_hex();
        let b1 = bounty_row(serde_json::json!({ "id": "b1", "issuer_pubkey": issuer_hex }));
        let b2 = bounty_row(serde_json::json!({ "id": "b2", "issuer_pubkey": issuer_hex }));
        let bob = Keys::generate();
        let event = claim_at(&bob, "everything", &[COORD], 200);
        let id = event.id.to_hex();
        let relays = vec!["wss://nos.lol".to_string()];
        let paid_on_b1 =
            events::zap_request(&issuer, &bob.public_key().to_hex(), &id, COORD, "b1", 100, &relays).unwrap();
        let receipts = vec![receipt_around(&paid_on_b1, &id)];
        let fact_on = |bounty: &Bounty| {
            let mut fact = facts_for(bounty, std::slice::from_ref(&event), &receipts, &[])
                .into_iter()
                .next()
                .unwrap()
                .0;
            fact.rank = Some(5.0);
            fact
        };

        let on_b1 = fact_on(&b1);
        assert!(on_b1.paid);
        let on_b2 = fact_on(&b2);
        assert!(!on_b2.paid, "B1's receipt does not pay the claim on B2");

        // B2 still has its slot and still pays the claim.
        let mut b2_counted = b2.clone();
        apply_usage(&mut b2_counted, std::slice::from_ref(&on_b2));
        let state = b2_counted.payment_state.unwrap();
        assert_eq!((state.paid_reward_count, state.open_reward_slots), (0, 1));
        let policy = Policy::of(&b2, true, 2);
        let plan: HashMap<String, Result<(), Hold>> =
            autopay::plan(&policy, std::slice::from_ref(&on_b2)).into_iter().collect();
        assert_eq!(plan[&on_b2.address], Ok(()));

        // Neither the settled row nor the receipt shows on B2's card.
        let merged = merge_claim(None, &on_b2, &event, &receipts, &b2, &[], 300).unwrap();
        assert!(merged.zap_receipt.is_none());
        assert!(merged.auto_payment.is_none());
        let merged = merge_claim(None, &on_b1, &event, &receipts, &b1, &[], 300).unwrap();
        assert_eq!(merged.auto_payment.map(|r| r.state).as_deref(), Some("settled"));
    }

    #[test]
    fn a_stored_attempt_reads_like_a_server_payment_row() {
        let attempt = |state| secrets::PayAttempt {
            bounty_id: "b1".into(),
            claim_address: "bob:x".into(),
            claim_event_id: "old".into(),
            claimant: "bob".into(),
            sats: 100,
            state,
            reason: None,
            started_at: 1_000,
            updated_at: 1_000,
        };
        let row = |state, receipt, now| attempt_row(&attempt(state), "e2", receipt, now).state;
        assert_eq!(row(AttemptState::Sending, false, 1_010), "attempting");
        assert_eq!(row(AttemptState::Sending, false, 2_000), "unknown");
        assert_eq!(row(AttemptState::Unknown, false, 1_010), "unknown");
        assert_eq!(row(AttemptState::Paid, false, 1_010), "paid");
        assert_eq!(row(AttemptState::Paid, false, 2_000), "paid_unreceipted");
        assert_eq!(row(AttemptState::Paid, true, 2_000), "settled");
        assert_eq!(row(AttemptState::Failed, false, 1_010), "failed");
        let full = attempt_row(&attempt(AttemptState::Paid), "e2", false, 1_010);
        assert_eq!(full.claim_event_id, "e2", "named by the claim on screen");
        assert_eq!(full.amount_sats, Some(100));
        assert!(full.is_pending(), "a fresh payment keeps the fast poll");
    }

    #[test]
    fn only_an_unanswered_create_is_outcome_unknown() {
        let unknown = |e| matches!(create_outcome(e), BridgeError::OutcomeUnknown(_));
        assert!(unknown(ApiError::Http("timed out".into())));
        assert!(unknown(ApiError::Decode("eof".into())));
        assert!(unknown(ApiError::Status { status: 502, message: String::new() }));
        assert!(!unknown(ApiError::Status { status: 400, message: String::new() }));
        assert!(!unknown(ApiError::Status { status: 401, message: String::new() }));
        assert!(!unknown(ApiError::Rejected("no".into())));
    }

    #[test]
    fn a_silent_or_dropped_relay_is_a_failed_read_not_no_events() {
        let event = EventBuilder::new(Kind::TextNote, "x").finalize(&Keys::generate()).unwrap();
        // Timed out: never an answer, connected or not.
        assert_eq!(relay_answer(None, true), Err(()));
        // The SDK's Ok(empty) after a disconnect, or from a relay never up.
        assert_eq!(relay_answer(Some(Ok(vec![])), false), Err(()));
        // A connected relay that reached EOSE with nothing: a real "none".
        assert_eq!(relay_answer(Some(Ok(vec![])), true), Ok(vec![]));
        assert_eq!(
            relay_answer(Some(Ok(vec![event.clone()])), false),
            Ok(vec![event])
        );
        assert_eq!(relay_answer(Some(Err(())), true), Err(()));
        // All timed out: nobody answered, so the designation read is Err and
        // Evidence::apply keeps the prior verdict.
        let (count, events) = answered(vec![
            ("wss://a".into(), relay_answer(None, true)),
            ("wss://b".into(), relay_answer(Some(Ok(vec![])), false)),
        ]);
        assert_eq!((count, events.len()), (0, 0));
    }

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

        // A different existing lud16 is never replaced: payouts go there.
        assert!(matches!(
            merge_lud16(Some(r#"{"lud16":"old@strike.me","name":"M"}"#), "new@coinos.io", None),
            Err(BridgeError::Profile(_))
        ));
        // The same one (a retry) goes through, not duplicated.
        let merged = merge_lud16(Some(r#"{"lud16":"new@coinos.io","name":"M"}"#), "new@coinos.io", None).unwrap();
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
    fn newest_profiles_keeps_the_newest_verified_kind_0_of_each_wanted_author() {
        let kind0 = |keys: &Keys, name: &str, at: u64| {
            EventBuilder::new(Kind::Metadata, format!(r#"{{"name":"{name}"}}"#))
                .custom_created_at(Timestamp::from_secs(at))
                .finalize(keys)
                .unwrap()
        };
        let (alice, outsider) = (Keys::generate(), Keys::generate());
        let forged = {
            let event = kind0(&alice, "Forged", 500);
            let json = event.as_json().replace("Forged", "Forgex");
            Event::from_json(json).unwrap()
        };
        let events = vec![
            kind0(&alice, "Old", 100),
            kind0(&alice, "New", 200),
            kind0(&alice, "Older", 50),
            kind0(&outsider, "Outside", 300),
            forged,
        ];
        let found = newest_profiles(events, &[alice.public_key()]);
        assert_eq!(found.len(), 1, "the outsider is dropped");
        let profile = &found[&alice.public_key().to_hex()];
        assert_eq!(profile.name.as_deref(), Some("New"), "newest wins, forged is dropped");
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
                publish_error: Some("The Nostr servers didn't accept the profile update.".into()),
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
            generated: true,
        };
        let printed = format!("{command:?}");
        assert!(!printed.contains("extremelysecret"), "leaked: {printed}");
        assert!(printed.contains("<redacted>"), "not redacted: {printed}");
    }

    #[test]
    fn community_updates_never_carry_a_token() {
        // The Brainstorm bearer token lives in the session behind a Secret;
        // nothing in the Command/Update channel may print it.
        let update = Update::Community {
            pubkey: "aa".repeat(32),
            generation: 1,
            part: CommunityPart::Calculated(Ok(SetupState::Ready { rows: 6 })),
        };
        let printed = format!("{update:?}");
        assert!(!printed.contains("token"), "leaked: {printed}");
        for command in [
            format!(
                "{:?}",
                Command::CalculateTrust {
                    pubkey: "aa".repeat(32),
                    generation: 1
                }
            ),
            format!(
                "{:?}",
                Command::PublishMap {
                    pubkey: "aa".repeat(32),
                    generation: 1
                }
            ),
            format!(
                "{:?}",
                Command::Follow {
                    pubkey: "aa".repeat(32),
                    generation: 1,
                    target: "bb".repeat(32),
                    allow_first_list: true
                }
            ),
        ] {
            assert!(!command.contains("token"), "leaked: {command}");
        }
    }

    #[test]
    fn pasted_keys_parse() {
        let keys = Keys::generate();
        let hex = keys.public_key().to_hex();
        let npub = keys.public_key().to_bech32().unwrap();
        let nprofile = Nip19Profile::new(
            keys.public_key(),
            [RelayUrl::parse("wss://r.example").unwrap()],
        )
        .to_bech32()
            .unwrap();
        let public = Some(PastedKey::Public(hex.clone()));
        assert_eq!(parse_pasted_key(&format!("  {npub}\n")), public);
        assert_eq!(parse_pasted_key(&format!(" nostr:{npub} ")), public);
        assert_eq!(parse_pasted_key(&format!("\t{nprofile} ")), public);
        let nsec = keys.secret_key().to_bech32().unwrap();
        assert_eq!(parse_pasted_key(&nsec), Some(PastedKey::Private));
        assert_eq!(parse_pasted_key(&format!("nostr:{nsec}")), Some(PastedKey::Private));
        let upper_nsec = nsec.to_ascii_uppercase();
        assert_eq!(parse_pasted_key(&upper_nsec), Some(PastedKey::Private));
        assert_eq!(
            parse_pasted_key(&format!("NOSTR:{upper_nsec}")),
            Some(PastedKey::Private)
        );
        assert_eq!(parse_pasted_key(&format!("Nostr:{npub}")), public);
        assert_eq!(parse_pasted_key(&npub.to_ascii_uppercase()), public);
        assert_eq!(parse_pasted_key(&hex), Some(PastedKey::Private));
        assert_eq!(
            parse_pasted_key(&keys.secret_key().to_secret_hex().to_ascii_uppercase()),
            Some(PastedKey::Private)
        );
        assert_eq!(
            parse_pasted_key(&format!("backup {nsec} (phone)")),
            Some(PastedKey::Private)
        );
        assert_eq!(
            parse_pasted_key("ncryptsec1qgg9947rlpvqu76pj5ecreduf9jxhselq2nae2kghhvd5g7dgjtcxfqtd67p9m0w57lspw8gsq6yphnm8623nsl8xn9j4jdzz84zm3frztj3z7s35vpzmqf6ksu8r89qk5z2zxfmu5gv8th8wclt0h4p"),
            Some(PastedKey::Private)
        );
        assert_eq!(parse_pasted_key("alice"), None);
        assert_eq!(parse_pasted_key("npub1notarealkey"), None);
        assert_eq!(parse_pasted_key(""), None);
    }

    #[test]
    fn recalc_poll_defaults_to_15_seconds() {
        assert_eq!(poll_secs_from(None), 15);
        assert_eq!(poll_secs_from(Some("")), 15);
        assert_eq!(poll_secs_from(Some("0")), 15);
        assert_eq!(poll_secs_from(Some("soon")), 15);
        assert_eq!(poll_secs_from(Some(" 3 ")), 3);
    }

    #[test]
    fn trust_relays_env_parses_or_falls_back() {
        // The pure half: trimming, empty entries, normalization, rejection.
        assert_eq!(
            parse_relay_list(" wss://a.example/ , , wss://b.example"),
            vec!["wss://a.example".to_string(), "wss://b.example".to_string()]
        );
        assert_eq!(
            parse_relay_list("https://not-a-relay.example, ws://local:7777/"),
            vec!["ws://local:7777".to_string()]
        );
        assert!(parse_relay_list("").is_empty());
        assert!(parse_relay_list("https://x , ftp://y").is_empty());
        // Unset env → the shipped defaults.
        if std::env::var("MC_TRUST_RELAYS").is_err() {
            assert_eq!(trust_relays().len(), TRUST_FALLBACK_RELAYS.len());
            assert_eq!(public_relays().len(), PUBLIC_RELAYS.len());
        }
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
