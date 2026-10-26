//! References in message text: what they name and where they open (spec 9.3).
//!
//! Two sources, as the spec splits them. The hub stamps `refs` for the
//! identities, items, lanes, roles and PRs a body names. The client finds
//! branches, commits and files itself, and keeps a match only when local git
//! (or a DONE or CLAIM in the chat) knows it. Text that only looks like a
//! reference stays plain.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::LazyLock;

use agmen_proto::{IdentityId, Payload, Ref, RefKind};
use regex::Regex;

use crate::backend::Snapshot;

/// What a reference names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum RefTarget {
    /// An agent's handle, such as `andy/fx1`.
    Agent(String),
    /// A person's name, such as `sean`.
    Person(String),
    Role(String),
    /// A branch, and the commit after `@` when the text names one.
    Branch {
        name: String,
        at: Option<String>,
    },
    Commit(String),
    /// A file's path in the worktree, and the lines `:2-8` name.
    File {
        path: String,
        lines: Option<(u32, u32)>,
    },
    Item(String),
    Lane(String),
    Pr(u32),
}

/// One reference: its byte range in the body and what it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) range: Range<usize>,
    pub(crate) target: RefTarget,
}

/// What the console knows, to resolve hub refs and confirm git references.
#[derive(Debug, Clone, Default)]
pub(crate) struct Known {
    handles: BTreeMap<IdentityId, String>,
    commits: BTreeSet<String>,
    branches: BTreeSet<String>,
    files: BTreeSet<String>,
}

impl Known {
    pub(crate) fn from_snapshot(snapshot: &Snapshot) -> Self {
        let mut known = Self {
            commits: snapshot.git.commits.clone(),
            branches: snapshot.git.branches.clone(),
            files: snapshot.git.files.clone(),
            ..Self::default()
        };
        known
            .handles
            .insert(snapshot.me_id.clone(), snapshot.me.clone());
        for group in &snapshot.roster {
            // People count whether or not they have posted (review C131).
            known.handles.insert(group.id.clone(), group.person.clone());
            for agent in &group.agents {
                known.handles.insert(agent.id.clone(), agent.handle.clone());
            }
        }
        // A running plan's team, whether or not its agents have posted.
        let team = snapshot.plan.iter().flat_map(|plan| &plan.team);
        for row in team {
            if let Some(id) = &row.id {
                let handle = format!("{}/{}", row.owner, row.handle);
                known.handles.insert(id.clone(), handle);
            }
        }
        for m in &snapshot.messages {
            known.handles.insert(m.from.clone(), m.from_handle.clone());
            // Branches and commits a DONE names count as known, as the spec says.
            if let Payload::Done {
                branch,
                commit,
                base_sha,
                ..
            } = &m.client.payload
            {
                known.branches.insert(branch.clone());
                known.commits.insert(commit.as_str().to_owned());
                known.commits.insert(base_sha.as_str().to_owned());
            }
        }
        known
    }

    /// The handle of the identity `id`, when the roster or the chat names it.
    pub(crate) fn handle(&self, id: &IdentityId) -> Option<&str> {
        self.handles.get(id).map(String::as_str)
    }

    /// The known file `token` names: the exact path, or the one path whose
    /// last parts it is (`release.yml` for `.github/workflows/release.yml`).
    fn file(&self, token: &str) -> Option<&String> {
        if let Some(exact) = self.files.get(token) {
            return Some(exact);
        }
        let suffix = format!("/{token}");
        let mut matches = self.files.iter().filter(|f| f.ends_with(&suffix));
        let first = matches.next()?;
        // An ambiguous short name stays plain rather than open the wrong file.
        matches.next().is_none().then_some(first)
    }

    fn commit_known(&self, sha: &str) -> bool {
        self.commits
            .iter()
            .any(|c| c.starts_with(sha) || sha.starts_with(c.as_str()))
    }
}

/// Files, branches and commits, in that order of precedence at one start:
/// `src/main.rs` is a file, not a branch, and a branch's `@sha` belongs to
/// the branch, not to a separate commit.
// A branch name ends on a letter or digit, so "fix/x." keeps its period
// as text (review C129).
static GIT_REF: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?P<file>[\w./-]+\.(?:rs|ts|tsx|js|md|yml|yaml|toml|lock|xml|json|py|go|sh)(?::(?P<from>\d+)(?:-(?P<to>\d+))?)?)|(?P<branch>\b[a-z][a-z0-9-]*/[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?(?:@[0-9a-f]{7,40})?)|(?P<commit>\b[0-9a-f]{7,40}\b)",
    )
    // A pattern that failed to compile would leave git names plain rather
    // than panic; refs_find_every_kind_in_chat_fixture proves it compiles.
    .ok()
});

/// The references in `body`: the hub's first, then git references the
/// client confirms. Sorted, and never overlapping.
pub(crate) fn find_refs(body: &str, hub_refs: &[Ref], known: &Known) -> Vec<Span> {
    let mut spans: Vec<Span> = hub_refs
        .iter()
        .filter_map(|r| hub_span(body, r, known))
        .collect();
    spans.sort_by_key(|s| s.range.start);
    spans.dedup_by(|later, earlier| later.range.start < earlier.range.end);
    let taken = spans.clone();
    let overlaps = |range: &Range<usize>| {
        taken
            .iter()
            .any(|s| s.range.start < range.end && range.start < s.range.end)
    };
    if let Some(re) = GIT_REF.as_ref() {
        for caps in re.captures_iter(body) {
            let Some(whole) = caps.get(0) else { continue };
            let range = whole.range();
            if overlaps(&range) {
                continue;
            }
            if let Some(target) = git_target(&caps, body, &range, known) {
                // A branch chip without its sha covers the name only.
                let range = match &target {
                    RefTarget::Branch { name, at: None } => range.start..range.start + name.len(),
                    _ => range,
                };
                spans.push(Span { range, target });
            }
        }
    }
    spans.sort_by_key(|s| s.range.start);
    spans
}

fn hub_span(body: &str, r: &Ref, known: &Known) -> Option<Span> {
    let (start, end) = (usize::try_from(r.start).ok()?, usize::try_from(r.end).ok()?);
    // An offset past the end or inside a character is the hub's mistake;
    // drop the ref rather than cut a word or panic.
    if start >= end
        || end > body.len()
        || !body.is_char_boundary(start)
        || !body.is_char_boundary(end)
    {
        return None;
    }
    let target = match r.kind {
        RefKind::Identity => {
            let id: IdentityId = r.target.parse().ok()?;
            let handle = known.handles.get(&id)?.clone();
            if handle.contains('/') {
                RefTarget::Agent(handle)
            } else {
                RefTarget::Person(handle)
            }
        }
        RefKind::Item => RefTarget::Item(r.target.clone()),
        RefKind::Lane => RefTarget::Lane(r.target.clone()),
        RefKind::Role => RefTarget::Role(r.target.clone()),
        RefKind::Pr => RefTarget::Pr(r.target.trim_start_matches('#').parse().ok()?),
    };
    Some(Span {
        range: start..end,
        target,
    })
}

fn git_target(
    caps: &regex::Captures<'_>,
    body: &str,
    range: &Range<usize>,
    known: &Known,
) -> Option<RefTarget> {
    let text = &body[range.clone()];
    if caps.name("branch").is_some() {
        let (name, at) = match text.split_once('@') {
            Some((name, sha)) => (name, Some(sha)),
            None => (text, None),
        };
        if !known.branches.contains(name) {
            return None;
        }
        // Only a sha git knows joins the chip (review C136); the caller
        // trims the range to the name otherwise.
        let at = at.filter(|sha| known.commit_known(sha)).map(str::to_owned);
        return Some(RefTarget::Branch {
            name: name.to_owned(),
            at,
        });
    }
    if caps.name("file").is_some() {
        // A path glued to a word or a slash before it is not a path start.
        let before = body[..range.start].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '/' || c == '_') {
            return None;
        }
        let path_text = text.split(':').next().unwrap_or(text);
        let path = known.file(path_text)?.clone();
        let num = |name: &str| caps.name(name).and_then(|m| m.as_str().parse::<u32>().ok());
        let lines = num("from").map(|from| {
            let to = num("to").unwrap_or(from);
            (from.min(to), from.max(to))
        });
        return Some(RefTarget::File { path, lines });
    }
    if caps.name("commit").is_some() && known.commit_known(text) {
        return Some(RefTarget::Commit(text.to_owned()));
    }
    None
}

#[cfg(test)]
#[path = "refs_tests.rs"]
mod tests;
