//! Agent discovery and setup in a trusted, joined worktree.
//!
//! Cursor and Gemini CLI MCP formats checked 2026-10-28:
//!
//! - Cursor (<https://cursor.com/docs/mcp>, checked 2026-10-28; the older
//!   <https://docs.cursor.com/context/model-context-protocol> redirects to
//!   <https://cursor.com/docs>): project config is `.cursor/mcp.json` and user
//!   config is `~/.cursor/mcp.json`. The top-level key is `mcpServers`; a
//!   stdio entry has `type` set to `"stdio"` (marked required in the STDIO
//!   field table), `command`, and optional `args`, `env` and `envFile`. The
//!   page documents no `cwd` and does not say where a server process starts,
//!   but its "Config interpolation" section
//!   (<https://cursor.com/docs/mcp#config-interpolation>, checked 2026-10-28)
//!   documents variable resolution in `command`, `args`, `env`, `url` and
//!   `headers`, with `${workspaceFolder}` expanding to the project root (the
//!   folder that contains `.cursor/mcp.json`), so setup writes `"type":
//!   "stdio"` and passes `["mcp", "--worktree", "${workspaceFolder}"]` in
//!   `args`.
//! - Gemini CLI (<https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md>
//!   and <https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md>):
//!   project config is `.gemini/settings.json` in the project root and user
//!   config is `~/.gemini/settings.json`. The top-level key is `mcpServers`
//!   alongside other settings; a stdio entry has `command`, optional `args`,
//!   `env` and `cwd` ("working directory in which to start the server"). The
//!   docs give no default for `cwd`, so setup sets `cwd` to the canonical
//!   worktree root.

mod claude;
mod codex;
mod json;
mod opencode;

use std::fmt;
use std::io::{BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};

use agmen_client::IdentityFile;
use anyhow::{Context as _, Result, bail};
use clap::ValueEnum;

use crate::{client::blocking, git};

/// The agents whose setup formats this version supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum AgentKind {
    Claude,
    Codex,
    Opencode,
    Cursor,
    Gemini,
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Claude => f.write_str("Claude Code"),
            Self::Codex => f.write_str("Codex"),
            Self::Opencode => f.write_str("OpenCode"),
            Self::Cursor => f.write_str("Cursor"),
            Self::Gemini => f.write_str("Gemini CLI"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plugin {
    Install,
    Decline,
}

/// Ask only when both streams are terminals; automation defaults to installing.
pub(crate) fn plugin_choice(no_plugin: bool) -> Result<Plugin> {
    if no_plugin {
        return Ok(Plugin::Decline);
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Ok(Plugin::Install);
    }
    print!(
        "{}",
        crate::output::clean(
            "Install the agmen plugin for Claude Code (wake-ups and the lane guard)? [Y/n] "
        )
    );
    std::io::stdout()
        .flush()
        .context("printing the plugin prompt")?;
    let mut answer = Vec::new();
    std::io::stdin()
        .lock()
        .take(1024)
        .read_until(b'\n', &mut answer)
        .context("reading the plugin choice")?;
    Ok(
        if matches!(
            String::from_utf8_lossy(&answer)
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "n" | "no"
        ) {
            Plugin::Decline
        } else {
            Plugin::Install
        },
    )
}

#[derive(Debug)]
pub(crate) struct AgentEnv {
    path: Vec<PathBuf>,
    home: PathBuf,
    codex_home: PathBuf,
}

impl AgentEnv {
    pub(crate) fn from_process() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
            .context("HOME or USERPROFILE must name your home directory")?;
        let home = PathBuf::from(home);
        let codex_home = std::env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map_or_else(|| home.join(".codex"), PathBuf::from);
        Ok(Self {
            // A relative or empty entry means the current directory, which can
            // be a repository that ships its own `claude`; never run that.
            path: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .filter(|dir| dir.is_absolute())
                .collect(),
            home,
            codex_home,
        })
    }

    fn find_program(&self, name: &str) -> Option<PathBuf> {
        let names = [
            Some(name.to_owned()),
            cfg!(windows).then(|| format!("{name}.exe")),
        ];
        self.path
            .iter()
            .flat_map(|dir| names.iter().flatten().map(move |name| dir.join(name)))
            .find(|path| {
                // Missing or inaccessible candidates cannot be used as programs.
                std::fs::metadata(path).is_ok_and(|metadata| {
                    if !metadata.is_file() {
                        return false;
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        metadata.permissions().mode() & 0o111 != 0
                    }
                    #[cfg(not(unix))]
                    true
                })
            })
    }

    fn detected(&self, agent: AgentKind) -> bool {
        match agent {
            AgentKind::Claude => {
                self.find_program("claude").is_some() || self.home.join(".claude").is_dir()
            }
            AgentKind::Codex => self.find_program("codex").is_some() || self.codex_home.is_dir(),
            AgentKind::Opencode => {
                self.find_program("opencode").is_some()
                    || self.home.join(".config/opencode").is_dir()
            }
            AgentKind::Cursor => {
                self.find_program("cursor").is_some()
                    || self.find_program("cursor-agent").is_some()
                    || self.home.join(".cursor").is_dir()
            }
            AgentKind::Gemini => {
                self.find_program("gemini").is_some() || self.home.join(".gemini").is_dir()
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Change {
    pub(crate) agent: AgentKind,
    pub(crate) outcome: Outcome,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Configured(String),
    Unchanged,
    Skipped(String),
    Failed(String),
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.agent)?;
        match &self.outcome {
            Outcome::Configured(text) => f.write_str(text),
            Outcome::Unchanged => f.write_str("already set up"),
            Outcome::Skipped(text) => write!(f, "skipped: {text}"),
            Outcome::Failed(text) => write!(f, "failed: {text}"),
        }
    }
}

pub(crate) async fn run(
    root: &Path,
    identity: &IdentityFile,
    only: Option<AgentKind>,
    plugin: Plugin,
) -> Result<Vec<Change>> {
    let env = blocking(AgentEnv::from_process).await?;
    let mut changes = Vec::new();
    for agent in [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Opencode,
        AgentKind::Cursor,
        AgentKind::Gemini,
    ] {
        if only.is_some_and(|only| only != agent) || !env.detected(agent) {
            continue;
        }
        let outcome = match agent {
            AgentKind::Claude => claude::run(root, &env, plugin).await,
            AgentKind::Codex => codex::run(root, identity, &env).await,
            AgentKind::Opencode => opencode::run(root).await,
            AgentKind::Cursor => {
                let entry = serde_json::json!({
                    "type": "stdio",
                    "command": "agmen",
                    "args": ["mcp", "--worktree", "${workspaceFolder}"],
                });
                write_worktree_file(root, ".cursor/mcp.json", |existing| {
                    json::merge_mcp_servers(existing, &entry, ".cursor/mcp.json")
                })
                .await
            }
            AgentKind::Gemini => gemini_setup(root).await,
        };
        changes.push(Change {
            agent,
            outcome: outcome.unwrap_or_else(|err| Outcome::Failed(format!("{err:#}"))),
        });
    }
    // Callers clean each summary for one terminal line, so print this
    // static notice separately after the config writes have finished.
    if changes.iter().any(|change| {
        change.agent != AgentKind::Claude
            && matches!(change.outcome, Outcome::Configured(_) | Outcome::Unchanged)
    }) {
        println!(
            "lane guard: only the Claude Code plugin blocks edits outside your lanes; other agents should call check_path before editing"
        );
    }
    Ok(changes)
}

async fn gemini_setup(root: &Path) -> Result<Outcome> {
    let to_resolve = root.to_owned();
    let canonical = blocking(move || {
        std::fs::canonicalize(&to_resolve)
            .with_context(|| format!("resolving {}", to_resolve.display()))
    })
    .await?;
    let cwd = canonical
        .to_str()
        .context("the worktree path is not UTF-8")?;
    let entry = serde_json::json!({
        "command": "agmen",
        "args": ["mcp"],
        "cwd": cwd,
    });
    write_worktree_file(root, ".gemini/settings.json", |existing| {
        json::merge_mcp_servers(existing, &entry, ".gemini/settings.json")
    })
    .await
}

#[derive(Debug)]
pub(crate) struct Merged {
    pub(crate) text: String,
    pub(crate) changed: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SetupError {
    #[error("{} cannot be read: {why}", file.display())]
    Unreadable { file: PathBuf, why: String },
}

const MAX_JSON_BYTES: u64 = 1024 * 1024;

/// Never replace a tracked file or a file the merger cannot understand.
pub(crate) async fn write_worktree_file(
    root: &Path,
    rel: &str,
    merge: impl FnOnce(Option<&str>) -> Result<Merged, SetupError>,
) -> Result<Outcome> {
    if git::is_tracked(root, rel).await? {
        return Ok(Outcome::Skipped(format!(
            "{rel} is tracked by git; add the agmen entry by hand (docs/mcp-setup.md)"
        )));
    }
    let parent = Path::new(rel)
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| root.join(dir));
    let path = root.join(rel);
    let check_parent = parent.clone();
    let existing = blocking(move || {
        if let Some(dir) = &check_parent
            && !worktree_dir_exists(dir)?
        {
            return Ok(None);
        }
        read_config(&path)
    })
    .await;
    let existing = match existing {
        Ok(text) => text,
        Err(err) => return Ok(Outcome::Skipped(format!("{err:#}"))),
    };
    let merged = match merge(existing.as_deref()) {
        Ok(merged) => merged,
        Err(SetupError::Unreadable { why, .. }) => {
            return Ok(Outcome::Skipped(
                SetupError::Unreadable {
                    file: rel.into(),
                    why,
                }
                .to_string(),
            ));
        }
    };
    let exclude = git::exclude_file(root).await?;
    let path = root.join(rel);
    let rel = rel.to_owned();
    blocking(move || {
        if merged.changed
            && let Some(dir) = &parent
        {
            create_worktree_dir(dir)?;
        }
        git::add_exclude(&exclude, &rel)?;
        if merged.changed {
            write_atomically(&path, merged.text.as_bytes())?;
            Ok(Outcome::Configured(format!("wrote {rel}")))
        } else {
            Ok(Outcome::Unchanged)
        }
    })
    .await
}

fn worktree_dir_exists(dir: &Path) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("reading {}", dir.display())),
    };
    if metadata.file_type().is_symlink() {
        bail!("{} is a symbolic link", dir.display());
    }
    if !metadata.is_dir() {
        bail!("{} is not a folder", dir.display());
    }
    Ok(true)
}

fn create_worktree_dir(dir: &Path) -> Result<()> {
    if worktree_dir_exists(dir)? {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o755);
    builder
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("setting permissions on {}", dir.display()))?;
    }
    Ok(())
}

/// Writes `bytes` to a new file beside `path` and renames it into place, so
/// a crash leaves the old file or the new one, never half of either, and a
/// hard link to the old file keeps its content. The old file's mode carries
/// over.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("the config path has no file name")?;
    let tmp = path.with_file_name(format!(".{name}.agmen-{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        if let Ok(old) = std::fs::metadata(path) {
            file.set_permissions(old.permissions())?;
        }
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(err) = result {
        // The temporary file is ours; the error being returned is the one
        // that matters if removing it also fails.
        drop(std::fs::remove_file(&tmp));
        return Err(err).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

fn read_config(path: &Path) -> Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_JSON_BYTES + 1).read_to_string(&mut text))
        .with_context(|| format!("reading {}", path.display()))?;
    if text.len() as u64 > MAX_JSON_BYTES {
        bail!("{} exceeds the 1 MiB limit", path.display());
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_codex_and_opencode() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let env = AgentEnv {
            path: vec![bin.clone()],
            home: dir.path().to_owned(),
            codex_home: dir.path().join(".codex"),
        };
        assert!(!env.detected(AgentKind::Codex));
        assert!(!env.detected(AgentKind::Opencode));
        std::fs::create_dir(&env.codex_home).unwrap();
        assert!(env.detected(AgentKind::Codex));
        assert!(!env.detected(AgentKind::Opencode));
        std::fs::remove_dir(&env.codex_home).unwrap();
        std::fs::create_dir_all(env.home.join(".config/opencode")).unwrap();
        assert!(env.detected(AgentKind::Opencode));
        assert!(!env.detected(AgentKind::Codex));
        std::fs::remove_dir_all(env.home.join(".config")).unwrap();
        for (name, agent) in [
            ("codex", AgentKind::Codex),
            ("opencode", AgentKind::Opencode),
        ] {
            let program = bin.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            });
            std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert!(!env.detected(agent));
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            assert!(env.detected(agent));
            std::fs::remove_file(program).unwrap();
        }
    }

    #[test]
    fn detects_agents_by_program_or_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let env = AgentEnv {
            path: vec![bin.clone()],
            home: dir.path().to_owned(),
            codex_home: dir.path().join(".codex"),
        };
        assert!(!env.detected(AgentKind::Claude));
        let program = bin.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert!(!env.detected(AgentKind::Claude));
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(env.detected(AgentKind::Claude));
        std::fs::remove_file(&program).unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        assert!(env.detected(AgentKind::Claude));
    }

    #[test]
    fn detects_cursor_and_gemini() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let env = AgentEnv {
            path: vec![bin.clone()],
            home: dir.path().to_owned(),
            codex_home: dir.path().join(".codex"),
        };
        assert!(!env.detected(AgentKind::Cursor));
        assert!(!env.detected(AgentKind::Gemini));
        std::fs::create_dir(env.home.join(".cursor")).unwrap();
        assert!(env.detected(AgentKind::Cursor));
        assert!(!env.detected(AgentKind::Gemini));
        std::fs::remove_dir(env.home.join(".cursor")).unwrap();
        std::fs::create_dir(env.home.join(".gemini")).unwrap();
        assert!(env.detected(AgentKind::Gemini));
        assert!(!env.detected(AgentKind::Cursor));
        std::fs::remove_dir(env.home.join(".gemini")).unwrap();
        for (name, agent) in [
            ("cursor", AgentKind::Cursor),
            ("cursor-agent", AgentKind::Cursor),
            ("gemini", AgentKind::Gemini),
        ] {
            let program = bin.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            });
            std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert!(!env.detected(agent));
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            assert!(env.detected(agent));
            std::fs::remove_file(program).unwrap();
        }
    }
}
