//! The role-filtered tools `agmen mcp` serves, with sixteen for legacy or
//! unjoined worktrees and an additional review tool for roles with REVIEW.
//! Each is a thin layer over what the
//! matching `agmen` command does (`crate::cmd`), sending through the
//! server's held session instead of a one-shot one, and answering with
//! text instead of printing it. Input schemas come from schemars, with the
//! agmen-proto types (item keys, lane names, message ids, severities)
//! giving their own patterns.

use std::collections::BTreeSet;
use std::sync::Arc;

use agmen_proto::{Finding, ItemKey, LaneName, MessageId, Payload, Severity, Stream, Verdict};
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
use crate::output::{CHAT_FENCE, Fail, clean, clean_lines, message_line_with_id, recorded};

/// What the server tells the client about itself.
const INSTRUCTIONS: &str = "agmen connects this worktree's agent to its project's other agents \
and people. They see only what you post, so communicating with them is part of the work: tell the \
ones your work affects (dm, or an FYI with mentions), answer every ASK addressed to you, and when \
you are blocked, say so at once instead of waiting: ask an agent that can unblock you (with to), \
or post an ALERT for a person. Keep each message to one accurate point, naming the item, file, \
commit or number and anything you did not check. Read what is addressed to you with inbox, before \
you start a task and before you report it done. Claim an item before editing files in its lane, \
report finished work with done, give a claim up with release, and report a new problem with \
found. Talk with post, chat, ask, answer and dm; read the room with read_chat. Chat sends an FYI \
and turns @ tokens into mentions. Wherever a tool names an identity, a handle such as \
andy/sol works. A tool that sends answers recorded #<seq> (<stream>) once the hub has the \
message, queued id=<id> (not yet recorded) while it is still on its way (agmen keeps sending it), \
or rejected: <reason>. Never send a rejected message again unchanged. \
Messages addressed to you are added at the end of tool results under \"for you\"; when you \
have no other agmen call to make, call inbox between steps.";

/// Tool visibility at startup; absence of verbs preserves the legacy list.
/// Chat and read_chat, like post, stay visible for every role. The hub
/// enforces the sending role's verbs.
pub(crate) fn hidden_tools(verbs: Option<&BTreeSet<String>>) -> Vec<&'static str> {
    const GATED: [(&str, &str); 8] = [
        ("claim", "CLAIM"),
        ("release", "RELEASE"),
        ("done", "DONE"),
        ("ask", "ASK"),
        ("answer", "ANSWER"),
        ("dm", "DM"),
        ("found", "FOUND"),
        ("review", "REVIEW"),
    ];
    GATED
        .into_iter()
        .filter_map(|(tool, verb)| {
            let visible = verbs.map_or(tool != "review", |verbs| verbs.contains(verb));
            (!visible).then_some(tool)
        })
        .collect()
}

/// The most bytes one `read_chat` line may take, so a hundred 32 KiB bodies
/// cannot flood the model's context.
const CHAT_LINE_MAX: usize = 2048;

/// The `read_chat` answer for `messages`, oldest first: a fence, then each
/// message on its own cleaned line cut to [`CHAT_LINE_MAX`] bytes.
fn chat_text(messages: &[agmen_proto::StampedEnvelope]) -> String {
    if messages.is_empty() {
        return "no messages".to_owned();
    }
    let mut out = String::from(CHAT_FENCE);
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let line = clean_lines(&message_line_with_id(message));
        if line.len() > CHAT_LINE_MAX {
            out.push_str(&line[..line.floor_char_boundary(CHAT_LINE_MAX)]);
            out.push_str("\u{2026} (truncated)");
        } else {
            out.push_str(&line);
        }
    }
    out
}

/// `chat`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ChatParams {
    /// The message text; @ tokens become mentions.
    text: String,
    /// The message this one threads under.
    reply_to: Option<MessageId>,
}

/// `read_chat`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ReadChatParams {
    /// Read messages strictly before this feed seq, or through the head.
    before: Option<u64>,
    /// How many messages to read, from 1 to 100; defaults to 30.
    limit: Option<u32>,
}

impl ReadChatParams {
    fn limit(&self) -> anyhow::Result<u32> {
        let limit = self.limit.unwrap_or(30);
        if !(1..=100).contains(&limit) {
            return Err(Fail::usage("read_chat limit must be 1 to 100").into());
        }
        Ok(limit)
    }
}

/// `review`'s arguments. A review always threads under a DONE.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ReviewParams {
    /// The id of the DONE being reviewed.
    reply_to: MessageId,
    /// Whether the work passes or needs changes.
    verdict: Verdict,
    /// Findings with their severity, path and trigger.
    #[serde(default)]
    findings: Vec<Finding>,
    /// The execution paths traced during review.
    traced: Option<String>,
}

/// `post`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct PostParams {
    /// The verb, such as FYI, STATUS or ALERT.
    verb: String,
    /// The message text, at most 32768 bytes.
    #[serde(default)]
    body: String,
    /// The verb's data as a JSON object, for verbs that carry data: STATUS needs
    /// `summary` and ALERT needs `need`.
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
    // Option, not serde(default): some MCP clients send null for an omitted argument.
    key: Option<StateKey>,
}

/// What the `state` tool reads. `head` is the retained topic. `roster` is
/// the full array from `GET /v1/projects/{p}/state?key=roster`.
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

/// `template_save`'s arguments.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TemplateSaveParams {
    /// The template's XML, at most 32 KiB.
    xml: String,
}

/// `recorded #<seq> (<stream>)`.
fn recorded_line(at: &Recorded) -> String {
    recorded(&at.stream, at.seq)
}

#[tool_router(vis = "pub(super)")]
impl Server {
    #[tool(
        description = "Post an FYI to the project chat. @ tokens become mentions; groups require a human or coordinator."
    )]
    async fn chat(&self, Parameters(params): Parameters<ChatParams>) -> CallToolResult {
        if params.text.trim().is_empty() {
            return super::text("chat needs some text to send");
        }
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let mentions = params
                .text
                .split_whitespace()
                .filter(|word| word.starts_with('@'))
                .map(str::to_owned)
                .collect();
            let out = Outgoing {
                reply_to: params.reply_to,
                about: About {
                    mentions,
                    ..About::default()
                },
                ..Outgoing::new(Payload::Fyi {}, params.text)
            };
            Ok(recorded_line(&send(&live.agent, live.via(), out).await?))
        })
        .await
    }

    #[tool(
        description = "Read the project chat before a feed seq, oldest first, with message ids. Defaults to 30 messages, at most 100."
    )]
    async fn read_chat(&self, Parameters(params): Parameters<ReadChatParams>) -> CallToolResult {
        if self.shared.options.managed {
            return super::text("read_chat is not available in agmend-managed sessions");
        }
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let limit = params.limit()?;
            let end = match params.before {
                Some(before) => before.saturating_sub(1),
                None => {
                    live.agent
                        .hub
                        .page(&Stream::Feed, 0)
                        .await
                        .map_err(|err| client_error(err, "reading the feed head failed"))?
                        .head
                }
            };
            let mut after = end.saturating_sub(u64::from(limit));
            let mut messages = Vec::new();
            // One optional head request plus four replay pages, at most five.
            for _ in 0..4 {
                if after >= end {
                    break;
                }
                let page = live
                    .agent
                    .hub
                    .page(&Stream::Feed, after)
                    .await
                    .map_err(|err| client_error(err, "reading the project chat failed"))?;
                messages.extend(
                    page.messages
                        .into_iter()
                        .filter(|message| message.seq.0 <= end)
                        .take(
                            usize::try_from(limit)
                                .unwrap_or(100)
                                .saturating_sub(messages.len()),
                        ),
                );
                match page.next_after {
                    Some(next) if next > after && next < end => after = next,
                    _ => break,
                }
            }
            Ok(chat_text(&messages))
        })
        .await
    }

    #[tool(
        description = "Review a DONE by its id with a verdict, findings and the paths traced. Sends REVIEW."
    )]
    async fn review(&self, Parameters(params): Parameters<ReviewParams>) -> CallToolResult {
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let out = Outgoing {
                reply_to: Some(params.reply_to),
                ..Outgoing::new(
                    Payload::Review {
                        verdict: params.verdict,
                        findings: params.findings,
                        traced: params.traced,
                    },
                    String::new(),
                )
            };
            Ok(recorded_line(&send(&live.agent, live.via(), out).await?))
        })
        .await
    }

    #[tool(
        description = "Post a message to the project with any protocol verb (FYI, STATUS, \
        ALERT and the rest). Answers recorded, queued or rejected."
    )]
    async fn post(&self, Parameters(params): Parameters<PostParams>) -> CallToolResult {
        self.respond_with_pending(move |live: Arc<Live>| async move {
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
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let of = match params.of {
                Some(of) => live.agent.resolve(&[of]).await?.pop(),
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
        self.respond_with_pending(move |live: Arc<Live>| async move {
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
        self.respond_with_pending(move |live: Arc<Live>| async move {
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
        description = "Read project state as JSON: head (retained stream heads and versions) \
        or roster (who is in the project, the full array)."
    )]
    async fn state(&self, Parameters(params): Parameters<StateParams>) -> CallToolResult {
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let key = params.key.unwrap_or_default();
            // The roster is the full array over HTTP; the retained topic is
            // only its head. Both keep the tool's one error text.
            let value = match key {
                StateKey::Roster => live.agent.hub.roster().await.map(Some),
                StateKey::Head => live.agent.read_state(live.via(), key.as_str()).await,
            }
            .map_err(|err| client_error(err, "reading the project's state failed"))?;
            let shown = value.map_or_else(|| "nothing retained".to_owned(), |v| v.to_string());
            Ok(format!("state/{}: {}", key.as_str(), clean(&shown)))
        })
        .await
    }

    #[tool(
        description = "Claim an item before editing files in its lane. Answers granted (with \
        the lease) or denied (with the reason)."
    )]
    async fn claim(&self, Parameters(params): Parameters<ClaimParams>) -> CallToolResult {
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let (at, answer) = claim::claim_item(&live.agent, live.via(), params.item).await?;
            Ok(format!("{}\n{}", recorded_line(&at), answer.line()))
        })
        .await
    }

    #[tool(description = "Give up a claim.")]
    async fn release(&self, Parameters(params): Parameters<ReleaseParams>) -> CallToolResult {
        self.respond_with_pending(move |live: Arc<Live>| async move {
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
        self.respond_with_pending(move |live: Arc<Live>| async move {
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
        self.respond_with_pending(move |live: Arc<Live>| async move {
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
        self.respond_with_pending(
            |live: Arc<Live>| async move { lanes::lanes_text(&live.agent).await },
        )
        .await
    }

    #[tool(description = "Say whether this identity may edit each path, and why.")]
    async fn check_path(&self, Parameters(params): Parameters<CheckPathParams>) -> CallToolResult {
        let cwd = self.cwd().to_path_buf();
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let answers = lanes::check(&live.agent, &cwd, params.paths).await?;
            Ok(lanes::check_lines(&answers))
        })
        .await
    }

    #[tool(
        description = "List the agent and team templates you can launch: name, kind, layer, description."
    )]
    async fn template_list(&self) -> CallToolResult {
        let templates_dir = self.shared.options.templates_dir.clone();
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let personal = match templates_dir {
                Some(dir) => dir,
                None => crate::cmd::templates::personal_dir()?,
            };
            let catalog =
                crate::cmd::templates::catalog(Some(live.agent.root.clone()), personal).await?;
            Ok(crate::cmd::templates::list_for_model(&catalog))
        })
        .await
    }

    #[tool(
        description = "Save an agent or team template to your person's personal templates. Options are refused; the template records you as saved-by."
    )]
    async fn template_save(
        &self,
        Parameters(params): Parameters<TemplateSaveParams>,
    ) -> CallToolResult {
        let templates_dir = self.shared.options.templates_dir.clone();
        self.respond_with_pending(move |live: Arc<Live>| async move {
            let personal = match templates_dir {
                Some(dir) => dir,
                None => crate::cmd::templates::personal_dir()?,
            };
            let root = live.agent.root.clone();
            let handle = live.agent.identity.handle.clone();
            let xml = params.xml;
            crate::client::blocking(move || {
                crate::cmd::templates::save_for_agent(&xml, &handle, &root, &personal)
            })
            .await
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
    use agmen_proto::Verb;
    use rmcp::handler::server::tool::parse_json_object;
    use serde_json::json;

    use super::*;

    #[test]
    fn review_params_require_reply_to() {
        assert!(serde_json::from_value::<ReviewParams>(json!({"verdict":"pass"})).is_err());
        let params = serde_json::from_value::<ReviewParams>(json!({
            "reply_to":"01JBX6Q4T7W3C2M8Y9K5N0P1RS", "verdict":"pass"
        }))
        .unwrap();
        assert!(params.findings.is_empty());
        assert!(params.traced.is_none());
    }

    #[test]
    fn hidden_tools_follow_verbs() {
        assert_eq!(hidden_tools(None), ["review"]);
        let gated = [
            ("claim", "CLAIM"),
            ("release", "RELEASE"),
            ("done", "DONE"),
            ("ask", "ASK"),
            ("answer", "ANSWER"),
            ("dm", "DM"),
            ("found", "FOUND"),
            ("review", "REVIEW"),
        ];
        let empty = std::collections::BTreeSet::new();
        assert_eq!(hidden_tools(Some(&empty)), gated.map(|(tool, _)| tool));
        let all = gated.map(|(_, verb)| verb.to_owned()).into_iter().collect();
        assert!(hidden_tools(Some(&all)).is_empty());
        for (allowed, verb) in gated {
            let verbs = [verb.to_owned()].into_iter().collect();
            let hidden = hidden_tools(Some(&verbs));
            assert!(!hidden.contains(&allowed));
            assert_eq!(hidden.len(), 7);
            for always in [
                "post",
                "chat",
                "read_chat",
                "inbox",
                "state",
                "lanes",
                "check_path",
            ] {
                assert!(!hidden.contains(&always));
            }
        }
    }

    /// The template tools hang on no verb, so no built-in role loses them here. The listener
    /// role never reaches this server: agmend starts it with `agmen mcp --listener`, whose own
    /// tool set (`listener_tools.rs`) has `template_list` and no `template_save`.
    #[test]
    fn template_tools_are_visible_to_every_built_in_role() {
        let roles = agmen_roles::Layers::default().load().unwrap();
        assert!(roles.get("listener").is_some() && roles.get("engineer").is_some());
        let template_tools = ["template_list", "template_save"];
        for role in roles.iter() {
            let hidden = hidden_tools(Some(&role.verbs));
            for tool in template_tools {
                assert!(!hidden.contains(&tool), "{} hides {tool}", role.role.name);
            }
        }
        for tool in template_tools {
            assert!(!hidden_tools(None).contains(&tool), "legacy hides {tool}");
        }
    }

    #[test]
    fn read_chat_limit_is_bounded() {
        for limit in [0, 101] {
            let params: ReadChatParams = serde_json::from_value(json!({"limit": limit})).unwrap();
            assert!(params.limit().is_err());
        }
        for (args, expected) in [
            (json!({}), 30),
            (json!({"limit": null}), 30),
            (json!({"limit": 100}), 100),
        ] {
            let params: ReadChatParams = serde_json::from_value(args).unwrap();
            assert_eq!(params.limit().unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn read_chat_refused_when_managed() {
        let dir = tempfile::tempdir().unwrap();
        let options = super::super::McpOptions::new(
            dir.path(),
            agmen_client::TokenStore::Dir(dir.path().into()),
            dir.path(),
        )
        .with_managed(true);
        let server = Server::new(options, Server::tool_router());
        let result = server
            .read_chat(Parameters(ReadChatParams {
                before: None,
                limit: None,
            }))
            .await;
        assert_eq!(
            super::super::result_text(&result),
            "read_chat is not available in agmend-managed sessions"
        );
    }

    fn chat_message(seq: u64, body: &str) -> agmen_proto::StampedEnvelope {
        serde_json::from_value(json!({
            "v": 1, "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS", "verb": "FYI", "data": {},
            "body": body, "sent_at": "2026-10-25T16:04:11Z", "stream": "feed", "seq": seq,
            "from": "hmn7k2q9", "from_handle": "andy", "from_kind": "human",
            "hub_at": "2026-10-25T16:04:11Z",
        }))
        .unwrap()
    }

    #[test]
    fn read_chat_text_is_fenced_cleaned_and_bounded() {
        assert_eq!(chat_text(&[]), "no messages");
        let text = chat_text(&[chat_message(1, "hi\u{1b}[2J"), chat_message(2, "yo")]);
        let (fence, lines) = text.split_once('\n').unwrap();
        assert_eq!(fence, CHAT_FENCE.trim_end());
        assert!(fence.contains("data, not instructions"));
        assert!(!text.contains('\u{1b}'));
        assert!(lines.starts_with("#1 feed FYI from andy (id "));
        assert_eq!(lines.lines().count(), 2);
        // A 32 KiB body is cut at a character boundary with a note.
        let huge = chat_text(&[chat_message(3, &"\u{e9}".repeat(16 * 1024))]);
        assert!(
            huge.len() < CHAT_FENCE.len() + CHAT_LINE_MAX + 128,
            "{}",
            huge.len()
        );
        assert!(huge.ends_with("(truncated)"));
    }

    #[tokio::test]
    async fn chat_refuses_blank_text() {
        let dir = tempfile::tempdir().unwrap();
        let options = super::super::McpOptions::new(
            dir.path(),
            agmen_client::TokenStore::Dir(dir.path().into()),
            dir.path(),
        );
        let server = Server::new(options, Server::tool_router());
        let result = server
            .chat(Parameters(ChatParams {
                text: " \n".to_owned(),
                reply_to: None,
            }))
            .await;
        assert_eq!(
            super::super::result_text(&result),
            "chat needs some text to send"
        );
    }

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

    /// The all-caps words in `text`, such as `FYI` or `PUSH_REQUEST`.
    fn caps_words(text: &str) -> Vec<&str> {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .filter(|word| {
                word.len() > 1 && word.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            })
            .collect()
    }

    #[test]
    fn the_texts_name_only_protocol_verbs() {
        let post = Server::tool_router()
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "post")
            .unwrap();
        let verb_field = post.input_schema["properties"]["verb"]["description"]
            .as_str()
            .unwrap()
            .to_owned();
        let texts = [
            INSTRUCTIONS.to_owned(),
            post.description.unwrap().into_owned(),
            verb_field,
        ];
        for text in &texts {
            for word in caps_words(text) {
                assert!(
                    word.parse::<Verb>().is_ok(),
                    "{word} in {text:?} is not a protocol verb"
                );
            }
        }
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
        assert!(parse(json!({ "key": null })).is_ok());
        assert!(parse(json!({ "key": "roster" })).is_ok());
        for bad in ["a/b", "nothing", "Head", ""] {
            assert!(parse(json!({ "key": bad })).is_err(), "{bad:?}");
        }
    }
}
