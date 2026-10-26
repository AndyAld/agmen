//! The planning room's kick-off checklist and when Kick off is enabled
//! (spec 7, Task 19 of the desktop plan).
//!
//! Draft specs (those with a `draft_note`) are not up for approval yet, so
//! they neither block kick-off nor name anyone as waiting.

use std::collections::BTreeSet;

use crate::backend::{Approval, PlanInfo, SpecState};

/// Lane paths deeper than this are treated as covering everything below
/// this depth, which can only report more overlaps, never fewer.
const MAX_SEGMENTS: usize = 64;

/// One line of the plan panel's "Before kick-off" list.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Check {
    pub(crate) label: String,
    pub(crate) ok: bool,
}

/// One owner's part of a plan's team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct Share {
    pub(crate) agents: usize,
    pub(crate) usd_per_day: u32,
}

/// The agents `owner` runs for `plan` and what they cost per day.
#[must_use]
pub(crate) fn share(plan: &PlanInfo, owner: &str) -> Share {
    plan.team
        .iter()
        .filter(|row| row.owner == owner)
        .fold(Share::default(), |acc, row| Share {
            agents: acc.agents + 1,
            usd_per_day: acc.usd_per_day.saturating_add(row.usd_per_day),
        })
}

/// The kick-off checklist: the repo exists, lanes do not overlap, every spec
/// is approved by all, and each owner approved their own agents and spend.
/// `me`'s share comes first, then the other owners in team order.
#[must_use]
pub(crate) fn checklist(plan: &PlanInfo, me: &str) -> Vec<Check> {
    let mut checks = vec![
        Check {
            label: format!("Repo {} exists on the git host", plan.name),
            ok: plan.repo_exists,
        },
        match lane_problem(plan) {
            Some(LaneProblem::DotDot { lane, path }) => Check {
                label: format!("Lane {lane} path {path} uses .."),
                ok: false,
            },
            Some(LaneProblem::Overlap(a, b)) => Check {
                label: format!("Lanes {a} and {b} overlap"),
                ok: false,
            },
            None => Check {
                label: "Lanes don't overlap".into(),
                ok: true,
            },
        },
    ];
    let approvers: BTreeSet<&str> = up_for_approval(plan)
        .flat_map(|spec| spec.approvals.keys().map(String::as_str))
        .collect();
    let approvers: Vec<&str> = approvers.into_iter().collect();
    checks.push(Check {
        label: if has_spec(plan) {
            format!("Specs approved by {}", join_names(&approvers))
        } else {
            "At least one spec approved".into()
        },
        ok: has_spec(plan) && up_for_approval(plan).all(SpecState::approved_by_all),
    });
    let mut owners = owners(plan);
    owners.sort_by_key(|owner| *owner != me);
    for owner in owners {
        let Share {
            agents,
            usd_per_day,
        } = share(plan, owner);
        checks.push(Check {
            label: format!(
                "{owner} approves {}, ${usd_per_day}/day",
                agent_count(agents)
            ),
            ok: share_approved(plan, owner),
        });
    }
    checks
}

/// Whether Kick off is enabled: the team has at least one agent, every
/// check is ok (which needs at least one approved spec), the plan branch is
/// merged, and the plan is not already running.
#[must_use]
pub(crate) fn kickoff_enabled(plan: &PlanInfo) -> bool {
    !plan.running
        && plan.branch_merged
        && !plan.team.is_empty()
        && checklist(plan, "").iter().all(|c| c.ok)
}

/// The Kick off button's text, which says what it waits for while disabled.
#[must_use]
pub(crate) fn kickoff_label(plan: &PlanInfo) -> String {
    let reason = if plan.running {
        "running".into()
    } else if kickoff_enabled(plan) {
        agent_count(plan.team.len())
    } else if let waiting = waiting_on(plan)
        && !waiting.is_empty()
    {
        format!("waiting on {}", join_names(&waiting))
    } else if !plan.repo_exists {
        "no repo".into()
    } else if plan.team.is_empty() {
        "no agents".into()
    } else if !has_spec(plan) {
        "no approved spec".into()
    } else if let Some(problem) = lane_problem(plan) {
        match problem {
            LaneProblem::DotDot { .. } => "a lane path uses ..".into(),
            LaneProblem::Overlap(..) => "lanes overlap".into(),
        }
    } else {
        // Every check passed and nobody waits, so only the merge is left.
        "waiting on the plan branch merge".into()
    };
    format!("Kick off · {reason}")
}

/// Records `person`'s approval of the spec at `path`. Returns whether
/// anything changed: not for an unknown or draft spec, a person who is not
/// an approver, or an approval already given.
#[must_use]
pub(crate) fn approve_spec(plan: &mut PlanInfo, path: &str, person: &str) -> bool {
    let Some(approval) = plan
        .specs
        .iter_mut()
        .find(|spec| spec.path == path && spec.draft_note.is_none())
        .and_then(|spec| spec.approvals.get_mut(person))
    else {
        return false;
    };
    let changed = *approval == Approval::Waiting;
    *approval = Approval::Approved;
    changed
}

fn up_for_approval(plan: &PlanInfo) -> impl Iterator<Item = &SpecState> {
    plan.specs.iter().filter(|spec| spec.draft_note.is_none())
}

fn has_spec(plan: &PlanInfo) -> bool {
    up_for_approval(plan).next().is_some()
}

/// The plan's owners, each once, in team order.
pub(crate) fn owners(plan: &PlanInfo) -> Vec<&str> {
    let mut owners: Vec<&str> = Vec::new();
    for row in &plan.team {
        if !owners.contains(&row.owner.as_str()) {
            owners.push(&row.owner);
        }
    }
    owners
}

fn share_approved(plan: &PlanInfo, owner: &str) -> bool {
    plan.shares_approved.get(owner).copied().unwrap_or(false)
}

/// Everyone who still has to approve a spec or their own share, by name.
fn waiting_on(plan: &PlanInfo) -> Vec<&str> {
    let specs = up_for_approval(plan).flat_map(|spec| {
        spec.approvals
            .iter()
            .filter(|(_, approval)| **approval == Approval::Waiting)
            .map(|(person, _)| person.as_str())
    });
    let shares = owners(plan)
        .into_iter()
        .filter(|owner| !share_approved(plan, owner));
    let names: BTreeSet<&str> = specs.chain(shares).collect();
    names.into_iter().collect()
}

/// "7 agents", "1 agent".
pub(crate) fn agent_count(n: usize) -> String {
    if n == 1 {
        "1 agent".into()
    } else {
        format!("{n} agents")
    }
}

/// "andy", "andy and sean", "adam, andy and sean".
fn join_names<S: AsRef<str>>(names: &[S]) -> String {
    match names {
        [] => String::new(),
        [one] => one.as_ref().to_owned(),
        [rest @ .., last] => {
            let rest: Vec<&str> = rest.iter().map(AsRef::as_ref).collect();
            format!("{} and {}", rest.join(", "), last.as_ref())
        }
    }
}

/// What is wrong with a plan's lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneProblem<'a> {
    /// A path that climbs out with `..`, which no lane may use.
    DotDot { lane: &'a str, path: &'a str },
    /// Two lanes, in plan order, with paths that overlap.
    Overlap(&'a str, &'a str),
}

/// The first problem with the plan's lanes: a `..` path first, then the
/// first two lanes that overlap.
fn lane_problem(plan: &PlanInfo) -> Option<LaneProblem<'_>> {
    let dot_dot = plan.lanes.iter().find_map(|lane| {
        lane.paths
            .iter()
            .find(|path| path.split('/').any(|s| s == ".."))
            .map(|path| LaneProblem::DotDot {
                lane: &lane.name,
                path,
            })
    });
    dot_dot.or_else(|| overlapping_lanes(plan).map(|(a, b)| LaneProblem::Overlap(a, b)))
}

/// The first two lanes, in plan order, with paths that overlap.
fn overlapping_lanes(plan: &PlanInfo) -> Option<(&str, &str)> {
    plan.lanes.iter().enumerate().find_map(|(i, a)| {
        plan.lanes.iter().skip(i + 1).find_map(|b| {
            let overlap = a
                .paths
                .iter()
                .any(|pa| b.paths.iter().any(|pb| paths_overlap(pa, pb)));
            overlap.then_some((a.name.as_str(), b.name.as_str()))
        })
    })
}

/// A lane path: its literal segments, and whether it covers everything
/// below them (`core/**`, `core/`).
struct LanePath<'a> {
    segments: Vec<&'a str>,
    open: bool,
}

impl<'a> LanePath<'a> {
    fn parse(path: &'a str) -> Self {
        let mut open = path.ends_with('/');
        let mut segments: Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .take(MAX_SEGMENTS + 1)
            .collect();
        if segments.len() > MAX_SEGMENTS {
            segments.truncate(MAX_SEGMENTS);
            open = true;
        }
        // `**` anywhere covers everything below it; treating `a/**/b` as
        // `a/**` only reports more overlaps.
        if let Some(at) = segments.iter().position(|s| *s == "**") {
            segments.truncate(at);
            open = true;
        }
        // Lanes reject `..` (lane_problem); here it matches everything, the
        // safe direction for a kick-off gate.
        if segments.contains(&"..") {
            segments.clear();
            open = true;
        }
        Self { segments, open }
    }
}

/// Whether two glob-like lane paths can match the same file. A segment with
/// `*` in it matches any segment, so the answer errs toward overlap.
fn paths_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (LanePath::parse(a), LanePath::parse(b));
    let same_prefix = a
        .segments
        .iter()
        .zip(&b.segments)
        .all(|(x, y)| x == y || x.contains('*') || y.contains('*'));
    let shorter = if a.segments.len() <= b.segments.len() {
        &a
    } else {
        &b
    };
    same_prefix && (a.segments.len() == b.segments.len() || shorter.open)
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
