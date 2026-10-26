//! The views center tabs show for items whose view is built: an agent's
//! session and a role's file (Task 10), a worktree file (Task 11), a
//! change's diff (Task 12), and Settings and its file (Task 17).

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use gpui::{AnyView, AppContext as _, Context, Subscription, Window};

use super::agent_view::AgentView;
use super::code_view::CodeView;
use super::diff_view::{DiffTab, DiffTabEvent};
use super::pane::{Pane, TabItem};
use super::project_panel::FileView;
use super::roles_panel::RoleFileView;
use super::settings_view::{SettingsEvent, SettingsSources, SettingsView};
use super::syntax::Language;
use super::{plan_model::PlanHandle, spec_view::SpecView};
use crate::backend::Snapshot;

/// The data the views read: the snapshot the center was built from,
/// shared with the workspace.
#[derive(Debug, Clone)]
pub(crate) struct TabSources {
    snapshot: Rc<Snapshot>,
}

impl TabSources {
    pub(crate) fn from_snapshot(snapshot: &Rc<Snapshot>) -> Self {
        Self {
            snapshot: Rc::clone(snapshot),
        }
    }

    #[cfg(test)]
    pub(crate) const fn snapshot(&self) -> &Rc<Snapshot> {
        &self.snapshot
    }
}

/// A tab's view and the subscription to its events, dropped together.
struct TabView {
    view: AnyView,
    _events: Option<Subscription>,
}

/// One pane's views, made the first time their tab shows and kept while
/// the tab stays open.
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
            TabItem::Agent(_)
                | TabItem::RoleFile(_)
                | TabItem::File { .. }
                | TabItem::Diff { .. }
                | TabItem::Settings(_)
                | TabItem::Spec(_)
        )
    }

    /// The view for `item`, or `None` when it has no view yet and the pane
    /// shows its placeholder. Views of tabs no longer in `open` are
    /// dropped, so the cache holds at most one view per open tab.
    pub(crate) fn view(
        &mut self,
        item: &TabItem,
        open: &[TabItem],
        window: &mut Window,
        cx: &mut Context<Pane>,
    ) -> Option<AnyView> {
        self.views.retain(|key, _| open.contains(key));
        if let Some(tab) = self.views.get(item) {
            return Some(tab.view.clone());
        }
        let mut events = None;
        let snapshot = &self.sources.snapshot;
        let view: AnyView = match item {
            // An agent with no session on this machine keeps the placeholder.
            TabItem::Agent(id) => {
                let session = snapshot.sessions.get(id)?.clone();
                cx.new(|cx| AgentView::new(snapshot, id, session, window, cx))
                    .into()
            }
            TabItem::RoleFile(name) => {
                let role = snapshot.roles.iter().find(|r| r.name == *name)?;
                cx.new(|cx| RoleFileView::new(role, cx)).into()
            }
            TabItem::File { worktree, path } => {
                let wt = snapshot
                    .worktrees
                    .iter()
                    .find(|wt| wt.handle == *worktree)?;
                cx.new(|cx| FileView::new(wt, path, cx)).into()
            }
            // A worktree the snapshot does not have keeps the placeholder,
            // as a file tab does.
            TabItem::Diff { worktree, path } => {
                let wt = snapshot
                    .worktrees
                    .iter()
                    .find(|wt| wt.handle == *worktree)?;
                let view = cx.new(|cx| DiffTab::new(wt, path, cx));
                // Open file opens the file's tab in this pane.
                events = Some(cx.subscribe(&view, |pane, _, event, cx| {
                    let DiffTabEvent::OpenFile { worktree, path } = event;
                    pane.open(
                        TabItem::File {
                            worktree: worktree.clone(),
                            path: path.clone(),
                        },
                        cx,
                    );
                }));
                view.into()
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
                // shortcut: the text is read when the tab is first drawn and
                // kept while it stays open, so a change made in Settings
                // after that shows once the tab is reopened; reload it on
                // Settings' saves when the file tab gets edited in place.
                let text = settings_file_text(path);
                cx.new(|_| CodeView::new(&text, Language::Toml, Vec::new()))
                    .into()
            }
            // Task 19: a spec's tab, over the workspace's plan model.
            TabItem::Spec(path) => {
                let model = cx.try_global::<PlanHandle>()?.0.clone();
                cx.new(|cx| SpecView::new(model, path, cx)).into()
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

#[cfg(test)]
mod tests {
    use super::settings_file_text;

    // Review (wirec7 minor 2): the two reasons a settings file has no text.
    #[test]
    fn settings_file_text_says_why_there_is_none() {
        let home = tempfile::tempdir().unwrap();
        let missing = settings_file_text(&home.path().join("settings.toml"));
        assert!(missing.starts_with("# No settings.toml yet"), "{missing}");
        let unreadable = settings_file_text(home.path());
        assert!(unreadable.starts_with("# Could not read "), "{unreadable}");
        let file = home.path().join("written.toml");
        std::fs::write(&file, "mode = \"dark\"\n").unwrap();
        assert_eq!(settings_file_text(&file), "mode = \"dark\"\n");
    }
}
