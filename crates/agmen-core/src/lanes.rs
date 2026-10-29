//! Lane rules (design 6.5): gitignore-style globs, the check that no two
//! lanes match one path, edit scopes, lends, and the judgment check-path
//! and DONE share.
//!
//! A lane glob follows gitignore: `*` and `?` never match `/`, `**` matches
//! any number of directories, a glob with no `/` matches at any depth
//! (`*.rs` is `**/*.rs`), a leading `/` anchors it at the repository root,
//! a trailing `/` matches only directories, and a glob that matches a
//! directory covers everything under it. `!` negation is refused.
//!
//! The backend stores the rows. This crate decides which lane a path is in
//! and whether an identity may edit it.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use agmen_proto::{
    EditScope, IdentityId, IdentityKind, ItemKey, LaneName, MAX_BODY_BYTES, ProjectId,
};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use regex_automata::Anchored;
use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::nfa::thompson;
use regex_automata::util::{start, syntax};
use serde::{Deserialize, Serialize};

use crate::core::{Core, CoreError};
use crate::store::{Reads, Store, StoreError, StoreTx};

/// The most lanes one project may have.
const MAX_LANES: usize = 64;
/// The most active grants per project, lane and recipient.
pub const MAX_ACTIVE_LENDS: i64 = 16;
/// A grant may last at most seven days from acceptance.
pub const MAX_LEND_DURATION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// A recipient can have grants in at most every lane in the project.
pub const MAX_LOADED_LENDS: i64 = 64 * MAX_ACTIVE_LENDS;
/// The most globs one lane may have.
const MAX_GLOBS_PER_LANE: usize = 32;
/// The most globs a project's lanes may have together, which bounds the
/// overlap check's pairs.
const MAX_GLOBS: usize = 256;
/// The longest glob, in bytes.
const MAX_GLOB_BYTES: usize = 256;
/// The most paths one check-path call may ask about.
pub const MAX_CHECK_PATHS: usize = 500;
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
pub const NOT_A_REPO_PATH: &str = "not a repository path";

/// A project's lanes, compiled for matching paths.
#[derive(Debug, Default)]
pub struct Lanes(Vec<CompiledLane>);

#[derive(Debug)]
struct CompiledLane {
    name: LaneName,
    set: GlobSet,
}

impl Lanes {
    /// Compiles stored lane rows. Lanes never overlap, so matching finds
    /// at most one.
    ///
    /// # Errors
    ///
    /// Returns [`LaneError`] when a stored glob does not compile. Rows the
    /// hub accepted already passed [`check_lane_specs`].
    ///
    /// # Examples
    ///
    /// ```
    /// use agmen_core::Lanes;
    /// use agmen_proto::LaneName;
    /// let lanes = Lanes::compile(vec![("L-UI".parse()?, vec!["ui/**".to_owned()])])?;
    /// assert_eq!(lanes.lane_of("ui/x.rs").map(LaneName::as_str), Some("L-UI"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn compile(rows: Vec<(LaneName, Vec<String>)>) -> Result<Self, LaneError> {
        let mut compiled = Vec::with_capacity(rows.len());
        for (name, globs) in rows {
            let mut set = GlobSetBuilder::new();
            for glob in &globs {
                for pattern in compile(glob).map_err(|err| LaneError::new(err.to_string()))? {
                    set.add(pattern);
                }
            }
            let set = set.build().map_err(|err| LaneError::new(err.to_string()))?;
            compiled.push(CompiledLane { name, set });
        }
        Ok(Self(compiled))
    }

    /// The lane whose globs match `path`, a repository path.
    #[must_use]
    pub fn lane_of(&self, path: &str) -> Option<&LaneName> {
        self.0
            .iter()
            .find(|lane| lane.set.is_match(path))
            .map(|lane| &lane.name)
    }
}

/// Why compiling or judging a lane failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct LaneError {
    message: String,
}

impl LaneError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<LaneError> for CoreError {
    fn from(err: LaneError) -> Self {
        Self::Stored(err.to_string())
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
///
/// # Errors
///
/// Returns a sentence when `globs` is empty, too long, or one glob does
/// not compile.
///
/// # Examples
///
/// ```
/// use agmen_core::scope_globs;
/// assert!(scope_globs(&["src/**".to_owned()]).is_ok());
/// assert!(scope_globs(&[]).is_err());
/// ```
pub fn scope_globs(globs: &[String]) -> Result<GlobSet, String> {
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

/// Accepts an edit scope whose path globs compile under the lane limits.
///
/// # Errors
///
/// Returns the glob sentence when a `paths` or `lanes_within` scope is
/// empty or does not compile. `lanes` and `none` always succeed.
///
/// # Examples
///
/// ```
/// use agmen_core::validate_edits;
/// use agmen_proto::EditScope;
/// validate_edits(&EditScope::Lanes)?;
/// # Ok::<(), String>(())
/// ```
pub fn validate_edits(edits: &EditScope) -> Result<(), String> {
    if let EditScope::Paths { globs } | EditScope::LanesWithin { globs } = edits {
        scope_globs(globs)?;
    }
    Ok(())
}

/// How a live identity's stored edit scope narrows a lane decision.
#[derive(Debug)]
pub enum EditRule {
    /// Claimed or lent lanes, with no extra glob.
    Lanes,
    /// Nothing, including a listener's scope.
    None,
    /// A path must be in an allowed lane and match these globs.
    Paths(GlobSet),
    /// Same as [`Paths`](Self::Paths), stored as `lanes_within`.
    LanesWithin(GlobSet),
}

impl EditRule {
    /// Builds the rule from the `edits` column. `None` is [`Lanes`](Self::Lanes).
    ///
    /// # Errors
    ///
    /// Returns [`LaneError`] when the JSON is not an edit scope or its
    /// globs do not compile.
    pub fn from_stored(value: Option<serde_json::Value>) -> Result<Self, LaneError> {
        let scope = value
            .map(serde_json::from_value::<EditScope>)
            .transpose()
            .map_err(|err| LaneError::new(err.to_string()))?;
        let globs = |patterns: &[String]| scope_globs(patterns).map_err(LaneError::new);
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

/// Active lends for one identity, compiled the same way as edit globs.
#[derive(Debug)]
pub struct Lends(Vec<(String, GlobSet)>);

impl Lends {
    /// Compiles lend rows. Each row is one grant's lane and path globs.
    ///
    /// # Errors
    ///
    /// Returns [`LaneError`] when a stored path glob does not compile.
    ///
    /// # Examples
    ///
    /// ```
    /// use agmen_core::Lends;
    /// Lends::compile(vec![])?;
    /// # Ok::<(), agmen_core::LaneError>(())
    /// ```
    pub fn compile(rows: Vec<(LaneName, Vec<String>)>) -> Result<Self, LaneError> {
        let mut compiled = Vec::with_capacity(rows.len());
        for (lane, paths) in rows {
            let globs = scope_globs(&paths).map_err(LaneError::new)?;
            compiled.push((lane.to_string(), globs));
        }
        Ok(Self(compiled))
    }

    fn allows(&self, lane: Option<&LaneName>, path: &str) -> bool {
        lane.is_some_and(|lane| {
            self.0
                .iter()
                .any(|(name, globs)| name == lane.as_str() && globs.is_match(path))
        })
    }
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
///
/// # Examples
///
/// ```
/// use agmen_core::repo_path;
/// assert_eq!(repo_path("./ui/x.rs"), Some("ui/x.rs"));
/// assert_eq!(repo_path("/etc/passwd"), None);
/// ```
#[must_use]
pub fn repo_path(path: &str) -> Option<&str> {
    let mut path = path;
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    let bad_segment = |segment: &str| matches!(segment, "" | "." | "..");
    (!path.starts_with('/') && !path.contains('\0') && !path.split('/').any(bad_segment))
        .then_some(path)
}

/// One lane in a replacement list. `owner` is a live identity, by id or handle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneSpec {
    /// The lane name, such as `L-UI`.
    pub name: LaneName,
    /// Gitignore-style globs, 1 to 32 of them.
    pub globs: Vec<String>,
    /// A live identity's id or handle, when the lane has an owner.
    #[serde(default)]
    pub owner: Option<String>,
}

/// Why a lane list was refused before the overlap check.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{description}")]
#[non_exhaustive]
pub struct LaneSpecError {
    /// The stable code, such as `too_many_lanes`.
    pub code: &'static str,
    /// The sentence a client sees.
    pub description: String,
}

fn spec_err(code: &'static str, description: impl Into<String>) -> LaneSpecError {
    LaneSpecError {
        code,
        description: description.into(),
    }
}

/// The checks a lane list must pass before the overlap check: limits,
/// unique names, and globs that parse.
///
/// # Errors
///
/// Returns [`LaneSpecError`] with today's codes: `too_many_lanes`,
/// `too_many_globs`, `duplicate_lane`, and `bad_globs`.
///
/// # Examples
///
/// ```
/// use agmen_core::{LaneSpec, check_lane_specs};
/// let lanes = [LaneSpec {
///     name: "L-UI".parse()?,
///     globs: vec!["ui/**".to_owned()],
///     owner: None,
/// }];
/// check_lane_specs(&lanes)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn check_lane_specs(lanes: &[LaneSpec]) -> Result<(), LaneSpecError> {
    if lanes.len() > MAX_LANES {
        return Err(spec_err(
            "too_many_lanes",
            format!("a project has at most {MAX_LANES} lanes"),
        ));
    }
    let total: usize = lanes.iter().map(|lane| lane.globs.len()).sum();
    if total > MAX_GLOBS {
        return Err(spec_err(
            "too_many_globs",
            format!("a project's lanes have at most {MAX_GLOBS} globs together"),
        ));
    }
    let mut names = BTreeSet::new();
    for lane in lanes {
        if !names.insert(&lane.name) {
            return Err(spec_err(
                "duplicate_lane",
                format!("{} is listed twice", lane.name),
            ));
        }
        if lane.globs.is_empty() || lane.globs.len() > MAX_GLOBS_PER_LANE {
            return Err(spec_err(
                "bad_globs",
                format!("{} needs 1 to {MAX_GLOBS_PER_LANE} globs", lane.name),
            ));
        }
        for glob in &lane.globs {
            compile(glob).map_err(|err| spec_err("bad_globs", format!("{}: {err}", lane.name)))?;
        }
    }
    Ok(())
}

/// One lane as the list route returns it.
#[derive(Debug, Serialize)]
struct LaneView {
    name: String,
    globs: Vec<String>,
    owner: Option<String>,
    owner_handle: Option<String>,
}

/// The list and replace answer: the lane set's version and its lanes, by name.
#[derive(Debug, Serialize)]
pub struct LaneList {
    v: i64,
    lanes: Vec<LaneView>,
}

impl LaneList {
    fn from_rows(rows: Vec<LaneRow>) -> Self {
        let v = rows.iter().map(|row| row.v).max().unwrap_or(0);
        Self {
            v,
            lanes: rows
                .into_iter()
                .map(|row| LaneView {
                    name: row.name,
                    globs: row.globs,
                    owner: row.owner,
                    owner_handle: row.owner_handle,
                })
                .collect(),
        }
    }

    /// The lane set's version, or zero when the project has no lanes.
    #[must_use]
    pub const fn version(&self) -> i64 {
        self.v
    }
}

/// A stored lane row: name, globs, owner id, owner handle, and version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneRow {
    /// The lane name.
    pub name: String,
    /// The lane's globs, as stored.
    pub globs: Vec<String>,
    /// The owner's identity id, when the lane has one.
    pub owner: Option<String>,
    /// The owner's handle, when the owner row still exists.
    pub owner_handle: Option<String>,
    /// The version of the set this row belongs to.
    pub v: i64,
}

/// A lane whose owner, if any, is already an identity id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLane {
    /// The lane name.
    pub name: LaneName,
    /// The lane's globs.
    pub globs: Vec<String>,
    /// The owner's identity id.
    pub owner: Option<IdentityId>,
}

/// One identity's active claim on an item of a lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneHolder {
    /// The item's lane.
    pub lane: LaneName,
    /// Who holds the claim.
    pub identity: IdentityId,
    /// The holder's handle, as the reason names it.
    pub handle: String,
}

/// One answer of check-path: one per path asked, in order, `path` as given
/// (ruling R16a). `reason` is built only from lane names, handles and fixed
/// phrases (ruling R44c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathCheck {
    /// The path as the caller sent it.
    pub path: String,
    /// The lane that contains the path, when it is a repository path in a lane.
    pub lane: Option<LaneName>,
    /// Whether the identity may edit the path.
    pub allowed: bool,
    /// Why `allowed` is what it is.
    pub reason: String,
}

/// The check-path answer for one path, including edit scope and lends.
///
/// # Examples
///
/// ```
/// use agmen_core::{EditRule, Lanes, Lends, judge_scoped};
/// let lanes = Lanes::compile(vec![])?;
/// let lends = Lends::compile(vec![])?;
/// let who = "worker00".parse()?;
/// let check = judge_scoped(&lanes, &[], &who, &EditRule::Lanes, &lends, "docs/a.md".into());
/// assert!(check.allowed);
/// assert_eq!(check.reason, "unowned");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn judge_scoped(
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

/// The check-path answer for `path`, before edit scope and lends.
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

/// A DONE path outside the sender's claimed lanes. `lane` is `None` when the
/// path is not a repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stray {
    /// The path as the client sent it.
    pub path: String,
    /// The lane that contains the path, when it has one.
    pub lane: Option<LaneName>,
}

/// The ALERT text for `strays`: at most 50 paths, one per line, then "and N
/// more", and never more than a body holds. Each path is the client's own
/// text, so it is quoted with Rust's string escapes: a newline or other
/// control character in it cannot start a line of the hub's text.
///
/// # Examples
///
/// ```
/// use agmen_core::{Stray, stray_body};
/// let item = "B-1".parse()?;
/// let strays = [Stray {
///     path: "ui/x.ts".to_owned(),
///     lane: Some("L-UI".parse()?),
/// }];
/// let body = stray_body("andy/sol", &item, &strays);
/// assert!(body.contains("ui/x.ts"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn stray_body(handle: &str, item: &ItemKey, strays: &[Stray]) -> String {
    stray_body_with(handle, item, strays, |stray| {
        stray
            .lane
            .as_ref()
            .map_or_else(|| NOT_A_REPO_PATH.to_owned(), ToString::to_string)
    })
}

/// The bounded renderer shared by lane diagnostics and role-scope diagnostics.
///
/// `label` names why each path is listed. The same length cap as
/// [`stray_body`] applies.
///
/// # Examples
///
/// ```
/// use agmen_core::{Stray, stray_body_with};
/// let item = "B-1".parse()?;
/// let strays = [Stray { path: "ui/x.ts".to_owned(), lane: None }];
/// let body = stray_body_with("andy/sol", &item, &strays, |_| "outside edit scope".to_owned());
/// assert!(body.contains("outside edit scope"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use]
pub fn stray_body_with(
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

/// Why replacing a project's lanes was refused or could not be stored.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReplaceError {
    /// The lane list failed the checks that run before the overlap check.
    #[error(transparent)]
    Spec(#[from] LaneSpecError),
    /// Two lanes match one path.
    #[error("{0}")]
    Overlapping(String),
    /// The overlap check could not build or search the globs.
    #[error("{0}")]
    TooComplex(String),
    /// A lane names an owner that is not a live identity of the project.
    #[error("{0}")]
    UnknownOwner(String),
    /// The caller is not a human or a coordinator.
    #[error("only humans and coordinators change lanes")]
    Forbidden,
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl<S: Store> Core<S> {
    /// Whether `identity` may edit each path, in the order asked.
    ///
    /// `Ok(None)` means the identity is missing or revoked. More than
    /// [`MAX_CHECK_PATHS`] paths is the caller's check: this reads every path
    /// it is given.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError`] when the store fails or a stored glob, kind, or
    /// edit scope does not compile.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use agmen_core::{Core, CoreError, Store};
    /// use agmen_proto::{IdentityId, ProjectId};
    ///
    /// async fn check<S: Store>(
    ///     core: &Core<S>,
    ///     project: &ProjectId,
    ///     identity: &IdentityId,
    /// ) -> Result<(), CoreError> {
    ///     let checks = core.check_path(project, identity, vec!["ui/x.rs".to_owned()]).await?;
    ///     let _ = checks;
    ///     Ok(())
    /// }
    /// ```
    pub async fn check_path(
        &self,
        project: &ProjectId,
        identity: &IdentityId,
        paths: Vec<String>,
    ) -> Result<Option<Vec<PathCheck>>, CoreError> {
        let mut reader = self.store().reader();
        let Some((kind, edits)) = reader.live_edit_scope(project, identity).await? else {
            return Ok(None);
        };
        let edits = if kind == IdentityKind::Listener {
            EditRule::None
        } else {
            EditRule::from_stored(edits)?
        };
        let lends = Lends::compile(reader.lends_to(project, identity).await?)?;
        let lanes = Lanes::compile(reader.lane_globs(project).await?)?;
        let holders = reader.lane_holders(project).await?;
        Ok(Some(judge_paths(
            &lanes, &holders, identity, &edits, &lends, paths,
        )))
    }

    /// Replaces `project`'s lanes with `specs` and returns the stored list.
    ///
    /// Humans and coordinators may call it. The overlap check runs before the
    /// transaction. An unknown owner drops the transaction without writing.
    ///
    /// # Errors
    ///
    /// Returns [`ReplaceError`] when `by` is not a human or coordinator, a
    /// spec or an overlap check fails, an owner is not live, or the store
    /// fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use agmen_core::{Core, ReplaceError, Store};
    /// use agmen_proto::{IdentityKind, ProjectId};
    ///
    /// async fn replace<S: Store>(
    ///     core: &Core<S>,
    ///     project: &ProjectId,
    /// ) -> Result<(), ReplaceError> {
    ///     let list = core.replace_lanes(project, IdentityKind::Coordinator, Vec::new()).await?;
    ///     let _ = list.version();
    ///     Ok(())
    /// }
    /// ```
    pub async fn replace_lanes(
        &self,
        project: &ProjectId,
        by: IdentityKind,
        specs: Vec<LaneSpec>,
    ) -> Result<LaneList, ReplaceError> {
        if !matches!(by, IdentityKind::Human | IdentityKind::Coordinator) {
            return Err(ReplaceError::Forbidden);
        }
        check_lane_specs(&specs)?;
        if let Some(overlap) = overlap_of(specs.clone()).await? {
            return Err(ReplaceError::Overlapping(overlap_message(&overlap)));
        }
        let mut tx = self.store().begin().await?;
        let resolved = resolve_owners(&mut tx, project, &specs).await?;
        tx.replace_lanes(project, &resolved).await?;
        tx.commit().await?;
        Ok(self.list_lanes(project).await?)
    }

    /// The project's lanes as the list route shows them, ordered by name.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the rows cannot be read.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use agmen_core::{Core, Store, StoreError};
    /// use agmen_proto::ProjectId;
    ///
    /// async fn version<S: Store>(core: &Core<S>, project: &ProjectId) -> Result<i64, StoreError> {
    ///     Ok(core.list_lanes(project).await?.version())
    /// }
    /// ```
    pub async fn list_lanes(&self, project: &ProjectId) -> Result<LaneList, StoreError> {
        let mut reader = self.store().reader();
        Ok(LaneList::from_rows(reader.lane_list(project).await?))
    }
}

fn judge_paths(
    lanes: &Lanes,
    holders: &[LaneHolder],
    identity: &IdentityId,
    edits: &EditRule,
    lends: &Lends,
    paths: Vec<String>,
) -> Vec<PathCheck> {
    paths
        .into_iter()
        .map(|path| judge_scoped(lanes, holders, identity, edits, lends, path))
        .collect()
}

async fn overlap_of(specs: Vec<LaneSpec>) -> Result<Option<Overlap>, ReplaceError> {
    let joined = tokio::task::spawn_blocking(move || find_overlap(&specs))
        .await
        .map_err(|err| ReplaceError::Store(StoreError::Backend(Box::new(err))))?;
    joined.map_err(|err| ReplaceError::TooComplex(err.to_string()))
}

fn overlap_message(overlap: &Overlap) -> String {
    format!(
        "{} glob {:?} and {} glob {:?} match a common path",
        overlap.lanes.0, overlap.globs.0, overlap.lanes.1, overlap.globs.1
    )
}

async fn resolve_owners(
    tx: &mut impl StoreTx,
    project: &ProjectId,
    specs: &[LaneSpec],
) -> Result<Vec<ResolvedLane>, ReplaceError> {
    if specs.len() > MAX_LANES {
        return Err(spec_err(
            "too_many_lanes",
            format!("a project has at most {MAX_LANES} lanes"),
        )
        .into());
    }
    let mut resolved = Vec::with_capacity(specs.len());
    for spec in specs {
        let owner = match &spec.owner {
            Some(handle) => Some(live_owner(tx, project, &spec.name, handle).await?),
            None => None,
        };
        resolved.push(ResolvedLane {
            name: spec.name.clone(),
            globs: spec.globs.clone(),
            owner,
        });
    }
    Ok(resolved)
}

async fn live_owner(
    tx: &mut impl StoreTx,
    project: &ProjectId,
    lane: &LaneName,
    handle: &str,
) -> Result<IdentityId, ReplaceError> {
    tx.resolve_owner(project, handle).await?.ok_or_else(|| {
        ReplaceError::UnknownOwner(format!(
            "{lane}'s owner is not a live identity of this project"
        ))
    })
}

#[cfg(test)]
#[path = "lanes_tests.rs"]
mod tests;
