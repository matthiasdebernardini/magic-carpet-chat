//! The wallet panel: the account's Lightning address and a QR to fund it, or
//! the one button that opens a Coinos wallet when the key has no address yet.
//!
//! Rendered in two places from the same state: the onboarding ready panel
//! (where a freshly generated key has no lud16 by definition) and the Wallet
//! screen (⌘6). The runtime does the signup and the kind-0 write; this module
//! only sends `Command::CreateCoinosWallet` and draws what came back. The
//! Coinos password never reaches this module — it lives in the keychain.

use gpui_kit::component::{Sizable as _, h_flex, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use qrcode::{Color, QrCode};

use magic_carpet_chat::coinos::lnurl_pay;
use magic_carpet_chat::secrets::Account;

use crate::dashboard::MONO;
use crate::palette::*;
use crate::shell::{AccountView, Shell};

/// What `Update::WalletCreated` said — public material only.
pub struct WalletCreated {
    pub account: Account,
    pub username: String,
    pub lightning_address: String,
    /// Set when the wallet exists but the kind-0 write failed: fixed text
    /// from the runtime, shown in amber under the retry button.
    pub publish_error: Option<String>,
}

/// The signup as the UI sees it. One per app, not per account: the flow is
/// for the claimant only. The issuer's wallet is the prod payer's, so its
/// panel never offers the button.
#[derive(Default)]
pub struct WalletState {
    pub submitting: bool,
    pub created: Option<WalletCreated>,
    /// Fixed text from the runtime — never the password.
    pub error: Option<String>,
}

/// The QR square's edge, including the quiet zone.
const QR_SIZE: f32 = 220.;
/// LUD-01 asks for a 4-module quiet zone around the code.
const QUIET_ZONE: usize = 4;

fn muted(text: impl Into<SharedString>, color: u32) -> AnyElement {
    div()
        .text_size(px(12.5))
        .text_color(rgb(color))
        .child(text.into())
        .into_any_element()
}

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
fn qr(text: &str) -> AnyElement {
    let Ok(code) = QrCode::new(text.as_bytes()) else {
        return muted("Could not draw the QR code.", RED);
    };
    let width = code.width();
    let modules: Vec<Color> = code.to_colors();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            window.paint_quad(fill(bounds, rgb(0xffffff)));
            let total = (width + 2 * QUIET_ZONE) as f32;
            let cell = (bounds.size.width / total).floor();
            // Centre the whole-pixel grid inside the square.
            let inset = (bounds.size.width - cell * total) / 2.;
            let origin = bounds.origin + point(inset, inset);
            for y in 0..width {
                for x in 0..width {
                    if modules[y * width + x] == Color::Dark {
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

/// The panel for `account`. `view.lud16` is the profile as the relays have
/// it; `state` is this session's signup, if any.
pub fn panel(
    account: Account,
    view: &AccountView,
    state: &WalletState,
    cx: &mut Context<Shell>,
) -> AnyElement {
    let created = state
        .created
        .as_ref()
        .filter(|created| created.account == account);
    let address = created
        .map(|created| created.lightning_address.clone())
        .or_else(|| view.lud16.clone());

    let body = if state.submitting {
        h_flex()
            .gap(px(8.))
            .items_center()
            .child(Spinner::new().small().color(rgb(TEXT_MUTED).into()))
            .child(muted("Opening a Coinos wallet…", TEXT_MUTED))
    } else if let Some(address) = address {
        let lnurl = lnurl_pay(&address);
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
                            .child(SharedString::from(address.clone())),
                    ),
            )
            .when_some(lnurl, |this, lnurl| {
                this.child(qr(&format!("lightning:{lnurl}"))).child(muted(
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
                                 under {account}-coinos-login.",
                                created.username
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
                    .when(account == Account::Claimant, |this| {
                        this.child(
                            button("wallet-retry-publish", "Retry publishing", false)
                                .on_click(cx.listener(|this, _, _, cx| this.create_wallet(cx))),
                        )
                    })
                    .child(muted(error, AMBER))
                })
            })
    } else if account == Account::Claimant {
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
