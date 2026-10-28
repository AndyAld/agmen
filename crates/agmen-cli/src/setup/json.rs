//! Merge the plain JSON MCP format while keeping unrelated settings.

use serde_json::{Value, json};

use super::{Merged, SetupError};

pub(crate) fn merge_mcp_servers(existing: Option<&str>) -> Result<Merged, SetupError> {
    let unreadable = |why: String| SetupError::Unreadable {
        file: ".mcp.json".into(),
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
    let entry = json!({"command":"agmen","args":["mcp"]});
    if servers.get("agmen") == Some(&entry) {
        return Ok(Merged {
            text: existing.unwrap_or_default().to_owned(),
            changed: false,
        });
    }
    servers.insert("agmen".into(), entry);
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

    #[test]
    fn adds_agmen_to_a_missing_file() {
        let merged = merge_mcp_servers(None).unwrap();
        assert!(merged.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&merged.text).unwrap(),
            json!({"mcpServers":{"agmen":{"command":"agmen","args":["mcp"]}}})
        );
    }

    #[test]
    fn keeps_other_servers_and_keys() {
        let merged =
            merge_mcp_servers(Some(r#"{"mcpServers":{"x":{"command":"x"}},"other":1}"#)).unwrap();
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
        let first = merge_mcp_servers(None).unwrap();
        let second = merge_mcp_servers(Some(&first.text)).unwrap();
        assert!(!second.changed);
        assert_eq!(first.text, second.text);
        let compact = r#"{"mcpServers":{"agmen":{"command":"agmen","args":["mcp"]}}}"#;
        assert_eq!(merge_mcp_servers(Some(compact)).unwrap().text, compact);
    }

    #[test]
    fn replaces_a_stale_agmen_entry() {
        let merged =
            merge_mcp_servers(Some(r#"{"mcpServers":{"agmen":{"command":"old"}}}"#)).unwrap();
        assert!(merged.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&merged.text).unwrap()["mcpServers"]["agmen"],
            json!({"command":"agmen","args":["mcp"]})
        );
    }

    #[test]
    fn refuses_files_that_are_not_json_objects() {
        for text in ["[]", "{,}", "// note\n{}", r#"{"mcpServers":[]}"#, "null"] {
            assert!(
                matches!(
                    merge_mcp_servers(Some(text)),
                    Err(SetupError::Unreadable { .. })
                ),
                "{text}"
            );
        }
    }
}
