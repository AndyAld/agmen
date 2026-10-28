use serde::Deserialize;
use std::collections::BTreeSet;

use crate::{RoleError, ToolClass, schema::valid_name};

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Tools {
    Text(String),
    List(Vec<String>),
}

impl Tools {
    fn classes(&self) -> Result<BTreeSet<ToolClass>, RoleError> {
        let texts = match self {
            Self::Text(s) => std::slice::from_ref(s),
            Self::List(v) => v.as_slice(),
        };
        let mut classes = BTreeSet::new();
        for tool in texts
            .iter()
            .flat_map(|s| s.split([',', ' ', '\n']))
            .filter(|s| !s.is_empty())
        {
            classes.insert(match tool {
                "Read" => ToolClass::Read,
                "Grep" | "Glob" => ToolClass::Search,
                "Edit" | "Write" | "NotebookEdit" => ToolClass::Edit,
                "Bash" => ToolClass::Shell,
                "WebFetch" | "WebSearch" => ToolClass::Web,
                "Agent" | "Task" => ToolClass::Subagents,
                _ => return Err(RoleError::Schema(format!("cannot map Claude tool {tool}"))),
            });
        }
        Ok(classes)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Frontmatter {
    tools: Option<Tools>,
    #[serde(rename = "disallowedTools")]
    disallowed_tools: Option<Tools>,
    model: Option<String>,
    #[serde(rename = "omitClaudeMd")]
    omit_claude_md: bool,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn classes(value: Option<&Tools>) -> Result<Option<String>, RoleError> {
    value
        .map(|tools| {
            tools.classes().map(|set| {
                set.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        })
        .transpose()
}

/// Convert YAML subagent settings and a Markdown body to version-one role XML.
///
/// Omitted tools inherit; an explicit allowlist or denylist maps to tool classes.
/// Unknown tool names are errors rather than silently broadening access.
///
/// # Errors
/// Returns schema or XML errors for invalid input or generated files over 64 KiB.
pub fn import_subagent(md: &str, name: &str) -> Result<String, RoleError> {
    if !valid_name(name) || md.len() > 64 * 1024 {
        return Err(RoleError::Schema(
            "invalid import name or file exceeds 64 KiB".into(),
        ));
    }
    let normalized = md.replace("\r\n", "\n");
    let (front, body) = frontmatter(&normalized)?;
    let tier = match front.model.as_deref() {
        None | Some("inherit") | Some("sonnet") => "standard",
        Some("haiku") => "small",
        Some("opus") => "deep",
        Some(model) if model.starts_with("claude-") && model.contains("opus") => "deep",
        Some(model) if model.starts_with("claude-") && model.contains("haiku") => "small",
        Some(model) if model.starts_with("claude-") && model.contains("sonnet") => "standard",
        Some(model) => {
            return Err(RoleError::Schema(format!(
                "cannot map model {model} to a tier"
            )));
        }
    };
    let parent = if front.omit_claude_md { "none" } else { "base" };
    let mut xml = format!(
        "<role name=\"{name}\" extends=\"{parent}\" kind=\"agent\" tier=\"{tier}\" version=\"1\">\n"
    );
    let allow = classes(front.tools.as_ref())?;
    let deny = classes(front.disallowed_tools.as_ref())?;
    if allow.is_some() || deny.is_some() {
        xml.push_str("  <tools");
        if let Some(allow) = allow {
            xml.push_str(&format!(" allow=\"{allow}\""));
        }
        if let Some(deny) = deny {
            xml.push_str(&format!(" deny=\"{deny}\""));
        }
        xml.push_str("/>\n");
    }
    xml.push_str(&format!(
        "  <purpose>{}</purpose>\n</role>\n",
        escape(body.trim())
    ));
    crate::parse(&xml)?;
    Ok(xml)
}

fn frontmatter(text: &str) -> Result<(Frontmatter, &str), RoleError> {
    let Some(rest) = text.strip_prefix("---\n") else {
        return Ok((Frontmatter::default(), text));
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches('\n') == "---" {
            let header = &rest[..offset];
            let front = if header.trim().is_empty() {
                Frontmatter::default()
            } else {
                serde_saphyr::from_str(header)
                    .map_err(|e| RoleError::Schema(format!("subagent YAML: {e}")))?
            };
            return Ok((front, &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(RoleError::Schema("unclosed YAML frontmatter".into()))
}
