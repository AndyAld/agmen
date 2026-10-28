//! The `agmen` subcommands, one file per group, dispatched from one
//! match. `agmen mcp` lives in `crate::mcp` and calls the shared parts of
//! `claim`, `inbox`, `lanes` and `post`.

mod account;
pub(crate) mod claim;
pub(crate) mod inbox;
mod join;
pub(crate) mod lanes;
pub(crate) mod post;
mod watch;

use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

/// Coordinate agents and people on one repository through an agmen hub.
#[derive(Debug, Parser)]
#[command(name = "agmen", version, about, propagate_version = true)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Sign in to a hub with your browser (device login).
    Login(account::LoginArgs),
    /// Sign out: forget your tokens on this machine.
    Logout,
    /// Show who is signed in, their projects and roles, and this worktree's identity.
    Whoami,
    /// Make a single-use invite to a project (admins and coordinators).
    Invite(account::InviteArgs),
    /// Join this worktree to a project as a new identity of yours.
    Join(join::JoinArgs),
    /// Revoke this worktree's identity and remove its identity file.
    Leave,
    /// Post a message with any verb.
    Post(post::PostArgs),
    /// Ask a question, of the room or of some identities.
    Ask(post::AskArgs),
    /// Answer an ASK.
    Answer(post::AnswerArgs),
    /// Send a direct message to 1 to 8 identities.
    #[command(override_usage = "agmen dm [OPTIONS] <TO>... <TEXT>")]
    Dm(post::DmArgs),
    /// List new messages for this identity and mark them read.
    Inbox(inbox::InboxArgs),
    /// Follow the feed and this identity's inbox until stopped.
    Watch(watch::WatchArgs),
    /// Claim an item; edits in its lane need the GRANT.
    Claim(claim::ClaimArgs),
    /// Give up a claim.
    Release(claim::ReleaseArgs),
    /// Renew the leases of the claims this worktree holds.
    Heartbeat(Wait),
    /// Report finished work on a claimed item.
    Done(claim::DoneArgs),
    /// Report a new problem; the hub answers with its item key.
    Found(post::FoundArgs),
    /// Hand an item to an identity (coordinators).
    Assign(post::AssignArgs),
    /// Show the project's lanes, or replace them from a file.
    Lanes(lanes::LanesArgs),
    /// List the project's items and who holds each.
    Items,
    /// Say whether this identity may edit each path.
    CheckPath(lanes::CheckPathArgs),
    /// Serve this worktree's identity to an agent as MCP tools on stdin and stdout.
    Mcp(crate::mcp::McpArgs),
}

/// How long a publishing command waits for the hub to record the message.
#[derive(Debug, Clone, Copy, Args)]
pub(crate) struct Wait {
    /// Seconds to wait for the hub's answer before exiting 5 with the
    /// message still queued.
    #[arg(long = "wait", value_name = "SECONDS", default_value = "5", value_parser = parse_wait)]
    pub(crate) wait: Duration,
}

/// Seconds from 0 to 600, fractions allowed.
fn parse_wait(text: &str) -> Result<Duration, String> {
    let secs: f64 = text
        .parse()
        .map_err(|_| format!("{text:?} is not a number of seconds"))?;
    if !(0.0..=600.0).contains(&secs) {
        return Err("the wait is 0 to 600 seconds".to_owned());
    }
    Ok(Duration::from_secs_f64(secs))
}

impl Cli {
    /// Runs the parsed command.
    pub(crate) async fn run(self) -> Result<()> {
        match self.command {
            Command::Login(args) => account::login(args).await,
            Command::Logout => account::logout().await,
            Command::Whoami => account::whoami().await,
            Command::Invite(args) => account::invite(args).await,
            Command::Join(args) => join::join(args).await,
            Command::Leave => join::leave().await,
            Command::Post(args) => post::post(args).await,
            Command::Ask(args) => post::ask(args).await,
            Command::Answer(args) => post::answer(args).await,
            Command::Dm(args) => post::dm(args).await,
            Command::Inbox(args) => inbox::inbox(args).await,
            Command::Watch(args) => watch::watch(args).await,
            Command::Claim(args) => claim::claim(args).await,
            Command::Release(args) => claim::release(args).await,
            Command::Heartbeat(wait) => claim::heartbeat(wait).await,
            Command::Done(args) => claim::done(args).await,
            Command::Found(args) => post::found(args).await,
            Command::Assign(args) => post::assign(args).await,
            Command::Lanes(args) => lanes::lanes(args).await,
            Command::Items => lanes::items().await,
            Command::CheckPath(args) => lanes::check_path(args).await,
            Command::Mcp(args) => crate::mcp::run(args).await,
        }
    }
}
