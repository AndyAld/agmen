//! Codex's stdio server `cwd` is supported, checked 2026-10-28:
//! <https://github.com/openai/codex/blob/main/docs/config.md> links to
//! <https://developers.openai.com/codex/config-reference>, now served at
//! <https://learn.chatgpt.com/docs/config-file/config-reference>.
//! It defines `mcp_servers.<id>.cwd` as the stdio process working directory.

use std::path::{Path, PathBuf};

use agmen_client::IdentityFile;
use agmen_proto::IdentityId;
use anyhow::{Context as _, Result};
use toml_edit::{Array, Document, Item, Value};

use super::{AgentEnv, Outcome, SetupError, read_config, write_atomically};
use crate::client::blocking;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexEntry {
    pub(crate) cwd: PathBuf,
    pub(crate) identity: IdentityId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexChange {
    Added,
    Unchanged,
    Repointed { from: PathBuf },
}

pub(super) async fn run(root: &Path, identity: &IdentityFile, env: &AgentEnv) -> Result<Outcome> {
    let path = env.codex_home.join("config.toml");
    let entry = CodexEntry {
        cwd: root.to_owned(),
        identity: identity.identity_id.clone(),
    };
    blocking(move || {
        let existing = match read_config(&path) {
            Ok(existing) => existing,
            Err(err) => return Ok(Outcome::Skipped(format!("{err:#}"))),
        };
        let (text, change) = match merge_codex(existing.as_deref(), &entry) {
            Ok(merged) => merged,
            Err(SetupError::Unreadable { why, .. }) => {
                return Ok(Outcome::Skipped(
                    SetupError::Unreadable { file: path, why }.to_string(),
                ));
            }
        };
        if change == CodexChange::Unchanged {
            return Ok(Outcome::Unchanged);
        }
        std::fs::create_dir_all(
            path.parent()
                .context("the Codex config path has no parent")?,
        )
        .with_context(|| format!("creating the Codex config folder for {}", path.display()))?;
        write_atomically(&path, text.as_bytes())?;
        Ok(Outcome::Configured(match change {
            CodexChange::Repointed { from } => format!(
                "now points at {} (was {})",
                entry.cwd.display(),
                from.display()
            ),
            CodexChange::Added | CodexChange::Unchanged => format!("wrote {}", path.display()),
        }))
    })
    .await
}

#[expect(
    clippy::too_many_lines,
    reason = "the bounded TOML insertion cases share the parsed source spans"
)]
pub(crate) fn merge_codex(
    existing: Option<&str>,
    entry: &CodexEntry,
) -> Result<(String, CodexChange), SetupError> {
    let unreadable = |why: String| SetupError::Unreadable {
        file: "config.toml".into(),
        why,
    };
    let source = existing.unwrap_or_default();
    // The full parser display includes source lines from the user's config.
    let doc = Document::parse(source).map_err(|err| unreadable(err.message().to_owned()))?;
    let servers = doc.get("mcp_servers");
    let servers_table = servers
        .map(|item| {
            item.as_table_like()
                .ok_or_else(|| unreadable("mcp_servers must be a table".into()))
        })
        .transpose()?;
    let server = servers_table.and_then(|table| table.get("agmen"));
    let table = server
        .map(|item| {
            item.as_table_like()
                .ok_or_else(|| unreadable("mcp_servers.agmen must be a table".into()))
        })
        .transpose()?;
    let cwd = entry
        .cwd
        .to_str()
        .ok_or_else(|| unreadable("the worktree path is not UTF-8".into()))?;
    let identity = entry.identity.to_string();
    let fields = [
        ("command", Value::from("agmen")),
        (
            "args",
            Value::Array(
                ["mcp", "--identity", &identity]
                    .into_iter()
                    .collect::<Array>(),
            ),
        ),
        ("cwd", Value::from(cwd)),
    ];
    let mut edits = Vec::with_capacity(4);
    let mut missing = Vec::with_capacity(3);
    for (key, value) in &fields {
        if let Some(old) = table.and_then(|table| table.get(key)) {
            let same = match (old.as_value(), value) {
                (Some(Value::String(old)), Value::String(new)) => old.value() == new.value(),
                (Some(Value::Array(old)), Value::Array(new)) => old
                    .iter()
                    .map(Value::as_str)
                    .eq(new.iter().map(Value::as_str)),
                _ => false,
            };
            if !same {
                let span = old
                    .as_value()
                    .and_then(Value::span)
                    .ok_or_else(|| unreadable(format!("agmen.{key} must be a value")))?;
                let replacement = if *key == "args" {
                    let mut parsed = source[span.clone()]
                        .parse::<Value>()
                        .map_err(|err| unreadable(err.message().to_owned()))?;
                    let args = parsed.as_array_mut().ok_or_else(|| {
                        unreadable("agmen.args must be an array of strings".into())
                    })?;
                    if args.iter().any(|arg| arg.as_str().is_none()) {
                        return Err(unreadable("agmen.args must be an array of strings".into()));
                    }
                    for (index, arg) in ["mcp", "--identity", &identity].into_iter().enumerate() {
                        if index < args.len() {
                            drop(args.replace(index, arg));
                        } else {
                            args.push(arg);
                        }
                    }
                    // Keep the comments attached to removed arguments too.
                    let mut comments = String::new();
                    let mut index = 0;
                    args.retain(|arg| {
                        index += 1;
                        if index > 3 {
                            for decor in [arg.decor().prefix(), arg.decor().suffix()] {
                                comments.push_str(
                                    decor.and_then(|raw| raw.as_str()).unwrap_or_default(),
                                );
                            }
                        }
                        index <= 3
                    });
                    comments.push_str(args.trailing().as_str().unwrap_or_default());
                    args.set_trailing(comments);
                    parsed.to_string()
                } else {
                    value.to_string()
                };
                edits.push((span, replacement));
            }
        } else {
            missing.push(format!("{key} = {value}"));
        }
    }
    if edits.is_empty() && missing.is_empty() {
        return Ok((source.to_owned(), CodexChange::Unchanged));
    }
    if !missing.is_empty() {
        if server.is_none() && servers.is_none_or(Item::is_table) {
            let separator = if source.is_empty() || source.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            edits.push((
                source.len()..source.len(),
                format!("{separator}[mcp_servers.agmen]\n{}\n", missing.join("\n")),
            ));
        } else {
            // Use the closest explicit table, so dotted and inline forms
            // keep their original spelling and every unrelated byte.
            for (container, prefix) in [
                (server, ""),
                (servers, "agmen."),
                (Some(doc.as_item()), "mcp_servers.agmen."),
            ] {
                let Some(container) = container else { continue };
                if let Some(table) = container
                    .as_inline_table()
                    .filter(|table| !table.is_dotted())
                {
                    let span = table
                        .span()
                        .ok_or_else(|| unreadable("missing inline table span".into()))?;
                    let comma = if table.is_empty() || table.trailing_comma() {
                        " "
                    } else {
                        ", "
                    };
                    let values = missing
                        .iter()
                        .map(|field| format!("{prefix}{field}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let pos = span.end - 1;
                    edits.push((pos..pos, format!("{comma}{values}")));
                    break;
                }
                if let Some(table) = container
                    .as_table()
                    .filter(|table| !table.is_implicit() && !table.is_dotted())
                {
                    let end = table
                        .span()
                        .ok_or_else(|| unreadable("missing table span".into()))?
                        .end;
                    let pos = if end == 0 {
                        0
                    } else {
                        source[end..]
                            .find('\n')
                            .map_or(source.len(), |offset| end + offset + 1)
                    };
                    let separator = if pos > 0 && !source[..pos].ends_with('\n') {
                        "\n"
                    } else {
                        ""
                    };
                    let values = missing
                        .iter()
                        .map(|field| format!("{prefix}{field}\n"))
                        .collect::<String>();
                    edits.push((pos..pos, format!("{separator}{values}")));
                    break;
                }
            }
        }
    }
    let change = match table
        .and_then(|table| table.get("cwd"))
        .and_then(Item::as_str)
    {
        Some(from) if from != cwd => CodexChange::Repointed { from: from.into() },
        _ => CodexChange::Added,
    };
    // Serialize only our new values. DocumentMut would normalize line
    // endings and add a final newline to the user's other tables.
    let mut text = source.to_owned();
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    for (span, replacement) in edits {
        text.replace_range(span, &replacement);
    }
    Document::parse(text.as_str()).map_err(|err| unreadable(err.message().to_owned()))?;
    Ok((text, change))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(root: &str) -> CodexEntry {
        CodexEntry {
            cwd: root.into(),
            identity: "idn7k2q9".parse().unwrap(),
        }
    }

    #[test]
    fn adds_the_agmen_server() {
        let (text, change) = merge_codex(None, &entry("/work/repo")).unwrap();
        assert_eq!(change, CodexChange::Added);
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        let server = &doc["mcp_servers"]["agmen"];
        assert_eq!(server["command"].as_str(), Some("agmen"));
        assert_eq!(server["cwd"].as_str(), Some("/work/repo"));
        assert_eq!(
            server["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            ["mcp", "--identity", "idn7k2q9"]
        );
    }

    #[test]
    fn keeps_comments_and_other_servers() {
        let original = "# mine\n[mcp_servers.other]\ncommand = \"x\"\n";
        let (text, _) = merge_codex(Some(original), &entry("/work/repo")).unwrap();
        assert!(text.starts_with(original), "{text}");
        let original = "# preferences\nmodel = 'mine' # keep\n\n[mcp_servers.other]\ncommand='x'\n\n[mcp_servers.agmen]\ncommand = 'old' # launcher\nargs = ['old']\ncwd = '/work/a' # root\n\n[projects.'/work/a']\ntrust_level='trusted' # stay\n";
        let (text, _) = merge_codex(Some(original), &entry("/work/b")).unwrap();
        assert!(text.starts_with(original.split("[mcp_servers.agmen]").next().unwrap()));
        assert!(text.ends_with("\n[projects.'/work/a']\ntrust_level='trusted' # stay\n"));
        for comment in ["# launcher", "# root"] {
            assert!(text.contains(comment), "{text}");
        }
        let original = "# mine\r\n[mcp_servers.other]\r\ncommand='x'\r\n\n[projects.'/work/a']\r\ntrust_level='trusted'";
        let (text, _) = merge_codex(Some(original), &entry("/work/repo")).unwrap();
        assert!(text.starts_with(original), "{text:?}");
        let original = "[mcp_servers.agmen]\r\ncommand='old' # launcher\r\nargs=['old']\r\ncwd='/work/a' # root\r\n[projects.'/work/a']\r\ntrust_level='trusted'";
        let (text, _) = merge_codex(Some(original), &entry("/work/b")).unwrap();
        assert!(text.ends_with("[projects.'/work/a']\r\ntrust_level='trusted'"));
        assert!(text.contains("# launcher\r\n"));
        assert!(text.contains("# root\r\n"));
    }

    #[test]
    fn codex_repairs_partial_and_inline_entries() {
        for original in [
            "[mcp_servers.agmen] # mine",
            "[mcp_servers.agmen]\ncommand='agmen'\n",
            "mcp_servers = { other = { command = 'x' } }",
            "mcp_servers = { agmen = {} }",
            "mcp_servers = { agmen.command = 'old' }",
            "mcp_servers.agmen.command = 'old'\n",
            "[mcp_servers]\nagmen.command = 'old'\n",
            "[mcp_servers.agmen.env]\nNAME='value'\n",
        ] {
            let (text, _) = merge_codex(Some(original), &entry("/work/repo")).unwrap();
            let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
            assert_eq!(
                doc["mcp_servers"]["agmen"]["command"].as_str(),
                Some("agmen"),
                "{text}"
            );
            assert_eq!(
                doc["mcp_servers"]["agmen"]["cwd"].as_str(),
                Some("/work/repo"),
                "{text}"
            );
            let (second, change) = merge_codex(Some(&text), &entry("/work/repo")).unwrap();
            assert_eq!(change, CodexChange::Unchanged);
            assert_eq!(text, second);
        }
    }

    #[test]
    fn same_worktree_is_unchanged() {
        let (first, _) = merge_codex(None, &entry("/work/repo")).unwrap();
        let (second, change) = merge_codex(Some(&first), &entry("/work/repo")).unwrap();
        assert_eq!(change, CodexChange::Unchanged);
        assert_eq!(second, first);
    }

    #[test]
    fn another_worktree_is_repointed_and_reported() {
        let (first, _) = merge_codex(None, &entry("/work/a")).unwrap();
        let (text, change) = merge_codex(Some(&first), &entry("/work/b")).unwrap();
        assert_eq!(
            change,
            CodexChange::Repointed {
                from: "/work/a".into()
            }
        );
        assert_eq!(
            text.parse::<toml_edit::DocumentMut>().unwrap()["mcp_servers"]["agmen"]["cwd"].as_str(),
            Some("/work/b")
        );
    }

    #[test]
    fn refuses_unusable_toml() {
        for text in ["[[", "mcp_servers = 1", "[mcp_servers]\nagmen = 1"] {
            assert!(
                matches!(
                    merge_codex(Some(text), &entry("/work/repo")),
                    Err(SetupError::Unreadable { .. })
                ),
                "{text}"
            );
        }
    }

    #[test]
    fn codex_refusal_does_not_quote_config_contents() {
        let text = "private_config_value = [bare_word]";
        let error = merge_codex(Some(text), &entry("/work/repo")).unwrap_err();
        assert!(!error.to_string().contains("private_config_value"));
    }

    #[test]
    fn codex_keeps_comments_inside_replaced_args() {
        let original = "[mcp_servers.agmen]\ncommand='agmen'\nargs = [\n  'mcp', # keep this comment\n  'old', # keep this too\n  'old-id',\n  'obsolete', # keep the last comment\n]\ncwd='/work/a'\n";
        let (text, _) = merge_codex(Some(original), &entry("/work/b")).unwrap();
        for comment in [
            "# keep this comment",
            "# keep this too",
            "# keep the last comment",
        ] {
            assert!(text.contains(comment), "{text}");
        }
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        let args = doc["mcp_servers"]["agmen"]["args"].as_array().unwrap();
        assert_eq!(
            args.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>(),
            ["mcp", "--identity", "idn7k2q9"]
        );
    }
}
