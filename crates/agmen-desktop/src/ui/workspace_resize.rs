//! The docks' drag handles: dragging resizes a dock live, a double click
//! resets it, and the widths are saved when a drag ends. A child of
//! `workspace`, so it reaches its state.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, CursorStyle, Div, DragMoveEvent, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Render,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::Workspace;
use crate::appearance::DesktopSettings;
use crate::ui::dock::DockSide;
use crate::ui::settings_view::SettingsSources;
use crate::ui::tokens::{BORDER_WIDTH, CENTER_MIN_WIDTH, LEFT_DOCK_WIDTH, RESIZE_HANDLE_WIDTH};

/// What a dock's drag handle drags. It draws nothing under the mouse.
#[derive(Debug, Clone, Copy)]
pub(super) struct DraggedDock(DockSide);

impl Render for DraggedDock {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// The widths the open docks (`None` when closed) draw at in a body
/// `total` wide: each inside its bounds, then, while the center would be
/// narrower than its minimum, the right dock and then the left give way,
/// down to their minimums.
fn fit(
    left: Option<Pixels>,
    right: Option<Pixels>,
    total: Pixels,
) -> (Option<Pixels>, Option<Pixels>) {
    let mut left = left.map(|width| DockSide::Left.clamp_width(width));
    let mut right = right.map(|width| DockSide::Right.clamp_width(width));
    let mut over = left.unwrap_or_default() + right.unwrap_or_default() + CENTER_MIN_WIDTH - total;
    for (side, width) in [(DockSide::Right, &mut right), (DockSide::Left, &mut left)] {
        if let Some(width) = width
            && over > px(0.)
        {
            let give = (*width - side.width_bounds().0).min(over);
            *width -= give;
            over -= give;
        }
    }
    (left, right)
}

/// A saved width as pixels inside `side`'s bounds.
fn from_saved(side: DockSide, saved: Option<u32>) -> Option<Pixels> {
    // f32 rounds only widths past 2^24 px, which the clamp drops anyway.
    saved.map(|width| side.clamp_width(px(width as f32)))
}

/// A width as whole pixels for the settings file.
fn to_saved(width: Option<Pixels>) -> Option<u32> {
    // Dock widths are clamped to 200 to 640 px, so the cast keeps every
    // value.
    width.map(|width| f32::from(width).round().max(0.) as u32)
}

impl Workspace {
    /// The left and right docks' saved widths, inside their bounds; unset,
    /// or both unset when the settings file cannot be read.
    pub(super) fn saved_dock_widths(cx: &App) -> (Option<Pixels>, Option<Pixels>) {
        let Some(sources) = cx.try_global::<SettingsSources>() else {
            return (None, None);
        };
        match DesktopSettings::load(&sources.settings_path()) {
            Ok(settings) => (
                from_saved(DockSide::Left, settings.left_dock_width),
                from_saved(DockSide::Right, settings.right_dock_width),
            ),
            Err(err) => {
                eprintln!("agmen-desktop: dock widths not restored: {err}");
                (None, None)
            }
        }
    }

    /// The width `side` would draw at with room to spare, `None` when it
    /// is closed.
    fn wanted_width(&self, side: DockSide, cx: &App) -> Option<Pixels> {
        let width = match side {
            DockSide::Left => self.left_dock.width().unwrap_or(LEFT_DOCK_WIDTH),
            DockSide::Right => self
                .right_dock
                .width()
                .unwrap_or_else(|| self.right_dock_default_width(cx)),
        };
        self.is_dock_open(side).then(|| side.clamp_width(width))
    }

    /// The open docks' widths in a body `total` wide.
    pub(super) fn dock_widths(&self, total: Pixels, cx: &App) -> (Option<Pixels>, Option<Pixels>) {
        fit(
            self.wanted_width(DockSide::Left, cx),
            self.wanted_width(DockSide::Right, cx),
            total,
        )
    }

    /// Sets `side`'s width from a drag to `wanted`, in a body `total`
    /// wide: inside its bounds, and never into the center's minimum.
    fn resize_dock(
        &mut self,
        side: DockSide,
        wanted: Pixels,
        total: Pixels,
        cx: &mut Context<Self>,
    ) {
        let (left, right) = self.dock_widths(total, cx);
        let (other_side, other) = match side {
            DockSide::Left => (DockSide::Right, right),
            DockSide::Right => (DockSide::Left, left),
        };
        // As a drag starts, a dock the window squeezes keeps the width it
        // draws at, so it never takes the room the dragged dock gives up.
        if !self.dock_dragged
            && let Some(drawn) = other
            && self
                .wanted_width(other_side, cx)
                .is_some_and(|wanted| drawn < wanted)
        {
            match other_side {
                DockSide::Left => self.left_dock.set_width(Some(drawn)),
                DockSide::Right => self.right_dock.set_width(Some(drawn)),
            }
        }
        let room = total - CENTER_MIN_WIDTH - other.unwrap_or_default();
        let width = side
            .clamp_width(wanted)
            .min(room)
            .max(side.width_bounds().0);
        match side {
            DockSide::Left => self.left_dock.set_width(Some(width)),
            DockSide::Right => self.right_dock.set_width(Some(width)),
        }
        self.dock_dragged = true;
        cx.notify();
    }

    /// Puts `side` back at its default width and saves that.
    fn reset_dock(&mut self, side: DockSide, cx: &mut Context<Self>) {
        match side {
            DockSide::Left => self.left_dock.set_width(None),
            DockSide::Right => self.right_dock.set_width(None),
        }
        self.save_dock_widths(cx);
        cx.notify();
    }

    /// Saves the widths once a drag that moved a dock ends.
    fn end_dock_drag(&mut self, cx: &App) {
        if std::mem::take(&mut self.dock_dragged) {
            self.save_dock_widths(cx);
        }
    }

    fn save_dock_widths(&self, cx: &App) {
        // shortcut: a failed save goes to stderr only, as vim mode's does;
        // show it in the window once the status bar has a place for notes.
        let Some(sources) = cx.try_global::<SettingsSources>() else {
            return;
        };
        let path = sources.settings_path();
        let saved = DesktopSettings::load(&path).and_then(|mut settings| {
            settings.left_dock_width = to_saved(self.left_dock.width());
            settings.right_dock_width = to_saved(self.right_dock.width());
            settings.save(&path)
        });
        if let Err(err) = saved {
            eprintln!("agmen-desktop: dock widths not saved: {err}");
        }
    }

    /// The drag handle on the inner border of `side`, a dock `width` wide,
    /// for the body's (relative) box: 6 px wide and centered on the border.
    /// The body draws it after both docks and the center, so both halves
    /// take the mouse, and before every overlay (palette, dialogs, toast,
    /// popups), which the workspace and its parents draw later, so those
    /// stay on top.
    pub(super) fn dock_resize_handle(
        side: DockSide,
        width: Pixels,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        // From the body's edge to the handle's near edge.
        let inset = width - (BORDER_WIDTH + RESIZE_HANDLE_WIDTH) / 2.;
        div()
            .id(match side {
                DockSide::Left => "left-dock-resize",
                DockSide::Right => "right-dock-resize",
            })
            .debug_selector(move || match side {
                DockSide::Left => "left-dock:resize".into(),
                DockSide::Right => "right-dock:resize".into(),
            })
            .absolute()
            .top_0()
            .bottom_0()
            .w(RESIZE_HANDLE_WIDTH)
            .when(side == DockSide::Left, |handle| handle.left(inset))
            .when(side == DockSide::Right, |handle| handle.right(inset))
            .cursor(CursorStyle::ResizeColumn)
            .occlude()
            .on_drag(DraggedDock(side), |dock, _, _, cx| {
                cx.stop_propagation();
                cx.new(|_| *dock)
            })
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseUpEvent, _, cx| {
                    if event.click_count == 2 {
                        this.reset_dock(side, cx);
                        cx.stop_propagation();
                    }
                }),
            )
    }

    /// Registers, on the body that holds the docks, the drag that resizes
    /// them and the mouse up that ends it, wherever the mouse is let go.
    pub(super) fn dock_resize_listeners(body: Div, cx: &Context<Self>) -> Div {
        body.on_drag_move(
            cx.listener(|this, event: &DragMoveEvent<DraggedDock>, _, cx| {
                let side = event.drag(cx).0;
                let x = event.event.position.x;
                let bounds = event.bounds;
                let wanted = match side {
                    DockSide::Left => x - bounds.left(),
                    DockSide::Right => bounds.right() - x,
                };
                this.resize_dock(side, wanted, bounds.size.width, cx);
            }),
        )
        // Capture phase, so no view under the mouse can stop it.
        .capture_any_mouse_up(cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_dock_drag(cx)))
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, cx| this.end_dock_drag(cx)),
        )
    }
}

#[cfg(test)]
#[path = "workspace_resize_tests.rs"]
mod tests;
