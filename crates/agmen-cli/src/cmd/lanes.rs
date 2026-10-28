//! `agmen lanes [set <file>]`, `agmen items` and `agmen check-path`: the
//! project's ledger over HTTP.
//!
//! `check-path` (rulings R16, R16a, R44 and R44c) turns each path into a
//! repository path (relative to the worktree root, `/` separated), asks the
//! hub, and prints one entry per input path in input order with `path`
//! exactly as given. A path outside the worktree goes to the hub as given,
//! which answers `not a repository path`. Reasons come from the hub, built
//! only from lane names, handles and fixed phrases. It exits 0 whenever the
//! check ran, allowed or not.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use agmen_proto::{IdentityId, LaneName};
use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{Api, blocking, human, read_login, send};
use crate::joined::{Agent, agent};
use crate::output::{Fail, clean, println_flush};

/// The most paths one check asks about (the hub's limit).
const MAX_CHECK_PATHS: usize = 500;
/// The largest lane file sent, in bytes.
const MAX_LANE_FILE: usize = 1024 * 1024;

#[derive(Debug, Args)]
pub(crate) struct LanesArgs {
    #[command(subcommand)]
    action: Option<LanesAction>,
}

#[derive(Debug, Subcommand)]
enum LanesAction {
    /// Replace the project's lanes from a TOML file of [[lanes]] tables or a
    /// JSON file with a lanes array; each lane has a name, globs and an
    /// optional owner.
    Set {
        /// The lane file, .toml or .json.
        file: PathBuf,
    },
}

/// One lane as the hub lists it.
#[derive(Debug, Deserialize)]
struct LaneView {
    name: String,
    globs: Vec<String>,
    owner_handle: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LaneList {
    v: i64,
    lanes: Vec<LaneView>,
}

/// `agmen lanes`.
pub(crate) async fn lanes(args: LanesArgs) -> Result<()> {
    let agent = agent().await?;
    let text = match args.action {
        None => lanes_text(&agent).await?,
        Some(LanesAction::Set { file }) => {
            let project = agent.identity.project.to_string();
            let (content_type, body) = read_lane_file(file).await?;
            let request = setter(&agent)
                .await?
                .request(Method::PUT, &["projects", &project, "lanes"])
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body);
            let list = send(request).await.context("replacing the lanes failed")?;
            lane_lines(&project, list)
        }
    };
    println_flush(&text).context("writing to stdout")
}

/// The project's lanes, one line each under a `lanes v<n> in project <p>`
/// line.
pub(crate) async fn lanes_text(agent: &Agent) -> Result<String> {
    let project = agent.identity.project.to_string();
    let list = agent.api()?.get(&["projects", &project, "lanes"]).await?;
    Ok(lane_lines(&project, list))
}

/// What `agmen lanes` prints for `list`, every hub-supplied string through
/// [`clean`].
fn lane_lines(project: &str, list: LaneList) -> String {
    let mut out = vec![format!("lanes v{} in project {project}", list.v)];
    if list.lanes.is_empty() {
        out.push("no lanes: every path is unowned".to_owned());
    }
    for lane in list.lanes {
        let owner = lane
            .owner_handle
            .map_or_else(String::new, |o| format!(" (owner {})", clean(&o)));
        let globs: Vec<_> = lane.globs.iter().map(|glob| clean(glob)).collect();
        out.push(format!("{}{owner}: {}", clean(&lane.name), globs.join(" ")));
    }
    out.join("\n")
}

/// The file's content type and text: by extension, else JSON when it
/// parses as JSON, else TOML.
async fn read_lane_file(file: PathBuf) -> Result<(&'static str, String)> {
    blocking(move || {
        let text = read_lane_text(&file)?;
        let kind = match file.extension().and_then(|ext| ext.to_str()) {
            Some("toml") => "application/toml",
            Some("json") => "application/json",
            _ if serde_json::from_str::<serde_json::Value>(&text).is_ok() => "application/json",
            _ => "application/toml",
        };
        Ok((kind, text))
    })
    .await
}

/// The whole of `file` as text. Reads at most one byte past
/// [`MAX_LANE_FILE`] and refuses a larger file: a file cut short can still
/// parse, and the PUT would then replace the lanes with part of them.
fn read_lane_text(file: &Path) -> Result<String> {
    use std::io::Read as _;
    let limit = u64::try_from(MAX_LANE_FILE + 1).unwrap_or(u64::MAX);
    let mut raw = Vec::new();
    std::fs::File::open(file)
        .and_then(|f| f.take(limit).read_to_end(&mut raw))
        .with_context(|| format!("reading {}", file.display()))?;
    if raw.len() > MAX_LANE_FILE {
        return Err(Fail::usage(format!(
            "{} is over {MAX_LANE_FILE} bytes, the largest lane file agmen sends",
            file.display()
        ))
        .into());
    }
    String::from_utf8(raw)
        .map_err(|_| Fail::usage(format!("{} is not UTF-8", file.display())).into())
}

/// Who replaces lanes: the signed-in person when they signed in at this
/// identity's hub (a member may), else the identity (a coordinator may).
async fn setter(agent: &Agent) -> Result<Api> {
    if let Some(login) = read_login().await?
        && login.hub_url.origin() == agent.identity.hub_url.origin()
    {
        return Ok(human().await?.api);
    }
    agent.api()
}

/// One item as the hub lists it.
#[derive(Debug, Deserialize)]
struct ItemView {
    key: String,
    title: Option<String>,
    lane: Option<String>,
    state: String,
    holder_handle: Option<String>,
    lease_until: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ItemList {
    items: Vec<ItemView>,
}

/// `agmen items`.
pub(crate) async fn items() -> Result<()> {
    let agent = agent().await?;
    let project = agent.identity.project.to_string();
    let list: ItemList = agent.api()?.get(&["projects", &project, "items"]).await?;
    let mut out = Vec::with_capacity(list.items.len().max(1));
    if list.items.is_empty() {
        out.push("no items".to_owned());
    }
    for item in list.items {
        out.push(item_line(&item));
    }
    println_flush(&out.join("\n")).context("writing to stdout")
}

/// One line of `agmen items`: `<key> <state> [<lane>], held by <holder>
/// until <lease>: <title>`, every field the hub gave through [`clean`].
fn item_line(item: &ItemView) -> String {
    let lane = item
        .lane
        .as_deref()
        .map_or_else(String::new, |lane| format!(" [{}]", clean(lane)));
    let held = match (&item.holder_handle, &item.lease_until) {
        (Some(holder), Some(until)) => {
            format!(", held by {} until {}", clean(holder), clean(until))
        }
        (Some(holder), None) => format!(", held by {}", clean(holder)),
        _ => String::new(),
    };
    let title = item
        .title
        .as_deref()
        .map_or(Cow::Borrowed("(security: title hidden)"), clean);
    format!(
        "{} {}{lane}{held}: {title}",
        clean(&item.key),
        clean(&item.state)
    )
}

#[derive(Debug, Args)]
pub(crate) struct CheckPathArgs {
    /// Print a JSON array of {path, lane, allowed, reason}.
    #[arg(long)]
    json: bool,
    /// The paths, absolute or relative to the working directory. Put --
    /// before them so a path starting with - stays a path.
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CheckRequest<'a> {
    identity: &'a IdentityId,
    paths: Vec<String>,
}

/// One answer, as the hub gives it and as `--json` prints it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PathCheck {
    path: String,
    lane: Option<LaneName>,
    allowed: bool,
    reason: String,
}

/// `agmen check-path`.
pub(crate) async fn check_path(args: CheckPathArgs) -> Result<()> {
    at_most_max_paths(&args.paths)?;
    let agent = agent().await?;
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let answers = check(&agent, &cwd, args.paths).await?;
    let text = if args.json {
        serde_json::to_string(&answers).context("encoding the answers")?
    } else {
        check_lines(&answers)
    };
    println_flush(&text).context("writing to stdout")
}

/// Asks the hub whether `agent` may edit each of `paths` (absolute, or
/// relative to `cwd`): one answer per path, in order, with `path` as given.
///
/// # Errors
///
/// A usage failure for more than [`MAX_CHECK_PATHS`] paths, the hub's
/// refusal, and an error when it answers a different number of entries.
pub(crate) async fn check(agent: &Agent, cwd: &Path, paths: Vec<String>) -> Result<Vec<PathCheck>> {
    at_most_max_paths(&paths)?;
    let root = agent.root.clone();
    let real_root = blocking(move || {
        std::fs::canonicalize(&root).with_context(|| format!("resolving {}", root.display()))
    })
    .await?;
    let placed = paths
        .iter()
        .map(|path| {
            repo_path(&[&agent.root, &real_root], cwd, path).unwrap_or_else(|| path.clone())
        })
        .collect();
    let project = agent.identity.project.to_string();
    let request = CheckRequest {
        identity: &agent.identity.identity_id,
        paths: placed,
    };
    let mut answers: Vec<PathCheck> = agent
        .api()?
        .post(&["projects", &project, "check-path"], &request)
        .await
        .context("checking the paths failed")?;
    if answers.len() != paths.len() {
        bail!(
            "the hub answered {} entries for {} paths",
            answers.len(),
            paths.len()
        );
    }
    for (answer, path) in answers.iter_mut().zip(paths) {
        answer.path = path;
    }
    Ok(answers)
}

/// A usage failure for more than [`MAX_CHECK_PATHS`] paths, the hub's
/// limit.
fn at_most_max_paths(paths: &[String]) -> Result<()> {
    if paths.len() > MAX_CHECK_PATHS {
        return Err(Fail::usage(format!("check at most {MAX_CHECK_PATHS} paths at once")).into());
    }
    Ok(())
}

/// `allowed <path>: <reason>` or `denied <path>: <reason>` per answer.
pub(crate) fn check_lines(answers: &[PathCheck]) -> String {
    answers
        .iter()
        .map(|a| {
            let verdict = if a.allowed { "allowed" } else { "denied" };
            format!("{verdict} {}: {}", clean(&a.path), clean(&a.reason))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `input` as a repository path: made absolute against `cwd`, `.` and `..`
/// resolved by name, and made relative to whichever of `roots` (the root
/// as git names it, and with every link followed) holds it. `None` when it
/// is outside them, is the root itself, or is not valid UTF-8 after all.
fn repo_path(roots: &[&Path], cwd: &Path, input: &str) -> Option<String> {
    let given = Path::new(input);
    let absolute = if given.is_absolute() {
        given.to_path_buf()
    } else {
        cwd.join(given)
    };
    let mut normal = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normal.pop() {
                    return None;
                }
            }
            other => normal.push(other),
        }
    }
    let relative = roots
        .iter()
        .find_map(|root| normal.strip_prefix(root).ok())?;
    let parts: Option<Vec<&str>> = relative
        .components()
        .map(|part| match part {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    let parts = parts?;
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_files_over_the_limit_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let lane = "[[lanes]]\nname = \"L-UI\"\nglobs = [\"/ui/\"]\n";
        let mut text = lane.repeat(MAX_LANE_FILE / lane.len());
        text.push_str(&"#".repeat(MAX_LANE_FILE - text.len()));
        let at_limit = dir.path().join("at.toml");
        std::fs::write(&at_limit, &text).unwrap();
        assert_eq!(read_lane_text(&at_limit).unwrap().len(), MAX_LANE_FILE);

        text.push('#');
        let over = dir.path().join("over.toml");
        std::fs::write(&over, &text).unwrap();
        let err = read_lane_text(&over).unwrap_err();
        let fail = err.downcast_ref::<Fail>().unwrap();
        assert_eq!(fail.code, crate::output::Code::Usage);
        assert!(fail.message.contains("1048576 bytes"), "{}", fail.message);
    }

    #[test]
    fn item_lines_drop_escape_sequences() {
        let item = ItemView {
            key: "B-7".to_owned(),
            title: Some("fix \u{1b}[2J\u{1b}]0;owned\u{7}the\r\nbuild".to_owned()),
            lane: Some("L-UI\u{1b}[0m".to_owned()),
            state: "claimed".to_owned(),
            holder_handle: Some("adam/\u{1b}[31mcoord".to_owned()),
            lease_until: None,
        };
        let line = item_line(&item);
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert_eq!(
            line,
            "B-7 claimed [L-UI[0m], held by adam/[31mcoord: fix [2J]0;ownedthebuild"
        );
        let hidden = ItemView {
            title: None,
            ..item
        };
        assert!(item_line(&hidden).ends_with(": (security: title hidden)"));
    }

    #[test]
    fn repo_paths() {
        let root = Path::new("/w/repo");
        let real = Path::new("/private/w/repo");
        let at = |cwd: &str, input: &str| repo_path(&[root, real], Path::new(cwd), input);
        assert_eq!(at("/w/repo", "src/a.rs").as_deref(), Some("src/a.rs"));
        assert_eq!(at("/w/repo", "-n.ts").as_deref(), Some("-n.ts"));
        assert_eq!(
            at("/w/repo/src", "../ui/./b.ts").as_deref(),
            Some("ui/b.ts")
        );
        assert_eq!(at("/w/repo", "/w/repo/ui/b.ts").as_deref(), Some("ui/b.ts"));
        assert_eq!(at("/w/repo", "/private/w/repo/x").as_deref(), Some("x"));
        assert_eq!(at("/w/repo", "../other/x"), None);
        assert_eq!(at("/w/repo", "/elsewhere/x.ts"), None);
        assert_eq!(at("/w/repo", "."), None);
        assert_eq!(at("/w/repo", "/w/repository/x"), None);
    }
}
