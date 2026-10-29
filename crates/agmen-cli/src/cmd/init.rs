//! Start a hosted project or hand off to self hosting.

use std::borrow::Cow;
use std::io::{BufRead, IsTerminal as _, Write};
use std::path::Path;
use std::process::Stdio;

use agmen_proto::{RepoUrl, Timestamp};
use anyhow::{Context as _, Result, bail};
use clap::Args;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use url::Url;

use super::{account, group, host::onboard, join};
use crate::client::{self, Group, Human, blocking, refusal};
use crate::git;
use crate::output::{Fail, clean, print_notices};

/// Default hosted hub, overridden by `--hub` or `AGMEN_HOSTED_HUB`.
pub(crate) const HOSTED_HUB: &str = "https://api.getagmen.ai";

const HOSTED_HUB_ENV: &str = "AGMEN_HOSTED_HUB";
const ANSWER_CAP: u64 = 64;
const LIST_CAP: usize = 1000;
const GROUP_NAME_CAP: usize = 39;
const NAME_THE_GROUP: &str = "name the group: --group <name or id>";
const NEW_GROUP_PROMPT: &str =
    "This creates a new group and starts your one free trial. Continue? [y/N] ";

/// Start this worktree on a self-hosted or agmen hosted hub.
#[derive(Debug, Args)]
pub(crate) struct InitArgs {
    /// Start your own hub with agmen host up.
    #[arg(long, conflicts_with = "hosted")]
    self_host: bool,
    /// Use agmen hosted.
    #[arg(long)]
    hosted: bool,
    /// Hosted hub URL (default: AGMEN_HOSTED_HUB, then agmen hosted).
    // No clap `env` here: clap counts an env value as given, so a hosted user
    // with AGMEN_HOSTED_HUB exported could not pass --self-host.
    #[arg(long, conflicts_with = "self_host")]
    hub: Option<Url>,
    /// Add a project to this group by exact name or tenant id.
    #[arg(long, conflicts_with_all = ["self_host", "new_group"])]
    group: Option<String>,
    /// Create a new group named after this folder.
    #[arg(long, conflicts_with = "self_host")]
    new_group: bool,
    /// Project name; defaults to this worktree folder's name.
    #[arg(long)]
    name: Option<String>,
    /// Name after your handle and a slash, as in agmen join --handle.
    #[arg(long)]
    handle: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hosting {
    SelfHost,
    Hosted,
}

/// Map init's project name and handle to `agmen host up` arguments.
#[must_use]
pub(crate) fn host_up_args(project: Option<&str>, handle: Option<&str>) -> Vec<String> {
    let mut args = vec!["host".to_owned(), "up".to_owned()];
    if let Some(project) = project {
        args.extend(["--project".to_owned(), project.to_owned()]);
    }
    if let Some(handle) = handle {
        args.extend(["--handle".to_owned(), handle.to_owned()]);
    }
    args
}

/// Initialize this worktree, checking git, choice, and identity before sign-in.
///
/// # Errors
///
/// Exit 2 for usage, an existing identity, or an empty, invalid or taken group
/// name; exit 1 for other failures, including a non-admin group or a self-host
/// hub refusing hosted group creation. Shared sign-in and join errors retain
/// their exit codes. Self hosting exits with the child's code, or 1 on a signal.
pub(crate) async fn run(args: InitArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let root = match git::toplevel(&cwd).await {
        Ok(root) => root,
        Err(err)
            if err.chain().any(|cause| {
                cause.is::<std::io::Error>()
                    || cause.is::<tokio::time::error::Elapsed>()
                    || cause.is::<std::string::FromUtf8Error>()
            }) =>
        {
            return Err(err);
        }
        Err(_) => {
            return Err(Fail::usage("run agmen init inside your project's git repository").into());
        }
    };
    let hosting = if args.self_host {
        Hosting::SelfHost
    } else if args.hosted {
        Hosting::Hosted
    } else if std::io::stdin().is_terminal() {
        blocking(|| choose_hosting(std::io::stdin().lock(), std::io::stdout().lock())).await?
    } else {
        return Err(Fail::usage("choose --self-host or --hosted").into());
    };
    if hosting == Hosting::SelfHost {
        let exe = std::env::current_exe().context("finding agmen to start self hosting")?;
        let status = Command::new(exe)
            .args(host_up_args(args.name.as_deref(), args.handle.as_deref()))
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .status()
            .await
            .context("running agmen host up")?;
        std::process::exit(status.code().unwrap_or(1));
    }
    let to_find = root.clone();
    let existing = blocking(move || {
        agmen_client::find_identity(&to_find).context("reading this worktree's identity")
    })
    .await?;
    if let Some(joined) = existing {
        return Err(Fail::usage(format!(
            "this worktree already joined as {}; run agmen leave first",
            clean(&joined.identity.handle)
        ))
        .into());
    }
    hosted(&root, args).await
}

/// The hosted hub: `--hub`, else `AGMEN_HOSTED_HUB` when set and not empty, else [`HOSTED_HUB`].
fn hosted_hub(flag: Option<Url>, env: Option<std::ffi::OsString>) -> Result<Url> {
    if let Some(hub) = flag {
        return Ok(hub);
    }
    let value = match env {
        Some(value) if !value.is_empty() => value
            .into_string()
            .map_err(|_| Fail::usage(format!("{HOSTED_HUB_ENV} is not valid UTF-8")))?,
        _ => HOSTED_HUB.to_owned(),
    };
    value.parse().map_err(|_| {
        Fail::usage(format!("{HOSTED_HUB_ENV} is not a URL: {}", clean(&value))).into()
    })
}

async fn hosted(root: &Path, args: InitArgs) -> Result<()> {
    let hub = hosted_hub(args.hub, std::env::var_os(HOSTED_HUB_ENV))?;
    let human = match client::human_at(&hub).await? {
        Some(human) => human,
        None => account::device_login(&hub).await?,
    };
    let me = human.api.me().await?;
    // The only place init prints them: the `agmen invite` it runs at the end
    // is told `--no-notices`, and `by_flag` below reuses this `me`.
    print_notices(&me.notices);
    let admin_groups: Vec<_> = me
        .groups
        .iter()
        .filter(|group| matches!(group.role.as_str(), "owner" | "admin"))
        .take(LIST_CAP)
        .cloned()
        .collect();
    let existing = if let Some(flag) = args.group.as_deref() {
        // `me` is already loaded above, so this does not read `/v1/me` again
        // and does not print the same notices twice.
        Some(group::by_flag(&me.groups, flag)?)
    } else if args.new_group || admin_groups.is_empty() {
        if !args.new_group && !me.groups.is_empty() {
            if !std::io::stdin().is_terminal() {
                return Err(Fail::usage(
                    "creating a new group starts your one free trial; pass --new-group to continue",
                )
                .into());
            }
            blocking(|| confirm_new_group(std::io::stdin().lock(), std::io::stdout().lock()))
                .await?;
        }
        None
    } else if admin_groups.len() == 1 {
        admin_groups.into_iter().next()
    } else if std::io::stdin().is_terminal() {
        Some(
            blocking(move || {
                choose_group(
                    &admin_groups,
                    std::io::stdin().lock(),
                    std::io::stdout().lock(),
                )
            })
            .await?,
        )
    } else {
        let mut message = NAME_THE_GROUP.to_owned();
        for group in admin_groups.iter().take(LIST_CAP) {
            message.push_str(&format!(
                "\n{} ({})",
                clean(&group.name),
                clean(&group.tenant)
            ));
        }
        return Err(Fail::usage(message).into());
    };
    let (project, project_name, group, checkout) = match existing {
        Some(group) => {
            if !matches!(group.role.as_str(), "owner" | "admin") {
                bail!("only the group's owner or an admin can add a project");
            }
            let project =
                onboard::create_project(&human, root, &group.tenant, args.name.as_deref()).await?;
            (project.project, project.name, group, None)
        }
        None => create_group(&human, root, args.name.as_deref()).await?,
    };
    let identity = join::join_here(
        &human,
        root,
        &project,
        &join::JoinArgs::with_handle(args.handle),
    )
    .await?;
    println!(
        "joined project {} ({}) as {} ({})",
        clean(&project_name),
        clean(&project),
        clean(&identity.handle),
        clean(identity.identity_id.as_str())
    );
    join::run_setup(root, &identity).await;
    println!("{}", group_summary(&group, checkout.as_deref()));
    if group.state.as_deref() != Some("read_only") {
        onboard::invite(&project).await?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct CreateGroup<'a> {
    name: String,
    project: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_url: Option<RepoUrl>,
}

#[derive(Debug, Deserialize)]
struct CreatedGroup {
    tenant: String,
    group: String,
    project: String,
    state: String,
    trial_ends_at: Option<Timestamp>,
    checkout_url: Option<String>,
}

async fn create_group(
    human: &Human,
    root: &Path,
    name: Option<&str>,
) -> Result<(String, String, Group, Option<String>)> {
    let folder = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let group_name = group_name(folder).ok_or_else(|| Fail::usage(
        "the folder name gives no group name; rename the folder or run agmen init --group <name or id>"
    ))?;
    let name = name.unwrap_or(folder);
    // A missing or invalid origin is optional and must not print an extra line.
    let repo_url = onboard::origin(root).await.ok();
    let created: CreatedGroup = human
        .api
        .post(
            &["groups"],
            &CreateGroup {
                name: group_name,
                project: name,
                repo_url,
            },
        )
        .await
        .map_err(group_error)?;
    let group = Group {
        tenant: created.tenant,
        name: created.group,
        role: "owner".to_owned(),
        state: Some(created.state),
        trial_ends_at: created.trial_ends_at,
    };
    Ok((
        created.project,
        name.to_owned(),
        group,
        created.checkout_url,
    ))
}

fn group_error(error: anyhow::Error) -> anyhow::Error {
    match refusal(&error).map(|r| (r.status, r.code.as_str())) {
        Some((404, "not_hosted")) => anyhow::anyhow!(
            "this hub is not agmen hosted: run agmen init --self-host, or agmen join with an invite"
        ),
        Some((422, "bad_group_name")) => Fail::usage(
            "the folder gives an invalid group name; run from a differently named folder or run agmen init --group <existing>"
        ).into(),
        Some((409, "group_exists")) => Fail::usage(
            "the folder's group name is already taken; run from a differently named folder or run agmen init --group <existing>"
        ).into(),
        _ => error,
    }
}

fn group_name(folder: &str) -> Option<String> {
    let normalized: String = folder
        .chars()
        .take(GROUP_NAME_CAP)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let name = normalized.trim_start_matches('-');
    (!name.is_empty()).then(|| name.to_owned())
}

pub(crate) fn state_words(state: Option<&str>) -> Cow<'_, str> {
    match state {
        Some("read_only") => Cow::Borrowed("read-only"),
        Some("past_due") => Cow::Borrowed("past due"),
        Some(value) => clean(value),
        None => Cow::Borrowed(""),
    }
}

fn group_summary(group: &Group, checkout: Option<&str>) -> String {
    let mut text = format!(
        "group {} ({}): {}",
        clean(&group.name),
        clean(&group.tenant),
        state_words(group.state.as_deref())
    );
    match group.state.as_deref() {
        Some("trial") => {
            if let Some(end) = group.trial_ends_at {
                text.push_str(&format!("\ntrial ends {}", clean(&end.to_string())));
            }
        }
        Some("read_only") => match checkout {
            Some(url) => text.push_str(&format!(
                "\nthe group is read-only until it is paid for; finish checkout: {}",
                clean(url)
            )),
            None => {
                text.push_str("\nthe group is read-only until it is paid for: agmen group billing")
            }
        },
        _ => {}
    }
    text
}

fn read_answer(input: impl BufRead) -> Result<String> {
    let mut bytes = Vec::new();
    input
        .take(ANSWER_CAP + 1)
        .read_until(b'\n', &mut bytes)
        .context("reading the answer")?;
    if bytes.len() as u64 > ANSWER_CAP {
        return Err(Fail::usage("the answer must be at most 64 bytes").into());
    }
    let answer = String::from_utf8(bytes).map_err(|_| Fail::usage("the answer must be UTF-8"))?;
    Ok(answer.trim().to_owned())
}

fn choose_hosting(input: impl BufRead, mut output: impl Write) -> Result<Hosting> {
    writeln!(output, "Self host or agmen hosted?\n1) Self host (free, requires setup)\n2) agmen hosted (7-day free trial)")
        .context("printing the hosting choices")?;
    output.flush().context("flushing the hosting choices")?;
    match read_answer(input)?.as_str() {
        "1" => Ok(Hosting::SelfHost),
        "2" => Ok(Hosting::Hosted),
        _ => Err(Fail::usage("choose 1 for self host or 2 for agmen hosted").into()),
    }
}

fn confirm_new_group(input: impl BufRead, mut output: impl Write) -> Result<()> {
    write!(output, "{NEW_GROUP_PROMPT}").context("printing the new group confirmation")?;
    output
        .flush()
        .context("flushing the new group confirmation")?;
    if matches!(
        read_answer(input)?.to_ascii_lowercase().as_str(),
        "y" | "yes"
    ) {
        Ok(())
    } else {
        Err(Fail::usage("group creation cancelled; pass --new-group to create a group").into())
    }
}

fn choose_group(groups: &[Group], input: impl BufRead, mut output: impl Write) -> Result<Group> {
    for (index, group) in groups.iter().take(LIST_CAP).enumerate() {
        writeln!(
            output,
            "{}) {} ({})",
            index + 1,
            clean(&group.name),
            clean(&group.tenant)
        )
        .context("printing the group choices")?;
    }
    write!(output, "Choose a group: ").context("printing the group prompt")?;
    output.flush().context("flushing the group choices")?;
    let answer = read_answer(input)?;
    let index = answer
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| answer.parse::<usize>().ok())
        .flatten()
        .and_then(|number| number.checked_sub(1))
        .filter(|&index| index < LIST_CAP);
    index
        .and_then(|index| groups.get(index))
        .cloned()
        .ok_or_else(|| Fail::usage("choose one of the listed group numbers").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Group;

    fn group(state: Option<&str>) -> Group {
        Group {
            tenant: "tenant01".into(),
            name: "andy/widgets".into(),
            role: "owner".into(),
            state: state.map(str::to_owned),
            trial_ends_at: None,
        }
    }

    #[test]
    fn host_up_args_map_name_to_project() {
        assert_eq!(
            host_up_args(Some("widgets"), Some("sol")),
            ["host", "up", "--project", "widgets", "--handle", "sol"]
        );
        assert_eq!(host_up_args(None, None), ["host", "up"]);
    }

    #[test]
    fn init_hosted_hub_prefers_flag_then_env_then_default() {
        let flag: Url = "http://127.0.0.1:1/".parse().unwrap();
        let env = || Some("http://127.0.0.1:2/".into());
        assert_eq!(hosted_hub(Some(flag.clone()), env()).unwrap(), flag);
        assert_eq!(
            hosted_hub(None, env()).unwrap().as_str(),
            "http://127.0.0.1:2/"
        );
        assert_eq!(
            hosted_hub(None, None).unwrap().as_str(),
            "https://api.getagmen.ai/"
        );
        assert_eq!(
            hosted_hub(None, Some("".into())).unwrap().as_str(),
            "https://api.getagmen.ai/"
        );
        assert!(hosted_hub(None, Some("not a url".into())).is_err());
    }

    #[test]
    fn init_group_name_from_folder() {
        for (folder, expected) in [
            ("Widgets", Some("widgets")),
            ("my_repo.rs", Some("my-repo-rs")),
            (".tmpAb12", Some("tmpab12")),
            ("___", None),
            ("", None),
            ("--Widgets", Some("widgets")),
            ("éWidgets", Some("widgets")),
        ] {
            assert_eq!(group_name(folder).as_deref(), expected);
        }
        assert_eq!(group_name(&"a".repeat(50)), Some("a".repeat(39)));
        assert_eq!(group_name(&format!("{}a", "-".repeat(39))), None);
    }

    #[test]
    fn init_state_words() {
        for (state, words) in [
            (Some("trial"), "trial"),
            (Some("read_only"), "read-only"),
            (Some("past_due"), "past due"),
            (Some("active"), "active"),
            (None, ""),
            (Some("unknown\u{1b}\n"), "unknown"),
        ] {
            assert_eq!(state_words(state), words);
        }
    }

    #[test]
    fn init_group_refusals_follow_coordinator_rulings() {
        for (status, code, exit, text) in [
            (
                404,
                "not_hosted",
                1,
                "this hub is not agmen hosted: run agmen init --self-host, or agmen join with an invite",
            ),
            (
                422,
                "bad_group_name",
                2,
                "the folder gives an invalid group name; run from a differently named folder or run agmen init --group <existing>",
            ),
            (
                409,
                "group_exists",
                2,
                "the folder's group name is already taken; run from a differently named folder or run agmen init --group <existing>",
            ),
            (403, "group_limit", 1, "you already own 3 groups"),
        ] {
            let error = group_error(
                client::Refusal {
                    status,
                    code: code.into(),
                    description: "you already own 3 groups".into(),
                }
                .into(),
            );
            let (message, actual_exit) = crate::output::explain(&error);
            assert_eq!(actual_exit, exit);
            assert!(message.contains(text), "{message}");
        }
    }

    #[test]
    fn init_read_only_prints_checkout_or_billing() {
        let group = group(Some("read_only"));
        assert_eq!(
            group_summary(&group, None),
            "group andy/widgets (tenant01): read-only\nthe group is read-only until it is paid for: agmen group billing"
        );
        let text = group_summary(&group, Some("https://checkout.example/pay\u{1b}"));
        assert_eq!(
            text,
            "group andy/widgets (tenant01): read-only\nthe group is read-only until it is paid for; finish checkout: https://checkout.example/pay"
        );
        assert!(!text.contains('\u{1b}'));
        let mut trial = self::group(Some("trial"));
        trial.trial_ends_at = Some("2026-11-05T00:00:00Z".parse().unwrap());
        assert_eq!(
            group_summary(&trial, None),
            "group andy/widgets (tenant01): trial\ntrial ends 2026-11-05T00:00:00Z"
        );
    }

    #[test]
    fn init_member_terminal_confirms_before_new_group() {
        for answer in ["y\n", "Y\n", "yes\n"] {
            let mut output = Vec::new();
            confirm_new_group(answer.as_bytes(), &mut output).unwrap();
            assert_eq!(
                String::from_utf8(output).unwrap(),
                "This creates a new group and starts your one free trial. Continue? [y/N] "
            );
        }
        for answer in ["", "\n", "n\n", "no\n", "maybe\n"] {
            let error = confirm_new_group(answer.as_bytes(), Vec::new()).unwrap_err();
            assert!(error.downcast_ref::<crate::output::Fail>().is_some());
        }
        assert!(
            confirm_new_group(format!("y{}\n", " ".repeat(64)).as_bytes(), Vec::new()).is_err()
        );
    }

    #[test]
    fn init_terminal_choices_are_bounded_and_sanitized() {
        assert_eq!(
            choose_hosting("1\n".as_bytes(), Vec::new()).unwrap(),
            Hosting::SelfHost
        );
        assert_eq!(
            choose_hosting("2\n".as_bytes(), Vec::new()).unwrap(),
            Hosting::Hosted
        );
        assert!(choose_hosting("3\n".as_bytes(), Vec::new()).is_err());
        let mut one = group(Some("active"));
        one.name.push('\u{1b}');
        let groups = [one, group(Some("trial"))];
        let mut output = Vec::new();
        assert_eq!(
            choose_group(&groups, "2\n".as_bytes(), &mut output)
                .unwrap()
                .state
                .as_deref(),
            Some("trial")
        );
        assert!(!String::from_utf8(output).unwrap().contains('\u{1b}'));
        for answer in ["", "0\n", "3\n", "andy/widgets\n", "+1\n"] {
            assert!(choose_group(&groups, answer.as_bytes(), Vec::new()).is_err());
        }
        assert!(read_answer("a".repeat(65).as_bytes()).is_err());
        assert!(read_answer(&b"\xff\n"[..]).is_err());
    }
}
