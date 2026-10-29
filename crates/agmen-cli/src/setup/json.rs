//! Merge the plain JSON MCP format while keeping unrelated settings.

use serde_json::{Value, json};

use super::{Merged, SetupError};

pub(crate) fn merge_mcp_servers(
    existing: Option<&str>,
    entry: &Value,
    file: &str,
) -> Result<Merged, SetupError> {
    let unreadable = |why: String| SetupError::Unreadable {
        file: file.into(),
        why,
    };
    let mut value: Value = match existing {
        Some(text) => serde_json::from_str(text).map_err(|err| unreadable(err.to_string()))?,
        None => json!({}),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| unreadable("expected a JSON object".into()))?;
    let servers = object.entry("mcpServers").or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| unreadable("mcpServers must be a JSON object".into()))?;
    if servers.get("agmen") == Some(entry) {
        return Ok(Merged {
            text: existing.unwrap_or_default().to_owned(),
            changed: false,
        });
    }
    servers.insert("agmen".into(), entry.clone());
    let text =
        serde_json::to_string_pretty(&value).map_err(|err| unreadable(err.to_string()))? + "\n";
    Ok(Merged {
        text,
        changed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::path::Path;

    fn claude_entry() -> Value {
        json!({"command":"agmen","args":["mcp"]})
    }

    #[test]
    fn adds_agmen_to_a_missing_file() {
        let merged = merge_mcp_servers(None, &claude_entry(), ".mcp.json").unwrap();
        assert!(merged.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&merged.text).unwrap(),
            json!({"mcpServers":{"agmen":{"command":"agmen","args":["mcp"]}}})
        );
    }

    #[test]
    fn keeps_other_servers_and_keys() {
        let merged = merge_mcp_servers(
            Some(r#"{"mcpServers":{"x":{"command":"x"}},"other":1}"#),
            &claude_entry(),
            ".mcp.json",
        )
        .unwrap();
        let value: Value = serde_json::from_str(&merged.text).unwrap();
        assert_eq!(value["mcpServers"]["x"], json!({"command":"x"}));
        assert_eq!(value["other"], 1);
        assert_eq!(
            value["mcpServers"]["agmen"],
            json!({"command":"agmen","args":["mcp"]})
        );
    }

    #[test]
    fn second_run_is_unchanged() {
        let first = merge_mcp_servers(None, &claude_entry(), ".mcp.json").unwrap();
        let second = merge_mcp_servers(Some(&first.text), &claude_entry(), ".mcp.json").unwrap();
        assert!(!second.changed);
        assert_eq!(first.text, second.text);
        let compact = r#"{"mcpServers":{"agmen":{"command":"agmen","args":["mcp"]}}}"#;
        assert_eq!(
            merge_mcp_servers(Some(compact), &claude_entry(), ".mcp.json")
                .unwrap()
                .text,
            compact
        );
    }

    #[test]
    fn replaces_a_stale_agmen_entry() {
        let merged = merge_mcp_servers(
            Some(r#"{"mcpServers":{"agmen":{"command":"old"}}}"#),
            &claude_entry(),
            ".mcp.json",
        )
        .unwrap();
        assert!(merged.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&merged.text).unwrap()["mcpServers"]["agmen"],
            json!({"command":"agmen","args":["mcp"]})
        );
    }

    #[test]
    fn writes_the_given_entry() {
        let cursor = json!({
            "type": "stdio",
            "command": "agmen",
            "args": ["mcp", "--worktree", "${workspaceFolder}"],
        });
        let first = merge_mcp_servers(None, &cursor, ".cursor/mcp.json").unwrap();
        assert!(first.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&first.text).unwrap(),
            json!({"mcpServers":{"agmen":cursor}})
        );
        let second = merge_mcp_servers(Some(&first.text), &cursor, ".cursor/mcp.json").unwrap();
        assert!(!second.changed);
        assert_eq!(second.text, first.text);
        let gemini = json!({"command":"agmen","args":["mcp"],"cwd":"/work/repo"});
        let third =
            merge_mcp_servers(Some(&second.text), &gemini, ".gemini/settings.json").unwrap();
        assert!(third.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&third.text).unwrap()["mcpServers"]["agmen"],
            gemini
        );
        let err = merge_mcp_servers(Some("[]"), &cursor, ".cursor/mcp.json").unwrap_err();
        let SetupError::Unreadable { file, .. } = err;
        assert_eq!(file, Path::new(".cursor/mcp.json"));
    }

    #[test]
    fn refuses_files_that_are_not_json_objects() {
        for text in ["[]", "{,}", "// note\n{}", r#"{"mcpServers":[]}"#, "null"] {
            assert!(
                matches!(
                    merge_mcp_servers(Some(text), &claude_entry(), ".mcp.json"),
                    Err(SetupError::Unreadable { .. })
                ),
                "{text}"
            );
        }
    }
}
