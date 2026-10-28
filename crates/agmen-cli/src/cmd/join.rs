//! `agmen join` and `agmen leave`: a worktree's identity.
//!
//! `join` mints `<your handle>/<name>` in a project you join with an invite
//! or already belong to (ruling R55), stores its token, writes
//! `.agmen/identity.json` at the worktree root (rulings R57 and the Task 11
//! file format), adds `.agmen/` to the worktree's `info/exclude`, and then
//! checks that the file passes the trust test every command applies.

use std::collections::BTreeSet;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::Path;

use agmen_client::{Broker, IDENTITY_DIR, IDENTITY_FILE, IdentityFile, TokenKey, find_identity};
use agmen_proto::IdentityId;
use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::client::{
    Human, blocking, delete_token, human, human_at, read_login, refusal, same_hub, send, set_token,
};
use crate::joined::joined;
use crate::output::{Fail, clean};
use crate::{git, setup};

/// The longest name after `<handle>/` (the hub's rule).
const MAX_NAME: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Invite {
    Link { hub: Url, code: String },
    Code(String),
}

pub(crate) fn parse_invite(text: &str) -> Result<Invite, Fail> {
    if text.len() > 8192 {
        return Err(Fail::usage("invite exceeds 8 KiB"));
    }
    let text = text.trim();
    let text = text
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(text);
    let is_code = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if is_code(text) {
        return Ok(Invite::Code(text.to_ascii_lowercase()));
    }
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Err(Fail::usage("give one invite link or a 64-hex invite code"));
    }
    let mut hub =
        Url::parse(text).map_err(|_| Fail::usage("give an invite link or a 64-hex invite code"))?;
    if hub.scheme() != "https"
        && !(hub.scheme() == "http"
            && matches!(hub.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    {
        return Err(Fail::usage(
            "invite links must be https, or http on localhost, 127.0.0.1 or [::1]",
        ));
    }
    if !hub.username().is_empty() || hub.password().is_some() {
        return Err(Fail::usage("invite links must not contain credentials"));
    }
    if hub.query().is_some() {
        return Err(Fail::usage("invite links must not contain a query"));
    }
    if hub.path_segments().and_then(Iterator::last) != Some("i") {
        return Err(Fail::usage("invite links must end in /i"));
    }
    let code = hub
        .fragment()
        .ok_or_else(|| Fail::usage("invite link is missing its invite code fragment"))?;
    if !is_code(code) {
        return Err(Fail::usage(
            "invite code fragment must contain 64 hex characters",
        ));
    }
    let code = code.to_ascii_lowercase();
    hub.set_fragment(None);
    hub.path_segments_mut()
        .map_err(|()| Fail::usage("invite link must name a hub"))?
        .pop()
        .push("");
    Ok(Invite::Link { hub, code })
}

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
#[command(group = clap::ArgGroup::new("how").args(["invite", "project", "stdin"]))]
pub(crate) struct JoinArgs {
    /// The invite link or code from agmen invite.
    invite: Option<String>,
    /// Read the invite from stdin, at most 8 KiB within 5 seconds.
    #[arg(long)]
    stdin: bool,
    /// Allow an invite link to switch your sign-in to another hub.
    #[arg(long)]
    yes: bool,
    /// The hub for a bare invite code.
    #[arg(long, env = "AGMEN_HUB", value_name = "URL")]
    hub: Option<Url>,
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
    /// Skip agent setup; run agmen setup later.
    #[arg(long)]
    no_setup: bool,
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
    name: String,
    repo_url: Option<agmen_proto::RepoUrl>,
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
    // Every local check runs here, before sign-in or any hub call: an invite
    // is single use, so a join that fails after accepting it costs the person
    // their link.
    let invite = args.invite.as_deref().map(parse_invite).transpose()?;
    let hub = match &invite {
        Some(Invite::Link { hub, .. }) => Some(hub.clone()),
        Some(Invite::Code(_)) => Some(match &args.hub {
            Some(hub) => hub.clone(),
            None => read_login().await?.map(|login| login.hub_url).ok_or_else(|| {
                Fail::usage("a bare invite code needs --hub <url> (or AGMEN_HUB); use an invite link instead")
            })?,
        }),
        None => None,
    };
    let root = match git::toplevel(&cwd).await {
        Ok(root) => Some(root),
        // An invite is accepted first and then points to the clone.
        Err(_) if invite.is_some() => None,
        Err(err) => return Err(err),
    };
    let local = match root {
        Some(root) => {
            check_not_joined(&root).await?;
            let permissions = args.permissions(&root).await?;
            Some((root, permissions))
        }
        None => None,
    };
    let (human, root, permissions, project, name) = if let (Some(invite), Some(hub)) = (invite, hub)
    {
        if matches!(invite, Invite::Link { .. }) {
            confirm_link_hub(&hub, &args).await?;
        }
        let (Invite::Link { code, .. } | Invite::Code(code)) = invite;
        let human = sign_in(&hub).await?;
        let accepted = accept(&human, &code).await?;
        let Some((root, permissions)) = local else {
            eprintln!(
                "joined project {} ({}). This folder is not a clone of its repository.",
                clean(&accepted.name),
                clean(&accepted.project)
            );
            if let Some(repo) = &accepted.repo_url {
                eprintln!("  git clone {}", clean(repo.as_str()));
            }
            bail!(
                "then, inside the clone: agmen join --project {}",
                clean(&accepted.project)
            );
        };
        (human, root, permissions, accepted.project, accepted.name)
    } else {
        let (root, permissions) = local.context("agmen join runs inside a git worktree")?;
        let human = human().await?;
        let (project, name) = member_project(&human, args.project.as_deref()).await?;
        (human, root, permissions, project, name)
    };
    let identity = mint_identity(&human, &root, &project, &args, permissions)
        .await
        .inspect_err(|_| {
            if args.invite.is_some() {
                eprintln!(
                    "the invite is used up; to finish in this folder: agmen join --project {}",
                    clean(&project)
                );
            }
        })?;
    println!(
        "joined project {} ({}) as {} ({}) at {}",
        clean(&name),
        identity.project,
        clean(&identity.handle),
        identity.identity_id,
        clean(identity.hub_url.as_str())
    );
    if args.no_setup {
        println!("agent setup skipped: run agmen setup");
    } else {
        run_setup(&root, &identity).await;
    }
    println!("Next: start your agent in this folder; it has the agmen tools.");
    Ok(())
}

/// A link may choose a new hub only after the person consents, before any request.
async fn confirm_link_hub(hub: &Url, args: &JoinArgs) -> Result<()> {
    let origin = hub.origin().ascii_serialization();
    let Some(saved) = read_login().await? else {
        eprintln!("Signing in to {origin} to accept this invite.");
        return Ok(());
    };
    if same_hub(&saved.hub_url, hub) || args.yes {
        return Ok(());
    }
    let saved = saved.hub_url.origin().ascii_serialization();
    if args.stdin || !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(Fail::usage(format!(
            "This invite is for {origin}. You are signed in to {saved}. Sign in to that hub first.\n\
             Rerun with --yes or sign in with agmen login --hub {origin}"
        ))
        .into());
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("join-confirm".into())
        .spawn(move || {
            let result = confirm_switch(
                &saved,
                &origin,
                std::io::stdin().lock(),
                std::io::stderr().lock(),
            );
            drop(send.send(result));
        })
        .context("starting the sign-in confirmation reader")?;
    // A detached reader lets the CLI exit on timeout without waiting for terminal input.
    match tokio::time::timeout(std::time::Duration::from_secs(120), receive).await {
        Ok(answer) => answer.context("reading the sign-in confirmation")?,
        Err(_) => bail!("join cancelled; your sign-in is unchanged"),
    }
}

fn confirm_switch(
    saved: &str,
    origin: &str,
    input: impl std::io::BufRead,
    mut output: impl std::io::Write,
) -> Result<()> {
    write!(
        output,
        "Switch your sign-in from {saved} to {origin}? [y/N] "
    )
    .context("printing the sign-in confirmation")?;
    output
        .flush()
        .context("flushing the sign-in confirmation")?;
    // The timeout must be able to print to stderr while this reader waits.
    drop(output);
    let mut answer = Vec::new();
    input
        .take(1024)
        .read_until(b'\n', &mut answer)
        .context("reading the sign-in confirmation")?;
    if answer.len() < 1024
        && matches!(
            String::from_utf8_lossy(&answer)
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "y" | "yes"
        )
    {
        return Ok(());
    }
    bail!("join cancelled; your sign-in is unchanged")
}

/// The signed-in person for `hub`: the saved sign-in when it is for that hub
/// and works, else a device login there. Another hub's sign-in is replaced,
/// and the person is told.
async fn sign_in(hub: &Url) -> Result<Human> {
    if let Some(human) = human_at(hub).await? {
        return Ok(human);
    }
    let old = read_login().await?;
    let human = tokio::time::timeout(
        std::time::Duration::from_secs(900),
        super::account::device_login(hub),
    )
    .await
    .context("signing in exceeded 900 seconds")??;
    if let Some(old) = old
        && !same_hub(&old.hub_url, hub)
    {
        println!(
            "signed out of {}: agmen keeps one sign-in per machine",
            clean(old.hub_url.as_str())
        );
    }
    Ok(human)
}

/// The project `agmen join` without a link picks: `--project`, else the only one.
async fn member_project(human: &Human, wanted: Option<&str>) -> Result<(String, String)> {
    let me = human
        .api
        .me()
        .await
        .context("reading who is signed in failed")?;
    let project = match wanted {
        Some(id) => me
            .projects
            .iter()
            .find(|p| p.project == id)
            .with_context(|| {
                format!("you are not a member of project {id}; ask its admin for an invite")
            })?,
        None => match me.projects.as_slice() {
            [] => {
                return Err(Fail::usage("no projects yet: run agmen join <invite link>").into());
            }
            [project] => project,
            projects => {
                let mut message = String::from("choose a project with --project <id>:");
                for project in projects {
                    message.push_str(&format!(
                        "\n  {} ({})",
                        clean(&project.project),
                        clean(&project.name)
                    ));
                }
                return Err(Fail::usage(message).into());
            }
        },
    };
    Ok((project.project.clone(), project.name.clone()))
}

/// Sets up the agents in `root`. A failure is reported and never fails the
/// join: the identity is already written and `agmen setup` can be rerun.
async fn run_setup(root: &Path, identity: &IdentityFile) {
    let result = async {
        let plugin = blocking(|| setup::plugin_choice(false)).await?;
        setup::run(root, identity, None, plugin).await
    }
    .await;
    match result {
        Ok(changes) => {
            if changes.is_empty() {
                println!("no agents found: install one, then run agmen setup");
            }
            for change in changes {
                if let setup::Outcome::Failed(why) = &change.outcome {
                    eprintln!("agent setup failed: {}; run agmen setup", clean(why));
                } else {
                    println!("{}", clean(&change.to_string()));
                }
            }
        }
        Err(err) => eprintln!(
            "agent setup failed: {}; run agmen setup",
            clean(&format!("{err:#}"))
        ),
    }
}

/// Mints and writes this worktree's identity using an existing project membership.
#[expect(
    dead_code,
    reason = "the plan's agmen host up and agmen init call it; join itself resolves the role earlier"
)]
pub(crate) async fn join_here(
    human: &Human,
    root: &Path,
    project: &str,
    args: &JoinArgs,
) -> Result<IdentityFile> {
    let permissions = args.permissions(root).await?;
    mint_identity(human, root, project, args, permissions).await
}

/// Refuses a worktree that already has an identity file.
async fn check_not_joined(root: &Path) -> Result<()> {
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
    Ok(())
}

async fn mint_identity(
    human: &Human,
    root: &Path,
    project: &str,
    args: &JoinArgs,
    (kind, role): (Kind, Option<agmen_proto::RoleSpec>),
) -> Result<IdentityFile> {
    check_not_joined(root).await?;
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
        None => default_name(root),
    };
    let handle = format!("{}/{name}", me.handle);
    let request = Mint {
        handle: &handle,
        project,
        kind,
        role,
    };
    let minted = request.send(human).await?;
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
    let exclude = git::exclude_file(root).await?;
    blocking(move || git::add_exclude(&exclude, ".agmen/")).await?;
    set_token(key, minted.token).await?;
    write_identity(root, &identity).await?;
    let check_root = root.to_owned();
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
    Ok(identity)
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
async fn accept(human: &Human, code: &str) -> Result<Accepted> {
    let request = human
        .api
        .request(Method::POST, &["invites", code, "accept"]);
    match send::<Accepted>(request).await {
        Ok(accepted) => Ok(accepted),
        Err(err) => Err(match refusal(&err).map(|r| r.code.as_str()) {
            Some("invite_used") => anyhow::anyhow!(
                "the invite was already used; if you used it, run agmen join --project <id> in your clone (agmen whoami lists your projects)"
            ),
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
    fn hub_switch_releases_stderr_before_waiting_for_input() {
        use std::cell::Cell;
        use std::rc::Rc;

        struct Prompt(Rc<Cell<bool>>);
        impl std::io::Write for Prompt {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Drop for Prompt {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        struct Answer(Rc<Cell<bool>>);
        impl std::io::Read for Answer {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                assert!(
                    self.0.get(),
                    "stderr must be unlocked before input can block"
                );
                bytes[..2].copy_from_slice(b"y\n");
                Ok(2)
            }
        }

        let released = Rc::new(Cell::new(false));
        confirm_switch(
            "https://saved.example",
            "https://link.example",
            std::io::BufReader::new(Answer(released.clone())),
            Prompt(released),
        )
        .unwrap();
    }

    #[test]
    fn hub_switch_requires_an_explicit_yes() {
        for answer in ["y\n", "yes\n", "Y\n", "YeS\n", " yes \n"] {
            let mut prompt = Vec::new();
            confirm_switch(
                "https://saved.example",
                "https://link.example",
                answer.as_bytes(),
                &mut prompt,
            )
            .unwrap();
            assert_eq!(
                String::from_utf8(prompt).unwrap(),
                "Switch your sign-in from https://saved.example to https://link.example? [y/N] "
            );
        }
        for answer in [
            "",
            "\n",
            "n\n",
            "no\n",
            "yes please\n",
            "yesterday\n",
            "y\0\n",
            &format!("y{}", " ".repeat(1024)),
        ] {
            let error = confirm_switch(
                "https://saved.example",
                "https://link.example",
                answer.as_bytes(),
                Vec::new(),
            )
            .unwrap_err();
            let (message, code) = crate::output::explain(&error);
            assert_eq!(code, 1);
            assert!(
                message.contains("join cancelled; your sign-in is unchanged"),
                "{message}"
            );
        }
    }

    #[test]
    fn invite_links_and_codes_parse() {
        let code = "ab".repeat(32);
        for (text, base) in [
            (
                format!("https://api.getagmen.ai/i#{code}"),
                "https://api.getagmen.ai/",
            ),
            (
                format!("  <https://h.example/hub/i#{code}>\n"),
                "https://h.example/hub/",
            ),
            (
                format!("https://h.example/i#{}", code.to_uppercase()),
                "https://h.example/",
            ),
            (
                format!("http://127.0.0.1:8080/i#{code}"),
                "http://127.0.0.1:8080/",
            ),
            (
                format!("http://localhost:8080/i#{code}"),
                "http://localhost:8080/",
            ),
            (format!("http://[::1]:8080/i#{code}"), "http://[::1]:8080/"),
        ] {
            let Invite::Link { hub, code: parsed } = parse_invite(&text).unwrap() else {
                panic!("expected a link");
            };
            assert_eq!(hub.as_str(), base);
            assert_eq!(parsed, code);
        }
        assert!(
            matches!(parse_invite(&format!(" <{}> ", code.to_uppercase())).unwrap(), Invite::Code(parsed) if parsed == code)
        );
        for (text, problem) in [
            (format!("http://h.example/i#{code}"), "must be https"),
            ("https://h.example/i".into(), "fragment"),
            ("https://h.example/i#abc".into(), "64"),
            (format!("https://h.example/x#{code}"), "/i"),
            (format!("https://h.example/i?c=1#{code}"), "query"),
            (String::new(), "invite"),
            ("x".repeat(9 * 1024), "8 KiB"),
            (
                format!("https://user:pass@h.example/i#{code}"),
                "credentials",
            ),
            (format!("https://h.example/i/#{}", code), "/i"),
        ] {
            let err = parse_invite(&text).unwrap_err();
            assert_eq!(err.code, crate::output::Code::Usage);
            assert!(err.message.contains(problem), "{}", err.message);
        }
    }

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
