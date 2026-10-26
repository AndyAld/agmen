//! What the chat's rows, bodies and reference chips look up, built once per
//! snapshot, and the hover card data for each kind of reference (spec 9.3).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use agmen_proto::{
    IdentityId, MessageId, Payload, Ref, Seq, StampedEnvelope, Stream, Timestamp, Verdict,
};
use gpui::SharedString;
use jiff::tz::TimeZone;

use super::components::Color;
use super::format::{clock, money_of, title_case};
use crate::backend::{AgentInfo, AgentStatus, ChangeCard, ModelChoice, Snapshot};
use crate::refs::{Known, RefTarget, Span, find_refs};

/// How many entries a hover card row lists before it says "and N more".
const MAX_CARD_ENTRIES: usize = 4;

/// A message body and the references in it, found once per snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyText {
    pub(crate) body: SharedString,
    /// Sorted and not overlapping, as `find_refs` returns them.
    pub(crate) spans: Vec<Span>,
}

/// What chips, message bodies and message rows look up, built once per
/// snapshot so a frame does no roster scans per chip and no reference scan
/// per body.
#[derive(Debug)]
pub(crate) struct ChatContext {
    snapshot: Rc<Snapshot>,
    known: Known,
    /// Each roster agent's lowercase role, by handle, then each plan team
    /// member's.
    roles: BTreeMap<String, String>,
    /// Handles of the agents this person's agmend runs.
    local: BTreeSet<String>,
    /// Ids of the messages in the snapshot, so a reply can tell whether its
    /// parent is in the chat.
    ids: BTreeSet<MessageId>,
    /// Hub position and client id of each message in the snapshot. The body
    /// cache is keyed by both: a client id alone is chosen by its sender and
    /// could be reused, and a position alone could be claimed by a message
    /// that is not in the snapshot.
    positions: BTreeSet<(Stream, Seq, MessageId)>,
    /// Hub position and client id of the message each change card belongs
    /// to: the first message by hub position from the listener with the
    /// card's id. Another sender may reuse that id (the hub keeps ids unique
    /// per sender only), and its message gets no card.
    card_rows: BTreeSet<(Stream, Seq, MessageId)>,
    /// Each drawn message's body and references, found on its first draw.
    /// Only messages in the snapshot are kept, so it holds at most one entry
    /// per message.
    bodies: RefCell<BTreeMap<(Stream, Seq, MessageId), Rc<BodyText>>>,
    zone: TimeZone,
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
        // A plan's team has its roles before its agents join the roster.
        for row in snapshot.plan.iter().flat_map(|plan| &plan.team) {
            roles
                .entry(format!("{}/{}", row.owner, row.handle))
                .or_insert_with(|| row.role.clone());
        }
        let ids = snapshot
            .messages
            .iter()
            .map(|m| m.client.id.clone())
            .collect();
        let positions = snapshot
            .messages
            .iter()
            .map(|m| (m.stream.clone(), m.seq, m.client.id.clone()))
            .collect();
        let card_rows = snapshot
            .changes
            .keys()
            .filter_map(|id| {
                snapshot
                    .messages
                    .iter()
                    .filter(|m| &m.client.id == id && m.from == snapshot.listener_id)
                    .map(|m| (m.stream.clone(), m.seq, m.client.id.clone()))
                    .min()
            })
            .collect();
        Self {
            snapshot,
            known,
            roles,
            local,
            ids,
            positions,
            card_rows,
            bodies: RefCell::default(),
            zone: TimeZone::UTC,
        }
    }

    /// Shows times in `zone`.
    #[must_use]
    pub(crate) fn with_zone(mut self, zone: TimeZone) -> Self {
        self.zone = zone;
        self
    }

    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// A body and its references: the hub's, then the git names local git
    /// confirms. Not cached; rows use [`ChatContext::body_of`].
    #[must_use]
    pub(crate) fn body_text(&self, body: &str, hub_refs: &[Ref]) -> BodyText {
        BodyText {
            body: body.to_owned().into(),
            spans: find_refs(body, hub_refs, &self.known),
        }
    }

    /// `env`'s body and references, found once per snapshot.
    #[must_use]
    pub(crate) fn body_of(&self, env: &StampedEnvelope) -> Rc<BodyText> {
        let key = (env.stream.clone(), env.seq, env.client.id.clone());
        if let Some(found) = self.bodies.borrow().get(&key) {
            return found.clone();
        }
        let found = Rc::new(self.body_text(env.client.body.as_str(), &env.refs));
        if self.positions.contains(&key) {
            self.bodies.borrow_mut().insert(key, found.clone());
        }
        found
    }

    /// The change card `env` carries, if it is the listener message the
    /// card belongs to. The sender is checked too: `card_rows` holds only
    /// listener messages, but a row built elsewhere could carry a listener
    /// message's position and id under another sender.
    #[must_use]
    pub(crate) fn change_card(&self, env: &StampedEnvelope) -> Option<&ChangeCard> {
        let key = (env.stream.clone(), env.seq, env.client.id.clone());
        if env.from == self.snapshot.listener_id && self.card_rows.contains(&key) {
            self.snapshot.changes.get(&env.client.id)
        } else {
            None
        }
    }

    /// Task 15: the model `env` was sent with or answered on, by its hub
    /// position.
    pub(crate) fn model_of(&self, env: &StampedEnvelope) -> Option<&ModelChoice> {
        self.snapshot
            .message_models
            .get(&(env.stream.clone(), env.seq))
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

    /// `at` as `15:27` in the context's zone.
    #[must_use]
    pub(crate) fn clock(&self, at: Timestamp) -> String {
        clock(at, &self.zone)
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
                let changed = listing(self.dones().filter_map(|(m, done)| {
                    done.changes
                        .iter()
                        .any(|c| c.path == *path)
                        .then(|| format!("{} · {}", m.from_handle, done.branch))
                }));
                if let Some(changed) = changed {
                    rows.push(("Changed in", changed));
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
        let named = listing(
            self.dones()
                .filter(|(_, d)| same(d.commit))
                .map(|(m, d)| format!("{} · {}", m.from_handle, d.branch)),
        );
        if let Some(named) = named {
            rows.push(("In DONE", named));
        }
        let based = listing(
            self.dones()
                .filter(|(_, d)| same(d.base))
                .map(|(m, d)| format!("{} · {}", m.from_handle, d.branch)),
        );
        if let Some(based) = based {
            rows.push(("Base of", based));
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
        let editors = listing(
            self.snapshot
                .agents
                .values()
                .filter(|a| a.edits.split_whitespace().any(|w| w == name))
                .map(|a| a.handle.clone()),
        );
        if let Some(editors) = editors {
            rows.push(("Edited by", editors));
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

/// Each value once, in first-seen order, as a comma list of the last
/// [`MAX_CARD_ENTRIES`] with "and N more" for the rest; `None` when there
/// are none.
fn listing(values: impl Iterator<Item = String>) -> Option<String> {
    let mut seen = BTreeSet::new();
    let unique: Vec<String> = values.filter(|v| seen.insert(v.clone())).collect();
    let hidden = unique.len().saturating_sub(MAX_CARD_ENTRIES);
    let shown = unique.get(hidden..).unwrap_or_default().join(", ");
    match hidden {
        _ if shown.is_empty() => None,
        0 => Some(shown),
        n => Some(format!("{shown} and {n} more")),
    }
}

fn join_or(values: impl Iterator<Item = String>, empty: &str) -> String {
    let joined = values.collect::<Vec<_>>().join(", ");
    if joined.is_empty() {
        empty.to_owned()
    } else {
        joined
    }
}

/// What a reference's hover card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RefCard {
    /// "Agent", "Branch", ...
    pub(crate) kind: &'static str,
    pub(crate) title: String,
    pub(crate) color: Color,
    /// Label and value rows.
    pub(crate) rows: Vec<(&'static str, String)>,
}

#[cfg(test)]
#[path = "chat_context_tests.rs"]
mod tests;
