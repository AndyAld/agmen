//! Hosts for `--view`: one view with its docks, before the workspace
//! hosts it, so screenshots can be compared with the mockups.

use gpui::{
    AnyView, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div,
};

use super::roles_panel::{RoleFileView, RolePanel, RolesPanel};
use super::title_bar::TitleBar;
use super::tokens::{BORDER_WIDTH, LEFT_DOCK_WIDTH, RIGHT_DOCK_WIDTH};
use crate::appearance::Appearance;
use crate::backend::{RoleFile, RoleInfo, Snapshot};

/// The Roles tab, reviewer.xml open, and its Role panel
/// (`desktop-role-file.webp`).
pub(crate) struct RolesDev {
    panel: Entity<RolesPanel>,
    file: Entity<RoleFileView>,
    name: String,
    info: Option<RoleInfo>,
}

impl RolesDev {
    /// Lists `roles` with `role` open.
    pub(crate) fn new(roles: Vec<RoleFile>, role: RoleFile, cx: &mut Context<Self>) -> Self {
        let file = cx.new(|cx| RoleFileView::new(&role, cx));
        let panel = cx.new(|_| RolesPanel::new(roles));
        let name = role.name.clone();
        panel.update(cx, |panel, _| panel.set_selected(&name));
        Self {
            panel,
            file,
            name,
            info: role.info,
        }
    }
}

impl Render for RolesDev {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let rule = super::rule_color(cx);
        div()
            .size_full()
            .flex()
            .bg(theme.background)
            .text_color(theme.text)
            .child(
                div()
                    .w(LEFT_DOCK_WIDTH)
                    .flex_none()
                    .border_r(BORDER_WIDTH)
                    .border_color(rule)
                    .child(self.panel.clone()),
            )
            .child(div().flex_1().min_w_0().child(self.file.clone()))
            .child(
                div()
                    .w(RIGHT_DOCK_WIDTH)
                    .flex_none()
                    .border_l(BORDER_WIDTH)
                    .border_color(rule)
                    .children(
                        self.info
                            .clone()
                            .map(|info| RolePanel::new(self.name.clone(), info)),
                    ),
            )
    }
}

/// A dev view under the app's title bar, so the window chrome (macOS
/// traffic lights, or the Windows and Linux buttons) sits where it will in
/// the workspace.
pub(crate) struct DevFrame {
    /// What the title bar shows.
    title: Snapshot,
    content: AnyView,
}

impl DevFrame {
    pub(crate) fn new(snapshot: Snapshot, content: impl Into<AnyView>) -> Self {
        Self {
            title: snapshot,
            content: content.into(),
        }
    }
}

impl Render for DevFrame {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(appearance.theme.background)
            .text_color(appearance.theme.text)
            .font_family(appearance.fonts.ui_family.clone())
            .child(TitleBar::new(&self.title))
            .child(div().flex_1().min_h_0().child(self.content.clone()))
    }
}
