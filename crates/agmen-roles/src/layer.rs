use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{
    BUILT_IN, EditScope, Kind, Role, RoleError, Tier, ToolClass, Worktree, ceiling, parse,
};

const MAX_DEPTH: usize = 8;
const MAX_ROLE_FILES: usize = 64;
const MAX_ROLE_BYTES: u64 = 64 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 4096;

/// The source layer that supplied a role's own definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Layer {
    /// Embedded role shipped with agmen.
    Builtin,
    /// Role in the project's `.agmen/roles` directory.
    Project,
    /// Role in the person's `$HOME/.config/agmen/roles` directory.
    Personal,
}

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Builtin => "builtin",
            Self::Project => "project",
            Self::Personal => "personal",
        })
    }
}

/// Sources in increasing precedence: embedded, project checkout, personal directory.
#[derive(Debug, Clone)]
pub struct Layers {
    /// Built-in source files, base first.
    pub builtin: Vec<(String, String)>,
    /// Trusted checkout root. Reads `.agmen/roles` inside this root only.
    pub project: Option<PathBuf>,
    /// Personal roles directory. Personal file links may be followed.
    pub personal: Option<PathBuf>,
}

impl Default for Layers {
    fn default() -> Self {
        Self {
            builtin: BUILT_IN
                .iter()
                .map(|(n, x)| ((*n).into(), (*x).into()))
                .collect(),
            project: None,
            personal: None,
        }
    }
}

/// A fully inherited role and its launch permissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The layer supplying this role's own definition, independent of its ancestors.
    pub layer: Layer,
    /// Role with inherited settings and prompt filled in.
    pub role: Role,
    /// Effective kind.
    pub kind: Kind,
    /// Base verbs plus the permitted role verbs.
    pub verbs: BTreeSet<String>,
    /// Effective tools after denies and ceilings.
    pub tools: BTreeSet<ToolClass>,
    /// Effective edit scope.
    pub edits: EditScope,
    /// Effective worktree policy.
    pub worktree: Worktree,
    /// Effective model tier.
    pub tier: Tier,
    /// Lowercase SHA-256 of source XML concatenated ancestor first.
    pub sha256: String,
    /// Permissions removed by kind ceilings, in deterministic order.
    pub stripped: Vec<String>,
}

/// Effective roles by name, including the base template.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    roles: BTreeMap<String, Resolved>,
}

impl Catalog {
    /// Iterate over resolved roles in name order, including base.
    pub fn iter(&self) -> impl Iterator<Item = &Resolved> {
        self.roles.values()
    }
    /// Find a role by its exact name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Resolved> {
        self.roles.get(name)
    }
    /// Number of roles, including base.
    #[must_use]
    pub fn len(&self) -> usize {
        self.roles.len()
    }
    /// Whether the catalog has no roles.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }
}

impl Layers {
    /// Read bounded, sorted local layers and resolve every role.
    ///
    /// Missing directories are empty. Project directory links that escape the
    /// trusted checkout and project file links are skipped, matching agmend.
    ///
    /// # Errors
    /// Returns schema, inheritance, duplicate-name, or directory I/O errors.
    pub fn load(&self) -> Result<Catalog, RoleError> {
        let mut sources = BTreeMap::new();
        let mut layers = BTreeMap::new();
        insert_layer(
            &mut sources,
            self.builtin.iter().map(|(_, xml)| xml.clone()),
            &mut layers,
            Layer::Builtin,
        )?;
        if let Some(root) = &self.project {
            let dir = root.join(".agmen/roles");
            if let (Ok(canonical_dir), Ok(canonical_root)) =
                (fs::canonicalize(&dir), fs::canonicalize(root))
                && canonical_dir.starts_with(canonical_root)
            {
                insert_layer(
                    &mut sources,
                    read_dir(&canonical_dir, false)?,
                    &mut layers,
                    Layer::Project,
                )?;
            }
        }
        if let Some(dir) = &self.personal {
            insert_layer(
                &mut sources,
                read_dir(dir, true)?,
                &mut layers,
                Layer::Personal,
            )?;
        }
        let mut roles = BTreeMap::new();
        for (name, layer) in layers {
            let mut role = resolve(&name, &sources)?;
            role.layer = layer;
            roles.insert(name, role);
        }
        Ok(Catalog { roles })
    }
}

pub(crate) type Sources = BTreeMap<String, (Role, String)>;

fn insert_layer(
    sources: &mut Sources,
    texts: impl IntoIterator<Item = String>,
    layers: &mut BTreeMap<String, Layer>,
    layer: Layer,
) -> Result<(), RoleError> {
    let mut names = BTreeSet::new();
    for text in texts {
        let role = parse(&text)?;
        if !names.insert(role.name.clone()) {
            return Err(RoleError::Schema(format!(
                "duplicate role {} in layer",
                role.name
            )));
        }
        layers.insert(role.name.clone(), layer);
        sources.insert(role.name.clone(), (role, text));
    }
    Ok(())
}

fn io(path: &Path, source: std::io::Error) -> RoleError {
    RoleError::Io {
        path: path.to_owned(),
        source,
    }
}

fn read_dir(dir: &Path, follow: bool) -> Result<Vec<String>, RoleError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(dir, e)),
    };
    let mut paths = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_DIRECTORY_ENTRIES {
            return Err(RoleError::Schema(
                "role directory exceeds 4096 entries".into(),
            ));
        }
        let path = entry.map_err(|e| io(dir, e))?.path();
        if path.extension() == Some(OsStr::new("xml")) {
            paths.push(path);
        }
    }
    paths.sort();
    let mut texts = Vec::new();
    for path in paths.into_iter().take(MAX_ROLE_FILES) {
        let meta = if follow {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        if !meta.is_ok_and(|m| m.is_file() && m.len() <= MAX_ROLE_BYTES) {
            continue;
        }
        let mut text = String::new();
        // Invalid UTF-8, unreadable and oversized files are skipped, as in agmend.
        let read =
            File::open(&path).and_then(|f| f.take(MAX_ROLE_BYTES + 1).read_to_string(&mut text));
        if read.is_ok() && text.len() as u64 <= MAX_ROLE_BYTES {
            texts.push(text);
        }
    }
    Ok(texts)
}

pub(crate) fn edits_narrow(child: &EditScope, parent: &EditScope) -> bool {
    match (child, parent) {
        (EditScope::None, _) | (EditScope::Lanes | EditScope::LanesWithin(_), EditScope::Lanes) => {
            true
        }
        (EditScope::Paths(child), EditScope::Paths(parent))
        | (EditScope::LanesWithin(child), EditScope::LanesWithin(parent)) => {
            child.iter().all(|c| {
                parent.iter().any(|p| {
                    c == p
                        || p.strip_suffix("/**").is_some_and(|prefix| {
                            c.starts_with(&format!("{prefix}/"))
                                && !c.split('/').any(|part| part == "..")
                        })
                })
            })
        }
        _ => false,
    }
}

pub(crate) fn resolve(name: &str, sources: &Sources) -> Result<Resolved, RoleError> {
    let chain = inheritance_chain(name, sources)?;
    let kind = chain.iter().find_map(|(r, _)| r.kind);
    let tier = chain.iter().find_map(|(r, _)| r.tier);
    if name != "base" && (kind.is_none() || tier.is_none()) {
        return Err(RoleError::NoKind(name.into()));
    }
    let kind = kind.unwrap_or(Kind::Agent);
    let tier = tier.unwrap_or(Tier::Standard);
    let mut tools = BTreeSet::new();
    let mut verbs: BTreeSet<String> = BTreeSet::new();
    let mut edits = EditScope::None;
    let mut worktree = Worktree::default();
    let mut prompt = Vec::new();
    let mut stripped = Vec::new();
    let mut hash = Sha256::new();
    let mut previous: Option<&Role> = None;
    let mut inherited_kind = None;
    let mut base_verbs = BTreeSet::new();
    for (role, source) in chain.iter().rev().copied() {
        let next_tools = role
            .tools
            .allow
            .clone()
            .unwrap_or_else(|| tools.clone())
            .difference(&role.tools.deny)
            .copied()
            .collect::<BTreeSet<_>>();
        let next_verbs = match (role.verbs_declared, role.name.as_str()) {
            (true, "base") => role.verbs.clone(),
            (true, _) => role.verbs.union(&base_verbs).cloned().collect(),
            (false, _) => verbs.clone(),
        };
        let next_edits = match (&role.edits, &edits) {
            (Some(EditScope::Paths(globs)), EditScope::Lanes | EditScope::LanesWithin(_)) => {
                EditScope::LanesWithin(globs.clone())
            }
            _ => role.edits.clone().unwrap_or_else(|| edits.clone()),
        };
        if previous.is_some_and(|p| p.name != "base")
            && (!next_tools.is_subset(&tools)
                || !next_verbs.is_subset(&verbs)
                || !edits_narrow(&next_edits, &edits)
                || role
                    .kind
                    .is_some_and(|k| inherited_kind.is_some_and(|p| p != k)))
        {
            return Err(RoleError::Widening(role.name.clone()));
        }
        tools = next_tools;
        verbs = next_verbs;
        if role.name == "base" {
            base_verbs = verbs.clone();
        }
        if let Some(k) = role.kind {
            inherited_kind = Some(k);
        }
        edits = next_edits;
        if let Some(w) = &role.worktree {
            worktree = w.clone();
        }
        prompt.extend(role.prompt.elements.iter().cloned());
        hash.update(source.as_bytes());
        previous = Some(role);
    }
    ceiling::apply(kind, &mut tools, &mut verbs, &mut edits, &mut stripped);
    let mut role = sources
        .get(name)
        .ok_or_else(|| RoleError::Missing(name.into()))?
        .0
        .clone();
    role.summary = chain
        .iter()
        .find_map(|(r, _)| (!r.summary.is_empty()).then(|| r.summary.clone()))
        .unwrap_or_default();
    role.kind = Some(kind);
    role.tier = Some(tier);
    role.tools.allow = Some(tools.clone());
    role.tools.deny.clear();
    role.verbs = verbs.clone();
    role.verbs_declared = true;
    role.edits = Some(edits.clone());
    role.worktree = Some(worktree.clone());
    role.prompt.elements = prompt;
    Ok(Resolved {
        layer: Layer::Builtin,
        role,
        kind,
        tier,
        tools,
        verbs,
        edits,
        worktree,
        sha256: hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        stripped,
    })
}

fn inheritance_chain<'a>(
    name: &'a str,
    sources: &'a Sources,
) -> Result<Vec<&'a (Role, String)>, RoleError> {
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = name;
    loop {
        if chain.len() >= MAX_DEPTH || !seen.insert(current) {
            return Err(RoleError::TooDeep(name.into()));
        }
        let source = sources
            .get(current)
            .ok_or_else(|| RoleError::Missing(current.into()))?;
        chain.push(source);
        match source.0.extends.as_deref() {
            Some("none") => break,
            Some(parent) => current = parent,
            None if current != "base" => current = "base",
            None => break,
        }
    }
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_iter_is_sorted_and_marks_layers() {
        let project = tempfile::tempdir().unwrap();
        let personal = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".agmen/roles")).unwrap();
        let reviewer = BUILT_IN
            .iter()
            .find(|(name, _)| *name == "reviewer")
            .unwrap()
            .1;
        let engineer = BUILT_IN
            .iter()
            .find(|(name, _)| *name == "engineer")
            .unwrap()
            .1;
        std::fs::write(project.path().join(".agmen/roles/reviewer.xml"), reviewer).unwrap();
        std::fs::write(project.path().join(".agmen/roles/engineer.xml"), engineer).unwrap();
        std::fs::write(personal.path().join("engineer.xml"), engineer).unwrap();
        let catalog = Layers {
            project: Some(project.path().to_owned()),
            personal: Some(personal.path().to_owned()),
            ..Layers::default()
        }
        .load()
        .unwrap();
        let names: Vec<_> = catalog.iter().map(|r| r.role.name.as_str()).collect();
        assert_eq!(names.len(), 8);
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(catalog.get("base").unwrap().layer, Layer::Builtin);
        assert_eq!(catalog.get("reviewer").unwrap().layer, Layer::Project);
        assert_eq!(catalog.get("engineer").unwrap().layer, Layer::Personal);
    }
}
