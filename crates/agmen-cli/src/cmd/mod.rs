//! The `agmen` subcommands, one file per group, dispatched from one
//! match. `agmen mcp` lives in `crate::mcp` and calls the shared parts of
//! `claim`, `inbox`, `lanes` and `post`.

mod account;
mod chat;
pub(crate) mod claim;
mod group;
mod host;
pub(crate) mod inbox;
mod init;
mod install_receipt;
mod join;
pub(crate) mod lanes;
#[cfg(unix)]
mod listener;
mod open;
pub(crate) mod post;
mod role;
#[cfg(unix)]
mod settings;
pub(crate) mod templates;
mod watch;
#[cfg(unix)]
mod workers;

use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

/// Coordinate agents and people on one repository through an agmen hub.
#[derive(Debug, Parser)]
#[command(
    name = "agmen",
    version,
    about,
    propagate_version = true,
    subcommand_precedence_over_arg = true,
    args_conflicts_with_subcommands = true,
    after_help = "With no arguments in a terminal, open the current folder in agmen.app. Use agmen <path> or agmen open [path] for another folder. A path matching a subcommand name requires agmen open <path>."
)]
pub(crate) struct Cli {
    /// Folder to open in the desktop app.
    path: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg(test)]
mod launch_tests {
    use super::*;

    #[test]
    fn bare_cli_opens_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let Command::Open { path } = Cli::try_parse_from(["agmen"])
            .unwrap()
            .into_command(true)
            .unwrap()
        else {
            panic!("expected open");
        };
        assert_eq!(
            open::folder(path.as_deref(), dir.path()).unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn path_and_explicit_open_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();
        for args in [
            vec!["agmen", path],
            vec!["agmen", "open"],
            vec!["agmen", "open", path],
        ] {
            let Command::Open { path } = Cli::try_parse_from(args)
                .unwrap()
                .into_command(true)
                .unwrap()
            else {
                panic!("expected open");
            };
            assert_eq!(
                open::folder(path.as_deref(), dir.path()).unwrap(),
                dir.path().canonicalize().unwrap()
            );
        }
    }

    #[test]
    fn subcommands_and_help_keep_dispatching() {
        assert!(
            format!("{:?}", Cli::try_parse_from(["agmen", "whoami"]).unwrap()).contains("Whoami")
        );
        for args in [["agmen", "--help"], ["agmen", "help"]] {
            assert_eq!(
                Cli::try_parse_from(args).unwrap_err().kind(),
                clap::error::ErrorKind::DisplayHelp
            );
        }
    }

    #[test]
    fn bare_cli_outside_a_terminal_keeps_the_usage_error() {
        let error = Cli::try_parse_from(["agmen"])
            .unwrap()
            .into_command(false)
            .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingSubcommand);
    }

    #[test]
    fn unknown_word_is_an_unknown_subcommand_not_a_launch() {
        for interactive in [true, false] {
            let error = Cli::try_parse_from(["agmen", "no-such-subcommand-or-folder"])
                .unwrap()
                .into_command(interactive)
                .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        }
    }

    #[test]
    fn join_accepts_stdin_but_refuses_two_sources() {
        assert!(Cli::try_parse_from(["agmen", "join", "--stdin"]).is_ok());
        assert!(Cli::try_parse_from(["agmen", "join", "--stdin", "secret"]).is_err());
        assert!(Cli::try_parse_from(["agmen", "join", "--stdin", "--project", "p"]).is_err());
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Control your project's listener.
    #[cfg(unix)]
    Listener(listener::ListenerArgs),
    /// Apply, cancel or undo listener settings changes.
    #[cfg(unix)]
    Settings(settings::SettingsArgs),
    /// Launch a managed agent in a new worktree.
    #[cfg(unix)]
    Launch(workers::LaunchArgs),
    /// List managed agents, or stop one.
    #[cfg(unix)]
    Workers(workers::WorkersArgs),
    /// Manage released agent worktrees.
    #[cfg(unix)]
    Worktree {
        #[command(subcommand)]
        command: workers::WorktreeCommand,
    },
    /// Raise a managed agent's budget.
    #[cfg(unix)]
    Budget(workers::BudgetArgs),
    /// Open a folder in the desktop app (defaults to the current folder).
    Open { path: Option<std::path::PathBuf> },
    /// Read the shell install receipt without changing ownership.
    InstallReceipt(install_receipt::ReceiptArgs),
    /// Sign in to a hub with your browser (device login).
    Login(account::LoginArgs),
    /// Sign out: forget your tokens on this machine.
    Logout,
    /// Show who is signed in, their projects and roles, and this worktree's identity.
    Whoami {
        /// Print the hub membership response as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Manage projects as a tenant administrator.
    Project {
        #[command(subcommand)]
        command: account::ProjectCommand,
    },
    /// Manage a group's members, roles and identities.
    Group(group::GroupArgs),
    /// Make a single-use invite to a project (admins and coordinators).
    Invite(account::InviteArgs),
    /// Join this worktree to a project as a new identity of yours.
    Join(join::JoinArgs),
    /// Start agmen here: self host or agmen hosted.
    Init(init::InitArgs),
    /// Set up your installed agents to use agmen in this worktree.
    Setup {
        #[arg(long, value_enum)]
        agent: Option<crate::setup::AgentKind>,
        #[arg(long)]
        no_plugin: bool,
    },
    /// List, show or import local roles.
    Role {
        #[command(subcommand)]
        command: role::RoleCommand,
    },
    /// List, export, import or save agent and team templates.
    Templates {
        #[command(subcommand)]
        command: templates::TemplateCommand,
    },
    /// Revoke this worktree's identity and remove its identity file.
    Leave,
    /// Post a message with any verb.
    Post(post::PostArgs),
    /// Talk to the project's swarm as the signed-in person.
    Chat(chat::ChatArgs),
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
    /// Run a self-hosted hub on this machine.
    Host(host::HostArgs),
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
    /// What to run. Bare `agmen` opens the app only from a terminal, so a
    /// script or agent keeps the usage error it got before the app launch
    /// existed, and a word that is neither a subcommand nor a folder stays an
    /// unknown subcommand.
    fn into_command(self, interactive: bool) -> Result<Command, clap::Error> {
        use clap::CommandFactory as _;
        use clap::error::ErrorKind;
        match (self.command, self.path) {
            (Some(command), _) => Ok(command),
            (None, None) if !interactive => Err(Self::command().error(
                ErrorKind::MissingSubcommand,
                "'agmen' requires a subcommand when not run from a terminal; use agmen open to launch the desktop app",
            )),
            (None, Some(path)) if !path.exists() => Err(Self::command().error(
                ErrorKind::InvalidSubcommand,
                format!(
                    "unrecognized subcommand or folder '{}'",
                    path.display()
                ),
            )),
            (None, path) => Ok(Command::Open { path }),
        }
    }

    /// Runs the parsed command.
    pub(crate) async fn run(self) -> Result<()> {
        use std::io::IsTerminal as _;
        let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        let command = self
            .into_command(interactive)
            .unwrap_or_else(|error| error.exit());
        match command {
            #[cfg(unix)]
            Command::Listener(args) => listener::run(args).await,
            #[cfg(unix)]
            Command::Settings(args) => settings::run(args).await,
            #[cfg(unix)]
            Command::Launch(args) => workers::launch(args).await,
            #[cfg(unix)]
            Command::Workers(args) => workers::workers(args).await,
            #[cfg(unix)]
            Command::Worktree { command } => workers::worktree(command).await,
            #[cfg(unix)]
            Command::Budget(args) => workers::budget(args).await,
            Command::Open { path } => open::run(path).await,
            Command::InstallReceipt(args) => install_receipt::run(args),
            Command::Login(args) => account::login(args).await,
            Command::Logout => account::logout().await,
            Command::Whoami { json } => account::whoami(json).await,
            Command::Project { command } => account::project(command).await,
            Command::Group(args) => group::run(args).await,
            Command::Invite(args) => account::invite(args).await,
            Command::Join(args) => join::join(args).await,
            Command::Init(args) => init::run(args).await,
            Command::Setup { agent, no_plugin } => {
                let joined = crate::joined::joined().await?;
                let plugin =
                    crate::client::blocking(move || crate::setup::plugin_choice(no_plugin)).await?;
                let changes =
                    crate::setup::run(&joined.root, &joined.identity, agent, plugin).await?;
                if changes.is_empty() {
                    println!(
                        "{}",
                        crate::output::clean("no agents found: install one, then run agmen setup")
                    );
                }
                for change in changes {
                    println!("{}", crate::output::clean(&change.to_string()));
                }
                Ok(())
            }
            Command::Role { command } => role::run(command).await,
            Command::Templates { command } => templates::run(command).await,
            Command::Leave => join::leave().await,
            Command::Post(args) => post::post(args).await,
            Command::Chat(args) => chat::chat(args).await,
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
            Command::Host(args) => host::run(args).await,
        }
    }
}
