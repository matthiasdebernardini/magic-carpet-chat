//! The nav icons, and the asset source that serves them.
//!
//! The mock draws its icons as inline SVG paths. gpui-component ships a lucide
//! set that does not contain a diamond, a bolt, or a tag, so the mock's own
//! paths are copied into `assets/icons/` and served from here. Anything this
//! source does not know about — every gpui-component icon — falls through to
//! the crate that does.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// `(request path, file bytes)`. The path is what [`Icon::path`] is given.
macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $((concat!("icons/mc/", $name, ".svg"),
               include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons![
    "overview",
    "bounties",
    "payments",
    "claimants",
    "boards",
    "wallet",
    "tags",
    "settings",
    "accounts",
    "chat",
    "logo",
];

/// A named icon, as a path this app's [`Assets`] can serve.
pub fn icon(name: &str) -> SharedString {
    SharedString::from(format!("icons/mc/{name}.svg"))
}

/// The app's icons, with gpui-component's behind them.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ICONS.iter().find(|(name, _)| *name == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut found = gpui_kit::assets::Assets.list(path)?;
        found.extend(
            ICONS
                .iter()
                .filter(|(name, _)| name.starts_with(path))
                .map(|(name, _)| SharedString::from(*name)),
        );
        Ok(found)
    }
}
