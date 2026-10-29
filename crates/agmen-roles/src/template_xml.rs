//! Write a template back to version-one XML.

use std::fmt::{self, Write};

use crate::import::escape;
use crate::{
    AgentSpec, AgentTemplate, MAX_NODE_DEPTH, MAX_NODES, MAX_OPTIONS, MAX_REFERENCES, Named, Node,
    NodeAgent, Reference, TeamTemplate, Template, TemplateOption,
};

/// Serialize a template to version-one XML.
///
/// Empty fields and empty lists are omitted. Text and attribute values are escaped so that
/// [`parse_template`](crate::parse_template) reads back an equal template.
///
/// The output is indented, so it can be larger than the file the template came from.
/// [`parse_template`](crate::parse_template) refuses more than
/// [`MAX_TEMPLATE_BYTES`](crate::MAX_TEMPLATE_BYTES), so a caller that saves or sends the
/// output checks its length first.
///
/// Every loop stops at the parser's limit, so a hand-built template beyond one is cut short
/// instead of rejected: nodes nested deeper than [`MAX_NODE_DEPTH`](crate::MAX_NODE_DEPTH)
/// levels, nodes past [`MAX_NODES`](crate::MAX_NODES) among their siblings, list items past
/// [`MAX_REFERENCES`](crate::MAX_REFERENCES) and options past
/// [`MAX_OPTIONS`](crate::MAX_OPTIONS). `parse_template` never produces such a template.
///
/// # Examples
/// ```
/// use agmen_roles::{parse_template, to_xml};
///
/// let template = parse_template(r#"<agent-template name="plain"/>"#)?;
/// assert_eq!(
///     to_xml(&template),
///     "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<agent-template name=\"plain\" version=\"1\" />\n"
/// );
/// # Ok::<(), agmen_roles::TemplateError>(())
/// ```
#[must_use]
pub fn to_xml(template: &Template) -> String {
    let mut out = String::new();
    emit(
        &mut out,
        format_args!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"),
    );
    match template {
        Template::Agent(named) => write_agent_root(&mut out, named),
        Template::Team(named) => write_team_root(&mut out, named),
    }
    out
}

fn emit(out: &mut String, args: fmt::Arguments<'_>) {
    // A fmt::Write error cannot happen on String.
    let _ignored = out.write_fmt(args);
}

fn write_agent_root(out: &mut String, named: &Named<AgentTemplate>) {
    let body = agent_has_body(&named.template);
    write_open(
        out,
        "agent-template",
        &named.name,
        named.saved_by.as_deref(),
        body,
    );
    if !body {
        return;
    }
    write_agent_body(out, &named.template, 1);
    emit(out, format_args!("</agent-template>\n"));
}

fn write_team_root(out: &mut String, named: &Named<TeamTemplate>) {
    let body = named.template.description.is_some() || !named.template.nodes.is_empty();
    write_open(
        out,
        "team-template",
        &named.name,
        named.saved_by.as_deref(),
        body,
    );
    if !body {
        return;
    }
    if let Some(description) = &named.template.description {
        write_text(out, 1, "description", description);
    }
    write_nodes(out, &named.template.nodes, 1, 1);
    emit(out, format_args!("</team-template>\n"));
}

fn write_open(out: &mut String, tag: &str, name: &str, saved_by: Option<&str>, body: bool) {
    let name = escape_attr(name);
    emit(out, format_args!("<{tag} name=\"{name}\""));
    if let Some(saved_by) = saved_by {
        let saved_by = escape_attr(saved_by);
        emit(out, format_args!(" saved-by=\"{saved_by}\""));
    }
    if body {
        emit(out, format_args!(" version=\"1\">\n"));
    } else {
        emit(out, format_args!(" version=\"1\" />\n"));
    }
}

fn agent_has_body(agent: &AgentTemplate) -> bool {
    agent.description.is_some()
        || spec_has_fields(&agent.agent)
        || agent.role.is_some()
        || !agent.skills.is_empty()
        || !agent.mcp.is_empty()
        || agent.budget_usd.is_some()
        || agent.prompt.is_some()
        || !agent.options.is_empty()
}

fn spec_has_fields(spec: &AgentSpec) -> bool {
    spec.kind.is_some() || spec.model.is_some() || spec.effort.is_some()
}

fn write_agent_body(out: &mut String, agent: &AgentTemplate, level: u32) {
    if let Some(description) = &agent.description {
        write_text(out, level, "description", description);
    }
    write_agent_spec(out, &agent.agent, level);
    if let Some(role) = &agent.role {
        indent(out, level);
        let role = escape_attr(role);
        emit(out, format_args!("<role name=\"{role}\" />\n"));
    }
    write_refs(out, level, "skills", "skill", &agent.skills);
    write_refs(out, level, "mcp", "server", &agent.mcp);
    if let Some(usd) = &agent.budget_usd {
        indent(out, level);
        let usd = escape_attr(usd);
        emit(out, format_args!("<budget usd=\"{usd}\" />\n"));
    }
    if let Some(prompt) = &agent.prompt {
        write_text(out, level, "prompt", prompt);
    }
    write_options(out, level, &agent.options);
}

fn write_agent_spec(out: &mut String, spec: &AgentSpec, level: u32) {
    if !spec_has_fields(spec) {
        return;
    }
    indent(out, level);
    emit(out, format_args!("<agent"));
    if let Some(kind) = &spec.kind {
        let kind = escape_attr(kind);
        emit(out, format_args!(" kind=\"{kind}\""));
    }
    if let Some(model) = &spec.model {
        let model = escape_attr(model);
        emit(out, format_args!(" model=\"{model}\""));
    }
    if let Some(effort) = &spec.effort {
        let effort = escape_attr(effort);
        emit(out, format_args!(" effort=\"{effort}\""));
    }
    emit(out, format_args!(" />\n"));
}

fn write_text(out: &mut String, level: u32, tag: &str, text: &str) {
    indent(out, level);
    let text = escape_text(text);
    if text.is_empty() {
        emit(out, format_args!("<{tag} />\n"));
        return;
    }
    emit(out, format_args!("<{tag}>{text}</{tag}>\n"));
}

fn write_refs(out: &mut String, level: u32, wrapper: &str, child: &str, refs: &[Reference]) {
    if refs.is_empty() {
        return;
    }
    indent(out, level);
    emit(out, format_args!("<{wrapper}>\n"));
    let child_level = level.saturating_add(1);
    for reference in refs.iter().take(MAX_REFERENCES) {
        indent(out, child_level);
        let name = escape_attr(&reference.name);
        emit(out, format_args!("<{child} name=\"{name}\""));
        if let Some(pack) = &reference.pack {
            let pack = escape_attr(&pack.to_string());
            emit(out, format_args!(" pack=\"{pack}\""));
        }
        emit(out, format_args!(" />\n"));
    }
    indent(out, level);
    emit(out, format_args!("</{wrapper}>\n"));
}

fn write_options(out: &mut String, level: u32, options: &[TemplateOption]) {
    if options.is_empty() {
        return;
    }
    indent(out, level);
    emit(out, format_args!("<options>\n"));
    let child_level = level.saturating_add(1);
    for option in options.iter().take(MAX_OPTIONS) {
        indent(out, child_level);
        let name = escape_attr(&option.name);
        let value = escape_attr(&option.value);
        emit(
            out,
            format_args!("<option name=\"{name}\" value=\"{value}\" />\n"),
        );
    }
    indent(out, level);
    emit(out, format_args!("</options>\n"));
}

fn write_nodes(out: &mut String, nodes: &[Node], level: u32, depth: u32) {
    if depth > MAX_NODE_DEPTH {
        return;
    }
    for node in nodes.iter().take(MAX_NODES) {
        write_node(out, node, level, depth);
    }
}

fn write_node(out: &mut String, node: &Node, level: u32, depth: u32) {
    indent(out, level);
    emit(out, format_args!("<node"));
    if let NodeAgent::Named(name) = &node.agent {
        let name = escape_attr(name);
        emit(out, format_args!(" template=\"{name}\""));
    }
    if node.count != 1 {
        let count = node.count;
        emit(out, format_args!(" count=\"{count}\""));
    }
    let inline = match &node.agent {
        NodeAgent::Inline(agent) => Some(agent.as_ref()),
        NodeAgent::Named(_) => None,
    };
    if inline.is_none() && node.children.is_empty() {
        emit(out, format_args!(" />\n"));
        return;
    }
    emit(out, format_args!(">\n"));
    let child_level = level.saturating_add(1);
    if let Some(agent) = inline {
        write_inline_agent(out, agent, child_level);
    }
    write_nodes(out, &node.children, child_level, depth.saturating_add(1));
    indent(out, level);
    emit(out, format_args!("</node>\n"));
}

fn write_inline_agent(out: &mut String, agent: &AgentTemplate, level: u32) {
    indent(out, level);
    if !agent_has_body(agent) {
        emit(out, format_args!("<agent-template />\n"));
        return;
    }
    emit(out, format_args!("<agent-template>\n"));
    write_agent_body(out, agent, level.saturating_add(1));
    indent(out, level);
    emit(out, format_args!("</agent-template>\n"));
}

/// Escape an attribute value written between double quotes. A parser turns a raw
/// tab, newline or carriage return into a space, so each is written as a character
/// reference.
fn escape_attr(value: &str) -> String {
    let escaped = escape(value)
        .replace('\t', "&#9;")
        .replace('\n', "&#10;")
        .replace('\r', "&#13;");
    let mut out = String::with_capacity(escaped.len());
    for c in escaped.chars() {
        push_char(&mut out, c);
    }
    out
}

/// Push `c`, or a numeric reference for DEL and the C1 controls (U+007F to U+009F).
/// XML allows them, but a terminal may act on a raw one (U+009B is the 8-bit CSI), and
/// an exported file is printed. The other C0 controls cannot be written in XML 1.0 at
/// all, and the parser refuses them, so a template it produced holds none.
fn push_char(out: &mut String, c: char) {
    if ('\u{7f}'..='\u{9f}').contains(&c) {
        emit(out, format_args!("&#{};", u32::from(c)));
    } else {
        out.push(c);
    }
}

/// Escape element text. A parser turns a raw carriage return into a newline, so it is
/// written as a character reference. Quotes and apostrophes stay raw: the roles
/// `escape` writes six bytes for each, which could push quote-heavy text past the
/// size limit the parser enforces. `>` is escaped only after `]]`, the one place XML
/// forbids it raw.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut brackets = 0u8;
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' if brackets >= 2 => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            _ => push_char(&mut out, c),
        }
        brackets = if c == ']' {
            brackets.saturating_add(1)
        } else {
            0
        };
    }
    out
}

fn indent(out: &mut String, level: u32) {
    let spaces = level
        .saturating_mul(2)
        .min(MAX_NODE_DEPTH.saturating_mul(2).saturating_add(8));
    for _ in 0..spaces {
        out.push(' ');
    }
}
