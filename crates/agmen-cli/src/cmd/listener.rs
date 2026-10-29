//! Human listener commands through agmend's local API.

use agmen_proto::ProjectId;
use anyhow::{Context as _, Result};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use super::workers::{connect, daemon_error, field, project};
use crate::output::{Fail, clean};

#[derive(Debug, Args)]
pub(crate) struct ListenerArgs {
    #[command(subcommand)]
    command: ListenerCommand,
}

#[derive(Debug, Subcommand)]
enum ListenerCommand {
    /// Start your headless listener in this worktree.
    Start {
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Stop your listener.
    Stop {
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Show your listener's state and daily spend.
    Status {
        #[arg(long)]
        project: Option<ProjectId>,
        #[arg(long)]
        json: bool,
    },
    /// Change only the settings named by flags.
    Set(SetArgs),
}

#[derive(Debug, Args)]
struct SetArgs {
    #[arg(long)]
    project: Option<ProjectId>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    effort: Option<String>,
    #[arg(long)]
    per_delegation: Option<String>,
    #[arg(long)]
    daily: Option<String>,
    #[arg(long)]
    max_workers: Option<u32>,
    #[arg(long)]
    listener_daily: Option<String>,
    #[arg(long, value_parser = ["on", "off"])]
    reviews: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    accept_from: Vec<String>,
    #[arg(long)]
    worker_role: Vec<String>,
}

pub(crate) async fn run(args: ListenerArgs) -> Result<()> {
    let (method, params) = match args.command {
        ListenerCommand::Start {
            project: explicit,
            model,
        } => {
            let project = project(explicit).await?;
            let cwd = std::env::current_dir().context("reading the working directory")?;
            let root = crate::git::toplevel(&cwd).await?;
            let mut params = json!({"project":project,"root":root});
            if let Some(model) = model {
                params["model"] = json!(model);
            }
            ("listener_start", params)
        }
        ListenerCommand::Stop { project: explicit } => {
            ("listener_stop", json!({"project":project(explicit).await?}))
        }
        ListenerCommand::Status {
            project: explicit,
            json,
        } => {
            let params = json!({"project":project(explicit).await?});
            let answer = connect()
                .await?
                .call("listener_status", params)
                .await
                .map_err(daemon_error)?;
            return status(&answer, json);
        }
        ListenerCommand::Set(args) => ("listener_set", set_params(args).await?),
    };
    let answer = connect()
        .await?
        .call(method, params.clone())
        .await
        .map_err(daemon_error)?;
    match method {
        "listener_start" => println!(
            "{} {}",
            clean(field(&answer, "identity_id")?),
            clean(field(&answer, "handle")?)
        ),
        "listener_stop" => println!("stopped {}", clean(field(&params, "project")?)),
        _ => {}
    }
    Ok(())
}

async fn set_params(args: SetArgs) -> Result<Value> {
    if args.accept_from.len() > 64 {
        return Err(Fail::usage("at most 64 --accept-from values are allowed").into());
    }
    if args.worker_role.len() > 32 {
        return Err(Fail::usage("at most 32 --worker-role values are allowed").into());
    }
    let mut params = serde_json::Map::new();
    for (key, value) in [
        ("model", args.model),
        ("effort", args.effort),
        ("per_delegation_usd", args.per_delegation),
        ("daily_usd", args.daily),
        ("listener_daily_usd", args.listener_daily),
        ("reviews", args.reviews),
    ] {
        if let Some(value) = value {
            params.insert(key.into(), json!(value));
        }
    }
    if let Some(value) = args.max_workers {
        params.insert("max_workers".into(), json!(value));
    }
    if !args.accept_from.is_empty() {
        params.insert("accept_from".into(), json!(args.accept_from));
    }
    if !args.worker_role.is_empty() {
        params.insert("worker_roles".into(), json!(args.worker_role));
    }
    if params.is_empty() {
        return Err(Fail::usage("listener set needs at least one settings flag").into());
    }
    params.insert("project".into(), json!(project(args.project).await?));
    Ok(Value::Object(params))
}

fn status(answer: &Value, json: bool) -> Result<()> {
    let rows = answer
        .as_array()
        .context("agmend's listener status is not an array")?;
    if json {
        println!("{}", serde_json::to_string(answer)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no listener");
    }
    for row in rows.iter().take(64) {
        let error = match row.get("error").and_then(Value::as_str) {
            Some(error) => format!("  error: {}", clean(error)),
            None => String::new(),
        };
        println!(
            "{} {} {} {} {}/{} USD{error}",
            clean(field(row, "project")?),
            clean(field(row, "handle")?),
            clean(field(row, "state")?),
            clean(field(row, "model")?),
            clean(field(row, "today_usd")?),
            clean(field(row, "daily_usd")?)
        );
    }
    Ok(())
}
