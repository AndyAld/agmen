//! Task 7: the window a plain launch opens. It connects to agmend for the
//! chosen project and shows the workspace on live data, or says plainly
//! why it cannot: no project, agmend not running, or agmend refusing the
//! project. It never shows the fixture.

use std::path::PathBuf;
use std::rc::Rc;

use agmen_proto::ProjectId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div,
};

use super::Workspace;
use super::components::{Button, ButtonStyle, Color, Label, LabelSize};
use super::tokens::{LAUNCH_STATE_WIDTH, SPACE_10, SPACE_16, TITLE_BAR_HEIGHT};
use crate::appearance::{Appearance, DesktopSettings};
use crate::backend::{AgmendBackend, Backend, BackendError, BackendHandle};
use crate::project::ProjectChoice;

/// Where a live launch connects and what it opens.
#[derive(Debug, Clone)]
pub(crate) struct LiveLaunch {
    /// agmend's socket.
    pub(crate) socket: PathBuf,
    pub(crate) project: ProjectChoice,
    /// agmen's settings file, where a launch that opens remembers its
    /// project; `None` remembers nothing.
    pub(crate) settings: Option<PathBuf>,
}

/// What the window shows.
#[derive(Debug)]
pub(crate) enum LiveState {
    /// Neither the folder nor the settings name a project.
    NoProject,
    /// Review fix 2: the connect runs off the main thread; Retry waits.
    Connecting,
    /// Nothing answers at agmend's socket.
    AgmendDown,
    /// agmend answered and refused, or its socket is not the person's own;
    /// the text says why.
    Refused(String),
    /// The project's workspace on agmend's data.
    Open(Entity<Workspace>),
}

/// The words a state other than [`LiveState::Open`] shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wording {
    pub(crate) heading: String,
    pub(crate) body: String,
    /// A command to run, in the code font.
    pub(crate) command: Option<&'static str>,
    /// A line under the command, such as why the folder's identity was not
    /// used.
    pub(crate) note: Option<String>,
    /// Whether Retry can help.
    pub(crate) retry: bool,
}

impl Wording {
    /// The words for `state` while trying `project`.
    pub(crate) fn of(state: &LiveState, project: &ProjectChoice) -> Self {
        match state {
            LiveState::NoProject => Self {
                heading: "No agmen project here".into(),
                body: "Run agmen join with your invite in the project's folder, then start \
                       agmen-desktop from that folder. After that it opens the last project \
                       it opened."
                    .into(),
                command: Some("agmen join <invite>"),
                note: match project {
                    ProjectChoice::None { why } => why.clone(),
                    ProjectChoice::Project(_) => None,
                },
                retry: false,
            },
            LiveState::Connecting => Self {
                heading: "Connecting to agmend…".into(),
                body: match project {
                    ProjectChoice::Project(id) => format!("Opening {id}."),
                    ProjectChoice::None { .. } => "Opening the project.".into(),
                },
                command: None,
                note: None,
                retry: false,
            },
            LiveState::AgmendDown => Self {
                heading: "agmend isn't running".into(),
                body: "agmen-desktop reads your project's chat through agmend, the agmen \
                       daemon. Start it, then press Retry."
                    .into(),
                command: Some("agmend service install"),
                // shortcut: `agmend service install` is planned (m0-m1 Task
                // 13) but not built; drop this line once it is.
                note: Some("Or run agmend in a terminal.".into()),
                retry: true,
            },
            LiveState::Refused(why) => Self {
                heading: match project {
                    ProjectChoice::Project(id) => format!("agmend can't open {id}"),
                    ProjectChoice::None { .. } => "agmend can't open the project".into(),
                },
                body: why.clone(),
                command: None,
                note: None,
                retry: true,
            },
            // Never drawn: the workspace is.
            LiveState::Open(_) => Self {
                heading: String::new(),
                body: String::new(),
                command: None,
                note: None,
                retry: false,
            },
        }
    }
}

/// The live window's root view. Tests find each state as `live:no-project`,
/// `live:agmend-down` and `live:refused`, its command as `live:command` and
/// its button as `live:retry`.
pub(crate) struct LiveRoot {
    launch: LiveLaunch,
    state: LiveState,
    focus_handle: FocusHandle,
    /// The connect under way, dropped with the window.
    _connecting: Option<Task<()>>,
}

impl LiveRoot {
    /// Tries the launch's project at once.
    pub(crate) fn new(launch: LiveLaunch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut root = Self {
            launch,
            state: LiveState::NoProject,
            focus_handle: cx.focus_handle(),
            _connecting: None,
        };
        root.connect(window, cx);
        root
    }

    /// The state the window shows.
    #[cfg(test)]
    pub(crate) const fn state(&self) -> &LiveState {
        &self.state
    }

    /// The open workspace, once agmend answered.
    #[cfg(test)]
    pub(crate) const fn workspace(&self) -> Option<&Entity<Workspace>> {
        match &self.state {
            LiveState::Open(workspace) => Some(workspace),
            _ => None,
        }
    }

    /// Starts connecting to agmend for the launch's project. The connect
    /// blocks on agmend, which waits on the hub (up to its 30 s request
    /// timeout), so it runs on a thread of its own and the window shows
    /// Connecting meanwhile. A thread rather than gpui's background
    /// executor: a call that blocks this long would hold one of its
    /// workers, and gpui's test scheduler runs background tasks on the
    /// test thread, where the window could never draw Connecting first.
    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ProjectChoice::Project(project) = &self.launch.project else {
            self.state = LiveState::NoProject;
            return;
        };
        let socket = self.launch.socket.clone();
        let project = project.clone();
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("agmend-connect".into())
            .spawn(move || {
                // The window closed meanwhile; the backend drops here.
                drop(done.send(AgmendBackend::connect(&socket, project.as_str())));
            });
        if let Err(err) = spawned {
            self.state = LiveState::Refused(format!("starting the agmend link failed: {err}"));
            return;
        }
        self.state = LiveState::Connecting;
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = result
                .await
                .unwrap_or_else(|_| Err(BackendError::Agmend("the agmend connect stopped".into())));
            // The window closed meanwhile; the backend drops with the result.
            drop(this.update_in(cx, |this, window, cx| this.connected(result, window, cx)));
        });
        self._connecting = Some(task);
    }

    /// Opens the workspace on the connected backend, or shows why not.
    fn connected(
        &mut self,
        result: Result<AgmendBackend, BackendError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._connecting = None;
        cx.notify();
        let ProjectChoice::Project(project) = &self.launch.project else {
            self.state = LiveState::NoProject;
            return;
        };
        let backend = match result {
            Ok(backend) => backend,
            Err(BackendError::NotConnected) => {
                self.state = LiveState::AgmendDown;
                return;
            }
            Err(err) => {
                self.state = LiveState::Refused(err.to_string());
                return;
            }
        };
        let backend: Rc<dyn Backend> = Rc::new(backend);
        let snapshot = backend.snapshot();
        // The chat subscribes to the global when it is built.
        cx.set_global(BackendHandle(backend));
        let workspace = cx.new(|cx| Workspace::live(snapshot, window, cx));
        window.focus(&workspace.focus_handle(cx), cx);
        self.state = LiveState::Open(workspace);
        self.remember(project.clone());
    }

    /// Writes the project that opened as the settings' last project, so a
    /// launch from outside a joined folder opens it again.
    fn remember(&self, project: ProjectId) {
        let Some(path) = &self.launch.settings else {
            return;
        };
        let saved = DesktopSettings::load(path).and_then(|mut settings| {
            settings.last_project = Some(project);
            settings.save(path)
        });
        if let Err(err) = saved {
            // The window is open; only the next launch outside the folder
            // loses the project, so say so and go on.
            eprintln!("agmen-desktop: not remembering the project: {err}");
        }
    }

    /// Retry: tries agmend again, unless a try is under way.
    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, LiveState::Connecting) {
            return;
        }
        self.connect(window, cx);
        cx.notify();
    }

    fn message(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let wording = Wording::of(&self.state, &self.launch.project);
        let selector = match self.state {
            LiveState::NoProject => "live:no-project",
            LiveState::Connecting => "live:connecting",
            LiveState::AgmendDown => "live:agmend-down",
            LiveState::Refused(_) | LiveState::Open(_) => "live:refused",
        };
        div()
            .debug_selector(|| selector.into())
            .w(LAUNCH_STATE_WIDTH)
            .flex()
            .flex_col()
            .items_start()
            .gap(SPACE_10)
            .child(
                Label::new(wording.heading)
                    .size(LabelSize::Title)
                    .weight(FontWeight::SEMIBOLD),
            )
            .child(Label::new(wording.body).wrap())
            .when_some(wording.command, |this, command| {
                this.child(
                    div()
                        .debug_selector(|| "live:command".into())
                        .child(Label::new(command).code().color(Color::Accent)),
                )
            })
            .when_some(wording.note, |this, note| {
                this.child(Label::new(note).wrap().color(Color::Placeholder))
            })
            .when(wording.retry, |this| {
                this.child(
                    Button::new("live:retry", "Retry")
                        .style(ButtonStyle::Filled)
                        .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx))),
                )
            })
    }
}

impl Focusable for LiveRoot {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        match &self.state {
            LiveState::Open(workspace) => workspace.focus_handle(cx),
            _ => self.focus_handle.clone(),
        }
    }
}

impl Render for LiveRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let LiveState::Open(workspace) = &self.state {
            return div()
                .size_full()
                .child(workspace.clone())
                .into_any_element();
        }
        let message = self.message(cx);
        let appearance = cx.global::<Appearance>();
        // Text sizes are rems, as in the workspace.
        window.set_rem_size(appearance.fonts.ui_size);
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(appearance.theme.background)
            .text_color(appearance.theme.text)
            .font_family(appearance.fonts.ui_family.clone())
            // The window's buttons sit here; the title bar is see-through.
            .child(div().h(TITLE_BAR_HEIGHT).flex_none())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(SPACE_16)
                    .child(message),
            )
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "live_root_tests.rs"]
mod tests;
