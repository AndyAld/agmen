//! Merge OpenCode's plain JSON config without changing unrelated settings.

use std::path::Path;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use super::{Merged, Outcome, SetupError, write_worktree_file};
use crate::client::blocking;

pub(super) async fn run(root: &Path) -> Result<Outcome> {
    let jsonc = root.join("opencode.jsonc");
    if blocking(move || match std::fs::symlink_metadata(jsonc) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).context("checking for opencode.jsonc"),
    })
    .await?
    {
        return Ok(Outcome::Skipped(
            "opencode.jsonc exists; add the agmen entry by hand (docs/mcp-setup.md)".into(),
        ));
    }
    write_worktree_file(root, "opencode.json", merge_opencode).await
}

pub(crate) fn merge_opencode(existing: Option<&str>) -> Result<Merged, SetupError> {
    let unreadable = |why: String| SetupError::Unreadable {
        file: "opencode.json".into(),
        why,
    };
    let mut value: Value = match existing {
        Some(text) => serde_json::from_str(text).map_err(|err| unreadable(err.to_string()))?,
        None => json!({"$schema":"https://opencode.ai/config.json"}),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| unreadable("expected a JSON object".into()))?;
    let servers = object.entry("mcp").or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| unreadable("mcp must be a JSON object".into()))?;
    let entry = json!({"type":"local","command":["agmen","mcp"],"enabled":true});
    if servers.get("agmen") == Some(&entry) {
        return Ok(Merged {
            text: existing.unwrap_or_default().to_owned(),
            changed: false,
        });
    }
    servers.insert("agmen".into(), entry);
    Ok(Merged {
        text: serde_json::to_string_pretty(&value).map_err(|err| unreadable(err.to_string()))?
            + "\n",
        changed: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn opencode_entry_is_local_and_enabled() {
        let first = merge_opencode(None).unwrap();
        let entry = json!({"type":"local","command":["agmen","mcp"],"enabled":true});
        assert!(first.changed);
        assert_eq!(
            serde_json::from_str::<Value>(&first.text).unwrap(),
            json!({"$schema":"https://opencode.ai/config.json","mcp":{"agmen":entry}})
        );
        let second = merge_opencode(Some(&first.text)).unwrap();
        assert!(!second.changed);
        assert_eq!(second.text, first.text);
        let original = r#"{"theme":"mine","mcp":{"other":{"type":"remote","url":"https://example.com"},"agmen":{"command":["old"]}}}"#;
        let merged = merge_opencode(Some(original)).unwrap();
        let value: Value = serde_json::from_str(&merged.text).unwrap();
        assert_eq!(value["theme"], "mine");
        assert_eq!(
            value["mcp"]["other"],
            json!({"type":"remote","url":"https://example.com"})
        );
        assert_eq!(value["mcp"]["agmen"], entry);
    }

    #[test]
    fn opencode_refuses_unusable_json() {
        for text in ["[]", "{,}", "// comment\n{}", r#"{"mcp":[]}"#] {
            assert!(
                matches!(
                    merge_opencode(Some(text)),
                    Err(SetupError::Unreadable { .. })
                ),
                "{text}"
            );
        }
    }
}
