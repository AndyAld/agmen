//! `agmen join` and `agmen leave`: a worktree's identity.
//!
//! `join` mints `<your handle>/<name>` in a project you join with an invite
//! or already belong to (ruling R55), stores its token, writes
//! `.agmen/identity.json` at the worktree root (rulings R57 and the Task 11
//! file format), adds `.agmen/` to the worktree's `info/exclude`, and then
//! checks that the file passes the trust test every command applies.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;

use agmen_client::{Broker, IDENTITY_DIR, IDENTITY_FILE, IdentityFile, TokenKey, find_identity};
use agmen_proto::IdentityId;
use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{Human, blocking, delete_token, human, refusal, send, set_token};
use crate::git;
use crate::joined::joined;
use crate::output::{Fail, clean};

/// The longest name after `<handle>/` (the hub's rule).
const MAX_NAME: usize = 32;

/// The kind of identity to mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    /// A coding agent.
    Agent,
    /// A coordinator; needs the coordinator or admin role.
    Coordinator,
}

#[derive(Debug, Args)]
#[command(group = clap::ArgGroup::new("how").required(true).args(["invite", "project", "stdin"]))]
pub(crate) struct JoinArgs {
    /// The invite code from agmen invite.
    invite: Option<String>,
    /// Read the invite from stdin, at most 8 KiB within 5 seconds.
    #[arg(long)]
    stdin: bool,
    /// A project you already belong to, by id (agmen whoami lists them).
    #[arg(long, value_name = "ID")]
    project: Option<String>,
    /// The name after your handle and a slash; defaults to the worktree
    /// folder's name.
    #[arg(long, value_name = "NAME")]
    handle: Option<String>,
    /// The kind of identity.
    #[arg(long, value_enum, default_value = "agent")]
    kind: Kind,
    /// Resolve a local role; its kind determines the identity kind.
    #[arg(long, value_name = "NAME", conflicts_with = "kind")]
    role: Option<String>,
}

impl JoinArgs {
    async fn with_stdin(mut self) -> Result<Self> {
        if self.stdin {
            self.invite = Some(read_stdin_invite().await?);
        }
        Ok(self)
    }

    async fn permissions(&self, root: &Path) -> Result<(Kind, Option<agmen_proto::RoleSpec>)> {
        let Some(name) = &self.role else {
            return Ok((self.kind, None));
        };
        let catalog = super::role::catalog(Some(root.to_owned())).await?;
        let role = super::role::find(&catalog, name)?;
        let kind = match role.kind {
            agmen_roles::Kind::Agent => Kind::Agent,
            agmen_roles::Kind::Coordinator => Kind::Coordinator,
            agmen_roles::Kind::Listener => {
                return Err(Fail::usage("listeners are started by agmend, not agmen join").into());
            }
        };
        if !role.stripped.is_empty() {
            eprintln!(
                "role ceiling stripped: {}",
                clean(&role.stripped.join(", "))
            );
        }
        Ok((
            kind,
            Some(agmen_proto::RoleSpec {
                role: role.role.name.parse().context("the role's name")?,
                verbs: role.verbs.clone(),
                edits: role.edits.clone().into(),
                role_sha256: role.sha256.parse().context("the resolved role's SHA-256")?,
            }),
        ))
    }
}

/// `POST /v1/invites/{code}/accept`'s answer.
#[derive(Debug, Deserialize)]
struct Accepted {
    project: String,
}

#[derive(Debug, Serialize)]
struct Mint<'a> {
    handle: &'a str,
    project: &'a str,
    kind: Kind,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    role: Option<agmen_proto::RoleSpec>,
}

impl Mint<'_> {
    async fn send(&self, human: &Human) -> Result<Minted> {
        let handle = self.handle;
        match human.api.post(&["identities"], self).await {
            Ok(minted) => Ok(minted),
            Err(err) => Err(match refusal(&err) {
                Some(r) if r.code == "bad_handle" || (r.status == 422 && r.code == "bad_role") => {
                    Fail::usage(clean(&r.description)).into()
                }
                Some(r) if r.code == "handle_taken" => {
                    anyhow::anyhow!("{handle} is taken in this project; pick another with --handle")
                }
                Some(r) if r.code == "coordinator_role_required" => anyhow::anyhow!(
                    "a coordinator identity needs the coordinator or admin role in the project"
                ),
                _ => err.context("minting the identity failed"),
            }),
        }
    }
}

/// `POST /v1/identities`'s answer.
#[derive(Deserialize)]
struct Minted {
    id: IdentityId,
    handle: String,
    tenant: String,
    project: String,
    token: String,
    verbs: Option<BTreeSet<String>>,
}

impl Minted {
    fn report_verb_changes(&self, role: Option<&agmen_proto::RoleSpec>) -> Result<()> {
        let Some(role) = role else {
            return Ok(());
        };
        let granted = self
            .verbs
            .as_ref()
            .context("the hub omitted the role's capped verbs")?;
        if granted != &role.verbs {
            let removed = role
                .verbs
                .difference(granted)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            let added = granted
                .difference(&role.verbs)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            eprintln!(
                "hub verb changes: removed [{}]; added [{}]",
                clean(&removed),
                clean(&added)
            );
        }
        Ok(())
    }
}

/// `agmen join`.
pub(crate) async fn join(args: JoinArgs) -> Result<()> {
    let args = args.with_stdin().await?;
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let root = git::toplevel(&cwd).await?;
    let (kind, role) = args.permissions(&root).await?;
    let file = root.join(IDENTITY_DIR).join(IDENTITY_FILE);
    let existing = file.clone();
    let present = blocking(move || {
        if std::fs::symlink_metadata(&existing).is_err() {
            return Ok(None);
        }
        let who = std::fs::read(&existing)
            .ok()
            .and_then(|raw| serde_json::from_slice::<IdentityFile>(&raw).ok())
            .map_or_else(|| "an unreadable identity".to_owned(), |id| id.handle);
        Ok(Some(who))
    })
    .await?;
    if let Some(who) = present {
        bail!(
            "this worktree already joined as {who} ({}); run agmen leave first, or delete the \
             file if it came with the repository",
            file.display()
        );
    }
    let human = human().await?;
    let project = match (&args.invite, args.project) {
        (Some(code), _) => accept(&human, code).await?,
        (None, Some(project)) => project,
        (None, None) => return Err(Fail::usage("give an invite code or --project <id>").into()),
    };
    let me = human
        .api
        .me()
        .await
        .context("reading who is signed in failed")?;
    if !me.projects.iter().any(|p| p.project == project) {
        bail!("you are not a member of project {project}; ask its admin for an invite");
    }
    let broker: Broker = me
        .broker_url
        .parse()
        .with_context(|| format!("the hub gave the broker address {:?}", me.broker_url))?;
    let name = match &args.handle {
        Some(name) => name
            .strip_prefix(&format!("{}/", me.handle))
            .unwrap_or(name)
            .to_owned(),
        None => default_name(&root),
    };
    let handle = format!("{}/{name}", me.handle);
    let request = Mint {
        handle: &handle,
        project: &project,
        kind,
        role,
    };
    let minted = request.send(&human).await?;
    minted.report_verb_changes(request.role.as_ref())?;
    let identity = IdentityFile {
        tenant: minted.tenant.parse().context("the hub's tenant id")?,
        project: minted.project.parse().context("the hub's project id")?,
        identity_id: minted.id,
        handle: minted.handle,
        owner_id: me.id,
        hub_url: human.login.hub_url.clone(),
        broker_url: broker,
        role: request.role.as_ref().map(|role| role.role.clone()),
        role_sha256: request.role.as_ref().map(|role| role.role_sha256.clone()),
        verbs: request.role.as_ref().and(minted.verbs),
    };
    let key = TokenKey::new(&identity.hub_url, &identity.identity_id).context("the hub URL")?;
    // The exclude goes first: once the file exists, every command applies
    // the trust test, which needs git to ignore it.
    let exclude = git::exclude_file(&root).await?;
    blocking(move || git::add_exclude(&exclude, ".agmen/")).await?;
    set_token(key, minted.token).await?;
    write_identity(&root, &identity).await?;
    let check_root = root.clone();
    let trusted = blocking(move || {
        find_identity(&check_root).context("the new identity file fails the trust test")
    })
    .await?;
    if trusted.is_none_or(|joined| joined.identity != identity) {
        bail!(
            "agmen cannot find the identity file it just wrote in {}",
            root.display()
        );
    }
    let (handle, id) = (clean(&identity.handle), &identity.identity_id);
    println!("joined project {} as {handle} ({id})", identity.project);
    Ok(())
}

async fn read_stdin_invite() -> Result<String> {
    use std::io::Read as _;
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("invite-stdin".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = std::io::stdin()
                .take(8193)
                .read_to_end(&mut bytes)
                .context("reading invite from stdin")
                .and_then(|_| stdin_invite(&bytes));
            drop(send.send(result));
        })
        .context("starting invite reader")?;
    let invite = tokio::time::timeout(std::time::Duration::from_secs(5), receive)
        .await
        .context("reading invite from stdin exceeded 5 seconds")?
        .context("invite reader stopped")??;
    Ok(invite)
}

fn stdin_invite(bytes: &[u8]) -> Result<String> {
    anyhow::ensure!(bytes.len() <= 8192, "stdin invite exceeds 8 KiB");
    let text = std::str::from_utf8(bytes)
        .context("stdin invite must be UTF-8")?
        .trim();
    anyhow::ensure!(
        !text.is_empty() && !text.chars().any(char::is_whitespace),
        "give one invite code on stdin"
    );
    Ok(text.to_owned())
}

/// Accepts invite `code` and returns its project. One already accepted by
/// someone else, expired or unknown fails with what the hub said.
async fn accept(human: &Human, code: &str) -> Result<String> {
    let request = human
        .api
        .request(Method::POST, &["invites", code, "accept"]);
    match send::<Accepted>(request).await {
        Ok(accepted) => Ok(accepted.project),
        Err(err) => Err(match refusal(&err).map(|r| r.code.as_str()) {
            Some("invite_used") => anyhow::anyhow!("the invite was already used"),
            Some("invite_expired") => anyhow::anyhow!("the invite has expired; ask for a new one"),
            Some("not_found") => anyhow::anyhow!("no such invite; check the code"),
            Some("already_member") => anyhow::anyhow!(
                "you already belong to the invite's project: run agmen join --project <id> \
                 (agmen whoami lists your projects)"
            ),
            _ => err.context("accepting the invite failed"),
        }),
    }
}

/// The worktree folder's name as a handle name: lowercase `[a-z0-9-]`,
/// starting with a letter or digit, at most 32 characters.
fn default_name(root: &Path) -> String {
    let folder = root
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let name: String = folder
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name: String = name
        .trim_start_matches('-')
        .chars()
        .take(MAX_NAME)
        .collect();
    if name.is_empty() {
        "agent".to_owned()
    } else {
        name
    }
}

/// Writes `<root>/.agmen/identity.json`, readable only by its owner.
async fn write_identity(root: &Path, identity: &IdentityFile) -> Result<()> {
    let dir = root.join(IDENTITY_DIR);
    let mut json = serde_json::to_vec_pretty(identity).context("encoding the identity file")?;
    json.push(b'\n');
    blocking(move || {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(IDENTITY_FILE);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(&json)
            .and_then(|()| file.sync_all())
            .with_context(|| format!("writing {}", path.display()))
    })
    .await
}

/// `agmen leave`.
pub(crate) async fn leave() -> Result<()> {
    // The identity's own token is not needed: the owner revokes it, so a
    // worktree whose token is gone can still leave.
    let joined = joined().await?;
    let human = human().await?;
    let identity = &joined.identity;
    if human.login.hub_url.origin() != identity.hub_url.origin() {
        bail!(
            "you are signed in at {}, but this worktree's identity is at {}; sign in there to \
             revoke it",
            human.login.hub_url,
            identity.hub_url
        );
    }
    let request = human.api.request(
        Method::DELETE,
        &["identities", identity.identity_id.as_str()],
    );
    match send::<()>(request).await {
        Ok(()) => {}
        Err(err) if refusal(&err).is_some_and(|r| r.status == 404) => eprintln!(
            "the hub knows no identity {} of yours; removing it here",
            identity.identity_id
        ),
        Err(err) => return Err(err.context("revoking the identity failed")),
    }
    // The file goes before the token: a file left without a token can still
    // be left again, a token left without a file is only clutter.
    let dir = joined.root.join(IDENTITY_DIR);
    blocking(move || {
        let path = dir.join(IDENTITY_FILE);
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        // The folder goes too when nothing else is in it.
        if std::fs::remove_dir(&dir).is_err() {
            tracing::debug!("{} is not empty; leaving it", dir.display());
        }
        Ok(())
    })
    .await?;
    let key = TokenKey::new(&identity.hub_url, &identity.identity_id).context("the hub URL")?;
    delete_token(key).await?;
    println!(
        "left: revoked {} ({})",
        clean(&identity.handle),
        identity.identity_id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_stdin_is_one_bounded_code() {
        assert_eq!(stdin_invite(b"test-invite\n").unwrap(), "test-invite");
        assert!(stdin_invite(b"").is_err());
        assert!(stdin_invite(b"one\ntwo").is_err());
        assert!(stdin_invite(&[b'a'; 8193]).is_err());
        assert!(stdin_invite(&[255]).is_err());
    }

    #[test]
    fn default_names() {
        assert_eq!(default_name(Path::new("/x/agmen")), "agmen");
        assert_eq!(default_name(Path::new("/x/My Repo_2")), "my-repo-2");
        assert_eq!(default_name(Path::new("/x/.hidden")), "hidden");
        assert_eq!(default_name(Path::new("/x/___")), "agent");
        assert_eq!(
            default_name(Path::new(&format!("/x/{}", "a".repeat(40)))).len(),
            32
        );
    }
}
