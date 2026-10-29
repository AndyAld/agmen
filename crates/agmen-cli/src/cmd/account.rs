//! `agmen login`, `logout`, `whoami` and `invite`: the signed-in person.

use std::time::Duration;

use agmen_client::find_identity;
use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::client::{
    Api, Human, Login, blocking, forget_login, http, human, read_login, refusal, save_login, send,
    token_grant, url,
};
use crate::output::{Fail, clean};

/// RFC 8628 section 3.4.
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// RFC 8628 section 3.5: a `slow_down` adds this to the interval.
const SLOW_DOWN: Duration = Duration::from_secs(5);
/// The most polls one login makes, whatever the hub says about expiry.
const MAX_POLLS: u32 = 720;
/// The shortest and longest invite: 1 hour and 30 days (ruling R58).
const INVITE_SECS: std::ops::RangeInclusive<u64> = 3600..=30 * 24 * 3600;

#[derive(Debug, Args)]
pub(crate) struct LoginArgs {
    /// The hub's base URL: https, or http on localhost or a loopback IP. Defaults to
    /// AGMEN_HUB, then to the hub you last signed in at.
    #[arg(long, env = "AGMEN_HUB", value_name = "URL", value_parser = HubUrlParser)]
    hub: Option<Url>,
}

/// Keeps clap from echoing URL credentials in a validation error.
#[derive(Debug, Clone)]
struct HubUrlParser;

impl clap::builder::TypedValueParser for HubUrlParser {
    type Value = Url;

    fn parse_ref(
        &self,
        _cmd: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Url, clap::Error> {
        let invalid = || {
            clap::Error::raw(
                clap::error::ErrorKind::ValueValidation,
                "invalid hub URL; use https",
            )
        };
        let url: Url = value
            .to_str()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        agmen_client::check_hub_url(&url).map_err(|error| {
            clap::Error::raw(clap::error::ErrorKind::ValueValidation, error.to_string())
        })?;
        Ok(url)
    }
}

/// RFC 8628 section 3.2.
#[derive(Debug, Deserialize)]
struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: Url,
    expires_in: u64,
    interval: u64,
}

/// `agmen login`.
pub(crate) async fn login(args: LoginArgs) -> Result<()> {
    let hub = match args.hub {
        Some(hub) => hub,
        None => read_login()
            .await?
            .map(|login| login.hub_url)
            .ok_or_else(|| {
                Fail::usage("name the hub: agmen login --hub <url>, or set AGMEN_HUB")
            })?,
    };
    let human = device_login(&hub).await?;
    println!("signed in as {}", clean(&human.login.handle));
    Ok(())
}

/// Signs in with the hub's device flow and returns the freshly saved login.
pub(crate) async fn device_login(hub: &Url) -> Result<Human> {
    agmen_client::check_hub_url(hub)?;
    let start: DeviceStart = send(http()?.post(url(hub, &["auth", "device"])))
        .await
        .context("starting the device login failed")?;
    println!("Your code is {}", clean(&start.user_code));
    println!(
        "Open {} and enter the code. It expires in {} minutes.",
        clean(start.verification_uri.as_str()),
        start.expires_in / 60
    );
    blocking(move || Ok(crate::open::open_url(&start.verification_uri))).await?;
    let mut interval = Duration::from_secs(start.interval.max(1));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(start.expires_in);
    let mut tokens = None;
    for _ in 0..MAX_POLLS {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() > deadline {
            break;
        }
        let grant = token_grant(
            hub,
            &[
                ("grant_type", DEVICE_CODE_GRANT),
                ("device_code", &start.device_code),
            ],
        )
        .await;
        match grant {
            Ok(granted) => {
                tokens = Some(granted);
                break;
            }
            Err(err) => match refusal(&err).map(|r| r.code.as_str()) {
                Some("authorization_pending") => {}
                Some("slow_down") => interval += SLOW_DOWN,
                Some("expired_token") => break,
                Some("access_denied") if refusal(&err).is_some_and(|r| r.status == 400) => {
                    bail!("the sign-in was declined on GitHub; run agmen login again");
                }
                _ => return Err(err.context("signing in failed")),
            },
        }
    }
    let Some(tokens) = tokens else {
        bail!("the code expired before it was confirmed; run agmen login again");
    };
    let api = Api::new(hub.clone(), tokens.access_token())?;
    let me = api.me().await.context("reading who signed in failed")?;
    let login = Login {
        hub_url: hub.clone(),
        human_id: me.id,
        handle: me.handle,
    };
    save_login(&login, &tokens).await?;
    Ok(Human { login, api })
}

/// `agmen logout`.
pub(crate) async fn logout() -> Result<()> {
    match forget_login().await? {
        Some(login) => println!(
            "signed out of {} as {}",
            login.hub_url,
            clean(&login.handle)
        ),
        None => println!("not signed in"),
    }
    Ok(())
}

/// `agmen whoami`.
pub(crate) async fn whoami(json: bool) -> Result<()> {
    let human = human().await?;
    let me = human
        .api
        .me()
        .await
        .context("reading who is signed in failed")?;
    if json {
        println!("{}", serde_json::to_string(&me)?);
        return Ok(());
    }
    println!(
        "{} ({}) at {}",
        clean(&me.handle),
        me.id,
        human.login.hub_url
    );
    if me.projects.is_empty() {
        println!("no projects yet: accept an invite with agmen join <invite link>");
    }
    for project in &me.projects {
        println!(
            "  project {} ({}, tenant {}): {}",
            clean(&project.project),
            clean(&project.name),
            clean(&project.tenant),
            clean(&project.role)
        );
    }
    for group in &me.groups {
        println!(
            "  group {} ({}): {}",
            clean(&group.name),
            clean(&group.tenant),
            clean(&group.role)
        );
    }
    let cwd = std::env::current_dir().context("reading the working directory")?;
    match blocking(move || Ok(find_identity(&cwd))).await? {
        Ok(Some(joined)) => println!(
            "this worktree: {} ({}) in project {}",
            clean(&joined.identity.handle),
            joined.identity.identity_id,
            joined.identity.project
        ),
        Ok(None) => {}
        Err(err) => println!("this worktree: {}", clean(&err.to_string())),
    }
    Ok(())
}

/// The role an invite gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum InviteRole {
    Member,
    Coordinator,
}

#[derive(Debug, Args)]
pub(crate) struct InviteArgs {
    /// The project's id. Defaults to this worktree's project, then to your
    /// only project.
    #[arg(long, value_name = "ID")]
    project: Option<String>,
    /// The role the invite gives.
    #[arg(long, value_enum, default_value = "member")]
    role: InviteRole,
    /// How long the invite stays open: a number with s, m, h or d, from 1h
    /// to 30d.
    #[arg(long, value_name = "DURATION", default_value = "7d", value_parser = parse_expiry)]
    expires: u64,
}

/// `7d`, `12h`, `90m` or `3600s` as seconds, from 1 hour to 30 days.
fn parse_expiry(text: &str) -> Result<u64, String> {
    let bad = || format!("{text:?} is not a duration such as 12h or 7d");
    let split = text.len().checked_sub(1).ok_or_else(bad)?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().map_err(|_| bad())?;
    let unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return Err(bad()),
    };
    let secs = number.checked_mul(unit).ok_or_else(bad)?;
    if INVITE_SECS.contains(&secs) {
        Ok(secs)
    } else {
        Err("an invite lasts from 1h to 30d".to_owned())
    }
}

#[derive(Debug, Serialize)]
struct CreateInvite<'a> {
    project: &'a str,
    role: InviteRole,
    expires_in_secs: u64,
}

#[derive(Debug, Deserialize)]
struct Created {
    link: Url,
    project: String,
    expires_at: String,
}

/// `agmen invite`.
pub(crate) async fn invite(args: InviteArgs) -> Result<()> {
    let human = human().await?;
    let project = match args.project {
        Some(project) => project,
        None => default_project(&human).await?,
    };
    let request = CreateInvite {
        project: &project,
        role: args.role,
        expires_in_secs: args.expires,
    };
    let created: Created = match human.api.post(&["invites"], &request).await {
        Ok(created) => created,
        Err(err) => {
            return Err(match refusal(&err).map(|r| r.code.as_str()) {
                Some("coordinator_required") => anyhow::anyhow!(
                    "only a project admin or coordinator can invite to project {project}"
                ),
                Some("admin_required") => {
                    anyhow::anyhow!("only a project admin can invite a coordinator")
                }
                _ => err.context("making the invite failed"),
            });
        }
    };
    println!(
        "single-use invite to project {} as {}, open until {}:",
        clean(&created.project),
        match args.role {
            InviteRole::Member => "member",
            InviteRole::Coordinator => "coordinator",
        },
        clean(&created.expires_at)
    );
    println!("  {}", clean(created.link.as_str()));
    println!("They run: agmen join {}", clean(created.link.as_str()));
    if blocking(move || Ok(crate::open::copy_text(created.link.as_str()))).await? {
        println!("(copied to the clipboard)");
    }
    println!("Send it. They open it and follow the steps on the page.");
    Ok(())
}

/// This worktree's project when it is joined, else the person's only
/// project.
async fn default_project(human: &crate::client::Human) -> Result<String> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    if let Ok(Some(joined)) = blocking(move || Ok(find_identity(&cwd))).await? {
        return Ok(joined.identity.project.to_string());
    }
    let me = human
        .api
        .me()
        .await
        .context("reading your projects failed")?;
    match me.projects.as_slice() {
        [only] => Ok(only.project.clone()),
        _ => Err(Fail::usage(
            "name the project: agmen invite --project <id> (agmen whoami lists them)",
        )
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_url_policy_argument_parser_accepts_safe_urls() {
        use clap::builder::TypedValueParser as _;
        for hub in [
            "http://localhost",
            "http://127.0.0.1",
            "http://127.2.3.4",
            "http://[::1]",
            "https://hub.example",
        ] {
            assert_eq!(
                HubUrlParser
                    .parse_ref(
                        &clap::Command::new("agmen"),
                        None,
                        std::ffi::OsStr::new(hub)
                    )
                    .unwrap(),
                hub.parse::<Url>().unwrap()
            );
        }
    }

    #[tokio::test]
    async fn hub_url_policy_login() {
        for hub in ["http://hub.example", "ftp://localhost"] {
            let error = login(LoginArgs {
                hub: Some(hub.parse().unwrap()),
            })
            .await
            .unwrap_err();
            assert!(error.to_string().contains("use https"), "{error:?}");
        }
    }

    #[test]
    fn expiries() {
        assert_eq!(parse_expiry("7d"), Ok(604_800));
        assert_eq!(parse_expiry("1h"), Ok(3600));
        assert_eq!(parse_expiry("3600s"), Ok(3600));
        assert_eq!(parse_expiry("30d"), Ok(2_592_000));
        for bad in ["", "d", "59m", "31d", "7", "7w", "-1h", "soon"] {
            assert!(parse_expiry(bad).is_err(), "{bad}");
        }
    }
}

/// Project management commands.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum ProjectCommand {
    /// Create a project in a tenant where you administer a project.
    Create {
        name: String,
        /// Required when you administer projects in several tenants.
        #[arg(long)]
        tenant: Option<agmen_proto::TenantId>,
    },
}

pub(crate) async fn project(command: ProjectCommand) -> Result<()> {
    let ProjectCommand::Create { name, tenant } = command;
    let human = human().await?;
    #[derive(Serialize)]
    struct Request {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tenant: Option<agmen_proto::TenantId>,
    }
    let created: crate::client::Membership = human
        .api
        .post(&["projects"], &Request { name, tenant })
        .await
        .context("creating the project failed")?;
    println!("{}", serde_json::to_string(&created)?);
    Ok(())
}
