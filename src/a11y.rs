//! Accessibility: one call puts an element in the AccessKit tree.
//!
//! gpui reports a node only when it has a global element ID and a role. The
//! accessibility id is what leaves the process (AXIdentifier on macOS,
//! AccessibleId on AT-SPI); the e2e driver finds controls by it, so every id
//! is listed in docs/a11y-ids.md and must stay stable.
//!
//! Text passed to a div as a plain string makes no node of its own, so the
//! label carries the visible text. Keep the two in step.

use gpui_kit::*;

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
        let label = label.into();
        let el = self.accessibility_id(id).role(role);
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
