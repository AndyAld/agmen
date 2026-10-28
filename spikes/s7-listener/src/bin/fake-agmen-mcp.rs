//! Fake agmen MCP tools for S7. Never connects to agmend or launches workers.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, ServiceExt as _, tool, tool_handler, tool_router};
use s7_listener::DelegateArgs;
use s7_listener::harness::{MAX_INPUT_BYTES, MAX_OUTPUT_BYTES, ToolCall};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

const MAX_CALLS: usize = 1024;
const SERVER_LIFETIME: Duration = Duration::from_secs(180);

#[derive(Debug)]
struct Log {
    file: tokio::fs::File,
    bytes: usize,
    calls: usize,
}

#[derive(Debug, Clone)]
struct Fake {
    log: Arc<Mutex<Log>>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct AnswerArgs {
    reply_to: String,
    body: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct DmArgs {
    to: Vec<String>,
    body: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct PostArgs {
    verb: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct StateArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

impl Fake {
    async fn record(&self, tool: &str, args: impl Serialize, result: Value) -> CallToolResult {
        match self.append(tool, args).await {
            Ok(()) => CallToolResult::success(vec![ContentBlock::text(result.to_string())]),
            Err(_) => CallToolResult::error(vec![ContentBlock::text("fake MCP call log failed")]),
        }
    }

    async fn append(&self, tool: &str, args: impl Serialize) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(&ToolCall {
            tool: tool.into(),
            args: serde_json::to_value(args).context("encoding fake tool args")?,
        })
        .context("encoding fake tool call")?;
        bytes.push(b'\n');
        let mut log = self.log.lock().await;
        anyhow::ensure!(
            bytes.len() <= MAX_INPUT_BYTES,
            "fake MCP call exceeds 64 KiB"
        );
        anyhow::ensure!(log.calls < MAX_CALLS, "fake MCP call limit reached");
        anyhow::ensure!(
            log.bytes + bytes.len() <= MAX_OUTPUT_BYTES,
            "fake MCP log exceeds 1 MiB"
        );
        tokio::time::timeout(Duration::from_secs(5), log.file.write_all(&bytes))
            .await
            .context("fake MCP log write timed out")?
            .context("appending fake MCP call")?;
        log.bytes += bytes.len();
        log.calls += 1;
        Ok(())
    }
}

#[tool_router]
impl Fake {
    #[tool(description = "Answer a message with a canned acknowledgement")]
    async fn answer(&self, Parameters(args): Parameters<AnswerArgs>) -> CallToolResult {
        self.record("answer", args, json!({"recorded":true})).await
    }

    #[tool(description = "Forward a direct message without sending it")]
    async fn dm(&self, Parameters(args): Parameters<DmArgs>) -> CallToolResult {
        self.record("dm", args, json!({"recorded":true})).await
    }

    #[tool(description = "Record a post without publishing it")]
    async fn post(&self, Parameters(args): Parameters<PostArgs>) -> CallToolResult {
        self.record("post", args, json!({"recorded":true})).await
    }

    #[tool(description = "Read canned project state")]
    async fn state(&self, Parameters(args): Parameters<StateArgs>) -> CallToolResult {
        let result = match args.key.as_deref() {
            Some("roster") => json!({"roster":[]}),
            _ => json!({"feed_seq":0}),
        };
        self.record("state", args, result).await
    }

    #[tool(description = "List canned workers")]
    async fn workers(&self) -> CallToolResult {
        self.record("workers", json!({}), json!({"workers":[]}))
            .await
    }

    #[tool(description = "Record a delegation without launching or spending money")]
    async fn delegate(&self, Parameters(args): Parameters<DelegateArgs>) -> CallToolResult {
        self.record(
            "delegate",
            args,
            json!({"worker":"fake-worker", "delegated":true}),
        )
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Fake {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("fake-agmen-mcp", env!("CARGO_PKG_VERSION")),
        )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path =
        PathBuf::from(std::env::var_os("S7_LOG").context("S7_LOG must name the fake tool log")?);
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .context("opening S7_LOG")?;
    let bytes = usize::try_from(file.metadata().await.context("reading S7_LOG size")?.len())?;
    anyhow::ensure!(bytes <= MAX_OUTPUT_BYTES, "existing S7_LOG exceeds 1 MiB");
    let fake = Fake {
        log: Arc::new(Mutex::new(Log {
            file,
            bytes,
            calls: 0,
        })),
        tool_router: Fake::tool_router(),
    };
    tokio::time::timeout(SERVER_LIFETIME, async {
        let server = fake
            .serve(rmcp::transport::stdio())
            .await
            .context("starting fake MCP stdio")?;
        server.waiting().await.context("serving fake MCP stdio")?;
        anyhow::Ok(())
    })
    .await
    .context("fake MCP exceeded 180 second lifetime")??;
    Ok(())
}
