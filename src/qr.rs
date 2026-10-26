//! QR codes: the wallet's funding code and the account's npub code.

use std::sync::Arc;

use gpui_kit::*;
use qrcode::{Color, QrCode};

use crate::dashboard::muted;
use crate::palette::*;

/// LUD-01 asks for a 4-module quiet zone around the code.
const QUIET_ZONE: usize = 4;

/// Encode `text`; `None` when it does not fit a QR code.
pub fn encode(text: &str) -> Option<Arc<QrCode>> {
    QrCode::new(text.as_bytes()).ok().map(Arc::new)
}

/// The NIP-21 link phone Nostr apps open for `npub`.
pub fn npub_link(npub: &str) -> String {
    format!("nostr:{npub}")
}

/// The code painted module by module: white square, one quad per dark
/// module, no image crate and no asset. Modules snap to whole pixels so the
/// grid has no hairline seams a camera could misread. `edge` is the square's
/// side in pixels, quiet zone included.
pub fn view(code: Option<Arc<QrCode>>, edge: f32) -> AnyElement {
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
    .size(px(edge))
    .flex_shrink_0()
    .rounded(px(8.))
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{encode, npub_link};
    use qrcode::{Color, QrCode};

    #[test]
    fn the_code_carries_the_nostr_npub_link() {
        let npub = "npub1sg6plzptd64u62a878hep2kev88swjh3tw00gjsfl8f237lmu63q0uf63m";
        let link = npub_link(npub);
        assert_eq!(link, format!("nostr:{npub}"));
        let code = encode(&link).expect("an npub link fits a QR code");
        let again = QrCode::new(link.as_bytes()).expect("same text encodes");
        assert_eq!(code.width(), again.width());
        let same = (0..code.width()).all(|y| {
            (0..code.width())
                .all(|x| (code[(x, y)] == Color::Dark) == (again[(x, y)] == Color::Dark))
        });
        assert!(same, "the modules are the encoding of the link");
    }
}
