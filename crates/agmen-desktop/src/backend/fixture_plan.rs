//! The planning room's data beyond the checklist's inputs (Task 19): the
//! design spec's text and comments, when people approved, the owners' caps
//! and who is at work (console.py `MD`, `build_d_spec` and the kick-off
//! dialog in `build_d_plan`).

use std::collections::BTreeMap;

use agmen_proto::{GitSha, Timestamp};

use super::{Action, AgentStatus, BackendError, PlanInfo, Snapshot, SpecComment, SpecReview};

/// The design spec (console.py `MD`).
const RIDGELINE_DESIGN: &str = "\
# Ridgeline design
Status: v3 · drafted by sean/plan · approved by andy

## 1. Intent
Offline trail maps for iOS and Android. A downloaded region renders
with no network, and a hike records to a GPX track.

## 2. Goals
- Render a downloaded region offline in under 1 s on a 2023 phone.
- One Rust core for tiles, routing and sync; thin native UIs.
- Sync tracks between devices when online.

## 3. Non-goals
- Turn-by-turn navigation in v1.
- A web app.

## 4. Lanes
| Lane    | Paths    | Agent     | Runs on |
| L-CORE  | core/**  | andy/core | andy    |
| L-APP   | app/**   | andy/app  | andy    |
| L-SYNC  | sync/**  | sean/sync | sean    |
| L-TESTS | tests/** | sean/qa   | sean    |

## 5. Open questions
- Tile source and license: tile-sources.md, sean/tiles.
";

/// The design spec's path, as the plan lists it.
pub(super) const DESIGN_PATH: &str = "docs/specs/ridgeline-design.md";

/// A time on the mockup's day in Mountain time (UTC-6), as the chat's.
fn at(mountain: &str) -> Result<Timestamp, BackendError> {
    let bad = || BackendError::BadFixture(format!("time {mountain}"));
    let (h, m) = mountain.split_once(':').ok_or_else(bad)?;
    let h: u32 = h.parse().map_err(|_| bad())?;
    format!("2026-10-25T{:02}:{m}:00Z", (h + 6) % 24)
        .parse()
        .map_err(|_| bad())
}

/// The design spec's review at `sha`: andy approved at 15:47 (his FYI), sean at
/// 15:52 (his DECISION) once `sean_approved`.
pub(super) fn reviews(sean_approved: bool, sha: GitSha) -> Result<Vec<SpecReview>, BackendError> {
    let mut approved_at = BTreeMap::from([("andy".to_owned(), at("15:47")?)]);
    if sean_approved {
        approved_at.insert("sean".into(), at("15:52")?);
    }
    let comment = |author: &str, body: &str| SpecComment {
        author: author.into(),
        body: body.into(),
    };
    Ok(vec![SpecReview {
        path: DESIGN_PATH.into(),
        sha,
        text: RIDGELINE_DESIGN.into(),
        approved_at,
        comments: vec![
            comment(
                "sean/plan",
                "§2: '1 s' is a target until R-1 measures tile decode on a real phone.",
            ),
            comment("andy", "Fine, keep it as the target."),
            comment(
                "sean/plan",
                "§5: tiles decides self-hosting; the plan stays valid either way.",
            ),
        ],
    }])
}

/// The owners' daily caps in the kick-off dialog.
pub(super) fn caps_usd() -> BTreeMap<String, u32> {
    BTreeMap::from([("andy".into(), 80), ("sean".into(), 60)])
}

/// sean approved his share with the design spec at 15:52.
pub(super) fn shares_approved_at(
    sean_approved: bool,
) -> Result<BTreeMap<String, Timestamp>, BackendError> {
    let mut times = BTreeMap::new();
    if sean_approved {
        times.insert("sean".into(), at("15:52")?);
    }
    Ok(times)
}

/// The planning room's people: andy idle, sean at work (frame 8656).
pub(super) fn presence() -> BTreeMap<String, AgentStatus> {
    BTreeMap::from([
        ("andy".into(), AgentStatus::Idle),
        ("sean".into(), AgentStatus::Working),
    ])
}

/// Gives each team member the identity the hub makes for it at kick-off,
/// named `{owner}/{handle}` as the running roster names it.
///
/// # Errors
///
/// When a handle does not make a valid id.
pub(super) fn identify_team(plan: &mut PlanInfo) -> Result<(), BackendError> {
    for row in &mut plan.team {
        row.id = Some(super::fixture_data::id(&format!(
            "{}/{}",
            row.owner, row.handle
        ))?);
    }
    Ok(())
}

/// Does a planning room action on the fixture's plan, as the person:
/// approve a spec at the commit they read, approve their own share, or
/// kick off a plan whose checks all pass.
///
/// # Errors
///
/// When there is no plan, the spec is unknown, a draft or at another
/// commit, or the plan is not ready to kick off.
pub(super) fn act(snapshot: &mut Snapshot, action: Action) -> Result<(), BackendError> {
    let me = snapshot.me.clone();
    let plan = snapshot
        .plan
        .as_mut()
        .ok_or(BackendError::Unsupported("a plan action without a plan"))?;
    match action {
        Action::ApproveSpec { path, sha } => {
            let spec = plan
                .specs
                .iter()
                .find(|spec| spec.path == path && spec.draft_note.is_none())
                .ok_or(BackendError::Unsupported(
                    "approving a spec that is not up for approval",
                ))?;
            // Spec 7: an approval names the exact commit the approver read.
            if spec.sha != sha {
                return Err(BackendError::Unsupported(
                    "approving a spec at an older commit",
                ));
            }
            if crate::plan::approve_spec(plan, &path, &me) {
                let now = Timestamp::now();
                if let Some(review) = plan.reviews.iter_mut().find(|r| r.path == path) {
                    review.approved_at.insert(me, now);
                }
            }
            Ok(())
        }
        Action::ApproveShare => {
            if plan.shares_approved.insert(me.clone(), true) != Some(true) {
                plan.shares_approved_at.insert(me, Timestamp::now());
            }
            Ok(())
        }
        Action::KickOff => {
            if !crate::plan::kickoff_enabled(plan) {
                return Err(BackendError::Unsupported(
                    "kicking off a plan that is not ready",
                ));
            }
            plan.running = true;
            identify_team(plan)
        }
        _ => Err(BackendError::Unsupported(
            "that action in the planning room",
        )),
    }
}
