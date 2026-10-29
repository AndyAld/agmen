//! Local agent and team template commands.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use agmen_proto::IdentityId;
use agmen_roles::{
    AgentTemplate, Catalog, Layer, MAX_DIRECTORY_ENTRIES, MAX_NODE_DEPTH, MAX_NODES, MAX_OPTIONS,
    MAX_REFERENCES, MAX_TEMPLATE_BYTES, MAX_TEMPLATE_FILES, Node, NodeAgent, Template,
    TemplateCatalog, TemplateEntry, TemplateLayers, parse_template, to_xml,
};
use anyhow::{Context as _, Result};
use clap::Subcommand;
use serde_json::{Value, json};

use crate::client::blocking;
use crate::git;
use crate::output::{Fail, clean};

const MAX_CATALOG_ENTRIES: usize = 3 * MAX_TEMPLATE_FILES;
const MAX_PROBLEMS: usize = 3 * (MAX_DIRECTORY_ENTRIES + MAX_TEMPLATE_FILES + 2);

#[derive(Debug, Subcommand)]
pub(crate) enum TemplateCommand {
    /// List templates and any skipped files.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Export a template, inlining a team's named agents.
    Export {
        name: String,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    /// Import a file into your personal templates.
    Import {
        file: PathBuf,
        #[arg(long)]
        keep_options: bool,
        #[arg(long)]
        force: bool,
    },
    /// Save a running agent's recorded settings to a personal template.
    #[cfg(unix)]
    Save {
        name: String,
        #[arg(long)]
        from: IdentityId,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        force: bool,
    },
}

pub(crate) fn personal_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/agmen/templates"))
}

pub(crate) async fn catalog(
    project: Option<PathBuf>,
    personal: PathBuf,
) -> Result<TemplateCatalog> {
    blocking(move || {
        Ok(TemplateLayers {
            project,
            personal: Some(personal),
            ..Default::default()
        }
        .load())
    })
    .await
}

fn description(template: &Template) -> Option<&str> {
    match template {
        Template::Agent(named) => named.template.description.as_deref(),
        Template::Team(named) => named.template.description.as_deref(),
    }
}

pub(crate) fn list_text(catalog: &TemplateCatalog) -> String {
    let mut text = String::from("name\tkind\tlayer\tdescription\n");
    for entry in catalog.iter().take(MAX_CATALOG_ENTRIES) {
        let line = description(&entry.template)
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default();
        let end = (0..=line.len().min(80))
            .rev()
            .find(|&end| line.is_char_boundary(end))
            .unwrap_or(0);
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            clean(entry.template.name()),
            clean(&entry.template.kind().to_string()),
            clean(&entry.layer.to_string()),
            clean(&line[..end])
        ));
    }
    for problem in catalog.problems().iter().take(MAX_PROBLEMS) {
        let source = problem
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| problem.layer.to_string());
        // Schema errors and paths may contain terminal controls.
        text.push_str(&clean(&format!("skipped {source}: {}", problem.message)));
        text.push('\n');
    }
    text
}

fn list_json(catalog: &TemplateCatalog) -> Value {
    let templates: Vec<_> = catalog
        .iter()
        .take(MAX_CATALOG_ENTRIES)
        .map(|entry| {
            json!({
                "name": entry.template.name(), "kind": entry.template.kind().to_string(),
                "layer": entry.layer.to_string(), "description": description(&entry.template),
                "saved_by": entry.template.saved_by(), "path": json_path(entry.path.as_deref()),
            })
        })
        .collect();
    let problems: Vec<_> = catalog.problems().iter().take(MAX_PROBLEMS).map(|problem| json!({
        "layer": problem.layer.to_string(), "path": json_path(problem.path.as_deref()), "message": problem.message,
    })).collect();
    json!({"templates":templates, "problems":problems})
}

fn json_path(path: Option<&Path>) -> Option<String> {
    path.map(|path| path.to_string_lossy().into_owned())
}

fn find<'a>(catalog: &'a TemplateCatalog, name: &str) -> Result<&'a TemplateEntry> {
    catalog.get(name).ok_or_else(|| {
        let names = catalog
            .iter()
            .take(MAX_CATALOG_ENTRIES)
            .map(|entry| entry.template.name())
            .collect::<Vec<_>>()
            .join(", ");
        Fail::usage(format!(
            "unknown template {}; available templates: {}",
            clean(name),
            clean(&names)
        ))
        .into()
    })
}

fn export(catalog: &TemplateCatalog, name: &str) -> Result<String> {
    let mut template = find(catalog, name)?.template.clone();
    if let Template::Team(team) = &mut template {
        team.template = catalog.inline(&team.template)?;
    }
    let xml = to_xml(&template);
    if xml.len() > MAX_TEMPLATE_BYTES {
        return Err(Fail::usage("exported template exceeds 32 KiB").into());
    }
    Ok(xml)
}

pub(crate) async fn run(command: TemplateCommand) -> Result<()> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    // Local template commands also work outside a repository.
    let project = git::toplevel(&cwd).await.ok();
    let personal = personal_dir()?;
    let catalog = catalog(project.clone(), personal.clone()).await?;
    match command {
        TemplateCommand::List { json } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&list_json(&catalog))
                        .context("encoding template rows")?
                );
            } else {
                print!("{}", list_text(&catalog));
            }
        }
        TemplateCommand::Export { name, out, force } => {
            let xml = export(&catalog, &name)?;
            if let Some(path) = out {
                blocking(move || {
                    let mut options = OpenOptions::new();
                    options.write(true);
                    if force {
                        options.create(true).truncate(true);
                    } else {
                        options.create_new(true);
                    }
                    let mut file = options
                        .open(&path)
                        .with_context(|| format!("creating {}", path.display()))?;
                    file.write_all(xml.as_bytes())
                        .with_context(|| format!("writing {}", path.display()))
                })
                .await?;
            } else {
                print!("{xml}");
            }
        }
        TemplateCommand::Import {
            file,
            keep_options,
            force,
        } => {
            let roles = super::role::catalog(project).await?;
            let mut template = blocking(move || read_import(&file)).await?;
            print!("{}", import_summary(&template, &catalog, &roles));
            if !keep_options {
                let count = template.strip_options();
                if count > 0 {
                    println!("dropped {count} options; rerun with --keep-options to keep them");
                }
            }
            let path = blocking(move || {
                write_personal(
                    &personal,
                    &catalog,
                    &template,
                    if force {
                        Overwrite::Replace
                    } else {
                        Overwrite::Refuse
                    },
                )
            })
            .await?;
            println!("imported {}", clean(&path.display().to_string()));
        }
        #[cfg(unix)]
        TemplateCommand::Save {
            name,
            from,
            description,
            force,
        } => {
            let template = running_agent(&name, &from, description).await?;
            print!("{}", to_xml(&template));
            let path = blocking(move || {
                write_personal(
                    &personal,
                    &catalog,
                    &template,
                    if force {
                        Overwrite::Replace
                    } else {
                        Overwrite::Refuse
                    },
                )
            })
            .await?;
            println!("saved {}", clean(&path.display().to_string()));
            println!(
                "skills and MCP servers are not recorded for running agents yet; add them to the file by hand"
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) enum Overwrite {
    Refuse,
    Replace,
    #[cfg_attr(not(test), expect(dead_code, reason = "used by the Task 8 MCP writer"))]
    IfSavedBy(String),
}

pub(crate) fn write_personal(
    dir: &Path,
    catalog: &TemplateCatalog,
    template: &Template,
    overwrite: Overwrite,
) -> Result<PathBuf> {
    let xml = to_xml(template);
    if xml.len() > MAX_TEMPLATE_BYTES {
        return Err(Fail::usage("template file exceeds 32 KiB").into());
    }
    // Validate hand-built templates too, before using their names as paths.
    parse_template(&xml)
        .map_err(|error| Fail::usage(format!("invalid template: {}", clean(&error.to_string()))))?;
    let name = template.name();
    let target = if let Some(entry) = catalog
        .get(name)
        .filter(|entry| entry.layer == Layer::Personal)
    {
        let path = entry
            .path
            .as_ref()
            .context("personal template has no path")?;
        let location = clean(&path.display().to_string()).into_owned();
        if entry.template.kind() != template.kind() {
            return Err(Fail::usage(format!(
                "a personal {} template named {name} exists at {location}; pick another name",
                entry.template.kind()
            ))
            .into());
        }
        match overwrite {
            Overwrite::Refuse => {
                return Err(Fail::usage(format!(
                    "{name} exists at {location}; rerun with --force to replace it"
                ))
                .into());
            }
            Overwrite::IfSavedBy(handle) if entry.template.saved_by() != Some(handle.as_str()) => {
                return Err(Fail::usage(format!(
                    "{name} at {location} was not saved by {}",
                    clean(&handle)
                ))
                .into());
            }
            Overwrite::Replace | Overwrite::IfSavedBy(_) => {}
        }
        path.clone()
    } else {
        let path = dir.join(format!("{name}{}", template.kind().file_suffix()));
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                return Err(Fail::usage(format!(
                    "{} exists; pick another name",
                    clean(&path.display().to_string())
                ))
                .into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", path.display()));
            }
        }
        path
    };
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut temporary = target.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    let result: Result<PathBuf> = (|| {
        file.write_all(xml.as_bytes())
            .with_context(|| format!("writing {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, &target)
            .with_context(|| format!("replacing {}", target.display()))?;
        Ok(target)
    })();
    // Keep the write error: a failed cleanup is added to it, not shown instead.
    result.map_err(|error| match fs::remove_file(&temporary) {
        Ok(()) => error,
        Err(cleanup) => error.context(format!(
            "and removing {} failed: {cleanup}",
            temporary.display()
        )),
    })
}

fn read_import(path: &Path) -> Result<Template> {
    let mut bytes = Vec::new();
    File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .take((MAX_TEMPLATE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() > MAX_TEMPLATE_BYTES {
        return Err(Fail::usage("template file exceeds 32 KiB").into());
    }
    let xml = std::str::from_utf8(&bytes).context("template file is not UTF-8")?;
    parse_template(xml).with_context(|| format!("parsing {}", path.display()))
}

fn import_summary(template: &Template, catalog: &TemplateCatalog, roles: &Catalog) -> String {
    let mut text = format!(
        "name: {}\nkind: {}\noptions:\n",
        clean(template.name()),
        template.kind()
    );
    let mut agents = Vec::new();
    match template {
        Template::Agent(named) => agents.push(&named.template),
        Template::Team(named) => collect_agents(&named.template.nodes, &mut agents, 1),
    }
    for agent in agents.iter().take(MAX_NODES) {
        for option in agent.options.iter().take(MAX_OPTIONS) {
            text.push_str(&format!(
                "  {} = {}\n",
                clean(&option.name),
                clean(&option.value)
            ));
        }
    }
    if let Some(description) = description(template) {
        text.push_str(&format!("description: {}\n", clean(description)));
    }
    for agent in agents.into_iter().take(MAX_NODES) {
        agent_summary(&mut text, agent, roles);
    }
    if let Template::Team(named) = template {
        missing_nodes(&mut text, &named.template.nodes, catalog, 1);
    }
    text
}

fn collect_agents<'a>(nodes: &'a [Node], agents: &mut Vec<&'a AgentTemplate>, depth: u32) {
    if depth > MAX_NODE_DEPTH {
        return;
    }
    for node in nodes.iter().take(MAX_NODES) {
        if let NodeAgent::Inline(agent) = &node.agent {
            agents.push(agent);
        }
        collect_agents(&node.children, agents, depth + 1);
    }
}

fn agent_summary(text: &mut String, agent: &AgentTemplate, roles: &Catalog) {
    for (label, value) in [
        ("agent", &agent.agent.kind),
        ("model", &agent.agent.model),
        ("effort", &agent.agent.effort),
    ] {
        if let Some(value) = value {
            text.push_str(&format!("{label}: {}\n", clean(value)));
        }
    }
    if let Some(role) = &agent.role {
        text.push_str(&format!(
            "role: {}{}\n",
            clean(role),
            if roles.get(role).is_none() {
                ": missing"
            } else {
                ""
            }
        ));
    }
    for (label, references) in [("skill", &agent.skills), ("MCP", &agent.mcp)] {
        for reference in references.iter().take(MAX_REFERENCES) {
            let source = reference.pack.as_ref().map_or_else(
                || "local".to_owned(),
                |pack| {
                    format!("pack {pack}: not installed (pack installs arrive in a later phase)")
                },
            );
            text.push_str(&format!(
                "{label} {}: {}\n",
                clean(&reference.name),
                clean(&source)
            ));
        }
    }
    if let Some(budget) = &agent.budget_usd {
        text.push_str(&format!("budget: {}\n", clean(budget)));
    }
    if let Some(prompt) = &agent.prompt {
        text.push_str(&format!("prompt: {}\n", clean(prompt)));
    }
}

fn missing_nodes(text: &mut String, nodes: &[Node], catalog: &TemplateCatalog, depth: u32) {
    if depth > MAX_NODE_DEPTH {
        return;
    }
    for node in nodes.iter().take(MAX_NODES) {
        if let NodeAgent::Named(name) = &node.agent
            && catalog.get(name).is_none()
        {
            text.push_str(&format!("node {}: not found here\n", clean(name)));
        }
        missing_nodes(text, &node.children, catalog, depth + 1);
    }
}

#[cfg(unix)]
async fn running_agent(
    name: &str,
    from: &IdentityId,
    description: Option<String>,
) -> Result<Template> {
    let mut daemon = super::workers::connect().await?;
    let answer = super::workers::call(&mut daemon, "agent_list", json!({})).await?;
    let agents = answer
        .get("agents")
        .and_then(Value::as_array)
        .context("agmend's answer has no agents array")?;
    // Each record occupies at least one byte in the already bounded local frame.
    let entry = agents
        .iter()
        .take(agmen_local::MAX_FRAME as usize)
        .find(|entry| entry.get("identity").and_then(Value::as_str) == Some(from.as_str()))
        .ok_or_else(|| Fail::usage(format!("{from} is not one of your agents on this machine")))?;
    from_agent(entry, name, description)
}

#[cfg(any(unix, test))]
fn from_agent(entry: &Value, name: &str, description: Option<String>) -> Result<Template> {
    let identity = entry
        .get("identity")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let template = Template::Agent(agmen_roles::Named {
        name: name.to_owned(),
        saved_by: None,
        template: AgentTemplate {
            description,
            agent: agmen_roles::AgentSpec {
                // shortcut: agmend launches only Claude Code (ClaudeRunner, launch.rs); read the kind from agent_list once agent profiles add it (solo-mode Phase 4).
                kind: Some("claude".into()),
                model: entry
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                effort: None,
            },
            role: entry.get("role").and_then(Value::as_str).map(str::to_owned),
            budget_usd: entry
                .get("budget_usd")
                .and_then(Value::as_str)
                .map(str::to_owned),
            ..Default::default()
        },
    });
    parse_template(&to_xml(&template)).map_err(|error| {
        Fail::usage(format!(
            "agent {}: {}",
            clean(identity),
            clean(&error.to_string())
        ))
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agmen_roles::Named;

    #[test]
    fn write_personal_refuses_a_kind_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xx.team.xml");
        let xml = r#"<team-template name="xx"><node template="claude"/></team-template>"#;
        std::fs::write(&path, xml).unwrap();
        let catalog = TemplateLayers {
            personal: Some(dir.path().into()),
            ..Default::default()
        }
        .load();
        let agent = parse_template(r#"<agent-template name="xx"/>"#).unwrap();
        let error = write_personal(dir.path(), &catalog, &agent, Overwrite::Replace).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("personal team template named xx"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), xml);
    }

    #[test]
    fn write_personal_checks_serialized_size() {
        let dir = tempfile::tempdir().unwrap();
        let template = Template::Agent(Named {
            name: "plain".into(),
            saved_by: None,
            template: AgentTemplate {
                prompt: Some("<".repeat(16 * 1024)),
                ..Default::default()
            },
        });
        let error = write_personal(
            dir.path(),
            &TemplateLayers::default().load(),
            &template,
            Overwrite::Replace,
        )
        .unwrap_err();
        assert!(error.to_string().contains("32 KiB"), "{error}");
        assert!(!dir.path().join("plain.agent.xml").exists());
    }

    #[test]
    fn write_personal_refuses_a_path_in_the_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("templates");
        std::fs::create_dir(&dir).unwrap();
        let catalog = TemplateLayers::default().load();
        for name in ["../x", "a/b", "..", "x/../../y", "Ab", ""] {
            let template = Template::Agent(Named {
                name: name.into(),
                saved_by: None,
                template: AgentTemplate::default(),
            });
            let error = write_personal(&dir, &catalog, &template, Overwrite::Replace).unwrap_err();
            assert!(
                error.to_string().contains("invalid template"),
                "{name}: {error}"
            );
        }
        let names: Vec<_> = std::fs::read_dir(root.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "nothing is written outside the folder");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn write_personal_leaves_no_temp_file_when_the_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.agent.xml");
        std::fs::write(&path, r#"<agent-template name="plain"/>"#).unwrap();
        let catalog = TemplateLayers {
            personal: Some(dir.path().into()),
            ..Default::default()
        }
        .load();
        // The catalog still names the file, but a folder now sits there, so the rename fails.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let template = parse_template(r#"<agent-template name="plain"/>"#).unwrap();
        let error =
            write_personal(dir.path(), &catalog, &template, Overwrite::Replace).unwrap_err();
        assert!(error.to_string().contains("replacing"), "{error:#}");
        assert!(path.is_dir());
        assert!(!dir.path().join("plain.agent.xml.tmp").exists());
    }

    #[test]
    fn write_personal_refuses_unloaded_files_and_temp_links() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = TemplateLayers::default().load();
        let template = parse_template(r#"<agent-template name="plain"/>"#).unwrap();
        let path = dir.path().join("plain.agent.xml");
        std::fs::write(&path, "broken").unwrap();
        assert!(write_personal(dir.path(), &catalog, &template, Overwrite::Replace).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::remove_file(&path).unwrap();
        let tmp = dir.path().join("plain.agent.xml.tmp");
        std::fs::write(&tmp, "reserved").unwrap();
        assert!(write_personal(dir.path(), &catalog, &template, Overwrite::Refuse).is_err());
        assert_eq!(std::fs::read_to_string(&tmp).unwrap(), "reserved");
        assert!(!path.exists());
    }

    #[test]
    fn agent_cannot_replace_a_persons_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.agent.xml");
        let original = r#"<agent-template name="plain"/>"#;
        std::fs::write(&path, original).unwrap();
        let catalog = TemplateLayers {
            personal: Some(dir.path().into()),
            ..Default::default()
        }
        .load();
        let agent = parse_template(r#"<agent-template name="plain" saved-by="ada/sol"/>"#).unwrap();
        let error = write_personal(
            dir.path(),
            &catalog,
            &agent,
            Overwrite::IfSavedBy("ada/sol".into()),
        )
        .unwrap_err();
        assert!(error.to_string().contains("was not saved by ada/sol"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn write_personal_checks_saved_by_length() {
        let dir = tempfile::tempdir().unwrap();
        let mut template = parse_template(r#"<agent-template name="plain"/>"#).unwrap();
        template.set_saved_by(Some("x".repeat(65)));
        let error = write_personal(
            dir.path(),
            &TemplateLayers::default().load(),
            &template,
            Overwrite::Refuse,
        )
        .unwrap_err();
        assert!(error.to_string().contains("saved-by"));
        assert!(!dir.path().join("plain.agent.xml").exists());
    }

    #[test]
    fn from_agent_maps_the_record() {
        let record =
            json!({"identity":"agent001", "model":"sonnet", "budget_usd":"5", "role":"engineer"});
        let Template::Agent(named) =
            from_agent(&record, "sol-copy", Some("A copy".into())).unwrap()
        else {
            panic!("agent")
        };
        assert_eq!(named.name, "sol-copy");
        assert_eq!(named.saved_by, None);
        let agent = named.template;
        assert_eq!(agent.agent.kind.as_deref(), Some("claude"));
        assert_eq!(agent.agent.model.as_deref(), Some("sonnet"));
        assert_eq!(agent.role.as_deref(), Some("engineer"));
        assert_eq!(agent.budget_usd.as_deref(), Some("5"));
        assert_eq!(agent.description.as_deref(), Some("A copy"));
        assert!(agent.agent.effort.is_none());
        assert!(agent.skills.is_empty() && agent.mcp.is_empty() && agent.options.is_empty());
    }

    #[test]
    fn from_agent_rejects_a_bad_value() {
        for (field, value, expected) in [
            ("budget_usd", "1e2", "usd"),
            ("model", "-rf", "model"),
            ("role", "Bad", "role"),
        ] {
            let mut record = json!({"identity":"agent001"});
            record[field] = json!(value);
            let error = from_agent(&record, "sol-copy", None).unwrap_err();
            let text = error.to_string();
            assert!(
                text.contains(expected) && text.contains("agent001"),
                "{text}"
            );
        }
    }

    #[test]
    fn from_agent_accepts_missing_fields_and_escapes_description() {
        let template = from_agent(
            &json!({"identity":"agent001", "model":null, "budget_usd":null, "role":null}),
            "sol-copy",
            Some("<options> & text".into()),
        )
        .unwrap();
        assert_eq!(parse_template(&to_xml(&template)).unwrap(), template);
        let Template::Agent(named) = template else {
            panic!("agent")
        };
        assert_eq!(
            named.template.description.as_deref(),
            Some("<options> & text")
        );
        assert!(named.template.agent.model.is_none() && named.template.role.is_none());
    }

    #[test]
    fn agent_can_replace_only_its_own_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.agent.xml");
        std::fs::write(
            &path,
            r#"<agent-template name="plain" saved-by="ada/sol"/>"#,
        )
        .unwrap();
        let catalog = TemplateLayers {
            personal: Some(dir.path().into()),
            ..Default::default()
        }
        .load();
        let template = parse_template(r#"<agent-template name="plain" saved-by="ada/sol"><description>new</description></agent-template>"#).unwrap();
        assert!(
            write_personal(
                dir.path(),
                &catalog,
                &template,
                Overwrite::IfSavedBy("ada/other".into())
            )
            .is_err()
        );
        assert!(!std::fs::read_to_string(&path).unwrap().contains("new"));
        write_personal(
            dir.path(),
            &catalog,
            &template,
            Overwrite::IfSavedBy("ada/sol".into()),
        )
        .unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("new"));
    }

    #[cfg(unix)]
    #[test]
    fn json_paths_accept_non_utf8_names() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let path = PathBuf::from(OsString::from_vec(b"bad\xff.xml".to_vec()));
        let value = json!({"path":json_path(Some(&path))});
        assert_eq!(value["path"], "bad\u{fffd}.xml");
        assert_eq!(json!({"path":json_path(None)})["path"], Value::Null);
    }
}
