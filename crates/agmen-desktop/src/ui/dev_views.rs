//! Hosts for `--view`: one view with its docks, before the workspace
//! hosts it, so screenshots can be compared with the mockups.

use gpui::{
    AnyView, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div,
};

use super::project_panel::{FileView, ProjectPanel, ProjectPanelEvent};
use super::roles_panel::{RoleFileView, RolePanel, RolesPanel, RolesPanelEvent};
use super::scope_chip::ScopeOption;
use super::title_bar::TitleBar;
use super::tokens::{BORDER_WIDTH, LEFT_DOCK_WIDTH, RIGHT_DOCK_WIDTH};
use crate::appearance::Appearance;
use crate::backend::{RoleFile, RoleInfo, Snapshot, WorktreeInfo};
use crate::icons::FileIcons;

/// The Roles tab, reviewer.xml open, and its Role panel
/// (`desktop-role-file.webp`).
pub(crate) struct RolesDev {
    roles: Vec<RoleFile>,
    panel: Entity<RolesPanel>,
    file: Entity<RoleFileView>,
    name: String,
    info: Option<RoleInfo>,
}

impl RolesDev {
    /// Lists `roles` with `role` open; clicking another role opens it.
    pub(crate) fn new(roles: Vec<RoleFile>, role: RoleFile, cx: &mut Context<Self>) -> Self {
        let file = cx.new(|cx| RoleFileView::new(&role, cx));
        let panel = cx.new(|_| RolesPanel::new(roles.clone()));
        let name = role.name.clone();
        panel.update(cx, |panel, _| panel.set_selected(&name));
        cx.subscribe(&panel, |this, _, event: &RolesPanelEvent, cx| {
            let RolesPanelEvent::Open(name) = event;
            this.open(name, cx);
        })
        .detach();
        Self {
            roles,
            panel,
            file,
            name,
            info: role.info,
        }
    }

    fn open(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(role) = self.roles.iter().find(|r| r.name == name).cloned() else {
            return;
        };
        self.file = cx.new(|cx| RoleFileView::new(&role, cx));
        self.name = role.name;
        self.info = role.info;
        cx.notify();
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

/// Which folders the Project dev view opens beyond the mockup's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectTests {
    /// `desktop-project.webp`: tests folded.
    Folded,
    /// `desktop-project-tests-open.webp`.
    Open,
}

/// The Project tab with fx1's main.rs open and an empty right dock
/// (`desktop-project.webp`).
pub(crate) struct ProjectDev {
    worktrees: Vec<WorktreeInfo>,
    panel: Entity<ProjectPanel>,
    file: Option<Entity<FileView>>,
}

impl ProjectDev {
    /// The first worktree with main.rs open and selected, `src/ui` open as
    /// in the mockup, and `tests` as `tests` says.
    pub(crate) fn new(snapshot: Snapshot, tests: ProjectTests, cx: &mut Context<Self>) -> Self {
        const MAIN: &str = "src/main.rs";
        let worktrees = snapshot.worktrees;
        let options = ScopeOption::for_worktrees(&worktrees, &snapshot.agents);
        let panel = cx.new(|cx| {
            let mut panel = ProjectPanel::new(worktrees.clone(), options, cx);
            panel.reveal(MAIN);
            panel.open_folder("src/ui");
            if tests == ProjectTests::Open {
                panel.open_folder("tests");
            }
            panel
        });
        cx.subscribe(&panel, |this, _, event: &ProjectPanelEvent, cx| {
            let ProjectPanelEvent::OpenFile { worktree, path } = event;
            this.open(worktree, path, cx);
        })
        .detach();
        let file = worktrees
            .first()
            .map(|wt| cx.new(|cx| FileView::new(wt, MAIN, cx)));
        Self {
            worktrees,
            panel,
            file,
        }
    }

    fn open(&mut self, owner: &agmen_proto::IdentityId, path: &str, cx: &mut Context<Self>) {
        let Some(wt) = self.worktrees.iter().find(|wt| &wt.owner == owner) else {
            return;
        };
        self.file = Some(cx.new(|cx| FileView::new(wt, path, cx)));
        cx.notify();
    }
}

impl Render for ProjectDev {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Fetch the OS file icons the last frame asked for, as the
        // workspace does.
        cx.defer(FileIcons::fetch_wanted);
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
            .child(div().flex_1().min_w_0().children(self.file.clone()))
            .child(
                div()
                    .w(RIGHT_DOCK_WIDTH)
                    .flex_none()
                    .border_l(BORDER_WIDTH)
                    .border_color(rule),
            )
    }
}

/// A dev view under the app's title bar, so the window chrome (macOS
/// traffic lights, or the Windows and Linux buttons) sits where it will in
/// the workspace.
pub(crate) struct DevFrame {
    title: TitleBar,
    content: AnyView,
}

impl DevFrame {
    pub(crate) fn new(title: TitleBar, content: impl Into<AnyView>) -> Self {
        Self {
            title,
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
            .child(self.title.clone())
            .child(div().flex_1().min_h_0().child(self.content.clone()))
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::backend::chat_snapshot;
    use crate::ui::test_support::{click, init, redraw};

    #[gpui::test]
    fn clicking_a_role_opens_its_file_and_panel(cx: &mut TestAppContext) {
        // Review C165.
        init(cx);
        let roles = chat_snapshot().roles;
        let reviewer = roles.iter().find(|r| r.name == "reviewer").unwrap().clone();
        let (view, cx) = cx.add_window_view(|_, cx| RolesDev::new(roles, reviewer, cx));
        redraw(cx);
        click(cx, "role:engineer");
        redraw(cx);
        view.read_with(cx, |v, cx| {
            assert_eq!(v.name, "engineer");
            assert!(v.info.is_some());
            assert_eq!(v.file.read(cx).file_name(), "engineer.xml");
        });
        assert!(cx.debug_bounds("role-duplicate").is_some());
        click(cx, "role:docs-writer");
        redraw(cx);
        assert!(
            cx.debug_bounds("role-duplicate").is_none(),
            "personal roles are editable"
        );
    }
}
