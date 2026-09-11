//! The account screen: create a new account, or paste an existing key. One
//! screen, two modes — first launch (no account stored; "just look around"
//! is the other way out) and add mode (the "+" in the rail; esc cancels).
//!
//! The secret goes to the runtime thread as a
//! [`magic_carpet_chat::secrets::Secret`] inside `Command::AddAccount`; this
//! module never reads the store and never sees the key again after the send.
//! The paste input is masked, and a rejected paste is answered with fixed
//! text that never echoes what was typed.

use gpui_kit::component::{
    Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::dashboard::{MONO, muted};
use crate::palette::*;
use crate::shell::{Shell, short_npub};
use crate::wallet;

/// Which card sits under the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Path {
    Choose,
    Create,
    Paste,
}

/// What the add came back with — public material only, straight from
/// `Update::AccountAdded`. The name and lud16 live on the account's
/// `AccountView`, where `ProfileLoaded` puts them.
pub struct ReadySummary {
    pub pubkey: String,
    pub npub: String,
    /// False when the profile probe itself failed: the lud16 line then says
    /// "couldn't check" instead of claiming there is none.
    pub profile_checked: bool,
    /// The probe worked but found no kind-0 at all — shown as "double-check
    /// the key", because a wrong paste (e.g. a hex public key) looks exactly
    /// like this.
    pub profile_found: bool,
}

/// The screen's state, owned by the shell like the forms are.
pub struct AccountSetup {
    /// No account stored: the browse card shows, and leaving without an
    /// account lands on the read-only dashboard.
    pub first_launch: bool,
    pub path: Path,
    /// The create path's optional display name.
    pub name: Entity<InputState>,
    /// The paste path's masked key input.
    pub key: Entity<InputState>,
    /// Fixed text from the runtime — never an echo of the input.
    pub error: Option<String>,
    /// Spinner text while the runtime works; Enter is ignored until it
    /// answers. The create path walks two steps: the key, then the wallet.
    pub working: Option<&'static str>,
    /// The create path sent `open_wallet`: `AccountAdded` is not the end of
    /// the wait, `WalletCreated` / `WalletFailed` is.
    pub awaiting_wallet: bool,
    /// The in-flight key was generated here, not pasted — a missing profile
    /// is then expected, not a wrong-key symptom.
    pub generated: bool,
    /// The account is stored and probed; Enter (or Start) finishes.
    pub ready: Option<ReadySummary>,
    _subs: Vec<Subscription>,
}

impl AccountSetup {
    pub fn new(first_launch: bool, window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("How others see you"));
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("nsec1… or 64-character hex")
        });
        let subs = [&name, &key]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this: &mut Shell, _, event, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.setup_enter(window, cx);
                    }
                })
            })
            .collect();
        Self {
            first_launch,
            path: Path::Choose,
            name,
            key,
            error: None,
            working: None,
            awaiting_wallet: false,
            generated: false,
            ready: None,
            _subs: subs,
        }
    }
}

fn card() -> Div {
    v_flex()
        .w_full()
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded(px(14.))
        .px(px(22.))
        .py(px(20.))
        .gap(px(10.))
}

fn card_title(text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(14.5))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

/// The logo tile from the icon rail, drawn a little larger.
fn logo() -> impl IntoElement {
    div()
        .size(px(52.))
        .rounded(px(14.))
        .overflow_hidden()
        .shadow(vec![
            BoxShadow::new(px(0.), px(6.), wash(ACCENT_GLOW)).blur_radius(px(22.)),
        ])
        .child(img(crate::icons::icon("logo")).size_full())
}

fn primary_button(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(18.))
        .py(px(9.))
        .rounded(px(9.))
        .bg(grad(ACCENT, ACCENT_DEEP))
        .cursor_pointer()
        .shadow(vec![
            BoxShadow::new(px(0.), px(4.), wash(ACCENT_GLOW)).blur_radius(px(14.)),
        ])
        .text_size(px(13.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(BG_RAIL))
        .text_center()
        .child(label)
}

fn quiet_button(id: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(14.))
        .py(px(8.))
        .rounded(px(9.))
        .border_1()
        .border_color(rgb(BORDER_3))
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(rgb(TEXT_MUTED))
        .text_center()
        .child(label)
}

fn dot_row(dot: u32, text: impl Into<SharedString>, color: u32) -> AnyElement {
    h_flex()
        .gap(px(9.))
        .items_start()
        .child(
            div()
                .size(px(8.))
                .mt(px(4.))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(dot)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(12.5))
                .text_color(rgb(color))
                .child(text.into()),
        )
        .into_any_element()
}

fn error_line(error: Option<String>) -> Option<AnyElement> {
    error.map(|error| {
        div()
            .text_size(px(12.))
            .text_color(rgb(RED))
            .child(SharedString::from(error))
            .into_any_element()
    })
}

fn back_link(cx: &mut Context<Shell>) -> AnyElement {
    div()
        .id("setup-back")
        .text_size(px(11.5))
        .text_color(rgb(TEXT_DIM))
        .cursor_pointer()
        .hover(|this| this.text_color(rgb(TEXT_MUTED)))
        .child("← Back")
        .on_click(cx.listener(|this, _, window, cx| this.setup_choose(Path::Choose, window, cx)))
        .into_any_element()
}

fn choice_card(cx: &mut Context<Shell>) -> AnyElement {
    card()
        .border_color(rgb(BORDER_3))
        .child(card_title("Your Nostr account"))
        .child(muted(
            "An account is a Nostr key. It stays on this Mac — the app only \
             ever shares your public identity.",
            TEXT_MUTED,
        ))
        .child(
            primary_button("setup-create", "Create a new account").on_click(cx.listener(
                |this, _, window, cx| this.setup_choose(Path::Create, window, cx),
            )),
        )
        .child(
            quiet_button("setup-paste", "I already have a key").on_click(cx.listener(
                |this, _, window, cx| this.setup_choose(Path::Paste, window, cx),
            )),
        )
        .into_any_element()
}

fn create_card(setup: &AccountSetup, cx: &mut Context<Shell>) -> AnyElement {
    card()
        .border_color(rgb(BORDER_3))
        .child(card_title("Create a new account"))
        .child(
            v_flex()
                .gap(px(3.))
                .child(
                    div()
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_DIM))
                        .child("Name (optional)"),
                )
                .child(div().text_size(px(12.5)).child(Input::new(&setup.name))),
        )
        // ponytail: the wallet is always on. A toggle would need a
        // create-without-wallet ready panel that then offers the wallet
        // button, which the paste path already is — use that instead.
        .child(dot_row(
            GREEN,
            "A Coinos Lightning wallet opens for this account, so claims can \
             pay you. Coinos holds the sats — keep the balance small.",
            TEXT_MUTED,
        ))
        .children(error_line(setup.error.clone()))
        .child(
            primary_button("setup-create-go", "Create — Enter")
                .on_click(cx.listener(|this, _, _, cx| this.setup_submit_create(cx))),
        )
        .child(back_link(cx))
        .into_any_element()
}

fn paste_card(setup: &AccountSetup, cx: &mut Context<Shell>) -> AnyElement {
    card()
        .border_color(rgb(BORDER_3))
        .child(card_title("Use your Nostr key"))
        .child(muted(
            "It starts with nsec1 — copy it from your Nostr app \
             (Settings > Keys in Primal or Damus).",
            TEXT_MUTED,
        ))
        .child(div().text_size(px(12.5)).child(Input::new(&setup.key)))
        .children(error_line(setup.error.clone()))
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(TEXT_DIM))
                .child("Enter continues"),
        )
        .child(back_link(cx))
        .into_any_element()
}

fn working_card(text: &'static str) -> AnyElement {
    card()
        .border_color(rgb(BORDER_3))
        .child(
            h_flex()
                .gap(px(10.))
                .items_center()
                .child(Spinner::new().small().color(rgb(TEXT_MUTED).into()))
                .child(div().text_size(px(13.)).child(text)),
        )
        .into_any_element()
}

/// The success panel: what the key resolved to, the wallet (address, QR,
/// the live "waiting for sats" line), and the Start button.
fn ready_card(shell: &Shell, summary: &ReadySummary, cx: &mut Context<Shell>) -> AnyElement {
    let generated = shell.setup.generated;
    let Some(view) = shell.view(&summary.pubkey) else {
        return working_card("Loading the account…");
    };
    let identity: SharedString = match &view.kind0_name {
        Some(name) => format!("{name} · {}", short_npub(&summary.npub)).into(),
        None => short_npub(&summary.npub).into(),
    };
    let wallet_box = |panel: AnyElement| {
        div()
            .mt(px(4.))
            .p(px(14.))
            .rounded(px(10.))
            .bg(rgb(BG_APP))
            .border_1()
            .border_color(rgb(BORDER))
            .child(panel)
    };
    // A wallet opened from this panel keeps the panel: its address and QR
    // are the point, and the lud16 it just published would otherwise hide it.
    let session_wallet = view.wallet.created.is_some() || view.wallet.error.is_some();
    let lud16_row = if session_wallet {
        wallet_box(wallet::panel(view, true, None, cx)).into_any_element()
    } else if let Some(lud16) = &view.lud16 {
        dot_row(GREEN, format!("Lightning address: {lud16}"), TEXT)
    } else if !summary.profile_checked {
        dot_row(
            TEXT_DIM,
            "Could not check the profile right now. You can still continue.",
            TEXT_MUTED,
        )
    } else {
        // No lud16, and we know it: instead of sending the person to another
        // app to add one, open a wallet right here. The panel shows the
        // address and a funding QR once the signup lands.
        v_flex()
            .gap(px(8.))
            .when(!summary.profile_found && !generated, |this| {
                // No kind-0 at all on the paste-existing-key path usually
                // means the WRONG key (a hex public key parses as a
                // plausible secret key and lands exactly here).
                this.child(dot_row(
                    AMBER,
                    "No profile found for this key — double-check you pasted the \
                     right one.",
                    AMBER,
                ))
            })
            .child(wallet_box(wallet::panel(view, true, None, cx)))
            .into_any_element()
    };
    card()
        .border_color(rgb(BORDER_3))
        .child(card_title("Account ready"))
        .child(
            h_flex().gap(px(9.)).items_center().child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(rgb(ACCENT_LIGHT))
                    .child(identity),
            ),
        )
        .child(lud16_row)
        .child(
            primary_button("setup-start", "Start — Enter")
                .mt(px(6.))
                .on_click(cx.listener(|this, _, window, cx| this.leave_account_setup(window, cx))),
        )
        .into_any_element()
}

fn browse_card(cx: &mut Context<Shell>) -> AnyElement {
    card()
        .child(card_title("Just look around"))
        .child(muted(
            "Browse the bounties read-only. The + in the rail adds an account later.",
            TEXT_MUTED,
        ))
        .child(
            quiet_button("setup-browse", "Continue without an account — esc")
                .mt(px(2.))
                .on_click(cx.listener(|this, _, window, cx| this.leave_account_setup(window, cx))),
        )
        .into_any_element()
}

fn cancel_card(cx: &mut Context<Shell>) -> AnyElement {
    card()
        .child(
            quiet_button("setup-cancel", "Cancel — esc")
                .on_click(cx.listener(|this, _, window, cx| this.leave_account_setup(window, cx))),
        )
        .into_any_element()
}

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> impl IntoElement {
    let setup = &shell.setup;
    let main = if let Some(text) = setup.working {
        working_card(text)
    } else if let Some(summary) = &setup.ready {
        ready_card(shell, summary, cx)
    } else {
        match setup.path {
            Path::Choose => choice_card(cx),
            Path::Create => create_card(setup, cx),
            Path::Paste => paste_card(setup, cx),
        }
    };
    // The way out without finishing: first launch browses read-only, add
    // mode goes back to whatever was on screen. Neither while the runtime is
    // mid-add, so the ready panel is never skipped by accident.
    let exit = if setup.working.is_some() || setup.ready.is_some() {
        None
    } else if setup.first_launch {
        Some(browse_card(cx))
    } else {
        Some(cancel_card(cx))
    };

    // Scrolls when the wallet panel makes the page taller than the window;
    // the auto vertical margin still centers it when it fits.
    div()
        .id("account-setup")
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .child(
            v_flex()
                .w_full()
                .items_center()
                .my_auto()
                .py(px(32.))
                .gap(px(26.))
                .child(
                    v_flex()
                        .items_center()
                        .gap(px(14.))
                        .child(logo())
                        .child(
                            div()
                                .text_size(px(22.))
                                .font_weight(FontWeight::BOLD)
                                .child("Magic Carpet"),
                        )
                        .child(div().text_size(px(13.)).text_color(rgb(TEXT_MUTED)).child(
                            "Claim items on live bounties and get paid in sats, automatically.",
                        )),
                )
                .child(
                    v_flex()
                        .w(px(470.))
                        .gap(px(14.))
                        .child(main)
                        .children(exit),
                ),
        )
}
