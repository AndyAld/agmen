//! The twelve tools `agmen mcp` serves. Each is a thin layer over what the
//! matching `agmen` command does (`crate::cmd`), sending through the
//! server's held session instead of a one-shot one, and answering with
//! text instead of printing it. Input schemas come from schemars, with the
//! agmen-proto types (item keys, lane names, message ids, severities)
//! giving their own patterns.

use std::sync::Arc;

use agmen_proto::{ItemKey, LaneName, MessageId, Payload, Severity};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{Live, Server};
use crate::cmd::post::{About, Outgoing, item_key, payload, send};
use crate::cmd::{claim, inbox, lanes};
use crate::joined::{Recorded, client_error};
use crate::output::{clean, message_line_with_id, recorded};

/// What the server tells the client about itself.
const INSTRUCTIONS: &str = "agmen connects this worktree's agent to its project's other agents and \
people. Read what is addressed to you with inbox. Claim an item before editing files in its lane, \
report finished work with done, give a claim up with release, and report a new problem with \
found. Talk with post, ask, answer and dm; wherever a tool names an identity, a handle such as \
andy/sol works. A tool that sends answers recorded #<seq> (<stream>) once the hub has the \
message, queued id=<id> (not yet recorded) while it is still on its way (agmen keeps sending it), \
or rejected: <reason>. Never send a rejected message again unchanged.";

/// `post`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct PostParams {
    /// The verb, such as FYI, STATUS or BLOCKED.
    verb: String,
    /// The message text, at most 32768 bytes.
    #[serde(default)]
    body: String,
    /// The verb's data as a JSON object, for verbs that carry data.
    data: Option<Map<String, Value>>,
    /// Recipients by handle or id, for DM and ASK only.
    #[serde(default)]
    to: Vec<String>,
    /// The id of the message this one answers or threads under.
    reply_to: Option<MessageId>,
    #[serde(flatten)]
    about: About,
}

/// `ask`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AskParams {
    /// The question, at most 32768 bytes.
    body: String,
    /// Who gets it in their inbox, by handle or id (1 to 8); else it goes to the feed.
    #[serde(default)]
    to: Vec<String>,
    /// The one identity that should answer, by handle or id.
    of: Option<String>,
    #[serde(flatten)]
    about: About,
}

/// `answer`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AnswerParams {
    /// The id of the ASK this answers, as inbox shows it.
    reply_to: MessageId,
    /// The answer, at most 32768 bytes.
    body: String,
    #[serde(flatten)]
    about: About,
}

/// `dm`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct DmParams {
    /// The recipients by handle or id (1 to 8).
    to: Vec<String>,
    /// The message, at most 32768 bytes.
    body: String,
    /// The id of the message this one answers or threads under.
    reply_to: Option<MessageId>,
}

/// `inbox`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct InboxParams {
    /// List from after this seq in each stream, not from where the last call stopped.
    after: Option<u64>,
}

/// `state`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct StateParams {
    /// head (stream heads and versions, the default) or roster (who is in the project).
    #[serde(default)]
    key: StateKey,
}

/// The retained `state/<key>` topics the `state` tool reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum StateKey {
    #[default]
    Head,
    Roster,
}

impl StateKey {
    /// The topic level.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Roster => "roster",
        }
    }
}

/// `claim`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ClaimParams {
    /// The item, such as B-412.
    item: ItemKey,
}

/// `release`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ReleaseParams {
    /// The item.
    item: ItemKey,
    /// Why, for the room.
    reason: Option<String>,
}

/// `done`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct DoneParams {
    /// The item.
    item: ItemKey,
    /// The commit holding the work: a sha, or any name git resolves, such as HEAD.
    commit: String,
    /// A note for the room.
    #[serde(default)]
    body: String,
}

/// `found`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct FoundParams {
    /// A short title.
    title: String,
    /// The details.
    #[serde(default)]
    body: String,
    /// The lane the problem belongs to, such as L-UI.
    lane: Option<LaneName>,
    /// How bad it is.
    severity: Option<Severity>,
}

/// `check_path`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct CheckPathParams {
    /// The paths (at most 500), absolute or relative to where agmen mcp runs.
    paths: Vec<String>,
}

/// `recorded #<seq> (<stream>)`.
fn recorded_line(at: &Recorded) -> String {
    recorded(&at.stream, at.seq)
}

#[tool_router(vis = "pub(super)")]
impl Server {
    #[tool(
        description = "Post a message to the project with any protocol verb (FYI, STATUS, \
        BLOCKED and the rest). Answers recorded, queued or rejected."
    )]
    async fn post(&self, Parameters(params): Parameters<PostParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let data = Value::Object(params.data.unwrap_or_default());
            let out = Outgoing {
                to: params.to,
                reply_to: params.reply_to,
                about: params.about,
                ..Outgoing::new(payload(&params.verb, data)?, params.body)
            };
            Ok(recorded_line(&send(&live.agent, live.via(), out).await?))
        })
        .await
    }

    #[tool(
        description = "Ask a question, of the room or of some identities. Answers arrive in \
        the inbox."
    )]
    async fn ask(&self, Parameters(params): Parameters<AskParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let of = match params.of {
                Some(of) => live.agent.resolve(live.via(), &[of]).await?.pop(),
                None => None,
            };
            let out = Outgoing {
                to: params.to,
                about: params.about,
                ..Outgoing::new(Payload::Ask { of }, params.body)
            };
            Ok(recorded_line(&send(&live.agent, live.via(), out).await?))
        })
        .await
    }

    #[tool(description = "Answer an ASK by its id, as inbox shows it.")]
    async fn answer(&self, Parameters(params): Parameters<AnswerParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let out = Outgoing {
                reply_to: Some(params.reply_to),
                about: params.about,
                ..Outgoing::new(Payload::Answer {}, params.body)
            };
            Ok(recorded_line(&send(&live.agent, live.via(), out).await?))
        })
        .await
    }

    #[tool(description = "Send a direct message to 1 to 8 identities, by handle or id.")]
    async fn dm(&self, Parameters(params): Parameters<DmParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let out = Outgoing {
                to: params.to,
                reply_to: params.reply_to,
                ..Outgoing::new(Payload::Dm {}, params.body)
            };
            Ok(recorded_line(&send(&live.agent, live.via(), out).await?))
        })
        .await
    }

    #[tool(
        description = "List the new messages addressed to this identity, oldest first, each \
        with its id, and mark them read."
    )]
    async fn inbox(&self, Parameters(params): Parameters<InboxParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let unread = inbox::unread(&live.agent, params.after).await?;
            let mut lines: Vec<String> = unread.messages.iter().map(message_line_with_id).collect();
            if lines.is_empty() {
                lines.push("no new messages".to_owned());
            }
            if unread.mark_read().await? {
                lines.push("more messages are waiting: call inbox again".to_owned());
            }
            Ok(lines.join("\n"))
        })
        .await
    }

    #[tool(
        description = "Read the project's retained state as JSON: head (stream heads and \
        versions) or roster (who is in the project and online)."
    )]
    async fn state(&self, Parameters(params): Parameters<StateParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let key = params.key.as_str();
            let value = live
                .agent
                .read_state(live.via(), key)
                .await
                .map_err(|err| client_error(err, "reading the project's state failed"))?;
            let shown = value.map_or_else(|| "nothing retained".to_owned(), |v| v.to_string());
            Ok(format!("state/{key}: {}", clean(&shown)))
        })
        .await
    }

    #[tool(
        description = "Claim an item before editing files in its lane. Answers granted (with \
        the lease) or denied (with the reason)."
    )]
    async fn claim(&self, Parameters(params): Parameters<ClaimParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let (at, answer) = claim::claim_item(&live.agent, live.via(), params.item).await?;
            Ok(format!("{}\n{}", recorded_line(&at), answer.line()))
        })
        .await
    }

    #[tool(description = "Give up a claim.")]
    async fn release(&self, Parameters(params): Parameters<ReleaseParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let at =
                claim::release_item(&live.agent, live.via(), params.item, params.reason).await?;
            Ok(recorded_line(&at))
        })
        .await
    }

    #[tool(
        description = "Report finished work on a claimed item at a commit; agmen fills in \
        the branch and the changed paths since the claim."
    )]
    async fn done(&self, Parameters(params): Parameters<DoneParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let (at, line) = claim::done_item(
                &live.agent,
                live.via(),
                params.item,
                &params.commit,
                params.body,
            )
            .await?;
            Ok(format!("{}\n{line}", recorded_line(&at)))
        })
        .await
    }

    #[tool(description = "Report a new problem; the hub answers with its item key, such as B-7.")]
    async fn found(&self, Parameters(params): Parameters<FoundParams>) -> CallToolResult {
        self.respond(move |live: Arc<Live>| async move {
            let payload = Payload::Found {
                title: params.title,
                lane: params.lane,
                severity: params.severity,
            };
            let at = send(&live.agent, live.via(), Outgoing::new(payload, params.body)).await?;
            Ok(format!(
                "{}\n{}",
                recorded_line(&at),
                item_key(&live.agent, &at).await?
            ))
        })
        .await
    }

    #[tool(description = "Show the project's lanes: each lane's name, owner and path globs.")]
    async fn lanes(&self) -> CallToolResult {
        self.respond(|live: Arc<Live>| async move { lanes::lanes_text(&live.agent).await })
            .await
    }

    #[tool(description = "Say whether this identity may edit each path, and why.")]
    async fn check_path(&self, Parameters(params): Parameters<CheckPathParams>) -> CallToolResult {
        let cwd = self.cwd().to_path_buf();
        self.respond(move |live: Arc<Live>| async move {
            let answers = lanes::check(&live.agent, &cwd, params.paths).await?;
            Ok(lanes::check_lines(&answers))
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("agmen", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

#[cfg(test)]
mod tests {
    use rmcp::handler::server::tool::parse_json_object;
    use serde_json::json;

    use super::*;

    /// Every string an `enum` or `const` in `schema` allows.
    fn allowed(schema: &Value, out: &mut Vec<String>) {
        match schema {
            Value::Object(map) => {
                if let Some(Value::String(value)) = map.get("const") {
                    out.push(value.clone());
                }
                if let Some(Value::Array(values)) = map.get("enum") {
                    out.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
                }
                map.values().for_each(|value| allowed(value, out));
            }
            Value::Array(values) => values.iter().for_each(|value| allowed(value, out)),
            _ => {}
        }
    }

    #[test]
    fn state_schema_lists_the_two_keys() {
        let tool = Server::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "state")
            .unwrap();
        let mut keys = Vec::new();
        allowed(&Value::Object((*tool.input_schema).clone()), &mut keys);
        keys.sort();
        assert_eq!(keys, ["head", "roster"], "{:?}", tool.input_schema);
    }

    #[test]
    fn state_refuses_other_keys() {
        let parse = |args: Value| {
            let Value::Object(args) = args else {
                panic!("tool arguments are a JSON object")
            };
            parse_json_object::<StateParams>(args)
        };
        assert!(parse(json!({})).is_ok());
        assert!(parse(json!({ "key": "roster" })).is_ok());
        for bad in ["a/b", "nothing", "Head", ""] {
            assert!(parse(json!({ "key": bad })).is_err(), "{bad:?}");
        }
    }
}
