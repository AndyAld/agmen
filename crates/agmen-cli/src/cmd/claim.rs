//! `agmen claim`, `release`, `heartbeat` and `done`: the claims this
//! worktree's identity holds. A GRANT records the commit checked out at
//! that moment in `claims.db`; DONE diffs from it.

use agmen_proto::{ItemKey, Payload, Timestamp};
use anyhow::{Context as _, Result, bail};
use clap::Args;
use jiff::SignedDuration;

use super::Wait;
use super::post::body;
use crate::claims::Claims;
use crate::client::blocking;
use crate::git;
use crate::joined::{Agent, agent, envelope};
use crate::output::{Code, Fail, println_flush};

/// How long a CLAIM stays valid on its way to the hub (the hub allows 120 s
/// of clock skew past it).
const CLAIM_VALID: SignedDuration = SignedDuration::from_secs(60);

#[derive(Debug, Args)]
pub(crate) struct ClaimArgs {
    /// The item, such as B-412.
    item: ItemKey,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen claim`: exits 0 on GRANT, 4 on DENY.
pub(crate) async fn claim(args: ClaimArgs) -> Result<()> {
    let agent = agent().await?;
    let base = git::commit(&agent.root, "HEAD")
        .await
        .context("a claim records the commit it starts from, and HEAD has none")?;
    let mut env = envelope(
        Payload::Claim {
            item: args.item.clone(),
        },
        String::new(),
    )?;
    env.valid_until = Some(
        Timestamp::now()
            .checked_add(CLAIM_VALID)
            .context("the clock is out of range")?,
    );
    let recorded = agent.publish(env, args.wait.wait).await?;
    Agent::print_recorded(&recorded)?;
    let item = args.item;
    match agent.hub_answer(&recorded).await?.map(|m| m.client) {
        Some(answer) => match answer.payload {
            Payload::Grant {
                item: granted,
                lease_until,
            } => {
                with_claims(&agent, move |claims| claims.record(&item, &base)).await?;
                println_flush(&format!("granted {granted} until {lease_until}"))
                    .context("writing to stdout")
            }
            Payload::Deny { item, reason } => {
                let reason = serde_json::to_value(reason)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                Err(Fail::new(
                    Code::Rejected,
                    format!("denied {item}: {reason} ({})", answer.body),
                )
                .into())
            }
            _ => bail!("the hub answered the CLAIM with something other than GRANT or DENY"),
        },
        None => {
            // Recorded, so the hub has decided; the answer will show in the
            // inbox. Keep the base so DONE can diff from it.
            with_claims(&agent, move |claims| claims.record(&item, &base)).await?;
            println_flush("no GRANT or DENY found yet: check agmen inbox")
                .context("writing to stdout")
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct ReleaseArgs {
    /// The item.
    item: ItemKey,
    /// Why, for the room.
    #[arg(long, value_name = "TEXT")]
    reason: Option<String>,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen release`.
pub(crate) async fn release(args: ReleaseArgs) -> Result<()> {
    let agent = agent().await?;
    let payload = Payload::Release {
        item: args.item.clone(),
        reason: args.reason,
    };
    let recorded = agent
        .publish(envelope(payload, String::new())?, args.wait.wait)
        .await?;
    let item = args.item;
    with_claims(&agent, move |claims| claims.remove(&item)).await?;
    Agent::print_recorded(&recorded)
}

/// `agmen heartbeat`: one HEARTBEAT naming every claim `claims.db` holds.
pub(crate) async fn heartbeat(wait: Wait) -> Result<()> {
    let agent = agent().await?;
    let items = with_claims(&agent, |claims| claims.items()).await?;
    if items.is_empty() {
        return println_flush("no claims to renew").context("writing to stdout");
    }
    let recorded = agent
        .publish(
            envelope(Payload::Heartbeat { items }, String::new())?,
            wait.wait,
        )
        .await?;
    Agent::print_recorded(&recorded)
}

#[derive(Debug, Args)]
pub(crate) struct DoneArgs {
    /// The item.
    item: ItemKey,
    /// The commit holding the work (a sha, or any name git resolves).
    #[arg(long, value_name = "SHA")]
    commit: String,
    /// A note for the room; - reads it from stdin.
    #[arg(long, value_name = "TEXT")]
    body: Option<String>,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen done`: DONE with `branch` from `git rev-parse` and `changes` from
/// `git diff --name-status --no-renames <base_sha_at_claim>...<commit>`.
pub(crate) async fn done(args: DoneArgs) -> Result<()> {
    let body = body(args.body.as_deref())?;
    let agent = agent().await?;
    let item = args.item.clone();
    let base = with_claims(&agent, move |claims| claims.base(&item))
        .await?
        .with_context(|| {
            format!(
                "this worktree has no recorded claim of {}: claim it with agmen claim first",
                args.item
            )
        })?;
    let commit = git::commit(&agent.root, &args.commit).await?;
    let branch = git::branch(&agent.root).await?;
    let changes = git::changes(&agent.root, &base, &commit).await?;
    let count = changes.len();
    let payload = Payload::Done {
        item: args.item.clone(),
        branch: branch.clone(),
        commit: commit.clone(),
        base_sha: base,
        checks: Vec::new(),
        changes,
    };
    let recorded = agent
        .publish(envelope(payload, body)?, args.wait.wait)
        .await?;
    let item = args.item.clone();
    with_claims(&agent, move |claims| claims.remove(&item)).await?;
    Agent::print_recorded(&recorded)?;
    println_flush(&format!(
        "done {} at {commit} on {branch}, {count} changed {}",
        args.item,
        if count == 1 { "path" } else { "paths" }
    ))
    .context("writing to stdout")
}

/// Runs `work` on the identity's claims file, off the async runtime.
async fn with_claims<R: Send + 'static>(
    agent: &Agent,
    work: impl FnOnce(&Claims) -> Result<R> + Send + 'static,
) -> Result<R> {
    let dir = agent.state_dir.clone();
    blocking(move || work(&Claims::open(&dir)?)).await
}
