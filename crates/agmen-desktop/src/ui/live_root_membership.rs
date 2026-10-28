//! Membership selection, project creation and invites through the bundled helper.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use gpui::prelude::FluentBuilder as _;
use gpui::{Context, IntoElement, ParentElement as _, Styled as _, Window, div};
use serde::Deserialize;

use super::{LiveRoot, ProjectChoice};
use crate::ui::components::{Button, ButtonStyle, Label, Modal};

#[derive(Debug, Clone, Deserialize)]
pub(super) struct Membership {
    pub(super) tenant: agmen_proto::TenantId,
    pub(super) project: agmen_proto::ProjectId,
    pub(super) name: String,
    role: MembershipRole,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MembershipRole {
    Admin,
    Coordinator,
    Member,
}

#[derive(Debug, Clone)]
pub(super) enum Action {
    List,
    Join(Membership),
    Create {
        name: String,
        tenant: Option<agmen_proto::TenantId>,
    },
    Invite(agmen_proto::ProjectId),
}

enum Answer {
    Projects(Vec<Membership>),
    Created(Membership),
    Joined(Membership, PathBuf),
    Invite(String),
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct HelperFailure {
    signed_out: bool,
    message: String,
}

fn successful(output: std::process::Output) -> anyhow::Result<Vec<u8>> {
    if !output.status.success() {
        let text = String::from_utf8_lossy(&output.stderr);
        return Err(HelperFailure {
            signed_out: output.status.code() == Some(3),
            message: format!(
                "Bundled agmen exited with {}: {}",
                output.status,
                text.trim()
            ),
        }
        .into());
    }
    Ok(output.stdout)
}

fn execute(
    program: &Path,
    helper: &super::onboarding::Helper,
    folder: Option<&Path>,
    action: Action,
) -> anyhow::Result<Answer> {
    let run = |args: &[std::ffi::OsString]| {
        successful(helper(
            program,
            args,
            Duration::from_secs(120),
            folder,
            &[],
            &mut |_| {},
        )?)
    };
    let join = |project: Membership| -> anyhow::Result<Answer> {
        let working = folder.context("select a folder before joining")?;
        // A previous helper may have joined this folder while its resolver was pending.
        if crate::project::choose_folder(working).0
            == ProjectChoice::Project(project.project.clone())
        {
            return Ok(Answer::Joined(project, working.to_owned()));
        }
        run(&[
            "join".into(),
            "--project".into(),
            project.project.as_str().into(),
        ])?;
        ensure!(
            matches!(crate::project::choose_folder(working).0, ProjectChoice::Project(id) if id == project.project),
            "agmen join did not create the selected project's trusted identity in this folder"
        );
        Ok(Answer::Joined(project, working.to_owned()))
    };
    match action {
        Action::List => {
            #[derive(Deserialize)]
            struct Me {
                projects: Vec<Membership>,
            }
            let bytes = run(&["whoami".into(), "--json".into()])?;
            let me: Me = serde_json::from_slice(&bytes).context("reading your projects failed")?;
            Ok(Answer::Projects(me.projects))
        }
        Action::Join(project) => join(project),
        Action::Create { name, tenant } => {
            ensure!(
                folder.is_some(),
                "select a folder before creating a project"
            );
            let mut args = vec!["project".into(), "create".into()];
            if let Some(tenant) = tenant {
                args.extend(["--tenant".into(), tenant.as_str().into()]);
            }
            if name.starts_with('-') {
                args.push("--".into());
            }
            args.push(name.into());
            let project = serde_json::from_slice(&run(&args)?)
                .context("reading the created project failed")?;
            Ok(Answer::Created(project))
        }
        Action::Invite(project) => {
            let bytes = run(&["invite".into(), "--project".into(), project.as_str().into()])?;
            let text = std::str::from_utf8(&bytes).context("reading the invite failed")?;
            let code = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("agmen join "))
                .filter(|code| {
                    code.len() <= 8192
                        && code
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                })
                .context("the helper did not return an invite code")?;
            Ok(Answer::Invite(code.to_owned()))
        }
    }
}

impl LiveRoot {
    pub(super) fn list_memberships(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._resolving.is_some()
            || self.rejected.as_ref().is_some_and(|folder| !folder.is_git)
            || (self.rejected.is_none()
                && !matches!(self.launch.project, ProjectChoice::Project(_)))
        {
            return;
        }
        if self.onboarding.signed_in
            || self
                .onboarding
                .login_path
                .as_ref()
                .is_some_and(|path| path.is_file())
        {
            self.start_membership(Action::List, window, cx);
        }
    }

    pub(super) fn start_membership(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.onboarding.task.is_some() {
            return;
        }
        let Some(program) = self.onboarding.program.clone() else {
            self.service_error = Some("The bundled agmen helper is missing. Install agmen.app from https://getagmen.ai/download/mac".into());
            cx.notify();
            return;
        };
        let folder = self
            .rejected
            .as_ref()
            .map(|f| f.folder.clone())
            .or_else(|| self.launch.root.clone());
        let working = folder.clone();
        let generation = self.onboarding.generation;
        let helper = self.onboarding.helper.clone();
        let listing = matches!(action, Action::List);
        self.service_error = None;
        let (send, receive) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("project-membership".into())
            .spawn(move || {
                let result = execute(&program, helper.as_ref(), working.as_deref(), action);
                drop(send.send(result));
            });
        if let Err(error) = spawned {
            self.service_error = Some(format!("Starting bundled agmen failed: {error}"));
            cx.notify();
            return;
        }
        self.onboarding.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = receive
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("the bundled helper stopped")));
            drop(this.update_in(cx, |this, window, cx| {
                this.onboarding.task = None;
                cx.notify();
                if this.onboarding.generation != generation {
                    // A new folder was selected while this helper was running.
                    if this._resolving.is_none() {
                        this.list_memberships(window, cx);
                    }
                    return;
                }
                match result {
                    Ok(Answer::Projects(projects)) => {
                        this.onboarding.selected = match projects.as_slice() {
                            [only] => Some(only.project.clone()),
                            _ => None,
                        };
                        this.onboarding.projects = Some(projects);
                        if this.rejected.is_some()
                            && let Some([only]) = this.onboarding.projects.as_deref()
                        {
                            this.start_membership(Action::Join(only.clone()), window, cx);
                        }
                    }
                    Ok(Answer::Created(project)) => {
                        this.onboarding.selected = Some(project.project.clone());
                        this.onboarding.projects = Some(vec![project.clone()]);
                        this.start_membership(Action::Join(project), window, cx);
                    }
                    Ok(Answer::Joined(project, folder)) => {
                        let name = folder
                            .file_name()
                            .unwrap_or(folder.as_os_str())
                            .to_string_lossy();
                        this.onboarding.join_notice = Some((
                            project.project.clone(),
                            folder.clone(),
                            format!("Joined {name} to {}", project.name),
                        ));
                        this.onboarding.projects = Some(vec![project]);
                        this.open_folder(folder, window, cx);
                    }
                    Ok(Answer::Invite(code)) => this.onboarding.invite_code = Some(code),
                    Err(error) => {
                        if error
                            .downcast_ref::<HelperFailure>()
                            .is_some_and(|failure| failure.signed_out)
                        {
                            this.onboarding.signed_in = false;
                            this.onboarding.login_revision =
                                this.onboarding.login_revision.wrapping_add(1);
                            this.onboarding.projects = None;
                        }
                        if listing {
                            this.onboarding.projects = None;
                        }
                        this.service_error = Some(format!("{error:#}"));
                    }
                }
                this.sync_invite_people(cx);
            }));
        }));
        cx.notify();
    }

    pub(super) fn create_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.onboarding.project_name.read(cx).text().to_owned();
        if name.is_empty()
            || name.chars().count() > 64
            || name.trim() != name
            || name.chars().any(char::is_control)
        {
            self.service_error = Some("The project name must be 1 to 64 characters, with no control characters or space at either end.".into());
            cx.notify();
            return;
        }
        let tenant = self.onboarding.tenant.read(cx).text().trim();
        let tenant = if tenant.is_empty() {
            None
        } else {
            match tenant.parse() {
                Ok(tenant) => Some(tenant),
                Err(error) => {
                    self.service_error = Some(format!("Invalid tenant id: {error}"));
                    cx.notify();
                    return;
                }
            }
        };
        self.start_membership(Action::Create { name, tenant }, window, cx);
    }

    pub(super) fn project_controls(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.onboarding.task.is_some();
        let projects = self.onboarding.projects.clone().unwrap_or_default();
        let selected = self.onboarding.selected.clone();
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::tokens::SPACE_10)
            .when(!projects.is_empty(), |this| {
                this.child(Label::new("Choose a project"))
                    .children(projects.iter().enumerate().map(|(index, project)| {
                        let project = project.clone();
                        let active = selected.as_ref() == Some(&project.project);
                        Button::new(
                            format!("live:project-choice:{index}"),
                            format!(
                                "{}{} (tenant {})",
                                if active { "✓ " } else { "" },
                                project.name,
                                project.tenant
                            ),
                        )
                        .disabled(busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.onboarding.selected = Some(project.project.clone());
                            cx.notify();
                        }))
                    }))
                    .child(
                        Button::new("live:join-project", "Join")
                            .style(ButtonStyle::Primary)
                            .disabled(busy || selected.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(project) = this
                                    .onboarding
                                    .projects
                                    .as_ref()
                                    .and_then(|projects| {
                                        projects.iter().find(|p| {
                                            Some(&p.project) == this.onboarding.selected.as_ref()
                                        })
                                    })
                                    .cloned()
                                {
                                    this.start_membership(Action::Join(project), window, cx);
                                }
                            })),
                    )
            })
            .when(self.onboarding.projects.is_none() && !busy, |this| {
                this.child(
                    Button::new("live:retry-projects", "Retry loading projects").on_click(
                        cx.listener(|this, _, window, cx| this.list_memberships(window, cx)),
                    ),
                )
            })
            .when(
                self.onboarding.projects.as_ref().is_some_and(Vec::is_empty),
                |this| {
                    this.child(Label::new("Project name"))
                        .child(self.onboarding.project_name.clone())
                        .child(Label::new("Tenant id (if you administer several tenants)"))
                        .child(self.onboarding.tenant.clone())
                        .child(
                            Button::new("live:create-project", "Create project")
                                .style(ButtonStyle::Primary)
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.create_project(window, cx)
                                })),
                        )
                },
            )
    }

    pub(super) fn can_invite(&self) -> bool {
        let project = match &self.launch.project {
            ProjectChoice::Project(id) => Some(id),
            _ => None,
        };
        self.rejected.is_none()
            && self
                .onboarding
                .projects
                .as_ref()
                .and_then(|projects| projects.iter().find(|p| Some(&p.project) == project))
                .is_some_and(|p| {
                    matches!(p.role, MembershipRole::Admin | MembershipRole::Coordinator)
                })
    }

    pub(super) fn sync_invite_people(&self, cx: &mut Context<Self>) {
        if let super::LiveState::Open(workspace) = &self.state {
            let enabled = self.can_invite();
            workspace.update(cx, |workspace, cx| workspace.set_invite_people(enabled, cx));
        }
    }

    pub(super) fn invite_controls(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div().when_some(
            self.onboarding
                .invite_code
                .clone()
                .filter(|_| self.can_invite()),
            |this, code| {
                let command = format!("agmen join {code}");
                this.child(
                    Modal::new(
                        "live:invite-result",
                        crate::ui::tokens::LAUNCH_STATE_WIDTH,
                        crate::ui::tokens::TITLE_BAR_HEIGHT,
                    )
                    .on_dismiss(cx.listener(|this, _, _, cx| {
                        this.onboarding.invite_code = None;
                        cx.notify();
                    }))
                    .child(Label::new(code.clone()).wrap())
                    .child(Button::new("live:copy-invite-code", "Copy code").on_click(
                        move |_, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(code.clone()))
                        },
                    ))
                    .child(Label::new(command.clone()).wrap())
                    .child(
                        Button::new("live:copy-invite-command", "Copy command").on_click(
                            move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    command.clone(),
                                ))
                            },
                        ),
                    )
                    .child(Label::new("Each code works once."))
                    .child(
                        Button::new("live:close-invite", "Close").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.onboarding.invite_code = None;
                                cx.notify();
                            },
                        )),
                    ),
                )
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt as _;

    #[test]
    fn invite_output_rejects_shell_expansion_and_flags() {
        for code in [
            "$(touch${IFS}/path)",
            "a;command",
            "--help",
            "a'b",
            "a`command`",
        ] {
            let helper = move |_: &Path,
                               _: &[std::ffi::OsString],
                               _: Duration,
                               _: Option<&Path>,
                               _: &[u8],
                               _: &mut dyn FnMut(&[u8])| {
                Ok(std::process::Output {
                    status: std::process::ExitStatus::from_raw(0),
                    stdout: format!("  agmen join {code}\n").into_bytes(),
                    stderr: vec![],
                })
            };
            assert!(
                execute(
                    Path::new("/injected/agmen"),
                    &helper,
                    None,
                    Action::Invite("project1".parse().unwrap())
                )
                .is_err(),
                "{code}"
            );
        }
    }
}
