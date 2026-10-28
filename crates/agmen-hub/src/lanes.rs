//! Lanes (design 6.5): a project's named sets of gitignore-style globs, the
//! check that no two lanes' globs match one path, `PUT` and `GET
//! /v1/projects/{p}/lanes`, `POST /v1/projects/{p}/check-path`, and the
//! DONE check that flags changed paths outside the sender's claimed lanes.
//!
//! A lane glob follows gitignore: `*` and `?` never match `/`, `**` matches
//! any number of directories, a glob with no `/` but a trailing one matches
//! at any depth (`*.rs` is `**/*.rs`), a leading `/` anchors it at the
//! repository root, a trailing `/` matches only directories, and a glob
//! that matches a directory covers everything under it. `!` negation is
//! refused.

use std::collections::{BTreeSet, HashSet};

use agmen_proto::{
    Body, Change, ClientEnvelope, EditScope, IdentityId, IdentityKind, ItemKey, LaneName,
    MAX_BODY_BYTES, NackReason, Needs, Payload, ProjectId, Stream, Timestamp, Topics, Urgency,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use regex_automata::Anchored;
use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::nfa::thompson;
use regex_automata::util::{start, syntax};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool, Postgres, Transaction};

use crate::auth::Caller;
use crate::claims::{LaneHolder, LedgerError, LedgerMessage, lane_holders};
use crate::http::{ApiError, Hub};
use crate::identities::Identity;
use crate::ingest::{HubPost, post_alert};
use crate::items::close_claimed;

/// The most lanes one project may have.
const MAX_LANES: usize = 64;
/// The most active grants per project, lane and recipient.
const MAX_ACTIVE_LENDS: i64 = 16;
/// A grant may last at most seven days from acceptance.
const MAX_LEND_DURATION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);
/// A recipient can have grants in at most every lane in the project.
const MAX_LOADED_LENDS: i64 = 64 * MAX_ACTIVE_LENDS;
/// The most globs one lane may have.
const MAX_GLOBS_PER_LANE: usize = 32;
/// The most globs a project's lanes may have together, which bounds the
/// overlap check's pairs.
const MAX_GLOBS: usize = 256;
/// The longest glob, in bytes.
const MAX_GLOB_BYTES: usize = 256;
/// The most paths one check-path call may ask about.
const MAX_CHECK_PATHS: usize = 500;
/// The size limit for each glob's DFA and its construction, in bytes.
const DFA_LIMIT: usize = 1 << 20;
/// [`DFA_LIMIT`] in MiB, as error messages name it.
const DFA_LIMIT_MIB: usize = DFA_LIMIT >> 20;
/// The most memory the DFAs of one lane set's overlap check may use
/// together, in bytes (ruling R50). Typical globs take 1 to 3 KiB each, so
/// [`MAX_GLOBS`] of them stay far below it.
const DFA_MEMORY_BUDGET: usize = 16 << 20;
/// The most DFA transitions the overlap check of one lane set may take.
const OVERLAP_BUDGET: u64 = 20_000_000;
/// The most out-of-lane paths an ALERT lists by name (ruling R35).
const MAX_LISTED_PATHS: usize = 50;

// check-path's fixed reasons (ruling R44c). Beside `lane <LANE> held by
// <HANDLE>`, the plugin's guard shows only the phrases in its
// CHECK_PATH_PHRASES (mods/agmen-claude/hooks/register.tsx), so a new
// phrase here goes there in the same change.
/// A path in no lane.
const UNOWNED: &str = "unowned";
/// A path in a lane nobody holds a claim in; the lane is in `lane`.
const NOT_CLAIMED: &str = "not claimed";
/// A path that is empty, absolute, or has an empty, `.` or `..` segment.
const NOT_A_REPO_PATH: &str = "not a repository path";

/// A project's lanes, compiled for matching paths.
#[derive(Debug, Default)]
struct Lanes(Vec<CompiledLane>);

#[derive(Debug)]
struct CompiledLane {
    name: LaneName,
    set: GlobSet,
}

impl Lanes {
    /// The lanes of `project` as stored.
    async fn load(db: impl PgExecutor<'_>, project: &str) -> Result<Self, LedgerError> {
        let rows: Vec<(String, Vec<String>)> =
            sqlx::query_as("SELECT name, globs FROM lanes WHERE project_id = $1 ORDER BY name")
                .bind(project)
                .fetch_all(db)
                .await?;
        rows.into_iter()
            .map(|(name, globs)| {
                let stored = |err: &dyn std::fmt::Display| LedgerError::Stored(err.to_string());
                let mut set = GlobSetBuilder::new();
                for glob in &globs {
                    for compiled in compile(glob).map_err(|err| stored(&err))? {
                        set.add(compiled);
                    }
                }
                Ok(CompiledLane {
                    name: name.parse().map_err(|err| stored(&err))?,
                    set: set.build().map_err(|err| stored(&err))?,
                })
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// The lane whose globs match `path`, a repository path. Lanes never
    /// overlap, so there is at most one.
    fn lane_of(&self, path: &str) -> Option<&LaneName> {
        self.0
            .iter()
            .find(|lane| lane.set.is_match(path))
            .map(|lane| &lane.name)
    }
}

/// Why a lane glob is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum GlobError {
    #[error("glob {0:?} is empty")]
    Empty(String),
    #[error("glob {0:?} is longer than {MAX_GLOB_BYTES} bytes")]
    TooLong(String),
    #[error("glob {0:?} negates; lanes take no ! globs")]
    Negated(String),
    #[error("glob {glob:?} does not parse: {reason}")]
    Syntax { glob: String, reason: String },
}

/// The globset patterns gitignore glob `glob` stands for: the glob itself
/// and, unless it only names directories or already ends in `**`,
/// everything under what it matches.
fn expand(glob: &str) -> Result<Vec<String>, GlobError> {
    if glob.len() > MAX_GLOB_BYTES {
        return Err(GlobError::TooLong(glob.to_owned()));
    }
    if glob.starts_with('!') {
        return Err(GlobError::Negated(glob.to_owned()));
    }
    let (dir_only, rest) = match glob.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, glob),
    };
    let (anchored, rest) = match rest.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    if rest.is_empty() {
        return Err(GlobError::Empty(glob.to_owned()));
    }
    let base = if anchored || rest.contains('/') {
        rest.to_owned()
    } else {
        format!("**/{rest}")
    };
    let covers_all = base == "**" || base.ends_with("/**");
    Ok(match (dir_only, covers_all) {
        (_, true) => vec![base],
        (true, false) => vec![format!("{base}/**")],
        (false, false) => vec![format!("{base}/**"), base],
    })
}

/// `glob` compiled the way lane matching and the overlap check both use.
fn compile(glob: &str) -> Result<Vec<Glob>, GlobError> {
    expand(glob)?
        .iter()
        .map(|pattern| {
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(true)
                .build()
                .map_err(|err| GlobError::Syntax {
                    glob: glob.to_owned(),
                    reason: err.kind().to_string(),
                })
        })
        .collect()
}

/// Reuses lane syntax and limits for role edits and LEND paths.
fn scope_globs(globs: &[String]) -> Result<GlobSet, String> {
    if globs.is_empty() || globs.len() > MAX_GLOBS_PER_LANE {
        return Err(format!("expected 1 to {MAX_GLOBS_PER_LANE} globs"));
    }
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        for compiled in compile(glob).map_err(|err| err.to_string())? {
            builder.add(compiled);
        }
    }
    builder.build().map_err(|err| err.to_string())
}

pub(crate) fn validate_edits(edits: &EditScope) -> Result<(), ApiError> {
    if let EditScope::Paths { globs } | EditScope::LanesWithin { globs } = edits {
        scope_globs(globs).map_err(|err| unprocessable("bad_edits", err))?;
    }
    Ok(())
}

#[derive(Debug)]
enum EditRule {
    Lanes,
    None,
    Paths(GlobSet),
    LanesWithin(GlobSet),
}

impl EditRule {
    fn from_stored(value: Option<serde_json::Value>) -> Result<Self, LedgerError> {
        let scope = value
            .map(serde_json::from_value::<EditScope>)
            .transpose()
            .map_err(|err| LedgerError::Stored(err.to_string()))?;
        let globs = |patterns: &[String]| scope_globs(patterns).map_err(LedgerError::Stored);
        Ok(match scope {
            None | Some(EditScope::Lanes) => Self::Lanes,
            Some(EditScope::None) => Self::None,
            Some(EditScope::Paths { globs: patterns }) => Self::Paths(globs(&patterns)?),
            Some(EditScope::LanesWithin { globs: patterns }) => {
                Self::LanesWithin(globs(&patterns)?)
            }
        })
    }

    fn allows(&self, path: &str, lane_allowed: bool) -> bool {
        match self {
            Self::None => false,
            Self::Lanes => lane_allowed,
            Self::Paths(globs) | Self::LanesWithin(globs) => lane_allowed && globs.is_match(path),
        }
    }
}

#[derive(Debug)]
struct Lends(Vec<(String, GlobSet)>);

impl Lends {
    async fn load(
        db: impl PgExecutor<'_>,
        project: &str,
        identity: &IdentityId,
    ) -> Result<Self, LedgerError> {
        let rows: Vec<(String, Vec<String>)> = sqlx::query_as(
            "SELECT g.lane, g.paths FROM lane_lends g
             JOIN lanes l ON l.project_id=g.project_id AND l.name=g.lane
             JOIN identities i ON i.id=g.from_id AND i.project_id=g.project_id
             WHERE g.project_id=$1 AND g.to_id=$2 AND g.until_at > now()
               AND i.revoked_at IS NULL
               AND (l.owner_identity_id=g.from_id OR i.kind='coordinator')
             ORDER BY g.lane, g.until_at, g.message_id LIMIT $3",
        )
        .bind(project)
        .bind(identity.as_str())
        .bind(MAX_LOADED_LENDS)
        .fetch_all(db)
        .await?;
        rows.into_iter()
            .map(|(lane, paths)| {
                scope_globs(&paths)
                    .map(|globs| (lane, globs))
                    .map_err(LedgerError::Stored)
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }

    fn allows(&self, lane: Option<&LaneName>, path: &str) -> bool {
        lane.is_some_and(|lane| {
            self.0
                .iter()
                .any(|(name, globs)| name == lane.as_str() && globs.is_match(path))
        })
    }
}

/// A LEND refusal is final; database errors remain retryable.
#[derive(Debug)]
pub(crate) enum LendError {
    Refused(NackReason),
    Database(sqlx::Error),
}

impl From<sqlx::Error> for LendError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

/// Validates and stores a LEND in the message transaction. Locking its lane
/// keeps replacement or ownership changes from racing the authorization.
pub(crate) async fn lend(
    tx: &mut Transaction<'_, Postgres>,
    topics: &Topics,
    sender: &IdentityId,
    kind: IdentityKind,
    env: &ClientEnvelope,
) -> Result<(), LendError> {
    let Payload::Lend {
        lane,
        paths,
        to,
        until,
    } = &env.payload
    else {
        return Err(LendError::Refused(NackReason::Schema));
    };
    let owner: Option<Option<String>> = sqlx::query_scalar(
        "SELECT owner_identity_id FROM lanes WHERE project_id=$1 AND name=$2 FOR UPDATE",
    )
    .bind(topics.project().as_str())
    .bind(lane.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    let Some(owner) = owner else {
        return Err(LendError::Refused(NackReason::Schema));
    };
    if kind != IdentityKind::Coordinator && owner.as_deref() != Some(sender.as_str()) {
        return Err(LendError::Refused(NackReason::NotLaneOwner));
    }
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM identities WHERE id=$1 AND project_id=$2 AND revoked_at IS NULL)",
    ).bind(to.as_str()).bind(topics.project().as_str()).fetch_one(&mut **tx).await?;
    let now = Timestamp::now();
    if !live || *until <= now || *until > now + MAX_LEND_DURATION || scope_globs(paths).is_err() {
        return Err(LendError::Refused(NackReason::Schema));
    }
    // The lane row lock serializes this count with concurrent grants.
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM lane_lends WHERE project_id=$1 AND lane=$2 AND to_id=$3 AND until_at > now()",
    ).bind(topics.project().as_str()).bind(lane.as_str()).bind(to.as_str())
        .fetch_one(&mut **tx).await?;
    if active >= MAX_ACTIVE_LENDS {
        return Err(LendError::Refused(NackReason::Schema));
    }
    sqlx::query(
        "INSERT INTO lane_lends (project_id, lane, message_id, from_id, to_id, paths, until_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7::timestamptz)",
    )
    .bind(topics.project().as_str())
    .bind(lane.as_str())
    .bind(env.id.as_str())
    .bind(sender.as_str())
    .bind(to.as_str())
    .bind(paths)
    .bind(until.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A DFA for the paths that one lane glob matches.
type GlobDfa = dense::DFA<Vec<u32>>;

/// Why the overlap check could not finish.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum OverlapError {
    #[error(
        "glob {0:?} is too complex to check for overlaps: its DFA needs more than {DFA_LIMIT_MIB} MiB"
    )]
    TooComplex(String),
    #[error(
        "the lane globs are too many or too complex to check for overlaps: the check needs more than {OVERLAP_BUDGET} steps"
    )]
    OverBudget,
    #[error(
        "the lane globs are too complex to check for overlaps: their DFAs need more than {} MiB together",
        DFA_MEMORY_BUDGET >> 20
    )]
    OverMemory,
}

/// Builds the DFA for `globs`, the compiled form of `source`, from the
/// regexes globset matches with, so the check and matching agree.
fn glob_dfa(source: &str, globs: &[Glob]) -> Result<GlobDfa, OverlapError> {
    let pattern = globs
        .iter()
        .map(|glob| format!("(?:{})", glob.regex()))
        .collect::<Vec<_>>()
        .join("|");
    dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .dfa_size_limit(Some(DFA_LIMIT))
                .determinize_size_limit(Some(DFA_LIMIT)),
        )
        .syntax(
            syntax::Config::new()
                .unicode(false)
                .utf8(false)
                .dot_matches_new_line(true),
        )
        .thompson(thompson::Config::new().utf8(false))
        .build(&pattern)
        .map_err(|_| OverlapError::TooComplex(source.to_owned()))
}

/// Whether some byte string is matched by both `a` and `b`: a search of the
/// product automaton, each step charged to `budget`.
fn intersects(a: &GlobDfa, b: &GlobDfa, budget: &mut u64) -> Result<bool, OverlapError> {
    let anchored = start::Config::new().anchored(Anchored::Yes);
    let (Ok(start_a), Ok(start_b)) = (a.start_state(&anchored), b.start_state(&anchored)) else {
        return Err(OverlapError::OverBudget);
    };
    let mut seen = HashSet::from([(start_a, start_b)]);
    let mut todo = vec![(start_a, start_b)];
    while let Some((state_a, state_b)) = todo.pop() {
        if a.is_match_state(a.next_eoi_state(state_a))
            && b.is_match_state(b.next_eoi_state(state_b))
        {
            return Ok(true);
        }
        for byte in 0..=u8::MAX {
            *budget = budget.checked_sub(1).ok_or(OverlapError::OverBudget)?;
            let next = (a.next_state(state_a, byte), b.next_state(state_b, byte));
            let dead = |dfa: &GlobDfa, state| dfa.is_dead_state(state) || dfa.is_quit_state(state);
            if !dead(a, next.0) && !dead(b, next.1) && seen.insert(next) {
                todo.push(next);
            }
        }
    }
    Ok(false)
}

/// Two globs of different lanes that match a common path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Overlap {
    lanes: (LaneName, LaneName),
    globs: (String, String),
}

/// The first pair of globs from different lanes in `lanes` that match a
/// common path, or `None` when the lanes are disjoint. Stops with
/// [`OverlapError::OverMemory`] as soon as the DFAs built so far pass
/// [`DFA_MEMORY_BUDGET`].
fn find_overlap(lanes: &[LaneSpec]) -> Result<Option<Overlap>, OverlapError> {
    let mut dfas = Vec::new();
    let mut memory = 0_usize;
    for (index, lane) in lanes.iter().enumerate() {
        for glob in &lane.globs {
            let compiled = compile(glob).map_err(|_| OverlapError::TooComplex(glob.clone()))?;
            let dfa = glob_dfa(glob, &compiled)?;
            memory = memory.saturating_add(dfa.memory_usage());
            if memory > DFA_MEMORY_BUDGET {
                return Err(OverlapError::OverMemory);
            }
            dfas.push((index, glob, dfa));
        }
    }
    let mut budget = OVERLAP_BUDGET;
    for (i, (lane_a, glob_a, dfa_a)) in dfas.iter().enumerate() {
        for (lane_b, glob_b, dfa_b) in dfas.iter().skip(i + 1) {
            if lane_a != lane_b && intersects(dfa_a, dfa_b, &mut budget)? {
                return Ok(Some(Overlap {
                    lanes: (lanes[*lane_a].name.clone(), lanes[*lane_b].name.clone()),
                    globs: ((*glob_a).clone(), (*glob_b).clone()),
                }));
            }
        }
    }
    Ok(None)
}

/// `path` as a repository path, or `None` when it is empty, absolute, or
/// has an empty, `.` or `..` segment. Leading `./` is dropped.
fn repo_path(path: &str) -> Option<&str> {
    let mut path = path;
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    let bad_segment = |segment: &str| matches!(segment, "" | "." | "..");
    (!path.starts_with('/') && !path.contains('\0') && !path.split('/').any(bad_segment))
        .then_some(path)
}

/// A `PUT` body: the whole lane list, as `{"lanes": [...]}` in JSON or
/// `[[lanes]]` tables in TOML.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneFile {
    lanes: Vec<LaneSpec>,
}

/// One lane in a `PUT` body. `owner` is a live identity of the project, by
/// id or by handle.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaneSpec {
    name: LaneName,
    globs: Vec<String>,
    #[serde(default)]
    owner: Option<String>,
}

/// One lane as `GET` and `PUT` answer.
#[derive(Debug, Serialize)]
pub(crate) struct LaneView {
    name: String,
    globs: Vec<String>,
    owner: Option<String>,
    owner_handle: Option<String>,
}

/// The `GET` and `PUT` answer: the lane set's version and its lanes, by name.
#[derive(Debug, Serialize)]
pub(crate) struct LaneList {
    v: i64,
    lanes: Vec<LaneView>,
}

/// Who called a project route, checked to belong to that project.
#[derive(Debug, Clone)]
pub(crate) enum Member {
    /// A human member of the project.
    Human,
    /// A live identity of the project.
    Identity(Identity),
}

/// The project `project` names, when `caller` belongs to it: a human with a
/// membership, or an identity of that project.
///
/// # Errors
///
/// 404 for a malformed project id; 403 `not_a_member` for a project the
/// caller does not belong to, which is also the answer for one that does
/// not exist.
pub(crate) async fn member_of(
    hub: &Hub,
    caller: Caller,
    project: &str,
) -> Result<(ProjectId, Member), ApiError> {
    let project: ProjectId = project
        .parse()
        .map_err(|_| ApiError::not_found("no such project"))?;
    let not_a_member =
        || ApiError::forbidden("not_a_member", "you are not a member of that project");
    match caller {
        Caller::Identity(identity) if identity.project == project => {
            Ok((project, Member::Identity(identity)))
        }
        Caller::Identity(_) => Err(not_a_member()),
        Caller::Human(human) => {
            let member: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM memberships WHERE human_id = $1 AND project_id = $2)",
            )
            .bind(human.as_str())
            .bind(project.as_str())
            .fetch_one(hub.pool())
            .await?;
            if member {
                Ok((project, Member::Human))
            } else {
                Err(not_a_member())
            }
        }
    }
}

fn unprocessable(code: &'static str, description: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, code, description)
}

/// `GET /v1/projects/{p}/lanes` (any member).
pub(crate) async fn list(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
) -> Result<Json<LaneList>, ApiError> {
    let (project, _) = member_of(&hub, caller, &project).await?;
    Ok(Json(lane_list(hub.pool(), &project).await?))
}

/// A lane's name, globs, owner id and handle, and v.
type LaneRow = (String, Vec<String>, Option<String>, Option<String>, i64);

async fn lane_list(pool: &PgPool, project: &ProjectId) -> Result<LaneList, sqlx::Error> {
    let rows: Vec<LaneRow> = sqlx::query_as(
        "SELECT l.name, l.globs, l.owner_identity_id, i.handle, l.v
         FROM lanes l LEFT JOIN identities i ON i.id = l.owner_identity_id
         WHERE l.project_id = $1 ORDER BY l.name",
    )
    .bind(project.as_str())
    .fetch_all(pool)
    .await?;
    let v = rows.iter().map(|row| row.4).max().unwrap_or(0);
    Ok(LaneList {
        v,
        lanes: rows
            .into_iter()
            .map(|(name, globs, owner, owner_handle, _)| LaneView {
                name,
                globs,
                owner,
                owner_handle,
            })
            .collect(),
    })
}

/// `PUT /v1/projects/{p}/lanes` (a human member or a coordinator identity
/// of the project): replaces the project's lanes with the body's, a JSON
/// (`application/json`) or TOML (`application/toml`) lane list. A set whose
/// globs overlap across lanes gets 422 `overlapping_lanes`.
pub(crate) async fn replace(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
    headers: HeaderMap,
    body: String,
) -> Result<Json<LaneList>, ApiError> {
    let (project, member) = member_of(&hub, caller, &project).await?;
    if let Member::Identity(identity) = &member
        && identity.kind != IdentityKind::Coordinator
    {
        return Err(ApiError::forbidden(
            "coordinator_required",
            "only humans and coordinators change lanes",
        ));
    }
    let file = parse_lane_file(&headers, &body)?;
    check_lane_specs(&file.lanes)?;
    let specs = file.lanes.clone();
    let overlap = tokio::task::spawn_blocking(move || find_overlap(&specs))
        .await
        .map_err(ApiError::stored)?
        .map_err(|err| unprocessable("lanes_too_complex", err.to_string()))?;
    if let Some(Overlap { lanes, globs }) = overlap {
        return Err(unprocessable(
            "overlapping_lanes",
            format!(
                "{} glob {:?} and {} glob {:?} match a common path",
                lanes.0, globs.0, lanes.1, globs.1
            ),
        ));
    }
    let mut owners = Vec::with_capacity(file.lanes.len());
    for lane in &file.lanes {
        owners.push(match &lane.owner {
            Some(owner) => Some(resolve_owner(hub.pool(), &project, &lane.name, owner).await?),
            None => None,
        });
    }

    let mut tx = hub.pool().begin().await?;
    // Serializes PUTs of one project, so each gets its own v. NO KEY UPDATE
    // leaves inserts that reference the project unblocked.
    sqlx::query("SELECT 1 FROM projects WHERE id = $1 FOR NO KEY UPDATE")
        .bind(project.as_str())
        .execute(&mut *tx)
        .await?;
    let v: i64 =
        sqlx::query_scalar("SELECT coalesce(max(v), 0) + 1 FROM lanes WHERE project_id = $1")
            .bind(project.as_str())
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM lanes WHERE project_id = $1")
        .bind(project.as_str())
        .execute(&mut *tx)
        .await?;
    for (lane, owner) in file.lanes.iter().zip(&owners) {
        sqlx::query(
            "INSERT INTO lanes (project_id, name, globs, owner_identity_id, v)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(project.as_str())
        .bind(lane.name.as_str())
        .bind(&lane.globs)
        .bind(owner.as_deref())
        .bind(v)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    tracing::info!(%project, v, lanes = file.lanes.len(), "replaced lanes");
    Ok(Json(lane_list(hub.pool(), &project).await?))
}

/// The body as a lane list, by its content type.
fn parse_lane_file(headers: &HeaderMap, body: &str) -> Result<LaneFile, ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase());
    let bad = |err: &dyn std::fmt::Display| unprocessable("bad_lanes", err.to_string());
    match content_type.as_deref() {
        Some("application/json") => serde_json::from_str(body).map_err(|err| bad(&err)),
        Some("application/toml") => toml::from_str(body).map_err(|err| bad(&err)),
        _ => Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "send the lanes as application/json or application/toml",
        )),
    }
}

/// The checks a lane list must pass before the overlap check: limits,
/// unique names, and globs that parse.
fn check_lane_specs(lanes: &[LaneSpec]) -> Result<(), ApiError> {
    if lanes.len() > MAX_LANES {
        return Err(unprocessable(
            "too_many_lanes",
            format!("a project has at most {MAX_LANES} lanes"),
        ));
    }
    let total: usize = lanes.iter().map(|lane| lane.globs.len()).sum();
    if total > MAX_GLOBS {
        return Err(unprocessable(
            "too_many_globs",
            format!("a project's lanes have at most {MAX_GLOBS} globs together"),
        ));
    }
    let mut names = BTreeSet::new();
    for lane in lanes {
        if !names.insert(&lane.name) {
            return Err(unprocessable(
                "duplicate_lane",
                format!("{} is listed twice", lane.name),
            ));
        }
        if lane.globs.is_empty() || lane.globs.len() > MAX_GLOBS_PER_LANE {
            return Err(unprocessable(
                "bad_globs",
                format!("{} needs 1 to {MAX_GLOBS_PER_LANE} globs", lane.name),
            ));
        }
        for glob in &lane.globs {
            compile(glob)
                .map_err(|err| unprocessable("bad_globs", format!("{}: {err}", lane.name)))?;
        }
    }
    Ok(())
}

/// The id of `owner`, a live identity of `project` named by id or handle.
async fn resolve_owner(
    pool: &PgPool,
    project: &ProjectId,
    lane: &LaneName,
    owner: &str,
) -> Result<String, ApiError> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM identities
         WHERE project_id = $1 AND revoked_at IS NULL AND (id = $2 OR handle = $2)",
    )
    .bind(project.as_str())
    .bind(owner)
    .fetch_optional(pool)
    .await?;
    id.ok_or_else(|| {
        unprocessable(
            "unknown_owner",
            format!("{lane}'s owner is not a live identity of this project"),
        )
    })
}

/// `POST /v1/projects/{p}/check-path` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckPath {
    identity: IdentityId,
    paths: Vec<String>,
}

/// One answer of check-path: one per path asked, in order, `path` as given
/// (ruling R16a). `reason` is built only from lane names, handles and fixed
/// phrases (ruling R44c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PathCheck {
    path: String,
    lane: Option<LaneName>,
    allowed: bool,
    reason: String,
}

/// `POST /v1/projects/{p}/check-path` (any member): whether `identity` may
/// edit each path. A path in no lane is allowed (`unowned`); a path in a
/// lane is allowed when `identity` holds a claim on an item of that lane.
pub(crate) async fn check_path(
    State(hub): State<Hub>,
    Path(project): Path<String>,
    caller: Caller,
    Json(request): Json<CheckPath>,
) -> Result<Json<Vec<PathCheck>>, ApiError> {
    let (project, _) = member_of(&hub, caller, &project).await?;
    if request.paths.len() > MAX_CHECK_PATHS {
        return Err(unprocessable(
            "too_many_paths",
            format!("check at most {MAX_CHECK_PATHS} paths at once"),
        ));
    }
    let live: Option<(String, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT kind, edits FROM identities WHERE id = $1 AND project_id = $2 AND revoked_at IS NULL",
    )
    .bind(request.identity.as_str())
    .bind(project.as_str())
    .fetch_optional(hub.pool())
    .await?;
    let Some((kind, edits)) = live else {
        return Err(ApiError::not_found(
            "no live identity with that id in this project",
        ));
    };
    let edits = if kind == "listener" {
        EditRule::None
    } else {
        EditRule::from_stored(edits).map_err(ApiError::stored)?
    };
    let lends = Lends::load(hub.pool(), project.as_str(), &request.identity)
        .await
        .map_err(ApiError::stored)?;
    let lanes = Lanes::load(hub.pool(), project.as_str())
        .await
        .map_err(ApiError::stored)?;
    let holders = lane_holders(hub.pool(), project.as_str())
        .await
        .map_err(ApiError::stored)?;
    Ok(Json(
        request
            .paths
            .into_iter()
            .map(|path| judge_scoped(&lanes, &holders, &request.identity, &edits, &lends, path))
            .collect(),
    ))
}

fn judge_scoped(
    lanes: &Lanes,
    holders: &[LaneHolder],
    identity: &IdentityId,
    edits: &EditRule,
    lends: &Lends,
    path: String,
) -> PathCheck {
    let mut check = judge(lanes, holders, identity, path);
    let Some(path) = repo_path(&check.path) else {
        return check;
    };
    let lane_allowed = check.allowed || lends.allows(check.lane.as_ref(), path);
    let original_allowed = check.allowed;
    check.allowed = edits.allows(path, lane_allowed);
    if !check.allowed && !edits.allows(path, true) {
        check.reason = "outside edit scope".to_owned();
    } else if check.allowed && !original_allowed {
        check.reason = "allowed by lend".to_owned();
    }
    check
}

/// The check-path answer for `path`.
fn judge(lanes: &Lanes, holders: &[LaneHolder], identity: &IdentityId, path: String) -> PathCheck {
    let outside = |path: String, allowed: bool, reason: &str| PathCheck {
        path,
        lane: None,
        allowed,
        reason: reason.to_owned(),
    };
    let Some(repo) = repo_path(&path) else {
        return outside(path, false, NOT_A_REPO_PATH);
    };
    let Some(lane) = lanes.lane_of(repo).cloned() else {
        return outside(path, true, UNOWNED);
    };
    let in_lane: Vec<&LaneHolder> = holders
        .iter()
        .filter(|holder| holder.lane == lane)
        .collect();
    let own = in_lane.iter().find(|holder| &holder.identity == identity);
    let (allowed, reason) = match (own, in_lane.first()) {
        (Some(own), _) => (true, format!("lane {lane} held by {}", own.handle)),
        (None, Some(other)) => (false, format!("lane {lane} held by {}", other.handle)),
        (None, None) => (false, NOT_CLAIMED.to_owned()),
    };
    PathCheck {
        path,
        lane: Some(lane),
        allowed,
        reason,
    }
}

/// A DONE path outside the sender's claimed lanes: its lane, or `None` for
/// a path that is not a repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stray {
    path: String,
    lane: Option<LaneName>,
}

/// Checks a DONE ingest has just written in `tx`: closes the item and ends
/// the sender's claim on it when the sender holds it, and when a changed path lies in a lane the sender holds
/// no claim in (or is no repository path), posts an ALERT naming those
/// paths to the coordinators, or to the sender's owner when the project has
/// none (ruling R35).
pub(crate) async fn check_done(
    tx: &mut Transaction<'_, Postgres>,
    topics: &Topics,
    msg: &LedgerMessage,
    item: &ItemKey,
    changes: &[Change],
) -> Result<(), LedgerError> {
    let project = topics.project().as_str();
    // The lanes held before this DONE ends its claim, which counts.
    let holders = lane_holders(&mut **tx, project).await?;
    close_claimed(tx, project, item, &msg.from).await?;
    let lanes = Lanes::load(&mut **tx, project).await?;
    let edits: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT edits FROM identities WHERE id=$1")
            .bind(msg.from.as_str())
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
    let edits = if msg.from_kind == IdentityKind::Listener {
        EditRule::None
    } else {
        EditRule::from_stored(edits)?
    };
    let lends = Lends::load(&mut **tx, project, &msg.from).await?;
    let strays: Vec<Stray> = changes
        .iter()
        .filter_map(|change| {
            let check = judge_scoped(
                &lanes,
                &holders,
                &msg.from,
                &edits,
                &lends,
                change.path.clone(),
            );
            (!check.allowed).then_some(Stray {
                path: check.path,
                lane: check.lane,
            })
        })
        .collect();
    if strays.is_empty() {
        return Ok(());
    }
    let body = if matches!(edits, EditRule::Lanes) {
        stray_body(&msg.from_handle, item, &strays)
    } else {
        stray_body_with(&msg.from_handle, item, &strays, |stray| match &stray.lane {
            Some(lane) => lane.to_string(),
            None if repo_path(&stray.path).is_some() => "outside edit scope".to_owned(),
            None => NOT_A_REPO_PATH.to_owned(),
        })
    };
    let alert = HubPost {
        payload: Payload::Alert {
            need: format!(
                "{item}: DONE changed {} outside the claimed lanes",
                plural(strays.len(), "path")
            ),
            urgency: Urgency::Normal,
            needs: Needs::Anyone,
            actions: Vec::new(),
        },
        body: Body::try_from(body)?,
        reply_to: Some(msg.env.id.clone()),
        items: vec![item.clone()],
        lane: None,
        mentioned: Vec::new(),
        streams: vec![Stream::Feed],
    };
    let fallback = msg.from_owner.as_ref().unwrap_or(&msg.from);
    post_alert(tx, topics, alert, fallback).await?;
    Ok(())
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// The ALERT text for `strays`: at most [`MAX_LISTED_PATHS`] paths, one per
/// line, then "and N more", and never more than a body holds. Each path is
/// the client's own text, so it is quoted with Rust's string escapes: a
/// newline or other control character in it cannot start a line of the
/// hub's text (ruling R44c).
fn stray_body(handle: &str, item: &ItemKey, strays: &[Stray]) -> String {
    stray_body_with(handle, item, strays, |stray| {
        stray
            .lane
            .as_ref()
            .map_or_else(|| NOT_A_REPO_PATH.to_owned(), ToString::to_string)
    })
}

/// The bounded renderer shared by lane diagnostics and role-scope diagnostics.
fn stray_body_with(
    handle: &str,
    item: &ItemKey,
    strays: &[Stray],
    label: impl Fn(&Stray) -> String,
) -> String {
    let mut body = format!("{handle}'s DONE for {item} changed paths outside its claimed lanes:");
    // Room for the "and N more" line.
    let limit = MAX_BODY_BYTES - 32;
    let mut listed = 0;
    for stray in strays.iter().take(MAX_LISTED_PATHS) {
        let line = format!("\n- {:?} ({})", stray.path, label(stray));
        if body.len() + line.len() > limit {
            break;
        }
        body.push_str(&line);
        listed += 1;
    }
    let rest = strays.len() - listed;
    if rest > 0 {
        body.push_str(&format!("\nand {rest} more"));
    }
    body
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
