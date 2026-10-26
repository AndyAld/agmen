//! The left dock's Git tab (`desktop-git.webp`, spec 9.2): the scoped
//! worktree's Changes and Commits, every worktree by person, and the local
//! branches, each section with a count and a fold.

use std::collections::BTreeSet;
use std::path::Path;

use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::components::{
    Badge, Color, Dot, Icon, IconSize, Indent, Label, LabelSize, ListItem, Tooltip,
};
use super::format::clock;
use super::scope_chip::{ScopeChip, ScopeChipEvent, ScopeOption};
use super::tagged;
use super::tokens::{ICON_LG, PROJECT_ROW_HEIGHT, SPACE_6};
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
/// and `"git-branch:{name}"`; the scope row as `git-scope`. Row parts add a
/// suffix: a change's `:status`, `:icon` and `:path`, a commit's `:sha`, a
/// person's `:name`, a worktree's `:dot` and `:head`.
pub(crate) struct GitPanel {
    /// The person's own first; the first is the scoped one.
    worktrees: Vec<WorktreeInfo>,
    branches: Vec<String>,
    folded: BTreeSet<GitSection>,
    folded_people: BTreeSet<String>,
    /// The selected change's path in the scoped worktree.
    selected: Option<String>,
    /// Which worktree Changes and Commits show; the scope chip sets it.
    scoped: usize,
    scope: Entity<ScopeChip>,
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

/// A section's count: mono 11 in text.placeholder on a count chip (V94,
/// V114, node 5113).
fn count_badge(text: impl Into<SharedString>) -> Badge {
    Badge::new(text, Color::Placeholder).count()
}

/// A row's trailing detail, such as `+2 −5` or an author: console.py's mono
/// 11 in text.placeholder (V93).
fn meta(text: impl Into<SharedString>) -> Label {
    Label::new(text)
        .code()
        .size(LabelSize::Small)
        .color(Color::Placeholder)
}

/// A change's status letter: mono 12 Bold in its kind's color (node 5116).
fn status_letter(kind: ChangeKind) -> Label {
    let (letter, color) = match kind {
        ChangeKind::Added => ("A", Color::Success),
        ChangeKind::Modified => ("M", Color::Warning),
        ChangeKind::Deleted => ("D", Color::Error),
    };
    Label::new(letter)
        .code()
        .weight(FontWeight::BOLD)
        .color(color)
}

impl GitPanel {
    /// Branches and other people's worktrees start folded, as in the mockup.
    /// `options` are the scope chip's, one per worktree.
    pub(crate) fn new(
        worktrees: Vec<WorktreeInfo>,
        branches: impl IntoIterator<Item = String>,
        options: Vec<ScopeOption>,
        cx: &mut Context<Self>,
    ) -> Self {
        let scope = cx.new(|_| ScopeChip::new(options));
        cx.subscribe(&scope, |this, _, event: &ScopeChipEvent, cx| {
            let ScopeChipEvent::Switch(owner) = event;
            this.switch(owner, cx);
        })
        .detach();
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
            scoped: 0,
            scope,
        }
    }

    fn scoped(&self) -> Option<&WorktreeInfo> {
        self.worktrees.get(self.scoped)
    }

    /// Shows `owner`'s worktree in Changes and Commits, nothing selected,
    /// and names it on the scope chip.
    pub(crate) fn switch(&mut self, owner: &agmen_proto::IdentityId, cx: &mut Context<Self>) {
        if let Some(index) = self.worktrees.iter().position(|w| &w.owner == owner) {
            self.scoped = index;
            self.selected = None;
            self.scope.update(cx, |chip, cx| chip.select(owner, cx));
            cx.notify();
        }
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
        let folded = !self.folded.contains(&section);
        self.set_folded(section, folded, cx);
    }

    /// Folds `section` when `folded`, else unfolds it.
    pub(crate) fn set_folded(&mut self, section: GitSection, folded: bool, cx: &mut Context<Self>) {
        if folded {
            self.folded.insert(section);
        } else {
            self.folded.remove(&section);
        }
        cx.notify();
    }

    fn toggle_person(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.folded_people.remove(name) {
            self.folded_people.insert(name.to_owned());
        }
        cx.notify();
    }

    /// The owner of the worktree Changes and Commits show.
    #[cfg(test)]
    pub(crate) fn scoped_owner(&self) -> Option<&agmen_proto::IdentityId> {
        self.scoped().map(|w| &w.owner)
    }

    /// The owner the scope chip shows.
    #[cfg(test)]
    pub(crate) fn chip_owner(&self, cx: &gpui::App) -> Option<agmen_proto::IdentityId> {
        self.scope.read(cx).selected_owner().cloned()
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
            .child(count_badge(self.badge(section)));
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
        // The Project panel's 16 px file icon (V91).
        let icon = cx.try_global::<FileIcons>().map_or_else(
            || Icon::new(UiIcon::File).into_any_element(),
            |icons| {
                icons
                    .icon(Path::new(name), false, false)
                    .render(icon_color, ICON_LG)
            },
        );
        let path = change.path.clone();
        let id = format!("git-change:{}", change.path);
        // Rows sit on the Project panel's tree indent, console.py's
        // tree_row (V91): no leading slot, 6 px gaps.
        ListItem::new(id.clone(), Indent::Tree(1))
            .height(PROJECT_ROW_HEIGHT)
            .selected(self.selected.as_deref() == Some(change.path.as_str()))
            .child(tagged(format!("{id}:status"), status_letter(change.kind)).flex_none())
            .child(tagged(format!("{id}:icon"), icon).flex_none())
            .child(tagged(format!("{id}:path"), Label::new(change.path.clone()).code()).flex_none())
            .end(meta(format!(
                "+{} \u{2212}{}",
                change.added, change.removed
            )))
            .on_click(cx.listener(move |this, _, _, cx| this.change_clicked(&path, cx)))
    }

    fn commit_row(commit: &CommitInfo, owner: &str) -> AnyElement {
        let tip: SharedString = commit_tooltip(commit, &jiff::tz::TimeZone::system()).into();
        let id = format!("git-commit:{}", commit.sha);
        let row = ListItem::new(id.clone(), Indent::Tree(1))
            .height(PROJECT_ROW_HEIGHT)
            .child(
                tagged(
                    format!("{id}:sha"),
                    Label::new(commit.sha.to_string())
                        .code()
                        .color(Color::Accent),
                )
                .flex_none(),
            )
            .child(Label::new(commit.subject.clone()).size(LabelSize::Small))
            .end(meta(commit_author(&commit.author, owner).to_owned()));
        div()
            .id(SharedString::from(format!("git-commit-tip:{}", commit.sha)))
            .w_full()
            .tooltip(move |_, cx| {
                let text = tip.clone();
                cx.new(|_| Tooltip::new(text)).into()
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
        let id = format!("git-person:{name}");
        let mut row = ListItem::new(id.clone(), Indent::Tree(1))
            .height(PROJECT_ROW_HEIGHT)
            .leading(Icon::new(chevron).size(IconSize::Small).color(Color::Muted))
            .child(
                tagged(
                    format!("{id}:name"),
                    Label::new(name.to_owned()).weight(FontWeight::MEDIUM),
                )
                .flex_none(),
            );
        if folded {
            row = row.child(meta(count.to_string()));
        }
        row.on_click(cx.listener(move |this, _, _, cx| this.toggle_person(&owned, cx)))
    }

    fn worktree_row(worktree: &WorktreeInfo) -> ListItem {
        let color = match worktree.state {
            WorktreeState::Pushed => Color::Success,
            WorktreeState::LocalOnly => Color::Warning,
            WorktreeState::Reading => Color::Accent,
        };
        let id = format!("git-worktree:{}", worktree.handle);
        let mut row = ListItem::new(id.clone(), Indent::Tree(2))
            .height(PROJECT_ROW_HEIGHT)
            .child(tagged(format!("{id}:dot"), Dot::new(color)).flex_none())
            // A long head clips here so the agent's name stays in view.
            .child(
                div()
                    .debug_selector(move || format!("{id}:head"))
                    .min_w_0()
                    .overflow_hidden()
                    .child(Label::new(worktree.head.label()).code()),
            );
        if let WorktreeHead::Detached { item, .. } = &worktree.head {
            row = row.child(Label::new(item.clone()).code().color(Color::Placeholder));
        }
        row.end(meta(agent_name(&worktree.handle).to_owned()))
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
        div()
            .debug_selector(|| "git-scope".into())
            .w_full()
            // The chip pads itself, as in the Project panel.
            .child(self.scope.clone())
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
                    if let Some(w) = self.scoped() {
                        for change in &w.changes {
                            rows.push(self.change_row(change, cx).into_any_element());
                        }
                    }
                }
                GitSection::Commits => {
                    if let Some(w) = self.scoped() {
                        rows.extend(w.commits.iter().map(|c| Self::commit_row(c, &w.handle)));
                    }
                }
                GitSection::Worktrees => self.worktree_rows(&mut rows, cx),
                GitSection::Branches => rows.extend(self.branches.iter().map(|name| {
                    ListItem::new(format!("git-branch:{name}"), Indent::Tree(1))
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

#[cfg(test)]
#[path = "git_panel_tests.rs"]
mod tests;
