//! The Dashboard screen: "Needs attention" beside "Recent activity".
//!
//! Every row renders from the shell's live state. "Needs attention" is derived
//! (no account, relay down, list failures) and "Recent activity" is the feed
//! the nostr runtime's updates write, newest first.

use std::collections::HashMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{h_flex, v_flex};

use magic_carpet_chat::api;
use magic_carpet_chat::nostr;

use crate::a11y::Accessible;
use crate::account::Tab;
use crate::palette::*;
use crate::shell::{ActivityItem, Load, Shell, account_hue, display_name, initials};
use crate::timefmt;
use crate::trust_state::MapVerdict;

/// The mock's monospace face, used for every sats figure and every npub.
/// Menlo is the bundled macOS face. gpui's Linux text system matches family
/// names exactly and does not resolve the generic "monospace", so every layout
/// failed the lookup and repainted slowly; Linux registers a bundled font.
pub const MONO: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else {
    MONO_FONT_FAMILY
};

/// The family name inside [`MONO_FONT`]; a test keeps the two in step.
const MONO_FONT_FAMILY: &str = "JetBrains Mono";

/// JetBrains Mono Regular and Bold (SIL OFL 1.1, licence beside them),
/// registered with gpui's text system at startup on every platform but macOS.
/// Bold also serves semibold text by nearest match.
#[cfg(any(test, not(target_os = "macos")))]
pub const MONO_FONTS: [&[u8]; 2] = [
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
];

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

/// [`avatar`] drawn over a remote picture. gpui fetches the URL through its
/// own HTTP client and caches it per URL; the initials show while it loads,
/// on error, and when `picture` is absent or not http(s).
pub fn remote_avatar(
    picture: Option<&str>,
    initials: impl Into<SharedString>,
    hue: (u32, u32),
    size: f32,
    text: f32,
) -> AnyElement {
    let initials: SharedString = initials.into();
    let Some(url) = picture.filter(|u| u.starts_with("https://") || u.starts_with("http://"))
    else {
        return avatar(initials, hue, size, text).into_any_element();
    };
    let (loading, failed) = (initials.clone(), initials);
    img(SharedUri::from(url.to_string()))
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .object_fit(ObjectFit::Cover)
        .with_loading(move || avatar(loading.clone(), hue, size, text).into_any_element())
        .with_fallback(move || avatar(failed.clone(), hue, size, text).into_any_element())
        .into_any_element()
}

/// A count in a soft accent pill — the badge beside "Needs attention".
pub fn count_badge(count: impl Into<SharedString>) -> impl IntoElement {
    badge_div(count.into())
}

fn badge_div(count: SharedString) -> Div {
    div()
        .px(px(7.))
        .py(px(1.))
        .rounded_full()
        .bg(wash(ACCENT_WASH))
        .font_family(MONO)
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(ACCENT_LIGHT))
        .child(count)
}

/// `id` is the screen-wide prefix: `<id>-heading`, `<id>-count`, `<id>-list`.
fn card(
    id: &'static str,
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
                        .child(title)
                        .a11y(format!("{id}-heading"), Role::Heading, title),
                )
                .when_some(badge, |this, badge| {
                    this.child(badge_div(badge.clone().into()).a11y(
                        format!("{id}-count"),
                        Role::Label,
                        badge,
                    ))
                }),
        )
        .child(
            v_flex()
                .children(rows)
                .a11y(format!("{id}-list"), Role::List, title),
        )
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
    /// Stable kebab slug of what this row is about; part of its a11y id.
    kind: &'static str,
    dot: u32,
    title: String,
    description: String,
    action: Action,
    avatar: Option<(String, (u32, u32), String, String)>,
}

impl Attention {
    fn new(kind: &'static str, dot: u32, title: String, description: String) -> Self {
        Self {
            kind,
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
            "read-only",
            TEXT_DIM,
            "Read-only — add an account to claim".into(),
            "Click the + button in the left bar to create an account or paste a \
                 secret key. Keys stay on this computer."
                .into(),
        ));
    }
    if !shell.relay_connected {
        rows.push(Attention::new(
            "relay-disconnected",
            AMBER,
            "Relay disconnected".into(),
            format!(
                "Reconnecting to {}. If it stays down, restart the app.",
                nostr::relay_url()
            ),
        ));
    }
    if let Load::Failed(message) = &shell.bounties {
        rows.push(
            Attention::new("bounty-list-unavailable", RED, "Bounty list unavailable".into(), message.clone())
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
            let (kind, description) = match verdict {
                MapVerdict::Missing => (
                    "map-missing",
                    "No kind-10040 event found — other apps can’t locate your rank provider.",
                ),
                MapVerdict::Deactivated => (
                    "map-deactivated",
                    "Your Treasure Map is empty, so other apps can’t find your score provider. Click Fix to republish it.",
                ),
                MapVerdict::NoRankRow => (
                    "map-no-rank-row",
                    "Your Treasure Map has no rank entry, so apps can’t look up your rank. Click Fix to see what is missing.",
                ),
            };
            rows.push(
                Attention::new(kind, AMBER, "Publish your Treasure Map".into(), description.into())
                    .action(Action::OpenTrust(pubkey()))
                    .for_account(view, ix),
            );
        } else if let Some(message) = state.map_unreadable() {
            rows.push(
                Attention::new(
                    "map-unreadable",
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
                    "no-follows",
                    AMBER,
                    "Follow someone".into(),
                    "Trust scores can’t be calculated for an empty follow list.".into(),
                )
                .action(Action::OpenTrust(pubkey()))
                .for_account(view, ix),
            );
        }
        if state.no_verified_follower() {
            rows.push(
                Attention::new(
                    "no-verified-follower",
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
    let row_id = match &row.avatar {
        Some((_, _, _, pubkey)) => format!("dash-attention-{pubkey}-{}", row.kind),
        None => format!("dash-attention-{}", row.kind),
    };
    h_flex()
        .a11y(row_id.clone(), Role::ListItem, row_key.clone())
        .aria_description(row.description.clone())
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
                                .child(SharedString::from(row.title))
                                .a11y(format!("{row_id}-title"), Role::Label, row_key.clone()),
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
                        .child(SharedString::from(row.description.clone()))
                        .a11y(
                            format!("{row_id}-description"),
                            Role::Label,
                            row.description,
                        ),
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
                    .a11y(format!("{row_id}-action"), Role::Button, label)
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
        (TEXT_DIM, "Loading bounties…")
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
                .child(SharedString::from(text.to_string()))
                .a11y("dash-attention-clear", Role::Label, text),
        )
        .into_any_element()
}

/// ActivityItem carries no event id, so the key is its timestamp plus an
/// FNV-1a hash of its text; identical items get a `-<n>` suffix in order.
fn activity_key(event: &ActivityItem) -> String {
    let hash = event.text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    format!("dash-activity-{}-{hash:x}", event.at)
}

fn activity_row(event: &ActivityItem, now: u64, id: String) -> AnyElement {
    let meta = format!(
        "{} UTC · {}",
        timefmt::hhmmss_utc(event.at),
        timefmt::relative(now, event.at)
    );

    h_flex()
        .a11y(id.clone(), Role::ListItem, event.text.clone())
        .aria_description(meta.clone())
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
                        .child(SharedString::from(meta.clone()))
                        .a11y(format!("{id}-meta"), Role::Label, meta),
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
        .child("Nothing yet. New claims and payments show up here.")
        .a11y(
            "dash-activity-empty",
            Role::Label,
            "Nothing yet. New claims and payments show up here.",
        )
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
        let mut seen: HashMap<String, usize> = HashMap::new();
        shell
            .activity
            .iter()
            .map(|event| {
                let key = activity_key(event);
                let n = seen.entry(key.clone()).or_insert(0);
                *n += 1;
                let id = if *n == 1 { key } else { format!("{key}-{n}") };
                activity_row(event, now, id)
            })
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
                        .child("Dashboard")
                        .a11y("dash-heading", Role::Heading, "Dashboard"),
                )
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(summary.clone()))
                        .a11y("dash-summary", Role::Label, summary),
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
                        .a11y("dash-new-item", Role::Button, action_label)
                        .on_click(cx.listener(|this, _, window, cx| this.new_item(window, cx))),
                ),
        )
        .child(
            h_flex()
                .gap(px(16.))
                .items_start()
                .child(card("dash-attention", "Needs attention", badge, attention_elements))
                .child(card("dash-activity", "Recent activity", None, activity_elements)),
        )
}

#[cfg(test)]
mod tests {
    use super::{MONO_FONTS, MONO_FONT_FAMILY};

    /// The family gpui indexes each bundled face under is the one `MONO` names
    /// off macOS; a mismatch would send Linux back to the failing lookup.
    /// fontdb's rule: the typographic family (name ID 16) wins, else ID 1.
    #[test]
    fn every_bundled_mono_face_is_indexed_under_the_mono_family() {
        for data in MONO_FONTS {
            let face = ttf_parser::Face::parse(data, 0).expect("bundled font parses");
            let family_of = |id| {
                face.names()
                    .into_iter()
                    .filter(|name| name.name_id == id)
                    .find_map(|name| name.to_string())
            };
            let family = family_of(ttf_parser::name_id::TYPOGRAPHIC_FAMILY)
                .or_else(|| family_of(ttf_parser::name_id::FAMILY));
            assert_eq!(family.as_deref(), Some(MONO_FONT_FAMILY));
        }
    }
}
