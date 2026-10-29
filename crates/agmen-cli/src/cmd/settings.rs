//! Human decisions on listener settings changes.

use agmen_proto::ProjectId;
use anyhow::Result;
use clap::{Args, Subcommand};
use serde_json::json;

use super::workers::{connect, daemon_error, project};
use crate::output::clean;

#[derive(Debug, Args)]
pub(crate) struct SettingsArgs {
    #[command(subcommand)]
    command: SettingsCommand,
}

#[derive(Debug, Subcommand)]
enum SettingsCommand {
    /// Apply a proposed change.
    Apply {
        id: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Cancel a proposed change.
    Cancel {
        id: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
    /// Undo an applied change.
    Undo {
        id: String,
        #[arg(long)]
        project: Option<ProjectId>,
    },
}

pub(crate) async fn run(args: SettingsArgs) -> Result<()> {
    let (id, explicit, decision, verb) = match args.command {
        SettingsCommand::Apply { id, project } => (id, project, Some(true), "applied"),
        SettingsCommand::Cancel { id, project } => (id, project, Some(false), "cancelled"),
        SettingsCommand::Undo { id, project } => (id, project, None, "undone"),
    };
    let mut params = json!({"project":project(explicit).await?,"id":id});
    let method = if let Some(apply) = decision {
        params["apply"] = json!(apply);
        "settings_decide"
    } else {
        "settings_undo"
    };
    connect()
        .await?
        .call(method, params)
        .await
        .map_err(daemon_error)?;
    println!("{verb} {}", clean(&id));
    Ok(())
}
