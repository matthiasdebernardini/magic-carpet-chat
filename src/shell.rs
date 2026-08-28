//! The app shell: the icon rail, the nav column, the top bar, and whichever
//! screen the nav points at — plus all the live state those screens render.
//!
//! Everything on screen is backed by state fed from the nostr runtime thread
//! (see `magic_carpet_chat::nostr`): the UI sends [`Command`]s down one
//! channel and drains [`Update`]s from the other into the fields here, using
//! the same task-held-in-`self` pattern as the chat's streaming reply.

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    TitleBar, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use magic_carpet_chat::api::{Bounty, BountyDetail, CreateBounty};
use magic_carpet_chat::nostr::{self, Command, ErrorSource, Update};
use magic_carpet_chat::secrets::Account;

use crate::bounties;
use crate::chat::Chat;
use crate::dashboard::{self, MONO, avatar};
use crate::icons::icon;
use crate::palette::*;
use crate::timefmt;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Dashboard,
    Bounties,
    Payments,
    Claimants,
    Boards,
    Wallet,
    Tags,
    Chat,
    Accounts,
    Settings,
}

impl Screen {
    fn label(self) -> &'static str {
        match self {
            Screen::Dashboard => "Dashboard",
            Screen::Bounties => "Bounties",
            Screen::Payments => "Payments",
            Screen::Claimants => "Claimants",
            Screen::Boards => "Leaderboards",
            Screen::Wallet => "Wallet",
            Screen::Tags => "Tags",
            Screen::Chat => "Chat",
            Screen::Accounts => "Accounts",
            Screen::Settings => "Settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Screen::Dashboard => "overview",
            Screen::Bounties => "bounties",
            Screen::Payments => "payments",
            Screen::Claimants => "claimants",
            Screen::Boards => "boards",
            Screen::Wallet => "wallet",
            Screen::Tags => "tags",
            Screen::Chat => "chat",
            Screen::Accounts => "accounts",
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
        PrevAccount, NextAccount, Refresh, NewItem, CloseForm, SelectPrev, SelectNext
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
        KeyBinding::new("up", SelectPrev, None),
        KeyBinding::new("down", SelectNext, None),
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

/// What the app knows about one of the two identities. Everything starts
/// unknown; the runtime fills it in (or reports the key missing).
#[derive(Default)]
pub(crate) struct AccountView {
    pub pubkey: Option<String>,
    pub npub: Option<String>,
    /// True once the runtime confirmed there is no stored key.
    pub missing: bool,
    /// From the account's kind-0 on the instance relay, when one exists.
    pub kind0_name: Option<String>,
    pub picture: Option<String>,
}

/// The label shown before (or without) a kind-0: the account's ROLE, never a
/// person or brand name — the app cannot know who an imported key belongs to
/// until the profile arrives, and a kind-0 fetch failure is silent.
pub(crate) fn fallback_name(account: Account) -> &'static str {
    match account {
        Account::Issuer => "Issuer",
        Account::Claimant => "Claimant",
    }
}

pub(crate) fn display_name(account: Account, view: &AccountView) -> String {
    view.kind0_name
        .clone()
        .unwrap_or_else(|| fallback_name(account).to_string())
}

pub(crate) fn initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .collect::<String>()
        .to_uppercase()
}

pub(crate) fn account_hue(account: Account) -> (u32, u32) {
    match account {
        Account::Issuer => (HUE_AK_FROM, HUE_AK_TO),
        Account::Claimant => (HUE_BO_FROM, HUE_BO_TO),
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
    pub(crate) account: Account,
    pub(crate) issuer: AccountView,
    pub(crate) claimant: AccountView,
    pub(crate) relay_connected: bool,
    pub(crate) activity: Vec<ActivityItem>,
    pub(crate) bounties: Load<Vec<Bounty>>,
    /// The bounty the detail pane (and the auto-pay pill) is about.
    pub(crate) selected: Option<String>,
    /// Latest full snapshot per watched bounty, straight from the runtime.
    pub(crate) details: HashMap<String, BountyDetail>,
    pub(crate) bounty_form: Option<BountyForm>,
    pub(crate) claim_form: Option<ClaimForm>,
    /// The bounty half of a submitted form, parked while the DList publishes.
    pending_bounty: Option<CreateBounty>,
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
        for command in [Command::Connect, Command::LoadAccounts, Command::FetchBounties] {
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

        Self {
            screen: Screen::Dashboard,
            account: Account::Issuer,
            issuer: AccountView::default(),
            claimant: AccountView::default(),
            relay_connected: false,
            activity: Vec::new(),
            bounties: Load::Loading,
            selected: None,
            details: HashMap::new(),
            bounty_form: None,
            claim_form: None,
            pending_bounty: None,
            commands,
            chat,
            search: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search lists, bounties, profiles…")
            }),
            focus,
            _updates,
            _clock,
        }
    }

    pub(crate) fn view(&self, account: Account) -> &AccountView {
        match account {
            Account::Issuer => &self.issuer,
            Account::Claimant => &self.claimant,
        }
    }

    fn view_mut(&mut self, account: Account) -> &mut AccountView {
        match account {
            Account::Issuer => &mut self.issuer,
            Account::Claimant => &mut self.claimant,
        }
    }

    pub(crate) fn selected_bounty(&self) -> Option<&Bounty> {
        let id = self.selected.as_deref()?;
        if let Some(detail) = self.details.get(id) {
            return Some(&detail.bounty);
        }
        match &self.bounties {
            Load::Ready(list) => list.iter().find(|b| b.id == id),
            _ => None,
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
        match update {
            Update::RelayStatus(connected) => {
                self.relay_connected = connected;
            }
            Update::AccountLoaded {
                account,
                pubkey,
                npub,
            } => {
                let view = self.view_mut(account);
                view.pubkey = Some(pubkey);
                view.npub = Some(npub);
                view.missing = false;
            }
            Update::AccountMissing { account } => {
                self.view_mut(account).missing = true;
            }
            Update::ProfileLoaded {
                account,
                name,
                picture,
            } => {
                let view = self.view_mut(account);
                view.kind0_name = name;
                view.picture = picture;
            }
            Update::Bounties(list) => {
                if self.selected.is_none()
                    && let Some(first) = list.first() {
                        self.select(first.id.clone());
                    }
                self.bounties = Load::Ready(list);
            }
            Update::BountiesFailed(message) => {
                // Keep data already on screen; only a first load fails visibly.
                if !matches!(self.bounties, Load::Ready(_)) {
                    self.bounties = Load::Failed(message.clone());
                }
                self.push_activity(RED, format!("Bounty list failed: {message}"), now);
            }
            Update::BountyDetail(detail) => {
                self.details.insert(detail.bounty.id.clone(), *detail);
            }
            Update::DListPublished {
                account,
                coordinate,
            } => {
                self.push_activity(
                    ACCENT_LIGHT,
                    format!("Published list {coordinate}"),
                    now,
                );
                if account == Account::Issuer
                    && let Some(mut req) = self.pending_bounty.take() {
                        req.list_coordinate = coordinate;
                        let _ = self.commands.unbounded_send(Command::CreateBounty {
                            account: Account::Issuer,
                            req,
                        });
                    }
            }
            Update::BountyCreated { id } => {
                self.push_activity(
                    ACCENT_LIGHT,
                    format!("Bounty {} created — watching for claims", short_id(&id)),
                    now,
                );
                self.bounty_form = None;
                self.focus.focus(window, cx);
                self.selected = Some(id.clone());
                let _ = self.commands.unbounded_send(Command::WatchBounty { id });
                let _ = self.commands.unbounded_send(Command::FetchBounties);
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
                // An error fails a form only when it came from that form's own
                // command — everything else (watch polls, account refreshes)
                // is feed-only noise as far as the forms are concerned.
                match source {
                    ErrorSource::DList | ErrorSource::Bounty => {
                        if let Some(form) = &mut self.bounty_form
                            && form.submitting {
                                form.submitting = false;
                                form.error = Some(message.clone());
                                self.pending_bounty = None;
                            }
                    }
                    ErrorSource::Claim => {
                        if let Some(form) = &mut self.claim_form
                            && form.submitting {
                                form.submitting = false;
                                form.error = Some(message.clone());
                            }
                    }
                    ErrorSource::Runtime | ErrorSource::Accounts | ErrorSource::Watch => {}
                }
                self.push_activity(RED, message, now);
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------ navigation

    /// Switches the main pane. The chat takes the caret with it, so a click on
    /// "Chat" leaves the window typeable.
    fn go(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = screen;
        if screen == Screen::Chat {
            self.chat.focus_handle(cx).focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    fn set_account(&mut self, account: Account, window: &mut Window, cx: &mut Context<Self>) {
        if self.account != account {
            self.account = account;
            // An open form belongs to the account that opened it.
            self.close_forms(window, cx);
        }
        cx.notify();
    }

    fn step_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = match self.account {
            Account::Issuer => Account::Claimant,
            Account::Claimant => Account::Issuer,
        };
        self.set_account(next, window, cx);
    }

    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let _ = self.commands.unbounded_send(Command::FetchBounties);
        let _ = self.commands.unbounded_send(Command::LoadAccounts);
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

    /// cmd-N: as the issuer, a new DList+bounty; as the claimant, a claim on
    /// the selected bounty. From any screen — it navigates to Bounties first.
    pub(crate) fn new_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Bounties {
            self.go(Screen::Bounties, window, cx);
        }
        match self.account {
            Account::Issuer => self.open_bounty_form(window, cx),
            Account::Claimant => self.open_claim_form(window, cx),
        }
    }

    pub(crate) fn close_forms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        if form.submitting {
            return;
        }
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
            account: Account::Issuer,
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
        if name.is_empty() {
            if let Some(form) = &mut self.claim_form {
                form.error = Some("The claim needs an item name.".into());
            }
            cx.notify();
            return;
        }
        let _ = self.commands.unbounded_send(Command::PublishClaim {
            account: Account::Claimant,
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

    fn logo(&self) -> impl IntoElement {
        let bar = |width: f32, alpha: u32| {
            div()
                .w(px(width))
                .h(px(4.))
                .rounded(px(2.))
                .bg(rgba(0x0d0d1400 | alpha))
        };

        v_flex()
            .id("logo")
            .size(px(38.))
            .mb(px(6.))
            .flex_shrink_0()
            .rounded(px(11.))
            .bg(grad(ACCENT_DEEP, ACCENT_PALE))
            .items_center()
            .justify_center()
            .gap(px(3.))
            .cursor_pointer()
            .shadow(vec![
                BoxShadow::new(px(0.), px(4.), wash(ACCENT_GLOW)).blur_radius(px(16.)),
            ])
            .child(bar(20., 0xcc))
            .child(bar(14., 0x99))
            .child(bar(8., 0x66))
    }

    /// The account's round marker: kind-0 picture when there is one, initials
    /// when a key is loaded, a dashed placeholder when there is no key at all.
    pub(crate) fn account_marker(
        account: Account,
        view: &AccountView,
        size: f32,
        text: f32,
    ) -> AnyElement {
        if let Some(url) = &view.picture {
            return img(SharedUri::from(url.clone()))
                .size(px(size))
                .rounded_full()
                .into_any_element();
        }
        if view.npub.is_some() {
            let name = display_name(account, view);
            return avatar(initials(&name), account_hue(account), size, text).into_any_element();
        }
        div()
            .size(px(size))
            .rounded_full()
            .border_1()
            .border_dashed()
            .border_color(rgb(BORDER_3))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(text))
            .text_color(rgb(TEXT_DIM))
            .child("?")
            .into_any_element()
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
            .child(self.logo())
            .child(div().w(px(32.)).h(px(1.)).bg(rgb(LINE)))
            .children(Account::ALL.into_iter().enumerate().map(|(ix, account)| {
                let view = self.view(account);
                let ring = if account == self.account {
                    rgb(ACCENT)
                } else {
                    rgb(BG_NAV_ON)
                };
                div()
                    .id(("account", ix))
                    .flex_shrink_0()
                    .cursor_pointer()
                    .rounded_full()
                    .shadow(vec![
                        BoxShadow::new(px(0.), px(0.), rgb(BG_RAIL).into()).spread_radius(px(2.)),
                        BoxShadow::new(px(0.), px(0.), ring.into()).spread_radius(px(4.)),
                    ])
                    .child(Self::account_marker(account, view, 42., 15.))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_account(account, window, cx);
                    }))
            }))
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
                rail_button(Screen::Accounts, self.screen == Screen::Accounts).on_click(
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
        let account = self.account;
        let view = self.view(account);
        let name = display_name(account, view);
        let identity: SharedString = match (&view.npub, view.missing) {
            (Some(npub), _) => short_npub(npub).into(),
            (None, true) => "no key — run --import-keys".into(),
            (None, false) => "loading key…".into(),
        };

        // The three cards are the issuer's live bounty economics; before the
        // list loads (or when it failed) they say so instead of guessing.
        let (open_value, committed, paid_out, note): (
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
                let paid: u64 = list
                    .iter()
                    .filter_map(|b| b.payment_state.as_ref())
                    .map(|s| s.paid_reward_count * s.reward_amount_sats)
                    .sum();
                (
                    open.len().to_string().into(),
                    timefmt::fmt_sats(committed).into(),
                    timefmt::fmt_sats(paid).into(),
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
            .w(px(218.))
            .flex_shrink_0()
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
                            .gap(px(8.))
                            .items_center()
                            .px(px(11.))
                            .py(px(6.))
                            .rounded(px(9.))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .text_size(px(13.5))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(SharedString::from(name)),
                                    )
                                    .child(
                                        div()
                                            .mt(px(1.))
                                            .font_family(MONO)
                                            .text_size(px(10.5))
                                            .text_color(rgb(TEXT_DIM))
                                            .child(identity),
                                    ),
                            )
                            .child(div().text_color(rgb(TEXT_DIM)).child("›")),
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
                "PAID OUT",
                paid_out,
                Some("sats"),
                GREEN,
                GREEN_DIM,
                Some("auto-pay to date".into()),
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
            Screen::Bounties => div()
                .id("bounties")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .child(
                    div()
                        .w_full()
                        .max_w(px(980.))
                        .mx_auto()
                        .px(px(28.))
                        .pt(px(26.))
                        .pb(px(60.))
                        .child(bounties::render(self, cx)),
                )
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
            other => Self::placeholder(other).into_any_element(),
        }
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &GoDashboard, w, cx| this.go(Screen::Dashboard, w, cx)))
            .on_action(cx.listener(|this, _: &GoBounties, w, cx| this.go(Screen::Bounties, w, cx)))
            .on_action(cx.listener(|this, _: &GoPayments, w, cx| this.go(Screen::Payments, w, cx)))
            .on_action(cx.listener(|this, _: &GoClaimants, w, cx| this.go(Screen::Claimants, w, cx)))
            .on_action(cx.listener(|this, _: &GoBoards, w, cx| this.go(Screen::Boards, w, cx)))
            .on_action(cx.listener(|this, _: &GoWallet, w, cx| this.go(Screen::Wallet, w, cx)))
            .on_action(cx.listener(|this, _: &GoTags, w, cx| this.go(Screen::Tags, w, cx)))
            .on_action(cx.listener(|this, _: &GoChat, w, cx| this.go(Screen::Chat, w, cx)))
            .on_action(cx.listener(|this, _: &PrevAccount, w, cx| this.step_account(w, cx)))
            .on_action(cx.listener(|this, _: &NextAccount, w, cx| this.step_account(w, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.refresh(cx)))
            .on_action(cx.listener(|this, _: &NewItem, w, cx| this.new_item(w, cx)))
            .on_action(cx.listener(|this, _: &CloseForm, w, cx| this.close_forms(w, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.select_step(-1, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.select_step(1, cx)))
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
            .child(
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
                    ),
            )
    }
}
