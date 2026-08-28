//! The Dashboard screen: "Needs attention" beside "Recent activity".
//!
//! Every row renders from the shell's live state. "Needs attention" is derived
//! (missing keys, relay down, list failures) and "Recent activity" is the feed
//! the nostr runtime's updates write, newest first.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex};

use magic_carpet_chat::api;
use magic_carpet_chat::nostr;
use magic_carpet_chat::secrets::Account;

use crate::palette::*;
use crate::shell::{ActivityItem, Load, Shell};
use crate::timefmt;

/// The mock's monospace face, used for every sats figure and every npub.
pub const MONO: &str = "Menlo";

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

/// A derived "Needs attention" row. `retry` wires the Fix button to a live
/// refresh; rows whose fix happens outside the app (key import) get none.
struct Attention {
    dot: u32,
    title: String,
    description: String,
    retry: bool,
}

/// The honest to-do list: everything here is read off live state, and an empty
/// list renders as an explicit all-clear instead of an empty card.
fn attention_rows(shell: &Shell) -> Vec<Attention> {
    let mut rows = Vec::new();
    // Only the CLAIMANT key gates anything a user can do here — claims are
    // signed with it. A missing ISSUER key is deliberately NOT a row: the
    // app lists the house issuer's bounties without one, claiming never
    // needs it, and steering a claimant at the issuer slot would replace
    // the bounty list with their own (empty) one.
    if shell.view(Account::Claimant).missing {
        let description = if shell.account == Account::Claimant {
            "No key for the Claimant account — paste your nsec in the sidebar \
             input to claim and get paid. It stays in this Mac's keychain."
        } else {
            "No key for the Claimant account — press ⌘] to switch to it, then \
             paste your nsec in the sidebar input."
        };
        rows.push(Attention {
            dot: TEXT_DIM,
            title: "Read-only — import a key to claim".into(),
            description: description.into(),
            retry: false,
        });
    }
    if !shell.relay_connected {
        rows.push(Attention {
            dot: AMBER,
            title: "Relay disconnected".into(),
            description: format!("Reconnecting to {}", nostr::relay_url()),
            retry: false,
        });
    }
    if let Load::Failed(message) = &shell.bounties {
        rows.push(Attention {
            dot: RED,
            title: "Bounty list unavailable".into(),
            description: message.clone(),
            retry: true,
        });
    }
    rows
}

fn attention_row(row: Attention, cx: &mut Context<Shell>) -> AnyElement {
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
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(row.title)),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(row.description)),
                ),
        )
        .when(row.retry, |this| {
            this.child(
                div()
                    .id("retry")
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
                    .child("Retry →")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
        })
        .into_any_element()
}

fn all_clear(shell: &Shell) -> AnyElement {
    // "Keys loaded" may only be claimed when it is true of BOTH accounts —
    // an issuer-keyless claimant still reaches this row (that gap is not an
    // attention item), so the text drops the claim instead of lying.
    let all_keys = Account::ALL
        .into_iter()
        .all(|account| shell.view(account).npub.is_some());
    let (dot, text): (u32, &str) = if matches!(shell.bounties, Load::Loading) {
        (TEXT_DIM, "Loading the bounty list…")
    } else if all_keys {
        (GREEN, "All clear — keys loaded, relay connected, bounty list live.")
    } else {
        (GREEN, "All clear — relay connected, bounty list live.")
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
    let keys_loaded = Account::ALL
        .into_iter()
        .filter(|account| shell.view(*account).npub.is_some())
        .count();
    let summary = format!("{keys_loaded} of 2 account keys · watching {host}");

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

    let action_label = match shell.account {
        Account::Issuer => "+ New DList + bounty",
        Account::Claimant => "+ Claim an item",
    };
    // With no key for the active account the CTA cannot do what it says —
    // it dims, and a click (like ⌘N) routes to the sidebar key input.
    let keyless = shell.view(shell.account).missing;

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
