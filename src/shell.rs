//! The app shell: the icon rail, the nav column, the top bar, and whichever
//! screen the nav points at — plus all the live state those screens render.
//!
//! Everything on screen is backed by state fed from the nostr runtime thread
//! (see `magic_carpet_chat::nostr`): the UI sends [`Command`]s down one
//! channel and drains [`Update`]s from the other into the fields here, using
//! the same task-held-in-`self` pattern as the chat's streaming reply.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{
    Root, TitleBar, WindowExt as _, h_flex, notification::Notification,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use magic_carpet_chat::api::{Bounty, BountyDetail, CreateBounty};
use magic_carpet_chat::coinos;
use magic_carpet_chat::events;
use magic_carpet_chat::nostr::{self, Command, ErrorSource, Update, WalletCreated};
use magic_carpet_chat::secrets::{self, Secret};

use crate::account::{self, AccountPage, Tab};
use crate::account_setup::{self, AccountSetup, Path as SetupPath, ReadySummary};
use crate::bounties;
use crate::chat::Chat;
use crate::dashboard::{self, MONO, avatar};
use crate::icons::icon;
use crate::palette::*;
use crate::timefmt;
use crate::trust_state::TrustState;
use crate::wallet::{self, PendingSent, SendInputs, SendState, WalletState};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    /// Create an account or paste a key. Never in the nav — reachable through
    /// the first-screen decision (no account stored) and the rail's "+".
    AccountSetup,
    Dashboard,
    Bounties,
    Payments,
    Claimants,
    Boards,
    Wallet,
    Tags,
    Chat,
    Accounts,
    /// One stored account's page: header, five tabs, real Trust data. Not in
    /// the nav — reached from the dashboard's Fix rows, Accounts › Manage,
    /// and the sidebar's account chip.
    Account,
    Settings,
}

impl Screen {
    fn label(self) -> &'static str {
        match self {
            Screen::AccountSetup => "Account",
            Screen::Dashboard => "Dashboard",
            Screen::Bounties => "Bounties",
            Screen::Payments => "Payments",
            Screen::Claimants => "Claimants",
            Screen::Boards => "Leaderboards",
            Screen::Wallet => "Wallet",
            Screen::Tags => "Tags",
            Screen::Chat => "Chat",
            Screen::Accounts => "Accounts",
            Screen::Account => "Account",
            Screen::Settings => "Settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Screen::AccountSetup => "accounts",
            Screen::Dashboard => "overview",
            Screen::Bounties => "bounties",
            Screen::Payments => "payments",
            Screen::Claimants => "claimants",
            Screen::Boards => "boards",
            Screen::Wallet => "wallet",
            Screen::Tags => "tags",
            Screen::Chat => "chat",
            Screen::Accounts => "accounts",
            Screen::Account => "accounts",
            Screen::Settings => "settings",
        }
    }
}

/// A 16px icon in a fixed slot.
///
/// gpui's `svg` paints with the colour on its own style and inherits nothing,
/// so the colour is always passed in here.
fn glyph(name: &'static str, color: u32) -> impl IntoElement {
    div()
        .size(px(16.))
        .flex_shrink_0()
        .child(svg().path(icon(name)).size(px(16.)).text_color(rgb(color)))
}

actions!(
    shell,
    [
        GoDashboard, GoBounties, GoPayments, GoClaimants, GoBoards, GoWallet, GoTags, GoChat,
        PrevAccount, NextAccount, Refresh, NewItem, CloseForm, SelectPrev, SelectNext, Confirm
    ]
);

/// cmd-1…cmd-8 follow the nav order; cmd-[ / cmd-] step through accounts.
/// Keyboard routes exist for their own sake, but they are also the only input
/// UI automation can drive: gpui ignores synthetic mouse events posted to the
/// process, while key events arrive fine. The full demo path is in DEMO.md.
pub fn keybindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-1", GoDashboard, None),
        KeyBinding::new("cmd-2", GoBounties, None),
        KeyBinding::new("cmd-3", GoPayments, None),
        KeyBinding::new("cmd-4", GoClaimants, None),
        KeyBinding::new("cmd-5", GoBoards, None),
        KeyBinding::new("cmd-6", GoWallet, None),
        KeyBinding::new("cmd-7", GoTags, None),
        KeyBinding::new("cmd-8", GoChat, None),
        KeyBinding::new("cmd-[", PrevAccount, None),
        KeyBinding::new("cmd-]", NextAccount, None),
        KeyBinding::new("cmd-r", Refresh, None),
        KeyBinding::new("cmd-n", NewItem, None),
        // The text inputs bind escape themselves but propagate it when they
        // have nothing to do; cmd-. is the cancel that no input ever eats.
        KeyBinding::new("escape", CloseForm, None),
        KeyBinding::new("cmd-.", CloseForm, None),
        // Scoped to the Shell node: a focused text input is deeper in the
        // key-context stack, so its own up/down/enter bindings win there.
        // Context-less (None) versions of these would shadow every input's
        // Enter — GPUI treats no-context bindings as deepest-context and
        // resolves ties by later registration, and these are registered
        // after gpui_kit::init.
        KeyBinding::new("up", SelectPrev, Some("Shell")),
        KeyBinding::new("down", SelectNext, Some("Shell")),
        // The account screen's "Start": only fires when the shell itself has focus.
        KeyBinding::new("enter", Confirm, Some("Shell")),
    ]
}

/// The nav column order. Counts are computed from live state at render.
const NAV: [Screen; 8] = [
    Screen::Dashboard,
    Screen::Bounties,
    Screen::Payments,
    Screen::Claimants,
    Screen::Boards,
    Screen::Wallet,
    Screen::Tags,
    Screen::Chat,
];

// ------------------------------------------------------------- shared state

/// Async data with its failure spelled out, so every screen can render an
/// honest loading / error / empty state instead of a fake default.
pub(crate) enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// What the app knows about one stored account. The runtime announces the
/// public half of the key; the profile fills in when its kind-0 arrives.
pub(crate) struct AccountView {
    pub pubkey: String,
    pub npub: String,
    /// From the account's kind-0 on the instance relay, when one exists.
    pub kind0_name: Option<String>,
    pub picture: Option<String>,
    /// The kind-0's Lightning address — payouts need one.
    pub lud16: Option<String>,
    /// Where the kind-0 read stands. Only `Read` makes a missing `lud16`
    /// mean "none" rather than "unknown".
    pub profile: ProfileRead,
    /// The Coinos wallet, shared by the account screen's ready panel and ⌘6.
    pub wallet: WalletState,
}

/// The account's kind-0 read: under way, failed, or done (found or
/// confirmed absent).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileRead {
    Checking,
    Failed,
    Read,
}

impl AccountView {
    fn new(pubkey: String, npub: String) -> Self {
        Self {
            pubkey,
            npub,
            kind0_name: None,
            picture: None,
            lud16: None,
            profile: ProfileRead::Checking,
            wallet: WalletState::default(),
        }
    }

    /// Where payouts land: the Coinos wallet this app opened first, else the
    /// profile's lud16 as the relays have it.
    pub(crate) fn payout_address(&self) -> Option<&str> {
        self.wallet
            .created
            .as_ref()
            .map(|created| created.lightning_address.as_str())
            .or(self.lud16.as_deref())
    }

    /// Keep the funding QR in step with `payout_address`; call after any
    /// change to `lud16` or `wallet.created`.
    fn sync_qr(&mut self) {
        let address = self.payout_address().map(str::to_string);
        self.wallet.sync_qr(address.as_deref());
    }
}

/// The label shown before (or without) a kind-0: a fixed placeholder, never
/// a role or a guessed name — the app cannot know who a key belongs to until
/// the profile arrives, and a kind-0 fetch failure is silent. Not the short
/// npub: the identity line under it already shows that.
pub(crate) fn display_name(view: &AccountView) -> String {
    view.kind0_name
        .clone()
        .unwrap_or_else(|| "New account".to_string())
}

/// The avatar letters: two words' initials from the profile name, else the
/// two characters after `npub1` — so two nameless accounts do not both show
/// an "N".
pub(crate) fn initials(view: &AccountView) -> String {
    match &view.kind0_name {
        Some(name) => name
            .split_whitespace()
            .take(2)
            .filter_map(|word| word.chars().next())
            .collect::<String>()
            .to_uppercase(),
        None => view
            .npub
            .strip_prefix("npub1")
            .unwrap_or(&view.npub)
            .chars()
            .take(2)
            .collect::<String>()
            .to_uppercase(),
    }
}

/// Item 1 of the money path: the panel pre-fills a stored wallet's address
/// on launch, but only the profile as the relays have it says whether the
/// payer can see that address. `Some` = the profile has no lud16, and the
/// text the panel shows next to "Retry publishing". A different lud16 is
/// `None`: it is never overwritten, so there is nothing to retry — the
/// panel names that address instead.
pub(crate) fn relay_address_gap(relay_lud16: Option<&str>) -> Option<String> {
    relay_lud16
        .is_none()
        .then(|| "The profile on the relays does not carry this address yet".to_string())
}

/// The two hue pairs from the mock, alternated down the rail by position.
pub(crate) fn account_hue(ix: usize) -> (u32, u32) {
    if ix.is_multiple_of(2) {
        (HUE_AK_FROM, HUE_AK_TO)
    } else {
        (HUE_BO_FROM, HUE_BO_TO)
    }
}

/// Which account to act as: `preferred` if it is still stored, else the
/// first stored one, else none (read-only).
pub(crate) fn pick_active(pubkeys: &[String], preferred: Option<&str>) -> Option<String> {
    preferred
        .filter(|p| pubkeys.iter().any(|stored| stored == p))
        .map(str::to_string)
        .or_else(|| pubkeys.first().cloned())
}

/// The list read sent on an unknown create outcome (its `seq`) is out
/// until a reply to it, or to a later read, lands. A reply to an earlier
/// read (a ⌘R sent mid-create) holds the pre-timeout list and unlocks
/// nothing.
fn still_checking_list(checking: Option<u64>, update: &Update) -> Option<u64> {
    let reply = match update {
        Update::Bounties { seq, .. } | Update::BountiesFailed { seq, .. } => *seq,
        _ => return checking,
    };
    checking.filter(|&wanted| reply < wanted)
}

/// A bounty form is locked while its request is out, or while the list
/// read after an unknown outcome is.
fn form_locked(submitting: bool, checking_list: Option<u64>) -> bool {
    submitting || checking_list.is_some()
}

/// Where the armed remove's balance read stands. The second click waits
/// while it is `Checking`, so a fast double-click cannot skip the warning.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BalanceCheck {
    /// The arming click's `FetchBalance`, by request id, is out.
    Checking(u64),
    Done,
    Failed,
}

impl BalanceCheck {
    /// The check for the `armed` account after `update`: only the reply to
    /// the arming click's own read ends the wait. A poll or a send's re-read
    /// sent before arming carries another id and changes nothing. After a
    /// failed read, any balance for the account (the 3 s poll) is news, so
    /// the sats warning can show.
    fn after(self, armed: &str, update: &Update) -> Self {
        match (self, update) {
            (Self::Checking(id), Update::Balance { pubkey, request, .. })
                if pubkey == armed && *request == Some(id) =>
            {
                Self::Done
            }
            (Self::Checking(id), Update::BalanceFailed { pubkey, request })
                if pubkey == armed && *request == Some(id) =>
            {
                Self::Failed
            }
            (Self::Failed, Update::Balance { pubkey, .. }) if pubkey == armed => Self::Done,
            _ => self,
        }
    }
}

/// What `apply` folds in before it handles `update`: the list check after
/// an unknown create outcome, and the armed remove's balance check.
fn fold_checks(
    checking_list: Option<&mut Option<u64>>,
    armed: Option<&str>,
    remove_check: &mut BalanceCheck,
    update: &Update,
) {
    if let Some(checking) = checking_list {
        *checking = still_checking_list(*checking, update);
    }
    if let Some(armed) = armed {
        *remove_check = remove_check.after(armed, update);
    }
}

/// The next bounty-list read, under a fresh sequence number.
fn next_bounties_read(seq: &mut u64) -> Command {
    *seq += 1;
    Command::FetchBounties { seq: *seq }
}

/// An unknown create outcome: lock the form on a fresh list read and
/// return that read.
fn on_outcome_unknown(checking_list: &mut Option<u64>, seq: &mut u64) -> Command {
    let read = next_bounties_read(seq);
    *checking_list = Some(*seq);
    read
}

/// What a click on "Remove this account" does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RemoveAction {
    /// First click: arm it and read the balance.
    Arm,
    /// Armed, but the balance read is still out: do nothing yet.
    Wait,
    /// Armed, but a submit is in flight for this account: refuse and disarm.
    Refuse,
    Remove,
}

/// One click on "Remove" for `pubkey`, over the shell's remove state.
/// Returns what the click did and the command to send, if any. `held`:
/// this is the active account and a form is in flight.
fn click_remove(
    confirm_remove: &mut Option<String>,
    check: &mut BalanceCheck,
    request: &mut u64,
    pubkey: String,
    has_token: bool,
    held: bool,
) -> (RemoveAction, Option<Command>) {
    let armed = confirm_remove.as_deref() == Some(pubkey.as_str());
    let action = remove_click(armed, *check, held);
    let command = match action {
        // The warning shows "Checking balance…" until the read lands.
        RemoveAction::Wait => None,
        // Refused, so disarmed: the next click arms it afresh.
        RemoveAction::Refuse => {
            *confirm_remove = None;
            None
        }
        RemoveAction::Remove => {
            *confirm_remove = None;
            Some(Command::RemoveAccount { pubkey })
        }
        // The armed card warns about sats still in the wallet; read the
        // balance fresh for it, under its own request id.
        RemoveAction::Arm => {
            let read = has_token.then(|| {
                *request += 1;
                Command::FetchBalance {
                    pubkey: pubkey.clone(),
                    request: Some(*request),
                }
            });
            *check = if read.is_some() {
                BalanceCheck::Checking(*request)
            } else {
                BalanceCheck::Done
            };
            *confirm_remove = Some(pubkey);
            read
        }
    };
    (action, command)
}

/// `held`: this is the active account and a form is in flight.
fn remove_click(armed: bool, check: BalanceCheck, held: bool) -> RemoveAction {
    if !armed {
        RemoveAction::Arm
    } else if matches!(check, BalanceCheck::Checking(_)) {
        RemoveAction::Wait
    } else if held {
        RemoveAction::Refuse
    } else {
        RemoveAction::Remove
    }
}

pub(crate) fn short_npub(npub: &str) -> String {
    if npub.chars().count() <= 18 {
        npub.to_string()
    } else {
        let head: String = npub.chars().take(12).collect();
        let tail: String = npub
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{head}…{tail}")
    }
}

pub(crate) fn short_id(id: &str) -> String {
    id.chars().take(8).collect::<String>() + "…"
}

/// The instance hides claims from keys it ranks below 2, so a new key's claim
/// is on the relay but invisible in the list. Shown in the claim form and in
/// the published-claim details.
pub(crate) const RANK_NOTE: &str = "This instance only lists claims from keys the \
    issuer's web of trust ranks 2 or higher. A new key has no rank yet, so \
    your claim can be on the relay and still not show here.";

/// The toast for a just-published claim: one short line, and the full text
/// behind a click when there is more to say.
struct ClaimNotice {
    title: &'static str,
    short: String,
    full: Option<String>,
}

/// `payout` is `(auto_pay, min_rank, amount_sats)` of the claimed bounty, or
/// `None` when no list we hold has it.
fn claim_notice(payout: Option<(bool, u64, u64)>) -> ClaimNotice {
    match payout {
        Some((true, min_rank, sats)) => {
            let sats = timefmt::fmt_sats(sats);
            ClaimNotice {
                title: "Claim published — auto-pay bounty",
                short: format!(
                    "With rank {min_rank} or higher, {sats} sats arrive within about a \
                     minute. Click for details."
                ),
                full: Some(format!(
                    "With rank {min_rank} or higher, {sats} sats arrive at your Lightning \
                     address within about a minute. Below that, the issuer reviews the \
                     claim by hand. {RANK_NOTE}"
                )),
            }
        }
        Some((false, _, sats)) => {
            let sats = timefmt::fmt_sats(sats);
            ClaimNotice {
                title: "Claim published — manual-pay bounty",
                short: format!(
                    "The issuer reviews your claim and pays {sats} sats by hand. Click for \
                     details."
                ),
                full: Some(format!(
                    "This bounty does not auto-pay: the issuer reviews your claim and \
                     sends the {sats} sats by hand. {RANK_NOTE}"
                )),
            }
        }
        None => ClaimNotice {
            title: "Claim published",
            short: "The issuer's relay has it.".to_string(),
            full: None,
        },
    }
}

/// One row of "Recent activity", written the moment its update arrives.
pub(crate) struct ActivityItem {
    pub dot: u32,
    pub text: String,
    /// Unix seconds. Server timestamps where the update carries one,
    /// otherwise the arrival time.
    pub at: u64,
}

// -------------------------------------------------------------------- forms

/// Label, placeholder, prefilled default — in Enter-advance order.
pub(crate) const BOUNTY_FIELD_DEFS: [(&str, &str, &str); 7] = [
    ("Item (singular)", "US City", ""),
    ("Items (plural)", "US Cities", ""),
    ("List description", "optional", ""),
    ("Bounty criteria", "What earns the reward", ""),
    ("Reward per item (sats)", "", "100"),
    ("Bounty cap (sats)", "", "400"),
    ("Auto-pay minimum rank", "", "2"),
];

const F_SINGULAR: usize = 0;
const F_PLURAL: usize = 1;
const F_DESCRIPTION: usize = 2;
const F_CRITERIA: usize = 3;
const F_AMOUNT: usize = 4;
const F_CAP: usize = 5;
const F_RANK: usize = 6;

/// The issuer's "New DList + bounty" form. Enter advances through the fields
/// and submits from the last one; the flow it starts is
/// PublishDList → DListPublished → CreateBounty → BountyCreated.
pub(crate) struct BountyForm {
    pub fields: Vec<Entity<InputState>>,
    pub error: Option<String>,
    pub submitting: bool,
    /// The create's outcome is unknown and a list read is out: the form
    /// stays locked until it lands, so the duplicate check can see the
    /// bounty the server may have made.
    pub checking_list: Option<u64>,
    _subs: Vec<Subscription>,
}

/// The claimant's one-field claim form for the selected bounty.
pub(crate) struct ClaimForm {
    pub bounty_id: String,
    pub coordinate: String,
    pub name: Entity<InputState>,
    pub error: Option<String>,
    pub submitting: bool,
    _sub: Subscription,
}

// -------------------------------------------------------------------- shell

pub struct Shell {
    pub(crate) screen: Screen,
    /// Every stored account, in rail (store) order.
    views: Vec<(String, AccountView)>,
    /// The account the forms act as. `None` is read-only mode: no account.
    pub(crate) active: Option<String>,
    pub(crate) relay_connected: bool,
    pub(crate) activity: Vec<ActivityItem>,
    pub(crate) bounties: Load<Vec<Bounty>>,
    /// The bounty the detail pane (and the auto-pay pill) is about.
    pub(crate) selected: Option<String>,
    /// Latest full snapshot per watched bounty, straight from the runtime.
    pub(crate) details: HashMap<String, BountyDetail>,
    /// A just-created bounty, rebuilt from the submitted form values, shown
    /// until the server list/detail catches up — so the detail pane never
    /// flashes "Loading bounty…" right after go-live.
    optimistic: Option<Bounty>,
    pub(crate) bounty_form: Option<BountyForm>,
    pub(crate) claim_form: Option<ClaimForm>,
    /// The account screen; rendered only while `screen` is AccountSetup.
    pub(crate) setup: AccountSetup,
    /// Set once the first `AccountsLoaded` routed the app, so a later one
    /// (⌘R, a remove) keeps the account on screen instead of re-reading the
    /// store's preference.
    first_screen_decided: bool,
    /// The bounty half of a submitted form, parked while the DList publishes.
    pending_bounty: Option<CreateBounty>,
    /// The request as it went to the server (coordinate filled in), kept so
    /// `BountyCreated` can build the optimistic card from real values.
    submitted_bounty: Option<CreateBounty>,
    /// Scrolls the bounty list; `scroll_to` is the id to bring into view once
    /// the list containing it arrives.
    pub(crate) bounty_list_scroll: ScrollHandle,
    scroll_to: Option<String>,
    commands: mpsc::UnboundedSender<Command>,
    chat: Entity<Chat>,
    search: Entity<InputState>,
    /// Keeps key dispatch anchored inside the shell on screens with no input
    /// of their own, so the cmd-N bindings always land.
    focus: FocusHandle,
    /// A gpui Task is cancel-on-drop; the update drain lives here.
    _updates: Task<()>,
    /// Re-renders every 30 s so the relative timestamps stay truthful.
    _clock: Task<()>,
    /// The 3-second balance poll for the wallet on screen (the ready panel's
    /// account, or ⌘6's active one); `None` when neither is showing.
    balance_poll: Option<(String, Task<()>)>,
    /// "Remove this account" is armed for this pubkey: the next click on it
    /// deletes the nsec and the Coinos password together, so one click is
    /// not enough. Navigation, an account switch, or an AccountsLoaded
    /// without that account disarms it; other runtime updates leave it alone.
    confirm_remove: Option<String>,
    /// The arming click's balance read for `confirm_remove`.
    remove_check: BalanceCheck,
    /// The last arming click's `FetchBalance` request id.
    balance_request: u64,
    /// The last `FetchBounties` sequence number sent.
    bounties_seq: u64,
    /// The Wallet screen's recipient and amount; cleared on an account switch.
    send_inputs: SendInputs,
    /// Per-account NIP-85 state (Treasure Map, assertions, follows), keyed by
    /// pubkey. Each entry carries the fetch generation it answers to.
    pub(crate) trust: HashMap<String, TrustState>,
    /// The Account page's own state — which account, which tab — while
    /// `screen` is `Screen::Account`.
    pub(crate) account_page: Option<AccountPage>,
    /// Kept alive so re-activating the window can re-fetch a stale Trust tab.
    _activation: Subscription,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Chat::new focuses its composer, but the window opens on the
        // Dashboard, where that composer is not rendered. A focus on an
        // unrendered node gives keystrokes no dispatch path and the cmd-N
        // bindings are dead on arrival — so the shell takes the focus back.
        let chat = cx.new(|cx| Chat::new(window, cx));
        let focus = cx.focus_handle();
        focus.focus(window, cx);

        // The runtime spawns its own tokio thread and returns instantly;
        // nothing tokio-dependent is ever awaited on the gpui executor.
        let handle = nostr::spawn_runtime();
        let commands = handle.commands;
        for command in [
            Command::Connect,
            Command::LoadAccounts,
            Command::FetchBounties { seq: 0 },
        ] {
            let _ = commands.unbounded_send(command);
        }

        let mut updates = handle.updates;
        let _updates = cx.spawn_in(window, async move |this, cx| {
            // futures channels are executor-agnostic — awaiting the receiver
            // here, on the gpui executor, is the supported bridge.
            while let Some(update) = updates.next().await {
                let alive = this.update_in(cx, |shell, window, cx| {
                    shell.apply(update, window, cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let _clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        let setup = AccountSetup::new(true, window, cx);
        let send_inputs = SendInputs::new(window, cx);

        // Coming back to the window re-reads the Trust tab on screen — but
        // only when its last read is more than 30 s old.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                return;
            }
            let Some(pubkey) = this.account_page.as_ref().map(|page| page.pubkey.clone())
            else {
                return;
            };
            let stale = this
                .trust
                .get(&pubkey)
                .map(|state| {
                    state.requested_at == 0
                        || timefmt::now_unix().saturating_sub(state.requested_at) > 30
                })
                .unwrap_or(true);
            if stale {
                this.fetch_trust(&pubkey);
                cx.notify();
            }
        });

        Self {
            screen: Screen::Dashboard,
            views: Vec::new(),
            active: None,
            relay_connected: false,
            activity: Vec::new(),
            bounties: Load::Loading,
            selected: None,
            details: HashMap::new(),
            optimistic: None,
            bounty_form: None,
            claim_form: None,
            setup,
            first_screen_decided: false,
            pending_bounty: None,
            submitted_bounty: None,
            bounty_list_scroll: ScrollHandle::new(),
            scroll_to: None,
            commands,
            chat,
            search: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search lists, bounties, profiles…")
            }),
            focus,
            _updates,
            _clock,
            balance_poll: None,
            confirm_remove: None,
            remove_check: BalanceCheck::Done,
            balance_request: 0,
            bounties_seq: 0,
            send_inputs,
            trust: HashMap::new(),
            account_page: None,
            _activation: activation,
        }
    }

    pub(crate) fn view(&self, pubkey: &str) -> Option<&AccountView> {
        self.views.iter().find(|(p, _)| p == pubkey).map(|(_, v)| v)
    }

    fn view_mut(&mut self, pubkey: &str) -> Option<&mut AccountView> {
        self.views.iter_mut().find(|(p, _)| p == pubkey).map(|(_, v)| v)
    }

    pub(crate) fn active_view(&self) -> Option<&AccountView> {
        self.view(self.active.as_deref()?)
    }

    pub(crate) fn views(&self) -> impl Iterator<Item = &AccountView> {
        self.views.iter().map(|(_, v)| v)
    }

    /// The active account IS the issuer: ⌘N opens the bounty form, not a
    /// claim.
    pub(crate) fn is_active_issuer(&self) -> bool {
        self.active.as_deref().is_some_and(nostr::is_issuer)
    }

    /// The feed's name for an account: profile name, else short npub.
    fn label(&self, pubkey: &str) -> String {
        self.view(pubkey)
            .map(display_name)
            .unwrap_or_else(|| short_id(pubkey))
    }

    /// The bounty by id, from the freshest source that has it: the detail
    /// snapshot, then the list, then the optimistic copy. Server data always
    /// wins; the optimistic copy only fills the gap between BountyCreated and
    /// the first list/detail that includes it.
    fn bounty(&self, id: &str) -> Option<&Bounty> {
        if let Some(detail) = self.details.get(id) {
            return Some(&detail.bounty);
        }
        if let Load::Ready(list) = &self.bounties
            && let Some(bounty) = list.iter().find(|b| b.id == id)
        {
            return Some(bounty);
        }
        self.optimistic.as_ref().filter(|b| b.id == id)
    }

    pub(crate) fn selected_bounty(&self) -> Option<&Bounty> {
        self.bounty(self.selected.as_deref()?)
    }

    /// True while the selected bounty renders from the optimistic copy — the
    /// detail pane says "syncing" instead of pretending claims were checked.
    pub(crate) fn selected_is_optimistic(&self) -> bool {
        match (&self.selected, &self.optimistic) {
            (Some(id), Some(bounty)) => {
                bounty.id == *id && !self.details.contains_key(id)
            }
            _ => false,
        }
    }

    fn push_activity(&mut self, dot: u32, text: String, at: u64) {
        self.activity.insert(0, ActivityItem { dot, text, at });
        self.activity.truncate(200);
    }

    fn claim_name(&self, bounty_id: &str, claim_event_id: &str) -> String {
        self.details
            .get(bounty_id)
            .and_then(|detail| {
                detail
                    .claims
                    .iter()
                    .find(|claim| claim.event.id == claim_event_id)
            })
            .map(|claim| claim.item_name().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| short_id(claim_event_id))
    }

    // ------------------------------------------------------- runtime updates

    /// Folds one runtime update into the state every screen renders from.
    fn apply(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        let now = timefmt::now_unix();
        // The "copied" tick lives until the next update lands.
        if let Some(page) = &mut self.account_page {
            page.copied = None;
        }
        fold_checks(
            self.bounty_form.as_mut().map(|form| &mut form.checking_list),
            self.confirm_remove.as_deref(),
            &mut self.remove_check,
            &update,
        );
        match update {
            Update::RelayStatus(connected) => {
                self.relay_connected = connected;
            }
            Update::AccountsLoaded { accounts, active } => {
                // Rebuild in store order, keeping what is already known
                // (profile, wallet, balance) for accounts that stayed.
                let mut old = std::mem::take(&mut self.views);
                let had_accounts = !old.is_empty();
                self.views = accounts
                    .into_iter()
                    .map(|info| {
                        let mut view = match old.iter().position(|(p, _)| *p == info.pubkey) {
                            Some(ix) => old.swap_remove(ix).1,
                            None => AccountView::new(info.pubkey.clone(), info.npub.clone()),
                        };
                        // A stored login WITHOUT a token was saved before a
                        // register that never answered: the wallet may not
                        // exist, so it stays "no address" until a retry
                        // settles it.
                        if let Some(wallet) = info.wallet.filter(|w| w.has_token) {
                            view.wallet.has_token = true;
                            if view.wallet.created.is_none() {
                                view.wallet.created = Some(WalletCreated {
                                    username: wallet.username,
                                    lightning_address: wallet.lightning_address,
                                    publish_error: None,
                                });
                            }
                            view.sync_qr();
                        }
                        (info.pubkey, view)
                    })
                    .collect();
                let pubkeys: Vec<String> = self.views.iter().map(|(p, _)| p.clone()).collect();
                // The store's preference routes the first load; after that
                // the account on screen stays unless it is gone.
                let preferred = if self.first_screen_decided {
                    self.active.clone().or(active)
                } else {
                    active
                };
                let was_active = self.active.clone();
                self.active = pick_active(&pubkeys, preferred.as_deref());
                if self.active.is_none() {
                    self.close_forms(window, cx);
                }
                if self.active != was_active {
                    self.send_inputs.clear(window, cx);
                }
                let first_load = !self.first_screen_decided;
                self.first_screen_decided = true;
                // Trust state follows the account set: removed accounts lose
                // their entries, and everyone gets a fresh read.
                self.trust.retain(|p, _| pubkeys.contains(p));
                // Disarm only when the armed account is gone. Trust parts
                // land mid-confirm and must not reset the second click.
                if self
                    .confirm_remove
                    .as_ref()
                    .is_some_and(|p| !pubkeys.contains(p))
                {
                    self.confirm_remove = None;
                }
                // A removed account's page shows "removed", not its key.
                if let Some(page) = &mut self.account_page
                    && !pubkeys.contains(&page.pubkey)
                {
                    page.hide_nsec();
                }
                self.fetch_trust_all();
                // No account: first launch, or the last one was just removed.
                // Not a ⌘R while already browsing read-only — that would
                // bounce the person back to the account screen they left.
                if self.views.is_empty() && (first_load || had_accounts) {
                    self.open_account_setup(true, window, cx);
                }
            }
            Update::ProfileLoaded {
                pubkey,
                name,
                picture,
                lud16,
            } => {
                if let Some(view) = self.view_mut(&pubkey) {
                    view.kind0_name = name;
                    view.picture = picture;
                    view.lud16 = lud16;
                    view.profile = ProfileRead::Read;
                    // A wallet pre-filled from the store on launch claimed
                    // nothing about the relays; the profile read settles
                    // whether "Retry publishing" is due.
                    if let Some(created) = &mut view.wallet.created {
                        created.publish_error =
                            relay_address_gap(view.lud16.as_deref());
                    }
                    view.sync_qr();
                }
            }
            Update::ProfileUnavailable { pubkey } => {
                if let Some(view) = self.view_mut(&pubkey) {
                    view.profile = ProfileRead::Failed;
                }
            }
            Update::AddAccountFailed { message } => {
                // `message` is fixed text from the runtime; it never echoes
                // what was pasted.
                if self.screen == Screen::AccountSetup {
                    self.setup.working = None;
                    self.setup.awaiting_wallet = false;
                    self.setup.error = Some(message);
                } else {
                    self.push_activity(RED, message, now);
                }
            }
            Update::AccountAdded {
                pubkey,
                npub,
                profile_checked,
                profile_found,
            } => {
                self.push_activity(GREEN, format!("Added account {}", short_npub(&npub)), now);
                if self.view(&pubkey).is_none() {
                    self.views
                        .push((pubkey.clone(), AccountView::new(pubkey.clone(), npub.clone())));
                }
                if let Some(view) = self.view_mut(&pubkey) {
                    view.profile = if profile_checked {
                        ProfileRead::Read
                    } else {
                        ProfileRead::Failed
                    };
                }
                // A new account's trust read starts the moment it lands.
                self.fetch_trust(&pubkey);
                // Read-only mode ends with the first account.
                if self.active.is_none() {
                    self.active = Some(pubkey.clone());
                }
                // The create path continues straight into the wallet: the
                // view exists now, so its panel can show the spinner.
                if self.setup.awaiting_wallet
                    && let Some(view) = self.view_mut(&pubkey)
                {
                    view.wallet.submitting = true;
                    view.wallet.error = None;
                }
                if self.screen == Screen::AccountSetup {
                    self.setup.error = None;
                    self.setup.working = self
                        .setup
                        .awaiting_wallet
                        .then_some("Opening your Coinos wallet…");
                    self.setup.ready = Some(ReadySummary {
                        pubkey,
                        npub,
                        profile_checked,
                        profile_found,
                    });
                    // The key is in the store; the input has no reason to
                    // keep holding it.
                    self.setup
                        .key
                        .update(cx, |state, cx| state.set_value("", window, cx));
                    // Enter now means "Start" — the Confirm binding needs the
                    // shell to hold the focus.
                    self.focus.focus(window, cx);
                }
            }
            Update::WalletCreated { pubkey, created } => {
                // One feed line per event; the panel shows the same error
                // next to its retry button.
                let (dot, suffix) = match &created.publish_error {
                    None => (GREEN, String::new()),
                    Some(error) => (AMBER, format!(" — {error}")),
                };
                self.push_activity(
                    dot,
                    format!("Coinos wallet ready: {}{suffix}", created.lightning_address),
                    now,
                );
                if let Some(view) = self.view_mut(&pubkey) {
                    view.wallet.submitting = false;
                    view.wallet.error = None;
                    view.wallet.has_token = true;
                    view.wallet.created = Some(created);
                    view.sync_qr();
                }
                self.finish_setup_wallet(&pubkey);
            }
            Update::WalletFailed { pubkey, message } => {
                if let Some(view) = self.view_mut(&pubkey) {
                    view.wallet.submitting = false;
                    view.wallet.error = Some(message.clone());
                }
                let who = self.label(&pubkey);
                self.push_activity(RED, format!("Coinos wallet for {who} failed: {message}"), now);
                self.finish_setup_wallet(&pubkey);
            }
            Update::Balance { pubkey, sats, .. } => {
                // A number that just flips is easy to miss: a change gets a
                // fading tag beside the balance, and a toast with the math
                // — one per send (keyed by the pending `Sent`), or one per
                // arrival while this wallet is on screen.
                let is_active = self.active.as_deref() == Some(pubkey.as_str());
                let on_screen = self.wallet_on_screen(&pubkey);
                let Some(view) = self.view_mut(&pubkey) else {
                    return self.finish_apply(cx);
                };
                let changed = view.wallet.record_balance(sats);
                let pending = view.wallet.pending_sent.take();
                let fmt = timefmt::fmt_sats;
                let toast = match (pending, changed.as_ref()) {
                    (Some(sent), _) if is_active => {
                        let math = match sent.before {
                            Some(before) => format!("{} → {} sats", fmt(before), fmt(sats)),
                            None => format!("{} sats", fmt(sats)),
                        };
                        Some((
                            format!("Sent {} sats", fmt(sent.sats)),
                            format!("{math} · to {}", sent.to),
                        ))
                    }
                    (Some(_), _) => None,
                    (None, Some(delta)) if delta.current > delta.previous && on_screen => Some((
                        format!("Received {} sats", fmt(delta.current - delta.previous)),
                        format!("{} → {} sats", fmt(delta.previous), fmt(delta.current)),
                    )),
                    (None, _) => None,
                };
                if changed.is_some() {
                    let cleared = pubkey.clone();
                    let clear = cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(wallet::DELTA_FADE).await;
                        let _ = this.update(cx, |shell, cx| {
                            if let Some(view) = shell.view_mut(&cleared) {
                                view.wallet.delta = None;
                            }
                            cx.notify();
                        });
                    });
                    if let Some(view) = self.view_mut(&pubkey) {
                        view.wallet.delta_clear = Some(clear);
                    }
                }
                if let Some((title, body)) = toast {
                    window.push_notification(Notification::info(body).title(title), cx);
                }
            }
            // Only the armed remove waits on it; folded in above.
            Update::BalanceFailed { .. } => {}
            Update::Sent { pubkey, to, sats } => {
                let who = self.label(&pubkey);
                self.push_activity(
                    GREEN,
                    format!("{who} sent {} sats to {to}", timefmt::fmt_sats(sats)),
                    now,
                );
                if self.active.as_deref() == Some(pubkey.as_str()) {
                    self.send_inputs.clear(window, cx);
                }
                if let Some(view) = self.view_mut(&pubkey) {
                    view.wallet.pending_sent = Some(PendingSent {
                        to: to.clone(),
                        sats,
                        before: view.wallet.balance,
                    });
                    view.wallet.send = Some(SendState::Sent { to, sats });
                }
            }
            Update::SendFailed {
                pubkey,
                message,
                may_have_paid,
            } => {
                let who = self.label(&pubkey);
                self.push_activity(RED, format!("Send from {who} failed: {message}"), now);
                // The sats may be gone: a reflex Enter on the same amount
                // must not pay twice, so the amount goes, the recipient stays.
                if may_have_paid && self.active.as_deref() == Some(pubkey.as_str()) {
                    self.send_inputs
                        .sats
                        .update(cx, |state, cx| state.set_value("", window, cx));
                }
                if let Some(view) = self.view_mut(&pubkey) {
                    view.wallet.send = Some(SendState::Failed(message));
                }
            }
            Update::Bounties { list, .. } => {
                if self.selected.is_none()
                    && let Some(first) = list.first() {
                        self.select(first.id.clone());
                    }
                // The server list now carries the just-created bounty: the
                // optimistic copy has done its job.
                if let Some(bounty) = &self.optimistic
                    && list.iter().any(|b| b.id == bounty.id) {
                        self.optimistic = None;
                    }
                // A bounty created this session scrolls into view as soon as
                // it exists in the list.
                if let Some(target) = self.scroll_to.take() {
                    match list.iter().position(|b| b.id == target) {
                        Some(ix) => self.bounty_list_scroll.scroll_to_item(ix),
                        None => self.scroll_to = Some(target),
                    }
                }
                self.bounties = Load::Ready(list);
            }
            Update::BountiesFailed { message, .. } => {
                // Keep data already on screen; only a first load fails visibly.
                if !matches!(self.bounties, Load::Ready(_)) {
                    self.bounties = Load::Failed(message.clone());
                }
                self.push_activity(RED, format!("Bounty list failed: {message}"), now);
            }
            Update::BountyDetail(detail) => {
                if self.optimistic.as_ref().is_some_and(|b| b.id == detail.bounty.id) {
                    self.optimistic = None;
                }
                self.details.insert(detail.bounty.id.clone(), *detail);
            }
            Update::DListPublished { pubkey, coordinate } => {
                self.push_activity(
                    ACCENT_LIGHT,
                    format!("Published list {coordinate}"),
                    now,
                );
                if let Some(mut req) = self.pending_bounty.take() {
                    req.list_coordinate = coordinate;
                    // Kept so BountyCreated can build the optimistic card
                    // from the exact values the server was sent.
                    self.submitted_bounty = Some(req.clone());
                    let _ = self
                        .commands
                        .unbounded_send(Command::CreateBounty { pubkey, req });
                }
            }
            Update::BountyCreated { id } => {
                self.push_activity(
                    ACCENT_LIGHT,
                    format!("Bounty {} created — watching for claims", short_id(&id)),
                    now,
                );
                self.bounty_form = None;
                self.pending_bounty = None;
                self.focus.focus(window, cx);
                self.selected = Some(id.clone());
                // The detail pane shows the submitted values immediately; the
                // watch's first snapshot (or the refreshed list) replaces them.
                if let Some(req) = self.submitted_bounty.take() {
                    self.optimistic = Some(optimistic_bounty(&id, req, now));
                }
                self.scroll_to = Some(id.clone());
                let _ = self.commands.unbounded_send(Command::WatchBounty { id });
                self.fetch_bounties();
            }
            Update::ClaimPublished {
                bounty_id,
                event_id,
            } => {
                self.push_activity(
                    GREEN,
                    format!("Claim {} submitted", short_id(&event_id)),
                    now,
                );
                self.claim_form = None;
                self.focus.focus(window, cx);
                // Say how this bounty pays: the claim card alone reads as
                // "nothing happened" when the payout is manual or gated. The
                // toast stays one line and autohides; the full text is an
                // alert dialog behind a click, so it never covers the card.
                let notice = claim_notice(
                    self.bounty(&bounty_id)
                        .map(|b| (b.auto_pay_on(), b.auto_pay_min_rank.unwrap_or(0), b.amount_sats)),
                );
                let mut toast = Notification::info(notice.short).title(notice.title);
                if let Some(full) = notice.full {
                    let title = SharedString::from(notice.title);
                    let full = SharedString::from(full);
                    toast = toast.on_click(move |_, window, cx| {
                        let (title, full) = (title.clone(), full.clone());
                        window.open_alert_dialog(cx, move |alert, _, _| {
                            alert.title(title.clone()).description(full.clone())
                        });
                    });
                }
                window.push_notification(toast, cx);
                // Watch the bounty the claim went to — the form snapshots it
                // at open, so this is right even if selection moved meanwhile.
                let _ = self
                    .commands
                    .unbounded_send(Command::WatchBounty { id: bounty_id });
            }
            Update::PaymentState {
                bounty_id,
                claim_event_id,
                state,
                reason,
                ts,
            } => {
                let name = self.claim_name(&bounty_id, &claim_event_id);
                let why = reason.map(|r| format!(" ({r})")).unwrap_or_default();
                let (dot, text) = match state.as_str() {
                    "attempting" => (ACCENT, format!("Auto-pay started for \"{name}\"")),
                    "paid" => (
                        AMBER,
                        format!("Paid \"{name}\" — waiting for the zap receipt"),
                    ),
                    "settled" => (GREEN, format!("Payment for \"{name}\" settled")),
                    "paid_unreceipted" => (
                        AMBER,
                        format!("Paid \"{name}\", but no receipt arrived{why}"),
                    ),
                    "failed" => (RED, format!("Auto-pay for \"{name}\" failed{why}")),
                    other => (TEXT_MUTED, format!("\"{name}\" payment state: {other}{why}")),
                };
                self.push_activity(dot, text, if ts > 0 { ts } else { now });
            }
            Update::ReceiptSeen {
                bounty_id: _,
                receipt_id,
                secs_from_claim,
                at,
            } => {
                self.push_activity(
                    GREEN,
                    format!(
                        "Zap receipt {} landed {secs_from_claim} s after the claim",
                        short_id(&receipt_id)
                    ),
                    // The receipt's own timestamp: a receipt learned late must
                    // never read as an instant payout.
                    if at > 0 { at } else { now },
                );
            }
            Update::Error { source, message } => {
                sentry::capture_message(&format!("{source:?}: {message}"), sentry::Level::Error);
                // An error fails a form only when it came from that form's own
                // command — everything else (watch polls, account refreshes)
                // is feed-only noise as far as the forms are concerned.
                match source {
                    ErrorSource::DList
                    | ErrorSource::Bounty
                    | ErrorSource::BountyOutcomeUnknown => {
                        if let Some(form) = &mut self.bounty_form
                            && form.submitting {
                                form.submitting = false;
                                form.error = Some(message.clone());
                                self.pending_bounty = None;
                                self.submitted_bounty = None;
                                if source == ErrorSource::BountyOutcomeUnknown {
                                    // A timeout or dropped reply says nothing
                                    // about whether the server created it.
                                    form.error = Some(format!(
                                        "{message} The server may have created this bounty; check the list before retrying."
                                    ));
                                    let read = on_outcome_unknown(
                                        &mut form.checking_list,
                                        &mut self.bounties_seq,
                                    );
                                    let _ = self.commands.unbounded_send(read);
                                }
                            }
                    }
                    ErrorSource::Claim => {
                        if let Some(form) = &mut self.claim_form
                            && form.submitting {
                                form.submitting = false;
                                form.error = Some(message.clone());
                            }
                    }
                    ErrorSource::Runtime
                    | ErrorSource::Accounts
                    | ErrorSource::Watch
                    | ErrorSource::Wallet => {}
                }
                self.push_activity(RED, message, now);
            }
            Update::Trust {
                pubkey,
                generation,
                part,
            } => {
                // Stale generations and updates for accounts no longer in
                // the store are dropped, never folded in.
                if self.view(&pubkey).is_some()
                    && let Some(state) = self.trust.get_mut(&pubkey)
                    && state.accepts(generation)
                {
                    state.apply(part, now);
                }
            }
        }
        self.finish_apply(cx);
    }

    /// The tail of every `apply`: the poll follows the wallet on screen, and
    /// the screen re-renders.
    fn finish_apply(&mut self, cx: &mut Context<Self>) {
        self.sync_balance_poll(cx);
        cx.notify();
    }

    /// Is this account's wallet panel what the person is looking at? The
    /// same rule `sync_balance_poll` uses: ⌘6's active account, or the
    /// account screen's ready panel.
    fn wallet_on_screen(&self, pubkey: &str) -> bool {
        match self.screen {
            Screen::Wallet => self.active.as_deref() == Some(pubkey),
            Screen::AccountSetup => self.setup.ready.as_ref().is_some_and(|r| r.pubkey == pubkey),
            _ => false,
        }
    }

    // ------------------------------------------------------------ navigation

    /// Switches the main pane. The chat takes the caret with it, so a click on
    /// "Chat" leaves the window typeable.
    pub(crate) fn go(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = screen;
        self.confirm_remove = None;
        if screen != Screen::Account {
            self.hide_nsec();
        }
        if screen == Screen::Chat {
            self.chat.focus_handle(cx).focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        self.sync_balance_poll(cx);
        cx.notify();
    }

    pub(crate) fn set_account(&mut self, pubkey: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.view(pubkey).is_none() {
            return;
        }
        if self.active.as_deref() != Some(pubkey) {
            // The form's reply would land under the other account, and a
            // retry would go out as it.
            if self.hold_for_submit() {
                cx.notify();
                return;
            }
            self.active = Some(pubkey.to_string());
            self.confirm_remove = None;
            // An open form belongs to the account that opened it.
            self.close_forms(window, cx);
            self.send_inputs.clear(window, cx);
            let _ = self.commands.unbounded_send(Command::SetActive {
                pubkey: pubkey.to_string(),
            });
        }
        self.sync_balance_poll(cx);
        cx.notify();
    }

    /// ⌘[ / ⌘]: the previous / next account in rail order, wrapping. A
    /// no-op with fewer than two.
    fn step_account(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.views.len();
        if count < 2 {
            return;
        }
        let current = self
            .active
            .as_deref()
            .and_then(|active| self.views.iter().position(|(p, _)| p == active))
            .unwrap_or(0) as isize;
        let next = (current + step).rem_euclid(count as isize) as usize;
        let pubkey = self.views[next].0.clone();
        self.set_account(&pubkey, window, cx);
    }

    // -------------------------------------------------------- trust reads

    /// Begin a fresh trust read for one account: bump its generation, mark
    /// every part pending (the prior answers stay for rendering), and queue
    /// the runtime command.
    pub(crate) fn fetch_trust(&mut self, pubkey: &str) {
        if self.view(pubkey).is_none() {
            return;
        }
        let generation = self
            .trust
            .get(pubkey)
            .map(|state| state.generation + 1)
            .unwrap_or(1);
        let entry = self.trust.entry(pubkey.to_string()).or_default();
        entry.begin(generation, timefmt::now_unix());
        let _ = self.commands.unbounded_send(Command::FetchTrust {
            pubkey: pubkey.to_string(),
            generation,
        });
    }

    /// First load / ⌘R: re-read every stored account's trust state.
    fn fetch_trust_all(&mut self) {
        let pubkeys: Vec<String> = self.views.iter().map(|(p, _)| p.clone()).collect();
        for pubkey in pubkeys {
            self.fetch_trust(&pubkey);
        }
    }

    /// The Account page for `pubkey` on `tab` — the destination of the
    /// dashboard's Fix rows, Accounts › Manage, and the sidebar chip. A page
    /// whose read has never run (or ran >30 s ago) kicks one off.
    pub(crate) fn open_account(
        &mut self,
        pubkey: &str,
        tab: Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.view(pubkey).is_none() {
            return;
        }
        let stale = self
            .trust
            .get(pubkey)
            .map(|state| {
                state.requested_at == 0
                    || timefmt::now_unix().saturating_sub(state.requested_at) > 30
            })
            .unwrap_or(true);
        if stale {
            self.fetch_trust(pubkey);
        }
        self.account_page = Some(AccountPage::new(pubkey, tab));
        self.go(Screen::Account, window, cx);
    }

    // ------------------------------------------------------ account screen

    /// Both doors in: first launch (no account stored) and the rail's "+".
    /// A fresh screen every time, so nothing typed last time lingers.
    pub(crate) fn open_account_setup(
        &mut self,
        first_launch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Leaving the setup screen can switch accounts; see set_account.
        if self.hold_for_submit() {
            cx.notify();
            return;
        }
        self.close_forms(window, cx);
        self.setup = AccountSetup::new(first_launch, window, cx);
        self.screen = Screen::AccountSetup;
        self.hide_nsec();
        self.focus.focus(window, cx);
        self.sync_balance_poll(cx);
        cx.notify();
    }

    pub(crate) fn setup_choose(
        &mut self,
        path: SetupPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.setup.path = path;
        self.setup.error = None;
        match path {
            SetupPath::Create => self
                .setup
                .name
                .update(cx, |state, cx| state.focus(window, cx)),
            SetupPath::Paste => self
                .setup
                .key
                .update(cx, |state, cx| state.focus(window, cx)),
            SetupPath::Choose => self.focus.focus(window, cx),
        }
        cx.notify();
    }

    /// Enter inside a setup input: submit the open path, or — once the add
    /// came back — start the app.
    pub(crate) fn setup_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Mid-add (the create path sets `ready` at AccountAdded while the
        // wallet still opens), Enter would skip the ready panel and the QR.
        if self.setup.working.is_some() {
            return;
        }
        if self.setup.ready.is_some() {
            self.leave_account_setup(window, cx);
            return;
        }
        match self.setup.path {
            SetupPath::Create => self.setup_submit_create(cx),
            SetupPath::Paste => self.setup_submit_paste(cx),
            SetupPath::Choose => {}
        }
    }

    /// "Create": a fresh key generated here, run through the same add path
    /// as a paste, with the wallet opened in the same breath. The nsec exists
    /// on this thread only inside `Secret`.
    pub(crate) fn setup_submit_create(&mut self, cx: &mut Context<Self>) {
        if self.setup.working.is_some() || self.setup.ready.is_some() {
            return;
        }
        let keys = nostr_sdk::prelude::Keys::generate();
        let nsec = match nostr_sdk::prelude::ToBech32::to_bech32(keys.secret_key()) {
            Ok(nsec) => nsec,
            Err(e) => {
                self.setup.error = Some(format!("Could not create a key: {e}"));
                cx.notify();
                return;
            }
        };
        let name = self.setup.name.read(cx).value().trim().to_string();
        self.setup.error = None;
        self.setup.generated = true;
        self.setup.awaiting_wallet = true;
        self.setup.working = Some("Creating your key…");
        let _ = self.commands.unbounded_send(Command::AddAccount {
            secret: Secret::new(nsec),
            name: Some(name).filter(|n| !n.is_empty()),
            open_wallet: true,
        });
        cx.notify();
    }

    fn setup_submit_paste(&mut self, cx: &mut Context<Self>) {
        if self.setup.working.is_some() || self.setup.ready.is_some() {
            return;
        }
        // Wrapped immediately: the pasted key exists on this thread only
        // inside `Secret`, which redacts itself everywhere.
        let secret = Secret::new(self.setup.key.read(cx).value().trim().to_string());
        if secret.expose().is_empty() {
            self.setup.error = Some("Paste a key first.".into());
            cx.notify();
            return;
        }
        self.setup.error = None;
        self.setup.generated = false;
        self.setup.awaiting_wallet = false;
        self.setup.working = Some("Checking the key…");
        let _ = self.commands.unbounded_send(Command::AddAccount {
            secret,
            name: None,
            open_wallet: false,
        });
        cx.notify();
    }

    /// The create path's second step is over (wallet opened, or not): the
    /// ready panel takes over and shows whichever it was.
    fn finish_setup_wallet(&mut self, pubkey: &str) {
        if self.setup.awaiting_wallet
            && self.setup.ready.as_ref().is_some_and(|r| r.pubkey == pubkey)
        {
            self.setup.awaiting_wallet = false;
            self.setup.working = None;
        }
    }

    /// Every door out of the account screen: Start, "just look around",
    /// Cancel, esc. A finished add becomes the active account; otherwise
    /// whatever was active stays (or nothing — read-only).
    pub(crate) fn leave_account_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::AccountSetup {
            return;
        }
        // The input may still hold a pasted key: esc with text in the field,
        // or esc while an add is in flight. `set_value` also clears the
        // input's undo history, so nothing keeps the secret alive after this.
        self.setup
            .key
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.setup.error = None;
        if let Some(ready) = self.setup.ready.take() {
            self.set_account(&ready.pubkey, window, cx);
        }
        self.go(Screen::Dashboard, window, cx);
    }

    /// The global Enter binding: only the account screen's Start listens.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen == Screen::AccountSetup && self.setup.ready.is_some() {
            self.leave_account_setup(window, cx);
        }
    }

    /// esc / ⌘.: on the account screen this leaves it; everywhere else it
    /// cancels whichever form is open.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen == Screen::AccountSetup {
            // Mid-add, esc would skip the ready panel — and with it the QR
            // the create path exists to show. Wait for the runtime.
            if self.setup.working.is_some() {
                return;
            }
            self.leave_account_setup(window, cx);
        } else {
            self.close_forms(window, cx);
        }
    }

    // --------------------------------------------------------------- wallet

    /// "Create a Coinos wallet" and "Retry publishing": one command. The
    /// runtime reuses a login already stored for the account, so pressing
    /// it twice never opens two wallets.
    pub(crate) fn create_wallet(&mut self, pubkey: String, cx: &mut Context<Self>) {
        let Some(view) = self.view_mut(&pubkey) else {
            return;
        };
        if view.wallet.submitting {
            return;
        }
        view.wallet.submitting = true;
        view.wallet.error = None;
        let _ = self
            .commands
            .unbounded_send(Command::CreateCoinosWallet { pubkey });
        cx.notify();
    }

    /// "Send" on the Wallet screen, and Enter in either of its fields: the
    /// active account pays what the inputs say. The obvious mistakes (no
    /// amount, not an address) are caught here; everything else comes back
    /// as `Sent` or `SendFailed` for this pubkey. Inert while a send is in
    /// flight, so a second Enter cannot pay twice.
    pub(crate) fn send_sats(&mut self, cx: &mut Context<Self>) {
        let Some(pubkey) = self.active.clone() else {
            return;
        };
        let to = self.send_inputs.to.read(cx).value().trim().to_string();
        let amount = self.send_inputs.sats.read(cx).value().trim().to_string();
        let Some(view) = self.view_mut(&pubkey) else {
            return;
        };
        if !view.wallet.has_token || view.wallet.send == Some(SendState::Sending) {
            return;
        }
        let checked = match amount.replace(',', "").parse::<u64>() {
            Ok(sats) if sats > 0 => {
                if coinos::split_address(&to).is_some() {
                    Ok(sats)
                } else {
                    Err("Enter a Lightning address like name@domain.com")
                }
            }
            _ => Err("Enter a whole number of sats"),
        };
        view.wallet.send = Some(match checked {
            Ok(_) => SendState::Sending,
            Err(message) => SendState::Failed(message.into()),
        });
        if let Ok(sats) = checked {
            let _ = self
                .commands
                .unbounded_send(Command::Send { pubkey, to, sats });
        }
        cx.notify();
    }

    /// "Copy password": from the store straight to the clipboard. It is
    /// never rendered and never logged — only a store failure reaches the
    /// feed, and that message carries a path, not a secret.
    pub(crate) fn copy_coinos_password(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        match secrets::load_coinos_login(pubkey) {
            Ok(Some(login)) => copy_secret(login.password, cx),
            Ok(None) => self.push_activity(
                AMBER,
                "No Coinos login is stored for this account".into(),
                timefmt::now_unix(),
            ),
            Err(e) => self.push_activity(RED, e.to_string(), timefmt::now_unix()),
        }
        cx.notify();
    }

    /// "Copy" on the Identity tab's nsec row: the same path as
    /// [`Self::copy_coinos_password`] — store to clipboard, nothing kept,
    /// nothing logged. Works with the key masked.
    pub(crate) fn copy_nsec(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        match secrets::nsec(pubkey) {
            Ok(Some(nsec)) => {
                copy_secret(nsec, cx);
                if let Some(page) = &mut self.account_page {
                    page.copied = Some("nsec".into());
                }
            }
            Ok(None) => self.push_activity(
                AMBER,
                "No secret key is stored for this account".into(),
                timefmt::now_unix(),
            ),
            Err(e) => self.push_activity(RED, e.to_string(), timefmt::now_unix()),
        }
        cx.notify();
    }

    /// "Reveal": read the nsec fresh into the page for
    /// [`account::NSEC_SHOWN_FOR`], re-rendering every second for the
    /// countdown. A second Reveal restarts the clock (the old task drops).
    pub(crate) fn reveal_nsec(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let nsec = match secrets::nsec(pubkey) {
            Ok(Some(nsec)) => nsec,
            Ok(None) => {
                self.push_activity(
                    AMBER,
                    "No secret key is stored for this account".into(),
                    timefmt::now_unix(),
                );
                cx.notify();
                return;
            }
            Err(e) => {
                self.push_activity(RED, e.to_string(), timefmt::now_unix());
                cx.notify();
                return;
            }
        };
        let Some(page) = self.account_page.as_mut().filter(|p| p.pubkey == pubkey) else {
            return;
        };
        page.revealed = Some(account::Revealed {
            nsec,
            hide_at: Instant::now() + account::NSEC_SHOWN_FOR,
        });
        page.reveal_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(1))
                    .await;
                let Ok(hidden) = this.update(cx, |shell, cx| {
                    cx.notify();
                    let Some(page) = &mut shell.account_page else {
                        return true;
                    };
                    let expired = page
                        .revealed
                        .as_ref()
                        .is_none_or(|r| Instant::now() >= r.hide_at);
                    if expired {
                        // Only the key: this task is `reveal_timer` and ends
                        // by returning, not by dropping itself.
                        page.revealed = None;
                    }
                    expired
                }) else {
                    return;
                };
                if hidden {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// Masks the Account page's nsec, if one is showing.
    fn hide_nsec(&mut self) {
        if let Some(page) = &mut self.account_page {
            page.hide_nsec();
        }
    }

    /// Whether the next remove click on `pubkey` deletes it.
    pub(crate) fn remove_armed(&self, pubkey: &str) -> bool {
        self.confirm_remove.as_deref() == Some(pubkey)
    }

    /// The armed remove's warning: the balance read still out, the read
    /// failed, or the wallet still holds sats. Arming re-reads the balance,
    /// and the second click waits for it.
    pub(crate) fn remove_warning(&self, pubkey: &str) -> Option<String> {
        if !self.remove_armed(pubkey) {
            return None;
        }
        match self.remove_check {
            BalanceCheck::Checking(_) => return Some("Checking balance…".into()),
            BalanceCheck::Failed => {
                return Some(
                    "The balance could not be read. Click again to remove anyway.".into(),
                );
            }
            BalanceCheck::Done => {}
        }
        let sats = self.view(pubkey)?.wallet.balance?;
        (sats > 0).then(|| {
            format!(
                "This wallet still holds {} sats. Send them first, \
                 or copy the Coinos username and password.",
                timefmt::fmt_sats(sats)
            )
        })
    }

    /// "Remove this account", in two clicks: the first arms it (the link
    /// reads "Click again to remove"), the second has the runtime delete the
    /// nsec and Coinos login and re-announce the store; `AccountsLoaded`
    /// then moves the active account if it was this one.
    pub(crate) fn remove_account(&mut self, pubkey: String, cx: &mut Context<Self>) {
        let has_token = self.view(&pubkey).is_some_and(|view| view.wallet.has_token);
        // Removing the active account moves it; see set_account.
        let held = self.active.as_deref() == Some(pubkey.as_str()) && self.form_in_flight();
        let (action, command) = click_remove(
            &mut self.confirm_remove,
            &mut self.remove_check,
            &mut self.balance_request,
            pubkey,
            has_token,
            held,
        );
        if action == RemoveAction::Refuse {
            // Says so in the feed.
            self.hold_for_submit();
        }
        if let Some(command) = command {
            let _ = self.commands.unbounded_send(command);
        }
        cx.notify();
    }

    /// The wallet on screen — the ready panel's account, or ⌘6's active one
    /// — is polled every 3 s while it has a token. Restarted only when the
    /// target changes, so re-renders never reset the cadence.
    fn sync_balance_poll(&mut self, cx: &mut Context<Self>) {
        let target = match self.screen {
            Screen::AccountSetup => self.setup.ready.as_ref().map(|r| r.pubkey.clone()),
            Screen::Wallet => self.active.clone(),
            _ => None,
        }
        .filter(|pubkey| self.view(pubkey).is_some_and(|v| v.wallet.has_token));
        if self.balance_poll.as_ref().map(|(p, _)| p) == target.as_ref() {
            return;
        }
        self.balance_poll = target.map(|pubkey| {
            let commands = self.commands.clone();
            let polled = pubkey.clone();
            let task = cx.spawn(async move |_, cx| {
                // Cancel-on-drop: replacing or clearing `balance_poll` ends it.
                loop {
                    let _ = commands.unbounded_send(Command::FetchBalance {
                        pubkey: polled.clone(),
                        request: None,
                    });
                    cx.background_executor()
                        .timer(Duration::from_secs(3))
                        .await;
                }
            });
            (pubkey, task)
        });
    }

    /// Asks for the bounty list under the next sequence number.
    fn fetch_bounties(&mut self) {
        let read = next_bounties_read(&mut self.bounties_seq);
        let _ = self.commands.unbounded_send(read);
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.fetch_bounties();
        let _ = self.commands.unbounded_send(Command::LoadAccounts);
        // LoadAccounts re-reads every kind-0; a failed one is pending again.
        for (_, view) in &mut self.views {
            if view.profile == ProfileRead::Failed {
                view.profile = ProfileRead::Checking;
            }
        }
        if !matches!(self.bounties, Load::Ready(_)) {
            self.bounties = Load::Loading;
        }
        cx.notify();
    }

    // ------------------------------------------------------------- selection

    pub(crate) fn select(&mut self, id: String) {
        // WatchBounty is idempotent (a live watcher for this id is left
        // alone, and a restarted one keeps its ledger), so selection can
        // always ask — re-selecting never replays history into the feed.
        let _ = self
            .commands
            .unbounded_send(Command::WatchBounty { id: id.clone() });
        self.selected = Some(id);
        // An open claim form claims the highlighted bounty, not the one it
        // was opened on; otherwise "Claim selected" lies once you arrow away.
        if let Some(bounty) = self.selected_bounty() {
            let (id, coordinate) = (bounty.id.clone(), bounty.list_coordinate.clone());
            // A form mid-publish keeps its target so the reply (and a
            // retry) lands on the bounty it was sent to.
            if let Some(form) = self.claim_form.as_mut().filter(|f| !f.submitting) {
                form.bounty_id = id;
                form.coordinate = coordinate;
                form.error = None;
            }
        }
    }

    fn select_step(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.screen != Screen::Bounties {
            return;
        }
        let Load::Ready(list) = &self.bounties else {
            return;
        };
        if list.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|id| list.iter().position(|b| b.id == id));
        let next = match current {
            Some(ix) => (ix as isize + step).clamp(0, list.len() as isize - 1) as usize,
            None => 0,
        };
        let id = list[next].id.clone();
        self.select(id);
        cx.notify();
    }

    // ----------------------------------------------------------------- forms

    /// cmd-N: as the issuer, a new DList+bounty; as anyone else, a claim on
    /// the selected bounty. From any screen — it navigates to Bounties first.
    pub(crate) fn new_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // No account: the honest response is the account screen, not a form
        // whose submit can only fail.
        if self.active.is_none() {
            self.open_account_setup(false, window, cx);
            return;
        }
        if self.screen != Screen::Bounties {
            self.go(Screen::Bounties, window, cx);
        }
        if self.is_active_issuer() {
            self.open_bounty_form(window, cx)
        } else {
            self.open_claim_form(window, cx)
        }
    }

    /// A bounty or claim request is out and unanswered, or the list read
    /// after an unknown create outcome is.
    fn form_in_flight(&self) -> bool {
        self.bounty_form
            .as_ref()
            .is_some_and(|f| form_locked(f.submitting, f.checking_list))
            || self.claim_form.as_ref().is_some_and(|f| f.submitting)
    }

    /// Refuses the actions that would drop or re-own an in-flight form,
    /// saying so in the feed. True when the caller must stop.
    fn hold_for_submit(&mut self) -> bool {
        let held = self.form_in_flight();
        if held {
            self.push_activity(
                AMBER,
                "A submit is in flight; wait for it to finish".into(),
                timefmt::now_unix(),
            );
        }
        held
    }

    /// Closes both forms — except mid-submit (esc, Cancel, or the account
    /// vanishing): the request is out, and a closed form invites a resubmit
    /// and a second bounty or claim. The reply closes or re-enables it.
    pub(crate) fn close_forms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form_in_flight() {
            return;
        }
        if self.bounty_form.is_some() || self.claim_form.is_some() {
            self.bounty_form = None;
            self.claim_form = None;
            self.pending_bounty = None;
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    fn focus_bounty_field(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &self.bounty_form
            && let Some(field) = form.fields.get(ix) {
                field.update(cx, |state, cx| state.focus(window, cx));
            }
    }

    fn open_bounty_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.claim_form = None;
        if self.bounty_form.is_some() {
            // Already open: just put the caret back on the first field.
            self.focus_bounty_field(F_SINGULAR, window, cx);
            cx.notify();
            return;
        }

        let fields: Vec<Entity<InputState>> = BOUNTY_FIELD_DEFS
            .iter()
            .map(|(_, placeholder, default)| {
                cx.new(|cx| {
                    let mut state = InputState::new(window, cx).placeholder(*placeholder);
                    if !default.is_empty() {
                        state = state.default_value(*default);
                    }
                    state
                })
            })
            .collect();

        let subs = fields
            .iter()
            .enumerate()
            .map(|(ix, field)| {
                cx.subscribe_in(field, window, move |this: &mut Self, _, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.bounty_form_step(ix, window, cx);
                    }
                })
            })
            .collect();

        self.bounty_form = Some(BountyForm {
            fields,
            error: None,
            submitting: false,
            checking_list: None,
            _subs: subs,
        });
        self.focus_bounty_field(F_SINGULAR, window, cx);
        cx.notify();
    }

    /// Enter in field `ix`: advance the caret, or submit from the last field.
    fn bounty_form_step(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix + 1 < BOUNTY_FIELD_DEFS.len() {
            self.focus_bounty_field(ix + 1, window, cx);
        } else {
            self.submit_bounty_form(window, cx);
        }
    }

    fn submit_bounty_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.bounty_form else {
            return;
        };
        if form_locked(form.submitting, form.checking_list) {
            return;
        }
        // Forms close on an account switch, so the active account is the
        // one that opened this form.
        let Some(pubkey) = self.active.clone() else {
            return;
        };
        let value =
            |ix: usize| -> String { form.fields[ix].read(cx).value().trim().to_string() };

        let singular = value(F_SINGULAR);
        let plural = value(F_PLURAL);
        let description = value(F_DESCRIPTION);
        let criteria = value(F_CRITERIA);

        // Everything is validated BEFORE anything publishes: the DList goes
        // out first and cannot be unpublished, so a bounty body the server
        // would 400 (normalizeBountyCreatePayload: amountSats must be a
        // positive integer, bountyCapSats >= amountSats, autoPayMinRank
        // positive) must be caught here, not by the server.
        // (message, field to put the caret back on)
        let mut fail: Option<(&str, usize)> = None;
        if singular.is_empty() {
            fail = Some(("The list needs a singular item name.", F_SINGULAR));
        } else if criteria.is_empty() {
            fail = Some(("The bounty needs criteria.", F_CRITERIA));
        }
        let amount = match parse_sats(&value(F_AMOUNT)) {
            Some(amount) if amount >= 1 => Some(amount),
            _ => {
                fail = fail.or(Some((
                    "Reward per item must be a positive whole number of sats.",
                    F_AMOUNT,
                )));
                None
            }
        };
        let cap_text = value(F_CAP);
        let cap = if cap_text.is_empty() {
            // The server defaults the cap to amountSats.
            None
        } else {
            match parse_sats(&cap_text) {
                Some(cap) if cap >= 1 => Some(cap),
                _ => {
                    fail = fail.or(Some((
                        "Bounty cap must be a positive whole number of sats.",
                        F_CAP,
                    )));
                    None
                }
            }
        };
        if let (Some(amount), Some(cap)) = (amount, cap)
            && cap < amount {
                fail = fail.or(Some((
                    "The bounty cap must be at least the reward per item.",
                    F_CAP,
                )));
            }
        let rank_text = value(F_RANK);
        let min_rank = if rank_text.is_empty() {
            None
        } else {
            match parse_sats(&rank_text) {
                Some(rank) if rank >= 1 => Some(rank),
                _ => {
                    fail = fail.or(Some((
                        "Minimum rank must be a positive whole number.",
                        F_RANK,
                    )));
                    None
                }
            }
        };

        // A bounty on this list already exists — typically a timed-out
        // submit the server finished anyway. The server never dedupes. Only
        // as good as the last list read: the form stays locked until the
        // reply to the read sent on the unknown outcome (or a later one)
        // lands. A create the server finishes after that read still slips
        // by, and a BountiesFailed unlocks against the old list.
        let coordinate = nostr_sdk::prelude::PublicKey::from_hex(&pubkey)
            .map(|pk| events::header_coordinate(&pk, &events::header_dtag(&singular)))
            .ok();
        if let (Load::Ready(bounties), Some(coordinate)) = (&self.bounties, &coordinate)
            && bounties.iter().any(|b| {
                &b.list_coordinate == coordinate && b.effective_status() == "open"
            })
        {
            fail = fail.or(Some((
                "An open bounty for this list already exists. Pick it from the list instead.",
                F_SINGULAR,
            )));
        }

        if let Some((message, field)) = fail {
            if let Some(form) = &mut self.bounty_form {
                form.error = Some(message.to_string());
            }
            self.focus_bounty_field(field, window, cx);
            cx.notify();
            return;
        }

        self.pending_bounty = Some(CreateBounty {
            // Filled in when DListPublished comes back with the coordinate.
            list_coordinate: String::new(),
            amount_sats: amount.unwrap_or(0),
            criteria,
            bounty_cap_sats: cap,
            // The rehearsed demo shape: per-item rewards, one per npub,
            // auto-pay armed behind the rank gate.
            reward_per_item: true,
            max_rewards_per_npub: Some(1),
            auto_pay: true,
            auto_pay_min_rank: min_rank,
        });
        let _ = self.commands.unbounded_send(Command::PublishDList {
            pubkey,
            singular,
            plural,
            description: if description.is_empty() {
                None
            } else {
                Some(description)
            },
        });
        if let Some(form) = &mut self.bounty_form {
            form.error = None;
            form.submitting = true;
        }
        cx.notify();
    }

    fn open_claim_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.bounty_form = None;
        if let Some(form) = &self.claim_form {
            form.name.update(cx, |state, cx| state.focus(window, cx));
            cx.notify();
            return;
        }
        // The claim needs the list coordinate, so a bounty must be selected.
        let Some(bounty) = self.selected_bounty() else {
            return;
        };
        let bounty_id = bounty.id.clone();
        let coordinate = bounty.list_coordinate.clone();

        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Item name, e.g. Memphis"));
        let sub = cx.subscribe_in(&name, window, |this: &mut Self, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_claim_form(window, cx);
            }
        });
        name.update(cx, |state, cx| state.focus(window, cx));
        self.claim_form = Some(ClaimForm {
            bounty_id,
            coordinate,
            name,
            error: None,
            submitting: false,
            _sub: sub,
        });
        cx.notify();
    }

    fn submit_claim_form(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.claim_form else {
            return;
        };
        if form.submitting {
            return;
        }
        let name = form.name.read(cx).value().trim().to_string();
        let coordinate = form.coordinate.clone();
        let bounty_id = form.bounty_id.clone();
        let Some(pubkey) = self.active.clone() else {
            return;
        };
        if name.is_empty() {
            if let Some(form) = &mut self.claim_form {
                form.error = Some("The claim needs an item name.".into());
            }
            cx.notify();
            return;
        }
        let _ = self.commands.unbounded_send(Command::PublishClaim {
            pubkey,
            bounty_id,
            name,
            list_coordinate: coordinate,
        });
        if let Some(form) = &mut self.claim_form {
            form.error = None;
            form.submitting = true;
        }
        cx.notify();
    }

    // ------------------------------------------------------------ icon rail

    fn logo(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("logo")
            .size(px(38.))
            .mb(px(6.))
            .flex_shrink_0()
            .rounded(px(11.))
            .overflow_hidden()
            .cursor_pointer()
            .shadow(vec![
                BoxShadow::new(px(0.), px(4.), wash(ACCENT_GLOW)).blur_radius(px(16.)),
            ])
            .on_click(cx.listener(|this, _, w, cx| this.go(Screen::Dashboard, w, cx)))
            .child(img(crate::icons::icon("logo")).size_full())
    }

    /// The account's round marker: kind-0 picture when there is one, else
    /// initials on the rail hue for its position.
    pub(crate) fn account_marker(view: &AccountView, ix: usize, size: f32, text: f32) -> AnyElement {
        if let Some(url) = &view.picture {
            return img(SharedUri::from(url.clone()))
                .size(px(size))
                .rounded_full()
                .into_any_element();
        }
        avatar(initials(view), account_hue(ix), size, text).into_any_element()
    }

    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rail_button = |screen: Screen, on: bool| {
            div()
                .id(SharedString::from(screen.label()))
                .size(px(36.))
                .flex_shrink_0()
                .rounded(px(10.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(if on { rgb(BG_NAV_ON) } else { rgb(BG_RAIL) })
                .hover(|this| this.bg(rgb(BG_NAV_ON)))
                .child(glyph(screen.icon(), if on { TEXT } else { TEXT_MUTED }))
        };

        v_flex()
            .w(px(70.))
            .flex_shrink_0()
            .bg(rgb(BG_RAIL))
            .border_r_1()
            .border_color(rgb(LINE))
            .items_center()
            .gap(px(12.))
            .py(px(16.))
            .child(self.logo(cx))
            .child(div().w(px(32.)).h(px(1.)).bg(rgb(LINE)))
            .children(self.views.iter().enumerate().map(|(ix, (pubkey, view))| {
                let ring = if self.active.as_deref() == Some(pubkey.as_str()) {
                    rgb(ACCENT)
                } else {
                    rgb(BG_NAV_ON)
                };
                let pubkey = pubkey.clone();
                div()
                    .id(("account", ix))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .rounded_full()
                    .shadow(vec![
                        BoxShadow::new(px(0.), px(0.), rgb(BG_RAIL).into()).spread_radius(px(2.)),
                        BoxShadow::new(px(0.), px(0.), ring.into()).spread_radius(px(4.)),
                    ])
                    .child(Self::account_marker(view, ix, 42., 15.))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_account(&pubkey, window, cx);
                    }))
            }))
            .child(
                // "+": the one door to another account, pasted or created.
                div()
                    .id("add-account")
                    .size(px(42.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_dashed()
                    .border_color(rgb(BORDER_3))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(20.))
                    .text_color(rgb(TEXT_DIM))
                    .hover(|this| this.border_color(rgb(ACCENT)).text_color(rgb(TEXT)))
                    .child("+")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_account_setup(false, window, cx)
                    })),
            )
            .child(div().flex_1())
            .child(
                v_flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().size(px(8.)).rounded_full().bg(rgb(
                        if self.relay_connected { GREEN } else { RED },
                    )))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(rgb(TEXT_DIM))
                            .child("relay"),
                    ),
            )
            .child(
                rail_button(
                    Screen::Accounts,
                    matches!(self.screen, Screen::Accounts | Screen::Account),
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.go(Screen::Accounts, window, cx)),
                ),
            )
            .child(
                rail_button(Screen::Settings, self.screen == Screen::Settings).on_click(
                    cx.listener(|this, _, window, cx| this.go(Screen::Settings, window, cx)),
                ),
            )
    }

    // --------------------------------------------------------- nav sidebar

    fn stat_card(
        label: &'static str,
        value: SharedString,
        unit: Option<&'static str>,
        hue: u32,
        dim: u32,
        note: Option<SharedString>,
    ) -> impl IntoElement {
        v_flex()
            .mx(px(2.))
            .mb(px(6.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .bg(rgb(BG_APP))
            .border_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .text_size(px(9.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT_DIM))
                    .child(label),
            )
            .child(
                h_flex()
                    .mt(px(2.))
                    .gap(px(4.))
                    .items_baseline()
                    .font_family(MONO)
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(hue))
                            .child(value),
                    )
                    .when_some(unit, |this, unit| {
                        this.child(div().text_size(px(10.)).text_color(rgb(dim)).child(unit))
                    }),
            )
            .when_some(note, |this, note| {
                this.child(
                    div()
                        .mt(px(1.))
                        .text_size(px(10.5))
                        .text_color(rgb(TEXT_DIM))
                        .child(note),
                )
            })
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.active_view();
        let name: SharedString = view
            .map(display_name)
            .unwrap_or_else(|| "Read-only".to_string())
            .into();
        let identity: SharedString = match view {
            Some(view) => short_npub(&view.npub).into(),
            // Quiet, not alarming: browsing works without an account. The
            // "+" in the rail is the fix path.
            None => "no account — + adds one".into(),
        };
        // The address payouts go to, as the relays have it (or the Coinos
        // wallet this app opened). Claimants asked where their sats land.
        let address = view.and_then(|v| v.payout_address()).map(SharedString::new);
        let active = self.active.clone();
        let remove_armed = active.is_some() && self.confirm_remove == active;
        let sats_left = active.as_deref().and_then(|p| self.remove_warning(p));

        // The three cards are the issuer's live bounty economics; before the
        // list loads (or when it failed) they say so instead of guessing.
        //
        // The money-out card counts SETTLED (receipted) payouts only: the
        // list endpoint's paymentState is the one dataset loaded for every
        // bounty, and its paidRewardCount excludes paid_unreceipted rows —
        // sats can leave the wallet before any receipt lands. Per-claim
        // autoPayment rows exist only for watched bounties, so summing them
        // would silently undercount. The label says exactly what is counted,
        // so the sidebar can never contradict a claim card.
        let (open_value, committed, settled, note): (
            SharedString,
            SharedString,
            SharedString,
            Option<SharedString>,
        ) = match &self.bounties {
            Load::Ready(list) => {
                let open: Vec<&Bounty> = list
                    .iter()
                    .filter(|b| b.effective_status() == "open")
                    .collect();
                let committed: u64 = open
                    .iter()
                    .filter_map(|b| b.payment_state.as_ref())
                    .map(|s| s.open_reward_slots * s.reward_amount_sats)
                    .sum();
                let settled: u64 = list
                    .iter()
                    .filter_map(|b| b.payment_state.as_ref())
                    .map(|s| s.paid_reward_count * s.reward_amount_sats)
                    .sum();
                (
                    open.len().to_string().into(),
                    timefmt::fmt_sats(committed).into(),
                    timefmt::fmt_sats(settled).into(),
                    Some(format!("{} bounties total", list.len()).into()),
                )
            }
            Load::Loading => ("…".into(), "…".into(), "…".into(), Some("loading".into())),
            Load::Failed(_) => (
                "—".into(),
                "—".into(),
                "—".into(),
                Some("list unavailable".into()),
            ),
        };

        v_flex()
            .id("sidebar")
            .w(px(218.))
            .flex_shrink_0()
            .min_h(px(0.))
            // Scrolls when the window is shorter than the nav plus the cards.
            .overflow_y_scroll()
            .bg(rgb(BG_SIDEBAR))
            .border_r_1()
            .border_color(rgb(LINE))
            .gap(px(2.))
            .px(px(10.))
            .py(px(14.))
            .child(
                div()
                    .mb(px(6.))
                    .pb(px(6.))
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        h_flex()
                            .id("account-header")
                            .gap(px(8.))
                            .items_center()
                            .px(px(11.))
                            .py(px(6.))
                            .rounded(px(9.))
                            .cursor_pointer()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .text_size(px(13.5))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(name),
                                    )
                                    .child(
                                        div()
                                            .mt(px(1.))
                                            .font_family(MONO)
                                            .text_size(px(10.5))
                                            .text_color(rgb(TEXT_DIM))
                                            .child(identity),
                                    )
                                    .when_some(address, |this, address| {
                                        this.child(
                                            div()
                                                .mt(px(1.))
                                                .font_family(MONO)
                                                .text_size(px(10.5))
                                                .text_color(rgb(TEXT_DIM))
                                                .child(address),
                                        )
                                    })
                                    .when_some(active, |this, pubkey| {
                                        this.child(
                                            div()
                                                .id("remove-account")
                                                .mt(px(3.))
                                                .text_size(px(10.))
                                                .text_color(rgb(if remove_armed {
                                                    RED
                                                } else {
                                                    TEXT_DIM
                                                }))
                                                .cursor_pointer()
                                                .hover(|this| this.text_color(rgb(RED)))
                                                .child(if remove_armed {
                                                    "Click again to remove"
                                                } else {
                                                    "Remove this account"
                                                })
                                                .on_click(cx.listener(
                                                    move |this, _, _, cx| {
                                                        // Sits inside the account
                                                        // switcher; don't switch too.
                                                        cx.stop_propagation();
                                                        this.remove_account(pubkey.clone(), cx)
                                                    },
                                                )),
                                        )
                                    })
                                    .when_some(sats_left, |this, warning| {
                                        this.child(
                                            div()
                                                .mt(px(2.))
                                                .text_size(px(10.))
                                                .text_color(rgb(AMBER))
                                                .child(warning),
                                        )
                                    }),
                            )
                            .child(
                                // The chevron alone keeps its "next account"
                                // step, like ⌘] — the rest of the chip opens
                                // the account's Trust tab.
                                div()
                                    .id("account-step")
                                    .text_color(rgb(TEXT_DIM))
                                    .child("›")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.step_account(1, window, cx);
                                    })),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                match this.active.clone() {
                                    Some(active) => {
                                        this.open_account(&active, Tab::Trust, window, cx)
                                    }
                                    None => this.go(Screen::Accounts, window, cx),
                                }
                            })),
                    ),
            )
            .children(NAV.iter().map(|&screen| {
                let on = self.screen == screen;
                let badge = match (screen, &self.bounties) {
                    (Screen::Bounties, Load::Ready(list)) => {
                        let open = list
                            .iter()
                            .filter(|b| b.effective_status() == "open")
                            .count();
                        (open > 0).then(|| open.to_string())
                    }
                    _ => None,
                };
                h_flex()
                    .id(SharedString::from(screen.label()))
                    .gap(px(10.))
                    .items_center()
                    .px(px(11.))
                    .py(px(9.))
                    .rounded(px(9.))
                    .cursor_pointer()
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .when(on, |this| this.bg(rgb(BG_NAV_ON)))
                    .text_color(if on { rgb(TEXT) } else { rgb(TEXT_MUTED) })
                    .hover(|this| this.bg(rgb(BG_HOVER)))
                    .child(glyph(screen.icon(), if on { TEXT } else { TEXT_MUTED }))
                    .child(div().flex_1().min_w(px(0.)).child(screen.label()))
                    .when_some(badge, |this, badge| {
                        this.child(dashboard::count_badge(badge))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.go(screen, window, cx)))
            }))
            .child(div().flex_1())
            .child(Self::stat_card(
                "OPEN BOUNTIES",
                open_value,
                None,
                ACCENT_LIGHT,
                TEXT_DIM,
                note,
            ))
            .child(Self::stat_card(
                "COMMITTED",
                committed,
                Some("sats"),
                AMBER,
                AMBER_DIM,
                Some("open reward slots".into()),
            ))
            .child(Self::stat_card(
                "SETTLED TO DATE",
                settled,
                Some("sats"),
                GREEN,
                GREEN_DIM,
                Some("receipted auto-pay".into()),
            ))
    }

    // ------------------------------------------------------------- top bar

    fn top_bar(&self) -> impl IntoElement {
        // The pill tells the truth about the focused bounty, or stays away.
        let armed = self.selected_bounty().map(|b| b.auto_pay_on());

        h_flex()
            .flex_shrink_0()
            .gap(px(12.))
            .items_center()
            .px(px(26.))
            .py(px(10.))
            .min_h(px(46.))
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex_1()
                    .min_w(px(170.))
                    .text_size(px(12.5))
                    .child(Input::new(&self.search)),
            )
            .when_some(armed, |this, armed| {
                this.child(if armed {
                    h_flex()
                        .flex_shrink_0()
                        .gap(px(9.))
                        .items_center()
                        .px(px(14.))
                        .py(px(7.))
                        .rounded_full()
                        .bg(wash(GREEN_WASH))
                        .border_1()
                        .border_color(rgb(GREEN_EDGE))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(GREEN))
                        .child(div().size(px(8.)).rounded_full().bg(rgb(GREEN)))
                        .child("Auto-pay armed")
                } else {
                    h_flex()
                        .flex_shrink_0()
                        .gap(px(9.))
                        .items_center()
                        .px(px(14.))
                        .py(px(7.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(BORDER_2))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_MUTED))
                        .child(div().size(px(8.)).rounded_full().bg(rgb(TEXT_DIM)))
                        .child("Auto-pay off")
                })
            })
    }

    // --------------------------------------------------------- main content

    fn placeholder(screen: Screen) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(21.))
                    .font_weight(FontWeight::BOLD)
                    .child(screen.label()),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_MUTED))
                    .child("This screen is not built yet."),
            )
    }

    fn main(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.screen {
            Screen::Dashboard => div()
                .id("dashboard")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(
                    div()
                        .w_full()
                        .max_w(px(900.))
                        .mx_auto()
                        .px(px(28.))
                        .pt(px(26.))
                        .pb(px(60.))
                        .child(dashboard::render(self, cx)),
                )
                .into_any_element(),
            // The Bounties screen does NOT scroll as one page: the list pane
            // and the claim list each scroll on their own, so the detail
            // pane's header (title / status / metrics) stays pinned while the
            // claims scroll under it.
            Screen::Bounties => v_flex()
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .max_w(px(980.))
                .mx_auto()
                .px(px(28.))
                .pt(px(26.))
                .pb(px(20.))
                .child(bounties::render(self, cx))
                .into_any_element(),
            Screen::Chat => v_flex()
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .max_w(px(900.))
                .mx_auto()
                .px(px(28.))
                .py(px(18.))
                .child(self.chat.clone())
                .into_any_element(),
            Screen::Wallet => {
                let title = match self.active_view() {
                    Some(view) => format!("Wallet — {}", display_name(view)),
                    None => "Wallet".to_string(),
                };
                let panel = match self.active_view() {
                    Some(view) => wallet::panel(view, false, Some(&self.send_inputs), cx),
                    None => dashboard::muted(
                        "No account — press + in the rail to add one.",
                        TEXT_MUTED,
                    ),
                };
                div()
                    .id("wallet")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(560.))
                            .mx_auto()
                            .px(px(28.))
                            .pt(px(26.))
                            .pb(px(60.))
                            .gap(px(14.))
                            .child(
                                div()
                                    .text_size(px(21.))
                                    .font_weight(FontWeight::BOLD)
                                    .child(SharedString::from(title)),
                            )
                            .child(
                                v_flex()
                                    .w_full()
                                    .bg(rgb(BG_CARD))
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .rounded(px(14.))
                                    .px(px(22.))
                                    .py(px(20.))
                                    .child(panel),
                            ),
                    )
                    .into_any_element()
            }
            // Accounts and Account scroll as one page, like the Dashboard.
            Screen::Accounts | Screen::Account => div()
                .id("accounts")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(
                    div()
                        .w_full()
                        .max_w(px(900.))
                        .mx_auto()
                        .px(px(28.))
                        .pt(px(26.))
                        .pb(px(60.))
                        .child(if self.screen == Screen::Accounts {
                            account::render_accounts(self, cx)
                        } else {
                            account::render_account(self, cx)
                        }),
                )
                .into_any_element(),
            other => Self::placeholder(other).into_any_element(),
        }
    }
}

/// How long a copied nsec or Coinos password stays on the clipboard.
const SECRET_ON_CLIPBOARD_FOR: Duration = Duration::from_secs(30);

/// Copy `secret`, then clear the clipboard after [`SECRET_ON_CLIPBOARD_FOR`]
/// if it still holds exactly that value — something copied since is left
/// alone. gpui has no concealed-clipboard flag, so clipboard managers may
/// still keep their own copy. On Wayland the clear only takes effect while
/// the window has focus, and quitting within the 30 s leaves the clipboard
/// as it is. The value lives only in this task.
fn copy_secret(secret: Secret, cx: &mut Context<Shell>) {
    cx.write_to_clipboard(ClipboardItem::new_string(secret.expose().to_string()));
    cx.spawn(async move |_, cx| {
        cx.background_executor().timer(SECRET_ON_CLIPBOARD_FOR).await;
        cx.update(|cx| {
            let unchanged = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some_and(|text| text == secret.expose());
            if unchanged {
                // An empty item is clearContents on macOS.
                cx.write_to_clipboard(ClipboardItem { entries: vec![] });
            }
        });
    })
    .detach();
}

/// The just-created bounty, rebuilt from the values the server was actually
/// sent, so the detail pane renders them instantly. Server data replaces it
/// as soon as the first list/detail arrives — this is a stopgap for the
/// seconds in between, never a source of record.
fn optimistic_bounty(id: &str, req: CreateBounty, now: u64) -> Bounty {
    Bounty {
        id: id.to_string(),
        issuer_pubkey: nostr::issuer_pubkey(),
        list_coordinate: req.list_coordinate,
        amount_sats: req.amount_sats,
        criteria: Some(req.criteria).filter(|c| !c.is_empty()),
        expiration: None,
        created_at: Some(now),
        status: Some("open".into()),
        bounty_cap_sats: req.bounty_cap_sats,
        reward_per_item: Some(serde_json::Value::Bool(req.reward_per_item)),
        max_rewards_per_npub: req.max_rewards_per_npub,
        auto_pay: Some(serde_json::Value::Bool(req.auto_pay)),
        auto_pay_min_rank: req.auto_pay_min_rank,
        derived_status: Some("open".into()),
        payment_state: None,
    }
}

/// A sats field, tolerant of `1,000` / `1_000` styles. `None` when nothing
/// numeric is left.
fn parse_sats(value: &str) -> Option<u64> {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() != value.replace([',', '_', ' '], "").len() {
        return None;
    }
    digits.parse().ok()
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The account screen takes the whole window: no rail, no sidebar, no
        // top bar — one decision at a time.
        let body: AnyElement = if self.screen == Screen::AccountSetup {
            account_setup::render(self, cx).into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_h(px(0.))
                // h_flex centres its children; the three columns must each
                // run the full height instead.
                .items_stretch()
                .child(self.rail(cx))
                .child(self.sidebar(cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .child(self.top_bar())
                        .child(self.main(cx)),
                )
                .into_any_element()
        };

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            // Names this node in the key-context stack so the enter/up/down
            // bindings below only fire here. A focused text input sits deeper
            // in the stack, so its own bindings win — without this, a
            // context-less "enter" registered after gpui_kit::init
            // shadows the input's Enter everywhere (GPUI treats no-context
            // bindings as deepest-context, later registration wins).
            .key_context("Shell")
            .on_action(cx.listener(|this, _: &GoDashboard, w, cx| this.go(Screen::Dashboard, w, cx)))
            .on_action(cx.listener(|this, _: &GoBounties, w, cx| this.go(Screen::Bounties, w, cx)))
            .on_action(cx.listener(|this, _: &GoPayments, w, cx| this.go(Screen::Payments, w, cx)))
            .on_action(cx.listener(|this, _: &GoClaimants, w, cx| this.go(Screen::Claimants, w, cx)))
            .on_action(cx.listener(|this, _: &GoBoards, w, cx| this.go(Screen::Boards, w, cx)))
            .on_action(cx.listener(|this, _: &GoWallet, w, cx| this.go(Screen::Wallet, w, cx)))
            .on_action(cx.listener(|this, _: &GoTags, w, cx| this.go(Screen::Tags, w, cx)))
            .on_action(cx.listener(|this, _: &GoChat, w, cx| this.go(Screen::Chat, w, cx)))
            .on_action(cx.listener(|this, _: &PrevAccount, w, cx| this.step_account(-1, w, cx)))
            .on_action(cx.listener(|this, _: &NextAccount, w, cx| this.step_account(1, w, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx)))
            .on_action(cx.listener(|this, _: &NewItem, w, cx| this.new_item(w, cx)))
            .on_action(cx.listener(|this, _: &CloseForm, w, cx| this.cancel(w, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.select_step(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_step(1, cx)))
            .on_action(cx.listener(|this, _: &Confirm, w, cx| this.confirm(w, cx)))
            .bg(rgb(BG_APP))
            .text_color(rgb(TEXT))
            .text_size(px(13.))
            .child(
                // Our own title bar: it keeps the window draggable and the
                // traffic lights clear of the icon rail.
                TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .justify_center()
                        .text_size(px(12.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Magic Carpet"),
                ),
            )
            .child(body)
            // Toasts live in Root; the view has to draw the layer.
            .children(Root::render_notification_layer(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BalanceCheck, RANK_NOTE, RemoveAction, claim_notice, click_remove, fold_checks,
        form_locked, next_bounties_read, on_outcome_unknown, pick_active, relay_address_gap,
        remove_click, still_checking_list,
    };
    use magic_carpet_chat::nostr::{Command, Update};

    #[test]
    fn an_auto_pay_claim_gets_a_short_toast_and_the_rank_note_behind_a_click() {
        let notice = claim_notice(Some((true, 2, 21_000)));
        assert_eq!(notice.title, "Claim published — auto-pay bounty");
        assert_eq!(
            notice.short,
            "With rank 2 or higher, 21,000 sats arrive within about a minute. Click for details."
        );
        let full = notice.full.expect("auto-pay has details");
        assert!(full.contains(RANK_NOTE), "the rank note moved out of the toast");
        assert!(!notice.short.contains(RANK_NOTE));
        // Nothing to expand when the bounty is unknown.
        assert!(claim_notice(None).full.is_none());
        let manual = claim_notice(Some((false, 0, 500)));
        assert!(manual.full.expect("manual pay has details").contains("by hand"));
    }

    #[test]
    fn a_stored_wallet_shows_retry_until_the_relays_carry_its_address() {
        // Relaunch: the panel pre-fills the Coinos address from the store.
        // Only a profile read decides whether the payer can see it.
        assert_eq!(relay_address_gap(Some("carpet12@coinos.io")), None);
        assert!(relay_address_gap(None).is_some(), "confirmed-empty profile");
        assert_eq!(
            relay_address_gap(Some("me@strike.me")),
            None,
            "a different lud16 is never overwritten, so there is no retry"
        );
    }

    #[test]
    fn the_stored_active_account_wins_while_it_still_exists() {
        let stored = vec!["a".to_string(), "b".to_string()];
        assert_eq!(pick_active(&stored, Some("b")).as_deref(), Some("b"));
        // Gone (removed, or a stale preference): the first one.
        assert_eq!(pick_active(&stored, Some("zz")).as_deref(), Some("a"));
        assert_eq!(pick_active(&stored, None).as_deref(), Some("a"));
        // No accounts at all: read-only.
        assert_eq!(pick_active(&[], Some("a")), None);
    }

    #[test]
    fn an_unknown_create_keeps_the_form_locked_until_its_own_list_read_lands() {
        let bounties = |seq| Update::Bounties {
            seq,
            list: Vec::new(),
        };
        let other = Update::RelayStatus(true);
        assert_eq!(still_checking_list(Some(5), &other), Some(5));
        // A ⌘R sent before the timeout answers with the old list.
        assert_eq!(still_checking_list(Some(5), &bounties(4)), Some(5));
        assert_eq!(still_checking_list(Some(5), &bounties(5)), None);
        assert_eq!(still_checking_list(Some(5), &bounties(6)), None);
        // A failed read unlocks too, so the form can never stay locked.
        let failed = Update::BountiesFailed {
            seq: 5,
            message: "down".into(),
        };
        assert_eq!(still_checking_list(Some(5), &failed), None);
        assert_eq!(still_checking_list(None, &bounties(9)), None);
        // The check counts as a submit in flight.
        assert!(form_locked(false, Some(5)));
        assert!(form_locked(true, None));
        assert!(!form_locked(false, None));
    }

    #[test]
    fn the_second_remove_click_waits_for_the_arming_clicks_own_balance_read() {
        let balance = |pubkey: &str, request| Update::Balance {
            pubkey: pubkey.into(),
            sats: 5,
            request,
        };
        let failed = |pubkey: &str, request| Update::BalanceFailed {
            pubkey: pubkey.into(),
            request,
        };
        let checking = BalanceCheck::Checking(7);
        // A poll (no id), an earlier arm, or another account changes nothing.
        assert_eq!(checking.after("a", &failed("a", None)), checking);
        assert_eq!(checking.after("a", &balance("a", None)), checking);
        assert_eq!(checking.after("a", &failed("a", Some(6))), checking);
        assert_eq!(checking.after("a", &balance("b", Some(7))), checking);
        assert_eq!(checking.after("a", &balance("a", Some(7))), BalanceCheck::Done);
        // A failed read ends the wait; the confirm then says so.
        assert_eq!(checking.after("a", &failed("a", Some(7))), BalanceCheck::Failed);
        assert_eq!(
            BalanceCheck::Done.after("a", &failed("a", Some(7))),
            BalanceCheck::Done
        );
        // After a failed read, the 3 s poll's balance brings the sats
        // warning back; another account's does not.
        assert_eq!(BalanceCheck::Failed.after("a", &balance("a", None)), BalanceCheck::Done);
        assert_eq!(BalanceCheck::Failed.after("a", &balance("b", None)), BalanceCheck::Failed);
    }

    #[test]
    fn a_remove_click_arms_waits_refuses_or_removes() {
        use RemoveAction::*;
        let checking = BalanceCheck::Checking(1);
        assert_eq!(remove_click(false, BalanceCheck::Done, true), Arm);
        assert_eq!(remove_click(true, checking, false), Wait);
        assert_eq!(remove_click(true, checking, true), Wait);
        assert_eq!(remove_click(true, BalanceCheck::Done, true), Refuse);
        assert_eq!(remove_click(true, BalanceCheck::Failed, false), Remove);
        assert_eq!(remove_click(true, BalanceCheck::Done, false), Remove);
    }

    #[test]
    fn an_unknown_outcome_locks_the_form_on_the_read_it_sends() {
        let mut seq = 3;
        assert!(matches!(next_bounties_read(&mut seq), Command::FetchBounties { seq: 4 }));
        let mut checking = None;
        let Command::FetchBounties { seq: sent } = on_outcome_unknown(&mut checking, &mut seq)
        else {
            panic!("not a list read");
        };
        assert_eq!((sent, checking, seq), (5, Some(5), 5));
        // A ⌘R reply from before the timeout keeps it locked; the reply to
        // the read sent above unlocks it, through the fold `apply` runs.
        let mut remove_check = BalanceCheck::Done;
        let reply = |seq| Update::Bounties {
            seq,
            list: Vec::new(),
        };
        fold_checks(Some(&mut checking), None, &mut remove_check, &reply(4));
        assert_eq!(checking, Some(5));
        fold_checks(Some(&mut checking), None, &mut remove_check, &reply(sent));
        assert_eq!(checking, None);
    }

    #[test]
    fn remove_clicks_arm_with_their_own_read_then_wait_for_it() {
        let (mut armed, mut check, mut request) = (None, BalanceCheck::Done, 0);
        let mut click = |armed: &mut Option<String>, check: &mut BalanceCheck, held| {
            click_remove(armed, check, &mut request, "a".into(), true, held)
        };

        // First click: armed, and the read it sends carries the id it waits on.
        let (action, command) = click(&mut armed, &mut check, false);
        assert_eq!(action, RemoveAction::Arm);
        let Some(Command::FetchBalance { pubkey, request: Some(id) }) = command else {
            panic!("the arming click sent no tagged balance read");
        };
        assert_eq!((pubkey.as_str(), check), ("a", BalanceCheck::Checking(id)));
        assert_eq!(armed.as_deref(), Some("a"));

        // A fast second click waits and stays armed.
        let (action, command) = click(&mut armed, &mut check, false);
        assert_eq!((action, command.is_none()), (RemoveAction::Wait, true));
        assert_eq!(armed.as_deref(), Some("a"));

        // A poll's reply does not end the wait; the arming read's does.
        let poll = Update::Balance {
            pubkey: "a".into(),
            sats: 0,
            request: None,
        };
        fold_checks(None, armed.as_deref(), &mut check, &poll);
        assert_eq!(check, BalanceCheck::Checking(id));
        let own = Update::Balance {
            pubkey: "a".into(),
            sats: 0,
            request: Some(id),
        };
        fold_checks(None, armed.as_deref(), &mut check, &own);
        assert_eq!(check, BalanceCheck::Done);

        // Held by an in-flight submit: refused and disarmed.
        let (action, command) = click(&mut armed, &mut check, true);
        assert_eq!((action, command.is_none()), (RemoveAction::Refuse, true));
        assert_eq!(armed, None);

        // Re-armed and answered, the second click removes.
        click(&mut armed, &mut check, false);
        let BalanceCheck::Checking(id) = check else {
            panic!("re-arming did not read the balance");
        };
        let failed = Update::BalanceFailed {
            pubkey: "a".into(),
            request: Some(id),
        };
        fold_checks(None, armed.as_deref(), &mut check, &failed);
        let (action, command) = click(&mut armed, &mut check, false);
        assert_eq!(action, RemoveAction::Remove);
        assert!(matches!(command, Some(Command::RemoveAccount { pubkey }) if pubkey == "a"));
        assert_eq!(armed, None);

        // No Coinos login: nothing to read, so nothing to wait for.
        let (mut armed, mut check) = (None, BalanceCheck::Failed);
        let (_, command) = click_remove(&mut armed, &mut check, &mut 0, "b".into(), false, false);
        assert_eq!((command.is_none(), check), (true, BalanceCheck::Done));
    }
}
