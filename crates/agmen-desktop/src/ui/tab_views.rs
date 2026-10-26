//! The views center tabs show for items whose view is built: a role's file
//! (Task 10), a worktree file (Task 11), a change's diff (Task 12), and
//! Settings and its file (Task 17).

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use gpui::{AnyView, AppContext as _, Context, Subscription};

use super::code_view::CodeView;
use super::diff_view::DiffView;
use super::pane::{Pane, TabItem};
use super::project_panel::FileView;
use super::roles_panel::RoleFileView;
use super::settings_view::{SettingsEvent, SettingsSources, SettingsView};
use super::syntax::Language;
use crate::backend::{RoleFile, Snapshot, WorktreeInfo};

/// The data the views read, from the snapshot.
#[derive(Debug, Clone, Default)]
pub(crate) struct TabSources {
    roles: Vec<RoleFile>,
    worktrees: Vec<WorktreeInfo>,
}

impl TabSources {
    pub(crate) fn from_snapshot(snapshot: &Snapshot) -> Self {
        Self {
            roles: snapshot.roles.clone(),
            worktrees: snapshot.worktrees.clone(),
        }
    }
}

/// A tab's view and the subscription to its events, dropped together.
struct TabView {
    view: AnyView,
    _events: Option<Subscription>,
}

/// One pane's views, made the first time their tab shows and kept while
/// the tab stays open.
#[derive(Default)]
pub(crate) struct TabViews {
    sources: Rc<TabSources>,
    views: HashMap<TabItem, TabView>,
}

impl std::fmt::Debug for TabViews {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TabViews")
            .field("views", &self.views.len())
            .finish_non_exhaustive()
    }
}

impl TabViews {
    pub(crate) fn new(sources: Rc<TabSources>) -> Self {
        Self {
            sources,
            views: HashMap::new(),
        }
    }

    /// Whether `item`'s view draws its own breadcrumb, so the pane draws
    /// none above it.
    pub(crate) const fn draws_breadcrumb(item: &TabItem) -> bool {
        matches!(
            item,
            TabItem::RoleFile(_) | TabItem::File { .. } | TabItem::Settings(_)
        )
    }

    /// The view for `item`, or `None` when it has no view yet and the pane
    /// shows its placeholder. Views of tabs no longer in `open` are
    /// dropped, so the cache holds at most one view per open tab.
    pub(crate) fn view(
        &mut self,
        item: &TabItem,
        open: &[TabItem],
        cx: &mut Context<Pane>,
    ) -> Option<AnyView> {
        self.views.retain(|key, _| open.contains(key));
        if let Some(tab) = self.views.get(item) {
            return Some(tab.view.clone());
        }
        let mut events = None;
        let view: AnyView = match item {
            TabItem::RoleFile(name) => {
                let role = self.sources.roles.iter().find(|r| r.name == *name)?;
                cx.new(|cx| RoleFileView::new(role, cx)).into()
            }
            TabItem::File { worktree, path } => {
                let wt = self
                    .sources
                    .worktrees
                    .iter()
                    .find(|wt| wt.handle == *worktree)?;
                cx.new(|cx| FileView::new(wt, path, cx)).into()
            }
            TabItem::Diff { worktree, path } => {
                // A path with no change shows the diff view's empty state.
                let hunks = self
                    .sources
                    .worktrees
                    .iter()
                    .find(|wt| wt.handle == *worktree)
                    .and_then(|wt| wt.changes.iter().find(|c| c.path == *path))
                    .map(|c| c.hunks.clone())
                    .unwrap_or_default();
                let language = Language::for_path(std::path::Path::new(path));
                cx.new(|_| DiffView::new(hunks, language)).into()
            }
            TabItem::Settings(_) => {
                let sources = cx.try_global::<SettingsSources>()?.clone();
                let view = cx.new(|cx| SettingsView::new(sources, cx));
                // "Open settings.toml" opens the file in this pane.
                events = Some(cx.subscribe(&view, |pane, _, event, cx| {
                    let SettingsEvent::OpenFile(path) = event;
                    pane.open(TabItem::SettingsFile(path.clone()), cx);
                }));
                view.into()
            }
            TabItem::SettingsFile(path) => {
                let text = settings_file_text(path);
                cx.new(|_| CodeView::new(&text, Language::Toml, Vec::new()))
                    .into()
            }
            _ => return None,
        };
        self.views.insert(
            item.clone(),
            TabView {
                view: view.clone(),
                _events: events,
            },
        );
        Some(view)
    }
}

/// The settings file's text, or a TOML comment saying why there is none:
/// Settings writes the file on the first change, so it may not exist yet.
fn settings_file_text(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            "# No settings.toml yet: Settings writes it on the first change.\n".to_owned()
        }
        Err(err) => format!("# Could not read {}: {err}\n", path.display()),
    }
}
