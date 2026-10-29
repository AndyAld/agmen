//! `agmen group members|remove|role|transfer|revoke|seats|billing`.

use std::borrow::Cow;

use agmen_client::find_identity;
use anyhow::{Context as _, Result, anyhow};
use clap::{Args, Subcommand, ValueEnum};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::init::state_words;
use crate::client::{self, Human, blocking, refusal, send};
use crate::output::{Code, Fail, NOTICE_READS, clean, print_notices};

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
    /// Show paid seats, or add or remove them.
    Seats {
        #[command(subcommand)]
        action: Option<SeatsAction>,
        #[command(flatten)]
        group: GroupFlag,
    },
    /// Open the billing portal, or start a Team or Business plan.
    Billing {
        #[command(flatten)]
        group: GroupFlag,
        /// Start `team` or `business` instead of following the group's state.
        #[arg(long, value_enum)]
        plan: Option<PlanChoice>,
        /// How many seats to buy with `--plan`. From 1. The hub caps this at 50.
        #[arg(long, requires = "plan", value_parser = clap::value_parser!(u32).range(1..))]
        seats: Option<u32>,
    },
}

#[derive(Debug, Subcommand)]
enum SeatsAction {
    /// Buy more seats.
    Add {
        /// How many seats to add, from 1 to 50.
        #[arg(value_parser = clap::value_parser!(u32).range(1..=50))]
        n: u32,
    },
    /// Drop paid seats.
    Remove {
        /// How many seats to remove, from 1 to 50.
        #[arg(value_parser = clap::value_parser!(u32).range(1..=50))]
        n: u32,
    },
}

/// `--group` on every subcommand except `revoke`.
#[derive(Debug, Args)]
struct GroupFlag {
    /// Group name or id. Defaults to this worktree's group, or your only group.
    #[arg(long, global = true, value_name = "name or id")]
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
/// A usage error exits 2, including these seat and billing cases: `this hub
/// is self-hosted: no seats or billing`, `this group has no paid plan, so it
/// has no seats or billing`, `<group> has no seats or billing`, and `the
/// group has only <paid> paid seats`. `not_a_group_member` exits 3. Any
/// other hub refusal exits 1.
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
        GroupCommand::Seats { action, group } => {
            seats(&human, action, group.group.as_deref()).await
        }
        GroupCommand::Billing { group, plan, seats } => {
            billing(&human, group.group.as_deref(), plan, seats).await
        }
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
    print_notices(&me.notices);
    match flag {
        Some(value) => by_flag(&me.groups, value),
        None => by_default(&me.groups).await,
    }
}

pub(crate) fn by_flag(groups: &[client::Group], value: &str) -> Result<client::Group> {
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
    // Revoke does not read `/v1/me` for its own work. One best-effort read
    // prints owner notices; its failure must not change this success.
    print_best_effort_notices(human).await;
    Ok(())
}

/// Reads `/v1/me` once and prints owner notices. The command has already
/// succeeded, so a failed read is dropped and not retried.
pub(crate) async fn print_best_effort_notices(human: &Human) {
    for _ in 0..NOTICE_READS {
        // The command succeeded. A notice is optional, so a failed read
        // does not change the exit code and is not retried.
        if let Ok(me) = human.api.me().await {
            print_notices(&me.notices);
            return;
        }
    }
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

/// A self-hosted hub has no seats or billing.
const SELF_HOSTED: &str = "this hub is self-hosted: no seats or billing";

/// A hosted group with no plan row has no seats or billing.
const NO_PAID_PLAN: &str = "this group has no paid plan, so it has no seats or billing";

/// Reads of `GET /seats` when `/v1/me` omitted `state`. One call, no retry.
const ABSENT_STATE_READS: usize = 1;

/// Plan checkouts after a read-only portal refusal. One step, no retry.
const READ_ONLY_FALLBACKS: usize = 1;

/// `team` or `business` for `agmen group billing --plan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PlanChoice {
    Team,
    Business,
}

impl PlanChoice {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Team => "team",
            Self::Business => "business",
        }
    }
}

/// What `agmen group billing` does for one group state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BillingStep {
    Portal,
    Plan {
        name: &'static str,
        seats: Option<u32>,
    },
    PortalThenPlan,
    Absent,
    Unsupported,
}

/// How many paid seats to put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeatDelta {
    Add(u32),
    Remove(u32),
}

/// `PUT /seats` or `POST /plan`: a checkout link, or the row the hub stored.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Billed {
    Checkout {
        checkout_url: String,
    },
    Stored {
        plan: Option<String>,
        seats_paid: u32,
    },
}

#[derive(Debug, Deserialize)]
struct SeatsView {
    used: u32,
    paid: u32,
    plan: Option<String>,
    state: String,
    quota: u32,
    used_today: u32,
}

#[derive(Debug, Serialize)]
struct PaidBody {
    paid: u32,
}

#[derive(Debug, Serialize)]
struct PlanBody<'a> {
    plan: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    seats: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct PortalBody {
    portal_url: String,
}

/// A group with no `state` has no plan row, so there is nothing to bill, with
/// or without `--plan`. Otherwise `--plan` wins and the group's state picks
/// the call.
fn billing_step(state: Option<&str>, plan: Option<PlanChoice>, seats: Option<u32>) -> BillingStep {
    let Some(state) = state else {
        return BillingStep::Absent;
    };
    if let Some(choice) = plan {
        return BillingStep::Plan {
            name: choice.as_str(),
            seats,
        };
    }
    match state {
        "active" | "past_due" => BillingStep::Portal,
        "trial" => BillingStep::Plan {
            name: "team",
            seats: None,
        },
        "read_only" => BillingStep::PortalThenPlan,
        _ => BillingStep::Unsupported,
    }
}

/// The next paid count, or a usage error when it does not fit.
fn seat_target(paid: u32, delta: SeatDelta) -> Result<u32, Fail> {
    match delta {
        SeatDelta::Add(n) => paid
            .checked_add(n)
            .ok_or_else(|| Fail::usage("that many seats does not fit")),
        SeatDelta::Remove(n) => paid
            .checked_sub(n)
            .ok_or_else(|| Fail::usage(format!("the group has only {paid} paid seats"))),
    }
}

/// Plan words for a seat line. A null plan during a trial is `trial`.
fn plan_words<'a>(plan: Option<&'a str>, state: Option<&str>) -> Cow<'a, str> {
    match plan {
        Some(plan) => clean(plan),
        None if state == Some("trial") => Cow::Borrowed("trial"),
        None => Cow::Borrowed("no plan"),
    }
}

/// The two lines `agmen group seats` prints.
fn seats_lines(
    used: u32,
    paid: u32,
    plan: Option<&str>,
    state: &str,
    quota: u64,
    used_today: u64,
) -> (String, String) {
    let seats = format!(
        "seats: {used} used of {paid} paid ({}, {})",
        plan_words(plan, Some(state)),
        state_words(Some(state))
    );
    let today = format!("today: {used_today} of {quota} messages");
    (seats, today)
}

/// The stdout line for a seat change, or the error when nothing was stored.
fn seat_change_outcome(
    group: &str,
    requested: u32,
    billed: &Billed,
    state: Option<&str>,
) -> Result<String> {
    match billed {
        Billed::Checkout { checkout_url } => Ok(format!(
            "Finish checkout to add the seats: {}",
            clean(checkout_url)
        )),
        Billed::Stored { plan, seats_paid } if plan.is_none() && *seats_paid != requested => {
            Err(anyhow!(
                "no seats changed: {} has no paid plan yet; agmen group billing starts the Team plan, which includes 4 seats",
                clean(group)
            ))
        }
        Billed::Stored { plan, seats_paid } => Ok(format!(
            "seats: {seats_paid} paid ({})",
            plan_words(plan.as_deref(), state)
        )),
    }
}

/// Maps a seat or billing refusal. Unknown codes use [`group_error`].
fn billing_error(err: anyhow::Error, group_name: &str, what: &'static str) -> anyhow::Error {
    let kind = refusal(&err).map(|r| (r.status, r.code.clone(), r.description.clone()));
    let Some((status, code, description)) = kind else {
        return group_error(err, "", what);
    };
    let name = clean(group_name);
    match (status, code.as_str()) {
        (404, "not_hosted") => Fail::usage(SELF_HOSTED).into(),
        (404, "not_found") => Fail::usage(format!("{name} has no seats or billing")).into(),
        (403, "owner_required") => anyhow!("only the group's owner can change billing"),
        (409, "seats_in_use") => anyhow!(
            "the group has more members than that: remove members first (agmen group members lists them)"
        ),
        // The hub sends this code in two cases: it put Paddle's seats back
        // ("a member joined while seats were changing"), or it could not and
        // kept Paddle's lower count ("... Paddle could not be restored").
        // So the line does not say whether anything was stored.
        (409, "seats_below_used") => anyhow!(
            "{}; check agmen group seats and try again",
            clean(&description)
        ),
        (409, "no_subscription") | (422, "seats_over_50") => anyhow!("{}", clean(&description)),
        (429, "rate_limited") => {
            anyhow!("Paddle calls for this group are limited; try again in a minute")
        }
        (502, "paddle_bad_url") => anyhow!(
            "Paddle returned a link the hub will not pass on; nothing changed, try again later"
        ),
        (502, "upstream_unavailable") => anyhow!("Paddle did not answer; try again later"),
        _ => group_error(err, "", what),
    }
}

/// A `GET /seats` failure when `/v1/me` sent no state.
fn absent_state_failure(err: anyhow::Error, group_name: &str) -> anyhow::Error {
    let kind = refusal(&err).map(|r| (r.status, r.code.clone()));
    match kind.as_ref().map(|(status, code)| (*status, code.as_str())) {
        Some((404, "not_hosted")) => Fail::usage(SELF_HOSTED).into(),
        Some((404, "not_found")) => Fail::usage(NO_PAID_PLAN).into(),
        _ => billing_error(err, group_name, "reading seats failed"),
    }
}

/// `<group> is <state>: no billing to show`.
fn no_billing_to_show(group: &str, state: &str) -> anyhow::Error {
    anyhow!(
        "{} is {}: no billing to show",
        clean(group),
        state_words(Some(state))
    )
}

fn is_no_subscription(err: &anyhow::Error) -> bool {
    refusal(err).is_some_and(|r| r.status == 409 && r.code == "no_subscription")
}

async fn seats(human: &Human, action: Option<SeatsAction>, flag: Option<&str>) -> Result<()> {
    let group = resolve_group(human, flag).await?;
    if group.state.is_none() {
        return reject_absent(human, &group).await;
    }
    match action {
        None => show_seats(human, &group).await,
        Some(SeatsAction::Add { n }) => change_seats(human, &group, SeatDelta::Add(n)).await,
        Some(SeatsAction::Remove { n }) => change_seats(human, &group, SeatDelta::Remove(n)).await,
    }
}

async fn show_seats(human: &Human, group: &client::Group) -> Result<()> {
    let view = fetch_seats(human, group).await?;
    let (seats, today) = seats_lines(
        view.used,
        view.paid,
        view.plan.as_deref(),
        &view.state,
        u64::from(view.quota),
        u64::from(view.used_today),
    );
    println!("{seats}");
    println!("{today}");
    Ok(())
}

async fn change_seats(human: &Human, group: &client::Group, delta: SeatDelta) -> Result<()> {
    let view = fetch_seats(human, group).await?;
    let paid = seat_target(view.paid, delta)?;
    let billed: Billed = send(
        human
            .api
            .request(Method::PUT, &["tenants", &group.tenant, "seats"])
            .json(&PaidBody { paid }),
    )
    .await
    .map_err(|err| billing_error(err, &group.name, "changing seats failed"))?;
    println!(
        "{}",
        seat_change_outcome(&group.name, paid, &billed, group.state.as_deref())?
    );
    Ok(())
}

async fn fetch_seats(human: &Human, group: &client::Group) -> Result<SeatsView> {
    human
        .api
        .get(&["tenants", &group.tenant, "seats"])
        .await
        .map_err(|err| billing_error(err, &group.name, "reading seats failed"))
}

/// One `GET /seats` when the group has no state. A 200 is treated as no plan.
async fn reject_absent(human: &Human, group: &client::Group) -> Result<()> {
    let mut failure = None;
    for _ in 0..ABSENT_STATE_READS {
        match human
            .api
            .get::<SeatsView>(&["tenants", &group.tenant, "seats"])
            .await
        {
            Ok(_) => return Err(Fail::usage(NO_PAID_PLAN).into()),
            Err(err) => failure = Some(err),
        }
    }
    match failure {
        Some(err) => Err(absent_state_failure(err, &group.name)),
        None => Err(anyhow!("reading seats failed")),
    }
}

async fn billing(
    human: &Human,
    flag: Option<&str>,
    plan: Option<PlanChoice>,
    seats: Option<u32>,
) -> Result<()> {
    let group = resolve_group(human, flag).await?;
    match billing_step(group.state.as_deref(), plan, seats) {
        BillingStep::Absent => reject_absent(human, &group).await,
        BillingStep::Unsupported => Err(unsupported_billing(&group)),
        BillingStep::Portal => open_portal(human, &group).await,
        BillingStep::Plan { name, seats } => print_plan(human, &group, name, seats).await,
        BillingStep::PortalThenPlan => portal_then_plan(human, &group).await,
    }
}

fn unsupported_billing(group: &client::Group) -> anyhow::Error {
    match group.state.as_deref() {
        Some(state) => no_billing_to_show(&group.name, state),
        None => anyhow!("this group has no billing to show"),
    }
}

async fn open_portal(human: &Human, group: &client::Group) -> Result<()> {
    let portal: PortalBody = human
        .api
        .post(
            &["tenants", &group.tenant, "portal"],
            &serde_json::json!({}),
        )
        .await
        .map_err(|err| billing_error(err, &group.name, "opening the billing portal failed"))?;
    println!("billing portal: {}", clean(&portal.portal_url));
    Ok(())
}

async fn print_plan(
    human: &Human,
    group: &client::Group,
    plan: &str,
    seats: Option<u32>,
) -> Result<()> {
    let billed: Billed = human
        .api
        .post(
            &["tenants", &group.tenant, "plan"],
            &PlanBody { plan, seats },
        )
        .await
        .map_err(|err| billing_error(err, &group.name, "changing the plan failed"))?;
    match billed {
        Billed::Checkout { checkout_url } => {
            println!("finish checkout: {}", clean(&checkout_url));
        }
        Billed::Stored { plan, seats_paid } => {
            println!(
                "{} is on {} with {seats_paid} paid seats",
                clean(&group.name),
                plan_words(plan.as_deref(), group.state.as_deref())
            );
        }
    }
    Ok(())
}

async fn portal_then_plan(human: &Human, group: &client::Group) -> Result<()> {
    match human
        .api
        .post::<_, PortalBody>(
            &["tenants", &group.tenant, "portal"],
            &serde_json::json!({}),
        )
        .await
    {
        Ok(portal) => {
            println!("billing portal: {}", clean(&portal.portal_url));
            Ok(())
        }
        Err(err) if is_no_subscription(&err) && READ_ONLY_FALLBACKS == 1 => {
            print_plan(human, group, "team", None).await
        }
        Err(err) => Err(billing_error(
            err,
            &group.name,
            "opening the billing portal failed",
        )),
    }
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
