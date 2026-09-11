//! The wallet panel: the account's Lightning address, a QR to fund it and
//! the live balance — or the one button that opens a Coinos wallet when the
//! key has no address yet.
//!
//! Rendered in two places from the same state: the account screen's ready
//! panel (where a freshly generated key has no lud16 by definition) and the
//! Wallet screen (⌘6). The runtime does the signup and the kind-0 write; this
//! module only sends `Command::CreateCoinosWallet` and draws what came back.
//! The Coinos password never reaches this module — "Copy password" asks the
//! shell, which moves it from the store to the clipboard without rendering it.
//!
//! The Wallet screen also sends: a recipient, an amount, one button. The
//! inputs belong to the shell (one pair, cleared on an account switch); the
//! outcome lives on the account's [`WalletState`], where `Sent` and
//! `SendFailed` land by pubkey.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::{
    Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use qrcode::{Color, QrCode};

use magic_carpet_chat::coinos::lnurl_pay;
use magic_carpet_chat::nostr::{self, WalletCreated};

use crate::dashboard::{MONO, muted};
use crate::palette::*;
use crate::shell::{AccountView, Shell};
use crate::timefmt;

/// The wallet as the UI sees it, one per account view.
#[derive(Default)]
pub struct WalletState {
    pub submitting: bool,
    /// This session's signup, or the stored login as `AccountsLoaded`
    /// reported it.
    pub created: Option<WalletCreated>,
    /// Fixed text from the runtime — never the password.
    pub error: Option<String>,
    /// A balance can be read: the store holds the register token. Drives
    /// the 3-second poll and the "Waiting for sats…" line.
    pub has_token: bool,
    /// The last balance read, in sats.
    pub balance: Option<u64>,
    /// The last send from this wallet, as the status line reports it.
    pub send: Option<SendState>,
    /// A `Sent` whose fresh `Balance` has not arrived yet; that `Balance`
    /// turns it into the one toast per send.
    pub pending_sent: Option<PendingSent>,
    /// The last balance change, shown as a fading tag beside the balance
    /// until `delta_clear` fires.
    pub delta: Option<BalanceDelta>,
    /// Cancel-on-drop: clears `delta` when its fade is over.
    pub delta_clear: Option<Task<()>>,
    /// Bumped per change, so each tag is a new animation from full opacity.
    delta_seq: u64,
    /// The funding QR for the address on screen, keyed by that address so it
    /// is encoded when the address changes, not on every frame. The inner
    /// `None` is an address the QR encoder refused.
    qr: Option<(String, Option<Arc<QrCode>>)>,
}

/// One balance change: what it was, what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceDelta {
    pub previous: u64,
    pub current: u64,
    /// Names the fade animation, so a new change restarts it.
    seq: u64,
}

/// A send the runtime confirmed, waiting for the balance that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSent {
    pub to: String,
    pub sats: u64,
    /// The balance on screen when the send went out; `None` if there was
    /// none yet.
    pub before: Option<u64>,
}

/// How long the delta tag stays before it is gone: readable for about four
/// seconds, then it fades over the last two (the easing below).
pub const DELTA_FADE: Duration = Duration::from_secs(6);

impl WalletState {
    /// Take a fresh balance read. Returns the change when there was a
    /// balance to compare with and it differs — never on the first read.
    pub fn record_balance(&mut self, sats: u64) -> Option<BalanceDelta> {
        let previous = self.balance.replace(sats);
        let previous = previous.filter(|&p| p != sats)?;
        self.delta_seq += 1;
        let delta = BalanceDelta {
            previous,
            current: sats,
            seq: self.delta_seq,
        };
        self.delta = Some(delta.clone());
        Some(delta)
    }

    /// Re-encode the QR if `address` is not the one it was built for. Called
    /// from the shell whenever the address can change; a no-op otherwise.
    pub fn sync_qr(&mut self, address: Option<&str>) {
        let Some(address) = address else {
            self.qr = None;
            return;
        };
        if self.qr.as_ref().is_some_and(|(known, _)| known == address) {
            return;
        }
        // No LNURL means no QR and no hint, exactly as if nothing was known.
        self.qr = lnurl_pay(address).map(|lnurl| {
            let code = QrCode::new(format!("lightning:{lnurl}").as_bytes()).ok().map(Arc::new);
            (address.to_string(), code)
        });
    }
}

/// One send, from the click to the runtime's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendState {
    Sending,
    Sent { to: String, sats: u64 },
    /// Fixed text from the runtime or the input check.
    Failed(String),
}

/// The Send block's two inputs. One pair for the whole shell: the block
/// only ever shows the active account's wallet, and a switch clears them.
pub struct SendInputs {
    pub to: Entity<InputState>,
    pub sats: Entity<InputState>,
    _subs: Vec<Subscription>,
}

impl SendInputs {
    /// Enter in either field sends, like the button.
    pub fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let to = cx.new(|cx| InputState::new(window, cx).placeholder("name@domain.com"));
        let sats = cx.new(|cx| InputState::new(window, cx).placeholder("sats"));
        let subs = [&to, &sats]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this: &mut Shell, _, event, _, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.send_sats(cx);
                    }
                })
            })
            .collect();
        Self { to, sats, _subs: subs }
    }

    pub fn clear(&self, window: &mut Window, cx: &mut Context<Shell>) {
        for input in [&self.to, &self.sats] {
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
    }
}

/// The QR square's edge, including the quiet zone.
const QR_SIZE: f32 = 220.;
/// LUD-01 asks for a 4-module quiet zone around the code.
const QUIET_ZONE: usize = 4;

fn button(id: &'static str, label: &'static str, primary: bool) -> Stateful<Div> {
    let base = div()
        .id(id)
        .px(px(14.))
        .py(px(8.))
        .rounded(px(9.))
        .cursor_pointer()
        .text_size(px(12.5))
        .text_center()
        .child(label);
    if primary {
        base.bg(grad(ACCENT, ACCENT_DEEP))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(BG_RAIL))
            .shadow(vec![
                BoxShadow::new(px(0.), px(4.), wash(ACCENT_GLOW)).blur_radius(px(14.)),
            ])
    } else {
        base.border_1()
            .border_color(rgb(BORDER_3))
            .text_color(rgb(TEXT_MUTED))
    }
}

/// `lightning:<LNURL>` painted module by module: white square, one quad per
/// dark module, no image crate and no asset. Modules snap to whole pixels so
/// the grid has no hairline seams a camera could misread.
fn qr(code: Option<Arc<QrCode>>) -> AnyElement {
    let Some(code) = code else {
        return muted("Could not draw the QR code.", RED);
    };
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let width = code.width();
            window.paint_quad(fill(bounds, rgb(0xffffff)));
            let total = (width + 2 * QUIET_ZONE) as f32;
            let cell = (bounds.size.width / total).floor();
            // Centre the whole-pixel grid inside the square.
            let inset = (bounds.size.width - cell * total) / 2.;
            let origin = bounds.origin + point(inset, inset);
            for y in 0..width {
                for x in 0..width {
                    if code[(x, y)] == Color::Dark {
                        let corner = origin
                            + point(
                                cell * (x + QUIET_ZONE) as f32,
                                cell * (y + QUIET_ZONE) as f32,
                            );
                        window
                            .paint_quad(fill(Bounds::new(corner, size(cell, cell)), rgb(0x000000)));
                    }
                }
            }
        },
    )
    .size(px(QR_SIZE))
    .flex_shrink_0()
    .rounded(px(8.))
    .into_any_element()
}

/// The tag's two words: the signed change ("+15", "−30") and "was N".
pub fn delta_text(previous: u64, current: u64) -> (String, String) {
    let signed = if current >= previous {
        format!("+{}", timefmt::fmt_sats(current - previous))
    } else {
        format!("−{}", timefmt::fmt_sats(previous - current))
    };
    (signed, format!("was {}", timefmt::fmt_sats(previous)))
}

/// The change beside the balance, fading out over [`DELTA_FADE`]. The
/// shell drops it from the state when the fade is over.
fn delta_tag(delta: &BalanceDelta) -> AnyElement {
    let (signed, was) = delta_text(delta.previous, delta.current);
    let tone = if delta.current >= delta.previous { GREEN } else { RED };
    h_flex()
        .gap(px(4.))
        .text_size(px(11.))
        .child(div().text_color(rgb(tone)).child(signed))
        .child(div().text_color(rgb(TEXT_MUTED)).child(was))
        .with_animation(
            ElementId::NamedInteger("wallet-delta".into(), delta.seq),
            // t^4 stays near zero for most of the run, so the tag holds at
            // full opacity and only drops away at the end.
            Animation::new(DELTA_FADE).with_easing(|t| t * t * t * t),
            |this, t| this.opacity(1. - t),
        )
        .into_any_element()
}

/// The line under the QR. `first_funding` is the account screen: the person
/// is waiting for their first sats, so say so; the Wallet screen states the
/// balance, with the last change fading beside it.
fn balance_line(state: &WalletState, first_funding: bool) -> AnyElement {
    match (state.balance, first_funding) {
        (Some(sats), true) if sats > 0 => {
            muted(format!("Received {} sats", timefmt::fmt_sats(sats)), GREEN)
        }
        (_, true) => muted("Waiting for sats… scan with your phone wallet", TEXT_MUTED),
        (Some(sats), false) => h_flex()
            .gap(px(8.))
            .items_baseline()
            .child(muted(
                format!("Balance: {} sats", timefmt::fmt_sats(sats)),
                if sats > 0 { GREEN } else { TEXT_MUTED },
            ))
            .when_some(state.delta.as_ref(), |this, delta| this.child(delta_tag(delta)))
            .into_any_element(),
        (None, false) => muted("Balance: …", TEXT_MUTED),
    }
}

#[cfg(test)]
mod tests {
    // Named imports: `super::*` would bring gpui's `test` attribute along.
    use super::{WalletState, delta_text};
    use crate::timefmt;

    #[test]
    fn the_delta_tag_reads_as_a_signed_change_and_the_old_balance() {
        assert_eq!(delta_text(439, 409), ("−30".to_string(), "was 439".to_string()));
        assert_eq!(delta_text(40, 55), ("+15".to_string(), "was 40".to_string()));
        assert_eq!(
            delta_text(1_000, 12_345),
            (
                format!("+{}", timefmt::fmt_sats(11_345)),
                format!("was {}", timefmt::fmt_sats(1_000))
            )
        );
    }

    #[test]
    fn the_first_balance_read_is_not_a_change() {
        let mut state = WalletState::default();
        assert_eq!(state.record_balance(40), None, "nothing to compare with yet");
        assert_eq!(state.record_balance(40), None, "the same number is not a change");
        let delta = state.record_balance(55).expect("an increase");
        assert_eq!((delta.previous, delta.current), (40, 55));
        let again = state.record_balance(25).expect("a decrease");
        assert!(again.seq > delta.seq, "each change restarts the fade");
        assert_eq!(state.balance, Some(25));
    }
}

/// Recipient, amount, Send, and the status line under them. `Send` is inert
/// while a send is in flight, so a double click cannot pay twice.
fn send_block(state: &WalletState, inputs: &SendInputs, cx: &mut Context<Shell>) -> AnyElement {
    let sending = matches!(state.send, Some(SendState::Sending));
    let status = match &state.send {
        None => None,
        Some(SendState::Sending) => Some(
            h_flex()
                .gap(px(8.))
                .items_center()
                .child(Spinner::new().small().color(rgb(TEXT_MUTED).into()))
                .child(muted("Sending…", TEXT_MUTED))
                .into_any_element(),
        ),
        Some(SendState::Sent { to, sats }) => Some(muted(
            format!("Sent {} sats to {to}", timefmt::fmt_sats(*sats)),
            GREEN,
        )),
        Some(SendState::Failed(message)) => Some(muted(message.clone(), RED)),
    };
    v_flex()
        .gap(px(8.))
        .items_start()
        .pt(px(4.))
        .child(
            div()
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_DIM))
                .child("SEND TO A LIGHTNING ADDRESS"),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .items_center()
                .child(div().w(px(220.)).text_size(px(12.5)).child(Input::new(&inputs.to)))
                .child(div().w(px(90.)).text_size(px(12.5)).child(Input::new(&inputs.sats)))
                .child(
                    button("wallet-send", "Send", true)
                        .when(sending, |this| this.opacity(0.5))
                        .when(!sending, |this| {
                            this.on_click(cx.listener(|this, _, _, cx| this.send_sats(cx)))
                        }),
                ),
        )
        .when_some(status, |this, status| this.child(status))
        .into_any_element()
}

/// The panel for `view`: `view.lud16` is the profile as the relays have it,
/// `view.wallet` the stored or just-opened Coinos wallet, if any. `send` is
/// the Wallet screen's input pair; the account screen passes `None`, because
/// a wallet that is waiting for its first sats has nothing to send.
pub fn panel(
    view: &AccountView,
    first_funding: bool,
    send: Option<&SendInputs>,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let pubkey = view.pubkey.clone();
    let state = &view.wallet;
    let created = state.created.as_ref();
    let address = view.payout_address();

    let body = if state.submitting {
        h_flex()
            .gap(px(8.))
            .items_center()
            .child(Spinner::new().small().color(rgb(TEXT_MUTED).into()))
            .child(muted("Opening a Coinos wallet…", TEXT_MUTED))
    } else if let Some(address) = address {
        v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .items_center()
                    .child(muted("Lightning address", TEXT_MUTED))
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(13.))
                            .text_color(rgb(TEXT))
                            .child(SharedString::new(address)),
                    ),
            )
            .when_some(state.qr.as_ref().map(|(_, code)| code.clone()), |this, code| {
                this.child(qr(code)).child(muted(
                    "Scan with Strike or any Lightning wallet to add sats.",
                    TEXT_MUTED,
                ))
            })
            .when(state.has_token, |this| this.child(balance_line(state, first_funding)))
            .when_some(send.filter(|_| state.has_token), |this, inputs| {
                this.child(send_block(state, inputs, cx))
            })
            .when_some(created, |this, created| {
                let username = created.username.clone();
                let for_password = pubkey.clone();
                this.child(
                    v_flex()
                        .gap(px(10.))
                        .items_start()
                        .child(muted(
                            format!(
                                "Coinos username {} — only needed on coinos.io itself.",
                                created.username
                            ),
                            TEXT_MUTED,
                        ))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(
                                    button("wallet-copy-username", "Copy username", false)
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                username.clone(),
                                            ));
                                        })),
                                )
                                .child(
                                    button("wallet-copy-password", "Copy password", false)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.copy_coinos_password(&for_password, cx);
                                        })),
                                ),
                        ),
                )
                .when_some(created.publish_error.clone(), |this, error| {
                    let for_retry = pubkey.clone();
                    this.child(muted(
                        "The wallet exists, but the address is not on your profile yet — \
                         claims will not be paid until it is.",
                        AMBER,
                    ))
                    .child(
                        button("wallet-retry-publish", "Retry publishing", false).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.create_wallet(for_retry.clone(), cx)
                            }),
                        ),
                    )
                    .child(muted(error, AMBER))
                })
            })
    } else if nostr::is_issuer(&pubkey) {
        // The house issuer's wallet is the prod payer's, managed elsewhere;
        // rewriting its lud16 from here would redirect the instance's payouts.
        v_flex().gap(px(10.)).child(muted(
            "The house key's wallet is not managed here.",
            TEXT_MUTED,
        ))
    } else {
        let for_create = pubkey.clone();
        v_flex()
            .gap(px(10.))
            .child(muted(
                "No Lightning address yet — claims are accepted but never paid.",
                AMBER,
            ))
            .child(
                button("wallet-create", "Create a Coinos wallet for this account", true)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.create_wallet(for_create.clone(), cx)
                    })),
            )
    };

    body.when_some(state.error.clone(), |this, error| {
        this.child(muted(error, RED))
    })
    .into_any_element()
}
