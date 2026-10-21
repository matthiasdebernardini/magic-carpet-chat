//! The "Your Trusted Community" panel — the in-app version of Brainstorm's
//! onboarding: follow a well-known account, ask Brainstorm to score the web
//! of trust, then sign and publish the kind-10040 Treasure Map built from
//! its `/setup` rows.
//!
//! One render covers two homes: the account screen's step card (under the
//! ready card) and the Trust tab's "Not located" Treasure Map card. State
//! lives in `Shell::community`, one entry per pubkey; every reply arrives
//! as `Update::Community` stamped with the panel's generation, and
//! [`accepts`] drops a late one. All the network work is the runtime's —
//! this file only sends Commands and renders Updates.

use std::time::Duration;

use gpui_kit::component::{
    Sizable as _, Size, h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use magic_carpet_chat::brainstorm::SearchHit;
use magic_carpet_chat::nostr::{self, Command, CommunityPart, FollowErr, FollowOk, SetupState};
use magic_carpet_chat::trust::Designation;
use nostr_sdk::prelude::ToBech32 as _;

use crate::account_setup::quiet_button;
use crate::dashboard::{MONO, avatar, muted};
use crate::palette::*;
use crate::shell::{Shell, short_npub};
use crate::timefmt;

// --------------------------------------------------------------- the state

/// The calculate step's progress; `Done` carries what the `/setup` probe
/// then saw — `Ready` alone enables step 3.
#[derive(Debug, Clone)]
pub(crate) enum Calc {
    Idle,
    Busy,
    Done(SetupState),
    Failed(String),
}

/// The publish step's progress. A success needs no `Done` to hold: the
/// re-read it kicks off turns the map card "Located" by itself.
#[derive(Debug, Clone)]
pub(crate) enum Map {
    Idle,
    Busy,
    Failed(String),
}

/// One panel's state, keyed by pubkey in `Shell::community`. `generation`
/// is assigned at creation (monotonic across a remove/re-add) and stamped
/// on every command; a reply stamped older is dropped by [`accepts`].
pub(crate) struct CommunityState {
    pub generation: u32,
    /// The profile-search input (created with the panel — it needs a
    /// window).
    pub search: Entity<InputState>,
    /// The latest answered search; `None` for "not asked" (and after a
    /// follow clears the box).
    pub search_results: Option<Result<Vec<SearchHit>, String>>,
    /// A search is in flight.
    pub searching: bool,
    /// Bumped per `SearchProfiles` sent; replies carry it back, stale seqs
    /// drop.
    pub search_seq: u64,
    /// The query the in-flight search answers — the "no match" line's `q`.
    pub last_query: String,
    /// The 300 ms debounce task; replaced per keystroke, cancelled on drop.
    pub debounce: Option<Task<()>>,
    /// Follows made this session, the chips: (hex pubkey, label).
    pub follows: Vec<(String, String)>,
    /// The follow in flight: (hex, label). `None` means the row is idle.
    pub pending: Option<(String, String)>,
    /// The last follow's failure, fixed text.
    pub follow_error: Option<String>,
    /// Relays proved "no list" for a key not minted here: the parked
    /// (target, label) until the person confirms or cancels.
    pub confirm_first: Option<(String, String)>,
    /// "Saved to N of M relays" under the chips: (plain lead, accepted
    /// hosts, refused hosts) — the hosts sit behind the "show relays" toggle.
    pub saved_line: Option<(String, String, String)>,
    /// Whether the saved line's hostnames are expanded.
    pub show_relays: bool,
    /// Whether the search box holds focus — draws the accent border.
    pub search_focused: bool,
    pub calc: Calc,
    pub map: Map,
    /// Keeps the search-input subscription alive.
    _sub: Subscription,
}

/// The stale-reply guard: the update belongs to `pubkey`'s panel only while
/// that panel exists (`panel` is its generation) and a newer one has not
/// taken over. A reply for account A never reaches B — `panel` is looked up
/// BY `pubkey` — and a remove/re-add's fresh generation out-ranks the old
/// life's answers.
pub(crate) fn accepts(panel: Option<u32>, generation: u32) -> bool {
    panel.is_some_and(|current| generation >= current)
}

/// "✓ Saved to N of M relays" plus the accepting/refusing hostnames — the
/// lead is plain language; the hosts sit behind the "show relays" toggle.
fn saved_line(done: &FollowOk) -> (String, String, String) {
    if done.already {
        return (
            "Already following. Nothing to save.".to_string(),
            String::new(),
            String::new(),
        );
    }
    let hosts: Vec<String> = done.accepted.iter().map(|url| relay_host(url)).collect();
    let refused: Vec<String> = done.rejected.iter().map(|url| relay_host(url)).collect();
    (
        format!(
            "Saved on {} of {} {}",
            done.accepted.len(),
            done.asked,
            if done.asked == 1 {
                "Nostr server"
            } else {
                "Nostr servers"
            }
        ),
        hosts.join(", "),
        refused.join(", "),
    )
}

fn relay_host(url: &str) -> String {
    url.trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

/// The red line under step 1 — fixed wording, counts where they carry
/// meaning. `NeedsConfirmation` never comes here: it parks in
/// `confirm_first` and renders as the amber box.
fn follow_error_text(error: &FollowErr) -> String {
    match error {
        FollowErr::Inconclusive { answered, asked } => format!(
            "Couldn't check your follow list: {answered} of {asked} Nostr servers answered. Nothing was saved. Try again in a moment."
        ),
        FollowErr::Rejected { asked } => {
            format!(
                "No Nostr server accepted your follow list (0 of {asked}). Nothing changed. Try again."
            )
        }
        FollowErr::Failed(message) => message.to_string(),
        FollowErr::NeedsConfirmation { .. } => String::new(),
    }
}

/// A hit's label: display name, else name, else a shortened npub.
fn hit_label(hit: &SearchHit) -> String {
    hit.display_name
        .as_deref()
        .or(hit.name.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| {
            nostr_sdk::prelude::PublicKey::from_hex(&hit.pubkey)
                .ok()
                .and_then(|pk| pk.to_bech32().ok())
                .map(|npub| short_npub(&npub))
                .unwrap_or_else(|| short_npub(&hit.pubkey))
        })
}

/// A stable hue per pubkey for the initial-tile avatar (no remote images).
fn hit_hue(pubkey: &str) -> (u32, u32) {
    const HUES: [(u32, u32); 5] = [
        (ACCENT, ACCENT_DEEP),
        (HUE_BO_FROM, HUE_BO_TO),
        (GREEN, GREEN_DIM),
        (AMBER, AMBER_DIM),
        (HUE_AK_FROM, HUE_AK_TO),
    ];
    let n = pubkey.bytes().fold(0usize, |acc, b| acc.wrapping_add(b as usize));
    HUES[n % HUES.len()]
}

// ----------------------------------------------------------- Shell methods

impl Shell {
    /// The panel state for `pubkey`, created on first need — the search
    /// input wants a window. Every creation also kicks off the one-time
    /// owner fetch, so the recommended row has a name to show.
    pub(crate) fn ensure_community(
        &mut self,
        pubkey: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.community.contains_key(pubkey) {
            return;
        }
        self.community_generation = self.community_generation.saturating_add(1).max(1);
        let key = pubkey.to_string();
        let search = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Search profiles to follow…");
            input.set_editor_style(gpui_kit::base::input::InputEditorStyle {
                muted_foreground: rgb(TEXT_DIM).into(),
                foreground: rgb(TEXT).into(),
                ..Default::default()
            });
            input
        });
        let sub = cx.subscribe_in(
            &search,
            window,
            move |this: &mut Shell, _, event, _window, cx| {
                match event {
                    InputEvent::Change => this.community_search_changed(&key, cx),
                    InputEvent::Focus | InputEvent::Blur => {
                        if let Some(state) = this.community.get_mut(&key) {
                            state.search_focused = matches!(event, InputEvent::Focus);
                        }
                        cx.notify();
                    }
                    _ => {}
                }
            },
        );
        self.community.insert(
            pubkey.to_string(),
            CommunityState {
                generation: self.community_generation,
                search,
                search_results: None,
                searching: false,
                search_seq: 0,
                last_query: String::new(),
                debounce: None,
                follows: Vec::new(),
                pending: None,
                follow_error: None,
                confirm_first: None,
                saved_line: None,
                show_relays: false,
                search_focused: false,
                calc: Calc::Idle,
                map: Map::Idle,
                _sub: sub,
            },
        );
        if !self.owner_requested {
            self.owner_requested = true;
            let _ = self.commands.unbounded_send(Command::FetchOwner);
        }
    }

    /// A keystroke in the panel's search box: debounce 300 ms, then send
    /// `SearchProfiles`. Under two trimmed characters asks nothing.
    fn community_search_changed(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let Some(state) = self.community.get_mut(pubkey) else {
            return;
        };
        let query = state.search.read(cx).value().trim().to_string();
        if query.len() < 2 {
            state.debounce = None;
            state.search_results = None;
            state.searching = false;
            cx.notify();
            return;
        }
        let key = pubkey.to_string();
        state.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = this.update(cx, |shell, cx| {
                let Some(state) = shell.community.get_mut(&key) else {
                    return;
                };
                let query = state.search.read(cx).value().trim().to_string();
                if query.len() < 2 {
                    return;
                }
                state.search_seq += 1;
                state.searching = true;
                state.last_query = query.clone();
                let _ = shell.commands.unbounded_send(Command::SearchProfiles {
                    pubkey: key.clone(),
                    generation: state.generation,
                    seq: state.search_seq,
                    query,
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// A "Follow" click: the target is in flight until `Followed` lands.
    /// `allow_first_list` is only set by the amber box's "Start a new
    /// follow list".
    pub(crate) fn community_follow(
        &mut self,
        pubkey: &str,
        target: String,
        label: String,
        allow_first_list: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.community.get_mut(pubkey) else {
            return;
        };
        if state.pending.is_some() {
            return;
        }
        state.pending = Some((target.clone(), label));
        state.follow_error = None;
        state.confirm_first = None;
        let _ = self.commands.unbounded_send(Command::Follow {
            pubkey: pubkey.to_string(),
            generation: state.generation,
            target,
            allow_first_list,
        });
        cx.notify();
    }

    /// The amber box's "Start a new follow list": re-send the parked target
    /// with consent.
    pub(crate) fn community_first_list(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let Some((target, label)) = self
            .community
            .get(pubkey)
            .and_then(|state| state.confirm_first.clone())
        else {
            return;
        };
        self.community_follow(pubkey, target, label, true, cx);
    }

    /// The amber box's "Cancel": drop the parked target untouched.
    pub(crate) fn community_first_list_cancel(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        if let Some(state) = self.community.get_mut(pubkey) {
            state.confirm_first = None;
        }
        cx.notify();
    }

    /// "Calculate my trust scores" — the UI dims it until ≥1 follow.
    pub(crate) fn community_calculate(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let Some(state) = self.community.get_mut(pubkey) else {
            return;
        };
        if matches!(state.calc, Calc::Busy) {
            return;
        }
        state.calc = Calc::Busy;
        let _ = self.commands.unbounded_send(Command::CalculateTrust {
            pubkey: pubkey.to_string(),
            generation: state.generation,
        });
        cx.notify();
    }

    /// "Try again" on a missing setup answer — a plain `/setup` GET, never
    /// the rate-limited POSTs.
    pub(crate) fn community_check_setup(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let Some(state) = self.community.get(pubkey) else {
            return;
        };
        let _ = self.commands.unbounded_send(Command::CheckSetup {
            pubkey: pubkey.to_string(),
            generation: state.generation,
        });
        cx.notify();
    }

    /// "Publish my Treasure Map" — enabled only on a `Ready` setup answer;
    /// the runtime re-checks before it signs.
    pub(crate) fn community_publish_map(&mut self, pubkey: &str, cx: &mut Context<Self>) {
        let Some(state) = self.community.get_mut(pubkey) else {
            return;
        };
        if !matches!(state.calc, Calc::Done(SetupState::Ready { .. }))
            || matches!(state.map, Map::Busy)
        {
            return;
        }
        state.map = Map::Busy;
        let _ = self.commands.unbounded_send(Command::PublishMap {
            pubkey: pubkey.to_string(),
            generation: state.generation,
        });
        cx.notify();
    }

    /// Fold a `Community` part into the panel it answers. Stale
    /// generations and panels that no longer exist are dropped untouched.
    pub(crate) fn apply_community(
        &mut self,
        pubkey: String,
        generation: u32,
        part: CommunityPart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.community.get(&pubkey).map(|state| state.generation);
        if !accepts(panel, generation) {
            return;
        }
        match part {
            CommunityPart::SearchResults { seq, result } => {
                let Some(state) = self.community.get_mut(&pubkey) else {
                    return;
                };
                // A reply to a query the box has already replaced is stale.
                if seq != state.search_seq {
                    return;
                }
                state.searching = false;
                state.search_results = Some(result);
            }
            CommunityPart::Followed(result) => {
                let mut refresh = false;
                if let Some(state) = self.community.get_mut(&pubkey) {
                    let pending = state.pending.take();
                    match result {
                        Ok(done) => {
                            let label = pending
                                .map(|(_, label)| label)
                                .unwrap_or_else(|| short_npub(&done.target));
                            if !state.follows.iter().any(|(p, _)| *p == done.target) {
                                state.follows.push((done.target.clone(), label));
                            }
                            state.confirm_first = None;
                            state.follow_error = None;
                            state.saved_line = Some(saved_line(&done));
                            // Following a hit clears the query (the mock's
                            // behaviour); the chips keep the record.
                            state
                                .search
                                .update(cx, |input, cx| input.set_value("", window, cx));
                            state.search_results = None;
                            refresh = true;
                        }
                        Err(FollowErr::NeedsConfirmation { .. }) => match pending {
                            Some(parked) => state.confirm_first = Some(parked),
                            None => {
                                state.follow_error = Some("Couldn't follow. Try again.".into());
                            }
                        },
                        Err(error) => {
                            state.follow_error = Some(follow_error_text(&error));
                        }
                    }
                }
                // A follow changes the dashboard's "Follow someone" row; the
                // panel renders from `designation.last`, so the re-read
                // leaves the steps standing.
                if refresh {
                    self.fetch_trust(&pubkey);
                }
            }
            CommunityPart::Calculated(result) => {
                if let Some(state) = self.community.get_mut(&pubkey) {
                    state.calc = match result {
                        Ok(setup) => Calc::Done(setup),
                        Err(message) => Calc::Failed(message),
                    };
                }
            }
            CommunityPart::SetupChecked(setup) => {
                // The GET probes the same gate Calculate's probe set; fold
                // it into Done so step 3 reacts.
                if let Some(state) = self.community.get_mut(&pubkey) {
                    state.calc = Calc::Done(setup);
                }
            }
            CommunityPart::MapPublished(result) => {
                let published = result.is_ok();
                if let Some(state) = self.community.get_mut(&pubkey) {
                    state.map = match result {
                        Ok(_) => Map::Idle,
                        Err(message) => Map::Failed(message),
                    };
                }
                if published {
                    self.push_activity(
                        GREEN,
                        "Published your Treasure Map".to_string(),
                        timefmt::now_unix(),
                    );
                    // The Trust card turns "Located" and the dashboard row
                    // clears on its own — one re-read.
                    self.fetch_trust(&pubkey);
                }
            }
        }
    }
}

// --------------------------------------------------------------- the pieces

fn panel_card() -> Div {
    v_flex()
        .w_full()
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER_3))
        .rounded(px(14.))
        .px(px(22.))
        .py(px(20.))
        .gap(px(10.))
}

/// The round mark — 28px beside the card title, 22px beside each step.
/// `bg` is read as RRGGBBAA when it carries alpha, RGB when it does not.
pub(crate) fn badge(size: f32, mark: &'static str, bg: u32, fg: u32) -> Div {
    div()
        .size(px(size))
        .rounded_full()
        .flex_shrink_0()
        .bg(if bg > 0xff_ff_ff {
            wash(bg)
        } else {
            rgb(bg).into()
        })
        .flex()
        .items_center()
        .justify_center()
        .font_family(MONO)
        .text_size(px(if size > 24. { 13. } else { 11. }))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(fg))
        .child(mark)
}

/// The lighter inner-step mark — a ring, not a fill, so it never reads as a
/// second copy of the card's solid 28px badge.
fn ring_badge(size: f32, mark: &'static str, width: f32, edge: u32, fg: u32) -> Div {
    let mut el = div()
        .size(px(size))
        .rounded_full()
        .flex_shrink_0()
        .border_color(rgb(edge))
        .flex()
        .items_center()
        .justify_center()
        .font_family(MONO)
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(fg))
        .child(mark);
    let width = Some(AbsoluteLength::from(px(width)));
    let widths = &mut el.style().border_widths;
    widths.top = width;
    widths.bottom = width;
    widths.left = width;
    widths.right = width;
    el
}

/// A coloured note with a leading mark — the green ✓, amber !, purple i.
fn note_box(
    mark: &'static str,
    mark_color: u32,
    fill: u32,
    edge: u32,
    text_color: u32,
    text: impl IntoElement,
) -> Div {
    h_flex()
        .gap(px(10.))
        .items_start()
        .bg(wash(fill))
        .border_1()
        .border_color(wash(edge))
        .rounded(px(9.))
        .px(px(13.))
        .py(px(11.))
        .text_size(px(12.5))
        .text_color(rgb(text_color))
        .child(
            div()
                .flex_shrink_0()
                .text_color(rgb(mark_color))
                .font_weight(FontWeight::BOLD)
                .child(mark),
        )
        .child(div().flex_1().min_w(px(0.)).child(text))
}

/// The row's "Follow" button — solid accent, dark text; `busy` shows the
/// spinner and swallows the click; `done` is the green-tinted "✓ Following".
fn follow_button(
    id: &'static str,
    busy: bool,
    done: bool,
    cx: &mut Context<Shell>,
    on_click: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .flex_shrink_0()
        .px(px(12.))
        .py(px(4.))
        .rounded(px(7.))
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD);
    let button = if done {
        button
            .bg(wash(GREEN_WASH))
            .border_1()
            .border_color(rgb(GREEN_EDGE))
            .text_color(rgb(GREEN))
    } else {
        button.bg(rgb(ACCENT)).text_color(rgb(BG_RAIL))
    };
    let button = if busy || done {
        button
    } else {
        button
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    };
    if busy {
        // The accent spinner needs a dark ground — the enabled fill IS the
        // accent, so the busy button drops to the dim fill.
        button
            .bg(rgb(BG_NAV_ON))
            .child(Spinner::new().small().color(rgb(ACCENT).into()))
    } else {
        button.child(if done { "✓ Following" } else { "Follow" })
    }
}

/// A step's title and its dim suffix as ONE wrapping paragraph — a
/// StyledText with a heavy run then a muted run, so the suffix flows
/// on the same line instead of dropping under. BOLD not SEMIBOLD: the
/// UI font here has no 600 face, so semibold resolves to regular.
fn step_title(title: &'static str, suffix: &'static str) -> StyledText {
    let full = format!("{title}{suffix}");
    let title_end = title.len();
    let mut marks = vec![(
        0..title_end,
        HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        },
    )];
    if title_end < full.len() {
        marks.push((
            title_end..full.len(),
            HighlightStyle {
                font_weight: Some(FontWeight::NORMAL),
                color: Some(rgb(TEXT_MUTED).into()),
                ..Default::default()
            },
        ));
    }
    StyledText::new(SharedString::from(full)).with_highlights(marks)
}

/// One step row: badge, title (with optional dim suffix), then the body.
fn step(
    number: u8,
    done: bool,
    dimmed: bool,
    title: &'static str,
    suffix: &'static str,
    body: impl IntoElement,
) -> Div {
    let num = match number {
        1 => "1",
        2 => "2",
        _ => "3",
    };
    // Active = solid accent with dark text; done = green tint + green ✓;
    // pending = the dim fill and dim glyph.
    let badge_el = if done {
        badge(22., "✓", 0x4ecb8d22, GREEN)
    } else if dimmed {
        ring_badge(22., num, 1., BORDER_3, TEXT_DIM)
    } else {
        ring_badge(22., num, 1.5, ACCENT, ACCENT_LIGHT)
    };
    h_flex()
        .gap(px(11.))
        .items_start()
        .child(
            div()
                .mt(px(1.))
                .when(dimmed, |this| this.opacity(0.55))
                .child(badge_el),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .gap(px(8.))
                .child(
                    div()
                        .when(dimmed, |this| this.opacity(0.55))
                        .text_size(px(13.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(step_title(title, suffix)),
                )
                .child(body),
        )
}

// ------------------------------------------------------------- the render

/// The three steps and the skip line — the panel body shared by the setup
/// card and the Trust tab's "Not located" map card.
pub(crate) fn steps(shell: &Shell, pubkey: &str, cx: &mut Context<Shell>) -> AnyElement {
    let Some(state) = shell.community.get(pubkey) else {
        return muted("Loading setup steps…", TEXT_MUTED);
    };
    let followed_now = !state.follows.is_empty();
    // Step 2 also unlocks when Feature 1's read already counts follows.
    let has_follows = followed_now
        || matches!(
            shell.trust.get(pubkey).map(|t| t.follows.last),
            Some(Some(Some(n))) if n > 0
        );
    let step1_done = followed_now || has_follows;

    // ------------------------------------------------------------ step 1

    let owner_row = if nostr::is_issuer(pubkey) {
        // The issuer is the recommended follow — it doesn't follow itself.
        None
    } else {
        let (hex, npub) = match &shell.owner {
            Some((hex, npub, _)) => (hex.clone(), npub.clone()),
            None => {
                let hex = nostr::issuer_pubkey();
                let npub = nostr_sdk::prelude::PublicKey::from_hex(&hex)
                    .ok()
                    .and_then(|pk| pk.to_bech32().ok())
                    .unwrap_or_else(|| hex.clone());
                (hex, npub)
            }
        };
        let name = shell
            .owner
            .as_ref()
            .and_then(|(_, _, name)| name.clone())
            .unwrap_or_else(|| "Magic Carpet owner".to_string());
        let owner_followed = state.follows.iter().any(|(p, _)| *p == hex);
        let owner_busy = state
            .pending
            .as_ref()
            .is_some_and(|(target, _)| *target == hex);
        let target = hex.clone();
        let label = name.clone();
        let key = pubkey.to_string();
        Some(
            h_flex()
                .items_center()
                .gap(px(10.))
                .bg(rgb(BG_APP))
                .border_1()
                .border_color(rgb(BORDER_2))
                .rounded(px(9.))
                .px(px(12.))
                .py(px(8.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .gap(px(1.))
                        .child(
                            h_flex()
                                .gap(px(4.))
                                .items_center()
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(12.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(SharedString::from(name)),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .px(px(7.))
                                        .py(px(1.))
                                        .rounded_full()
                                        .bg(wash(0x8f7df814))
                                        .border_1()
                                        .border_color(wash(0x8f7df833))
                                        .text_color(rgb(ACCENT_LIGHT))
                                        .text_size(px(9.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("RECOMMENDED — YOUR OWNER NPUB"),
                                ),
                        )
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(10.5))
                                .text_color(rgb(TEXT_DIM))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(SharedString::from(npub)),
                        ),
                )
                .child(follow_button(
                    "community-follow-owner",
                    owner_busy,
                    owner_followed,
                    cx,
                    move |this, _, cx| {
                        this.community_follow(&key, target.clone(), label.clone(), false, cx);
                    },
                )),
        )
    };

    let search_box = div()
        .w_full()
        .h(px(36.))
        .flex()
        .items_center()
        .bg(rgb(BG_APP))
        .border_1()
        .border_color(rgb(if state.search_focused {
            ACCENT
        } else {
            BORDER_2
        }))
        .rounded(px(9.))
        .px(px(12.))
        .text_size(px(12.5))
        .child(
            Input::new(&state.search)
                .with_size(Size::Small)
                .appearance(false)
                .pl(px(0.))
                .w_full(),
        );

    let results: Option<AnyElement> = match &state.search_results {
        Some(Ok(hits)) => {
            let shown: Vec<AnyElement> = hits
                .iter()
                .filter(|hit| !state.follows.iter().any(|(p, _)| *p == hit.pubkey))
                .take(5)
                .map(|hit| {
                    let label = hit_label(hit);
                    let busy = state
                        .pending
                        .as_ref()
                        .is_some_and(|(target, _)| *target == hit.pubkey);
                    let target = hit.pubkey.clone();
                    let label_clone = label.clone();
                    let key = pubkey.to_string();
                    // NIP-05 "_@domain" is the root identifier: show "domain".
                    let hint = hit
                        .nip05
                        .as_deref()
                        .map(|value| value.strip_prefix("_@").unwrap_or(value).to_string())
                        .unwrap_or_default();
                    h_flex()
                        .items_center()
                        .gap(px(10.))
                        .bg(rgb(BG_APP))
                        .border_1()
                        .border_color(rgb(BORDER_2))
                        .rounded(px(9.))
                        .px(px(12.))
                        .py(px(7.))
                        .child(avatar(
                            label.chars().next().unwrap_or('?').to_uppercase().to_string(),
                            hit_hue(&hit.pubkey),
                            20.,
                            8.5,
                        ))
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w(px(0.))
                                .gap(px(6.))
                                .items_baseline()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(label))
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::NORMAL)
                                        .text_color(rgb(TEXT_DIM))
                                        .child(SharedString::from(hint)),
                                ),
                        )
                        .child(follow_button(
                            "community-follow-hit",
                            busy,
                            false,
                            cx,
                            move |this, _, cx| {
                                this.community_follow(
                                    &key,
                                    target.clone(),
                                    label_clone.clone(),
                                    false,
                                    cx,
                                );
                            },
                        ))
                        .into_any_element()
                })
                .collect();
            if shown.is_empty() && !state.searching {
                Some(muted(
                    format!(
                        "No profiles match \u{201c}{}\u{201d}. Try another name.",
                        state.last_query
                    ),
                    TEXT_MUTED,
                ))
            } else {
                Some(v_flex().gap(px(6.)).children(shown).into_any_element())
            }
        }
        Some(Err(_)) => Some(muted("Search is down. Try again in a moment.", TEXT_MUTED)),
        None if state.searching => Some(
            h_flex()
                .gap(px(9.))
                .items_center()
                .child(Spinner::new().small().color(rgb(ACCENT).into()))
                .child(div().text_size(px(12.5)).text_color(rgb(TEXT_MUTED)).child("Searching…"))
                .into_any_element(),
        ),
        None => None,
    };

    let chips: Vec<AnyElement> = state
        .follows
        .iter()
        .map(|(_, label)| {
            h_flex()
                .items_center()
                .gap(px(6.))
                .bg(wash(0x4ecb8d12))
                .border_1()
                .border_color(wash(0x4ecb8d33))
                .text_color(rgb(0xb7e9d0))
                .rounded_full()
                .px(px(11.))
                .py(px(3.))
                .text_size(px(11.5))
                .font_weight(FontWeight::MEDIUM)
                .child(div().text_color(rgb(GREEN)).child("✓"))
                .child(SharedString::from(label.clone()))
                .into_any_element()
        })
        .collect();

    let confirm_box = state.confirm_first.as_ref().map(|_| {
        let key = pubkey.to_string();
        let key2 = pubkey.to_string();
        v_flex()
            .gap(px(8.))
            .bg(wash(0xf2b54412))
            .border_1()
            .border_color(wash(0xf2b54433))
            .rounded(px(9.))
            .px(px(13.))
            .py(px(11.))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(0xe8d9b0))
                    .child("No follow list was found for this key. If you have followed people with it before, cancel: a new list replaces the old one. If not, start a new follow list."),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("community-first-list")
                            .px(px(16.))
                            .py(px(7.))
                            .rounded(px(9.))
                            .bg(rgb(AMBER))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(BG_RAIL))
                            .child("Start a new follow list")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.community_first_list(&key, cx);
                            })),
                    )
                    .child(
                        div()
                            .id("community-first-list-cancel")
                            .px(px(16.))
                            .py(px(7.))
                            .rounded(px(9.))
                            .border_1()
                            .border_color(rgb(BORDER_3))
                            .cursor_pointer()
                            .text_size(px(12.5))
                            .text_color(rgb(TEXT))
                            .child("Cancel")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.community_first_list_cancel(&key2, cx);
                            })),
                    ),
            )
            .into_any_element()
    });

    // A list the trust read already counted satisfies step 1 on its own —
    // say so instead of leaving the step looking untouched.
    let existing_follows = shell
        .trust
        .get(pubkey)
        .and_then(|t| t.follows.last)
        .flatten()
        .unwrap_or(0);
    let already_line = (existing_follows > 0 && state.follows.is_empty()).then(|| {
        div()
            .text_size(px(12.5))
            .text_color(rgb(TEXT_MUTED))
            .child(format!(
                "You already follow {existing_follows} account{}. Go to step 2.",
                if existing_follows == 1 { "" } else { "s" }
            ))
            .into_any_element()
    });

    let step1 = step(
        1,
        step1_done,
        false,
        "Follow at least one well-known account",
        "",
        v_flex()
            .gap(px(8.))
            .children(already_line)
            .children(owner_row)
            .child(search_box)
            .children(results)
            .when(!chips.is_empty(), |this| {
                this.child(h_flex().gap(px(6.)).flex_wrap().children(chips))
            })
            .children(state.saved_line.as_ref().map(|(lead, hosts, refused)| {
                let saved_key = pubkey.to_string();
                let has_hosts = !hosts.is_empty() || !refused.is_empty();
                let mut column = v_flex().gap(px(4.)).child(
                    h_flex()
                        .items_baseline()
                        .gap(px(6.))
                        .text_size(px(12.))
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(rgb(GREEN))
                                .font_weight(FontWeight::BOLD)
                                .child("✓"),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(rgb(TEXT_MUTED))
                                .child(SharedString::from(lead.clone())),
                        )
                        .when(has_hosts, |this| {
                            this.child(
                                div()
                                    .id("community-show-relays")
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .text_size(px(11.))
                                    .text_color(rgb(TEXT_DIM))
                                    .child(if state.show_relays {
                                        "· hide servers"
                                    } else {
                                        "· show servers"
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(state) =
                                            this.community.get_mut(&saved_key)
                                        {
                                            state.show_relays = !state.show_relays;
                                        }
                                        cx.notify();
                                    })),
                            )
                        }),
                );
                if state.show_relays && has_hosts {
                    if !hosts.is_empty() {
                        column = column.child(
                            div()
                                .pl(px(18.))
                                .font_family(MONO)
                                .text_size(px(11.))
                                .text_color(rgb(TEXT_DIM))
                                .child(SharedString::from(hosts.clone())),
                        );
                    }
                    if !refused.is_empty() {
                        column = column.child(
                            div()
                                .pl(px(18.))
                                .font_family(MONO)
                                .text_size(px(11.))
                                .text_color(rgb(TEXT_DIM))
                                .child(SharedString::from(format!("Not saved on: {refused}"))),
                        );
                    }
                }
                column.into_any_element()
            }))
            .children(confirm_box)
            .children(state.follow_error.as_ref().map(|error| {
                div()
                    .text_size(px(12.))
                    .text_color(rgb(RED))
                    .child(SharedString::from(error.clone()))
                    .into_any_element()
            })),
    );

    // ------------------------------------------------------------ step 2

    let key = pubkey.to_string();
    let calc_body: AnyElement = match &state.calc {
        Calc::Busy => h_flex()
            .gap(px(9.))
            .items_center()
            .child(Spinner::new().small().color(rgb(ACCENT).into()))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_MUTED))
                    .child("Asking the Brainstorm API… verifying the request was received"),
            )
            .into_any_element(),
        Calc::Done(_) => div()
            .text_size(px(12.5))
            .text_color(rgb(0xb7e9d0))
            .child(
                h_flex()
                    .gap(px(4.))
                    .items_start()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(GREEN))
                            .font_weight(FontWeight::BOLD)
                            .child("✓"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child("Brainstorm confirmed — your community's trust scores are in the process of being calculated and published (may take up to an hour if being done for the first time)."),
                    ),
            )
            .into_any_element(),
        Calc::Failed(message) => v_flex()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(AMBER))
                    .child(SharedString::from(message.clone())),
            )
            .child(calc_button("community-calc-retry", has_follows, cx, {
                let key = key.clone();
                move |this, _, cx| this.community_calculate(&key, cx)
            }))
            .into_any_element(),
        Calc::Idle => {
            let mut column = v_flex().gap(px(8.));
            if !has_follows {
                column = column.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_DIM))
                        .child("Follow an account first."),
                );
            }
            column
                .child(calc_button("community-calc", has_follows, cx, move |this, _, cx| {
                    this.community_calculate(&key, cx)
                }))
                .into_any_element()
        }
    };

    let step2 = step(
        2,
        matches!(state.calc, Calc::Done(_)),
        !has_follows,
        "Ask Brainstorm to calculate your community's trust scores",
        " — it's free and easy!",
        calc_body,
    );

    // ------------------------------------------------------------ step 3

    let ready = matches!(state.calc, Calc::Done(SetupState::Ready { .. }));
    let key = pubkey.to_string();
    let map_body: AnyElement = match &state.map {
        Map::Busy => h_flex()
            .gap(px(9.))
            .items_center()
            .child(Spinner::new().small().color(rgb(ACCENT).into()))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_MUTED))
                    .child("Fetching tag data from api.brainstorm.world — rank provider pubkey, relay, and scores (rank, followers, hops…) — then signing and publishing…"),
            )
            .into_any_element(),
        _ => {
            let key2 = key.clone();
            let mut column = v_flex().gap(px(8.));
            if !ready {
                column = column.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_DIM))
                        .child("Finish step 2 first."),
                );
            }
            column = column.child(map_button("community-map", ready, cx, move |this, _, cx| {
                this.community_publish_map(&key2, cx)
            }));
            match &state.calc {
                Calc::Done(SetupState::Missing) => {
                    column = column
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(TEXT_MUTED))
                                .child("Brainstorm has no data for this account yet. The first calculation can take an hour. Try again later."),
                        )
                        .child(check_setup_link(pubkey, cx));
                }
                Calc::Done(SetupState::Unreadable) => {
                    column = column
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(TEXT_MUTED))
                                .child("Couldn't read the setup data. Try again."),
                        )
                        .child(check_setup_link(pubkey, cx));
                }
                _ => {}
            }
            if let Map::Failed(message) = &state.map {
                column = column.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(RED))
                        .child(SharedString::from(message.clone())),
                );
            }
            column.into_any_element()
        }
    };

    // Publishing flips the whole card to "calculated" on the re-read, so
    // the step itself never carries a done mark.
    let step3 = step(
        3,
        false,
        !ready && !matches!(state.map, Map::Busy),
        "Publish your Treasure Map",
        " — Magic Carpet builds it from api.brainstorm.world data and signs it locally",
        map_body,
    );

    v_flex()
        .gap(px(14.))
        .child(step1)
        .child(step2)
        .child(step3)
        .child(skip_line(cx))
        .into_any_element()
}

/// The card's way out — auto-width, outline for the "later" wording and a
/// gradient once the map is located.
pub(crate) fn dashboard_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .self_start()
        .px(px(22.))
        .py(px(12.))
        .rounded(px(10.))
        .cursor_pointer()
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD);
    let button = if primary {
        button.bg(grad(ACCENT, ACCENT_DEEP)).text_color(rgb(BG_RAIL))
    } else {
        button
            .border_1()
            .border_color(rgb(BORDER_3))
            .text_color(rgb(TEXT))
    };
    button.child(label)
}

fn calc_button(
    id: &'static str,
    enabled: bool,
    cx: &mut Context<Shell>,
    on_click: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .self_start()
        .px(px(16.))
        .py(px(9.))
        .rounded(px(9.))
        .bg(if enabled {
            grad(ACCENT, ACCENT_DEEP)
        } else {
            rgb(BG_NAV_ON).into()
        })
        .text_size(px(12.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if enabled {
            rgb(BG_RAIL)
        } else {
            rgb(TEXT_MUTED)
        });
    if enabled {
        button
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    } else {
        button.cursor_not_allowed()
    }
    .child("Calculate my trust scores")
}

fn map_button(
    id: &'static str,
    enabled: bool,
    cx: &mut Context<Shell>,
    on_click: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> Stateful<Div> {
    let button = div()
        .id(id)
        .self_start()
        .px(px(16.))
        .py(px(9.))
        .rounded(px(9.))
        .bg(if enabled {
            grad(ACCENT, ACCENT_DEEP)
        } else {
            rgb(BG_NAV_ON).into()
        })
        .text_size(px(12.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if enabled {
            rgb(BG_RAIL)
        } else {
            rgb(TEXT_MUTED)
        });
    if enabled {
        button
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
    } else {
        button.cursor_not_allowed()
    }
    .child("Publish my Treasure Map")
}

/// The "Try again" link under a not-yet-ready setup answer — a `CheckSetup`
/// command, never the rate-limited POSTs.
fn check_setup_link(pubkey: &str, cx: &mut Context<Shell>) -> Stateful<Div> {
    let key = pubkey.to_string();
    div()
        .id("community-check-setup")
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(rgb(ACCENT_LIGHT))
        .child("Try again")
        .on_click(cx.listener(move |this, _, _, cx| {
            this.community_check_setup(&key, cx);
        }))
}

/// The dim skip line under the three steps — one flowing text with an
/// underlined, clickable brainstorm.world range (period hugging the link).
fn skip_line(_cx: &mut Context<Shell>) -> AnyElement {
    let text = "You can skip this step and do it yourself later at brainstorm.world.";
    let link = text.find("brainstorm.world").unwrap()..text.len() - 1;
    div()
        .text_size(px(12.))
        .text_color(rgb(TEXT_DIM))
        .child(
            InteractiveText::new(
                "community-skip-link",
                StyledText::new(text).with_highlights([(
                    link.clone(),
                    HighlightStyle {
                        color: Some(rgb(ACCENT_LIGHT).into()),
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: Some(rgb(ACCENT_LIGHT).into()),
                            wavy: false,
                        }),
                        ..Default::default()
                    },
                )]),
            )
            .on_click(vec![link], |_, _, cx| {
                cx.open_url("https://brainstorm.world");
            }),
        )
        .into_any_element()
}

/// The account screen's step-3 card (under the ready card). `None` when the
/// account has no panel — the setup screen keeps its own Start button then.
pub(crate) fn card(shell: &Shell, pubkey: &str, cx: &mut Context<Shell>) -> Option<AnyElement> {
    shell.community.get(pubkey)?;

    let trust = shell.trust.get(pubkey);
    let designation = trust.and_then(|state| state.designation.last.as_ref());
    let failed = trust.and_then(|state| state.map_read_failed().map(str::to_string));

    // The card's centre, keyed off the last trust read — never its status
    // alone, so a re-read in flight keeps the steps standing.
    let (body, found) = match designation {
        Some(Designation::Set { .. }) => (
            note_box(
                "✓",
                GREEN,
                0x4ecb8d12,
                0x4ecb8d33,
                0xb7e9d0,
                "Your trusted community has been calculated.",
            )
            .into_any_element(),
            true,
        ),
        Some(Designation::None | Designation::Deactivated { .. }) => (
            v_flex()
                .gap(px(14.))
                .child(note_box(
                    "!",
                    AMBER,
                    0xf2b54412,
                    0xf2b54433,
                    0xe8d9b0,
                    "We cannot find the document (a nostr kind 10040 event) telling us that your community's trust scores have been calculated and are available to use. Good news: you can do the three needed steps right here.",
                ))
                .child(steps(shell, pubkey, cx))
                .into_any_element(),
            false,
        ),
        None if failed.is_some() => {
            let key = pubkey.to_string();
            (
                v_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(TEXT_MUTED))
                            .child(SharedString::from(failed.clone().unwrap_or_default())),
                    )
                    // A failed read proves nothing: no steps until a real
                    // answer lands.
                    .child(quiet_button("community-check-again", "Check again").on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.fetch_trust(&key);
                            cx.notify();
                        }),
                    ))
                    .into_any_element(),
                false,
            )
        }
        None => (
            h_flex()
                .gap(px(10.))
                .items_center()
                .child(Spinner::new().small().color(rgb(ACCENT).into()))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Checking the relays for your kind-10040 event…"),
                )
                .into_any_element(),
            false,
        ),
    };

    // Still checking: the card shows the line alone — the Dashboard button
    // and the i-box wait for a real answer.
    if designation.is_none() && failed.is_none() {
        return Some(
            panel_card()
                .child(
                    h_flex()
                        .items_center()
                        .gap(px(14.))
                        .child(badge(28., "3", ACCENT, BG_RAIL))
                        .child(
                            div()
                                .text_size(px(15.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Your Trusted Community"),
                        ),
                )
                .child(div().mt(px(6.)).ml(px(42.)).child(body))
                .into_any_element(),
        );
    }

    Some(
        panel_card()
            .border_color(if found {
                rgb(BORDER_3).into()
            } else {
                wash(0x8f7df855)
            })
            .child(
                h_flex()
                    .items_center()
                    .gap(px(14.))
                    .child(if found {
                        badge(28., "✓", 0x4ecb8d22, GREEN)
                    } else {
                        badge(28., "3", ACCENT, BG_RAIL)
                    })
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Your Trusted Community"),
                    ),
            )
            .child(
                v_flex()
                    .mt(px(6.))
                    .ml(px(42.))
                    .gap(px(14.))
                    .child(body)
                    .child(note_box(
                        "i",
                        ACCENT_LIGHT,
                        0x8f7df80d,
                        0x8f7df833,
                        0xc9c7d8,
                        {
                            let text = "There may be additional actions to take before this account is fully ready to use — such as topping up your wallet. The Dashboard will list those steps under Needs attention.";
                            let bold = text.find("Needs attention").unwrap()..text.len();
                            StyledText::new(text).with_highlights([(
                                bold,
                                HighlightStyle {
                                    font_weight: Some(FontWeight::BOLD),
                                    color: Some(rgb(TEXT).into()),
                                    ..Default::default()
                                },
                            )])
                        },
                    ))
                    .child(if found {
                        dashboard_button(
                            "community-dashboard",
                            "Take me to the Magic Carpet Dashboard",
                            true,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.leave_account_setup(window, cx);
                        }))
                        .into_any_element()
                    } else {
                        dashboard_button(
                            "community-dashboard",
                            "Take me to the Magic Carpet Dashboard, I\u{2019}ll visit Brainstorm.world later",
                            false,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.leave_account_setup(window, cx);
                        }))
                        .into_any_element()
                    }),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::accepts;

    /// A reply fits a live panel at its generation or newer; an older
    /// generation, or no panel at all, drops it.
    #[test]
    fn community_replies_need_a_live_panel_at_their_generation() {
        for (panel, generation, accepted) in [
            (Some(2), 2, true),
            (Some(2), 3, true),
            (Some(2), 1, false),
            (Some(1), 0, false),
            (Some(1), 1, true),
            (None, 5, false),
        ] {
            assert_eq!(
                accepts(panel, generation),
                accepted,
                "panel {panel:?}, generation {generation}"
            );
        }
    }
}
