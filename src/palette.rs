//! Every colour in the app, copied from the Magic Carpet design mock.
//!
//! The mock is a single HTML page with inline styles. Nothing here is derived
//! or guessed — each constant is a hex value read straight out of that page, so
//! a change to the design lands as a one-line change here.
//!
//! The mock also ships a light mode, but it is a whole-page CSS
//! `invert(1) hue-rotate(180deg)` filter over these same values. That filter
//! turns the indigo logo green and the "armed" pill red, so the palette below
//! is the design as drawn.

use gpui_kit::{App, Background, Hsla, linear_color_stop, linear_gradient, rgb, rgba};
use gpui_kit::component::Theme;

// ------------------------------------------------------------------ surfaces

/// The icon rail, and the background of an input.
pub const BG_RAIL: u32 = 0x0d0d14;
/// The nav column.
pub const BG_SIDEBAR: u32 = 0x101019;
/// The page behind the cards.
pub const BG_APP: u32 = 0x12121b;
/// A card, a chip that is on, a popover.
pub const BG_CARD: u32 = 0x191926;
/// A row under the pointer.
pub const BG_HOVER: u32 = 0x1f1f30;
/// The nav row that is on.
pub const BG_NAV_ON: u32 = 0x26263a;

// ------------------------------------------------------------------- borders

/// The hairline between the rail, the sidebar, and the top bar.
pub const LINE: u32 = 0x1f1f2e;
/// A card edge, and the rule between list rows.
pub const BORDER: u32 = 0x23233a;
/// A control edge.
pub const BORDER_2: u32 = 0x2a2a40;
/// A control edge that is emphasised, and the "Fix →" button.
pub const BORDER_3: u32 = 0x3a3a55;

// ---------------------------------------------------------------------- text

pub const TEXT: u32 = 0xe8e6f0;
pub const TEXT_MUTED: u32 = 0x8b89a0;
pub const TEXT_DIM: u32 = 0x5b5975;

// -------------------------------------------------------------------- accent

pub const ACCENT: u32 = 0x8f7df8;
pub const ACCENT_DEEP: u32 = 0x6d5cf0;
pub const ACCENT_LIGHT: u32 = 0xa99bfa;
/// Accent at 13% — a count badge.
pub const ACCENT_WASH: u32 = 0x8f7df822;
/// Accent at 24% — the glow under the primary button.
pub const ACCENT_GLOW: u32 = 0x6d5cf03d;

// -------------------------------------------------------------------- status

pub const GREEN: u32 = 0x4ecb8d;
/// The "sats" suffix beside an earned figure.
pub const GREEN_DIM: u32 = 0x2f9d6a;
pub const GREEN_EDGE: u32 = 0x2b4a3a;
/// Green at 6% — the fill of the "Auto-pay armed" pill.
pub const GREEN_WASH: u32 = 0x4ecb8d10;
pub const AMBER: u32 = 0xf2b544;
/// The "sats" suffix beside a spent figure.
pub const AMBER_DIM: u32 = 0x8a6f35;
pub const RED: u32 = 0xe5646c;

// --------------------------------------------------------------- account hue

pub const HUE_AK_FROM: u32 = 0xf2b544;
pub const HUE_AK_TO: u32 = 0xe78a3c;
pub const HUE_BO_FROM: u32 = 0x5fd4c4;
pub const HUE_BO_TO: u32 = 0x3f9de0;

// ------------------------------------------------------------------ helpers

/// A two-stop gradient at the mock's 140°.
///
/// The mock's logo tile has a third stop at 55%; gpui carries two, so the
/// middle violet is dropped and the sweep runs end to end.
pub fn grad(from: u32, to: u32) -> Background {
    linear_gradient(
        140.,
        linear_color_stop(rgb(from), 0.),
        linear_color_stop(rgb(to), 1.),
    )
}

/// Reads an `0xRRGGBBAA` constant as a colour.
pub fn wash(hex: u32) -> Hsla {
    rgba(hex).into()
}

/// Repaints gpui-component's own controls — the composer, the search field,
/// the scrollbars — in the mock's colours. Everything else in this app draws
/// from the constants above and never reads the theme.
pub fn apply(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    theme.colors.background = rgb(BG_APP).into();
    theme.colors.foreground = rgb(TEXT).into();
    theme.colors.border = rgb(BORDER_2).into();
    theme.colors.input = rgb(BORDER_2).into();
    theme.colors.muted = rgb(BG_CARD).into();
    theme.colors.muted_foreground = rgb(TEXT_MUTED).into();
    theme.colors.secondary = rgb(BG_CARD).into();
    theme.colors.secondary_foreground = rgb(TEXT).into();
    theme.colors.primary = rgb(ACCENT).into();
    theme.colors.primary_foreground = rgb(BG_RAIL).into();
    theme.colors.accent = rgb(BG_NAV_ON).into();
    theme.colors.accent_foreground = rgb(TEXT).into();
    theme.colors.ring = rgb(ACCENT).into();
    theme.colors.title_bar = rgb(BG_RAIL).into();
    theme.colors.title_bar_border = rgb(LINE).into();
    theme.radius = gpui_kit::px(9.);
    theme.radius_lg = gpui_kit::px(14.);
    Theme::sync_base(cx);
}
