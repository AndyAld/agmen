//! Listener tools use agmend's checked local socket and never load joined state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agmen_proto::{IdentityId, MessageId, Payload};
use anyhow::{Context as _, Result, bail};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt as _, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agmend::{Agmend, AgmendError, LaunchCredential};
use crate::output::clean;

#[derive(Clone)]
pub(super) struct Server {
    credential: Arc<LaunchCredential>,
    socket: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl Server {
    pub(super) fn new(credential: LaunchCredential, socket: PathBuf) -> Self {
        Self {
            credential: Arc::new(credential),
            socket,
            tool_router: Self::tool_router(),
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        // shortcut: one agmend connection per tool call; keep one open if tool latency matters.
        // shortcut: Linux with a non-default XDG_STATE_HOME is unverified because the listener does not inherit it; pass the socket path in the MCP config if agmend runs off macOS.
        let mut daemon = Agmend::connect_checked(&self.socket).await?;
        daemon
            .call_as(
                &self.credential.identity,
                &self.credential.token,
                method,
                params,
            )
            .await
            .map_err(|error| match error {
                AgmendError::Refused { message, .. } => anyhow::Error::msg(message),
                other => other.into(),
            })
    }

    async fn git(&self, method: &str, params: Value) -> Result<String> {
        let answer = self.call(method, params).await?;
        let mut text = cleaned(field(&answer, "text")?);
        if answer
            .get("truncated")
            .and_then(Value::as_bool)
            .context("agmend's answer has no truncated flag")?
        {
            text.push_str("\n(truncated at 32 KiB)");
        }
        Ok(text)
    }

    async fn send(
        &self,
        payload: Payload,
        body: String,
        reply_to: Option<MessageId>,
        to: Vec<IdentityId>,
    ) -> Result<String> {
        let mut envelope = crate::joined::envelope(payload, body)?;
        envelope.reply_to = reply_to;
        envelope.to = to;
        let answer = self.call("post", json!({"envelope":envelope})).await?;
        if let Some(dropped) = answer.get("dropped").and_then(Value::as_str) {
            return Ok(format!("dropped: {dropped}"));
        }
        if answer.get("queued").and_then(Value::as_bool) == Some(true) {
            return Ok(format!(
                "queued id={} (not yet recorded)",
                cleaned(field(&answer, "id")?)
            ));
        }
        let seq = answer
            .get("seq")
            .and_then(Value::as_u64)
            .context("agmend's answer has no seq")?;
        Ok(format!(
            "recorded #{seq} ({})",
            cleaned(field(&answer, "stream")?)
        ))
    }
}

fn field<'a>(answer: &'a Value, key: &str) -> Result<&'a str> {
    answer
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("agmend's answer has no string {key}"))
}

fn text(result: Result<String>) -> CallToolResult {
    let text = match result {
        Ok(text) => text,
        Err(error) => format!("refused: {error}"),
    };
    CallToolResult::success(vec![ContentBlock::text(cleaned(&text))])
}

// Discard ANSI sequences before clean removes their introducer; keep git line breaks.
fn cleaned(text: &str) -> String {
    #[derive(Clone, Copy)]
    enum Escape {
        Text,
        Start,
        Csi,
        Osc,
        OscEnd,
    }
    let mut state = Escape::Text;
    let mut plain = String::with_capacity(text.len());
    // One transition per input character, bounded by the local API frame.
    for ch in text.chars() {
        state = match (state, ch) {
            (Escape::Text, '\u{1b}') => Escape::Start,
            (Escape::Text, ch) => {
                plain.push(ch);
                Escape::Text
            }
            (Escape::Start, '[') => Escape::Csi,
            (Escape::Start, ']') => Escape::Osc,
            (Escape::Start, _) => Escape::Text,
            (Escape::Csi, '\u{40}'..='\u{7e}') => Escape::Text,
            (Escape::Csi, _) => Escape::Csi,
            (Escape::Osc, '\u{7}') | (Escape::OscEnd, '\\') => Escape::Text,
            (Escape::Osc, '\u{1b}') => Escape::OscEnd,
            (Escape::Osc | Escape::OscEnd, _) => Escape::Osc,
        };
    }
    crate::output::clean_lines(&plain)
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Answer {
    reply_to: MessageId,
    body: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Dm {
    /// Recipient identity ids, from 1 to 8.
    #[schemars(length(min = 1, max = 8))]
    to: Vec<IdentityId>,
    body: String,
    reply_to: Option<MessageId>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Post {
    /// One of FYI, ASK, ANSWER, STATUS, ALERT.
    verb: String,
    body: String,
    reply_to: Option<MessageId>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Diff {
    base: String,
    head: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Log {
    rev_range: Option<String>,
    n: Option<u32>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Show {
    rev: String,
    stat: Option<bool>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Change {
    target: String,
    key: String,
    value: Value,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Apply {
    /// the id of a message in the current batch, from its `id` field
    reply_to: MessageId,
    batch_id: u64,
    #[schemars(length(min = 1, max = 16))]
    changes: Vec<Change>,
}

#[tool_router]
impl Server {
    #[tool(description = "Answer a message in the current batch.")]
    async fn answer(&self, Parameters(args): Parameters<Answer>) -> CallToolResult {
        text(
            self.send(Payload::Answer {}, args.body, Some(args.reply_to), vec![])
                .await,
        )
    }
    #[tool(description = "Send a direct message to 1 to 8 identity ids.")]
    async fn dm(&self, Parameters(args): Parameters<Dm>) -> CallToolResult {
        if !(1..=8).contains(&args.to.len()) {
            return text(Err(anyhow::anyhow!("dm needs 1 to 8 recipient ids")));
        }
        text(
            self.send(Payload::Dm {}, args.body, args.reply_to, args.to)
                .await,
        )
    }
    #[tool(description = "Read the diff between two revisions.")]
    async fn git_diff(&self, Parameters(args): Parameters<Diff>) -> CallToolResult {
        text(
            self.git("git_diff", json!({"base":args.base,"head":args.head}))
                .await,
        )
    }
    #[tool(description = "Read the git log.")]
    async fn git_log(&self, Parameters(args): Parameters<Log>) -> CallToolResult {
        let mut params = json!({});
        if let Some(range) = args.rev_range {
            params["rev_range"] = json!(range);
        }
        if let Some(n) = args.n {
            params["n"] = json!(n);
        }
        text(self.git("git_log", params).await)
    }
    #[tool(description = "Read a revision, optionally showing only its stat.")]
    async fn git_show(&self, Parameters(args): Parameters<Show>) -> CallToolResult {
        let mut params = json!({"rev":args.rev});
        if let Some(stat) = args.stat {
            params["stat"] = json!(stat);
        }
        text(self.git("git_show", params).await)
    }
    #[tool(description = "Read git status.")]
    async fn git_status(&self, Parameters(_args): Parameters<Empty>) -> CallToolResult {
        text(self.git("git_status", json!({})).await)
    }
    #[tool(description = "Post FYI, ASK, ANSWER, STATUS or ALERT as the listener.")]
    async fn post(&self, Parameters(args): Parameters<Post>) -> CallToolResult {
        let payload = match args.verb.as_str() {
            "FYI" => Payload::Fyi {},
            "ASK" => Payload::Ask { of: None },
            "ANSWER" => Payload::Answer {},
            "STATUS" => Payload::Status {
                summary: args.body.clone(),
                items: vec![],
                spend_usd: None,
                hours: None,
                tokens: None,
            },
            "ALERT" => Payload::Alert {
                need: args.body.clone(),
                urgency: agmen_proto::Urgency::Normal,
                needs: agmen_proto::Needs::Owner,
                actions: vec![],
            },
            _ => {
                return text(Err(anyhow::anyhow!(
                    "the listener cannot post {}",
                    args.verb
                )));
            }
        };
        text(self.send(payload, args.body, args.reply_to, vec![]).await)
    }
    #[tool(description = "Read project participants.")]
    async fn roster(&self, Parameters(_args): Parameters<Empty>) -> CallToolResult {
        text(
            async {
                let answer = self.call("roster", json!({})).await?;
                let entries = answer
                    .as_array()
                    .context("agmend's roster is not an array")?;
                let mut lines = Vec::new();
                for entry in entries.iter().take(4096) {
                    let role = match entry.get("role") {
                        None | Some(Value::Null) => "-",
                        Some(Value::String(role)) => role,
                        _ => bail!("agmend's roster role is not a string"),
                    };
                    lines.push(format!(
                        "{} {} {}",
                        clean(&cleaned(field(entry, "handle")?)),
                        clean(&cleaned(field(entry, "kind")?)),
                        clean(&cleaned(role))
                    ));
                }
                Ok(lines.join("\n"))
            }
            .await,
        )
    }
    #[tool(description = "Apply settings requested by your person in the current batch.")]
    async fn settings_apply(&self, Parameters(args): Parameters<Apply>) -> CallToolResult {
        text(async {
            if !(1..=16).contains(&args.changes.len()) { bail!("settings_apply needs 1 to 16 changes"); }
            let changes: Vec<_> = args.changes.into_iter().map(|change|json!({"target":change.target,"key":change.key,"value":change.value})).collect();
            let answer = self.call("settings_apply",json!({"reply_to":args.reply_to,"batch_id":args.batch_id,"changes":changes})).await?;
            let id = field(&answer,"id")?;
            match field(&answer,"state")? {
                "applied" => Ok(format!("applied; undo id {id}")),
                "pending" => Ok(format!("waiting for your person to apply; id {id}")),
                _ => bail!("agmend returned an unknown settings state"),
            }
        }.await)
    }
    #[tool(description = "Read listener settings.")]
    async fn settings_get(&self, Parameters(_args): Parameters<Empty>) -> CallToolResult {
        text(
            async {
                let answer = self.call("settings_get", json!({})).await?;
                Ok(serde_json::to_string_pretty(&clean_json(answer, 0)?)?)
            }
            .await,
        )
    }
    #[tool(
        description = "List the agent and team templates you can launch: name, kind, layer, description."
    )]
    async fn template_list(&self, Parameters(_args): Parameters<Empty>) -> CallToolResult {
        text(list_templates().await)
    }
}

async fn list_templates() -> Result<String> {
    let personal = crate::cmd::templates::personal_dir()?;
    let cwd = std::env::current_dir().context("reading the working directory")?;
    list_templates_in(personal, &cwd).await
}

/// The listener has no worktree, so `cwd` stands in for one. Outside a checkout there is no
/// project layer, and the built-in and personal templates still list.
async fn list_templates_in(personal: PathBuf, cwd: &Path) -> Result<String> {
    let project = crate::git::toplevel(cwd).await.ok();
    let catalog = crate::cmd::templates::catalog(project, personal).await?;
    Ok(crate::cmd::templates::list_for_model(&catalog))
}

// Clean strings before JSON escapes could hide terminal control characters.
fn clean_json(value: Value, depth: usize) -> Result<Value> {
    if depth > 64 {
        bail!("agmend's settings exceed 64 nesting levels");
    }
    Ok(match value {
        Value::String(value) => Value::String(cleaned(&value)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .take(agmen_local::MAX_FRAME as usize)
                .map(|value| clean_json(value, depth + 1))
                .collect::<Result<_>>()?,
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .take(agmen_local::MAX_FRAME as usize)
                .map(|(key, value)| Ok((clean(&key).into_owned(), clean_json(value, depth + 1)?)))
                .collect::<Result<_>>()?,
        ),
        value => value,
    })
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("agmen-listener", env!("CARGO_PKG_VERSION")),
        )
    }
}

pub(super) async fn run() -> Result<()> {
    let credential = LaunchCredential::from_env()?.ok_or_else(||crate::output::Fail::usage("agmen mcp --listener needs AGMEN_IDENTITY and AGMEN_LOCAL_TOKEN; agmend sets them in the listener's MCP config"))?;
    let server = Server::new(credential, agmen_client::agmend_socket()?);
    server
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "listener_tools_tests.rs"]
mod tests;
