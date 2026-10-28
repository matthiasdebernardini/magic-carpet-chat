//! The Accounts screen — one card per stored account — and the Account
//! page: header, five-tab strip, and the two tabs with real content so far:
//! Identity & keys (npub, nsec export, remove) and Trust.
//!
//! The nsec leaves the store only two ways: straight to the clipboard, or
//! into [`Revealed`] while the Identity tab shows it. That copy is a
//! [`Secret`], lives at most [`NSEC_SHOWN_FOR`], and is dropped on Hide,
//! on a tab change, and on leaving the page.
//!
//! Everything the Trust tab shows came through `TrustState`: signature-
//! verified kind-10040 / kind-30382 / kind-3 reads plus the Brainstorm setup
//! answer, each with its own success-or-failure status, so a relay that
//! never answers reads as "couldn't check", never as "none".

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_kit::component::{h_flex, v_flex};
use nostr_sdk::prelude::*;

use magic_carpet_chat::nostr;
use magic_carpet_chat::secrets::Secret;
use magic_carpet_chat::trust::{self, Assertion, BrainstormKey, Designation};

use crate::a11y::Accessible;
use crate::community;
use crate::dashboard::MONO;
use crate::palette::*;
use crate::shell::{AccountView, Screen, Shell, display_name, short_id, short_npub};
use crate::timefmt;
use crate::trust_state::{Evidence, Status, TrustState};

/// The five tabs in the mock's strip order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tab {
    Identity,
    Trust,
    Progress,
    Wallet,
    Limits,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Identity => "Identity & keys",
            Tab::Trust => "Trust",
            Tab::Progress => "Progress",
            Tab::Wallet => "Wallet",
            Tab::Limits => "Limits & defaults",
        }
    }

    /// The tab's a11y id.
    fn id(self) -> &'static str {
        match self {
            Tab::Identity => "account-tab-identity",
            Tab::Trust => "account-tab-trust",
            Tab::Progress => "account-tab-progress",
            Tab::Wallet => "account-tab-wallet",
            Tab::Limits => "account-tab-limits",
        }
    }
}

/// The Account page's own state: which account it is about (not necessarily
/// the active one), which tab, which raw-JSON blocks are open, and which
/// copy button last fired — for its "copied" tick.
pub(crate) struct AccountPage {
    pub pubkey: String,
    pub tab: Tab,
    /// Open raw toggles, keyed by card ("map", "own", "instance").
    pub open_raw: HashSet<String>,
    /// Set by a copy click; the next update clears it.
    pub copied: Option<String>,
    /// The nsec while "Reveal" is on; `None` is the masked row.
    pub revealed: Option<Revealed>,
    /// The 1 s countdown behind `revealed`. Cancel-on-drop, so clearing it
    /// stops the clock.
    pub reveal_timer: Option<Task<()>>,
}

/// How long a revealed nsec stays on screen.
pub(crate) const NSEC_SHOWN_FOR: Duration = Duration::from_secs(30);

/// A revealed nsec and the moment it hides again. `Secret` keeps it out of
/// any `{:?}`; only the Identity tab's render calls `expose`.
pub(crate) struct Revealed {
    pub nsec: Secret,
    pub hide_at: Instant,
}

impl Revealed {
    /// Whole seconds left, rounded up, so the first frame reads 30.
    pub(crate) fn seconds_left(&self) -> u64 {
        self.hide_at
            .saturating_duration_since(Instant::now())
            .as_millis()
            .div_ceil(1000) as u64
    }
}

impl AccountPage {
    pub(crate) fn new(pubkey: &str, tab: Tab) -> Self {
        Self {
            pubkey: pubkey.to_string(),
            tab,
            open_raw: HashSet::new(),
            copied: None,
            revealed: None,
            reveal_timer: None,
        }
    }

    /// Masks the nsec again and stops its countdown.
    pub(crate) fn hide_nsec(&mut self) {
        self.revealed = None;
        self.reveal_timer = None;
    }
}

// ----------------------------------------------------------------- pieces

/// The card every tab section sits in.
fn card() -> Div {
    v_flex()
        .w_full()
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(14.))
        .px(px(22.))
        .py(px(20.))
}

/// A card's header line: title, the kind it reads in mono, and the verdict
/// pill at the right edge.
fn card_head(
    id: &'static str,
    title: &'static str,
    kind: &'static str,
    pill: AnyElement,
) -> impl IntoElement {
    h_flex()
        .gap(px(10.))
        .items_center()
        .child(
            div()
                .text_size(px(14.5))
                .font_weight(FontWeight::SEMIBOLD)
                .child(title)
                .a11y(id, Role::Heading, title),
        )
        .child(
            div()
                .font_family(MONO)
                .text_size(px(11.))
                .text_color(rgb(TEXT_DIM))
                .child(kind),
        )
        .child(div().flex_1())
        .child(pill)
}

/// The Located / Not located / None found pill: a dot and small caps label
/// on a wash of the same colour.
fn status_pill(id: &'static str, label: &'static str, color: u32, fill: u32) -> AnyElement {
    h_flex()
        .gap(px(7.))
        .items_center()
        .px(px(10.))
        .py(px(3.))
        .rounded_full()
        .bg(wash(fill))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(color))
        .child(div().size(px(6.)).rounded_full().bg(rgb(color)))
        .child(label)
        .a11y(id, Role::Label, label)
        .into_any_element()
}

/// A small bordered button ("Check again", "Switch", …).
fn outline_button(
    id: impl Into<SharedString>,
    label: &'static str,
    color: u32,
    cx: &mut Context<Shell>,
    on_click: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> impl IntoElement {
    let id = id.into();
    div()
        .id(id.clone())
        .flex_shrink_0()
        .px(px(13.))
        .py(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(BORDER_3))
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(rgb(color))
        .hover(move |this| this.border_color(rgb(ACCENT)))
        .child(label)
        .a11y(id, Role::Button, label)
        .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
}

/// A text link that opens a URL — the Brainstorm links.
fn open_link(
    id: &'static str,
    label: &'static str,
    url: String,
    cx: &mut Context<Shell>,
) -> impl IntoElement {
    div()
        .id(id)
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(rgb(ACCENT_LIGHT))
        .child(label)
        .a11y(id, Role::Link, label)
        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
}

/// One "Label — value" row of a card's fact grid.
fn fact(label: &'static str, value: impl IntoElement) -> impl IntoElement {
    h_flex()
        .items_baseline()
        .child(
            div()
                .w(px(130.))
                .flex_shrink_0()
                .text_size(px(12.5))
                .text_color(rgb(TEXT_DIM))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(12.5))
                .child(value),
        )
}

/// The "▸ View raw event" toggle and, under it, the verified event's pretty
/// JSON in a horizontally scrolling mono block.
fn raw_toggle(
    key: &'static str,
    label: &'static str,
    raw: &str,
    open: bool,
    cx: &mut Context<Shell>,
) -> impl IntoElement {
    let key_owned = key.to_string();
    let toggle_text = if open {
        format!("▾ Hide {label}")
    } else {
        format!("▸ View {label}")
    };
    let toggle = div()
        .id(SharedString::from(format!("account-trust-raw-{key}")))
        .mt(px(14.))
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(rgb(ACCENT_LIGHT))
        .child(toggle_text.clone())
        .a11y(format!("account-trust-raw-{key}"), Role::Button, toggle_text)
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(page) = &mut this.account_page {
                if open {
                    page.open_raw.remove(&key_owned);
                } else {
                    page.open_raw.insert(key_owned.clone());
                }
            }
            cx.notify();
        }));
    v_flex()
        .child(toggle)
        .when(open, |this| {
            this.child(
                div()
                    .id(SharedString::from(format!("account-trust-raw-scroll-{key}")))
                    .mt(px(10.))
                    .w_full()
                    .bg(rgb(BG_RAIL))
                    .border_1()
                    .border_color(rgb(BORDER_2))
                    .rounded(px(10.))
                    .px(px(16.))
                    .py(px(14.))
                    .overflow_x_scroll()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(11.5))
                            .whitespace_nowrap()
                            .text_color(rgb(0xc9c7d8))
                            .child(SharedString::from(raw.to_string())),
                    ),
            )
        })
}

const MISSING_MAP_TEXT: &str = "Your personalized community trust scores cannot be located because the relevant piece of published data (a kind-10040 \"Treasure Map\") could not be found. Please go to Brainstorm and follow the prompts to set up your trust network, then come back here.";

/// The red "!" note the mock uses for a missing Treasure Map. The Brainstorm
/// in its sentence is a link — drawn as its own line inside the box, since
/// gpui has no inline links in running text.
fn missing_map_note(cx: &mut Context<Shell>) -> impl IntoElement {
    h_flex()
        .gap(px(10.))
        .items_start()
        .bg(rgba(0xe5646c14))
        .border_1()
        .border_color(rgba(0xe5646c44))
        .rounded(px(9.))
        .px(px(13.))
        .py(px(11.))
        .text_size(px(12.5))
        .text_color(rgb(0xf0a9ae))
        .child(
            div()
                .flex_shrink_0()
                .text_color(rgb(RED))
                .font_weight(FontWeight::BOLD)
                .child("!"),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .gap(px(6.))
                .child(div().a11y("account-trust-missing-map", Role::Label, MISSING_MAP_TEXT).child(MISSING_MAP_TEXT))
                .child(open_link(
                    "account-trust-brainstorm-link",
                    "Open Brainstorm ↗",
                    "https://brainstorm.world".to_string(),
                    cx,
                )),
        )
}

/// The issuer's npub, shortened, for the "scores as seen by" lines.
fn issuer_short_npub() -> String {
    nostr_sdk::prelude::PublicKey::from_hex(&nostr::issuer_pubkey())
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .map(|npub| short_npub(&npub))
        .unwrap_or_else(|| "the issuer".to_string())
}

// ------------------------------------------------------------ Accounts screen

/// The Accounts screen: one card per stored account, each with Switch and
/// Manage. Matches the mock's `isAccounts` block.
pub(crate) fn render_accounts(shell: &Shell, cx: &mut Context<Shell>) -> AnyElement {
    v_flex()
        .w_full()
        .child(
            h_flex()
                .items_baseline()
                .mb(px(4.))
                .child(
                    div()
                        .text_size(px(21.))
                        .font_weight(FontWeight::BOLD)
                        .child("Accounts")
                        .a11y("accounts-title", Role::Heading, "Accounts"),
                )
                .child(div().flex_1())
                .child(outline_button(
                    "accounts-add",
                    "+ Add an account",
                    TEXT,
                    cx,
                    |this, window, cx| this.open_account_setup(false, window, cx),
                )),
        )
        .child(
            div()
                .mb(px(20.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("Each account is its own nsec, wallet, limits, and payment log. All of them watch the relay at once."),
        )
        .child(
            v_flex()
                .gap(px(12.))
                .children(
                    shell
                        .views()
                        .enumerate()
                        .map(|(ix, view)| account_card(shell, ix, view, cx)),
                )
                .a11y("accounts-list", Role::List, "Accounts"),
        )
        .child(
            div()
                .mt(px(16.))
                .text_size(px(11.5))
                .text_color(rgb(TEXT_DIM))
                .child("Removing an account deletes its key and wallet login from this Mac's accounts file after a confirmation."),
        )
        .into_any_element()
}

/// One account card: avatar, name + active pill, npub, balance, and the
/// Switch / Manage buttons.
fn account_card(shell: &Shell, ix: usize, view: &AccountView, cx: &mut Context<Shell>) -> AnyElement {
    let is_active = shell.active.as_deref() == Some(view.pubkey.as_str());
    let pubkey = view.pubkey.clone();
    let balance = view
        .wallet
        .balance
        .map(timefmt::fmt_sats)
        .unwrap_or_else(|| "—".to_string());
    let card_id = format!("accounts-card-{}", view.pubkey);
    let name = display_name(view);
    let payout = match view.payout_address() {
        Some(address) => format!("payouts → {address}"),
        None => "no payout address yet".to_string(),
    };
    h_flex()
        .w_full()
        .items_center()
        .gap(px(16.))
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(if is_active { BORDER_2 } else { BORDER }))
        .rounded(px(14.))
        .px(px(20.))
        .py(px(18.))
        .a11y(card_id.clone(), Role::Group, name.clone())
        .child(Shell::account_marker(view, ix, 46., 16.))
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .child(
                    h_flex()
                        .gap(px(10.))
                        .items_center()
                        .child(
                            div()
                                .text_size(px(14.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(SharedString::from(name.clone()))
                                .a11y(format!("{card_id}-name"), Role::Label, name.clone()),
                        )
                        .when(is_active, |this| {
                            this.child(
                                div()
                                    .px(px(8.))
                                    .py(px(2.))
                                    .rounded_full()
                                    .bg(wash(0x4ecb8d22))
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(GREEN))
                                    .child("active")
                                    .a11y(format!("{card_id}-active"), Role::Label, "active"),
                            )
                        }),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_DIM))
                        .child(SharedString::from(view.npub.clone()))
                        .a11y(format!("{card_id}-npub"), Role::Label, view.npub.clone()),
                )
                .child(
                    div()
                        .mt(px(3.))
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(payout.clone()))
                        .a11y(format!("{card_id}-payout"), Role::Label, payout),
                ),
        )
        .child(
            v_flex()
                .flex_shrink_0()
                .items_end()
                .child(
                    h_flex()
                        .gap(px(4.))
                        .items_baseline()
                        .font_family(MONO)
                        .child(
                            div()
                                .text_size(px(15.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(AMBER))
                                .child(balance.clone())
                                .a11y(format!("{card_id}-balance"), Role::Label, balance),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(AMBER_DIM))
                                .child("sats"),
                        ),
                )
                .child(
                    h_flex()
                        .mt(px(8.))
                        .gap(px(12.))
                        .when(!is_active, |this| {
                            let pubkey = pubkey.clone();
                            this.child(outline_button(
                                format!("{card_id}-switch"),
                                "Switch",
                                TEXT,
                                cx,
                                move |this, window, cx| this.set_account(&pubkey, window, cx),
                            ))
                        })
                        .child(outline_button(
                            format!("{card_id}-manage"),
                            "Manage",
                            ACCENT_LIGHT,
                            cx,
                            move |this, window, cx| {
                                this.open_account(&pubkey, Tab::Identity, window, cx);
                            },
                        )),
                ),
        )
        .into_any_element()
}

// ------------------------------------------------------------- Account page

/// The Account screen: the mock's `isAccountDetail` block — back link,
/// header, tab strip, and the tab's body. Identity & keys and Trust are
/// built; the other three tabs say "Coming soon."
pub(crate) fn render_account(shell: &Shell, cx: &mut Context<Shell>) -> AnyElement {
    let Some(page) = &shell.account_page else {
        return div()
            .text_size(px(12.5))
            .text_color(rgb(TEXT_MUTED))
            .child("No account selected.")
            .a11y("account-none", Role::Label, "No account selected.")
            .into_any_element();
    };
    let pubkey = page.pubkey.clone();
    let tab = page.tab;
    let Some(view) = shell.view(&pubkey) else {
        return div()
            .text_size(px(12.5))
            .text_color(rgb(TEXT_MUTED))
            .child("This account was removed.")
            .a11y("account-removed", Role::Label, "This account was removed.")
            .into_any_element();
    };
    let is_active = shell.active.as_deref() == Some(pubkey.as_str());
    let npub = view.npub.clone();
    let ix = shell
        .views()
        .enumerate()
        .find(|(_, v)| v.pubkey == pubkey)
        .map(|(ix, _)| ix)
        .unwrap_or(0);

    v_flex()
        .w_full()
        .child(
            div()
                .id("back-to-accounts")
                .mb(px(16.))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .hover(|this| this.text_color(rgb(TEXT)))
                .child("← All accounts")
                .a11y("account-back", Role::Button, "← All accounts")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.go(Screen::Accounts, window, cx);
                })),
        )
        .child(
            h_flex()
                .items_center()
                .gap(px(16.))
                .mb(px(22.))
                .child(Shell::account_marker(view, ix, 52., 18.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            h_flex()
                                .gap(px(10.))
                                .items_center()
                                .child(
                                    div()
                                        .text_size(px(21.))
                                        .font_weight(FontWeight::BOLD)
                                        .child(SharedString::from(display_name(view)))
                                        .a11y("account-title", Role::Heading, display_name(view)),
                                )
                                .when(is_active, |this| {
                                    this.child(
                                        div()
                                            .px(px(8.))
                                            .py(px(2.))
                                            .rounded_full()
                                            .bg(wash(0x4ecb8d22))
                                            .text_size(px(10.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(GREEN))
                                            .child("active")
                                            .a11y("account-active", Role::Label, "active"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .mt(px(2.))
                                .font_family(MONO)
                                .text_size(px(12.5))
                                .text_color(rgb(TEXT_MUTED))
                                .child(short_npub(&npub))
                                .a11y("account-npub", Role::Label, short_npub(&npub)),
                        ),
                )
                .child({
                    let url = format!("https://brainstorm.world/profile/{npub}");
                    div()
                        .id("account-view-profile")
                        .flex_shrink_0()
                        .px(px(16.))
                        .py(px(8.))
                        .rounded(px(9.))
                        .border_1()
                        .border_color(rgb(BORDER_3))
                        .cursor_pointer()
                        .text_size(px(13.))
                        .text_color(rgb(ACCENT_LIGHT))
                        .hover(|this| this.border_color(rgb(ACCENT)))
                        .child("View profile")
                        .a11y("account-view-profile", Role::Link, "View profile")
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                })
                .when(!is_active, |this| {
                    let pubkey = pubkey.clone();
                    this.child(outline_button(
                        "account-switch",
                        "Switch to this account",
                        TEXT,
                        cx,
                        move |this, window, cx| this.set_account(&pubkey, window, cx),
                    ))
                }),
        )
        .child(
            h_flex()
                .gap(px(2.))
                .mb(px(18.))
                .border_b_1()
                .border_color(rgb(BORDER))
                .a11y("account-tabs", Role::TabList, "Account sections")
                .children(
                    [
                        Tab::Identity,
                        Tab::Trust,
                        Tab::Progress,
                        Tab::Wallet,
                        Tab::Limits,
                    ]
                    .into_iter()
                    .map(|item| {
                        let on = tab == item;
                        div()
                            .id(item.id())
                            .px(px(14.))
                            .py(px(8.))
                            .mb(px(-1.))
                            .cursor_pointer()
                            .text_size(px(13.))
                            .text_color(if on { rgb(TEXT) } else { rgb(TEXT_MUTED) })
                            .border_b_2()
                            .border_color(if on { rgb(ACCENT) } else { rgba(0x00000000) })
                            .child(item.label())
                            .a11y(item.id(), Role::Tab, item.label())
                            .aria_selected(on)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(page) = &mut this.account_page {
                                    if page.tab != item {
                                        page.hide_nsec();
                                    }
                                    page.tab = item;
                                }
                                cx.notify();
                            }))
                    }),
                ),
        )
        .child(match tab {
            Tab::Identity => render_identity(shell, &pubkey, &npub, cx),
            Tab::Trust => render_trust(shell, &pubkey, cx),
            _ => div()
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("Coming soon.")
                .a11y(
                    match tab {
                        Tab::Progress => "account-progress-soon",
                        Tab::Wallet => "account-wallet-soon",
                        _ => "account-limits-soon",
                    },
                    Role::Label,
                    "Coming soon.",
                )
                .into_any_element(),
        })
        .into_any_element()
}

// ---------------------------------------------------------- Identity tab

/// The mock's Identity & keys tab: public identity, the nsec export, and
/// the remove card.
fn render_identity(
    shell: &Shell,
    pubkey: &str,
    npub: &str,
    cx: &mut Context<Shell>,
) -> AnyElement {
    v_flex()
        .gap(px(16.))
        .child(public_identity_card(shell, npub, cx))
        .child(secret_key_card(shell, pubkey, cx))
        .child(remove_card(shell, pubkey, cx))
        .into_any_element()
}

/// The card title the Identity tab uses: no kind, no pill.
fn plain_title(id: &'static str, title: &'static str) -> impl IntoElement {
    div()
        .mb(px(4.))
        .text_size(px(14.5))
        .font_weight(FontWeight::SEMIBOLD)
        .child(title)
        .a11y(id, Role::Heading, title)
}

fn card_note(text: &'static str) -> impl IntoElement {
    div()
        .mb(px(12.))
        .text_size(px(12.5))
        .text_color(rgb(TEXT_MUTED))
        .child(text)
}

/// The dark inset row a key sits in, with its buttons at the right.
fn key_row(border: u32) -> Div {
    h_flex()
        .gap(px(12.))
        .items_center()
        .bg(rgb(BG_APP))
        .border_1()
        .border_color(rgba(border))
        .rounded(px(10.))
        .px(px(14.))
        .py(px(12.))
}

/// The key text: mono, and free to wrap anywhere inside the row.
fn key_text(
    id: &'static str,
    label: impl Into<SharedString>,
    text: impl Into<SharedString>,
    color: u32,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_w(px(0.))
        .font_family(MONO)
        .text_size(px(12.5))
        .text_color(rgb(color))
        .child(text.into())
        .a11y(id, Role::Label, label)
}

/// A filled small button ("Reveal", "Hide").
fn fill_button(
    id: &'static str,
    label: &'static str,
    color: u32,
    cx: &mut Context<Shell>,
    on_click: impl Fn(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_shrink_0()
        .px(px(14.))
        .py(px(7.))
        .rounded(px(7.))
        .bg(rgb(BG_NAV_ON))
        .cursor_pointer()
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(color))
        .hover(|this| this.bg(rgb(0x32324c)))
        .child(label)
        .a11y(id, Role::Button, label)
        .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
}

fn public_identity_card(shell: &Shell, npub: &str, cx: &mut Context<Shell>) -> AnyElement {
    let copied = shell
        .account_page
        .as_ref()
        .is_some_and(|page| page.copied.as_deref() == Some("npub"));
    let copy = npub.to_string();
    card()
        .child(plain_title("account-public-title", "Public identity"))
        .child(card_note(
            "Safe to share — this is how vouchers and bounty lists find you.",
        ))
        .child(
            key_row(0x2a2a40ff)
                .child(key_text("account-npub-text", npub.to_string(), npub.to_string(), TEXT))
                .child(outline_button(
                    "account-copy-npub",
                    if copied { "copied" } else { "copy" },
                    ACCENT_LIGHT,
                    cx,
                    move |this, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        if let Some(page) = &mut this.account_page {
                            page.copied = Some("npub".into());
                        }
                        cx.notify();
                    },
                )),
        )
        .child(
            div()
                .mt(px(12.))
                .child(crate::qr::view(crate::qr::encode(&crate::qr::npub_link(npub)), 160.)),
        )
        .child(div().mt(px(12.)).child(open_link(
            "account-identity-view-profile",
            "View this profile on Brainstorm ↗",
            format!("https://brainstorm.world/profile/{npub}"),
            cx,
        )))
        .into_any_element()
}

fn secret_key_card(shell: &Shell, pubkey: &str, cx: &mut Context<Shell>) -> AnyElement {
    let page = shell.account_page.as_ref();
    let copied = page.is_some_and(|page| page.copied.as_deref() == Some("nsec"));
    let revealed = page.and_then(|page| page.revealed.as_ref());
    let copy_button = {
        let pubkey = pubkey.to_string();
        outline_button(
            "account-copy-nsec",
            if copied { "copied" } else { "copy" },
            ACCENT_LIGHT,
            cx,
            move |this, _, cx| this.copy_nsec(&pubkey, cx),
        )
        .into_any_element()
    };
    let body = match revealed {
        None => {
            let pubkey = pubkey.to_string();
            key_row(0x2a2a40ff)
                .child(key_text(
                    "account-nsec-text",
                    "nsec1••••••••••••••••••••••••••••••••••••••••••••••••••••",
                    "nsec1••••••••••••••••••••••••••••••••••••••••••••••••••••",
                    TEXT_DIM,
                ))
                .child(fill_button(
                    "account-reveal-nsec",
                    "Reveal",
                    TEXT,
                    cx,
                    move |this, _, cx| this.reveal_nsec(&pubkey, cx),
                ))
                .child(copy_button)
                .into_any_element()
        }
        Some(revealed) => v_flex()
            .child(
                key_row(0xe5646c55)
                    // The one place the nsec is formatted for display.
                    // The label is fixed: the key itself never reaches the tree.
                    .child(key_text(
                        "account-nsec-text",
                        "Secret key",
                        revealed.nsec.expose().to_string(),
                        0xf0c9cc,
                    ))
                    .child(copy_button)
                    .child(fill_button(
                        "account-hide-nsec",
                        "Hide",
                        TEXT_MUTED,
                        cx,
                        |this, _, cx| {
                            if let Some(page) = &mut this.account_page {
                                page.hide_nsec();
                            }
                            cx.notify();
                        },
                    )),
            )
            .child(
                h_flex()
                    .mt(px(10.))
                    .gap(px(10.))
                    .items_start()
                    .bg(wash(0xe5646c14))
                    .border_1()
                    .border_color(rgba(0xe5646c44))
                    .rounded(px(9.))
                    .px(px(13.))
                    .py(px(11.))
                    .text_size(px(12.5))
                    .text_color(rgb(0xf0a9ae))
                    .child(
                        div()
                            .text_color(rgb(RED))
                            .font_weight(FontWeight::BOLD)
                            .child("!"),
                    )
                    .child({
                        let warning = format!(
                            "Anyone with this key is this identity — they can spend from \
                             its wallet connection and sign as you. Paste it only into a \
                             signer you trust (a browser extension or native app), never \
                             into a website. It hides again in {}s.",
                            revealed.seconds_left()
                        );
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(warning.clone())
                            .a11y("account-nsec-warning", Role::Label, warning)
                    }),
            )
            .into_any_element(),
    };
    card()
        .child(plain_title("account-secret-title", "Secret key (nsec)"))
        .child(card_note(
            "Stored on this computer in accounts.json. Export it to move this \
             identity into another signer — a browser extension like Alby or \
             nos2x, or another machine.",
        ))
        .child(body)
        .into_any_element()
}

/// The two-click remove, the same flow as the sidebar's link.
fn remove_card(shell: &Shell, pubkey: &str, cx: &mut Context<Shell>) -> AnyElement {
    let armed = shell.remove_armed(pubkey);
    let sats_left = shell.remove_warning(pubkey);
    let pubkey = pubkey.to_string();
    card()
        .border_color(rgba(0xe5646c33))
        .child(
            h_flex()
                .gap(px(16.))
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_size(px(14.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0xf0a9ae))
                                .child("Remove this account")
                                .a11y("account-remove-title", Role::Heading, "Remove this account"),
                        )
                        .child(
                            div()
                                .mt(px(2.))
                                .text_size(px(12.5))
                                .text_color(rgb(TEXT_MUTED))
                                .child(
                                    "Deletes the nsec and Coinos login from accounts.json. \
                                     Export the nsec first if you ever want this identity back.",
                                ),
                        )
                        .when_some(sats_left, |this, warning| {
                            this.child(
                                div()
                                    .mt(px(6.))
                                    .text_size(px(12.5))
                                    .text_color(rgb(AMBER))
                                    .child(warning.clone())
                                    .a11y("account-remove-warning", Role::Label, warning),
                            )
                        }),
                )
                .child(
                    div()
                        .id("account-remove")
                        .flex_shrink_0()
                        .px(px(16.))
                        .py(px(8.))
                        .rounded(px(9.))
                        .border_1()
                        .border_color(rgba(if armed { 0xe5646cff } else { 0xe5646c66 }))
                        .when(armed, |this| this.bg(wash(0xe5646c14)))
                        .cursor_pointer()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(RED))
                        .hover(|this| this.bg(wash(0xe5646c14)))
                        .child(if armed { "Click again to remove" } else { "Remove…" })
                        .a11y(
                            "account-remove",
                            Role::Button,
                            if armed { "Click again to remove" } else { "Remove…" },
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_account(pubkey.clone(), cx)
                        })),
                ),
        )
        .into_any_element()
}

// ------------------------------------------------------------- Trust tab

/// The five Trust cards for the page's account.
fn render_trust(shell: &Shell, pubkey: &str, cx: &mut Context<Shell>) -> AnyElement {
    let now = timefmt::now_unix();
    let empty = TrustState::default();
    let state = shell.trust.get(pubkey).unwrap_or(&empty);
    v_flex()
        .gap(px(16.))
        .child(map_card(shell, pubkey, state, now, cx))
        .child(assertions_card(shell, pubkey, state, now, cx))
        .child(min_rank_card())
        .child(determination_card(shell, pubkey))
        .child(web_of_trust_card(shell, pubkey, state, cx))
        .into_any_element()
}

/// The group's uppercase dim label.
fn group_label(id: &'static str, text: &'static str) -> impl IntoElement {
    div()
        .mt(px(16.))
        .text_size(px(9.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(TEXT_DIM))
        .child(text)
        .a11y(id, Role::Heading, text)
}

/// One assertion's fact grid, shared by both perspectives: located count,
/// most recent, rank pill, hops, and the raw toggle.
fn assertion_facts(
    prefix: &'static str,
    assertion: &Assertion,
    raw_key: &'static str,
    own_pov: bool,
    now: u64,
    page: &AccountPage,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let hops_line = match assertion.hops {
        Some(hops) => {
            let steps = if hops == 1 { "step" } else { "steps" };
            let from = if own_pov { "you" } else { "the issuer" };
            format!("{hops} {steps} from {from}")
        }
        None => "not linked by follows".to_string(),
    };
    let located = format!("1 current score from {prefix}");
    let recent = format!(
        "{} — {}",
        timefmt::relative(now, assertion.at),
        timefmt::month_day_year(assertion.at)
    );
    let rank_text = format!("rank {}", assertion.rank);
    v_flex()
        .mt(px(12.))
        .gap(px(9.))
        .child(fact(
            "Located",
            div()
                .child(located.clone())
                .a11y(format!("account-trust-{raw_key}-located"), Role::Label, located),
        ))
        .child(fact(
            "Most recent",
            div()
                .child(recent.clone())
                .a11y(format!("account-trust-{raw_key}-recent"), Role::Label, recent),
        ))
        .child(fact(
            if own_pov { "Rank" } else { "Current rank" },
            h_flex()
                .gap(px(10.))
                .items_center()
                .child(
                    div()
                        .px(px(10.))
                        .py(px(2.))
                        .rounded_full()
                        .bg(wash(0x4ecb8d1a))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(GREEN))
                        .child(rank_text.clone())
                        .a11y(format!("account-trust-{raw_key}-rank"), Role::Label, rank_text),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(TEXT_MUTED))
                        .child(hops_line.clone())
                        .a11y(format!("account-trust-{raw_key}-hops"), Role::Label, hops_line),
                ),
        ))
        .when(own_pov && assertion.hops == Some(0), |this| {
            this.child(
                div()
                    .text_size(px(11.5))
                    .text_color(rgb(TEXT_DIM))
                    .child("This is your own point of view. Your score provider scores everyone from your view, you included.")
                    .a11y(
                        "account-trust-own-pov-note",
                        Role::Label,
                        "This is your own point of view. Your score provider scores everyone from your view, you included.",
                    ),
            )
        })
        .child(raw_toggle(
            raw_key,
            "most recent event",
            &assertion.raw,
            page.open_raw.contains(raw_key),
            cx,
        ))
        .into_any_element()
}

/// A group's failure or empty line, plus the "Check again" button.
fn assertion_note(
    key: &'static str,
    pubkey: &str,
    text: String,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let pubkey = pubkey.to_string();
    v_flex()
        .mt(px(12.))
        .gap(px(10.))
        .items_start()
        .child(
            div()
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(text.clone()))
                .a11y(format!("account-trust-{key}-error"), Role::Label, text),
        )
        .child(outline_button(format!("account-trust-{key}-check-again"), "Check again", TEXT_MUTED, cx, move |this, _, cx| {
            this.fetch_trust(&pubkey);
            cx.notify();
        }))
        .into_any_element()
}

/// One assertion group's body: located facts, the none-found line, the
/// failure line, or a quiet "checking".
#[allow(clippy::too_many_arguments)]
fn assertion_group_body(
    pubkey: &str,
    evidence: &Evidence<Option<Assertion>>,
    prefix: &'static str,
    own_pov: bool,
    raw_key: &'static str,
    page: &AccountPage,
    now: u64,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let none_found = if own_pov {
        "No kind-30382 score events about this npub were found from the rank provider."
    } else {
        "No score about this npub from the instance's rank provider yet. A new identity has rank 0 until someone vouches for it — send your npub to a voucher."
    };
    let body: AnyElement = match &evidence.last {
        Some(Some(assertion)) => {
            assertion_facts(prefix, assertion, raw_key, own_pov, now, page, cx).into_any_element()
        }
        Some(None) => div()
            .mt(px(12.))
            .text_size(px(12.5))
            .text_color(rgb(TEXT_MUTED))
            .child(none_found)
            .a11y(format!("account-trust-{raw_key}-none"), Role::Label, none_found)
            .into_any_element(),
        None => match &evidence.status {
            Status::Failed(message) => {
                return assertion_note(raw_key, pubkey, message.clone(), cx);
            }
            _ => div()
                .mt(px(12.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_DIM))
                .child("Checking…")
                .a11y(format!("account-trust-{raw_key}-checking"), Role::Label, "Checking…")
                .into_any_element(),
        },
    };
    // A failed re-read appends its line under the facts it kept.
    match &evidence.status {
        Status::Failed(message) => v_flex()
            .child(body)
            .child(assertion_note(raw_key, pubkey, message.clone(), cx))
            .into_any_element(),
        _ => body,
    }
}

/// Card 1: the kind-10040 Treasure Map — locatable, withdrawn, or missing.
fn map_card(
    shell: &Shell,
    pubkey: &str,
    state: &TrustState,
    now: u64,
    cx: &mut Context<Shell>,
) -> AnyElement {
    const MAP_STATUS: &str = "account-trust-map-status";
    let page = shell.account_page.as_ref().expect("account page on screen");
    let designation = state.designation.last.as_ref();
    let failed_message = state.map_read_failed();

    let pill = match designation {
        Some(Designation::Set { .. }) => status_pill(MAP_STATUS, "Located", GREEN, 0x4ecb8d1a),
        Some(Designation::None | Designation::Deactivated { .. }) => {
            status_pill(MAP_STATUS, "Not located", RED, 0xe5646c1a)
        }
        None if failed_message.is_some() => {
            status_pill(MAP_STATUS, "Not located", TEXT_DIM, 0x5b597522)
        }
        None => status_pill(MAP_STATUS, "Checking", TEXT_DIM, 0x5b597522),
    };

    let mut body = card()
        .child(card_head(
            "account-trust-map-title",
            "Treasure Map",
            "kind 10040",
            pill,
        ))
        .child(
            div()
                .mt(px(6.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("Tells other apps which provider's scores to trust for each kind of assertion, and where to find them. It must be locatable, or nobody can look up your rank."),
        );

    // The verdict's own content — a prior answer stays up through a failed
    // re-read, so this renders from `last` alone.
    match designation {
        Some(Designation::Set {
            rows,
            at,
            found_on,
            raw,
        }) => {
            let rank_row = rows.iter().find(|row| row.kind_tag == "30382:rank");
            let types = rows
                .iter()
                .map(|row| row.kind_tag.as_str())
                .collect::<Vec<_>>()
                .join(" · ");
            let entries_text = if types.is_empty() {
                "no rank entry".to_string()
            } else if rank_row.is_none() {
                format!("available — {types} · no rank entry")
            } else {
                format!("available — {types}")
            };
            let entries: AnyElement = if types.is_empty() {
                div()
                    .text_color(rgb(AMBER))
                    .child("no rank entry")
                    .a11y("account-trust-map-entries", Role::Label, entries_text)
                    .into_any_element()
            } else {
                h_flex()
                    .items_baseline()
                    .gap(px(4.))
                    .text_color(rgb(GREEN))
                    .child("available —")
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .child(SharedString::from(types)),
                    )
                    .when(rank_row.is_none(), |this| {
                        this.child(div().text_color(rgb(AMBER)).child("· no rank entry"))
                    })
                    .a11y("account-trust-map-entries", Role::Label, entries_text)
                    .into_any_element()
            };
            let published = format!(
                "{} — {}",
                timefmt::relative(now, *at),
                timefmt::month_day_year(*at)
            );
            body = body.child(
                v_flex()
                    .mt(px(14.))
                    .gap(px(9.))
                    .child(fact(
                        "Published",
                        div()
                            .child(published.clone())
                            .a11y("account-trust-map-published", Role::Label, published),
                    ))
                    .child(fact(
                        "Found on",
                        div()
                            .font_family(MONO)
                            .text_size(px(12.))
                            .child(SharedString::from(found_on.join(", ")))
                            .a11y("account-trust-map-found-on", Role::Label, found_on.join(", ")),
                    ))
                    .child(fact("Score entries", entries)),
            );
            if let Some(row) = rank_row {
                let copied = page.copied.as_deref() == Some("rank-provider");
                let key = row.key.clone();
                let brainstorm_line = match state.brainstorm.last.as_ref() {
                    Some(BrainstormKey::Assigned { key: assigned, .. }) => {
                        if *assigned == row.key {
                            "matches your assigned key".to_string()
                        } else {
                            format!("differs from your assigned key {assigned}")
                        }
                    }
                    Some(BrainstormKey::NoAccount) => {
                        "no Brainstorm account for this key".to_string()
                    }
                    // Still in flight reads as "checking", not as a failure.
                    None => match &state.brainstorm.status {
                        Status::Failed(_) => "couldn't check".to_string(),
                        _ => "checking…".to_string(),
                    },
                };
                body = body.child(
                    v_flex()
                        .gap(px(9.))
                        .child(fact(
                            "Rank provider",
                            h_flex()
                                .gap(px(10.))
                                .items_baseline()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .font_family(MONO)
                                        .text_size(px(12.))
                                        .child(SharedString::from(row.key.clone()))
                                        .a11y(
                                            "account-trust-map-rank-provider",
                                            Role::Label,
                                            row.key.clone(),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("account-trust-map-copy-rank-provider")
                                        .flex_shrink_0()
                                        .cursor_pointer()
                                        .text_size(px(12.))
                                        .text_color(rgb(ACCENT_LIGHT))
                                        .child(if copied { "copied" } else { "copy" })
                                        .a11y(
                                            "account-trust-map-copy-rank-provider",
                                            Role::Button,
                                            if copied { "copied" } else { "copy" },
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                key.clone(),
                                            ));
                                            if let Some(page) = &mut this.account_page {
                                                page.copied = Some("rank-provider".into());
                                            }
                                            cx.notify();
                                        })),
                                ),
                        ))
                        .child(fact(
                            "Provider relay",
                            div()
                                .font_family(MONO)
                                .text_size(px(12.))
                                .child(SharedString::from(row.relay.clone()))
                                .a11y(
                                    "account-trust-map-provider-relay",
                                    Role::Label,
                                    row.relay.clone(),
                                ),
                        ))
                        .child(fact(
                            "Brainstorm key",
                            div()
                                .child(brainstorm_line.clone())
                                .a11y("account-trust-map-brainstorm-key", Role::Label, brainstorm_line),
                        )),
                );
            }
            body = body.child(raw_toggle(
                "map",
                "raw event",
                raw,
                page.open_raw.contains("map"),
                cx,
            ));
        }
        Some(Designation::Deactivated { at, raw }) => {
            let deactivated = format!(
                "Your Treasure Map was published empty on {}.",
                timefmt::month_day_year(*at)
            );
            body = body
                .child(
                    div()
                        .mt(px(14.))
                        .text_size(px(12.5))
                        .text_color(rgb(0xf0a9ae))
                        .child(deactivated.clone())
                        .a11y("account-trust-map-deactivated", Role::Label, deactivated),
                )
                .child(
                    v_flex()
                        .mt(px(14.))
                        .gap(px(14.))
                        .child(missing_map_note(cx))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .text_color(rgb(TEXT_MUTED))
                                .child("Good news: you can do the three needed steps right here."),
                        )
                        // The Trusted Community steps live inside the "Not
                        // located" card — the fix happens right here.
                        .child(community::steps(shell, pubkey, cx)),
                )
                .child(raw_toggle(
                    "map",
                    "raw event",
                    raw,
                    page.open_raw.contains("map"),
                    cx,
                ));
        }
        Some(Designation::None) => {
            body = body.child(
                v_flex()
                    .mt(px(14.))
                    .gap(px(14.))
                    .child(missing_map_note(cx))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(TEXT_MUTED))
                            .child("Good news: you can do the three needed steps right here."),
                    )
                    .child(community::steps(shell, pubkey, cx)),
            );
        }
        None if failed_message.is_none() => {
            body = body.child(
                div()
                    .mt(px(14.))
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_MUTED))
                    .child("Checking Nostr servers…")
                    .a11y("account-trust-map-checking", Role::Label, "Checking Nostr servers…"),
            );
        }
        None => {}
    }

    // A failed read adds its note under whatever verdict survived.
    if let Some(message) = failed_message {
        let checked = match state.checked_at {
            Some(at) => format!(
                "Last successful check: {} — {}",
                timefmt::relative(now, at),
                timefmt::month_day_year(at)
            ),
            None => "Never checked successfully".to_string(),
        };
        body = body.child(
            div()
                .mt(px(14.))
                .bg(rgb(BG_APP))
                .border_1()
                .border_color(rgb(BORDER_2))
                .rounded(px(9.))
                .px(px(13.))
                .py(px(11.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child(format!("{message} {checked}"))
                .a11y("account-trust-map-error", Role::Label, format!("{message} {checked}")),
        );
    }

    // "Check again" under anything that is not Located — including a failed
    // re-read that still shows a prior Located verdict.
    if !matches!(designation, Some(Designation::Set { .. })) || failed_message.is_some() {
        let pubkey = pubkey.to_string();
        body = body.child(
            h_flex().mt(px(12.)).child(outline_button(
                "account-trust-map-check-again",
                "Check again",
                TEXT_MUTED,
                cx,
                move |this, _, cx| {
                    this.fetch_trust(&pubkey);
                    cx.notify();
                },
            )),
        );
    }

    body.into_any_element()
}

/// Card 2: the kind-30382 assertions — the account's own provider and the
/// instance issuer's provider, as two labelled groups.
fn assertions_card(
    shell: &Shell,
    pubkey: &str,
    state: &TrustState,
    now: u64,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let page = shell.account_page.as_ref().expect("account page on screen");

    const STATUS: &str = "account-trust-assertions-status";
    // The card's pill and headline follow the instance group (goal §5.4).
    let pill = match &state.instance.last {
        Some(Some(_)) => status_pill(STATUS, "Located", GREEN, 0x4ecb8d1a),
        Some(None) => status_pill(STATUS, "None found", AMBER, 0xf2b5441a),
        None => match state.instance.status {
            // A failed read is not an absence — grey, like "Checking".
            Status::Failed(_) => status_pill(STATUS, "Couldn’t check", TEXT_DIM, 0x5b597522),
            _ => status_pill(STATUS, "Checking", TEXT_DIM, 0x5b597522),
        },
    };

    let mut body = card()
        .child(card_head(
            "account-trust-assertions-title",
            "Trusted Assertions",
            "kind 30382",
            pill,
        ))
        .child(
            div()
                .mt(px(6.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("Signed score events about this npub, published by the rank provider. They are what issuers check before paying your claims."),
        );

    // The account's own provider — only when the map names a rank row.
    let has_rank_row = state
        .designation
        .last
        .as_ref()
        .and_then(Designation::rank_row)
        .is_some();
    if has_rank_row {
        body = body
            .child(group_label("account-trust-own-title", "From your score provider"))
            .child(assertion_group_body(
                pubkey,
                &state.own,
                "your score provider",
                true,
                "own",
                page,
                now,
                cx,
            ));
    }

    // The instance issuer's provider — always shown.
    let provider_line = match &state.instance_provider {
        Some((key, relay)) => format!(
            "Scores seen by {}, the issuer account this server pays for. Provider {} at {relay}.",
            issuer_short_npub(),
            short_id(key)
        ),
        None => format!(
            "Scores seen by {}, the issuer account this server pays for.",
            issuer_short_npub()
        ),
    };
    body = body
        .child(group_label(
            "account-trust-instance-title",
            "From the issuer's score provider",
        ))
        .child(
            div()
                .mt(px(6.))
                .text_size(px(11.5))
                .text_color(rgb(TEXT_DIM))
                .child(provider_line.clone())
                .a11y("account-trust-instance-provider", Role::Label, provider_line),
        )
        .child(assertion_group_body(
            pubkey,
            &state.instance,
            "the server's score provider",
            false,
            "instance",
            page,
            now,
            cx,
        ));

    body.into_any_element()
}

/// Card 3: the instance's fixed minimum rank. Read-only — it is the
/// instance's rule, not this account's setting.
fn min_rank_card() -> AnyElement {
    card()
        .child(
            h_flex()
                .gap(px(10.))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Minimum rank")
                        .a11y("account-trust-min-rank-title", Role::Heading, "Minimum rank"),
                )
                .child(
                    div()
                        .px(px(14.))
                        .py(px(5.))
                        .rounded(px(9.))
                        .bg(rgb(BG_APP))
                        .border_1()
                        .border_color(rgb(BORDER_2))
                        .font_family(MONO)
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(trust::INSTANCE_MIN_RANK.to_string())
                        .a11y(
                            "account-trust-min-rank",
                            Role::Label,
                            trust::INSTANCE_MIN_RANK.to_string(),
                        ),
                ),
        )
        .child(
            div()
                .mt(px(8.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("A claimant must have at least this rank — a score between 0 and 100, published about them by the issuer's rank provider — before any bounty on this instance is paid. Individual bounties can demand a higher rank, never a lower one. Fixed at 2 by the instance for now; future versions will let you change it, or swap rank for a different score entirely."),
        )
        .into_any_element()
}

/// Card 4: this account's point of view and scoring method, read-only.
fn determination_card(shell: &Shell, pubkey: &str) -> AnyElement {
    let view = shell.view(pubkey);
    let ix = shell
        .views()
        .enumerate()
        .find(|(_, v)| v.pubkey == pubkey)
        .map(|(ix, _)| ix)
        .unwrap_or(0);
    card()
        .child(
            h_flex()
                .gap(px(8.))
                .items_center()
                .child(
                    div()
                        .text_size(px(14.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Trust Determination")
                        .a11y("account-trust-determination-title", Role::Heading, "Trust Determination"),
                )
                .child(
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded_full()
                        .bg(wash(ACCENT_WASH))
                        .text_size(px(9.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(ACCENT_LIGHT))
                        .child("this account"),
                ),
        )
        .child(
            div()
                .mt(px(4.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("The default point of view and scoring method this account uses wherever it computes trust. Individual Trusted List pages can override per list — their \"Restore default\" returns to these values."),
        )
        .child(
            v_flex()
                .mt(px(12.))
                .max_w(px(420.))
                .bg(rgb(BG_APP))
                .border_1()
                .border_color(rgb(BORDER_2))
                .rounded(px(10.))
                .px(px(16.))
                .py(px(14.))
                .child(
                    div()
                        .text_size(px(9.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_DIM))
                        .child("POINT OF VIEW"),
                )
                .child(
                    h_flex()
                        .mt(px(10.))
                        .gap(px(10.))
                        .items_center()
                        .children(view.map(|view| Shell::account_marker(view, ix, 34., 13.)))
                        .child(
                            v_flex()
                                .min_w(px(0.))
                                .child(
                                    h_flex()
                                        .gap(px(7.))
                                        .items_center()
                                        .child(
                                            div()
                                                .text_size(px(14.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(SharedString::from(
                                                    view.map(display_name).unwrap_or_default(),
                                                ))
                                                .a11y(
                                                    "account-trust-pov-name",
                                                    Role::Label,
                                                    view.map(display_name).unwrap_or_default(),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .px(px(7.))
                                                .py(px(1.))
                                                .rounded_full()
                                                .bg(wash(0x8f7df814))
                                                .border_1()
                                                .border_color(rgba(0x8f7df833))
                                                .text_size(px(9.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(rgb(ACCENT_LIGHT))
                                                .child("owner"),
                                        ),
                                )
                                .child(
                                    div()
                                        .font_family(MONO)
                                        .text_size(px(10.5))
                                        .text_color(rgb(TEXT_DIM))
                                        .child(SharedString::from(
                                            view.map(|v| v.npub.clone()).unwrap_or_default(),
                                        ))
                                        .a11y(
                                            "account-trust-pov-npub",
                                            Role::Label,
                                            view.map(|v| v.npub.clone()).unwrap_or_default(),
                                        ),
                                ),
                        ),
                ),
        )
        .child(
            h_flex()
                .mt(px(14.))
                .gap(px(9.))
                .items_center()
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(TEXT_DIM))
                        .child("Scoring method"),
                )
                .child(
                    div()
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .bg(rgb(BG_RAIL))
                        .border_1()
                        .border_color(rgb(BORDER))
                        .text_size(px(12.))
                        .child("Trusted Assertions (rank)")
                        .a11y(
                            "account-trust-scoring-method",
                            Role::Label,
                            "Trusted Assertions (rank)",
                        ),
                ),
        )
        .child(
            div()
                .mt(px(10.))
                .text_size(px(11.5))
                .text_color(rgb(TEXT_DIM))
                .child("You can't change this yet. The server scores every claim from the issuer's point of view."),
        )
        .into_any_element()
}

/// Card 5: following count and verified followers, each honest about which
/// read produced it.
fn web_of_trust_card(
    shell: &Shell,
    pubkey: &str,
    state: &TrustState,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let (following, following_note) = match &state.follows.last {
        Some(Some(n)) => (n.to_string(), None),
        Some(None) => ("—".to_string(), Some("no follow list found")),
        None => match state.follows.status {
            Status::Failed(_) => ("—".to_string(), Some("couldn't read")),
            _ => ("—".to_string(), None),
        },
    };
    let (followers, followers_note) = match &state.instance.last {
        Some(Some(assertion)) => match assertion.followers {
            Some(n) => (n.to_string(), None),
            None => ("—".to_string(), Some("followers count unavailable")),
        },
        Some(None) => ("—".to_string(), Some("no score yet")),
        None => match state.instance.status {
            Status::Failed(_) => ("—".to_string(), Some("couldn't read")),
            _ => ("—".to_string(), None),
        },
    };
    let npub = PublicKey::from_hex(pubkey)
        .ok()
        .and_then(|key| key.to_bech32().ok())
        .unwrap_or_else(|| pubkey.to_string());
    let profile_url = format!("https://brainstorm.world/profile/{npub}");

    card()
        .child(
            div()
                .text_size(px(14.5))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Web of trust")
                .a11y("account-trust-wot-title", Role::Heading, "Web of trust"),
        )
        .child(
            div()
                .mt(px(6.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("From this npub's contact list, and the signed follows pointing back at it."),
        )
        .child(
            h_flex()
                .mt(px(14.))
                .gap(px(12.))
                .max_w(px(420.))
                .child(wot_tile(
                    "account-trust-following",
                    &following,
                    "Following",
                    following_note,
                ))
                .child(wot_tile(
                    "account-trust-followers",
                    &followers,
                    "Verified followers",
                    followers_note,
                )),
        )
        .child(
            div()
                .mt(px(10.))
                .text_size(px(11.5))
                .text_color(rgb(TEXT_DIM))
                .child(format!(
                    "Verified followers as counted by {}.",
                    issuer_short_npub()
                )),
        )
        .child(
            div()
                .mt(px(14.))
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .child("Followed someone new? Ask Brainstorm to score your web of trust again from your follow list."),
        )
        .child(
            div()
                .mt(px(10.))
                .child(community::recalc_block(shell, pubkey, "account-trust-recalc", cx)),
        )
        .child(open_link(
            "account-trust-wot-brainstorm",
            "See the full graph on Brainstorm ↗",
            profile_url,
            cx,
        ))
        .into_any_element()
}

/// One "42 / Following" tile; the note explains a "—".
fn wot_tile(
    id: &'static str,
    value: &str,
    label: &'static str,
    note: Option<&'static str>,
) -> AnyElement {
    v_flex()
        .flex_1()
        .bg(rgb(BG_APP))
        .border_1()
        .border_color(rgb(BORDER_2))
        .rounded(px(10.))
        .px(px(16.))
        .py(px(14.))
        .child(
            div()
                .font_family(MONO)
                .text_size(px(22.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(value.to_string()))
                .a11y(id, Role::Label, value.to_string()),
        )
        .child(
            div()
                .mt(px(2.))
                .text_size(px(12.))
                .text_color(rgb(TEXT_MUTED))
                .child(label)
                .a11y(format!("{id}-label"), Role::Label, label),
        )
        .when_some(note, |this, note| {
            this.child(
                div()
                    .mt(px(2.))
                    .text_size(px(11.))
                    .text_color(rgb(TEXT_DIM))
                    .child(note)
                    .a11y(format!("{id}-note"), Role::Label, note),
            )
        })
        .into_any_element()
}
