//! The left dock's Git tab (`desktop-git.webp`, spec 9.2): the scoped
//! worktree's Changes and Commits, every worktree by person, and the local
//! branches, each section with a count and a fold.

use std::collections::BTreeSet;
use std::path::Path;

use gpui::{
    AnyElement, AppContext as _, Context, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div,
};

use super::components::{Badge, Chip, Color, Icon, IconSize, Indent, Label, LabelSize, ListItem};
use super::format::clock;
use super::tokens::{
    BORDER_WIDTH, ICON_MD, PROJECT_ROW_HEIGHT, RADIUS_MD, SPACE_2, SPACE_6, SPACE_8, SPACE_10,
};
use crate::appearance::Appearance;
use crate::backend::{
    ChangeKind, CommitInfo, FileChange, WorktreeHead, WorktreeInfo, WorktreeState,
};
use crate::icons::{FileIcons, UiIcon};

/// What the Git panel tells the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitPanelEvent {
    /// A change was selected: open its diff. `worktree` is the owner's
    /// handle, `path` relative to the worktree root.
    OpenDiff { worktree: String, path: String },
}

/// The panel's sections, in the order it shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum GitSection {
    Changes,
    Commits,
    Worktrees,
    Branches,
}

impl GitSection {
    const ALL: [Self; 4] = [
        Self::Changes,
        Self::Commits,
        Self::Worktrees,
        Self::Branches,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::Changes => "CHANGES",
            Self::Commits => "COMMITS",
            Self::Worktrees => "WORKTREES",
            Self::Branches => "BRANCHES",
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Commits => "commits",
            Self::Worktrees => "worktrees",
            Self::Branches => "branches",
        }
    }
}

/// Tests find each section header as `"git-section:{slug}"` and its count
/// as `"git-count:{slug}"`; rows as `"git-change:{path}"`,
/// `"git-commit:{sha}"`, `"git-person:{name}"`, `"git-worktree:{handle}"`
/// and `"git-branch:{name}"`; the scope row as `git-scope`.
pub(crate) struct GitPanel {
    /// The person's own first; the first is the scoped one.
    worktrees: Vec<WorktreeInfo>,
    branches: Vec<String>,
    folded: BTreeSet<GitSection>,
    folded_people: BTreeSet<String>,
    /// The selected change's path in the scoped worktree.
    selected: Option<String>,
}

impl EventEmitter<GitPanelEvent> for GitPanel {}

/// The person part of a handle: `andy` of `andy/fx1`.
fn person(handle: &str) -> &str {
    handle.split_once('/').map_or(handle, |(person, _)| person)
}

/// The agent part of a handle: `fx1` of `andy/fx1`.
fn agent_name(handle: &str) -> &str {
    handle.split_once('/').map_or(handle, |(_, agent)| agent)
}

/// A commit's author as its row shows it: the agent alone when it is the
/// worktree owner's person's, else the whole handle.
fn commit_author<'a>(author: &'a str, owner: &str) -> &'a str {
    if person(author) == person(owner) {
        agent_name(author)
    } else {
        author
    }
}

/// The commit row's tooltip: author and time of day in `tz`.
fn commit_tooltip(commit: &CommitInfo, tz: &jiff::tz::TimeZone) -> String {
    format!("{} · {}", commit.author, clock(commit.at, tz))
}

/// A branch cut to its type and item, as the Commits header shows it:
/// `fix/c015` of `fix/c015-main-conflict`, `test/b-415` of
/// `test/b-415-replay-paging`. A branch without an item stays whole.
fn branch_item(branch: &str) -> &str {
    let Some((kind, rest)) = branch.split_once('/') else {
        return branch;
    };
    let letters = rest.bytes().take_while(u8::is_ascii_lowercase).count();
    let dash = usize::from(rest.as_bytes().get(letters) == Some(&b'-'));
    let digits = rest
        .bytes()
        .skip(letters + dash)
        .take_while(u8::is_ascii_digit)
        .count();
    if letters == 0 || digits == 0 {
        return branch;
    }
    let end = kind.len() + 1 + letters + dash + digits;
    branch.get(..end).unwrap_or(branch)
}

impl GitPanel {
    /// Branches and other people's worktrees start folded, as in the mockup.
    pub(crate) fn new(
        worktrees: Vec<WorktreeInfo>,
        branches: impl IntoIterator<Item = String>,
    ) -> Self {
        let me = worktrees.first().map(|w| person(&w.handle).to_owned());
        let folded_people = worktrees
            .iter()
            .map(|w| person(&w.handle))
            .filter(|p| me.as_deref() != Some(*p))
            .map(str::to_owned)
            .collect();
        Self {
            worktrees,
            branches: branches.into_iter().collect(),
            folded: BTreeSet::from([GitSection::Branches]),
            folded_people,
            selected: None,
        }
    }

    fn scoped(&self) -> Option<&WorktreeInfo> {
        self.worktrees.first()
    }

    /// What a section's header shows beside its title: a count, or for
    /// Commits the scoped branch's type and item.
    pub(crate) fn badge(&self, section: GitSection) -> String {
        match section {
            GitSection::Changes => self.scoped().map_or(0, |w| w.changes.len()).to_string(),
            GitSection::Commits => self.scoped().map_or_else(String::new, |w| match &w.head {
                WorktreeHead::Branch(name) => branch_item(name).to_owned(),
                WorktreeHead::Detached { at, .. } => at.to_string(),
            }),
            GitSection::Worktrees => self.worktrees.len().to_string(),
            GitSection::Branches => self.branches.len().to_string(),
        }
    }

    /// Folds or unfolds `section`.
    pub(crate) fn toggle_section(&mut self, section: GitSection, cx: &mut Context<Self>) {
        if !self.folded.remove(&section) {
            self.folded.insert(section);
        }
        cx.notify();
    }

    fn toggle_person(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.folded_people.remove(name) {
            self.folded_people.insert(name.to_owned());
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Highlights `path`'s change row without telling the workspace.
    pub(crate) fn set_selected(&mut self, path: &str) {
        self.selected = Some(path.to_owned());
    }

    fn change_clicked(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(worktree) = self.scoped().map(|w| w.handle.clone()) else {
            return;
        };
        self.set_selected(path);
        cx.emit(GitPanelEvent::OpenDiff {
            worktree,
            path: path.to_owned(),
        });
        cx.notify();
    }

    fn section_row(&self, section: GitSection, cx: &mut Context<Self>) -> ListItem {
        let chevron = if self.folded.contains(&section) {
            UiIcon::ChevronRight
        } else {
            UiIcon::ChevronDown
        };
        let slug = section.slug();
        let count = div()
            .debug_selector(move || format!("git-count:{slug}"))
            .child(Badge::new(self.badge(section), Color::Muted));
        ListItem::new(format!("git-section:{slug}"), Indent::Group)
            .leading(Icon::new(chevron).size(IconSize::Small).color(Color::Muted))
            .child(
                Label::new(section.title())
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .weight(FontWeight::SEMIBOLD),
            )
            .child(count)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_section(section, cx)))
    }

    fn change_row(&self, change: &FileChange, cx: &mut Context<Self>) -> ListItem {
        let theme = &cx.global::<Appearance>().theme;
        let icon_color = theme.icon_muted;
        let name = change.path.rsplit('/').next().unwrap_or(&change.path);
        let icon = cx.try_global::<FileIcons>().map_or_else(
            || {
                Icon::new(UiIcon::File)
                    .size(IconSize::Small)
                    .into_any_element()
            },
            |icons| {
                icons
                    .icon(Path::new(name), false, false)
                    .render(icon_color, ICON_MD)
            },
        );
        let (letter, color) = match change.kind {
            ChangeKind::Added => ("A", Color::Success),
            ChangeKind::Modified => ("M", Color::Warning),
            ChangeKind::Deleted => ("D", Color::Error),
        };
        let path = change.path.clone();
        ListItem::new(format!("git-change:{}", change.path), Indent::Depth(0))
            .height(PROJECT_ROW_HEIGHT)
            .selected(self.selected.as_deref() == Some(change.path.as_str()))
            .leading(
                Label::new(letter)
                    .size(LabelSize::Small)
                    .color(color)
                    .weight(FontWeight::BOLD),
            )
            .child(icon)
            .child(Label::new(change.path.clone()).code())
            .end(
                Label::new(format!("+{} \u{2212}{}", change.added, change.removed))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.change_clicked(&path, cx)))
    }

    fn commit_row(commit: &CommitInfo, owner: &str) -> AnyElement {
        let tip: SharedString = commit_tooltip(commit, &jiff::tz::TimeZone::system()).into();
        let row = ListItem::new(format!("git-commit:{}", commit.sha), Indent::Depth(0))
            .height(PROJECT_ROW_HEIGHT)
            .child(
                Label::new(commit.sha.to_string())
                    .code()
                    .color(Color::Accent),
            )
            .child(Label::new(commit.subject.clone()).size(LabelSize::Small))
            .end(
                Label::new(commit_author(&commit.author, owner).to_owned())
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            );
        div()
            .id(SharedString::from(format!("git-commit-tip:{}", commit.sha)))
            .w_full()
            .tooltip(move |_, cx| {
                let text = tip.clone();
                cx.new(|_| Tooltip(text)).into()
            })
            .child(row)
            .into_any_element()
    }

    fn person_row(&self, name: &str, count: usize, cx: &mut Context<Self>) -> ListItem {
        let folded = self.folded_people.contains(name);
        let chevron = if folded {
            UiIcon::ChevronRight
        } else {
            UiIcon::ChevronDown
        };
        let owned = name.to_owned();
        let mut row = ListItem::new(format!("git-person:{name}"), Indent::Depth(0))
            .height(PROJECT_ROW_HEIGHT)
            .leading(Icon::new(chevron).size(IconSize::Small).color(Color::Muted))
            .child(Label::new(name.to_owned()).weight(FontWeight::MEDIUM));
        if folded {
            row = row.child(
                Label::new(count.to_string())
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            );
        }
        row.on_click(cx.listener(move |this, _, _, cx| this.toggle_person(&owned, cx)))
    }

    fn worktree_row(worktree: &WorktreeInfo) -> ListItem {
        let color = match worktree.state {
            WorktreeState::Pushed => Color::Success,
            WorktreeState::LocalOnly => Color::Warning,
            WorktreeState::Reading => Color::Accent,
        };
        let mut row = ListItem::new(
            format!("git-worktree:{}", worktree.handle),
            Indent::Depth(1),
        )
        .height(PROJECT_ROW_HEIGHT)
        .leading(Label::new("●").size(LabelSize::XSmall).color(color))
        // A long head clips here so the agent's name stays in view.
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .child(Label::new(worktree.head.label()).code()),
        );
        if let WorktreeHead::Detached { item, .. } = &worktree.head {
            row = row.child(Label::new(item.clone()).code().color(Color::Placeholder));
        }
        row.end(
            Label::new(agent_name(&worktree.handle).to_owned())
                .size(LabelSize::Small)
                .color(Color::Placeholder),
        )
    }

    fn worktree_rows(&self, rows: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        let mut people: Vec<&str> = Vec::new();
        for w in &self.worktrees {
            let p = person(&w.handle);
            if !people.contains(&p) {
                people.push(p);
            }
        }
        for name in people {
            let theirs: Vec<&WorktreeInfo> = self
                .worktrees
                .iter()
                .filter(|w| person(&w.handle) == name)
                .collect();
            rows.push(self.person_row(name, theirs.len(), cx).into_any_element());
            if !self.folded_people.contains(name) {
                rows.extend(
                    theirs
                        .into_iter()
                        .map(|w| Self::worktree_row(w).into_any_element()),
                );
            }
        }
    }

    fn scope_row(&self) -> impl IntoElement {
        let label = self.scoped().map_or_else(String::new, |w| {
            format!("{} · {}", agent_name(&w.handle), w.head.label())
        });
        // shortcut: a placeholder for the worktree scope chip; the merge with
        // Task 11 swaps in its src/ui/scope_chip.rs, which also switches the
        // scoped worktree.
        div()
            .debug_selector(|| "git-scope".into())
            .w_full()
            .pt(SPACE_2)
            .pb(SPACE_8)
            .px(SPACE_10)
            .child(Chip::new(label).dot(Color::Warning))
    }
}

impl Render for GitPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        for section in GitSection::ALL {
            rows.push(self.section_row(section, cx).into_any_element());
            if self.folded.contains(&section) {
                continue;
            }
            match section {
                GitSection::Changes => {
                    let changes = self.scoped().map(|w| w.changes.clone()).unwrap_or_default();
                    for change in &changes {
                        rows.push(self.change_row(change, cx).into_any_element());
                    }
                }
                GitSection::Commits => {
                    if let Some(w) = self.scoped() {
                        rows.extend(w.commits.iter().map(|c| Self::commit_row(c, &w.handle)));
                    }
                }
                GitSection::Worktrees => self.worktree_rows(&mut rows, cx),
                GitSection::Branches => rows.extend(self.branches.iter().map(|name| {
                    ListItem::new(format!("git-branch:{name}"), Indent::Depth(0))
                        .height(PROJECT_ROW_HEIGHT)
                        .leading(
                            Icon::new(UiIcon::Git)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .child(Label::new(name.clone()).code())
                        .into_any_element()
                })),
            }
        }
        div()
            .w_full()
            .flex()
            .flex_col()
            .py(SPACE_6)
            .child(self.scope_row())
            .children(rows)
    }
}

/// A one-line tooltip on the elevated surface.
struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        div()
            .py(SPACE_2)
            .px(SPACE_6)
            .rounded(RADIUS_MD)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.elevated_surface)
            .child(Label::new(self.0.clone()).size(LabelSize::Small))
    }
}

#[cfg(test)]
#[path = "git_panel_tests.rs"]
mod tests;
