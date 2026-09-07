//! First-launch onboarding: paste a Nostr key, or continue read-only.
//!
//! Shown only when NO account has a key anywhere (keychain or env) — the
//! decision is `shell::onboarding_needed`. The pasted key goes to the runtime
//! thread as a [`magic_carpet_chat::secrets::Secret`] inside
//! `Command::ImportKey`; this module never reads the keychain and never sees
//! the key again after the send. The input is masked, and a rejected paste is
//! answered with fixed text that never echoes what was typed.

use gpui_kit::component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use magic_carpet_chat::secrets::Account;

use crate::dashboard::{MONO, muted};
use crate::palette::*;
use crate::shell::{Shell, short_npub};
use crate::wallet;

/// What the import came back with — public material only, straight from
/// `Update::ImportReady`. The name and lud16 live on the claimant's
/// `AccountView`, where `ProfileLoaded` put them just before.
pub struct ImportSummary {
    pub npub: String,
    /// False when the profile probe itself failed: the lud16 line then says
    /// "couldn't check" instead of claiming there is none.
    pub profile_checked: bool,
    /// The probe worked but found no kind-0 at all — shown as "double-check
    /// the key", because a wrong paste (e.g. a hex public key) looks exactly
    /// like this.
    pub profile_found: bool,
}

/// The onboarding screen's state, owned by the shell like the forms are.
pub struct Onboarding {
    pub input: Entity<InputState>,
    /// Fixed text from the runtime — never an echo of the input.
    pub error: Option<String>,
    /// An import is on the runtime thread; Enter is ignored until it answers.
    pub submitting: bool,
    /// The key is stored and probed; Enter (or the Start button) finishes.
    pub ready: Option<ImportSummary>,
    /// The in-flight key was generated here, not pasted — a missing profile
    /// is then expected, not a wrong-key symptom.
    pub generated: bool,
    _sub: Subscription,
}

impl Onboarding {
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("nsec1… or 64-character hex")
        });
        let sub = cx.subscribe_in(&input, window, |this: &mut Shell, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.onboarding_enter(window, cx);
            }
        });
        Self {
            input,
            error: None,
            submitting: false,
            ready: None,
            generated: false,
            _sub: sub,
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

/// The success panel: what the key resolved to, and the Start button.
fn ready_panel(shell: &Shell, summary: &ImportSummary, cx: &mut Context<Shell>) -> AnyElement {
    let generated = shell.onboarding.generated;
    let view = shell.view(Account::Claimant);
    let identity: SharedString = match &view.kind0_name {
        Some(name) => format!("{name} · {}", short_npub(&summary.npub)).into(),
        None => short_npub(&summary.npub).into(),
    };
    // A wallet opened from this panel keeps the panel: its address and QR
    // are the point, and the lud16 it just published would otherwise hide it.
    let lud16 = view.lud16.as_ref().filter(|_| view.wallet.created.is_none());
    let lud16_row = if let Some(lud16) = lud16 {
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
        let panel = wallet::panel(Account::Claimant, view, cx);
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
            .child(
                div()
                    .mt(px(4.))
                    .p(px(14.))
                    .rounded(px(10.))
                    .bg(rgb(BG_APP))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(panel),
            )
            .into_any_element()
    };
    v_flex()
        .gap(px(8.))
        .child(dot_row(GREEN, "Key saved to this Mac's keychain.", TEXT))
        .child(
            h_flex().gap(px(9.)).items_center().pl(px(17.)).child(
                div()
                    .font_family(MONO)
                    .text_size(px(12.))
                    .text_color(rgb(ACCENT_LIGHT))
                    .child(identity),
            ),
        )
        .child(lud16_row)
        .child(
            div()
                .id("onboarding-start")
                .mt(px(6.))
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
                .child("Start — Enter")
                .on_click(cx.listener(|this, _, window, cx| this.leave_onboarding(window, cx))),
        )
        .into_any_element()
}

fn key_card(shell: &Shell, cx: &mut Context<Shell>) -> AnyElement {
    let onboarding = &shell.onboarding;
    let body: AnyElement = match &onboarding.ready {
        Some(summary) => ready_panel(shell, summary, cx),
        None => v_flex()
            .gap(px(8.))
            .child(muted(
                "Paste your Nostr secret key. It stays in this Mac's keychain — \
                 the app only ever shares your public identity.",
                TEXT_MUTED,
            ))
            .child(muted(
                "It starts with nsec1 — copy it from your Nostr app \
                 (Settings > Keys in Primal or Damus).",
                TEXT_MUTED,
            ))
            .child(
                div()
                    .text_size(px(12.5))
                    .child(Input::new(&onboarding.input)),
            )
            .when_some(onboarding.error.clone(), |this, error| {
                this.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(RED))
                        .child(SharedString::from(error)),
                )
            })
            .child(div().text_size(px(11.)).text_color(rgb(TEXT_DIM)).child(
                if onboarding.submitting {
                    "Checking the key…"
                } else {
                    "Enter continues"
                },
            ))
            .child(
                div()
                    .id("onboarding-generate")
                    .mt(px(4.))
                    .px(px(14.))
                    .py(px(8.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(rgb(BORDER_3))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .text_color(rgb(TEXT_MUTED))
                    .text_center()
                    .child("No key yet? Create a new one")
                    .on_click(cx.listener(|this, _, _, cx| this.generate_onboarding_key(cx))),
            )
            .into_any_element(),
    };
    card()
        .border_color(rgb(BORDER_3))
        .child(card_title("Use your Nostr key"))
        .child(body)
        .into_any_element()
}

fn browse_card(cx: &mut Context<Shell>) -> AnyElement {
    card()
        .child(card_title("Just look around"))
        .child(muted(
            "Browse the bounties read-only. You can add a key later from the sidebar.",
            TEXT_MUTED,
        ))
        .child(
            div()
                .id("onboarding-browse")
                .mt(px(2.))
                .px(px(14.))
                .py(px(8.))
                .rounded(px(9.))
                .border_1()
                .border_color(rgb(BORDER_3))
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(rgb(TEXT_MUTED))
                .text_center()
                .child("Continue without a key — esc")
                .on_click(cx.listener(|this, _, window, cx| this.leave_onboarding(window, cx))),
        )
        .into_any_element()
}

pub fn render(shell: &Shell, cx: &mut Context<Shell>) -> impl IntoElement {
    // Scrolls when the wallet panel makes the page taller than the window;
    // the auto vertical margin still centers it when it fits.
    div()
        .id("onboarding")
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
                        .child(key_card(shell, cx))
                        .child(browse_card(cx)),
                ),
        )
}
