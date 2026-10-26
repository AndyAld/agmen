//! `agmen post`, `ask`, `answer`, `dm`, `found` and `assign`: messages
//! this worktree's identity sends. Each prints `recorded #<seq> (<stream>)`
//! once the hub has it (exit 0), `rejected: <reason>` on a final NACK
//! (exit 4), or `queued id=<id> (not yet recorded)` when the hub did not
//! answer within `--wait` (exit 5).

use std::io::Read as _;

use agmen_proto::{ItemKey, LaneName, MAX_BODY_BYTES, MessageId, Payload, Severity, Verb};
use anyhow::{Context as _, Result};
use clap::Args;
use serde_json::Value;

use super::Wait;
use crate::joined::{Agent, agent, envelope};
use crate::output::{Fail, println_flush};

/// Who and what a message is about.
#[derive(Debug, Args)]
pub(crate) struct About {
    /// An item the message is about, such as B-412.
    #[arg(long = "item", value_name = "KEY")]
    items: Vec<ItemKey>,
    /// The lane the message is about, such as L-UI.
    #[arg(long, value_name = "LANE")]
    lane: Option<LaneName>,
    /// A mention: a handle, an id, all, agents, humans, a lane, or role:NAME.
    #[arg(long = "mention", value_name = "WHO")]
    mentions: Vec<String>,
}

#[derive(Debug, Args)]
pub(crate) struct PostArgs {
    /// The verb, such as FYI or STATUS.
    verb: String,
    /// The body; - reads it from stdin.
    text: Option<String>,
    /// A recipient, by handle or id (DM and ASK).
    #[arg(long = "to", value_name = "WHO")]
    to: Vec<String>,
    /// The message this one answers or threads under.
    #[arg(long, value_name = "ID")]
    reply_to: Option<MessageId>,
    /// The verb's data as a JSON object; an empty object when left out.
    #[arg(long, value_name = "JSON")]
    data: Option<String>,
    #[command(flatten)]
    about: About,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen post`.
pub(crate) async fn post(args: PostArgs) -> Result<()> {
    let verb: Verb = args
        .verb
        .to_ascii_uppercase()
        .parse()
        .map_err(|_| Fail::usage(format!("{} is not a protocol v1 verb", args.verb)))?;
    let data: Value = match &args.data {
        Some(json) => serde_json::from_str(json)
            .map_err(|err| Fail::usage(format!("--data is not JSON: {err}")))?,
        None => Value::Object(serde_json::Map::new()),
    };
    let payload: Payload =
        serde_json::from_value(serde_json::json!({ "verb": verb.as_str(), "data": data }))
            .map_err(|err| Fail::usage(format!("the data for {verb} is not valid: {err}")))?;
    let body = body(args.text.as_deref())?;
    let agent = agent().await?;
    let mut env = envelope(payload, body)?;
    env.to = agent.resolve(&args.to).await?;
    env.reply_to = args.reply_to;
    apply_about(&agent, &mut env, args.about).await?;
    let recorded = agent.publish(env, args.wait.wait).await?;
    Agent::print_recorded(&recorded)
}

#[derive(Debug, Args)]
pub(crate) struct AskArgs {
    /// The question; - reads it from stdin.
    text: String,
    /// Who should get it in their inbox, by handle or id; the feed when left out.
    #[arg(long = "to", value_name = "WHO")]
    to: Vec<String>,
    /// The one identity that should answer.
    #[arg(long, value_name = "WHO")]
    of: Option<String>,
    #[command(flatten)]
    about: About,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen ask`.
pub(crate) async fn ask(args: AskArgs) -> Result<()> {
    let body = body(Some(&args.text))?;
    let agent = agent().await?;
    let of = match args.of {
        Some(of) => agent.resolve(&[of]).await?.pop(),
        None => None,
    };
    let mut env = envelope(Payload::Ask { of }, body)?;
    env.to = agent.resolve(&args.to).await?;
    apply_about(&agent, &mut env, args.about).await?;
    let recorded = agent.publish(env, args.wait.wait).await?;
    Agent::print_recorded(&recorded)
}

#[derive(Debug, Args)]
pub(crate) struct AnswerArgs {
    /// The id of the ASK this answers.
    reply_to: MessageId,
    /// The answer; - reads it from stdin.
    text: String,
    #[command(flatten)]
    about: About,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen answer`.
pub(crate) async fn answer(args: AnswerArgs) -> Result<()> {
    let body = body(Some(&args.text))?;
    let agent = agent().await?;
    let mut env = envelope(Payload::Answer {}, body)?;
    env.reply_to = Some(args.reply_to);
    apply_about(&agent, &mut env, args.about).await?;
    let recorded = agent.publish(env, args.wait.wait).await?;
    Agent::print_recorded(&recorded)
}

#[derive(Debug, Args)]
pub(crate) struct DmArgs {
    /// The recipients by handle or id (1 to 8), then the text (- reads it
    /// from stdin).
    #[arg(required = true, num_args = 2.., value_name = "WORD")]
    words: Vec<String>,
    /// The message this one answers or threads under.
    #[arg(long, value_name = "ID")]
    reply_to: Option<MessageId>,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen dm <to...> <text>`.
pub(crate) async fn dm(args: DmArgs) -> Result<()> {
    let mut words = args.words;
    let text = words
        .pop()
        .ok_or_else(|| Fail::usage("agmen dm <to...> <text>"))?;
    let body = body(Some(&text))?;
    let agent = agent().await?;
    let mut env = envelope(Payload::Dm {}, body)?;
    env.to = agent.resolve(&words).await?;
    env.reply_to = args.reply_to;
    let recorded = agent.publish(env, args.wait.wait).await?;
    Agent::print_recorded(&recorded)
}

#[derive(Debug, Args)]
pub(crate) struct FoundArgs {
    /// A short title.
    title: String,
    /// The details; - reads them from stdin.
    #[arg(long, value_name = "TEXT")]
    body: Option<String>,
    /// The lane the problem belongs to.
    #[arg(long, value_name = "LANE")]
    lane: Option<LaneName>,
    /// How bad it is.
    #[arg(long, value_parser = parse_severity)]
    severity: Option<Severity>,
    #[command(flatten)]
    wait: Wait,
}

fn parse_severity(text: &str) -> Result<Severity, String> {
    serde_json::from_value(Value::String(text.to_owned()))
        .map_err(|_| "the severity is low, med, high or security".to_owned())
}

/// `agmen found`: prints the item key the hub minted.
pub(crate) async fn found(args: FoundArgs) -> Result<()> {
    let body = body(args.body.as_deref())?;
    let agent = agent().await?;
    let payload = Payload::Found {
        title: args.title,
        lane: args.lane,
        severity: args.severity,
    };
    let recorded = agent
        .publish(envelope(payload, body)?, args.wait.wait)
        .await?;
    Agent::print_recorded(&recorded)?;
    let line = match agent.hub_answer(&recorded).await? {
        Some(answer) => match answer.client.payload {
            Payload::Item { key } => format!("item {key}"),
            _ => "the hub answered without an item key".to_owned(),
        },
        None => "no item key yet: agmen items lists it".to_owned(),
    };
    println_flush(&line).context("writing to stdout")
}

#[derive(Debug, Args)]
pub(crate) struct AssignArgs {
    /// The item.
    item: ItemKey,
    /// The assignee, by handle or id.
    identity: String,
    /// A note for the assignee.
    #[arg(long, value_name = "TEXT")]
    body: Option<String>,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen assign`.
pub(crate) async fn assign(args: AssignArgs) -> Result<()> {
    let body = body(args.body.as_deref())?;
    let agent = agent().await?;
    let to = agent
        .resolve(&[args.identity])
        .await?
        .pop()
        .context("resolving the assignee gave no id")?;
    let payload = Payload::Assign {
        item: args.item,
        to,
    };
    let recorded = agent
        .publish(envelope(payload, body)?, args.wait.wait)
        .await?;
    Agent::print_recorded(&recorded)
}

/// Sets the items, lane and mentions of `about` on `env`.
async fn apply_about(
    agent: &Agent,
    env: &mut agmen_proto::ClientEnvelope,
    about: About,
) -> Result<()> {
    env.items = about.items;
    env.lane = about.lane;
    env.mentions = agent.mentions(&about.mentions).await?;
    Ok(())
}

/// The body `text` gives: itself, stdin for `-`, or empty. Reads at most
/// one byte past the body limit, so a huge stdin is refused without
/// reading all of it.
pub(crate) fn body(text: Option<&str>) -> Result<String> {
    match text {
        Some("-") => {
            let mut raw = Vec::new();
            let limit = u64::try_from(MAX_BODY_BYTES + 1).unwrap_or(u64::MAX);
            std::io::stdin()
                .lock()
                .take(limit)
                .read_to_end(&mut raw)
                .context("reading the body from stdin")?;
            String::from_utf8(raw).map_err(|_| Fail::usage("the body on stdin is not UTF-8").into())
        }
        Some(text) => Ok(text.to_owned()),
        None => Ok(String::new()),
    }
}
