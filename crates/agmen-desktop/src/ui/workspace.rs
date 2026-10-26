//! The window's root view: title bar, docks, center and status bar.

use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, Styled, Window, actions, div, px,
};

use super::dock::{Dock, DockSide};
use crate::appearance::{Appearance, Theme};

actions!(workspace, [ToggleLeftDock, ToggleRightDock, Quit]);

/// Sizes from `docs/mockups/scripts/console.py`.
pub(crate) const TITLE_BAR_HEIGHT: Pixels = px(38.);
pub(crate) const STATUS_BAR_HEIGHT: Pixels = px(28.);
const LEFT_DOCK_WIDTH: Pixels = px(272.);
const RIGHT_DOCK_WIDTH: Pixels = px(340.);

/// The window's root view.
pub(crate) struct Workspace {
    focus_handle: FocusHandle,
    left_dock: Dock,
    right_dock: Dock,
}

impl Workspace {
    pub(crate) fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            left_dock: Dock::new(LEFT_DOCK_WIDTH),
            right_dock: Dock::new(RIGHT_DOCK_WIDTH),
        }
    }

    pub(crate) const fn is_dock_open(&self, side: DockSide) -> bool {
        match side {
            DockSide::Left => self.left_dock.is_open(),
            DockSide::Right => self.right_dock.is_open(),
        }
    }

    pub(crate) fn toggle_dock(&mut self, side: DockSide, cx: &mut Context<Self>) {
        match side {
            DockSide::Left => self.left_dock.toggle(),
            DockSide::Right => self.right_dock.toggle(),
        }
        cx.notify();
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn region(theme: &Theme, label: &'static str) -> impl IntoElement {
    div().text_color(theme.text_placeholder).child(label)
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let theme = &appearance.theme;
        let fonts = &appearance.fonts;
        let border = theme.border_variant;
        let mut body = div().flex().flex_row().flex_1().min_h_0();
        if self.is_dock_open(DockSide::Left) {
            body = body.child(
                div()
                    .w(self.left_dock.width())
                    .flex_none()
                    .h_full()
                    .border_r_1()
                    .border_color(border)
                    .p_2()
                    .child(region(theme, "Left dock")),
            );
        }
        body = body.child(div().flex_1().h_full().p_2().child(region(theme, "Center")));
        if self.is_dock_open(DockSide::Right) {
            body = body.child(
                div()
                    .w(self.right_dock.width())
                    .flex_none()
                    .h_full()
                    .border_l_1()
                    .border_color(border)
                    .p_2()
                    .child(region(theme, "Right dock")),
            );
        }

        div()
            .track_focus(&self.focus_handle)
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &ToggleLeftDock, _, cx| {
                this.toggle_dock(DockSide::Left, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRightDock, _, cx| {
                this.toggle_dock(DockSide::Right, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .font_family(fonts.ui_family.clone())
            .text_size(fonts.ui_size)
            .child(
                div()
                    .h(TITLE_BAR_HEIGHT)
                    .flex_none()
                    .border_b_1()
                    .border_color(border)
                    .pl(px(80.))
                    .flex()
                    .items_center()
                    .child("agmen"),
            )
            .child(body)
            .child(
                div()
                    .h(STATUS_BAR_HEIGHT)
                    .flex_none()
                    .border_t_1()
                    .border_color(border)
                    .px_2()
                    .flex()
                    .items_center()
                    .child(region(theme, "Status bar")),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn workspace_toggles_docks(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Appearance::builtin()));
        let (workspace, cx) = cx.add_window_view(Workspace::new);
        workspace.read_with(cx, |w, _| {
            assert!(w.is_dock_open(DockSide::Left));
            assert!(w.is_dock_open(DockSide::Right));
        });

        workspace.update_in(cx, |w, window, cx| w.focus_handle.focus(window, cx));
        cx.dispatch_action(ToggleRightDock);
        workspace.read_with(cx, |w, _| {
            assert!(w.is_dock_open(DockSide::Left));
            assert!(!w.is_dock_open(DockSide::Right));
        });

        cx.dispatch_action(ToggleRightDock);
        workspace.read_with(cx, |w, _| assert!(w.is_dock_open(DockSide::Right)));

        cx.dispatch_action(ToggleLeftDock);
        workspace.read_with(cx, |w, _| assert!(!w.is_dock_open(DockSide::Left)));
    }
}
