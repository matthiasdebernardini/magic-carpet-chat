//! The Dashboard screen: "Needs attention" beside "Recent activity".
//!
//! Every row is static demo data from [`crate::data`]. Nothing on this screen
//! is wired to a click yet, so it renders without a view or a context.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{h_flex, v_flex};

use crate::data::{self, ACCOUNTS, ACTIVITY, TASKS};
use crate::palette::*;

/// The mock's monospace face, used for every sats figure and every npub.
pub const MONO: &str = "Menlo";

/// A round account marker. `size` is the diameter; the initials scale with it.
pub fn avatar(initials: &'static str, hue: (u32, u32), size: f32, text: f32) -> impl IntoElement {
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
        .child(initials)
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

fn card(title: &'static str, badge: Option<&'static str>, rows: Vec<AnyElement>) -> impl IntoElement {
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

fn task_row(task: &'static data::Task) -> AnyElement {
    let account = &ACCOUNTS[task.account];

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
                .bg(rgb(task.dot)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .items_center()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(task.title),
                        )
                        .child(avatar(account.initials, account.hue, 16., 8.))
                        .when(task.planned, |this| {
                            this.child(
                                div()
                                    .px(px(7.))
                                    .py(px(2.))
                                    .rounded_full()
                                    .bg(rgb(BG_NAV_ON))
                                    .text_size(px(9.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT_MUTED))
                                    .child("PLANNED"),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(task.description),
                ),
        )
        .when(task.fixable, |this| {
            this.child(
                div()
                    .flex_shrink_0()
                    .mt(px(1.))
                    .px(px(11.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(rgb(BORDER_3))
                    .text_size(px(11.5))
                    .text_color(rgb(ACCENT_LIGHT))
                    .child("Fix →"),
            )
        })
        .into_any_element()
}

fn activity_row(event: &'static data::Event) -> AnyElement {
    let meta = format!("{} · {}", ACCOUNTS[event.account].name, event.when);

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
                .child(div().text_size(px(12.5)).child(event.text))
                .child(
                    div()
                        .mt(px(1.))
                        .text_size(px(11.))
                        .text_color(rgb(TEXT_DIM))
                        .child(meta),
                ),
        )
        .into_any_element()
}

/// The account filter chips under the heading. Both read as on, as in the mock.
fn account_chip(account: &'static data::Account) -> impl IntoElement {
    h_flex()
        .gap(px(7.))
        .items_center()
        .pl(px(5.))
        .pr(px(12.))
        .py(px(4.))
        .rounded_full()
        .border_1()
        .border_color(rgb(BORDER_3))
        .bg(rgb(BG_CARD))
        .text_size(px(12.))
        .child(avatar(account.initials, account.hue, 19., 8.5))
        .child(account.name)
}

pub fn render() -> impl IntoElement {
    v_flex()
        .child(
            h_flex()
                .gap(px(14.))
                .items_center()
                .mb(px(6.))
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
                        .child(data::SUMMARY),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_shrink_0()
                        .px(px(18.))
                        .py(px(9.))
                        .rounded(px(9.))
                        .bg(grad(ACCENT, ACCENT_DEEP))
                        .shadow(vec![
                            BoxShadow::new(px(0.), px(4.), wash(ACCENT_GLOW)).blur_radius(px(14.)),
                        ])
                        .text_size(px(13.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(BG_RAIL))
                        .child("+ Submit a new bounty"),
                ),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .items_center()
                .mb(px(20.))
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(rgb(TEXT_DIM))
                        .child("Showing:"),
                )
                .children(ACCOUNTS.iter().map(account_chip)),
        )
        .child(
            h_flex()
                .gap(px(16.))
                .items_start()
                .child(card(
                    "Needs attention",
                    Some("4"),
                    TASKS.iter().map(task_row).collect(),
                ))
                .child(card(
                    "Recent activity",
                    None,
                    ACTIVITY.iter().map(activity_row).collect(),
                )),
        )
}
