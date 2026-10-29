//! Agent and team template schema, limits, and parser.

use std::fmt;
use std::path::PathBuf;

use crate::schema::valid_name;

/// Largest template file, in bytes. Matches the chat body limit.
pub const MAX_TEMPLATE_BYTES: usize = 32 * 1024;
/// Most agents a team may start, multiplying each node's count along its path.
pub const MAX_TEAM_AGENTS: u32 = 64;
/// Deepest `<node>` nesting a team file may contain.
pub const MAX_NODE_DEPTH: u32 = 32;
/// Most `<node>` elements in one file.
pub const MAX_NODES: usize = 64;
/// Most skills in `<skills>`, and most servers in `<mcp>`.
pub const MAX_REFERENCES: usize = 64;
/// Most `<option>` elements on one agent.
pub const MAX_OPTIONS: usize = 32;
/// Most template files read from one layer directory.
pub const MAX_TEMPLATE_FILES: usize = 64;
/// Most directory entries scanned in one layer.
pub const MAX_DIRECTORY_ENTRIES: usize = 4096;

const MAX_DESCRIPTION_BYTES: usize = 1024;
const MAX_PROMPT_BYTES: usize = 16 * 1024;
const MAX_SAVED_BY_BYTES: usize = 64;
const MAX_MODEL_BYTES: usize = 128;
const MAX_KIND_BYTES: usize = 32;
const MAX_EFFORT_BYTES: usize = 16;
const MAX_REF_NAME_BYTES: usize = 64;
const MAX_OPTION_NAME_BYTES: usize = 64;
const MAX_OPTION_VALUE_BYTES: usize = 256;
const MAX_PACK_SOURCE_BYTES: usize = 256;
const MIN_PACK_SHA_BYTES: usize = 7;
const MAX_PACK_SHA_BYTES: usize = 40;
const MAX_USD_WHOLE: u32 = 1000;

/// Failure to parse, read, or resolve a template.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    /// Malformed XML.
    #[error("not XML: {0}")]
    Xml(#[from] roxmltree::Error),
    /// Wrong root tag.
    #[error("the root element is not <agent-template> or <team-template>")]
    NotATemplate,
    /// Missing template name.
    #[error("the template has no name")]
    NoName,
    /// Input larger than [`MAX_TEMPLATE_BYTES`].
    #[error("the template exceeds {limit} bytes")]
    TooLarge {
        /// Maximum size in bytes.
        limit: usize,
    },
    /// Unsupported schema content.
    #[error("template schema: {0}")]
    Schema(String),
    /// Missing template.
    #[error("no template is named \"{0}\"")]
    Missing(String),
    /// A team template was used where an agent template is required.
    #[error("\"{0}\" is a team template, not an agent template")]
    NotAnAgent(String),
    /// File operation failed.
    #[error("reading {path}: {source}")]
    Io {
        /// Failed path.
        path: PathBuf,
        /// Underlying disk error.
        #[source]
        source: std::io::Error,
    },
}

/// Whether a template describes one agent or a team.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemplateKind {
    /// One agent.
    Agent,
    /// A tree of agents.
    Team,
}

impl TemplateKind {
    /// File name suffix for this kind.
    #[must_use]
    pub const fn file_suffix(self) -> &'static str {
        match self {
            Self::Agent => ".agent.xml",
            Self::Team => ".team.xml",
        }
    }
}

impl fmt::Display for TemplateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Agent => "agent",
            Self::Team => "team",
        })
    }
}

/// A top-level template. The name lives here, so an inline agent stays nameless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named<T> {
    /// Template name.
    pub name: String,
    /// Identity that saved this template, when an agent wrote it.
    pub saved_by: Option<String>,
    /// Template body.
    pub template: T,
}

/// An agent or team template file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Template {
    /// One agent.
    Agent(Named<AgentTemplate>),
    /// A team of agents.
    Team(Named<TeamTemplate>),
}

impl Template {
    /// Root `name`.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Agent(named) => named.name.as_str(),
            Self::Team(named) => named.name.as_str(),
        }
    }

    /// `agent` or `team`.
    #[must_use]
    pub fn kind(&self) -> TemplateKind {
        match self {
            Self::Agent(_) => TemplateKind::Agent,
            Self::Team(_) => TemplateKind::Team,
        }
    }

    /// Root `saved-by`, when set.
    #[must_use]
    pub fn saved_by(&self) -> Option<&str> {
        match self {
            Self::Agent(named) => named.saved_by.as_deref(),
            Self::Team(named) => named.saved_by.as_deref(),
        }
    }

    /// Replace `saved-by`.
    pub fn set_saved_by(&mut self, saved_by: Option<String>) {
        match self {
            Self::Agent(named) => named.saved_by = saved_by,
            Self::Team(named) => named.saved_by = saved_by,
        }
    }

    /// Option names in document order, including options on inline team nodes.
    #[must_use]
    pub fn option_names(&self) -> Vec<&str> {
        let mut names = Vec::new();
        match self {
            Self::Agent(named) => push_option_names(&named.template, &mut names),
            Self::Team(named) => push_node_option_names(&named.template.nodes, &mut names, 1),
        }
        names
    }

    /// Remove every option and return how many were removed.
    pub fn strip_options(&mut self) -> usize {
        match self {
            Self::Agent(named) => {
                let count = named.template.options.len();
                named.template.options.clear();
                count
            }
            Self::Team(named) => strip_node_options(&mut named.template.nodes, 1),
        }
    }
}

/// Launch settings for one agent. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTemplate {
    /// Human-readable summary.
    pub description: Option<String>,
    /// Agent kind, model, and effort.
    pub agent: AgentSpec,
    /// Role name this agent runs as.
    pub role: Option<String>,
    /// Skills the agent should have.
    pub skills: Vec<Reference>,
    /// MCP servers the agent should have.
    pub mcp: Vec<Reference>,
    /// Budget in USD, kept as written when it is in range.
    pub budget_usd: Option<String>,
    /// Starting prompt.
    pub prompt: Option<String>,
    /// Extra launch options.
    pub options: Vec<TemplateOption>,
}

/// The `<agent>` element.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSpec {
    /// Agent kind, such as `claude` or `codex`.
    pub kind: Option<String>,
    /// Model name passed to the agent.
    pub model: Option<String>,
    /// Effort setting, lowercase letters.
    pub effort: Option<String>,
}

/// A skill or MCP server reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// Local name.
    pub name: String,
    /// Pack that provides it, when it is not installed locally.
    pub pack: Option<PackRef>,
}

/// A pack pinned at a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRef {
    /// Pack source, without the commit.
    pub source: String,
    /// Commit prefix, lowercase hex.
    pub sha: String,
}

impl fmt::Display for PackRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.source, self.sha)
    }
}

/// One `<option>` name and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateOption {
    /// Option name.
    pub name: String,
    /// Option value.
    pub value: String,
}

/// A team of agent nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamTemplate {
    /// Human-readable summary.
    pub description: Option<String>,
    /// Top-level nodes.
    pub nodes: Vec<Node>,
}

/// One node in a team tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Agent this node starts.
    pub agent: NodeAgent,
    /// How many of this node start with the team. Zero is not started.
    pub count: u32,
    /// Child nodes.
    pub children: Vec<Node>,
}

/// The agent a team node refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeAgent {
    /// Name of another agent template.
    Named(String),
    /// Agent settings written inside the node.
    Inline(Box<AgentTemplate>),
}

/// Parse one version-one agent or team template.
///
/// # Errors
/// Returns [`TemplateError`] when the input is larger than [`MAX_TEMPLATE_BYTES`],
/// not well-formed XML, or outside the template schema.
///
/// # Examples
/// ```
/// use agmen_roles::parse_template;
///
/// let template = parse_template(r#"<agent-template name="plain"/>"#)?;
/// assert_eq!(template.name(), "plain");
/// # Ok::<(), agmen_roles::TemplateError>(())
/// ```
pub fn parse_template(xml: &str) -> Result<Template, TemplateError> {
    if xml.len() > MAX_TEMPLATE_BYTES {
        return Err(TemplateError::TooLarge {
            limit: MAX_TEMPLATE_BYTES,
        });
    }
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    if root.tag_name().namespace().is_some() {
        return Err(TemplateError::Schema("namespaced template".into()));
    }
    match root.tag_name().name() {
        "agent-template" => parse_agent_root(root).map(Template::Agent),
        "team-template" => Err(TemplateError::Schema(
            "team templates are parsed in Task 2".into(),
        )),
        _ => Err(TemplateError::NotATemplate),
    }
}

fn parse_agent_root(root: roxmltree::Node<'_, '_>) -> Result<Named<AgentTemplate>, TemplateError> {
    attributes(root, &["name", "version", "saved-by"])?;
    let name = required_name(root)?;
    check_version(root)?;
    Ok(Named {
        name,
        saved_by: saved_by(root)?,
        template: parse_agent_body(root)?,
    })
}

fn parse_agent_body(root: roxmltree::Node<'_, '_>) -> Result<AgentTemplate, TemplateError> {
    let mut template = AgentTemplate::default();
    let mut seen = [false; 8];
    let mut count = 0usize;
    for node in root.children().filter(roxmltree::Node::is_element) {
        count = count.saturating_add(1);
        if count > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::Schema("too many child elements".into()));
        }
        take_agent_child(&mut template, &mut seen, node)?;
    }
    Ok(template)
}

fn take_agent_child(
    template: &mut AgentTemplate,
    seen: &mut [bool; 8],
    node: roxmltree::Node<'_, '_>,
) -> Result<(), TemplateError> {
    let tag = node.tag_name().name();
    if node.tag_name().namespace().is_some() {
        return Err(TemplateError::Schema(format!("namespaced element {tag}")));
    }
    let slot = match tag {
        "description" => 0,
        "agent" => 1,
        "role" => 2,
        "skills" => 3,
        "mcp" => 4,
        "budget" => 5,
        "prompt" => 6,
        "options" => 7,
        _ => return Err(TemplateError::Schema(format!("unknown element {tag}"))),
    };
    let Some(flag) = seen.get_mut(slot) else {
        return Err(TemplateError::Schema(format!("unknown element {tag}")));
    };
    if *flag {
        return Err(TemplateError::Schema(format!("duplicate {tag}")));
    }
    *flag = true;
    match slot {
        0 => template.description = Some(text_field(node, MAX_DESCRIPTION_BYTES)?),
        1 => template.agent = agent_spec(node)?,
        2 => template.role = Some(role_name(node)?),
        3 => template.skills = references(node, "skill")?,
        4 => template.mcp = references(node, "server")?,
        5 => template.budget_usd = Some(budget(node)?),
        6 => template.prompt = Some(text_field(node, MAX_PROMPT_BYTES)?),
        _ => template.options = option_list(node)?,
    }
    Ok(())
}

fn required_name(node: roxmltree::Node<'_, '_>) -> Result<String, TemplateError> {
    let Some(name) = node.attribute("name") else {
        return Err(TemplateError::NoName);
    };
    if !valid_name(name) {
        return Err(invalid("name", name));
    }
    Ok(name.to_owned())
}

fn check_version(node: roxmltree::Node<'_, '_>) -> Result<(), TemplateError> {
    match node.attribute("version") {
        None | Some("1") => Ok(()),
        Some(value) => Err(invalid("version", value)),
    }
}

fn saved_by(node: roxmltree::Node<'_, '_>) -> Result<Option<String>, TemplateError> {
    let Some(value) = node.attribute("saved-by") else {
        return Ok(None);
    };
    if !(1..=MAX_SAVED_BY_BYTES).contains(&value.len()) || has_control(value) {
        return Err(invalid("saved-by", value));
    }
    Ok(Some(value.to_owned()))
}

fn text_field(node: roxmltree::Node<'_, '_>, limit: usize) -> Result<String, TemplateError> {
    attributes(node, &[])?;
    let mut combined = String::new();
    let mut seen = 0usize;
    for child in node.children() {
        seen = seen.saturating_add(1);
        if seen > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::Schema("too many text nodes".into()));
        }
        if child.is_element() {
            return Err(TemplateError::Schema(format!(
                "nested markup in {}",
                node.tag_name().name()
            )));
        }
        if !child.is_text() {
            continue;
        }
        let Some(text) = child.text() else {
            continue;
        };
        if combined.len().saturating_add(text.len()) > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::TooLarge {
                limit: MAX_TEMPLATE_BYTES,
            });
        }
        combined.push_str(text);
    }
    let trimmed = combined.trim();
    if trimmed.len() > limit {
        return Err(invalid(node.tag_name().name(), trimmed));
    }
    Ok(trimmed.to_owned())
}

fn agent_spec(node: roxmltree::Node<'_, '_>) -> Result<AgentSpec, TemplateError> {
    attributes(node, &["kind", "model", "effort"])?;
    no_elements(node)?;
    Ok(AgentSpec {
        kind: optional(node, "kind", |value| valid_slug(value, MAX_KIND_BYTES))?,
        model: optional(node, "model", valid_model)?,
        effort: optional(node, "effort", valid_effort)?,
    })
}

fn role_name(node: roxmltree::Node<'_, '_>) -> Result<String, TemplateError> {
    attributes(node, &["name"])?;
    no_elements(node)?;
    match node.attribute("name") {
        Some(name) if valid_name(name) => Ok(name.to_owned()),
        Some(name) => Err(invalid("role", name)),
        None => Err(TemplateError::Schema("role has no name".into())),
    }
}

fn budget(node: roxmltree::Node<'_, '_>) -> Result<String, TemplateError> {
    attributes(node, &["usd"])?;
    no_elements(node)?;
    match node.attribute("usd") {
        Some(value) if valid_usd(value) => Ok(value.to_owned()),
        Some(value) => Err(invalid("usd", value)),
        None => Err(TemplateError::Schema("budget has no usd".into())),
    }
}

fn references(
    node: roxmltree::Node<'_, '_>,
    child_tag: &str,
) -> Result<Vec<Reference>, TemplateError> {
    attributes(node, &[])?;
    let mut refs = Vec::<Reference>::new();
    let mut seen = 0usize;
    for child in node.children().filter(roxmltree::Node::is_element) {
        seen = seen.saturating_add(1);
        if seen > MAX_REFERENCES {
            return Err(TemplateError::Schema(format!(
                "{child_tag} list exceeds {MAX_REFERENCES}"
            )));
        }
        if child.tag_name().namespace().is_some() || !child.has_tag_name(child_tag) {
            return Err(TemplateError::Schema(format!(
                "unknown element {} in {}",
                child.tag_name().name(),
                node.tag_name().name()
            )));
        }
        attributes(child, &["name", "pack"])?;
        no_elements(child)?;
        let Some(name) = child.attribute("name") else {
            return Err(TemplateError::Schema(format!("{child_tag} has no name")));
        };
        if !valid_ref_name(name) {
            return Err(invalid(child_tag, name));
        }
        if refs
            .iter()
            .take(MAX_REFERENCES)
            .any(|item| item.name == name)
        {
            return Err(TemplateError::Schema(format!(
                "duplicate {child_tag} \"{}\"",
                cut64(name)
            )));
        }
        let pack = match child.attribute("pack") {
            None => None,
            Some(value) => Some(parse_pack(value)?),
        };
        refs.push(Reference {
            name: name.to_owned(),
            pack,
        });
    }
    Ok(refs)
}

fn option_list(node: roxmltree::Node<'_, '_>) -> Result<Vec<TemplateOption>, TemplateError> {
    attributes(node, &[])?;
    let mut options = Vec::<TemplateOption>::new();
    let mut seen = 0usize;
    for child in node.children().filter(roxmltree::Node::is_element) {
        seen = seen.saturating_add(1);
        if seen > MAX_OPTIONS {
            return Err(TemplateError::Schema(format!(
                "option list exceeds {MAX_OPTIONS}"
            )));
        }
        if child.tag_name().namespace().is_some() || !child.has_tag_name("option") {
            return Err(TemplateError::Schema(format!(
                "unknown element {} in options",
                child.tag_name().name()
            )));
        }
        attributes(child, &["name", "value"])?;
        no_elements(child)?;
        let Some(name) = child.attribute("name") else {
            return Err(TemplateError::Schema("option has no name".into()));
        };
        let Some(value) = child.attribute("value") else {
            return Err(TemplateError::Schema("option has no value".into()));
        };
        if !valid_slug(name, MAX_OPTION_NAME_BYTES) {
            return Err(invalid("option", name));
        }
        if value.len() > MAX_OPTION_VALUE_BYTES || has_control(value) {
            return Err(invalid("option", value));
        }
        if options
            .iter()
            .take(MAX_OPTIONS)
            .any(|option| option.name == name)
        {
            return Err(TemplateError::Schema(format!(
                "duplicate option \"{}\"",
                cut64(name)
            )));
        }
        options.push(TemplateOption {
            name: name.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(options)
}

fn parse_pack(value: &str) -> Result<PackRef, TemplateError> {
    let Some((source, sha)) = value.rsplit_once('@') else {
        return Err(invalid("pack", value));
    };
    let source_ok = (1..=MAX_PACK_SOURCE_BYTES).contains(&source.len())
        && !source
            .chars()
            .take(MAX_PACK_SOURCE_BYTES.saturating_add(1))
            .any(|c| c.is_whitespace() || c.is_control());
    let sha_ok = (MIN_PACK_SHA_BYTES..=MAX_PACK_SHA_BYTES).contains(&sha.len())
        && sha
            .bytes()
            .take(MAX_PACK_SHA_BYTES.saturating_add(1))
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    if !source_ok || !sha_ok {
        return Err(invalid("pack", value));
    }
    Ok(PackRef {
        source: source.to_owned(),
        sha: sha.to_owned(),
    })
}

fn attributes(node: roxmltree::Node<'_, '_>, allowed: &[&str]) -> Result<(), TemplateError> {
    let mut seen = 0usize;
    for attribute in node.attributes() {
        seen = seen.saturating_add(1);
        if seen > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::Schema(format!(
                "too many attributes on {}",
                node.tag_name().name()
            )));
        }
        if attribute.namespace().is_some() || !allowed.contains(&attribute.name()) {
            return Err(TemplateError::Schema(format!(
                "unknown attribute {} on {}",
                attribute.name(),
                node.tag_name().name()
            )));
        }
    }
    Ok(())
}

fn no_elements(node: roxmltree::Node<'_, '_>) -> Result<(), TemplateError> {
    let mut seen = 0usize;
    for child in node.children() {
        seen = seen.saturating_add(1);
        if seen > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::Schema("too many child nodes".into()));
        }
        if child.is_element() {
            return Err(TemplateError::Schema(format!(
                "unknown element {} in {}",
                child.tag_name().name(),
                node.tag_name().name()
            )));
        }
    }
    Ok(())
}

fn optional(
    node: roxmltree::Node<'_, '_>,
    field: &'static str,
    valid: impl Fn(&str) -> bool,
) -> Result<Option<String>, TemplateError> {
    match node.attribute(field) {
        None => Ok(None),
        Some(value) if valid(value) => Ok(Some(value.to_owned())),
        Some(value) => Err(invalid(field, value)),
    }
}

fn push_option_names<'a>(agent: &'a AgentTemplate, names: &mut Vec<&'a str>) {
    for option in agent.options.iter().take(MAX_OPTIONS) {
        names.push(option.name.as_str());
    }
}

fn push_node_option_names<'a>(nodes: &'a [Node], names: &mut Vec<&'a str>, depth: u32) {
    if depth > MAX_NODE_DEPTH {
        return;
    }
    for node in nodes.iter().take(MAX_NODES) {
        if let NodeAgent::Inline(agent) = &node.agent {
            push_option_names(agent, names);
        }
        push_node_option_names(&node.children, names, depth.saturating_add(1));
    }
}

fn strip_node_options(nodes: &mut [Node], depth: u32) -> usize {
    if depth > MAX_NODE_DEPTH {
        return 0;
    }
    let mut count = 0usize;
    for node in nodes.iter_mut().take(MAX_NODES) {
        if let NodeAgent::Inline(agent) = &mut node.agent {
            count = count.saturating_add(agent.options.len());
            agent.options.clear();
        }
        count = count.saturating_add(strip_node_options(
            &mut node.children,
            depth.saturating_add(1),
        ));
    }
    count
}

fn invalid(field: &str, value: &str) -> TemplateError {
    TemplateError::Schema(format!("invalid {field} \"{}\"", cut64(value)))
}

fn cut64(value: &str) -> &str {
    if value.len() <= 64 {
        return value;
    }
    let mut end = 64;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn has_control(value: &str) -> bool {
    value
        .chars()
        .take(MAX_OPTION_VALUE_BYTES.saturating_add(1))
        .any(char::is_control)
}

fn valid_slug(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len())
        && value.bytes().next().is_some_and(|c| c.is_ascii_lowercase())
        && value
            .bytes()
            .take(max.saturating_add(1))
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn valid_model(model: &str) -> bool {
    (1..=MAX_MODEL_BYTES).contains(&model.len())
        && !model.starts_with('-')
        && model
            .bytes()
            .take(MAX_MODEL_BYTES.saturating_add(1))
            .all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, b'.' | b'-' | b'_' | b':' | b'[' | b']' | b'/' | b'@')
            })
}

fn valid_effort(value: &str) -> bool {
    (1..=MAX_EFFORT_BYTES).contains(&value.len())
        && value
            .bytes()
            .take(MAX_EFFORT_BYTES.saturating_add(1))
            .all(|c| c.is_ascii_lowercase())
}

fn valid_ref_name(value: &str) -> bool {
    (1..=MAX_REF_NAME_BYTES).contains(&value.len())
        && !value.starts_with(['.', '-'])
        && value
            .bytes()
            .take(MAX_REF_NAME_BYTES.saturating_add(1))
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

fn valid_usd(value: &str) -> bool {
    if value.len() > 11 {
        return false;
    }
    let (whole, frac) = if let Some((whole, frac)) = value.split_once('.') {
        if !(1..=6).contains(&frac.len()) || !ascii_digits(frac) {
            return false;
        }
        (whole, frac)
    } else {
        (value, "")
    };
    if !(1..=4).contains(&whole.len()) || !ascii_digits(whole) {
        return false;
    }
    let whole_n = decimal(whole);
    if whole_n > MAX_USD_WHOLE {
        return false;
    }
    let frac_n = scale_frac(frac);
    if whole_n == MAX_USD_WHOLE && frac_n > 0 {
        return false;
    }
    whole_n.saturating_mul(1_000_000).saturating_add(frac_n) > 0
}

fn ascii_digits(value: &str) -> bool {
    value.bytes().take(7).all(|c| c.is_ascii_digit())
}

fn decimal(value: &str) -> u32 {
    let mut number = 0u32;
    for byte in value.bytes().take(6) {
        let Some(digit) = byte.checked_sub(b'0') else {
            return 0;
        };
        if digit > 9 {
            return 0;
        }
        number = number.saturating_mul(10).saturating_add(u32::from(digit));
    }
    number
}

fn scale_frac(frac: &str) -> u32 {
    let mut number = decimal(frac);
    let mut width = frac.len();
    while width < 6 {
        number = number.saturating_mul(10);
        width = width.saturating_add(1);
    }
    number
}
