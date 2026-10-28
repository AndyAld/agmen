//! Folder opens and recent projects for the single live window.

use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Action, Context, Focusable as _, InteractiveElement as _, IntoElement, Menu, MenuItem,
    ParentElement as _, PathPromptOptions, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};

use super::{LiveRoot, LiveState, ProjectChoice};
use crate::appearance::DesktopSettings;
use crate::backend::BackendHandle;
use crate::keymap::{OpenProject, Quit};
use crate::ui::components::{Button, ButtonStyle, Color, Label, LabelSize, Modal};
use crate::ui::tokens::{LAUNCH_STATE_WIDTH, SPACE_10};

#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = menu, no_json)]
pub(super) struct OpenRecent(pub(crate) PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Action)]
#[action(namespace = menu, no_json)]
pub(super) struct RemoveRecent(pub(crate) PathBuf);

#[derive(Debug)]
pub(super) struct RejectedFolder {
    pub(super) folder: PathBuf,
    why: Option<String>,
    pub(super) is_git: bool,
}

impl LiveRoot {
    /// Every explicit open, including launch arguments and Dock file URLs,
    /// comes here. A rejected folder leaves the current backend alive.
    pub(crate) fn open_folder(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.onboarding.change_folder(Some(&folder));
        self.sync_invite_people(cx);
        // One resolver and one pending folder bound both work and memory.
        // A newer selection replaces the pending one and wins when git returns.
        if self._resolving.is_some() {
            self.pending_folder = Some(folder);
            return;
        }
        let checking = folder.clone();
        let login_path = self.onboarding.login_path.clone();
        let login_revision = self.onboarding.login_revision;
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("project-folder".into())
            .spawn(move || {
                let choice = crate::project::choose_folder(&checking);
                let git = crate::service::run_with_input(
                    Path::new("git"),
                    &["rev-parse".into(), "--show-toplevel".into()],
                    std::time::Duration::from_secs(5),
                    Some(&checking),
                    &[],
                    &mut |_| {},
                )
                .is_ok_and(|output| output.status.success());
                let login = login_path
                    .as_deref()
                    .map(agmen_client::Login::read)
                    .transpose();
                drop(done.send((choice, git, login)));
            });
        if let Err(error) = spawned {
            self.service_error = Some(format!("Checking the project folder failed: {error}"));
            cx.notify();
            return;
        }
        self._resolving = Some(cx.spawn_in(window, async move |this, cx| {
            let result = result.await;
            drop(this.update_in(cx, |this, window, cx| {
                this._resolving = None;
                if let Some(pending) = this.pending_folder.take() {
                    this.open_folder(pending, window, cx);
                    return;
                }
                cx.notify();
                let ((project, root), is_git, login) = match result {
                    Ok(choice) => choice,
                    Err(error) => {
                        this.service_error =
                            Some(format!("Checking the project folder stopped: {error}"));
                        return;
                    }
                };
                this.apply_saved_login(login_revision, login.map(Option::flatten), window, cx);
                if let ProjectChoice::None { why } = project {
                    this.rejected = Some(RejectedFolder {
                        folder,
                        why,
                        is_git,
                    });
                    let name = this
                        .rejected
                        .as_ref()
                        .and_then(|f| f.folder.file_name())
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    this.onboarding
                        .project_name
                        .update(cx, |input, cx| input.set_text(&name, window, cx));
                    this.onboarding
                        .tenant
                        .update(cx, |input, cx| input.set_text("", window, cx));
                    this.onboarding.projects = None;
                    if is_git {
                        this.list_memberships(window, cx);
                    }
                    window.focus(&this.focus_handle, cx);
                    return;
                }
                this.rejected = None;
                // Drop the old completion and backend before connecting anew.
                this._connecting = None;
                this.state = LiveState::NoProject;
                if cx.try_global::<BackendHandle>().is_some() {
                    drop(cx.remove_global::<BackendHandle>());
                }
                this.service_error = None;
                this.service_notice = None;
                this.launch.project = project;
                this.launch.root = root;
                this.connect(window, cx);
            }));
        }));
        cx.notify();
    }

    pub(super) fn apply_saved_login(
        &mut self,
        revision: u64,
        login: Result<Option<agmen_client::Login>, agmen_client::LoginError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if revision != self.onboarding.login_revision {
            return;
        }
        match login {
            Ok(login) => {
                self.onboarding.signed_in = login.is_some();
                if let Some(login) = login {
                    self.onboarding.hub.update(cx, |input, cx| {
                        input.set_text(login.hub_url.as_str(), window, cx)
                    });
                }
            }
            Err(error) => {
                self.onboarding.signed_in = false;
                self.service_error = Some(format!("Reading saved sign-in failed: {error}"));
            }
        }
    }

    pub(super) fn prompt_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._picking.is_some() {
            return;
        }
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Project".into()),
        });
        self._picking = Some(cx.spawn_in(window, async move |this, cx| {
            let selected = selected.await;
            drop(this.update_in(cx, |this, window, cx| {
                this._picking = None;
                match selected {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            this.open_folder(path, window, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        this.service_error =
                            Some(format!("Opening the folder picker failed: {error}"))
                    }
                    Err(error) => {
                        this.service_error = Some(format!("The folder picker stopped: {error}"))
                    }
                }
                cx.notify();
            }));
        }));
    }

    pub(super) fn refresh_project_menu(&self, cx: &gpui::App) {
        cx.set_menus(project_menus(&self.recents));
    }

    pub(super) fn remove_recent(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.recents.retain(|recent| recent != path);
        if let Some(settings) = &self.launch.settings {
            let saved = DesktopSettings::load(settings).and_then(|mut stored| {
                stored.recent_projects.clone_from(&self.recents);
                stored.save(settings)
            });
            if let Err(error) = saved {
                self.service_error = Some(format!("Removing the recent project failed: {error}"));
            }
        }
        self.refresh_project_menu(cx);
        cx.notify();
    }

    pub(super) fn recent_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .id("live:recents")
            .flex()
            .flex_col()
            .max_h(px(300.))
            .overflow_y_scroll()
            .gap(SPACE_10)
            .when(!self.recents.is_empty(), |this| {
                this.child(Label::new("Open Recent").size(LabelSize::Small))
            })
            .children(
                self.recents
                    .iter()
                    .take(10)
                    .enumerate()
                    .map(|(index, path)| {
                        let missing = !path.is_dir();
                        let open = path.clone();
                        let remove = path.clone();
                        let selector = format!(
                            "live:recent-{}:{index}",
                            if missing { "missing" } else { "project" }
                        );
                        div()
                            .debug_selector(move || selector)
                            .flex()
                            .flex_col()
                            .gap(SPACE_10)
                            .child(
                                Button::new(
                                    format!("live:open-recent:{index}"),
                                    format!(
                                        "{}{}",
                                        path.display(),
                                        if missing { " (missing)" } else { "" }
                                    ),
                                )
                                .disabled(missing)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.open_folder(open.clone(), window, cx)
                                    },
                                )),
                            )
                            .when(missing, |this| {
                                this.child(
                                    Button::new(
                                        format!("live:remove-recent:{index}"),
                                        "Remove from list",
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.remove_recent(&remove, cx),
                                    )),
                                )
                            })
                    }),
            )
    }

    fn dismiss_rejected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.onboarding.change_folder(None);
        self.rejected = None;
        self.sync_invite_people(cx);
        self.list_memberships(window, cx);
        window.focus(&self.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn rejected_message(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (folder, why) = self.rejected.as_ref().map_or_else(
            || (String::new(), None),
            |rejected| (rejected.folder.display().to_string(), rejected.why.clone()),
        );
        Modal::new("live:not-a-project", LAUNCH_STATE_WIDTH, px(100.))
            .on_dismiss(cx.listener(|this, _, window, cx| this.dismiss_rejected(window, cx)))
            .child(Label::new("Open Project").size(LabelSize::Title))
            .child(Label::new(folder).wrap())
            .when(
                self.rejected.as_ref().is_some_and(|folder| !folder.is_git),
                |this| {
                    this.child(
                        div()
                            .debug_selector(|| "live:not-git".into())
                            .child(Label::new("This folder is not a git repository.").wrap()),
                    )
                },
            )
            .when(
                self.rejected.as_ref().is_some_and(|folder| folder.is_git),
                |this| this.child(self.onboarding_controls(cx)),
            )
            .when_some(self.service_error.clone(), |this, error| {
                this.child(
                    div()
                        .debug_selector(|| "live:project-error".into())
                        .child(Label::new(error).wrap().color(Color::Error)),
                )
            })
            .when(self._resolving.is_some(), |this| {
                this.child(
                    div()
                        .debug_selector(|| "live:checking-folder".into())
                        .child(Label::new("Checking project folder…")),
                )
            })
            .when_some(why, |this, why| {
                this.child(Label::new(why).wrap().color(Color::Placeholder))
            })
            .child(
                Button::new("live:choose-another", "Choose Another Folder")
                    .style(ButtonStyle::Primary)
                    .on_click(cx.listener(|this, _, window, cx| this.prompt_project(window, cx))),
            )
            .child(
                Button::new(
                    "live:keep-project",
                    if matches!(self.state, LiveState::Open(_)) {
                        "Keep Current Project"
                    } else {
                        "Close"
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| this.dismiss_rejected(window, cx))),
            )
    }
}

pub(super) fn project_menus(recents: &[PathBuf]) -> Vec<Menu> {
    let recent_items = recents.iter().take(10).flat_map(|path| {
        let label = path.display().to_string();
        if path.is_dir() {
            vec![MenuItem::action(label, OpenRecent(path.clone()))]
        } else {
            vec![
                MenuItem::action(format!("{label} (missing)"), OpenRecent(path.clone()))
                    .disabled(true),
                MenuItem::action("Remove from list", RemoveRecent(path.clone())),
            ]
        }
    });
    vec![
        Menu::new("agmen").items([MenuItem::action("Quit agmen", Quit)]),
        Menu::new("File").items([
            MenuItem::action("Open Project…", OpenProject),
            MenuItem::submenu(Menu::new("Open Recent").items(recent_items)),
        ]),
        Menu::new("Project").items([MenuItem::action("New Agent…", crate::keymap::NewAgent)]),
    ]
}
