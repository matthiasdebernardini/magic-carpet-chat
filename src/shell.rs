//! The app shell: the icon rail, the nav column, the top bar, and whichever
//! screen the nav points at.
//!
//! Only two screens are built — the Dashboard and the chat. Every other nav row
//! switches to a placeholder, which keeps the nav honest without pretending the
//! screen exists.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    TitleBar, h_flex,
    input::{Input, InputState},
    v_flex,
};

use crate::chat::Chat;
use crate::dashboard::{self, MONO, avatar, count_badge};
use crate::data::{ACCOUNTS, ACTIVE};
use crate::icons::icon;
use crate::palette::*;

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
        PrevAccount, NextAccount
    ]
);

/// cmd-1…cmd-8 follow the nav order; cmd-[ / cmd-] step through accounts.
/// Keyboard routes exist for their own sake, but they are also the only input
/// UI automation can drive: gpui ignores synthetic mouse events posted to the
/// process, while key events arrive fine.
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
    ]
}

/// The nav column, in the mock's order, with the mock's counts.
const NAV: [(Screen, u32); 8] = [
    (Screen::Dashboard, 4),
    (Screen::Bounties, 7),
    (Screen::Payments, 1),
    (Screen::Claimants, 0),
    (Screen::Boards, 0),
    (Screen::Wallet, 0),
    (Screen::Tags, 0),
    (Screen::Chat, 0),
];

pub struct Shell {
    screen: Screen,
    account: usize,
    chat: Entity<Chat>,
    search: Entity<InputState>,
    /// Keeps key dispatch anchored inside the shell on screens with no input
    /// of their own, so the cmd-N bindings always land.
    focus: FocusHandle,
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
        Self {
            screen: Screen::Dashboard,
            account: ACTIVE,
            chat,
            search: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Search lists, bounties, profiles…")
            }),
            focus,
        }
    }

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

    fn step_account(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = ACCOUNTS.len() as isize;
        self.account = ((self.account as isize + step).rem_euclid(n)) as usize;
        self.go(Screen::Dashboard, window, cx);
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
            .children(ACCOUNTS.iter().enumerate().map(|(ix, account)| {
                let ring = if ix == self.account {
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
                    .child(avatar(account.initials, account.hue, 42., 15.))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.account = ix;
                        this.go(Screen::Dashboard, window, cx);
                    }))
            }))
            .child(
                div()
                    .size(px(42.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_dashed()
                    .border_color(rgb(BORDER_3))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(20.))
                    .text_color(rgb(TEXT_MUTED))
                    .child("+"),
            )
            .child(div().flex_1())
            .child(
                v_flex()
                    .items_center()
                    .gap(px(4.))
                    .child(div().size(px(8.)).rounded_full().bg(rgb(GREEN)))
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

    fn stat_card(label: &'static str, value: &'static str, hue: u32, dim: u32, note: Option<&'static str>) -> impl IntoElement {
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
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(dim))
                            .child("sats"),
                    ),
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
        let account = &ACCOUNTS[self.account];

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
                                            .child(account.name),
                                    )
                                    .child(
                                        div()
                                            .mt(px(1.))
                                            .font_family(MONO)
                                            .text_size(px(10.5))
                                            .text_color(rgb(TEXT_DIM))
                                            .child(account.npub),
                                    ),
                            )
                            .child(div().text_color(rgb(TEXT_DIM)).child("›")),
                    ),
            )
            .children(NAV.iter().map(|&(screen, badge)| {
                let on = self.screen == screen;
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
                    .when(badge > 0, |this| {
                        this.child(count_badge(badge.to_string()))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.go(screen, window, cx)))
            }))
            .child(div().flex_1())
            .child(Self::stat_card(
                "EARNED",
                crate::data::EARNED,
                GREEN,
                GREEN_DIM,
                Some(crate::data::EARNED_NOTE),
            ))
            .child(Self::stat_card(
                "SPENT",
                crate::data::SPENT,
                AMBER,
                AMBER_DIM,
                Some(crate::data::SPENT_NOTE),
            ))
            .child(Self::stat_card(
                "BALANCE",
                crate::data::BALANCE,
                AMBER,
                AMBER_DIM,
                None,
            ))
    }

    // ------------------------------------------------------------- top bar

    fn top_bar(&self) -> impl IntoElement {
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
            .child(
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
                    .child("Auto-pay armed"),
            )
            .child(
                div()
                    .size(px(30.))
                    .flex_shrink_0()
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(BORDER_2))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(TEXT_MUTED))
                    .child("?"),
            )
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

    fn main(&self) -> AnyElement {
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
                        .child(dashboard::render()),
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
            .on_action(cx.listener(|this, _: &PrevAccount, w, cx| this.step_account(-1, w, cx)))
            .on_action(cx.listener(|this, _: &NextAccount, w, cx| this.step_account(1, w, cx)))
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
                            .child(self.main()),
                    ),
            )
    }
}
