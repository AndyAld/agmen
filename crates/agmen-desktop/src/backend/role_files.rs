//! The Roles panel's files, resolved from their XML (spec 3.5), for the
//! fixture and for agmend's role files alike (Task 8).

use gpui::SharedString;

use crate::roles::{self, RoleSpec};

use super::{PersonGroup, RoleFile, RoleInfo, RoleLayer};

/// The precedence of a layer when two files share a name: a personal role
/// overrides a project one, which overrides a built-in (spec 3.5).
const fn rank(layer: RoleLayer) -> u8 {
    match layer {
        RoleLayer::Personal => 0,
        RoleLayer::Project => 1,
        RoleLayer::BuiltIn => 2,
    }
}

/// The built-in role whose color a role's dot takes: itself, or the
/// nearest built-in it extends. Base has no role color.
fn palette_role(
    name: &str,
    files: &[(String, RoleLayer, SharedString)],
    specs: &[RoleSpec],
) -> Option<String> {
    let mut current = name;
    // Bounded like resolve: a chain longer than the files is a loop.
    for _ in 0..=files.len() {
        if current == "base" {
            return None;
        }
        if files
            .iter()
            .any(|(n, layer, _)| n == current && *layer == RoleLayer::BuiltIn)
        {
            return Some(current.to_owned());
        }
        current = specs
            .iter()
            .find(|s| s.name == current)?
            .extends
            .as_deref()?;
    }
    None
}

/// What the Role panel shows for `own`, resolved against `effective`
/// (each name's winning file first), or `None` when it does not resolve.
fn info(own: &RoleSpec, effective: &[RoleSpec], roster: &[PersonGroup]) -> Option<RoleInfo> {
    let name = own.name.as_str();
    // The file's own settings first, then whatever its parents resolve to.
    let specs: Vec<RoleSpec> = std::iter::once(own.clone())
        .chain(effective.iter().cloned())
        .collect();
    let resolved = roles::resolve(name, &specs).ok()?;
    // Ruling D12: what the kind could send but this role cannot.
    let refused = agmen_proto::Verb::ALL
        .iter()
        .filter(|v| roles::kind_may_send(resolved.kind, **v))
        .map(|v| v.as_str())
        .filter(|v| !resolved.verbs.iter().any(|own| own == v))
        .map(str::to_owned)
        .collect();
    let used_by: Vec<(String, String)> = roster
        .iter()
        .flat_map(|group| &group.agents)
        .filter(|agent| agent.role == name)
        .map(|agent| {
            let mut note = format!("{} · {}", agent.harness, agent.status.label());
            if let Some(item) = &agent.item {
                note = format!("{note} {item}");
            }
            (agent.handle.clone(), note)
        })
        .collect();
    let mut extended_by: Vec<String> = effective
        .iter()
        .filter(|s| s.extends.as_deref() == Some(name))
        .map(|s| s.name.clone())
        .collect();
    extended_by.dedup();
    // Spec 3.4: no agent can limit which commands a shell runs.
    let read_only_shell =
        resolved.tools.iter().any(|t| t == "shell") && resolved.edits == roles::Edits::None;
    Some(RoleInfo {
        kind: resolved.kind,
        tier: resolved.tier,
        tools: resolved.tools,
        bash_note: read_only_shell.then(|| "read-only by rule only; not enforceable".to_owned()),
        edits: resolved.edits,
        verbs: resolved.verbs,
        refused,
        used_by,
        extended_by,
        mention: (
            format!("@{name}s"),
            format!("role:{name} on the wire · humans and coordinators"),
        ),
    })
}

/// Each role file with its Role panel, in the order given. A file that
/// does not parse or resolve shows its text with no panel.
pub(super) fn role_files(
    files: Vec<(String, RoleLayer, SharedString)>,
    roster: &[PersonGroup],
) -> Vec<RoleFile> {
    let parsed: Vec<Option<RoleSpec>> =
        files.iter().map(|(_, _, t)| roles::parse(t).ok()).collect();
    // Every parsed file, the winning layer of each name first, so resolve
    // finds the file that overrides the others.
    let mut ranked: Vec<(u8, &RoleSpec)> = files
        .iter()
        .zip(&parsed)
        .filter_map(|((_, layer, _), spec)| spec.as_ref().map(|s| (rank(*layer), s)))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    let effective: Vec<RoleSpec> = ranked.into_iter().map(|(_, s)| s.clone()).collect();
    files
        .iter()
        .zip(&parsed)
        .map(|((name, layer, text), spec)| RoleFile {
            name: name.clone(),
            layer: *layer,
            text: text.clone(),
            info: spec.as_ref().and_then(|own| info(own, &effective, roster)),
            palette_role: palette_role(name, &files, &effective),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_role_overrides_the_built_in() {
        let builtin = roles::BUILT_IN.iter().map(|(n, t)| {
            (
                (*n).to_owned(),
                RoleLayer::BuiltIn,
                SharedString::new_static(t),
            )
        });
        let project = (
            "reviewer".to_owned(),
            RoleLayer::Project,
            SharedString::from(
                r#"<role name="reviewer" extends="base" kind="agent" tier="small" version="1"/>"#,
            ),
        );
        let files: Vec<_> = builtin.chain([project]).collect();
        let out = role_files(files, &[]);
        let tiers: Vec<_> = out
            .iter()
            .filter(|r| r.name == "reviewer")
            .map(|r| (r.layer, r.info.as_ref().map(|i| i.tier)))
            .collect();
        // Each file shows its own settings.
        assert_eq!(
            tiers,
            [
                (RoleLayer::BuiltIn, Some(roles::Tier::Deep)),
                (RoleLayer::Project, Some(roles::Tier::Small)),
            ]
        );
    }
}
