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

use std::path::{Component, Path, PathBuf};

use agmen_proto::{IdentityId, LaneName};
use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{Api, blocking, human, read_login, send};
use crate::joined::{Agent, agent};
use crate::output::{Fail, println_flush};

/// The most paths one check asks about (the hub's limit).
const MAX_CHECK_PATHS: usize = 500;
/// The largest lane file read.
const MAX_LANE_FILE: u64 = 1024 * 1024;

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
    let project = agent.identity.project.to_string();
    let list: LaneList = match args.action {
        None => agent.api()?.get(&["projects", &project, "lanes"]).await?,
        Some(LanesAction::Set { file }) => {
            let (content_type, body) = read_lane_file(file).await?;
            let request = setter(&agent)
                .await?
                .request(Method::PUT, &["projects", &project, "lanes"])
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body);
            send(request).await.context("replacing the lanes failed")?
        }
    };
    let mut out = vec![format!("lanes v{} in project {project}", list.v)];
    if list.lanes.is_empty() {
        out.push("no lanes: every path is unowned".to_owned());
    }
    for lane in list.lanes {
        let owner = lane
            .owner_handle
            .map_or_else(String::new, |o| format!(" (owner {o})"));
        out.push(format!("{}{owner}: {}", lane.name, lane.globs.join(" ")));
    }
    println_flush(&out.join("\n")).context("writing to stdout")
}

/// The file's content type and text: by extension, else JSON when it
/// parses as JSON, else TOML.
async fn read_lane_file(file: PathBuf) -> Result<(&'static str, String)> {
    blocking(move || {
        use std::io::Read as _;
        let mut text = String::new();
        std::fs::File::open(&file)
            .and_then(|f| f.take(MAX_LANE_FILE).read_to_string(&mut text))
            .with_context(|| format!("reading {}", file.display()))?;
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
        let lane = item
            .lane
            .map_or_else(String::new, |lane| format!(" [{lane}]"));
        let held = match (item.holder_handle, item.lease_until) {
            (Some(holder), Some(until)) => format!(", held by {holder} until {until}"),
            (Some(holder), None) => format!(", held by {holder}"),
            _ => String::new(),
        };
        let title = item
            .title
            .unwrap_or_else(|| "(security: title hidden)".to_owned());
        out.push(format!("{} {}{lane}{held}: {title}", item.key, item.state));
    }
    println_flush(&out.join("\n")).context("writing to stdout")
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
struct PathCheck {
    path: String,
    lane: Option<LaneName>,
    allowed: bool,
    reason: String,
}

/// `agmen check-path`.
pub(crate) async fn check_path(args: CheckPathArgs) -> Result<()> {
    if args.paths.len() > MAX_CHECK_PATHS {
        return Err(Fail::usage(format!("check at most {MAX_CHECK_PATHS} paths at once")).into());
    }
    let agent = agent().await?;
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let root = agent.root.clone();
    let real_root = blocking(move || {
        std::fs::canonicalize(&root).with_context(|| format!("resolving {}", root.display()))
    })
    .await?;
    let paths = args
        .paths
        .iter()
        .map(|path| {
            repo_path(&[&agent.root, &real_root], &cwd, path).unwrap_or_else(|| path.clone())
        })
        .collect();
    let project = agent.identity.project.to_string();
    let request = CheckRequest {
        identity: &agent.identity.identity_id,
        paths,
    };
    let mut answers: Vec<PathCheck> = agent
        .api()?
        .post(&["projects", &project, "check-path"], &request)
        .await
        .context("checking the paths failed")?;
    if answers.len() != args.paths.len() {
        bail!(
            "the hub answered {} entries for {} paths",
            answers.len(),
            args.paths.len()
        );
    }
    for (answer, path) in answers.iter_mut().zip(args.paths) {
        answer.path = path;
    }
    let text = if args.json {
        serde_json::to_string(&answers).context("encoding the answers")?
    } else {
        answers
            .iter()
            .map(|a| {
                let verdict = if a.allowed { "allowed" } else { "denied" };
                format!("{verdict} {}: {}", a.path, a.reason)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    println_flush(&text).context("writing to stdout")
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
