//! Human listener commands through agmend's local API.

use agmen_proto::ProjectId;
use anyhow::{Context as _, Result};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use super::workers::{connect, daemon_error, field, project};
use crate::agmend::{Agmend, LAUNCH_CALL_TIMEOUT};
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
        /// Applies only to the listener being started. `agmen listener set --model` saves it.
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
    let answer = send(&mut connect().await?, method, params.clone()).await?;
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

/// Sends one listener call to agmend. A start can mint and roll back inside
/// agmend, so it waits [`LAUNCH_CALL_TIMEOUT`]; the other calls wait the usual 30 s.
async fn send(daemon: &mut Agmend, method: &str, params: Value) -> Result<Value> {
    let answer = if method == "listener_start" {
        daemon
            .call_within(LAUNCH_CALL_TIMEOUT, method, params)
            .await
    } else {
        daemon.call(method, params).await
    };
    answer.map_err(daemon_error)
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agmend::tests::Fake;

    /// A start whose hub mint stalls and rolls back answers after about 31 s,
    /// which is past the 30 s every other call waits.
    #[tokio::test(start_paused = true)]
    async fn a_start_gets_agmends_answer_after_thirty_one_seconds() {
        let answer = json!({"identity_id":"listen01","handle":"andy/listen"});
        let fake = Fake::slow(answer.clone()).await;
        let mut daemon = Agmend::connect(fake.path()).await.unwrap();
        let params = json!({"project":"project1","root":"/repo"});
        assert_eq!(
            send(&mut daemon, "listener_start", params).await.unwrap(),
            answer
        );
        assert_eq!(fake.requests()[1].method, "listener_start");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_or_set_still_gives_up_after_thirty_seconds() {
        for method in ["listener_stop", "listener_set"] {
            let fake = Fake::slow(json!({})).await;
            let mut daemon = Agmend::connect(fake.path()).await.unwrap();
            let error = send(&mut daemon, method, json!({"project":"project1"})).await;
            let error = error.unwrap_err().to_string();
            assert!(
                error.contains(&format!("did not answer `{method}` in time")),
                "{error}"
            );
        }
    }
}
