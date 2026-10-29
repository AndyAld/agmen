//! Bounded template discovery and self-contained team exports.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::{
    Layer, MAX_DIRECTORY_ENTRIES, MAX_NODE_DEPTH, MAX_NODES, MAX_TEMPLATE_BYTES,
    MAX_TEMPLATE_FILES, Named, Node, NodeAgent, TeamTemplate, Template, TemplateError,
    parse_template, to_xml,
};

/// Plain agent settings embedded in every binary.
pub const BUILT_IN_TEMPLATES: [(&str, &str); 2] = [
    (
        "claude",
        include_str!("../../../templates/claude.agent.xml"),
    ),
    ("codex", include_str!("../../../templates/codex.agent.xml")),
];

/// Sources in increasing precedence.
#[derive(Debug, Clone)]
pub struct TemplateLayers {
    /// Built-in labels and XML sources.
    pub builtin: Vec<(String, String)>,
    /// Checkout root containing `.agmen/templates`.
    pub project: Option<PathBuf>,
    /// Personal template directory. Links may be followed.
    pub personal: Option<PathBuf>,
}

impl Default for TemplateLayers {
    fn default() -> Self {
        Self {
            builtin: BUILT_IN_TEMPLATES
                .iter()
                .map(|(n, x)| ((*n).into(), (*x).into()))
                .collect(),
            project: None,
            personal: None,
        }
    }
}

/// A template and the source that supplied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateEntry {
    /// Source layer.
    pub layer: Layer,
    /// Source file, absent for built-ins.
    pub path: Option<PathBuf>,
    /// Validated template.
    pub template: Template,
}

/// A source skipped during discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Source layer.
    pub layer: Layer,
    /// File or directory, absent for built-ins.
    pub path: Option<PathBuf>,
    /// Reason the source was skipped.
    pub message: String,
}

/// Effective templates in name order, with discovery problems retained.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateCatalog {
    entries: BTreeMap<String, TemplateEntry>,
    problems: Vec<Problem>,
}

impl TemplateCatalog {
    /// Iterate in name order.
    pub fn iter(&self) -> impl Iterator<Item = &TemplateEntry> {
        self.entries.values()
    }

    /// Look up an exact name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&TemplateEntry> {
        self.entries.get(name)
    }

    /// Sources skipped while loading.
    #[must_use]
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// Copy a team, replacing named agents with their settings.
    ///
    /// # Errors
    /// Returns a missing-name, wrong-kind, size, node-count or depth error.
    pub fn inline(&self, team: &TeamTemplate) -> Result<TeamTemplate, TemplateError> {
        let mut count = 0;
        let result = TeamTemplate {
            description: team.description.clone(),
            nodes: self.inline_nodes(&team.nodes, 1, &mut count)?,
        };
        let xml = to_xml(&Template::Team(Named {
            // TeamTemplate has no root name. Use the shortest valid name here;
            // export checks the final XML with its original name and saved-by.
            name: "aa".into(),
            saved_by: None,
            template: result.clone(),
        }));
        if xml.len() > MAX_TEMPLATE_BYTES {
            return Err(TemplateError::TooLarge {
                limit: MAX_TEMPLATE_BYTES,
            });
        }
        Ok(result)
    }

    fn inline_nodes(
        &self,
        nodes: &[Node],
        depth: u32,
        count: &mut usize,
    ) -> Result<Vec<Node>, TemplateError> {
        if !nodes.is_empty() && depth > MAX_NODE_DEPTH {
            return Err(TemplateError::Schema(format!(
                "node nesting exceeds {MAX_NODE_DEPTH} levels"
            )));
        }
        let mut result = Vec::new();
        for node in nodes.iter().take(MAX_NODES + 1) {
            *count += 1;
            if *count > MAX_NODES {
                return Err(TemplateError::Schema(format!(
                    "team has more than {MAX_NODES} node elements"
                )));
            }
            let agent = match &node.agent {
                NodeAgent::Inline(agent) => agent.clone(),
                NodeAgent::Named(name) => match self.get(name).map(|entry| &entry.template) {
                    Some(Template::Agent(agent)) => Box::new(agent.template.clone()),
                    Some(Template::Team(_)) => return Err(TemplateError::NotAnAgent(name.clone())),
                    None => return Err(TemplateError::Missing(name.clone())),
                },
            };
            result.push(Node {
                agent: NodeAgent::Inline(agent),
                count: node.count,
                children: self.inline_nodes(&node.children, depth + 1, count)?,
            });
        }
        Ok(result)
    }

    fn problem(&mut self, layer: Layer, path: Option<PathBuf>, message: impl ToString) {
        self.problems.push(Problem {
            layer,
            path,
            message: message.to_string(),
        });
    }

    fn insert(
        &mut self,
        layer: Layer,
        path: Option<PathBuf>,
        xml: &str,
        names: &mut BTreeSet<String>,
    ) {
        match parse_template(xml) {
            Err(error) => self.problem(layer, path, error),
            Ok(template) => {
                if !names.insert(template.name().to_owned()) {
                    self.problem(
                        layer,
                        path,
                        format!("duplicate template {} in layer", template.name()),
                    );
                    return;
                }
                self.entries.insert(
                    template.name().to_owned(),
                    TemplateEntry {
                        layer,
                        path,
                        template,
                    },
                );
            }
        }
    }
}

impl TemplateLayers {
    /// Load bounded layers; every malformed source becomes a problem.
    #[must_use]
    pub fn load(&self) -> TemplateCatalog {
        let mut catalog = TemplateCatalog::default();
        let mut names = BTreeSet::new();
        let mut builtin: Vec<_> = self.builtin.iter().take(MAX_TEMPLATE_FILES).collect();
        builtin.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, xml) in builtin {
            catalog.insert(Layer::Builtin, None, xml, &mut names);
        }
        if self.builtin.len() > MAX_TEMPLATE_FILES {
            catalog.problem(
                Layer::Builtin,
                None,
                format!(
                    "{} template files left unread",
                    self.builtin.len() - MAX_TEMPLATE_FILES
                ),
            );
        }
        if let Some(root) = &self.project {
            let dir = root.join(".agmen/templates");
            match (fs::canonicalize(root), fs::canonicalize(&dir)) {
                (Ok(root), Ok(dir)) if dir.starts_with(&root) => {
                    load_dir(&mut catalog, &dir, Layer::Project, false)
                }
                (Err(error), _) | (_, Err(error))
                    if error.kind() != std::io::ErrorKind::NotFound =>
                {
                    catalog.problem(Layer::Project, Some(dir), error)
                }
                _ => {}
            }
        }
        if let Some(dir) = &self.personal {
            load_dir(&mut catalog, dir, Layer::Personal, true);
        }
        catalog
    }
}

fn load_dir(catalog: &mut TemplateCatalog, dir: &Path, layer: Layer, follow: bool) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            catalog.problem(layer, Some(dir.into()), error);
            return;
        }
    };
    let mut paths = Vec::new();
    for (index, entry) in entries.take(MAX_DIRECTORY_ENTRIES + 1).enumerate() {
        if index == MAX_DIRECTORY_ENTRIES {
            catalog.problem(
                layer,
                Some(dir.into()),
                format!("template directory exceeds {MAX_DIRECTORY_ENTRIES} entries"),
            );
            break;
        }
        match entry {
            Ok(entry) if entry.path().extension() == Some(OsStr::new("xml")) => {
                paths.push(entry.path())
            }
            Ok(_) => {}
            Err(error) => catalog.problem(layer, Some(dir.into()), error),
        }
    }
    paths.sort();
    if paths.len() > MAX_TEMPLATE_FILES {
        catalog.problem(
            layer,
            Some(dir.into()),
            format!(
                "{} template files left unread",
                paths.len() - MAX_TEMPLATE_FILES
            ),
        );
    }
    let mut names = BTreeSet::new();
    for path in paths.into_iter().take(MAX_TEMPLATE_FILES) {
        let meta = if follow {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        match meta {
            Ok(meta) if meta.is_symlink() && !follow => continue,
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                catalog.problem(layer, Some(path), "not a regular file");
                continue;
            }
            Err(error) => {
                catalog.problem(layer, Some(path), error);
                continue;
            }
        }
        // Read bytes, so a multibyte char cut at the limit still reports the size.
        let mut bytes = Vec::new();
        match File::open(&path).and_then(|file| {
            file.take((MAX_TEMPLATE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        }) {
            Err(error) => catalog.problem(layer, Some(path), error),
            Ok(_) if bytes.len() > MAX_TEMPLATE_BYTES => catalog.problem(
                layer,
                Some(path),
                TemplateError::TooLarge {
                    limit: MAX_TEMPLATE_BYTES,
                },
            ),
            Ok(_) => match String::from_utf8(bytes) {
                Ok(xml) => catalog.insert(layer, Some(path), &xml, &mut names),
                Err(error) => catalog.problem(layer, Some(path), error),
            },
        }
    }
}
