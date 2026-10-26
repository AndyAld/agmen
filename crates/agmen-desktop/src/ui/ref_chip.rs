//! Reference chips in message text and their hover cards (spec 9.3; mockup
//! `rich_body` and `ref_color` in `docs/mockups/scripts/console.py`).
//!
//! [`ChatContext`] is what chips, message bodies and message rows look up,
//! built once per snapshot. [`RefChip`] draws one reference; a click reports
//! [`RefEvent::OpenRef`], a ⌘-click [`RefEvent::OpenRefInSplit`], and hovering
//! shows a [`RefCard`].

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use agmen_proto::{IdentityId, MessageId, Payload, StampedEnvelope, Timestamp, Verdict};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::components::{ChangeHandler, Color, KvRow, Label, LabelSize};
use super::format::{money_of, title_case};
use super::tokens::{
    BORDER_WIDTH, RADIUS_LG, RADIUS_REF, REF_CARD_WIDTH, SPACE_4, SPACE_6, SPACE_10, SPACE_12,
    TINT_FILL, tint,
};
use crate::appearance::Appearance;
use crate::backend::{AgentInfo, AgentStatus, Snapshot};
use crate::refs::{Known, RefTarget, Span, find_refs};

/// The debug selector of a drawn hover card, for tests.
#[cfg(test)]
pub(crate) const REF_CARD_SELECTOR: &str = "ref-card";

/// What a reference chip asks its caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefEvent {
    /// Open the reference in a tab (a click).
    OpenRef(RefTarget),
    /// Open the reference in a split beside the current pane (a ⌘-click).
    OpenRefInSplit(RefTarget),
}

/// What chips, message bodies and message rows look up, built once per
/// snapshot so a frame does no roster scans per chip.
#[derive(Debug)]
pub(crate) struct ChatContext {
    snapshot: Rc<Snapshot>,
    known: Known,
    /// Each roster agent's lowercase role, by handle.
    roles: BTreeMap<String, String>,
    /// Handles of the agents this person's agmend runs.
    local: BTreeSet<String>,
    /// Ids of the messages in the snapshot, so a reply can tell whether its
    /// parent is in the chat.
    ids: BTreeSet<MessageId>,
    utc_offset_minutes: i32,
}

impl ChatContext {
    #[must_use]
    pub(crate) fn new(snapshot: Rc<Snapshot>) -> Self {
        let known = Known::from_snapshot(&snapshot);
        let mut roles = BTreeMap::new();
        let mut local = BTreeSet::new();
        for group in &snapshot.roster {
            for agent in &group.agents {
                roles.insert(agent.handle.clone(), agent.role.clone());
                if group.you {
                    local.insert(agent.handle.clone());
                }
            }
        }
        let ids = snapshot
            .messages
            .iter()
            .map(|m| m.client.id.clone())
            .collect();
        Self {
            snapshot,
            known,
            roles,
            local,
            ids,
            utc_offset_minutes: 0,
        }
    }

    /// Shows times at this offset from UTC, in minutes.
    // shortcut: the caller passes the offset, because the desktop crate has
    // no time zone database (agmen-proto builds jiff without one); read the
    // system zone once agmend or a tz source provides it.
    #[must_use]
    pub(crate) const fn with_utc_offset(mut self, minutes: i32) -> Self {
        self.utc_offset_minutes = minutes;
        self
    }

    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The references in a message's body: the hub's, then the git names
    /// local git confirms.
    #[must_use]
    pub(crate) fn spans(&self, body: &str, hub_refs: &[agmen_proto::Ref]) -> Vec<Span> {
        find_refs(body, hub_refs, &self.known)
    }

    /// The lowercase role of the roster agent `handle`.
    pub(crate) fn role(&self, handle: &str) -> Option<&str> {
        self.roles.get(handle).map(String::as_str)
    }

    pub(crate) fn handle(&self, id: &IdentityId) -> Option<&str> {
        self.known.handle(id)
    }

    /// Whether `handle` is an agent this person's agmend runs (spec 5.4:
    /// only those can be taken over).
    pub(crate) fn is_local_agent(&self, handle: &str) -> bool {
        self.local.contains(handle)
    }

    pub(crate) fn has_message(&self, id: &MessageId) -> bool {
        self.ids.contains(id)
    }

    /// `at` as `15:27` at the context's offset.
    #[must_use]
    pub(crate) fn clock(&self, at: Timestamp) -> String {
        let secs = at
            .as_second()
            .saturating_add(i64::from(self.utc_offset_minutes) * 60);
        let day = secs.rem_euclid(86_400);
        format!("{:02}:{:02}", day / 3600, day % 3600 / 60)
    }

    /// The chip's text color (`ref_color` in console.py): an agent in its
    /// role's color (an unknown handle as an engineer, as the mockup does),
    /// a person bright, branches green, items magenta, lanes yellow, and
    /// files, commits and PRs in the accent color.
    #[must_use]
    pub(crate) fn chip_color(&self, target: &RefTarget) -> Color {
        match target {
            RefTarget::Agent(handle) => {
                Color::Role(self.role(handle).unwrap_or("engineer").to_owned().into())
            }
            RefTarget::Person(_) => Color::TextMuted,
            RefTarget::Role(role) => Color::Role(role.clone().into()),
            RefTarget::Branch { .. } => Color::Success,
            RefTarget::Item(_) => Color::Player(1),
            RefTarget::Lane(_) => Color::Warning,
            RefTarget::Commit(_) | RefTarget::File { .. } | RefTarget::Pr(_) => Color::Accent,
        }
    }

    /// The hover card for `target` (spec 9.3: agent state and spend, branch
    /// and worktree, item claim), from what the snapshot holds.
    #[must_use]
    pub(crate) fn card(&self, target: &RefTarget) -> RefCard {
        let color = self.chip_color(target);
        let (kind, title, rows) = match target {
            RefTarget::Agent(handle) => ("Agent", handle.clone(), self.agent_rows(handle)),
            RefTarget::Person(name) => {
                let group = self.snapshot.roster.iter().find(|g| &g.person == name);
                let title = if group.is_some_and(|g| g.you) || *name == self.snapshot.me {
                    format!("{name} (you)")
                } else {
                    name.clone()
                };
                let agents = group.map_or_else(
                    || "none in this project".to_owned(),
                    |g| join_or(g.agents.iter().map(|a| a.name.clone()), "none"),
                );
                ("Person", title, vec![("Agents", agents)])
            }
            RefTarget::Role(role) => {
                let holders = self
                    .roster_agents()
                    .filter(|a| a.role == *role)
                    .map(|a| a.handle.clone());
                (
                    "Role",
                    role.clone(),
                    vec![("Held by", join_or(holders, "nobody here"))],
                )
            }
            RefTarget::Branch { name, at } => (
                "Branch",
                name.clone(),
                self.branch_rows(name, at.as_deref()),
            ),
            RefTarget::Commit(sha) => ("Commit", sha.clone(), self.commit_rows(sha)),
            RefTarget::File { path, lines } => {
                let mut rows = Vec::new();
                if let Some((from, to)) = lines {
                    let span = if from == to {
                        from.to_string()
                    } else {
                        format!("{from} to {to}")
                    };
                    rows.push(("Lines", span));
                }
                let changed = unique(self.dones().filter_map(|(m, done)| {
                    done.changes
                        .iter()
                        .any(|c| c.path == *path)
                        .then(|| format!("{} · {}", m.from_handle, done.branch))
                }));
                if !changed.is_empty() {
                    rows.push(("Changed in", changed.join(", ")));
                }
                ("File", path.clone(), rows)
            }
            RefTarget::Item(key) => ("Item", key.clone(), self.item_rows(key)),
            RefTarget::Lane(name) => ("Lane", name.clone(), self.lane_rows(name)),
            RefTarget::Pr(number) => (
                "Pull request",
                format!("#{number}"),
                // shortcut: PR cards come from the GitHub App (spec 10), a
                // later plan; until then the card says so.
                vec![("GitHub", "not connected yet".to_owned())],
            ),
        };
        RefCard {
            kind,
            title,
            color,
            rows,
        }
    }

    fn roster_agents(&self) -> impl Iterator<Item = &crate::backend::AgentRow> {
        self.snapshot.roster.iter().flat_map(|g| &g.agents)
    }

    fn agent_info(&self, handle: &str) -> Option<&AgentInfo> {
        self.snapshot.agents.values().find(|a| a.handle == handle)
    }

    /// Every DONE in the chat, oldest first, with its message.
    fn dones(&self) -> impl Iterator<Item = (&StampedEnvelope, DoneFields<'_>)> {
        self.snapshot
            .messages
            .iter()
            .filter_map(|m| match &m.client.payload {
                Payload::Done {
                    item,
                    branch,
                    commit,
                    base_sha,
                    changes,
                    ..
                } => Some((
                    m,
                    DoneFields {
                        item: item.as_str(),
                        branch,
                        commit: commit.as_str(),
                        base: base_sha.as_str(),
                        changes,
                    },
                )),
                _ => None,
            })
    }

    fn agent_rows(&self, handle: &str) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        let group = self
            .snapshot
            .roster
            .iter()
            .find(|g| g.agents.iter().any(|a| a.handle == handle));
        let row = group.and_then(|g| g.agents.iter().find(|a| a.handle == handle));
        match (group, row) {
            (Some(group), Some(agent)) => {
                rows.push(("Role", title_case(&agent.role)));
                rows.push(("Runs on", agent.harness.clone()));
                rows.push(("State", status_text(agent.status).to_owned()));
                let owner = if group.you {
                    format!("{} (you)", group.person)
                } else {
                    group.person.clone()
                };
                rows.push(("Owner", owner));
            }
            _ => rows.push(("State", "not in this project's roster".to_owned())),
        }
        if let Some(info) = self.agent_info(handle) {
            rows.push(("Spent", money_of(info.spent_usd, info.budget_usd)));
            rows.push(("Branch", info.branch.clone()));
        }
        rows
    }

    fn branch_rows(&self, name: &str, at: Option<&str>) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        if let Some(sha) = at {
            rows.push(("At", sha.to_owned()));
        }
        // shortcut: ahead and behind counts need the worktree data Task 12
        // adds; until then the card shows the worktree's own state line.
        match self.snapshot.agents.values().find(|a| a.branch == name) {
            Some(info) => {
                rows.push((
                    "Worktree",
                    format!("{} · {}", info.handle, info.worktree_state),
                ));
                rows.push(("Base", info.base.clone()));
            }
            None => rows.push(("Worktree", "none on this machine".to_owned())),
        }
        if let Some((m, done)) = self.dones().filter(|(_, d)| d.branch == name).last() {
            rows.push(("Last DONE", format!("{} · {}", m.from_handle, done.item)));
        }
        rows
    }

    fn commit_rows(&self, sha: &str) -> Vec<(&'static str, String)> {
        let same = |c: &str| c.starts_with(sha) || sha.starts_with(c);
        let mut rows = Vec::new();
        let named = unique(
            self.dones()
                .filter(|(_, d)| same(d.commit))
                .map(|(m, d)| format!("{} · {}", m.from_handle, d.branch)),
        );
        if !named.is_empty() {
            rows.push(("In DONE", named.join(", ")));
        }
        let based = unique(
            self.dones()
                .filter(|(_, d)| same(d.base))
                .map(|(m, d)| format!("{} · {}", m.from_handle, d.branch)),
        );
        if !based.is_empty() {
            rows.push(("Base of", based.join(", ")));
        }
        if rows.is_empty() {
            rows.push(("Known to", "local git".to_owned()));
        }
        rows
    }

    fn item_rows(&self, key: &str) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        let claimed = self
            .snapshot
            .messages
            .iter()
            .rev()
            .find_map(|m| match &m.client.payload {
                Payload::Claim { item } if item.as_str() == key => Some(m.from_handle.clone()),
                Payload::Delegated {
                    worker,
                    item: Some(item),
                    ..
                } if item.as_str() == key => Some(
                    self.handle(worker)
                        .map_or_else(|| worker.as_str().to_owned(), str::to_owned),
                ),
                _ => None,
            });
        if let Some(by) = claimed {
            rows.push(("Claimed by", by));
        }
        if let Some((m, done)) = self.dones().filter(|(_, d)| d.item == key).last() {
            rows.push((
                "Done",
                format!("{} · {}@{}", m.from_handle, done.branch, done.commit),
            ));
            let review = self.snapshot.messages.iter().rev().find_map(|r| {
                match (&r.client.payload, &r.client.reply_to) {
                    (Payload::Review { verdict, .. }, Some(parent)) if *parent == m.client.id => {
                        Some(format!("{} · {}", verdict_text(*verdict), r.from_handle))
                    }
                    _ => None,
                }
            });
            if let Some(review) = review {
                rows.push(("Review", review));
            }
        }
        if rows.is_empty() {
            rows.push(("Claim", "none in this chat".to_owned()));
        }
        rows
    }

    fn lane_rows(&self, name: &str) -> Vec<(&'static str, String)> {
        let mut rows = Vec::new();
        if let Some(lane) = self
            .snapshot
            .plan
            .as_ref()
            .and_then(|p| p.lanes.iter().find(|l| l.name == name))
        {
            rows.push(("Paths", lane.paths.join(", ")));
        }
        let editors = unique(
            self.snapshot
                .agents
                .values()
                .filter(|a| a.edits.split_whitespace().any(|w| w == name))
                .map(|a| a.handle.clone()),
        );
        if !editors.is_empty() {
            rows.push(("Edited by", editors.join(", ")));
        }
        if rows.is_empty() {
            rows.push(("Lane", "no details in this project yet".to_owned()));
        }
        rows
    }
}

/// The DONE fields a card reads.
struct DoneFields<'a> {
    item: &'a str,
    branch: &'a str,
    commit: &'a str,
    base: &'a str,
    changes: &'a [agmen_proto::Change],
}

const fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Idle => "idle",
        AgentStatus::Reading => "reading",
    }
}

const fn verdict_text(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "pass",
        Verdict::Changes => "changes",
    }
}

/// Each value once, in order.
fn unique(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = Vec::new();
    for value in values {
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    seen
}

fn join_or(values: impl Iterator<Item = String>, empty: &str) -> String {
    let joined = values.collect::<Vec<_>>().join(", ");
    if joined.is_empty() {
        empty.to_owned()
    } else {
        joined
    }
}

/// Whether the chip has a tinted fill (agents, people, roles, branches,
/// items, lanes) or is plain underlined text (files, commits, PRs), as
/// `rich_body` draws them.
#[must_use]
pub(crate) const fn filled(target: &RefTarget) -> bool {
    !matches!(
        target,
        RefTarget::Commit(_) | RefTarget::File { .. } | RefTarget::Pr(_)
    )
}

/// What a reference's hover card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefCard {
    /// "Agent", "Branch", ...
    pub(crate) kind: &'static str,
    pub(crate) title: String,
    pub(crate) color: Color,
    /// Label and value rows.
    pub(crate) rows: Vec<(&'static str, String)>,
}

/// The hover card's view: GPUI tooltips are views.
struct RefCardView {
    card: RefCard,
}

impl Render for RefCardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let card = &self.card;
        div()
            .debug_selector(|| "ref-card".into())
            .w(REF_CARD_WIDTH)
            .flex()
            .flex_col()
            .gap(SPACE_6)
            .py(SPACE_10)
            .px(SPACE_12)
            .rounded(RADIUS_LG)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.elevated_surface)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(SPACE_6)
                    .child(
                        Label::new(card.kind)
                            .size(LabelSize::Small)
                            .color(Color::Placeholder),
                    )
                    .child(
                        Label::new(card.title.clone())
                            .code()
                            .color(card.color.clone()),
                    ),
            )
            .children(
                card.rows
                    .iter()
                    .map(|(label, value)| KvRow::new(*label, value.clone())),
            )
    }
}

/// One reference in message text: a chip in the reference's color.
#[derive(IntoElement)]
pub(crate) struct RefChip {
    id: ElementId,
    text: SharedString,
    target: RefTarget,
    color: Color,
    flush_start: bool,
    flush_end: bool,
    context: Option<Rc<ChatContext>>,
    on_ref: Option<ChangeHandler<RefEvent>>,
}

impl std::fmt::Debug for RefChip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefChip")
            .field("id", &self.id)
            .field("text", &self.text)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl RefChip {
    /// `text` as the message writes it; `color` from
    /// [`ChatContext::chip_color`]. Tests find the chip by `id`.
    #[must_use]
    pub(crate) fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        target: RefTarget,
        color: Color,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            target,
            color,
            flush_start: false,
            flush_end: false,
            context: None,
            on_ref: None,
        }
    }

    /// No padding before the text, because punctuation touches it there
    /// (mockup fix 54).
    #[must_use]
    pub(crate) const fn flush_start(mut self) -> Self {
        self.flush_start = true;
        self
    }

    /// No padding after the text, because punctuation follows it (mockup
    /// fix 54).
    #[must_use]
    pub(crate) const fn flush_end(mut self) -> Self {
        self.flush_end = true;
        self
    }

    /// Shows the reference's card on hover, built from `context` when the
    /// pointer arrives.
    #[must_use]
    pub(crate) fn card(mut self, context: Rc<ChatContext>) -> Self {
        self.context = Some(context);
        self
    }

    /// Called with [`RefEvent::OpenRef`] on a click and
    /// [`RefEvent::OpenRefInSplit`] on a ⌘-click (Ctrl elsewhere).
    #[must_use]
    pub(crate) fn on_ref(
        mut self,
        handler: impl Fn(RefEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> Self {
        self.on_ref = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for RefChip {
    fn render(self, _window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let is_filled = filled(&self.target);
        let fill = tint(self.color.hsla(theme), TINT_FILL);
        let selector = self.id.to_string();
        let label = Label::new(self.text).code().color(self.color);
        let target = self.target;
        let hover_target = target.clone();
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .when(!self.flush_start, |this| this.pl(SPACE_4))
            .when(!self.flush_end, |this| this.pr(SPACE_4))
            .rounded(RADIUS_REF)
            .when(is_filled, |this| this.bg(fill))
            .when_some(self.on_ref, |this, handler| {
                this.cursor_pointer().on_click(move |event, window, cx| {
                    let target = target.clone();
                    let event = if event.modifiers().secondary(cx) {
                        RefEvent::OpenRefInSplit(target)
                    } else {
                        RefEvent::OpenRef(target)
                    };
                    handler(event, window, cx);
                })
            })
            .when_some(self.context, |this, context| {
                this.hoverable_tooltip(move |_window, cx| {
                    let card = context.card(&hover_target);
                    cx.new(|_| RefCardView { card }).into()
                })
            })
            .child(if is_filled { label } else { label.underline() })
    }
}

#[cfg(test)]
#[path = "ref_chip_tests.rs"]
mod tests;
