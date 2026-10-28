//! `agmen mcp` in this process, driven by an rmcp client over an in-memory
//! duplex, and a recorder that stands in for a hub and a broker and counts
//! every request and connection made to them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use agmen_cli::{McpError, McpOptions, serve};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};
use serde_json::Value;
use tokio::task::JoinHandle;
use url::Url;

/// The answer every tool gives outside a joined worktree.
pub const NOT_JOINED: &str = "not joined: run agmen join <invite> in this worktree";

/// The twelve tools, sorted.
pub const TOOLS: [&str; 12] = [
    "answer",
    "ask",
    "check_path",
    "claim",
    "dm",
    "done",
    "found",
    "inbox",
    "lanes",
    "post",
    "release",
    "state",
];

/// A running `agmen mcp` and the client connected to it.
pub struct Mcp {
    client: RunningService<RoleClient, ()>,
    server: JoinHandle<Result<(), McpError>>,
}

impl Mcp {
    /// Starts the server with `options` and connects a client. The broker
    /// forgets the server's session as soon as it stops.
    pub async fn start(options: McpOptions) -> Self {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let options = options.with_session_expiry(Duration::ZERO);
        let server = tokio::spawn(serve(server_io, options));
        let client = ().serve(client_io).await.expect("the MCP handshake");
        Self { client, server }
    }

    /// The names of the tools the server lists, sorted.
    pub async fn tools(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .client
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        names.sort();
        names
    }

    /// Calls `tool` with `args`, a JSON object.
    pub async fn call(&self, tool: &'static str, args: Value) -> CallToolResult {
        let Value::Object(args) = args else {
            panic!("tool arguments are a JSON object");
        };
        self.client
            .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
            .await
            .unwrap_or_else(|err| panic!("calling {tool}: {err}"))
    }

    /// Calls `tool` and returns its text, which must not be marked as an
    /// error.
    pub async fn text(&self, tool: &'static str, args: Value) -> String {
        let result = self.call(tool, args).await;
        let text = text_of(&result);
        assert_eq!(result.is_error, Some(false), "{tool}: {text}");
        text
    }

    /// Closes the client and waits for the server to stop.
    pub async fn stop(self) {
        self.client.cancel().await.unwrap();
        tokio::time::timeout(Duration::from_secs(20), self.server)
            .await
            .expect("agmen mcp stops once its client is gone")
            .unwrap()
            .unwrap();
    }
}

/// The text of `result`'s content, one block per line.
pub fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .map(|block| block.as_text().expect("text content").text.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every tool with arguments that pass its schema.
pub fn every_tool_call() -> Vec<(&'static str, Value)> {
    use serde_json::json;
    vec![
        ("post", json!({ "verb": "FYI", "body": "hi" })),
        ("ask", json!({ "body": "which way?" })),
        (
            "answer",
            json!({ "reply_to": "01JBX6Q4T7W3C2M8Y9K5N0P1RS", "body": "left" }),
        ),
        ("dm", json!({ "to": ["andy/sol"], "body": "hi" })),
        ("inbox", json!({})),
        ("state", json!({})),
        ("claim", json!({ "item": "B-1" })),
        ("release", json!({ "item": "B-1" })),
        ("done", json!({ "item": "B-1", "commit": "HEAD" })),
        ("found", json!({ "title": "the build is slow" })),
        ("lanes", json!({})),
        ("check_path", json!({ "paths": ["src/a.rs"] })),
    ]
}

/// A stand-in hub and broker on loopback that answer nothing useful and
/// count every HTTP request and TCP connection they get.
pub struct Recorder {
    /// The hub's URL.
    pub hub: Url,
    /// The broker's `mqtt://` URL.
    pub broker: String,
    requests: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Recorder {
    pub async fn start() -> Self {
        let requests = Arc::new(AtomicUsize::new(0));
        let hub_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub = Url::parse(&format!("http://{}", hub_listener.local_addr().unwrap())).unwrap();
        let counted = requests.clone();
        let router = axum::Router::new().fallback(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }
        });
        let http = tokio::spawn(async move { axum::serve(hub_listener, router).await.unwrap() });

        let connections = Arc::new(AtomicUsize::new(0));
        let broker_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let broker = format!("mqtt://{}", broker_listener.local_addr().unwrap());
        let counted = connections.clone();
        let tcp = tokio::spawn(async move {
            while let Ok((stream, _)) = broker_listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        Self {
            hub,
            broker,
            requests,
            connections,
            tasks: vec![http, tcp],
        }
    }

    /// HTTP requests and TCP connections so far.
    pub fn hits(&self) -> (usize, usize) {
        (
            self.requests.load(Ordering::SeqCst),
            self.connections.load(Ordering::SeqCst),
        )
    }
}
