//! A `claude` program for agmend's managed-agent launches: an executable
//! script that records what agmend sent and prints one JSON result.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use tempfile::TempDir;

/// Marks one invocation in the record file. The prompt agmend sends is a
/// JSON batch, which does not contain this line.
const RUN_MARKER: &str = "\n__AGMEN_FAKE_RUN__\n";

/// The `agmen` binary the daemon's MCP config should name.
pub fn agmen() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agmen-e2e"))
}

/// A fake `claude` in its own temp folder.
#[derive(Debug)]
pub struct FakeClaude {
    dir: TempDir,
}

impl FakeClaude {
    /// A fake that answers with one JSON result: `total_cost_usd` 0.01,
    /// `is_error` false, and `result` `"ok"`.
    pub fn new() -> Self {
        Self::with_body(
            "{\"type\":\"result\",\"total_cost_usd\":0.01,\"is_error\":false,\"result\":\"ok\"}\n",
        )
    }

    /// As [`new`](Self::new), printing `body` unchanged as the whole answer.
    pub fn with_body(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("fake claude folder");
        fs::write(dir.path().join("answer.json"), body).expect("answer file");
        let program = dir.path().join("claude");
        fs::write(&program, SCRIPT).expect("fake claude script");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).expect("script mode");
        Self { dir }
    }

    /// The executable path agmend should run.
    pub fn program(&self) -> PathBuf {
        self.dir.path().join("claude")
    }

    /// A PATH whose first directory is this fake's folder, then `/usr/bin`
    /// and `/bin`.
    pub fn launch_path(&self) -> std::ffi::OsString {
        let mut path = std::ffi::OsString::from(self.dir.path());
        path.push(":/usr/bin:/bin");
        path
    }

    /// How many times agmend has run this program.
    pub fn runs(&self) -> usize {
        self.recorded().matches(RUN_MARKER).count()
    }

    /// Argv and stdin from every run, in order. Empty when it has not run.
    pub fn recorded(&self) -> String {
        match fs::read_to_string(self.record_path()) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("reading the fake claude record: {error}"),
        }
    }

    fn record_path(&self) -> PathBuf {
        self.dir.path().join("invocations")
    }
}

/// The script `claude` is. `$0` is absolute when agmend launches it.
/// Each run appends a marker, then every argument, then stdin.
const SCRIPT: &str = r#"#!/bin/sh
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
{
  printf '\n__AGMEN_FAKE_RUN__\n'
  printf '%s\n' "$@"
  printf '\n__AGMEN_FAKE_STDIN__\n'
  cat
} >> "$dir/invocations"
cat "$dir/answer.json"
"#;
