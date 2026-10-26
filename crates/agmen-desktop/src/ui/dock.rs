//! A collapsible side dock.

use gpui::Pixels;

/// Which side of the window a dock sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockSide {
    Left,
    Right,
}

/// A dock's open state and width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Dock {
    open: bool,
    width: Pixels,
}

impl Dock {
    pub(crate) const fn new(width: Pixels) -> Self {
        Self { open: true, width }
    }

    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) const fn width(&self) -> Pixels {
        self.width
    }

    pub(crate) const fn toggle(&mut self) {
        self.open = !self.open;
    }
}
