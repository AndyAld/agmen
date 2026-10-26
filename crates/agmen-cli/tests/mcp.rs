//! `agmen mcp` without a hub: the tools it lists, and staying inert (the
//! not-joined answer, no request, no connection, no token sent) outside a
//! joined worktree, with an identity file the trust test refuses (rulings
//! R42 to R42b), or with `--identity` naming another identity; and the
//! `agmen mcp` binary itself over stdio.

// Test code throughout, helpers included: this lets clippy's
// allow-*-in-tests settings cover the helpers too.
#![cfg(test)]

mod support;

use std::path::Path;
use std::process::Stdio;

use agmen_cli::McpOptions;
use agmen_client::TokenKey;
use agmen_proto::IdentityId;
use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use support::mcp::{Mcp, NOT_JOINED, Recorder, TOOLS, every_tool_call, text_of};
use support::{Home, Worktree};

const ID: &str = "idn7k2q9";

/// Options for a server in `cwd` with `home`'s token store and state.
fn options(home: &Home, cwd: &Path) -> McpOptions {
    McpOptions::new(cwd, home.tokens(), home.state_base())
}

/// An identity file for `ID` whose hub and broker are `recorder`'s.
fn identity_json(recorder: &Recorder) -> String {
    serde_json::json!({
        "tenant": "tenant01", "project": "project1", "identity_id": ID,
        "handle": "andy/sol", "owner_id": "andyandy",
        "hub_url": recorder.hub, "broker_url": recorder.broker,
    })
    .to_string()
}

/// Stores a token for `ID` at `recorder`'s hub in `home`, so a server
/// that trusted the identity file would send it there.
fn store_token(home: &Home, recorder: &Recorder) {
    let id: IdentityId = ID.parse().unwrap();
    home.tokens()
        .set(&TokenKey::new(&recorder.hub, &id).unwrap(), "secret-token")
        .unwrap();
}

/// Writes `.agmen/identity.json` in `wt` and excludes `.agmen/` as
/// `agmen join` does.
fn join_like_agmen(wt: &Worktree, recorder: &Recorder) {
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(
        wt.path().join(".agmen/identity.json"),
        identity_json(recorder),
    )
    .unwrap();
    std::fs::write(wt.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
}

/// Calls every tool and asserts each answers the not-joined text without
/// marking it an error; then stops the server.
async fn assert_inert(mcp: Mcp) {
    for (tool, args) in every_tool_call() {
        let result = mcp.call(tool, args).await;
        assert_eq!(text_of(&result), NOT_JOINED, "{tool}");
        assert_eq!(result.is_error, Some(false), "{tool}");
    }
    mcp.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_tools() {
    let home = Home::new();
    let mcp = Mcp::start(options(&home, home.path())).await;
    assert_eq!(mcp.tools().await, TOOLS);
    mcp.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn not_joined_answers_every_tool() {
    let home = Home::new();
    // Outside any git worktree, then in a worktree with no identity file.
    assert_inert(Mcp::start(options(&home, home.path())).await).await;
    let wt = Worktree::new(&home);
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
}

/// The control for the inert tests below: with a trusted identity file the
/// server does ask the recorder for a broker token, so a count of zero
/// there means something.
#[tokio::test(flavor = "multi_thread")]
async fn trusted_identity_file_reaches_the_hub() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    let mcp = Mcp::start(options(&home, &wt.path())).await;
    let result = mcp.call("lanes", serde_json::json!({})).await;
    assert_ne!(text_of(&result), NOT_JOINED);
    assert_eq!(result.is_error, Some(true), "{}", text_of(&result));
    mcp.stop().await;
    assert!(recorder.hits().0 > 0, "{:?}", recorder.hits());
}

#[tokio::test(flavor = "multi_thread")]
async fn tracked_identity_file_leaves_mcp_inert() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    std::fs::create_dir(wt.path().join(".agmen")).unwrap();
    std::fs::write(
        wt.path().join(".agmen/identity.json"),
        identity_json(&recorder),
    )
    .unwrap();
    wt.git(&home, &["add", ".agmen/identity.json"]);
    wt.git(&home, &["commit", "-q", "-m", "an identity file"]);
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
    assert_eq!(recorder.hits(), (0, 0));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn symlinked_agmen_dir_leaves_mcp_inert() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::create_dir(elsewhere.path().join(".agmen")).unwrap();
    std::fs::write(
        elsewhere.path().join(".agmen/identity.json"),
        identity_json(&recorder),
    )
    .unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join(".agmen"), wt.path().join(".agmen")).unwrap();
    std::fs::write(wt.path().join(".git/info/exclude"), ".agmen/\n").unwrap();
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
    assert_eq!(recorder.hits(), (0, 0));
}

/// A committed `.AGMEN/identity.json` beside a committed `.gitignore`
/// naming `.agmen/`: on a case-insensitive file system it opens as
/// `.agmen/identity.json` and passes check-ignore; on a case-sensitive one
/// there is no `.agmen` at all. Either way nothing connects.
#[tokio::test(flavor = "multi_thread")]
async fn case_variant_identity_dir_leaves_mcp_inert() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    std::fs::create_dir(wt.path().join(".AGMEN")).unwrap();
    std::fs::write(
        wt.path().join(".AGMEN/identity.json"),
        identity_json(&recorder),
    )
    .unwrap();
    std::fs::write(wt.path().join(".gitignore"), ".agmen/\n").unwrap();
    wt.git(&home, &["add", "-f", ".gitignore", ".AGMEN/identity.json"]);
    wt.git(&home, &["commit", "-q", "-m", "a case variant"]);
    assert_inert(Mcp::start(options(&home, &wt.path())).await).await;
    assert_eq!(recorder.hits(), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn identity_override_must_match_the_worktree() {
    let recorder = Recorder::start().await;
    let home = Home::new();
    store_token(&home, &recorder);
    let wt = Worktree::new(&home);
    join_like_agmen(&wt, &recorder);
    let other: IdentityId = "idother99".parse().unwrap();
    let mcp = Mcp::start(options(&home, &wt.path()).with_identity(other)).await;
    for (tool, args) in every_tool_call() {
        let result = mcp.call(tool, args).await;
        assert_eq!(
            text_of(&result),
            format!("not joined as idother99: this worktree's identity is {ID}"),
            "{tool}"
        );
        assert_eq!(result.is_error, Some(false), "{tool}");
    }
    mcp.stop().await;
    assert_eq!(recorder.hits(), (0, 0));
}

/// The binary, as Claude Code starts it: `agmen mcp` on stdin and stdout,
/// its working directory and environment deciding the identity.
#[tokio::test(flavor = "multi_thread")]
async fn binary_serves_over_stdio() {
    let home = Home::new();
    let wt = Worktree::new(&home);
    let mut cmd = tokio::process::Command::from(home.agmen(&wt.path(), &["mcp"]));
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut child = cmd.spawn().unwrap();
    let io = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
    let client = ().serve(io).await.expect("the MCP handshake");
    let info = client.peer_info().expect("the server introduced itself");
    let name = info.server_info.as_ref().map(|server| server.name.as_str());
    assert_eq!(name, Some("agmen"));
    assert_eq!(client.list_all_tools().await.unwrap().len(), TOOLS.len());
    let args = serde_json::json!({ "verb": "FYI", "body": "hi" })
        .as_object()
        .unwrap()
        .clone();
    let result = client
        .call_tool(CallToolRequestParams::new("post").with_arguments(args))
        .await
        .unwrap();
    assert_eq!(text_of(&result), NOT_JOINED);
    client.cancel().await.unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(20), child.wait())
        .await
        .expect("agmen mcp exits once stdin closes")
        .unwrap();
    assert!(status.success(), "{status}");
}
