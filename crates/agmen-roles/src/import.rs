use std::collections::BTreeSet;

use crate::{RoleError, ToolClass, schema::valid_name};

const MAX_FRONTMATTER_BYTES: usize = 64 * 1024;
const MAX_FRONTMATTER_LINES: usize = 1024;

#[derive(Debug)]
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
                _ => {
                    return Err(RoleError::Schema(format!(
                        "cannot map Claude tool {tool:.64}"
                    )));
                }
            });
        }
        Ok(classes)
    }
}

#[derive(Debug, Default)]
struct Frontmatter {
    tools: Option<Tools>,
    disallowed_tools: Option<Tools>,
    model: Option<(String, usize)>,
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
    let tier = match front
        .model
        .as_ref()
        .map(|(model, line)| (model.as_str(), *line))
    {
        None | Some(("inherit" | "sonnet", _)) => "standard",
        Some(("haiku", _)) => "small",
        Some(("opus", _)) => "deep",
        Some((model, _)) if model.starts_with("claude-") && model.contains("opus") => "deep",
        Some((model, _)) if model.starts_with("claude-") && model.contains("haiku") => "small",
        Some((model, _)) if model.starts_with("claude-") && model.contains("sonnet") => "standard",
        Some((model, line)) => {
            return Err(yaml_error(
                line,
                &format!("cannot map model {model:.64} to a tier"),
            ));
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
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some((opening, rest)) = text.split_once('\n') else {
        return Ok((Frontmatter::default(), text));
    };
    if opening.trim_end() != "---" {
        return Ok((Frontmatter::default(), text));
    }
    let mut offset = 0;
    for (index, line) in rest.split_inclusive('\n').enumerate() {
        if line.trim_end() == "---" {
            let front = read_header(&rest[..offset])?;
            return Ok((front, &rest[offset + line.len()..]));
        }
        offset += line.len();
        if offset > MAX_FRONTMATTER_BYTES {
            return Err(yaml_error(index + 2, "frontmatter exceeds 64 KiB"));
        }
        if index >= MAX_FRONTMATTER_LINES {
            return Err(yaml_error(index + 2, "frontmatter exceeds 1024 lines"));
        }
    }
    Err(yaml_error(1, "unclosed YAML frontmatter"))
}

fn yaml_error(line: usize, message: &str) -> RoleError {
    RoleError::Schema(format!("subagent YAML line {line}: {message}"))
}

fn read_header(header: &str) -> Result<Frontmatter, RoleError> {
    let mut front = Frontmatter::default();
    let mut seen = BTreeSet::new();
    let mut block: Option<(&str, Option<usize>)> = None;
    let mut ignore_children = false;
    for (index, raw) in header.lines().enumerate() {
        let line = index + 2;
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if ignore_children && (text == "-" || text.starts_with("- ")) {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if let Some((key, block_indent)) = block.as_mut()
            && text.starts_with("- ")
        {
            if raw.contains('\t') || block_indent.is_some_and(|width| width != indent) {
                return Err(yaml_error(line, "inconsistent block list indentation"));
            }
            *block_indent = Some(indent);
            let value = scalar(text[2..].trim(), line)?;
            Tools::Text(value.clone())
                .classes()
                .map_err(|error| yaml_error(line, &error.to_string()))?;
            let target = if *key == "tools" {
                &mut front.tools
            } else {
                &mut front.disallowed_tools
            };
            let tools = target.get_or_insert_with(|| Tools::List(Vec::new()));
            if let Tools::List(items) = tools {
                items.push(value);
            }
            continue;
        }
        if indent > 0 {
            if ignore_children {
                continue;
            }
            return Err(yaml_error(line, "expected a top-level key or list item"));
        }
        block = None;
        let (key, value) = text
            .split_once(':')
            .ok_or_else(|| yaml_error(line, "expected key: value"))?;
        let value = value.trim();
        if key.starts_with(['\'', '"']) {
            return Err(yaml_error(line, "quoted keys are not supported"));
        }
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            return Err(yaml_error(line, "invalid key"));
        }
        // Flow, complex, anchor, alias, tag and merge syntax can name a real key
        // indirectly, so refuse it instead of ignoring the line as an unknown key.
        if key.starts_with(['{', '[', '?', '&', '*', '!', '|', '>', '%', '@', '`'])
            || key.contains(['\'', '"', '{', '}', '[', ']', '&', '*', '!'])
            || key.contains("<<")
        {
            return Err(yaml_error(line, "unsupported key syntax"));
        }
        ignore_children = !matches!(key, "tools" | "disallowedTools" | "model" | "omitClaudeMd");
        if ignore_children {
            continue;
        }
        if !seen.insert(key) {
            return Err(yaml_error(line, "duplicate key"));
        }
        match key {
            "tools" | "disallowedTools" => {
                if value.is_empty() || value.starts_with('#') {
                    block = Some((key, None));
                    continue;
                }
                let tools = tool_value(value, line)?;
                tools
                    .classes()
                    .map_err(|error| yaml_error(line, &error.to_string()))?;
                if key == "tools" {
                    front.tools = Some(tools);
                } else {
                    front.disallowed_tools = Some(tools);
                }
            }
            "model" => front.model = Some((scalar(value, line)?, line)),
            "omitClaudeMd" => {
                front.omit_claude_md = match value.split(" #").next().map(str::trim) {
                    Some("true") => true,
                    Some("false") => false,
                    _ => return Err(yaml_error(line, "expected true or false")),
                };
            }
            _ => {}
        }
    }
    Ok(front)
}

fn tool_value(value: &str, line: usize) -> Result<Tools, RoleError> {
    let Some(mut rest) = value.strip_prefix('[') else {
        return scalar(value, line).map(Tools::Text);
    };
    let mut items = Vec::new();
    // Each iteration consumes at least one character of the bounded header.
    for _ in 0..=value.len() {
        rest = rest.trim_start();
        if let Some(tail) = rest.strip_prefix(']') {
            scalar_tail(tail, line)?;
            return Ok(Tools::List(items));
        }
        let (item, tail) = scalar_prefix(rest, line, true)?;
        items.push(item);
        rest = tail.trim_start();
        if let Some(tail) = rest.strip_prefix(',') {
            rest = tail;
        } else if !rest.starts_with(']') {
            return Err(yaml_error(line, "expected comma or closing bracket"));
        }
    }
    Err(yaml_error(line, "unclosed flow list"))
}

fn scalar(value: &str, line: usize) -> Result<String, RoleError> {
    let (value, tail) = scalar_prefix(value, line, false)?;
    scalar_tail(tail, line)?;
    Ok(value)
}

fn scalar_tail(tail: &str, line: usize) -> Result<(), RoleError> {
    if tail.trim().is_empty()
        || (tail.starts_with(char::is_whitespace) && tail.trim_start().starts_with('#'))
    {
        Ok(())
    } else {
        Err(yaml_error(line, "unexpected text after scalar"))
    }
}

fn scalar_prefix(value: &str, line: usize, flow: bool) -> Result<(String, &str), RoleError> {
    let mut chars = value.chars();
    let first = chars
        .next()
        .ok_or_else(|| yaml_error(line, "expected scalar"))?;
    if first != '\'' && first != '"' {
        let end = value
            .find(|c| flow && matches!(c, ',' | ']'))
            .unwrap_or(value.len());
        let plain = &value[..end];
        if flow && plain.contains(" #") {
            return Err(yaml_error(line, "comment before closing flow list bracket"));
        }
        let plain = plain
            .split_once(" #")
            .map_or(plain, |(text, _)| text)
            .trim();
        if plain.is_empty()
            || "[]{},&*!|>%@`#".contains(first)
            || plain.contains(['[', ']', '{', '}'])
            || plain.contains(": ")
        {
            return Err(yaml_error(line, "expected a plain or quoted scalar"));
        }
        return Ok((plain.to_owned(), &value[end..]));
    }
    let mut output = String::new();
    for _ in 0..value.len() {
        let c = chars
            .next()
            .ok_or_else(|| yaml_error(line, "unclosed quoted scalar"))?;
        if c == first {
            if first == '\'' && chars.as_str().starts_with('\'') {
                chars.next();
                output.push('\'');
            } else {
                return Ok((output, chars.as_str()));
            }
        } else if c == '\\' && first == '"' {
            output.push(quoted_escape(&mut chars, line)?);
        } else {
            output.push(c);
        }
    }
    Err(yaml_error(line, "unclosed quoted scalar"))
}

fn quoted_escape(chars: &mut std::str::Chars<'_>, line: usize) -> Result<char, RoleError> {
    let escape = chars
        .next()
        .ok_or_else(|| yaml_error(line, "unfinished escape"))?;
    let c = match escape {
        '0' => '\0',
        'a' => '\x07',
        'b' => '\x08',
        't' | '\t' => '\t',
        'n' => '\n',
        'v' => '\x0b',
        'f' => '\x0c',
        'r' => '\r',
        'e' => '\x1b',
        ' ' | '"' | '/' | '\\' => escape,
        'N' => '\u{85}',
        '_' => '\u{a0}',
        'L' => '\u{2028}',
        'P' => '\u{2029}',
        'x' | 'u' | 'U' => {
            let width = match escape {
                'x' => 2,
                'u' => 4,
                _ => 8,
            };
            let mut code = 0_u32;
            for _ in 0..width {
                let digit = chars
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .ok_or_else(|| yaml_error(line, "invalid hexadecimal escape"))?;
                code = (code << 4) | digit;
            }
            return char::from_u32(code).ok_or_else(|| yaml_error(line, "invalid Unicode escape"));
        }
        _ => return Err(yaml_error(line, "unsupported escape")),
    };
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_byte_limit_is_checked_before_reading_values() {
        let header = format!("#{}\n", "x".repeat(MAX_FRONTMATTER_BYTES - 2));
        frontmatter(&format!("---\n{header}---\nBody")).unwrap();
        let error = frontmatter(&format!("---\n#{header}---\nBody")).unwrap_err();
        assert!(error.to_string().contains("line 2"));
        assert!(error.to_string().contains("64 KiB"));
    }

    #[test]
    fn quoted_scalars_decode_yaml_escapes_and_preserve_punctuation() {
        assert_eq!(
            scalar("'it''s, [quoted] # text'", 2).unwrap(),
            "it's, [quoted] # text"
        );
        assert_eq!(
            scalar(r#""\0\a\b\t\n\v\f\r\e\ \"\/\\\N\_\L\P""#, 2).unwrap(),
            "\0\x07\x08\t\n\x0b\x0c\r\x1b \"/\\\u{85}\u{a0}\u{2028}\u{2029}"
        );
    }
}
