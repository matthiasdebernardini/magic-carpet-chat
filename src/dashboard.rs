//! The Dashboard screen: "Needs attention" beside "Recent activity".
//!
//! Every row renders from the shell's live state. "Needs attention" is derived
//! (no account, relay down, list failures) and "Recent activity" is the feed
//! the nostr runtime's updates write, newest first.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{h_flex, v_flex};

use magic_carpet_chat::api;
use magic_carpet_chat::nostr;

use crate::account::Tab;
use crate::palette::*;
use crate::shell::{ActivityItem, Load, Shell, account_hue, display_name, initials};
use crate::timefmt;
use crate::trust_state::MapVerdict;

/// The mock's monospace face, used for every sats figure and every npub.
pub const MONO: &str = "Menlo";

/// One line of small secondary text in `color`.
pub(crate) fn muted(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div()
        .text_size(px(12.5))
        .text_color(rgb(color))
        .child(text.into())
        .into_any_element()
}

/// A round account marker. `size` is the diameter; the initials scale with it.
pub fn avatar(
    initials: impl Into<SharedString>,
    hue: (u32, u32),
    size: f32,
    text: f32,
) -> impl IntoElement {
    div()
        .size(px(size))
        .rounded_full()
        .bg(grad(hue.0, hue.1))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(text))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(BG_RAIL))
        .child(initials.into())
}

/// A count in a soft accent pill — the badge beside "Needs attention".
pub fn count_badge(count: impl Into<SharedString>) -> impl IntoElement {
    div()
        .px(px(7.))
        .py(px(1.))
        .rounded_full()
        .bg(wash(ACCENT_WASH))
        .font_family(MONO)
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(ACCENT_LIGHT))
        .child(count.into())
}

fn card(
    title: &'static str,
    badge: Option<String>,
    rows: Vec<AnyElement>,
) -> impl IntoElement {
    v_flex()
        .flex_1()
        .min_w(px(0.))
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(14.))
        .px(px(20.))
        .py(px(18.))
        .child(
            h_flex()
                .gap(px(8.))
                .items_baseline()
                .mb(px(4.))
                .child(
                    div()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .when_some(badge, |this, badge| this.child(count_badge(badge))),
        )
        .child(v_flex().children(rows))
}

/// What a row's button does. Navigation rows open the account's Trust tab;
/// retry rows re-run the failed read.
#[derive(Clone)]
enum Action {
    None,
    /// Re-fetch the bounty list.
    Refresh,
    /// Re-run this account's whole trust read.
    RetryTrust(String),
    /// Open this account's Account page on the Trust tab.
    OpenTrust(String),
}

impl Action {
    fn label(&self) -> &'static str {
        match self {
            Action::Refresh | Action::RetryTrust(_) => "Retry →",
            Action::OpenTrust(_) | Action::None => "Fix →",
        }
    }
}

/// A derived "Needs attention" row. `avatar` marks the account a row is
/// about — initials, hue pair, and the name for the title tooltip.
struct Attention {
    dot: u32,
    title: String,
    description: String,
    action: Action,
    avatar: Option<(String, (u32, u32), String, String)>,
}

impl Attention {
    fn new(dot: u32, title: String, description: String) -> Self {
        Self {
            dot,
            title,
            description,
            action: Action::None,
            avatar: None,
        }
    }

    fn action(mut self, action: Action) -> Self {
        self.action = action;
        self
    }

    /// The account this row is about: initials + hue + name + pubkey (the
    /// pubkey keys the tooltip element id — display names can collide).
    fn for_account(mut self, view: &crate::shell::AccountView, ix: usize) -> Self {
        self.avatar = Some((
            initials(view),
            account_hue(ix),
            display_name(view),
            view.pubkey.clone(),
        ));
        self
    }
}

/// The honest to-do list: everything here is read off live state, and an empty
/// list renders as an explicit all-clear instead of an empty card.
fn attention_rows(shell: &Shell) -> Vec<Attention> {
    let mut rows = Vec::new();
    // Claims are signed with an account's key, so no account means no
    // claiming. The bounty list needs none: it is the house issuer's.
    if shell.active_view().is_none() {
        rows.push(Attention::new(
            TEXT_DIM,
            "Read-only — add an account to claim".into(),
            "Press + in the rail to create an account or paste your \
                 nsec. It stays in this Mac's accounts file."
                .into(),
        ));
    }
    if !shell.relay_connected {
        rows.push(Attention::new(
            AMBER,
            "Relay disconnected".into(),
            format!("Reconnecting to {}", nostr::relay_url()),
        ));
    }
    if let Load::Failed(message) = &shell.bounties {
        rows.push(
            Attention::new(RED, "Bounty list unavailable".into(), message.clone())
                .action(Action::Refresh),
        );
    }
    // Then the per-account trust reads, in rail order. A `Loading` part is
    // not a verdict — its rows appear only when the read has answered.
    for (ix, view) in shell.views().enumerate() {
        let Some(state) = shell.trust.get(&view.pubkey) else {
            continue;
        };
        let pubkey = || view.pubkey.clone();
        if let Some(verdict) = state.needs_map() {
            let description = match verdict {
                MapVerdict::Missing => {
                    "No kind-10040 event found — other apps can’t locate your rank provider."
                }
                MapVerdict::Deactivated => {
                    "Your Treasure Map is empty — other apps can’t locate your rank provider."
                }
                MapVerdict::NoRankRow => {
                    "Your Treasure Map names no rank provider — other apps can’t look up your rank."
                }
            };
            rows.push(
                Attention::new(AMBER, "Publish your Treasure Map".into(), description.into())
                    .action(Action::OpenTrust(pubkey()))
                    .for_account(view, ix),
            );
        } else if let Some(message) = state.map_unreadable() {
            rows.push(
                Attention::new(
                    AMBER,
                    "Couldn’t check your Treasure Map".into(),
                    message.to_string(),
                )
                .action(Action::RetryTrust(pubkey()))
                .for_account(view, ix),
            );
        }
        if state.no_follows() {
            rows.push(
                Attention::new(
                    AMBER,
                    "Follow someone".into(),
                    "Trust scores can’t be calculated for an empty contact list.".into(),
                )
                .action(Action::OpenTrust(pubkey()))
                .for_account(view, ix),
            );
        }
        if state.no_verified_follower() {
            rows.push(
                Attention::new(
                    AMBER,
                    "Gain a follower".into(),
                    "You need at least one verified follower for a trust score.".into(),
                )
                .action(Action::OpenTrust(pubkey()))
                .for_account(view, ix),
            );
        }
    }
    rows
}

/// The account name behind an attention row's avatar: gpui tooltips need a
/// view, so the name rides in a one-line entity.
struct NameTip(String);

impl Render for NameTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(self.0.clone())
    }
}

fn attention_row(row: Attention, cx: &mut Context<Shell>) -> AnyElement {
    let has_action = !matches!(row.action, Action::None);
    // The title keys the avatar's element id; it moves into the row text
    // before the avatar closure runs, so take a copy up front.
    let row_key = row.title.clone();
    h_flex()
        .gap(px(11.))
        .items_start()
        .py(px(11.))
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .size(px(8.))
                .mt(px(5.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(row.dot)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .child(
                    h_flex()
                        .gap(px(7.))
                        .items_center()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(row.title)),
                        )
                        .when_some(row.avatar, |this, (letters, hue, name, pubkey)| {
                            this.child(
                                div()
                                    .id(SharedString::from(format!(
                                        "attn-avatar-{row_key}-{pubkey}"
                                    )))
                                    .tooltip(move |_, cx| {
                                        cx.new(|_| NameTip(name.clone())).into()
                                    })
                                    .child(avatar(letters, hue, 18., 8.5)),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(row.description)),
                ),
        )
        .when(has_action, |this| {
            let label = row.action.label();
            let id = match &row.action {
                Action::Refresh => "attention-refresh".to_string(),
                Action::RetryTrust(pubkey) => format!("attention-retry-{pubkey}"),
                Action::OpenTrust(pubkey) => format!("attention-fix-{pubkey}"),
                Action::None => unreachable!(),
            };
            this.child(
                div()
                    .id(id)
                    .flex_shrink_0()
                    .mt(px(1.))
                    .px(px(11.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(rgb(BORDER_3))
                    .cursor_pointer()
                    .text_size(px(11.5))
                    .text_color(rgb(ACCENT_LIGHT))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| match &row.action {
                        Action::Refresh => this.refresh(cx),
                        Action::RetryTrust(pubkey) => {
                            this.fetch_trust(pubkey);
                            cx.notify();
                        }
                        Action::OpenTrust(pubkey) => {
                            this.open_account(pubkey, Tab::Trust, window, cx)
                        }
                        Action::None => {}
                    })),
            )
        })
        .into_any_element()
}

fn all_clear(shell: &Shell) -> AnyElement {
    let (dot, text): (u32, &str) = if matches!(shell.bounties, Load::Loading) {
        (TEXT_DIM, "Loading the bounty list…")
    } else {
        (GREEN, "Nothing needs you right now. The carpet flies itself.")
    };
    h_flex()
        .gap(px(11.))
        .items_start()
        .py(px(11.))
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .size(px(8.))
                .mt(px(5.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(dot)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(text.to_string())),
        )
        .into_any_element()
}

fn activity_row(event: &ActivityItem, now: u64) -> AnyElement {
    let meta = format!(
        "{} UTC · {}",
        timefmt::hhmmss_utc(event.at),
        timefmt::relative(now, event.at)
    );

    h_flex()
        .gap(px(11.))
        .items_start()
        .py(px(10.))
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .size(px(8.))
                .mt(px(5.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(event.dot)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .child(SharedString::from(event.text.clone())),
                )
                .child(
                    div()
                        .mt(px(1.))
                        .text_size(px(11.))
                        .text_color(rgb(TEXT_DIM))
                        .child(SharedString::from(meta)),
                ),
        )
        .into_any_element()
}

fn empty_activity() -> AnyElement {
    div()
        .py(px(11.))
        .border_t_1()
        .border_color(rgb(BORDER))
        .text_size(px(12.5))
        .text_color(rgb(TEXT_DIM))
        .child("Nothing yet — claims, payments, and receipts appear here live.")
        .into_any_element()
}

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> impl IntoElement {
    let now = timefmt::now_unix();
    let host = api::base_url();
    let host = host
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let accounts = shell.views().count();
    let summary = format!(
        "{accounts} account{} · watching {host}",
        if accounts == 1 { "" } else { "s" }
    );

    let attention = attention_rows(shell);
    let badge = (!attention.is_empty()).then(|| attention.len().to_string());
    let attention_elements: Vec<AnyElement> = if attention.is_empty() {
        vec![all_clear(shell)]
    } else {
        attention
            .into_iter()
            .map(|row| attention_row(row, cx))
            .collect()
    };

    let activity_elements: Vec<AnyElement> = if shell.activity.is_empty() {
        vec![empty_activity()]
    } else {
        shell
            .activity
            .iter()
            .map(|event| activity_row(event, now))
            .collect()
    };

    let action_label = if shell.is_active_issuer() {
        "+ New DList + bounty"
    } else {
        "+ Submit a bounty claim"
    };
    // With no account the CTA cannot do what it says — it dims, and a click
    // (like ⌘N) opens the account screen.
    let keyless = shell.active_view().is_none();

    v_flex()
        .child(
            h_flex()
                .gap(px(14.))
                .items_center()
                .mb(px(20.))
                .child(
                    div()
                        .text_size(px(21.))
                        .font_weight(FontWeight::BOLD)
                        .child("Dashboard"),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(summary)),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("new-item")
                        .flex_shrink_0()
                        .px(px(18.))
                        .py(px(9.))
                        .rounded(px(9.))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(!keyless, |this| {
                            this.bg(grad(ACCENT, ACCENT_DEEP))
                                .shadow(vec![
                                    BoxShadow::new(px(0.), px(4.), wash(ACCENT_GLOW))
                                        .blur_radius(px(14.)),
                                ])
                                .text_color(rgb(BG_RAIL))
                        })
                        .when(keyless, |this| {
                            this.border_1()
                                .border_color(rgb(BORDER_3))
                                .text_color(rgb(TEXT_DIM))
                        })
                        .child(action_label)
                        .on_click(cx.listener(|this, _, window, cx| this.new_item(window, cx))),
                ),
        )
        .child(
            h_flex()
                .gap(px(16.))
                .items_start()
                .child(card("Needs attention", badge, attention_elements))
                .child(card("Recent activity", None, activity_elements)),
        )
}
