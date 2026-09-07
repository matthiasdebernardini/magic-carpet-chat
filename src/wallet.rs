//! The wallet panel: the account's Lightning address and a QR to fund it, or
//! the one button that opens a Coinos wallet when the key has no address yet.
//!
//! Rendered in two places from the same state: the onboarding ready panel
//! (where a freshly generated key has no lud16 by definition) and the Wallet
//! screen (⌘6). The runtime does the signup and the kind-0 write; this module
//! only sends `Command::CreateCoinosWallet` and draws what came back. The
//! Coinos password never reaches this module — it lives in the keychain.

use std::sync::Arc;

use gpui_kit::component::{Sizable as _, h_flex, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use qrcode::{Color, QrCode};

use magic_carpet_chat::coinos::lnurl_pay;
use magic_carpet_chat::nostr::WalletCreated;
use magic_carpet_chat::secrets::Account;

use crate::dashboard::{MONO, muted};
use crate::palette::*;
use crate::shell::{AccountView, Shell};

/// The signup as the UI sees it, one per account view. The flow only ever
/// runs for the claimant; the issuer's wallet is the prod payer's, so its
/// panel never offers the button.
#[derive(Default)]
pub struct WalletState {
    pub submitting: bool,
    pub created: Option<WalletCreated>,
    /// Fixed text from the runtime — never the password.
    pub error: Option<String>,
    /// The funding QR for the address on screen, keyed by that address so it
    /// is encoded when the address changes, not on every frame. The inner
    /// `None` is an address the QR encoder refused.
    qr: Option<(String, Option<Arc<QrCode>>)>,
}

impl WalletState {
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

/// The panel for `account`: `view.lud16` is the profile as the relays have
/// it, `view.wallet` this session's signup, if any.
pub fn panel(account: Account, view: &AccountView, cx: &mut Context<Shell>) -> AnyElement {
    let local = account.has_local_wallet();
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
            .when_some(created, |this, created| {
                let username = created.username.clone();
                this.child(
                    v_flex()
                        .gap(px(10.))
                        .items_start()
                        .child(muted(
                            format!(
                                "Coinos username {} — password saved in this Mac's keychain \
                                 under {}.",
                                created.username,
                                account.coinos_entry_key()
                            ),
                            TEXT_MUTED,
                        ))
                        .child(
                            button("wallet-copy-username", "Copy username", false).on_click(
                                cx.listener(move |_, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        username.clone(),
                                    ));
                                }),
                            ),
                        ),
                )
                .when_some(created.publish_error.clone(), |this, error| {
                    this.child(muted(
                        "The wallet exists, but the address is not on your profile yet — \
                         claims will not be paid until it is.",
                        AMBER,
                    ))
                    .when(local, |this| {
                        this.child(
                            button("wallet-retry-publish", "Retry publishing", false)
                                .on_click(cx.listener(|this, _, _, cx| this.create_wallet(cx))),
                        )
                    })
                    .child(muted(error, AMBER))
                })
            })
    } else if local {
        v_flex()
            .gap(px(10.))
            .child(muted(
                "No Lightning address yet — claims are accepted but never paid.",
                AMBER,
            ))
            .child(
                button("wallet-create", "Create a Coinos wallet for this key", true)
                    .on_click(cx.listener(|this, _, _, cx| this.create_wallet(cx))),
            )
    } else {
        v_flex().gap(px(10.)).child(muted(
            "The house key's wallet is not managed here.",
            TEXT_MUTED,
        ))
    };

    body.when_some(state.error.clone(), |this, error| {
        this.child(muted(error, RED))
    })
    .into_any_element()
}
