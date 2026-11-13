//! Human and parent-agent worker commands through agmend's local API.

use agmen_proto::{IdentityId, ProjectId};
use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::agmend::{Agmend, AgmendError, LAUNCH_CALL_TIMEOUT, LaunchCredential};
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
    #[arg(long)]
    brief: Option<String>,
    /// The coding agent to run it on: claude (the default) or opencode.
    #[arg(long)]
    provider: Option<String>,
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
    Stop {
        identity: IdentityId,
        /// Leave this agent's children running.
        #[arg(long)]
        keep_children: bool,
    },
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

pub(super) fn daemon_error(error: AgmendError) -> anyhow::Error {
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

pub(super) async fn project(explicit: Option<ProjectId>) -> Result<ProjectId> {
    match explicit {
        Some(project) => Ok(project),
        None => Ok(crate::joined::joined().await?.identity.project),
    }
}

pub(super) fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("agmend's answer has no string {key}"))
}

pub(crate) async fn launch(args: LaunchArgs) -> Result<()> {
    let credential = LaunchCredential::from_env()?;
    if credential.is_some() && args.project.is_some() {
        return Err(Fail::usage(
            "--project does not apply to an agent's launch: the child joins its parent's project",
        )
        .into());
    }
    // agmend's child_launch needs a role; a person's own launch may omit it.
    if credential.is_some() && args.role.is_none() {
        return Err(Fail::usage("--role is required for an agent's launch").into());
    }
    if credential.is_some() && args.provider.is_some() {
        return Err(Fail::usage(
            "--provider does not apply to an agent's launch: the child runs on its parent's coding agent",
        )
        .into());
    }
    if credential.is_none() && args.brief.is_some() {
        return Err(Fail::usage(
            "--brief is for an agent's launch; message your agent after launching it",
        )
        .into());
    }
    let mut daemon = connect().await?;
    let mut params = json!({"name":args.name});
    if credential.is_none() {
        let project = project(args.project).await?;
        let cwd = std::env::current_dir().context("reading the working directory")?;
        let root = crate::git::toplevel(&cwd).await?;
        params["project"] = json!(project);
        params["root"] = json!(root);
    }
    if let Some(role) = args.role {
        params["role"] = json!(role);
    }
    if let Some(model) = args.model {
        params["model"] = json!(model);
    }
    if let Some(budget) = args.budget {
        params["budget_usd"] = json!(budget);
    }
    if let Some(brief) = args.brief {
        params["brief"] = json!(brief);
    }
    if let Some(provider) = args.provider {
        params["provider"] = json!(provider);
    }
    let answer = send_launch(&mut daemon, credential, params).await?;
    println!(
        "{} {} {}",
        clean(field(&answer, "identity")?),
        clean(field(&answer, "worktree")?),
        clean(field(&answer, "branch")?)
    );
    Ok(())
}

/// Sends the launch as the parent agent when `credential` is set, else as the
/// person. agmend may mint and roll back inside it, so it waits [`LAUNCH_CALL_TIMEOUT`].
async fn send_launch(
    daemon: &mut Agmend,
    credential: Option<LaunchCredential>,
    params: Value,
) -> Result<Value> {
    match credential {
        Some(credential) => {
            daemon
                .call_as_within(
                    LAUNCH_CALL_TIMEOUT,
                    &credential.identity,
                    &credential.token,
                    "child_launch",
                    params,
                )
                .await
        }
        None => {
            daemon
                .call_within(LAUNCH_CALL_TIMEOUT, "agent_launch", params)
                .await
        }
    }
    .map_err(daemon_error)
}

pub(crate) async fn workers(args: WorkersArgs) -> Result<()> {
    if let Some(WorkersCommand::Stop {
        identity,
        keep_children,
    }) = args.command
    {
        let credential = LaunchCredential::from_env()?;
        let mut daemon = connect().await?;
        let mut params = json!({"identity":identity});
        if keep_children {
            params["subtree"] = json!(false);
        }
        let answer = match credential {
            Some(credential) => daemon
                .call_as(
                    &credential.identity,
                    &credential.token,
                    "child_stop",
                    params,
                )
                .await
                .map_err(daemon_error)?,
            None => call(&mut daemon, "agent_stop", params).await?,
        };
        println!(
            "{} {}",
            clean(field(&answer, "state")?),
            clean(field(&answer, "identity")?)
        );
        return Ok(());
    }
    let mut daemon = connect().await?;
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
            // An agent launched with no budget reports the largest amount as its cap.
            if agent.get("no_budget") == Some(&Value::Bool(true)) {
                "none".into()
            } else {
                clean(field(agent, "budget_usd")?)
            },
            clean(role)
        );
    }
    Ok(())
}

/// Checked agent worktrees plus the over-limit entries one prune lists.
const MAX_KEPT_LINES: usize = 512;

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
    let unlisted = match answer.get("unlisted") {
        None => None,
        Some(count) => Some(
            count
                .as_u64()
                .context("agmend's unlisted count is not a number")?,
        ),
    };
    for branch in removed.iter().take(256) {
        let branch = branch
            .as_str()
            .context("agmend's removed branch is not a string")?;
        println!("removed {}", clean(branch));
    }
    for entry in kept.iter().take(MAX_KEPT_LINES) {
        println!(
            "kept {}: {}",
            clean(field(entry, "branch")?),
            clean(field(entry, "reason")?)
        );
    }
    if let Some(count) = unlisted {
        println!(
            "{count} more agent worktrees over the per-call limit; prune again after these are handled"
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agmend::tests::Fake;

    /// A launch whose hub mint stalls and rolls back answers after about 31 s,
    /// which is past the 30 s every other call waits.
    #[tokio::test(start_paused = true)]
    async fn a_launch_gets_agmends_answer_after_thirty_one_seconds() {
        let answer =
            json!({"identity":"agent001","worktree":"/fake/worker","branch":"agent/worker"});
        let fake = Fake::slow(answer.clone()).await;
        let mut daemon = Agmend::connect(fake.path()).await.unwrap();
        let person = send_launch(&mut daemon, None, json!({"name":"worker"})).await;
        assert_eq!(person.unwrap(), answer, "the person's launch");
        let credential = LaunchCredential {
            identity: "parent01".parse().unwrap(),
            token: "a".repeat(64).parse().unwrap(),
        };
        let params = json!({"name":"k1","role":"reviewer"});
        let agent = send_launch(&mut daemon, Some(credential), params).await;
        assert_eq!(agent.unwrap(), answer, "the agent's launch");
        let methods: Vec<_> = fake.requests().into_iter().map(|r| r.method).collect();
        assert_eq!(methods, ["hello", "agent_launch", "child_launch"]);
    }
}
