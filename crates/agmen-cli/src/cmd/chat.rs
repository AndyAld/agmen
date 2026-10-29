//! Human chat through the hub's authenticated post and feed routes.

use std::io::{self, BufRead, Read as _};
use std::time::Duration;

use agmen_client::{Page, find_identity, read_hub_body, resolve_handle};
use agmen_proto::{DeliveryReply, Mention, MessageId, Payload, ProjectId, StampedEnvelope};
use anyhow::{Context as _, Result, ensure};
use clap::Args;
use reqwest::{Method, RequestBuilder};
use serde::Deserialize;
use serde_json::Value;

use crate::client::{self, Api, Human, Refusal, blocking, human, refusal};
use crate::joined::{envelope, mention_tokens};
use crate::output::{Fail, clean, clean_lines, explain, println_flush};

const MAX_LINE: usize = 64 * 1024;
const POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Args)]
pub(crate) struct ChatArgs {
    /// Post once; without text, follow the feed and send stdin lines.
    text: Option<String>,
    /// Mention everyone in the project.
    #[arg(long)]
    all: bool,
    /// The message this one threads under.
    #[arg(long, value_name = "ID")]
    reply_to: Option<MessageId>,
    /// The project; otherwise use this worktree or your only project.
    #[arg(long, value_name = "ID")]
    project: Option<ProjectId>,
}

pub(crate) async fn chat(args: ChatArgs) -> Result<()> {
    if args
        .text
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        return Err(
            Fail::usage("nothing to say: give some text, or no text to follow the chat").into(),
        );
    }
    let human = human().await?;
    let project = project(&human, args.project.clone()).await?;
    if let Some(text) = &args.text {
        let seq = post(&human.api, &project, text, &args).await?;
        return println_flush(&format!("recorded #{seq}")).context("writing to stdout");
    }
    // Dropping the follow future cancels its current bounded HTTP request.
    let followed = tokio::select! {
        result = follow(&human.api, &project, &args) => result,
        signal = tokio::signal::ctrl_c() => signal.context("waiting for Ctrl-C"),
    };
    match followed {
        // A reader that went away ends the session without an error.
        Err(err) if err.is::<Closed>() => Ok(()),
        other => other,
    }
}

/// Stdout has no reader left (a closed pipe).
#[derive(Debug)]
struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stdout is closed")
    }
}

impl std::error::Error for Closed {}

/// Print one line, naming a closed pipe as [`Closed`].
fn print(line: &str) -> Result<()> {
    println_flush(line).map_err(|err| match err.kind() {
        io::ErrorKind::BrokenPipe => Closed.into(),
        _ => anyhow::Error::new(err).context("writing to stdout"),
    })
}

/// Whether follow mode cannot go on after `err`: the hub no longer takes the
/// token, or nobody reads the output. Anything else (a hub that is down or
/// busy, a rejected line) is reported and the session carries on.
fn fatal(err: &anyhow::Error) -> bool {
    err.is::<Closed>() || refusal(err).is_some_and(|r| matches!(r.status, 401 | 403))
}

async fn project(human: &Human, explicit: Option<ProjectId>) -> Result<ProjectId> {
    if let Some(project) = explicit {
        return Ok(project);
    }
    let cwd = std::env::current_dir().context("reading the working directory")?;
    let joined =
        blocking(move || find_identity(&cwd).context("loading .agmen/identity.json failed"))
            .await?;
    if let Some(joined) = joined {
        return Ok(joined.identity.project);
    }
    let me = human.api.me().await?;
    match me.projects.as_slice() {
        [only] => only
            .project
            .parse()
            .context("the hub's project id is invalid"),
        [] => Err(Fail::usage("no projects yet: run agmen join <invite link>").into()),
        projects => Err(Fail::usage(format!(
            "choose a project with --project: {}",
            projects
                .iter()
                .map(|p| format!("{} ({})", p.project, p.name))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into()),
    }
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    value: Value,
}

async fn post(api: &Api, project: &ProjectId, text: &str, args: &ChatArgs) -> Result<u64> {
    let mut env = envelope(Payload::Fyi {}, text.to_owned())?;
    env.reply_to = args.reply_to.clone();
    let (mut mentions, names) =
        mention_tokens(text.split_whitespace().filter(|word| word.starts_with('@')));
    if args.all && !mentions.contains(&Mention::All) {
        mentions.insert(0, Mention::All);
    }
    if !names.is_empty() {
        let snapshot: Snapshot = client::send(query(
            api.request(Method::GET, &["projects", project.as_str(), "state"]),
            &[("key", "roster".to_owned())],
        )?)
        .await?;
        for name in names {
            let id = resolve_handle(&snapshot.value, &name)
                .context("the project's roster is not valid")?
                .or_else(|| name.parse().ok())
                .ok_or_else(|| {
                    Fail::usage(format!(
                        "no identity in this project has the handle @{name}"
                    ))
                })?;
            mentions.push(Mention::Identity(id));
        }
    }
    env.mentions = mentions;
    let response = api
        .request(Method::POST, &["projects", project.as_str(), "messages"])
        .json(&env)
        .send()
        .await
        .context("posting to the project chat failed")?;
    let status = response.status();
    let bytes = read_hub_body(response)
        .await
        .context("reading the hub's chat answer failed")?;
    if let Ok(reply) = serde_json::from_slice::<DeliveryReply>(&bytes) {
        return match reply {
            DeliveryReply::Ack { id, seq, .. } => {
                ensure!(id == env.id, "the hub acknowledged another message");
                Ok(seq.0)
            }
            DeliveryReply::Nack { reason, .. } => Err(Fail::rejected(reason).into()),
        };
    }
    if status.as_u16() == 413 {
        return Err(Fail::rejected("envelope_too_large").into());
    }
    if status.is_success() {
        anyhow::bail!("the hub's chat answer is not an ACK or NACK");
    }
    let body: Value =
        serde_json::from_slice(&bytes).context("the hub's chat refusal is not JSON")?;
    Err(Refusal {
        status: status.as_u16(),
        code: body["error"].as_str().unwrap_or_default().to_owned(),
        description: body["error_description"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    }
    .into())
}

async fn page(api: &Api, project: &ProjectId, after: u64, limit: u32) -> Result<Page> {
    client::send(query(
        api.request(
            Method::GET,
            &["projects", project.as_str(), "streams", "feed"],
        ),
        &[("after", after.to_string()), ("limit", limit.to_string())],
    )?)
    .await
}

/// Add query pairs without enabling reqwest's optional query feature.
fn query(request: RequestBuilder, pairs: &[(&str, String)]) -> Result<RequestBuilder> {
    let (http, request) = request.build_split();
    let mut request = request.context("building the chat request")?;
    request.url_mut().query_pairs_mut().extend_pairs(pairs);
    Ok(RequestBuilder::from_parts(http, request))
}

fn line(message: &StampedEnvelope) -> String {
    format!(
        "#{} {}: {}",
        message.seq,
        clean(&message.from_handle),
        clean_lines(message.client.body.as_str())
    )
}

/// Print at most five replay pages per poll, keeping the cursor at the last
/// printed message so a busy feed resumes on the next poll.
async fn show(api: &Api, project: &ProjectId, after: &mut u64, end: u64) -> Result<()> {
    for _ in 0..5 {
        if *after >= end {
            break;
        }
        let page = page(api, project, *after, 500).await?;
        for message in page.messages.iter().filter(|m| m.seq.0 <= end) {
            print(&line(message))?;
            *after = (*after).max(message.seq.0);
        }
        match page.next_after {
            Some(next) if next > *after && next < end => *after = next,
            Some(_) if *after < end && !page.messages.is_empty() => continue,
            _ => break,
        }
    }
    Ok(())
}

/// Read one UTF-8 stdin line with at most 64 KiB of content and a CRLF.
fn read_line(input: &mut impl BufRead) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let read = input
        .take((MAX_LINE + 2) as u64)
        .read_until(b'\n', &mut bytes)
        .context("reading chat from stdin")?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > MAX_LINE {
        return Err(Fail::usage("a chat stdin line exceeds 64 KiB").into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| Fail::usage("the chat line on stdin is not UTF-8").into())
}

async fn follow(api: &Api, project: &ProjectId, args: &ChatArgs) -> Result<()> {
    let head = page(api, project, 0, 1).await?.head;
    let mut after = head.saturating_sub(50);
    show(api, project, &mut after, head).await?;
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    // A dedicated thread keeps a pending terminal read out of Tokio's
    // blocking pool, whose shutdown would otherwise wait for stdin on Ctrl-C.
    std::thread::Builder::new()
        .name("chat-stdin".to_owned())
        .spawn(move || {
            let mut input = std::io::stdin().lock();
            loop {
                let line = read_line(&mut input);
                let stop = !matches!(line, Ok(Some(_)));
                if tx.blocking_send(line).is_err() || stop {
                    break;
                }
            }
        })
        .context("starting the chat stdin reader")?;
    // shortcut: polling has 2 s latency and one request every 2 s when
    // caught up. Switch to GET /v1/projects/{p}/events for streaming updates.
    let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + POLL, POLL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The session ends on stdin EOF or Ctrl-C. Each read is capped at 64 KiB,
    // each HTTP request at 10 s, and each poll at five pages.
    let mut down = false;
    loop {
        tokio::select! {
            input = rx.recv() => {
                let Some(text) = input.transpose()?.flatten() else { break; };
                if text.trim().is_empty() {
                    continue;
                }
                // A rejected or unsent line is reported; the session goes on.
                if let Err(err) = post(api, project, &text, args).await {
                    report(err, false)?;
                }
                if let Err(err) = show(api, project, &mut after, u64::MAX).await {
                    report(err, down)?;
                    down = true;
                }
            }
            _ = poll.tick() => match show(api, project, &mut after, u64::MAX).await {
                Ok(()) => down = false,
                // One notice per outage, not one every 2 s.
                Err(err) => {
                    report(err, down)?;
                    down = true;
                }
            },
        }
    }
    Ok(())
}

/// Print `err` to stderr unless `quiet`, or end the session if it is [`fatal`].
fn report(err: anyhow::Error, quiet: bool) -> Result<()> {
    if fatal(&err) {
        return Err(err);
    }
    if !quiet {
        eprintln!("{}", explain(&err).0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Code;
    use axum::Json;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post as route_post};
    use serde_json::json;

    #[test]
    fn chat_stdin_lines_are_bounded_and_utf8() {
        let mut input = std::io::Cursor::new(b"first\r\n\nlast");
        for expected in [Some("first"), Some(""), Some("last"), None] {
            assert_eq!(read_line(&mut input).unwrap().as_deref(), expected);
        }
        for suffix in ["", "\n", "\r\n"] {
            let text = format!("{}{suffix}", "x".repeat(MAX_LINE));
            assert_eq!(
                read_line(&mut std::io::Cursor::new(text))
                    .unwrap()
                    .unwrap()
                    .len(),
                MAX_LINE
            );
        }
        let mut huge = std::io::Cursor::new(vec![b'x'; MAX_LINE * 2]);
        assert!(
            read_line(&mut huge)
                .unwrap_err()
                .to_string()
                .contains("64 KiB")
        );
        assert_eq!(huge.position(), (MAX_LINE + 2) as u64);
        assert!(
            read_line(&mut std::io::Cursor::new([0xff, b'\n']))
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
    }

    #[tokio::test]
    async fn chat_human_post_resolves_mentions_and_reads_nacks() {
        for (status, reason) in [
            (200, None),
            (422, Some("forbidden_mention")),
            (409, Some("conflict")),
            (429, Some("rate_limited")),
            (503, Some("hub_busy")),
            (413, Some("envelope_too_large")),
            (401, Some("unauthorized")),
        ] {
            let app = axum::Router::new()
                .route("/v1/projects/project1/state", get(|headers: HeaderMap, axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                    assert_eq!(headers["authorization"], "Bearer human-test");
                    assert_eq!(query["key"], "roster");
                    Json(json!({"value":[{"identity_id":"adamcoord", "handle":"adam/coord", "kind":"coordinator", "status":"offline"}]}))
                }))
                .route("/v1/projects/project1/messages", route_post(move |headers: HeaderMap, Json(env): Json<Value>| async move {
                    assert_eq!(headers["authorization"], "Bearer human-test");
                    assert_eq!(env["verb"], "FYI");
                    assert_eq!(env["mentions"], json!(["all", "adamcoord"]));
                    assert_eq!(env["reply_to"], "01JBX6Q4T7W3C2M8Y9K5N0P1RS");
                    let body = match reason {
                        None => json!({"verb":"ACK", "id":env["id"], "seq":1, "stream":"feed"}),
                        Some(reason) if status == 413 || status == 401 => json!({"error":reason}),
                        Some(reason) => json!({"verb":"NACK", "id":env["id"], "reason":reason, "retryable":status >= 429}),
                    };
                    (StatusCode::from_u16(status).unwrap(), Json(body))
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap();
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let api = Api::new(base, "human-test").unwrap();
            let args = ChatArgs {
                text: None,
                all: true,
                reply_to: Some("01JBX6Q4T7W3C2M8Y9K5N0P1RS".parse().unwrap()),
                project: None,
            };
            let result = post(
                &api,
                &"project1".parse().unwrap(),
                "stand by @adam/coord",
                &args,
            )
            .await;
            task.abort();
            if status == 401 {
                assert_eq!(crate::output::explain(&result.unwrap_err()).1, 3);
            } else if let Some(reason) = reason {
                let err = result.unwrap_err();
                let fail = err.downcast_ref::<Fail>().unwrap();
                assert_eq!(fail.code, Code::Rejected);
                assert_eq!(fail.message, format!("rejected: {reason}"));
            } else {
                assert_eq!(result.unwrap(), 1);
            }
        }
    }
}
