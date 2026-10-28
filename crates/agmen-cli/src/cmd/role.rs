//! Local role discovery, inspection and bounded subagent imports.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::PathBuf;

use agmen_roles::{Catalog, Layers, Resolved, import_subagent, parse, render_prompt};
use anyhow::{Context as _, Result};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;

use crate::client::blocking;
use crate::git;
use crate::output::{Fail, clean};

const MAX_IMPORT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Subcommand)]
pub(crate) enum RoleCommand {
    /// List available roles, excluding the base template.
    List {
        /// Print rows as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the resolved settings, prompt and stripped permissions.
    Show { name: String },
    /// Import a Claude subagent Markdown file without overwriting a role.
    Import {
        file: PathBuf,
        #[arg(long, value_enum, default_value = "project")]
        to: Destination,
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum Destination {
    Project,
    Personal,
}

fn personal_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/agmen/roles"))
}

/// Reused by join so role discovery has one source of truth.
pub(super) async fn catalog(project: Option<PathBuf>) -> Result<Catalog> {
    let layers = Layers {
        project,
        personal: Some(personal_dir()?),
        ..Layers::default()
    };
    blocking(move || layers.load().context("loading roles")).await
}

pub(super) fn find<'a>(catalog: &'a Catalog, name: &str) -> Result<&'a Resolved> {
    catalog.get(name).ok_or_else(|| {
        let names = catalog
            .iter()
            .filter(|r| r.role.name != "base")
            .map(|r| clean(&r.role.name).into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        Fail::usage(format!(
            "unknown role {}; available roles: {names}",
            clean(name)
        ))
        .into()
    })
}

#[derive(Serialize)]
struct Row<'a> {
    name: &'a str,
    layer: String,
    kind: String,
    tier: String,
    tools: Vec<String>,
    verbs: &'a std::collections::BTreeSet<String>,
}

impl<'a> From<&'a Resolved> for Row<'a> {
    fn from(role: &'a Resolved) -> Self {
        Self {
            name: &role.role.name,
            layer: role.layer.to_string(),
            kind: role.kind.to_string(),
            tier: role.tier.to_string(),
            tools: role.tools.iter().map(ToString::to_string).collect(),
            verbs: &role.verbs,
        }
    }
}

pub(crate) async fn run(command: RoleCommand) -> Result<()> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    // Role listing and personal imports also work outside a repository.
    let project = git::toplevel(&cwd).await.ok();
    match command {
        RoleCommand::Import { file, to, name } => {
            let (dir, root) = match to {
                Destination::Project => {
                    let root = project.context("project imports run inside a git worktree")?;
                    (root.join(".agmen/roles"), Some(root))
                }
                Destination::Personal => (personal_dir()?, None),
            };
            let path = blocking(move || import(file, dir, root, name)).await?;
            println!("imported {}", clean(&path.display().to_string()));
        }
        RoleCommand::List { json } => {
            let catalog = catalog(project).await?;
            let rows: Vec<_> = catalog
                .iter()
                .filter(|r| r.role.name != "base")
                .map(Row::from)
                .collect();
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&rows).context("encoding role rows")?
                );
            } else {
                println!("name\tlayer\tkind\ttier\ttools\tverbs");
                for row in rows {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        clean(row.name),
                        row.layer,
                        row.kind,
                        row.tier,
                        clean(&row.tools.join(" ")),
                        clean(&row.verbs.iter().cloned().collect::<Vec<_>>().join(" "))
                    );
                }
            }
        }
        RoleCommand::Show { name } => {
            let catalog = catalog(project).await?;
            let role = find(&catalog, &name)?;
            let row = Row::from(role);
            println!(
                "name: {}\nlayer: {}\nkind: {}\ntier: {}\ntools: {}\nverbs: {}\nedits: {}\nworktree: {}\nrole_sha256: {}",
                clean(row.name),
                row.layer,
                row.kind,
                row.tier,
                clean(&row.tools.join(" ")),
                clean(&row.verbs.iter().cloned().collect::<Vec<_>>().join(" ")),
                clean(&role.edits.to_string()),
                clean(&format!("{:?}", role.worktree)),
                role.sha256
            );
            println!("prompt:");
            for line in render_prompt(role).lines() {
                println!("{}", clean(line));
            }
            for stripped in &role.stripped {
                println!("stripped: {}", clean(stripped));
            }
        }
    }
    Ok(())
}

fn import(
    file: PathBuf,
    dir: PathBuf,
    root: Option<PathBuf>,
    name: Option<String>,
) -> Result<PathBuf> {
    let name = name
        .or_else(|| file.file_stem().and_then(|s| s.to_str()).map(str::to_owned))
        .ok_or_else(|| Fail::usage("give --name for an input without a UTF-8 file stem"))?;
    let mut md = String::new();
    File::open(&file)
        .with_context(|| format!("opening {}", file.display()))?
        .take(MAX_IMPORT_BYTES + 1)
        .read_to_string(&mut md)
        .with_context(|| format!("reading {}", file.display()))?;
    if md.len() as u64 > MAX_IMPORT_BYTES {
        return Err(Fail::usage("subagent file exceeds 64 KiB").into());
    }
    let xml = import_subagent(&md, &name).map_err(|err| Fail::usage(clean(&err.to_string())))?;
    // Validate the generated XML before creating any output file.
    parse(&xml).context("validating imported role XML")?;
    if let Some(root) = root {
        let root = fs::canonicalize(&root).context("resolving project root")?;
        // Check each parent before creating a child, so an existing directory
        // link cannot redirect a project import outside the checkout.
        for folder in [root.join(".agmen"), dir.clone()] {
            match fs::create_dir(&folder) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => {
                    return Err(err).with_context(|| format!("creating {}", folder.display()));
                }
            }
            let canonical = fs::canonicalize(&folder)
                .with_context(|| format!("resolving {}", folder.display()))?;
            anyhow::ensure!(
                canonical.starts_with(&root),
                "project role directory escapes the checkout"
            );
        }
    } else {
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let path = dir.join(format!("{name}.xml"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| {
            format!(
                "creating {} (existing roles are never overwritten)",
                path.display()
            )
        })?;
    output
        .write_all(xml.as_bytes())
        .and_then(|()| output.sync_all())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
