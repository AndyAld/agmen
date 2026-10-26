//! The views center tabs show for items whose view is built: a role's file
//! (Task 10) and a worktree file (Task 11). Each view draws its own
//! breadcrumb, so the pane draws none above it.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{AnyView, App, AppContext as _};

use super::pane::TabItem;
use super::project_panel::FileView;
use super::roles_panel::RoleFileView;
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

/// One pane's views, made the first time their tab shows and kept while
/// the tab stays open.
#[derive(Default)]
pub(crate) struct TabViews {
    sources: Rc<TabSources>,
    views: HashMap<TabItem, AnyView>,
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

    /// The view for `item`, or `None` when it has no view yet and the pane
    /// shows its placeholder. Views of tabs no longer in `open` are
    /// dropped, so the cache holds at most one view per open tab.
    pub(crate) fn view(
        &mut self,
        item: &TabItem,
        open: &[TabItem],
        cx: &mut App,
    ) -> Option<AnyView> {
        self.views.retain(|key, _| open.contains(key));
        if let Some(view) = self.views.get(item) {
            return Some(view.clone());
        }
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
            _ => return None,
        };
        self.views.insert(item.clone(), view.clone());
        Some(view)
    }
}
