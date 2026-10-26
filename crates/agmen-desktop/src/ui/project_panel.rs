//! The left dock's Project tab and the file it opens in a center tab
//! (`desktop-project.webp`, `desktop-project-tests-open.webp`).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::RangeInclusive;
use std::path::Path;

use agmen_proto::IdentityId;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div,
};

use super::code_view::{CodeView, GutterMark};
use super::components::{
    Breadcrumb, Button, ButtonStyle, Color, Icon, IconSize, Indent, Label, ListItem,
};
use super::scope_chip::{ScopeChip, ScopeChipEvent, ScopeOption, branch_label};
use super::syntax::Language;
use super::tokens::{ICON_LG, ICON_SM, PROJECT_ROW_HEIGHT, SPACE_6};
use crate::appearance::Appearance;
use crate::backend::{ChangeKind, FileChange, WorktreeHead, WorktreeInfo};
use crate::icons::{FileIcons, UiIcon};

/// What the Project panel tells the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProjectPanelEvent {
    /// A file was clicked: open it in a center tab.
    OpenFile { worktree: IdentityId, path: String },
}

/// How deep the tree goes; deeper paths are left out.
const MAX_DEPTH: u8 = 64;

/// Whether a row is a folder or a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Folder,
    File,
}

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// Relative to the worktree root with `/` separators; empty for the
    /// root itself.
    path: String,
    name: String,
    depth: u8,
    kind: Kind,
}

impl Entry {
    fn is_folder(&self) -> bool {
        self.kind == Kind::Folder
    }
}

/// The last part of `path`.
fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

/// Zed's panel order within a folder: names compared case-insensitively,
/// then exactly, so the order is total.
fn by_name(a: &str, b: &str) -> std::cmp::Ordering {
    let (x, y) = (name_of(a), name_of(b));
    x.to_lowercase()
        .cmp(&y.to_lowercase())
        .then_with(|| x.cmp(y))
}

/// The tree under a root folder named `root`, every folder open, in Zed's
/// order: at each level folders first, then files, each by name.
fn build_tree(root: &str, files: &[String]) -> Vec<Entry> {
    // shortcut: the whole tree is built from one list of every path, as the
    // fixture holds it. The live backend needs the worktree's root path and
    // lazy loading per folder before large repos (100k files) reach it.
    let mut folders: BTreeMap<String, Folder> = BTreeMap::new();
    for file in files {
        let parts: Vec<&str> = file.split('/').filter(|part| !part.is_empty()).collect();
        let Some((name, dirs)) = parts.split_last() else {
            continue;
        };
        let mut parent = String::new();
        for dir in dirs {
            let path = join(&parent, dir);
            folders
                .entry(parent)
                .or_default()
                .folders
                .insert(path.clone());
            parent = path;
        }
        let path = join(&parent, name);
        folders.entry(parent).or_default().files.insert(path);
    }
    let mut out = vec![Entry {
        path: String::new(),
        name: root.to_owned(),
        depth: 0,
        kind: Kind::Folder,
    }];
    push_children(&folders, "", 1, &mut out);
    out
}

/// One folder's children, by path.
#[derive(Debug, Default)]
struct Folder {
    folders: BTreeSet<String>,
    files: BTreeSet<String>,
}

/// Appends `parent`'s children at `depth`, each folder followed by its own
/// children. Stops at [`MAX_DEPTH`], so recursion is bounded.
fn push_children(
    folders: &BTreeMap<String, Folder>,
    parent: &str,
    depth: u8,
    out: &mut Vec<Entry>,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let Some(folder) = folders.get(parent) else {
        return;
    };
    let mut subfolders: Vec<&String> = folder.folders.iter().collect();
    subfolders.sort_by(|a, b| by_name(a, b));
    for path in subfolders {
        out.push(Entry {
            path: path.clone(),
            name: name_of(path).to_owned(),
            depth,
            kind: Kind::Folder,
        });
        push_children(folders, path, depth.saturating_add(1), out);
    }
    let mut files: Vec<&String> = folder.files.iter().collect();
    files.sort_by(|a, b| by_name(a, b));
    for path in files {
        out.push(Entry {
            path: path.clone(),
            name: name_of(path).to_owned(),
            depth,
            kind: Kind::File,
        });
    }
}

/// The marker a changed file shows at the row's end, and its color.
fn change_marker(changes: &[FileChange], path: &str) -> Option<(&'static str, Color)> {
    let change = changes.iter().find(|change| change.path == path)?;
    Some(match change.kind {
        ChangeKind::Modified => ("M", Color::Warning),
        ChangeKind::Added => ("A", Color::Success),
        ChangeKind::Deleted => ("D", Color::Error),
    })
}

/// The root folder's name: the worktree's folder, which agmen names after
/// the branch with `/` turned into `-`.
fn root_name(worktree: &WorktreeInfo) -> String {
    // shortcut: derived from the head until the backend gives each
    // worktree's root path (the live backend, ADR-0011).
    match &worktree.head {
        WorktreeHead::Branch(_) => branch_label(worktree).replace('/', "-"),
        WorktreeHead::Detached { item, .. } => item.clone(),
    }
}

/// Tests find each row as `"tree:{path}"` (the root as `tree:`) and a
/// change marker as `"tree-marker:{path}"`.
pub(crate) struct ProjectPanel {
    worktrees: Vec<WorktreeInfo>,
    current: usize,
    entries: Vec<Entry>,
    /// Open folders by path; the root is `""`.
    open: HashSet<String>,
    selected: Option<String>,
    scope: Entity<ScopeChip>,
}

impl EventEmitter<ProjectPanelEvent> for ProjectPanel {}

impl ProjectPanel {
    /// Shows the first worktree with its root open; `options` are the
    /// scope chip's, one per worktree.
    pub(crate) fn new(
        worktrees: Vec<WorktreeInfo>,
        options: Vec<ScopeOption>,
        cx: &mut Context<Self>,
    ) -> Self {
        let scope = cx.new(|_| ScopeChip::new(options));
        cx.subscribe(&scope, |this, _, event: &ScopeChipEvent, cx| {
            let ScopeChipEvent::Switch(owner) = event;
            this.switch(owner.clone(), cx);
        })
        .detach();
        let mut panel = Self {
            worktrees,
            current: 0,
            entries: Vec::new(),
            open: HashSet::new(),
            selected: None,
            scope,
        };
        panel.show(0);
        panel
    }

    /// Rebuilds the tree for worktree `index`, root open, nothing selected.
    fn show(&mut self, index: usize) {
        self.current = index;
        self.entries = self
            .worktrees
            .get(index)
            .map(|wt| build_tree(&root_name(wt), &wt.files))
            .unwrap_or_default();
        self.open = HashSet::from([String::new()]);
        self.selected = None;
    }

    fn switch(&mut self, owner: IdentityId, cx: &mut Context<Self>) {
        if let Some(index) = self.worktrees.iter().position(|wt| wt.owner == owner) {
            self.show(index);
            cx.notify();
        }
    }

    /// The owner of the shown worktree.
    pub(crate) fn worktree(&self) -> Option<IdentityId> {
        self.worktrees.get(self.current).map(|wt| wt.owner.clone())
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Selects `path` and opens every folder above it, without telling the
    /// workspace.
    pub(crate) fn reveal(&mut self, path: &str) {
        let mut parent = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        if let Some((_, dirs)) = parts.split_last() {
            for dir in dirs {
                parent = join(&parent, dir);
                self.open.insert(parent.clone());
            }
        }
        self.selected = Some(path.to_owned());
    }

    /// Opens the folder at `path`, without telling the workspace.
    pub(crate) fn open_folder(&mut self, path: &str) {
        self.open.insert(path.to_owned());
    }

    /// The rows a folded tree shows, top to bottom.
    fn visible_rows(&self) -> impl Iterator<Item = &Entry> {
        let mut folded_at: Option<u8> = None;
        self.entries.iter().filter(move |entry| {
            if let Some(depth) = folded_at {
                if entry.depth > depth {
                    return false;
                }
                folded_at = None;
            }
            if entry.is_folder() && !self.open.contains(&entry.path) {
                folded_at = Some(entry.depth);
            }
            true
        })
    }

    /// A folder folds or opens; a file is selected and opened.
    fn clicked(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        match entry.kind {
            Kind::Folder => {
                if !self.open.remove(&entry.path) {
                    self.open.insert(entry.path.clone());
                }
            }
            Kind::File => {
                self.selected = Some(entry.path.clone());
                if let Some(owner) = self.worktree() {
                    cx.emit(ProjectPanelEvent::OpenFile {
                        worktree: owner,
                        path: entry.path.clone(),
                    });
                }
            }
        }
        cx.notify();
    }

    fn row(&self, entry: &Entry, cx: &mut Context<Self>) -> ListItem {
        let theme = &cx.global::<Appearance>().theme;
        let icon_color = theme.icon_muted;
        let folder = entry.is_folder();
        let open = folder && self.open.contains(&entry.path);
        let chevron: AnyElement = if folder {
            Icon::new(if open {
                UiIcon::ChevronDown
            } else {
                UiIcon::ChevronRight
            })
            .size(IconSize::Small)
            .color(Color::Muted)
            .into_any_element()
        } else {
            div().w(ICON_SM).flex_none().into_any_element()
        };
        let icon = cx.try_global::<FileIcons>().map_or_else(
            || Icon::new(if folder { UiIcon::Folder } else { UiIcon::File }).into_any_element(),
            |icons| {
                icons
                    .icon(Path::new(&entry.name), folder, open)
                    .render(icon_color, ICON_LG)
            },
        );
        let changes = self
            .worktrees
            .get(self.current)
            .map_or(&[][..], |wt| wt.changes.as_slice());
        let marker = change_marker(changes, &entry.path);
        let name_color = marker.as_ref().map_or(Color::Default, |(_, c)| c.clone());
        let mut item = ListItem::new(format!("tree:{}", entry.path), Indent::Tree(entry.depth))
            .height(PROJECT_ROW_HEIGHT)
            .selected(self.selected.as_deref() == Some(entry.path.as_str()))
            .child(chevron)
            .child(icon)
            .child(Label::new(entry.name.clone()).code().color(name_color));
        if let Some((text, color)) = marker {
            let selector = format!("tree-marker:{}", entry.path);
            item = item.end(
                div().debug_selector(|| selector).child(
                    Label::new(text)
                        .code()
                        .weight(FontWeight::BOLD)
                        .color(color),
                ),
            );
        }
        let clicked = entry.clone();
        item.on_click(cx.listener(move |this, _, _, cx| this.clicked(&clicked, cx)))
    }
}

impl Render for ProjectPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible: Vec<Entry> = self.visible_rows().cloned().collect();
        let rows: Vec<ListItem> = visible.iter().map(|entry| self.row(entry, cx)).collect();
        div()
            .debug_selector(|| "project-panel".into())
            .w_full()
            .flex()
            .flex_col()
            .pt(SPACE_6)
            .child(self.scope.clone())
            .children(rows)
    }
}

/// A file from a worktree in a center tab: the worktree and the path in
/// the breadcrumb, then the text with its change marks.
pub(crate) struct FileView {
    parts: Vec<SharedString>,
    code: Entity<CodeView>,
}

/// The text and change marks of `path` in `worktree`.
fn contents(worktree: &WorktreeInfo, path: &str) -> (String, Vec<GutterMark>) {
    // shortcut: a file the backend has not read shows a placeholder line;
    // the live backend reads files and their marks from `git diff`
    // (ADR-0011).
    worktree.texts.get(path).map_or_else(
        || {
            (
                format!("{path}: the fixture has no contents for this file.\n"),
                Vec::new(),
            )
        },
        |file| (file.text.clone(), file.marks.clone()),
    )
}

impl FileView {
    pub(crate) fn new(worktree: &WorktreeInfo, path: &str, cx: &mut Context<Self>) -> Self {
        let (text, marks) = contents(worktree, path);
        let language = Language::for_path(Path::new(path));
        let code = cx.new(|_| CodeView::new(&text, language, marks));
        let parts = std::iter::once(SharedString::from(worktree.handle.clone()))
            .chain(
                path.split('/')
                    .map(|part| SharedString::from(part.to_owned())),
            )
            .collect();
        Self { parts, code }
    }

    /// Scrolls to `lines` (1-based) and highlights them.
    pub(crate) fn show_lines(&mut self, lines: RangeInclusive<u32>, cx: &mut Context<Self>) {
        self.code.update(cx, |code, cx| code.show_lines(lines, cx));
    }

    #[cfg(test)]
    pub(crate) fn parts(&self) -> Vec<&str> {
        self.parts.iter().map(SharedString::as_ref).collect()
    }
}

impl Render for FileView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        // shortcut: Diff does nothing: the file view sends the workspace no
        // events, and no task in the desktop plan wires one. Revisit when a
        // file view event opens TabItem::Diff, as GitPanelEvent::OpenDiff
        // does from the Git panel.
        let crumb = Breadcrumb::new(self.parts.clone())
            .end(Button::new("file-diff", "Diff").style(ButtonStyle::Plain));
        div()
            .debug_selector(|| "file-view".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .child(crumb)
            .child(div().flex_1().min_h_0().child(self.code.clone()))
    }
}

#[cfg(test)]
#[path = "project_panel_tests.rs"]
mod tests;
