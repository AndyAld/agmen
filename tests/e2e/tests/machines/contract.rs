//! The contract between the CLI and the Claude Code plugin: what `agmen
//! watch --json` and `agmen check-path --json` print is what the plugin's
//! parsers in mods/agmen-claude/hooks/register.tsx read.
//!
//! The wiring: `it_plugin_contract_matches_the_cli` captures real output
//! from the CLI against a real hub and broker, with this run's worktree
//! path, ids and handles replaced by fixed ones, and compares its shape
//! (each line's fields, value types and id formats) with the output the
//! fixture file [`FIXTURE`] holds. The plugin's own test
//! (register.test.tsx, `parses_real_cli_output`) feeds the fixture's lines
//! through the plugin. So a CLI change that alters the output fails here
//! until the fixture is captured again ([`UPDATE_VAR`]), and the plugin
//! test then runs against the new output.

use std::collections::BTreeSet;
use std::str::FromStr;

use agmen_proto::{GitSha, IdentityId, ItemKey, LaneName, MessageId, Stream, Timestamp, Verb};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The fixture the plugin test reads: a TypeScript module whose one export
/// is the captured output as a JSON literal.
pub const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../mods/agmen-claude/hooks/cli-output.fixture.ts"
);

/// Set to 1 to write [`FIXTURE`] from this run's output instead of
/// comparing.
pub const UPDATE_VAR: &str = "AGMEN_E2E_UPDATE_FIXTURE";

/// What [`FIXTURE`] says before the JSON literal.
const HEADER: &str = "\
// Real output of the agmen CLI, captured by tests/e2e/tests/it_m1.rs
// (it_plugin_contract_matches_the_cli) and read by register.test.tsx, so
// the plugin's parsers run against what the CLI prints. The worktree path,
// ids and handles of the run are replaced by fixed ones. Do not edit it by
// hand; capture it again with
//   AGMEN_E2E_UPDATE_FIXTURE=1 cargo nextest run -p agmen-e2e --profile it -E 'test(it_plugin_contract)'
export const CLI_OUTPUT = ";

/// The output the plugin reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliOutput {
    /// `agmen watch --json`'s lines, as printed.
    pub watch: Vec<String>,
    /// `agmen check-path --json -- <paths>` runs.
    pub check_path: Vec<CheckPathRun>,
}

/// One `agmen check-path --json -- <paths>` run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckPathRun {
    /// The paths asked.
    pub paths: Vec<String>,
    /// What it printed.
    pub stdout: String,
}

/// Fixed stand-ins for what differs between runs, applied in order.
#[derive(Debug, Default)]
pub struct Replace(Vec<(String, String)>);

impl Replace {
    /// Replaces `from` with `to` in everything [`Replace::apply`] gets.
    pub fn with(mut self, from: impl Into<String>, to: &str) -> Self {
        self.0.push((from.into(), to.to_owned()));
        self
    }

    /// `text` with every stand-in applied.
    pub fn apply(&self, text: &str) -> String {
        self.0
            .iter()
            .fold(text.to_owned(), |text, (from, to)| text.replace(from, to))
    }
}

/// Compares `fresh` with [`FIXTURE`] by shape, or writes it there when
/// [`UPDATE_VAR`] is 1.
pub fn check_or_update(fresh: &CliOutput) {
    if std::env::var(UPDATE_VAR).as_deref() == Ok("1") {
        let json = serde_json::to_string_pretty(fresh).unwrap();
        std::fs::write(FIXTURE, format!("{HEADER}{json}\n")).unwrap();
        return;
    }
    let text = std::fs::read_to_string(FIXTURE).unwrap();
    let json = text
        .strip_prefix(HEADER)
        .unwrap_or_else(|| panic!("{FIXTURE} does not start with the header this test writes"));
    let fixture: CliOutput = serde_json::from_str(json).unwrap();
    assert_eq!(
        shapes(fresh),
        shapes(&fixture),
        "the CLI's output changed shape since {FIXTURE} was captured: capture it again with \
         {UPDATE_VAR}=1, then run the plugin's tests (npm test in mods/agmen-claude)"
    );
}

/// The distinct shapes of `output`'s watch lines and check-path answers.
fn shapes(output: &CliOutput) -> BTreeSet<String> {
    let watch = output.watch.iter().map(|line| ("watch", line));
    let check = output
        .check_path
        .iter()
        .map(|run| ("check-path", &run.stdout));
    watch
        .chain(check)
        .map(|(source, text)| {
            let value: Value = serde_json::from_str(text).unwrap();
            format!("{source} {}", shape(&value))
        })
        .collect()
}

/// `value` with each string replaced by the format it has, each number by
/// `number`, and each array by the distinct shapes of its entries.
fn shape(value: &Value) -> Value {
    match value {
        Value::Null => Value::from("null"),
        Value::Bool(_) => Value::from("bool"),
        Value::Number(_) => Value::from("number"),
        Value::String(text) => Value::from(format_of(text)),
        Value::Array(entries) => {
            let distinct: BTreeSet<String> = entries.iter().map(|v| shape(v).to_string()).collect();
            Value::from(distinct.into_iter().collect::<Vec<_>>())
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| (name.clone(), shape(field)))
                .collect(),
        ),
    }
}

/// Which agmen-proto format `text` has, if any.
fn format_of(text: &str) -> &'static str {
    let is = |ok: bool, name| ok.then_some(name);
    is(MessageId::from_str(text).is_ok(), "message id")
        .or_else(|| is(IdentityId::from_str(text).is_ok(), "identity id"))
        .or_else(|| is(Verb::from_str(text).is_ok(), "verb"))
        .or_else(|| is(Timestamp::from_str(text).is_ok(), "timestamp"))
        .or_else(|| is(ItemKey::from_str(text).is_ok(), "item key"))
        .or_else(|| is(LaneName::from_str(text).is_ok(), "lane"))
        .or_else(|| is(Stream::from_str(text).is_ok(), "stream"))
        .or_else(|| is(GitSha::from_str(text).is_ok(), "git sha"))
        .or_else(|| is(text.starts_with('/'), "path"))
        .unwrap_or("text")
}
