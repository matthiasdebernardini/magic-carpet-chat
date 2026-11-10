//! Accessibility: one call puts an element in the AccessKit tree.
//!
//! gpui reports a node only when it has a global element ID and a role. The
//! accessibility id is what leaves the process (AXIdentifier on macOS,
//! AccessibleId on AT-SPI); the e2e driver finds controls by it, so every id
//! is listed in docs/a11y-ids.md and must stay stable.
//!
//! Text passed to a div as a plain string makes no node of its own, so the
//! label carries the visible text. Keep the two in step.
//!
//! That makes the helper the one place all labelled text passes through, so it
//! also records every call for "Copy debug info" ([`on_screen`]) and makes
//! every Label copy its text on a right-click.

use std::cell::RefCell;

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

thread_local! {
    /// `(id, role, label)` of every `a11y` call since the last
    /// [`begin_frame`]. gpui draws on one thread.
    static ON_SCREEN: RefCell<Vec<(String, String, String)>> = const { RefCell::new(Vec::new()) };
}

/// Forget the last frame's elements. The root view calls it at the top of its
/// render; every view under it renders after that, in the same frame.
pub(crate) fn begin_frame() {
    ON_SCREEN.with_borrow_mut(Vec::clear);
}

/// Every labelled element of the last frame, in render order.
pub(crate) fn on_screen() -> Vec<(String, String, String)> {
    ON_SCREEN.with_borrow(Clone::clone)
}

pub(crate) trait Accessible {
    /// Sets the accessibility id, role and label. A plain `Div` also gets the
    /// id as its gpui element id; a `Stateful<Div>` keeps the one it has.
    fn a11y(
        self,
        id: impl Into<SharedString>,
        role: Role,
        label: impl Into<SharedString>,
    ) -> Stateful<Div>;
}

impl Accessible for Div {
    fn a11y(
        self,
        id: impl Into<SharedString>,
        role: Role,
        label: impl Into<SharedString>,
    ) -> Stateful<Div> {
        let id = id.into();
        self.id(id.clone()).a11y(id, role, label)
    }
}

impl Accessible for Stateful<Div> {
    fn a11y(
        self,
        id: impl Into<SharedString>,
        role: Role,
        label: impl Into<SharedString>,
    ) -> Stateful<Div> {
        let (id, label) = (id.into(), label.into());
        ON_SCREEN.with_borrow_mut(|seen| {
            seen.push((id.to_string(), format!("{role:?}"), label.to_string()));
        });
        let el = self.accessibility_id(id).role(role);
        // Right button only: the left click keeps whatever the element does.
        let el = if role == Role::Label && !label.is_empty() {
            let text = label.to_string();
            el.on_mouse_down(MouseButton::Right, move |_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                window.push_notification(Notification::info("Copied"), cx);
            })
        } else {
            el
        };
        // AT-SPI names a Label from its value, not its label: without the
        // value every Label reads as empty there. Linux only: on macOS a
        // value makes a Label settable text and hides sibling Labels.
        #[cfg(target_os = "linux")]
        let el = if role == Role::Label {
            el.aria_value(label.clone())
        } else {
            el
        };
        el.aria_label(label)
    }
}
