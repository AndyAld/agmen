//! `agmen group members|remove|role|transfer|revoke`.

use agmen_client::find_identity;
use anyhow::{Context as _, Result};
use clap::{Args, Subcommand, ValueEnum};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::client::{self, Human, blocking, refusal, send};
use crate::output::{Code, Fail, clean};

/// The hub stops a member list at 1000 people and 1000 identities each.
const LIST_CAP: usize = 1000;

const NAME_THE_GROUP: &str = "name the group: --group <name or id> (agmen whoami lists them)";

/// `agmen group`.
#[derive(Debug, Args)]
pub(crate) struct GroupArgs {
    #[command(subcommand)]
    command: GroupCommand,
}

#[derive(Debug, Subcommand)]
enum GroupCommand {
    /// List members and their live identities.
    Members {
        #[command(flatten)]
        group: GroupFlag,
    },
    /// Remove a member. Their identities in the group are revoked.
    Remove {
        /// The member's handle.
        handle: String,
        #[command(flatten)]
        group: GroupFlag,
    },
    /// Change a member's role. `owner` is a usage error.
    Role {
        /// The member's handle.
        handle: String,
        /// `admin` or `member`.
        #[arg(value_enum)]
        role: GroupRole,
        #[command(flatten)]
        group: GroupFlag,
    },
    /// Make an admin the owner. They are the only one who can hand it back.
    Transfer {
        /// The member's handle. They must already be an admin.
        handle: String,
        #[command(flatten)]
        group: GroupFlag,
        /// Confirm the transfer.
        #[arg(long)]
        yes: bool,
    },
    /// Revoke one identity. The id names its group, so this takes no `--group`.
    Revoke {
        /// The identity id (`agmen group members` lists them).
        #[arg(value_name = "IDENTITY")]
        id: String,
    },
}

/// `--group` on every subcommand except `revoke`.
#[derive(Debug, Args)]
struct GroupFlag {
    /// Group name or id. Defaults to this worktree's group, or your only group.
    #[arg(long, value_name = "name or id")]
    group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum GroupRole {
    Admin,
    Member,
}

impl GroupRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    /// The role as the success line says it: "an admin", "a member".
    const fn with_article(self) -> &'static str {
        match self {
            Self::Admin => "an admin",
            Self::Member => "a member",
        }
    }
}

/// Runs one `agmen group` command.
///
/// # Errors
///
/// A usage error exits 2. `not_a_group_member` exits 3. Any other hub
/// refusal exits 1.
pub(crate) async fn run(args: GroupArgs) -> Result<()> {
    let human = client::human().await?;
    match args.command {
        GroupCommand::Members { group } => members(&human, group.group.as_deref()).await,
        GroupCommand::Remove { handle, group } => {
            remove(&human, &handle, group.group.as_deref()).await
        }
        GroupCommand::Role {
            handle,
            role,
            group,
        } => change_role(&human, &handle, role, group.group.as_deref()).await,
        GroupCommand::Transfer { handle, group, yes } => {
            transfer(&human, &handle, group.group.as_deref(), yes).await
        }
        GroupCommand::Revoke { id } => revoke(&human, &id).await,
    }
}

/// The group `flag` names, or this worktree's group when `flag` is absent.
///
/// A tenant id match wins. Otherwise the stored name must match exactly,
/// and a hosted name is the full `<owner handle>/<name>`. With no flag,
/// this worktree's tenant is the group. When this directory has not joined,
/// the person's only group is.
///
/// # Errors
///
/// A usage error when nothing matches (`no group <value>: agmen whoami
/// lists yours`), when several names match (one `<name> (<id>)` line each),
/// or when no flag is given, this directory has not joined, and the person
/// does not have exactly one group (`name the group: --group <name or id>
/// (agmen whoami lists them)`).
pub(crate) async fn resolve_group(human: &Human, flag: Option<&str>) -> Result<client::Group> {
    let me = human.api.me().await.context("reading your groups failed")?;
    match flag {
        Some(value) => by_flag(&me.groups, value),
        None => by_default(&me.groups).await,
    }
}

fn by_flag(groups: &[client::Group], value: &str) -> Result<client::Group> {
    if let Some(group) = groups.iter().find(|group| group.tenant == value) {
        return Ok(group.clone());
    }
    let mut matches = groups.iter().filter(|group| group.name == value).cloned();
    let Some(first) = matches.next() else {
        return Err(no_group(value).into());
    };
    let Some(second) = matches.next() else {
        return Ok(first);
    };
    let mut found = vec![first, second];
    found.extend(matches.take(LIST_CAP.saturating_sub(2)));
    Err(Fail::usage(several(value, &found)).into())
}

async fn by_default(groups: &[client::Group]) -> Result<client::Group> {
    if let Some(tenant) = worktree_tenant().await? {
        return groups
            .iter()
            .find(|group| group.tenant == tenant)
            .cloned()
            .ok_or_else(|| no_group(&tenant).into());
    }
    match groups {
        [only] => Ok(only.clone()),
        _ => Err(Fail::usage(NAME_THE_GROUP).into()),
    }
}

async fn worktree_tenant() -> Result<Option<String>> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    match blocking(move || Ok(find_identity(&cwd))).await? {
        Ok(Some(joined)) => Ok(Some(joined.identity.tenant.as_str().to_owned())),
        Ok(None) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn no_group(value: &str) -> Fail {
    Fail::usage(format!(
        "no group {}: agmen whoami lists yours",
        clean(value)
    ))
}

fn several(value: &str, groups: &[client::Group]) -> String {
    let mut message = format!("several groups match {}:", clean(value));
    for group in groups.iter().take(LIST_CAP) {
        message.push('\n');
        message.push_str(&clean(&group.name));
        message.push_str(" (");
        message.push_str(&clean(&group.tenant));
        message.push(')');
    }
    message
}

async fn members(human: &Human, flag: Option<&str>) -> Result<()> {
    let group = resolve_group(human, flag).await?;
    let list: MemberList = human
        .api
        .get(&["tenants", &group.tenant, "members"])
        .await
        .map_err(|err| group_error(err, "", "listing the group's members failed"))?;
    for member in list.members.iter().take(LIST_CAP) {
        print_member(member);
    }
    Ok(())
}

fn print_member(member: &MemberRow) {
    let projects = member
        .projects
        .iter()
        .take(LIST_CAP)
        .map(|project| clean(&project.name))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "{} {} ({projects})",
        clean(&member.role),
        clean(&member.handle)
    );
    for identity in member.identities.iter().take(LIST_CAP) {
        let project = member
            .projects
            .iter()
            .find(|project| project.project == identity.project)
            .map_or_else(|| clean(&identity.project), |project| clean(&project.name));
        println!(
            "  {} {} in {project}",
            clean(&identity.id),
            clean(&identity.handle)
        );
    }
}

async fn remove(human: &Human, handle: &str, flag: Option<&str>) -> Result<()> {
    let group = resolve_group(human, flag).await?;
    send::<()>(human.api.request(
        Method::DELETE,
        &["tenants", &group.tenant, "members", handle],
    ))
    .await
    .map_err(|err| group_error(err, handle, "removing the member failed"))?;
    println!("removed {} from {}", clean(handle), clean(&group.name));
    Ok(())
}

async fn change_role(
    human: &Human,
    handle: &str,
    role: GroupRole,
    flag: Option<&str>,
) -> Result<()> {
    let group = resolve_group(human, flag).await?;
    send::<serde_json::Value>(
        human
            .api
            .request(
                Method::PUT,
                &["tenants", &group.tenant, "members", handle, "role"],
            )
            .json(&RoleBody {
                role: role.as_str(),
            }),
    )
    .await
    .map_err(|err| group_error(err, handle, "changing the role failed"))?;
    println!(
        "{} is now {} of {}",
        clean(handle),
        role.with_article(),
        clean(&group.name)
    );
    Ok(())
}

async fn transfer(human: &Human, handle: &str, flag: Option<&str>, yes: bool) -> Result<()> {
    let group = resolve_group(human, flag).await?;
    if !yes {
        return Err(Fail::usage(format!(
            "this makes {} the owner of {}; only they can hand it back. Rerun with --yes",
            clean(handle),
            clean(&group.name),
        ))
        .into());
    }
    send::<serde_json::Value>(
        human
            .api
            .request(Method::POST, &["tenants", &group.tenant, "transfer"])
            .json(&TransferBody { to: handle }),
    )
    .await
    .map_err(|err| group_error(err, handle, "transferring ownership failed"))?;
    println!(
        "{} is now the owner of {}",
        clean(handle),
        clean(&group.name)
    );
    Ok(())
}

async fn revoke(human: &Human, id: &str) -> Result<()> {
    send::<()>(human.api.request(Method::DELETE, &["identities", id]))
        .await
        .map_err(|err| revoke_error(err, id))?;
    println!(
        "revoked {}; its agent is disconnected and cannot reconnect",
        clean(id)
    );
    Ok(())
}

fn revoke_error(err: anyhow::Error, id: &str) -> anyhow::Error {
    if refusal(&err).is_some_and(|r| r.status == 404) {
        return anyhow::anyhow!(
            "no identity {} that you can revoke (agmen group members lists them)",
            clean(id)
        );
    }
    group_error(err, "", "revoking the identity failed")
}

fn group_error(err: anyhow::Error, handle: &str, what: &'static str) -> anyhow::Error {
    let code = refusal(&err).map(|r| r.code.clone());
    let Some(code) = code else {
        return err.context(what);
    };
    let handle = clean(handle);
    match code.as_str() {
        "group_admin_required" => {
            anyhow::anyhow!("only the group's owner or an admin can do that")
        }
        "owner_required" => anyhow::anyhow!("only the group's owner can do that"),
        "owner_cannot_be_removed" => {
            anyhow::anyhow!("the owner cannot be removed; transfer ownership first")
        }
        "owner_role_fixed" => {
            anyhow::anyhow!("the owner's role cannot change; transfer ownership instead")
        }
        "use_transfer" => anyhow::anyhow!("use agmen group transfer to make someone the owner"),
        "not_a_group_admin" => {
            anyhow::anyhow!("make {handle} an admin first: agmen group role {handle} admin")
        }
        "no_such_member" => anyhow::anyhow!("{handle} is not in this group"),
        "not_a_group_member" => {
            Fail::new(Code::NotLoggedIn, "you are not a member of that group").into()
        }
        _ => err.context(what),
    }
}

#[derive(Debug, Deserialize)]
struct MemberList {
    members: Vec<MemberRow>,
}

#[derive(Debug, Deserialize)]
struct MemberRow {
    handle: String,
    role: String,
    projects: Vec<MemberProject>,
    identities: Vec<MemberIdentity>,
}

#[derive(Debug, Deserialize)]
struct MemberProject {
    project: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct MemberIdentity {
    id: String,
    project: String,
    handle: String,
}

#[derive(Debug, Serialize)]
struct RoleBody<'a> {
    role: &'a str,
}

#[derive(Debug, Serialize)]
struct TransferBody<'a> {
    to: &'a str,
}
