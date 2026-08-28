//! First-launch onboarding: paste a Nostr key, or continue read-only.
//!
//! Shown only when NO account has a key anywhere (keychain or env) — the
//! decision is `shell::onboarding_needed`. The pasted key goes to the runtime
//! thread as a [`magic_carpet_chat::secrets::Secret`] inside
//! `Command::ImportKey`; this module never reads the keychain and never sees
//! the key again after the send. The input is masked, and a rejected paste is
//! answered with fixed text that never echoes what was typed.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};

use crate::dashboard::MONO;
use crate::palette::*;
use crate::shell::{Shell, short_npub};

/// What the import came back with — public material only, straight from
/// `Update::ImportReady`.
pub struct ImportSummary {
    pub npub: String,
    pub name: Option<String>,
    pub lud16: Option<String>,
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

fn muted(text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(12.))
        .text_color(rgb(TEXT_MUTED))
        .child(text.into())
}

/// The logo tile from the icon rail, drawn a little larger.
fn logo() -> impl IntoElement {
    let bar = |width: f32, alpha: u32| {
        div()
            .w(px(width))
            .h(px(5.))
            .rounded(px(2.5))
            .bg(rgba(0x0d0d1400 | alpha))
    };
    v_flex()
        .size(px(52.))
        .rounded(px(14.))
        .bg(grad(ACCENT_DEEP, ACCENT_PALE))
        .items_center()
        .justify_center()
        .gap(px(4.))
        .shadow(vec![
            BoxShadow::new(px(0.), px(6.), wash(ACCENT_GLOW)).blur_radius(px(22.)),
        ])
        .child(bar(26., 0xcc))
        .child(bar(18., 0x99))
        .child(bar(10., 0x66))
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
fn ready_panel(summary: &ImportSummary, cx: &mut Context<Shell>) -> AnyElement {
    let identity: SharedString = match &summary.name {
        Some(name) => format!("{name} · {}", short_npub(&summary.npub)).into(),
        None => short_npub(&summary.npub).into(),
    };
    let lud16_row = if let Some(lud16) = &summary.lud16 {
        dot_row(GREEN, format!("Lightning address: {lud16}"), TEXT)
    } else if !summary.profile_checked {
        dot_row(
            TEXT_DIM,
            "Could not check the profile right now. You can still continue.",
            TEXT_MUTED,
        )
    } else if !summary.profile_found {
        // No kind-0 at all: for the paste-existing-key path this usually
        // means the WRONG key (a hex public key parses as a plausible
        // secret key and lands exactly here).
        dot_row(
            AMBER,
            "No profile found for this key — double-check you pasted the \
             right one. A brand-new key can browse, but it will not be paid.",
            AMBER,
        )
    } else {
        // Consequence first, and at full strength: this is the single most
        // payout-critical fact on the screen.
        dot_row(
            AMBER,
            "Claims from this key will NOT be paid until your profile has a \
             Lightning address — add one in your Nostr app, then reopen \
             Magic Carpet.",
            AMBER,
        )
    };
    v_flex()
        .gap(px(8.))
        .child(dot_row(GREEN, "Key saved to this Mac's keychain.", TEXT))
        .child(
            h_flex()
                .gap(px(9.))
                .items_center()
                .pl(px(17.))
                .child(
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
        Some(summary) => ready_panel(summary, cx),
        None => v_flex()
            .gap(px(8.))
            .child(muted(
                "Paste your Nostr secret key. It stays in this Mac's keychain — \
                 the app only ever shares your public identity.",
            ))
            .child(muted(
                "It starts with nsec1 — copy it from your Nostr app \
                 (Settings > Keys in Primal or Damus).",
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
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(TEXT_DIM))
                    .child(if onboarding.submitting {
                        "Checking the key…"
                    } else {
                        "Enter continues"
                    }),
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
    v_flex()
        .flex_1()
        .min_h(px(0.))
        .items_center()
        .justify_center()
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
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(TEXT_MUTED))
                        .child("Claim items on live bounties and get paid in sats, automatically."),
                ),
        )
        .child(
            v_flex()
                .w(px(470.))
                .gap(px(14.))
                .child(key_card(shell, cx))
                .child(browse_card(cx)),
        )
}
