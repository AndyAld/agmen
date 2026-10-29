//! Human worker commands through agmend's local API.

use agmen_proto::{IdentityId, ProjectId};
use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::agmend::{Agmend, AgmendError};
use crate::output::{Code, Fail, clean};

#[derive(Debug, Args)]
pub(crate) struct LaunchArgs {
    #[arg(long)]
    name: String,
    #[arg(long)]
    role: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    budget: Option<serde_json::Number>,
    #[arg(long)]
    project: Option<ProjectId>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct WorkersArgs {
    #[arg(long)]
    project: Option<ProjectId>,
    #[arg(long)]
    json: bool,
    #[command(subcommand)]
    command: Option<WorkersCommand>,
}

#[derive(Debug, Subcommand)]
enum WorkersCommand {
    /// Stop a managed agent.
    Stop { identity: IdentityId },
}

#[derive(Debug, Subcommand)]
pub(crate) enum WorktreeCommand {
    /// Prune merged worktrees released by agmend; save dirty work in WIP.
    Prune {
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        base: Option<String>,
        /// Confirm removal of exactly this unmerged branch.
        #[arg(long)]
        confirm: Option<String>,
    },
}

#[derive(Debug, Args)]
pub(crate) struct BudgetArgs {
    identity: IdentityId,
    #[arg(long)]
    add: serde_json::Number,
}

fn daemon_error(error: AgmendError) -> anyhow::Error {
    match error {
        AgmendError::NotRunning => Fail::new(Code::NotLoggedIn, error.to_string()).into(),
        AgmendError::Refused { message, .. } => anyhow::Error::msg(clean(&message).into_owned()),
        other => other.into(),
    }
}

pub(super) async fn connect() -> Result<Agmend> {
    Agmend::connect_default().await.map_err(daemon_error)
}

pub(super) async fn call(daemon: &mut Agmend, method: &str, params: Value) -> Result<Value> {
    daemon.call(method, params).await.map_err(daemon_error)
}

async fn project(explicit: Option<ProjectId>) -> Result<ProjectId> {
    match explicit {
        Some(project) => Ok(project),
        None => Ok(crate::joined::joined().await?.identity.project),
    }
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("agmend's answer has no string {key}"))
}

pub(crate) async fn launch(args: LaunchArgs) -> Result<()> {
    let mut daemon = connect().await?;
    let project = project(args.project).await?;
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let root = crate::git::toplevel(&cwd).await?;
    let mut params = json!({"project":project,"name":args.name,"root":root});
    if let Some(role) = args.role {
        params["role"] = json!(role);
    }
    if let Some(model) = args.model {
        params["model"] = json!(model);
    }
    if let Some(budget) = args.budget {
        params["budget_usd"] = json!(budget);
    }
    let answer = call(&mut daemon, "agent_launch", params).await?;
    println!(
        "{} {} {}",
        clean(field(&answer, "identity")?),
        clean(field(&answer, "worktree")?),
        clean(field(&answer, "branch")?)
    );
    Ok(())
}

pub(crate) async fn workers(args: WorkersArgs) -> Result<()> {
    let mut daemon = connect().await?;
    if let Some(WorkersCommand::Stop { identity }) = args.command {
        let answer = call(&mut daemon, "agent_stop", json!({"identity":identity})).await?;
        println!(
            "{} {}",
            clean(field(&answer, "state")?),
            clean(field(&answer, "identity")?)
        );
        return Ok(());
    }
    let params = args
        .project
        .map_or_else(|| json!({}), |project| json!({"project":project}));
    let answer = call(&mut daemon, "agent_list", params).await?;
    let agents = answer
        .get("agents")
        .and_then(Value::as_array)
        .context("agmend's answer has no agents array")?;
    if args.json {
        println!("{}", serde_json::to_string(&answer)?);
        return Ok(());
    }
    for agent in agents.iter().take(256) {
        let role = match agent.get("role") {
            Some(Value::Null) => "-",
            Some(Value::String(role)) => role,
            _ => bail!("agmend's answer has no role"),
        };
        println!(
            "{} {} {} {} {}/{} {}",
            clean(field(agent, "state")?),
            clean(field(agent, "handle")?),
            clean(field(agent, "identity")?),
            clean(field(agent, "branch")?),
            clean(field(agent, "spent_usd")?),
            clean(field(agent, "budget_usd")?),
            clean(role)
        );
    }
    Ok(())
}

pub(crate) async fn worktree(command: WorktreeCommand) -> Result<()> {
    let WorktreeCommand::Prune {
        project: explicit,
        base,
        confirm,
    } = command;
    let mut daemon = connect().await?;
    let project = project(explicit).await?;
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let root = crate::git::toplevel(&cwd).await?;
    // remember_root runs before snapshot's hub reads. A hub failure still
    // registers the checkout; prune checks the local project and owner itself.
    match daemon
        .call("console_snapshot", json!({"project":project,"root":root}))
        .await
    {
        Ok(_)
        | Err(AgmendError::Refused {
            code: agmen_local::ErrorCode::Hub,
            ..
        }) => {}
        Err(error) => return Err(daemon_error(error)),
    }
    let mut params = json!({"project":project});
    if let Some(base) = base {
        params["base"] = json!(base);
    }
    if let Some(confirm) = confirm {
        params["confirm"] = json!(confirm);
    }
    let answer = call(&mut daemon, "prune", params).await?;
    let removed = answer
        .get("removed")
        .and_then(Value::as_array)
        .context("agmend's answer has no removed array")?;
    let kept = answer
        .get("kept")
        .and_then(Value::as_array)
        .context("agmend's answer has no kept array")?;
    for branch in removed.iter().take(256) {
        let branch = branch
            .as_str()
            .context("agmend's removed branch is not a string")?;
        println!("removed {}", clean(branch));
    }
    for entry in kept.iter().take(256) {
        println!(
            "kept {}: {}",
            clean(field(entry, "branch")?),
            clean(field(entry, "reason")?)
        );
    }
    Ok(())
}

pub(crate) async fn budget(args: BudgetArgs) -> Result<()> {
    let mut daemon = connect().await?;
    let answer = call(
        &mut daemon,
        "agent_budget",
        json!({"identity":args.identity,"add_usd":args.add}),
    )
    .await?;
    println!(
        "{} {}",
        clean(field(&answer, "identity")?),
        clean(field(&answer, "budget_usd")?)
    );
    Ok(())
}
