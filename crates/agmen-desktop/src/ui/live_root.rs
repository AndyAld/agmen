//! Task 7: the window a plain launch opens. It connects to agmend for the
//! chosen project and shows the workspace on live data, or says plainly
//! why it cannot: no project, agmend not running, or agmend refusing the
//! project. It never shows the fixture.

use std::path::PathBuf;
use std::rc::Rc;

use agmen_proto::ProjectId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Task, Window, div,
};

use super::Workspace;
use super::components::{Button, ButtonStyle, CheckRow, Color, DialogFooter, Label, Modal};
use super::tokens::{LAUNCH_STATE_WIDTH, SPACE_10, SPACE_16, TITLE_BAR_HEIGHT};
use crate::appearance::{Appearance, DesktopSettings};
use crate::backend::{AgmendBackend, Backend, BackendError, BackendHandle, HostSocket, HostState};
use crate::bundle::Bundle;
use crate::project::ProjectChoice;
use crate::service::{self, Check, Observation};

#[path = "live_root_project.rs"]
mod project_open;
use project_open::{OpenRecent, RejectedFolder, RemoveRecent};
#[path = "live_root_membership.rs"]
mod membership;
#[path = "live_root_onboarding.rs"]
mod onboarding;

/// Where a live launch connects and what it opens.
#[derive(Debug, Clone)]
pub(crate) struct LiveLaunch {
    /// agmend's socket.
    pub(crate) socket: PathBuf,
    pub(crate) project: ProjectChoice,
    /// Task 8: the checkout the working folder is in, when it named the
    /// project; agmend reads its worktrees and git facts.
    pub(crate) root: Option<PathBuf>,
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
    HostStarting,
    /// Nothing answers at agmend's socket.
    AgmendDown,
    NeedsService(Check),
    /// agmend answered and refused, or its socket is not the person's own;
    /// the text says why.
    Refused(String),
    /// The project's workspace on agmend's data.
    Open(Entity<Workspace>),
}

impl LiveState {
    /// The debug selector tests find this state by.
    const fn selector(&self) -> &'static str {
        match self {
            Self::NoProject => "live:no-project",
            Self::Connecting => "live:connecting",
            Self::HostStarting => "live:host-starting",
            Self::AgmendDown => "live:agmend-down",
            Self::NeedsService(_) => "live:needs-service",
            Self::Refused(_) => "live:refused",
            Self::Open(_) => "live:open",
        }
    }
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
                body: "Open a project folder, or run agmen join with your invite in a folder to join a project."
                    .into(),
                command: Some("agmen join <invite code>"),
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
            LiveState::HostStarting => Self {
                heading: "Starting your hub…".into(),
                body: "Waiting up to 30 seconds for your hub to start.".into(),
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
                note: None,
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
            LiveState::NeedsService(check) => Self {
                heading: check.action().unwrap_or("Check agmend").into(),
                body: match check {
                    Check::TemporaryBundle => "Move to Applications, then relaunch agmen from the moved app before setting up agmend.".into(),
                    Check::Moved { configured } => format!("The service points to a missing executable at {}. Start agmend from this app. Start at login is optional in Settings.", configured.display()),
                    Check::Skewed { running, ours, .. } => format!("agmend version {} is incompatible with this app's version {ours}. Switching replaces the configured service program with this app's agmend.", running.as_deref().unwrap_or("unknown")),
                    Check::Elsewhere { exe } => format!("The service at {} is stopped or unavailable. Its installation owns this same-version service, so agmen will keep its configured path. Repair or start it through that installation, then Retry.", exe.display()),
                    _ => "agmen needs agmend, the agmen background service, to open your projects.".into(),
                },
                command: None,
                note: Some(format!("Log: {}", service::log_path().display())),
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
/// `live:connecting`, `live:agmend-down`, `live:refused` and `live:open`,
/// its command as `live:command` and its button as `live:retry`.
pub(crate) struct LiveRoot {
    launch: LiveLaunch,
    state: LiveState,
    focus_handle: FocusHandle,
    /// The connect under way, dropped with the window.
    _connecting: Option<Task<()>>,
    bundle: Option<Bundle>,
    service_notice: Option<String>,
    service_error: Option<String>,
    cli_install: Option<Entity<super::cli_install::CliInstall>>,
    quit_choice: Option<service::QuitChoice>,
    quit_remember: bool,
    quit_busy: bool,
    quit_requested: bool,
    pub(crate) quit_approved: bool,
    _quitting: Option<Task<()>>,
    recents: Vec<PathBuf>,
    rejected: Option<RejectedFolder>,
    _picking: Option<Task<()>>,
    _resolving: Option<Task<()>>,
    pending_folder: Option<PathBuf>,
    onboarding: onboarding::Onboarding,
}

impl LiveRoot {
    /// Tries the launch's project at once.
    pub(crate) fn new(
        launch: LiveLaunch,
        initial_path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.set_global(HostSocket(launch.socket.clone()));
        let stored = launch
            .settings
            .as_deref()
            .map(DesktopSettings::load)
            .transpose();
        let (recents, error) = match stored {
            Ok(settings) => (settings.unwrap_or_default().recent_projects, None),
            Err(error) => (
                Vec::new(),
                Some(format!("Reading recent projects failed: {error}")),
            ),
        };
        let bundle = std::env::current_exe()
            .ok()
            .and_then(|exe| crate::bundle::locate(&exe));
        let onboarding = onboarding::Onboarding::new(
            bundle.as_ref().map(|bundle| bundle.agmen.clone()),
            window,
            cx,
        );
        let mut root = Self {
            launch,
            state: LiveState::NoProject,
            focus_handle: cx.focus_handle(),
            _connecting: None,
            bundle,
            service_notice: None,
            service_error: error,
            cli_install: super::cli_install::CliInstall::for_app(cx),
            quit_choice: None,
            quit_remember: false,
            quit_busy: false,
            quit_requested: false,
            quit_approved: false,
            _quitting: None,
            recents,
            rejected: None,
            _picking: None,
            _resolving: None,
            pending_folder: None,
            onboarding,
        };
        root.refresh_project_menu(cx);
        root.connect(window, cx);
        if let Some(path) = initial_path {
            root.open_folder(path, window, cx);
        }
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
        let hosting = self.launch.socket.with_file_name("host.toml").exists();
        let startup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        if matches!(self.launch.project, ProjectChoice::None { .. })
            && self.bundle.is_none()
            && !hosting
        {
            self.state = LiveState::NoProject;
            return;
        }
        let socket = self.launch.socket.clone();
        let project = self.launch.project.clone();
        let root = self.launch.root.clone();
        let bundle = self.bundle.clone();
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("agmend-connect".into())
            .spawn(move || {
                // The window closed meanwhile; the backend drops here.
                let connected = match bundle.as_ref() {
                    Some(bundle) => service::start(bundle, &socket, &mut service::run),
                    None => service::check(None, &socket, &mut service::run),
                }
                .map_err(|error| BackendError::Agmend(error.to_string()))
                .and_then(|observed| {
                    Self::connect_observed(
                        &socket,
                        &project,
                        root.as_deref(),
                        bundle.is_some(),
                        hosting.then_some(startup_deadline),
                        observed,
                    )
                });
                drop(done.send(connected));
            });
        if let Err(err) = spawned {
            self.state = LiveState::Refused(format!("starting the agmend link failed: {err}"));
            return;
        }
        self.state = if hosting {
            LiveState::HostStarting
        } else {
            LiveState::Connecting
        };
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = if hosting {
                let timeout = cx
                    .background_executor()
                    .timer(startup_deadline.saturating_duration_since(std::time::Instant::now()));
                futures::pin_mut!(result, timeout);
                match futures::future::select(result, timeout).await {
                    futures::future::Either::Left((result, _)) => result.unwrap_or_else(|_| {
                        Err(BackendError::Agmend("the agmend connect stopped".into()))
                    }),
                    futures::future::Either::Right(_) => Err(BackendError::Agmend(
                        "Your hub did not become ready within 30 seconds.".into(),
                    )),
                }
            } else {
                result.await.unwrap_or_else(|_| {
                    Err(BackendError::Agmend("the agmend connect stopped".into()))
                })
            };
            // The window closed meanwhile; the backend drops with the result.
            drop(this.update_in(cx, |this, window, cx| this.connected(result, window, cx)));
        });
        self._connecting = Some(task);
    }

    /// Connects once agmend's service is checked or started. Hosting waits
    /// up to `hosting` for the hub to answer.
    fn connect_observed(
        socket: &std::path::Path,
        project: &ProjectChoice,
        root: Option<&std::path::Path>,
        bundled: bool,
        hosting: Option<std::time::Instant>,
        mut observed: Observation,
    ) -> Result<(Observation, Option<AgmendBackend>), BackendError> {
        // A service that needs setup cannot start the hub, so its screen
        // comes before the hub wait.
        if !observed.check.can_connect() || (bundled && !observed.running) {
            return Ok((observed, None));
        }
        if let Some(deadline) = hosting {
            return Self::connect_host(socket, project, root, observed, deadline);
        }
        let backend = match project {
            ProjectChoice::Project(project) => {
                match AgmendBackend::connect(socket, project.as_str(), root) {
                    Ok(backend) => Some(backend),
                    Err(BackendError::NotConnected) if bundled => {
                        observed.running = false;
                        if observed.check == Check::Ready {
                            observed.check = Check::Stopped;
                        }
                        None
                    }
                    Err(error) => return Err(error),
                }
            }
            ProjectChoice::None { .. } => None,
        };
        Ok((observed, backend))
    }

    fn connect_host(
        socket: &std::path::Path,
        project: &ProjectChoice,
        root: Option<&std::path::Path>,
        observed: Observation,
        deadline: std::time::Instant,
    ) -> Result<(Observation, Option<AgmendBackend>), BackendError> {
        use std::time::{Duration, Instant};
        let mut last_error = "Your hub did not start within 30 seconds.".to_owned();
        let health = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                BackendError::Agmend(format!("Building hub health client failed: {error}"))
            })?;
        for _ in 0..120 {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match AgmendBackend::host_status_before(
                socket,
                false,
                deadline.min(now + Duration::from_secs(2)),
            ) {
                Ok(status) if status.configured && status.state == HostState::Failed => {
                    return Err(BackendError::Agmend(
                        status
                            .last_error
                            .unwrap_or_else(|| "Your hub failed to start.".into()),
                    ));
                }
                Ok(status) if status.configured && status.state == HostState::Running => {
                    let connected = match project {
                        ProjectChoice::Project(project) => {
                            AgmendBackend::connect_host(socket, project.as_str(), root, deadline)
                                .map(Some)
                        }
                        ProjectChoice::None { .. } => {
                            let url = status
                                .public_url
                                .as_deref()
                                .and_then(|url| reqwest::Url::parse(url).ok())
                                .and_then(|url| url.join("healthz").ok())
                                .ok_or_else(|| {
                                    BackendError::Agmend(
                                        "Host configuration has no valid public URL.".into(),
                                    )
                                })?;
                            health
                                .get(url)
                                .timeout(
                                    Duration::from_secs(2)
                                        .min(deadline.saturating_duration_since(Instant::now())),
                                )
                                .send()
                                .and_then(reqwest::blocking::Response::error_for_status)
                                .map(|_| None)
                                .map_err(|error| {
                                    BackendError::Agmend(format!("Your hub is not ready: {error}"))
                                })
                        }
                    };
                    match connected {
                        Ok(backend) => return Ok((observed, backend)),
                        Err(error) => last_error = error.to_string(),
                    }
                }
                Ok(_) => {}
                Err(error) => last_error = error.to_string(),
            }
            std::thread::sleep(
                Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        Err(BackendError::Agmend(format!(
            "Your hub did not become ready within 30 seconds: {last_error}"
        )))
    }

    /// Opens the workspace on the connected backend, or shows why not.
    fn connected(
        &mut self,
        result: Result<(Observation, Option<AgmendBackend>), BackendError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connected_result(result, window, cx);
        if self.quit_requested {
            self.quit_requested = false;
            self.request_quit(window, cx);
        }
    }

    fn connected_result(
        &mut self,
        result: Result<(Observation, Option<AgmendBackend>), BackendError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._connecting = None;
        cx.notify();
        let (observed, backend) = match result {
            Ok(result) => result,
            Err(BackendError::NotConnected) => {
                self.state = LiveState::AgmendDown;
                return;
            }
            Err(err) => {
                self.state = LiveState::Refused(err.to_string());
                return;
            }
        };
        let check = observed.check;
        self.service_notice = match &check {
            Check::Elsewhere { exe } => Some(format!("This app uses agmend at {}.", exe.display())),
            _ => None,
        };
        if !check.can_connect() || (self.bundle.is_some() && !observed.running) {
            self.state = LiveState::NeedsService(check);
            return;
        }
        let Some(backend) = backend else {
            self.state = LiveState::NoProject;
            return;
        };
        if !backend.readiness().ready
            || (self.bundle.is_some()
                && backend.identity().version.as_deref() != Some(env!("CARGO_PKG_VERSION")))
        {
            self.state = LiveState::Refused(
                "agmend changed while connecting; Retry to check its service".into(),
            );
            return;
        }
        let ProjectChoice::Project(project) = &self.launch.project else {
            self.state = LiveState::NoProject;
            return;
        };
        let backend: Rc<dyn Backend> = Rc::new(backend);
        let snapshot = backend.snapshot();
        // The chat subscribes to the global when it is built.
        cx.set_global(BackendHandle(backend));
        let workspace = cx.new(|cx| Workspace::live(snapshot, window, cx));
        window.focus(&workspace.focus_handle(cx), cx);
        self.state = LiveState::Open(workspace);
        self.remember(project.clone(), cx);
        self.sync_invite_people(cx);
        if let Some((joined, _, notice)) = self.onboarding.join_notice.take()
            && self.launch.project == ProjectChoice::Project(joined)
        {
            self.service_notice = Some(notice);
        }
        if self.onboarding.projects.is_none() && self.onboarding.task.is_none() {
            self.list_memberships(window, cx);
        }
    }

    /// Writes the project that opened as the settings' last project, so a
    /// launch from outside a joined folder opens it again.
    fn remember(&mut self, project: ProjectId, cx: &mut Context<Self>) {
        if let Some(root) = &self.launch.root {
            self.recents.retain(|path| path != root);
            self.recents.insert(0, root.clone());
            self.recents.truncate(10);
        }
        self.refresh_project_menu(cx);
        let Some(path) = &self.launch.settings else {
            return;
        };
        let saved = DesktopSettings::load(path).and_then(|mut settings| {
            settings.last_project = Some(project);
            settings.recent_projects.clone_from(&self.recents);
            settings.save(path)
        });
        if let Err(err) = saved {
            // The window is open; only the next launch outside the folder
            // loses the project, so say so and go on.
            eprintln!("agmen-desktop: not remembering the project: {err}");
            self.service_error = Some(format!("Saving the project failed: {err}"));
        }
    }

    /// Retry: tries agmend again, unless a try is under way.
    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.state, LiveState::Connecting | LiveState::HostStarting) {
            return;
        }
        self.connect(window, cx);
        cx.notify();
    }

    fn restart_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self._connecting.is_some() {
            return;
        }
        let socket = self.launch.socket.clone();
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("host-restart".into())
            .spawn(move || {
                drop(done.send(AgmendBackend::host_status(&socket, true)));
            });
        if let Err(error) = spawned {
            self.state = LiveState::Refused(format!("Restarting your hub failed: {error}"));
            cx.notify();
            return;
        }
        self.state = LiveState::HostStarting;
        self._connecting = Some(cx.spawn_in(window, async move |this, cx| {
            let result = result.await;
            drop(this.update_in(cx, |this, window, cx| {
                this._connecting = None;
                match result {
                    Ok(Ok(_)) => this.connect(window, cx),
                    Ok(Err(error)) => this.state = LiveState::Refused(error.to_string()),
                    Err(error) => {
                        this.state =
                            LiveState::Refused(format!("Restarting your hub stopped: {error}"))
                    }
                }
                cx.notify();
            }));
        }));
        cx.notify();
    }

    fn setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bundle) = self.bundle.clone() else {
            return;
        };
        if matches!(self.state, LiveState::NeedsService(Check::TemporaryBundle)) {
            cx.open_url("file:///Applications");
            return;
        }
        let previous = match &self.state {
            LiveState::NeedsService(check) if check.action().is_some() => check.clone(),
            _ => return,
        };
        let switch = matches!(previous, Check::Skewed { .. });
        let socket = self.launch.socket.clone();
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("agmend-setup".into())
            .spawn(move || {
                let result = if switch {
                    service::install(&bundle, &mut service::run)
                } else {
                    service::start(&bundle, &socket, &mut service::run).map(|_| ())
                };
                drop(done.send(result));
            });
        if let Err(error) = spawned {
            self.service_error = Some(format!("Starting service setup failed: {error}"));
            cx.notify();
            return;
        }
        self.service_error = None;
        self.state = LiveState::Connecting;
        self._connecting = Some(cx.spawn_in(window, async move |this, cx| {
            let result = result.await;
            drop(this.update_in(cx, |this, window, cx| {
                this._connecting = None;
                match result {
                    Ok(Ok(())) => this.connect(window, cx),
                    result => {
                        this.service_error = Some(match result {
                            Ok(Err(error)) => error.to_string(),
                            Err(error) => format!("Service setup stopped: {error}"),
                            Ok(Ok(())) => String::new(),
                        });
                        this.state = LiveState::NeedsService(previous);
                        if this.quit_requested {
                            this.quit_requested = false;
                            this.request_quit(window, cx);
                        }
                    }
                }
                cx.notify();
            }));
        }));
        cx.notify();
    }

    /// All user quit triggers enter here before GPUI starts shutting down.
    pub(crate) fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_busy || self.quit_choice.is_some() {
            return;
        }
        if matches!(self.state, LiveState::Connecting | LiveState::HostStarting) {
            self.quit_requested = true;
            return;
        }
        let Some(bundle) = self.bundle.clone() else {
            self.quit_approved = true;
            cx.quit();
            return;
        };
        let socket = self.launch.socket.clone();
        let settings = self.launch.settings.clone();
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("agmend-quit-check".into())
            .spawn(move || {
                let decision = (|| -> anyhow::Result<_> {
                    let owned = service::owns_running(&bundle, &socket, &mut service::run)?;
                    let remembered = settings
                        .as_ref()
                        .map(|path| DesktopSettings::load(path))
                        .transpose()?
                        .and_then(|settings| settings.quit_keep_running);
                    Ok(service::quit_choice(owned, remembered))
                })();
                drop(done.send(decision));
            });
        if let Err(error) = spawned {
            self.service_error = Some(format!("Checking agmend before quitting: {error}"));
            cx.notify();
            return;
        }
        self.quit_busy = true;
        self._quitting = Some(cx.spawn_in(window, async move |this, cx| {
            let decision = result.await;
            drop(this.update_in(cx, |this, window, cx| {
                this.quit_busy = false;
                this._quitting = None;
                match decision {
                    Ok(Ok(service::QuitChoice::Ask)) => {
                        this.quit_choice = Some(service::QuitChoice::Ask);
                        window.focus(&this.focus_handle, cx);
                    }
                    Ok(Ok(choice)) => this.choose_quit(choice, window, cx),
                    error => {
                        // An unreadable identity is never permission to stop a daemon.
                        eprintln!("agmen-desktop: keeping agmend on quit: {error:?}");
                        this.choose_quit(service::QuitChoice::Keep, window, cx);
                    }
                }
                cx.notify();
            }));
        }));
    }

    fn cancel_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_busy || self.quit_choice != Some(service::QuitChoice::Ask) {
            return;
        }
        self.quit_choice = None;
        self.quit_remember = false;
        self.quit_requested = false;
        window.focus(&self.focus_handle(cx), cx);
        cx.notify();
    }

    fn choose_quit(
        &mut self,
        choice: service::QuitChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_busy {
            return;
        }
        if self.quit_remember
            && let Some(path) = &self.launch.settings
        {
            let saved = DesktopSettings::load(path).and_then(|mut settings| {
                settings.quit_keep_running = Some(choice == service::QuitChoice::Keep);
                settings.save(path)
            });
            if let Err(error) = saved {
                self.service_error = Some(format!("Saving the quit choice: {error}"));
                cx.notify();
                return;
            }
        }
        if choice == service::QuitChoice::Keep {
            self.quit_approved = true;
            cx.quit();
            return;
        }
        let Some(bundle) = self.bundle.clone() else {
            return;
        };
        let socket = self.launch.socket.clone();
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("agmend-shutdown".into())
            .spawn(move || {
                let stopped = service::finish_quit(choice, &mut |bound| {
                    let deadline = std::time::Instant::now() + bound;
                    if service::owns_running(&bundle, &socket, &mut service::run)? {
                        let remaining = deadline
                            .checked_duration_since(std::time::Instant::now())
                            .ok_or_else(|| anyhow::anyhow!("shutdown exceeded 15 s"))?;
                        service::shutdown(&bundle, &socket, remaining)?;
                    }
                    Ok(())
                });
                drop(done.send(stopped));
            });
        if let Err(error) = spawned {
            self.service_error = Some(format!("Starting shutdown: {error}"));
            cx.notify();
            return;
        }
        self.quit_busy = true;
        self.quit_choice = Some(service::QuitChoice::Stop);
        self._quitting = Some(cx.spawn_in(window, async move |this, cx| {
            let deadline = cx
                .background_executor()
                .timer(std::time::Duration::from_secs(15));
            futures::pin_mut!(deadline);
            match futures::future::select(result, deadline).await {
                futures::future::Either::Left((Ok(Ok(())), _)) => {}
                futures::future::Either::Left((error, _)) => {
                    eprintln!("agmen-desktop: shutdown: {error:?}")
                }
                futures::future::Either::Right(_) => {
                    eprintln!("agmen-desktop: shutdown exceeded 15 s")
                }
            }
            drop(this.update(cx, |this, cx| {
                this.quit_approved = true;
                cx.quit();
            }));
        }));
        cx.notify();
    }

    fn quit_dialog(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        self.quit_choice?;
        let busy = self.quit_busy;
        Some(
            Modal::medium("quit:dialog")
                .title(Label::title("Keep agents running in the background?"))
                .child(Label::new("Your agents and the listener keep working and receiving messages after agmen closes.").wrap())
                .error(self.service_error.clone())
                .child(
                    div()
                        .id("quit:remember")
                        .debug_selector(|| "quit:remember".into())
                        .when(!busy, |this| this.cursor_pointer())
                        .child(CheckRow::new("Don't ask again", self.quit_remember))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.quit_busy {
                                this.quit_remember = !this.quit_remember;
                                cx.notify();
                            }
                        })),
                )
                .footer(
                    DialogFooter::new(
                        Button::new("quit:everything", if busy { "Stopping agmend…" } else { "Quit Everything" })
                            .on_click(cx.listener(|this, _, window, cx| this.choose_quit(service::QuitChoice::Stop, window, cx))),
                    )
                        .destructive(true)
                        .cancel(Button::new("quit:cancel", "Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.cancel_quit(window, cx))))
                        .secondary(Button::new("quit:keep", "Keep Running")
                            .style(ButtonStyle::Primary)
                            .on_click(cx.listener(|this, _, window, cx| this.choose_quit(service::QuitChoice::Keep, window, cx)))),
                ),
        )
    }

    fn message(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let wording = Wording::of(&self.state, &self.launch.project);
        let selector = self.state.selector();
        div()
            .debug_selector(|| selector.into())
            .w(LAUNCH_STATE_WIDTH)
            .flex()
            .flex_col()
            .items_start()
            .gap(SPACE_10)
            .child(
                Label::title(wording.heading),
            )
            .child(Label::new(wording.body).wrap())
            .when(matches!(self.state, LiveState::NoProject | LiveState::Refused(_)), |this| {
                this.child(Button::new("live:open-project", "Open Project…")
                    .style(ButtonStyle::Primary)
                    .on_click(cx.listener(|this, _, window, cx| this.prompt_project(window, cx))))
            })
            .when(matches!(self.state, LiveState::NoProject), |this| this.child(self.recent_list(cx)))
            .when_some(self.bundle.as_ref().filter(|_| matches!(self.state, LiveState::NeedsService(Check::Missing | Check::Stopped | Check::Moved { .. } | Check::Skewed { .. }))), |this, bundle| {
                let switching = matches!(self.state, LiveState::NeedsService(Check::Skewed { .. }));
                let command = if switching { service::command(bundle) } else { format!("\"{}\"", bundle.agmend.display()) };
                this.child(Label::new(if switching { format!("Switching updates the login service to {}. macOS may show a Background Items Added notice.", bundle.app.display()) } else { "agmen starts its bundled agmend. Start at login is optional in Settings.".into() }).wrap())
                    .child(div().debug_selector(|| "live:command".into()).child(Label::new(command.clone()).code().wrap()))
                    .child(Button::new("live:copy-service-command", "Copy command").on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(command.clone()))))
            })
            .when_some(self.service_error.clone(), |this, error| this.child(Label::new(error).wrap().color(Color::Error)))
            .when_some(self.service_notice.clone(), |this, notice| this.child(Label::new(notice).wrap()))
            .when_some(match &self.state { LiveState::NeedsService(check) => check.action(), _ => None }, |this, action| {
                this.child(Button::new("live:setup-service", action).style(ButtonStyle::Primary).on_click(cx.listener(|this, _, window, cx| this.setup(window, cx))))
            })
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
            .when(matches!(self.state, LiveState::Refused(_)) && self.launch.socket.with_file_name("host.toml").exists(), |this| {
                this.child(Button::new("live:restart-host", "Restart hub")
                    .style(ButtonStyle::Primary)
                    .on_click(cx.listener(|this, _, window, cx| this.restart_host(window, cx))))
            })
            .when_some(self.cli_install.clone(), |this, view| this.child(view))
    }
}

impl Focusable for LiveRoot {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        if self.quit_choice.is_some() {
            return self.focus_handle.clone();
        }
        match &self.state {
            LiveState::Open(workspace) => workspace.focus_handle(cx),
            _ => self.focus_handle.clone(),
        }
    }
}

/// Routes a live window's quit triggers (Cmd+Q, the Quit menu item and
/// closing the window) through `request_quit`.
pub(crate) fn install_quit_handlers(
    root: &Entity<LiveRoot>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    let weak = root.downgrade();
    window.on_window_should_close(cx, move |window, cx| {
        weak.update(cx, |root, cx| {
            if root.quit_approved {
                return true;
            }
            root.request_quit(window, cx);
            false
        })
        .unwrap_or(true)
    });
    // The Quit menu item when nothing in the window has focus.
    let weak = root.downgrade();
    let handle = window.window_handle();
    cx.on_action(move |_: &crate::keymap::Quit, cx| {
        drop(handle.update(cx, |_, window, cx| {
            drop(weak.update(cx, |root, cx| root.request_quit(window, cx)));
        }));
    });
}

impl LiveRoot {
    fn shell(&self, cx: &mut Context<Self>) -> gpui::Div {
        let appearance = cx.global::<Appearance>();
        div()
            .track_focus(&self.focus_handle)
            .on_key_down(super::focus_tab)
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.quit_choice == Some(service::QuitChoice::Ask)
                    && event.keystroke.key == "enter"
                {
                    window.prevent_default();
                    this.choose_quit(service::QuitChoice::Keep, window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.quit_choice == Some(service::QuitChoice::Ask)
                    && event.keystroke.key == "escape"
                {
                    this.cancel_quit(window, cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .relative()
            .bg(appearance.theme.background)
            .text_color(appearance.theme.text)
            .font_family(appearance.fonts.ui_family.clone())
            .on_action(
                cx.listener(|this, _: &super::title_bar::InvitePeople, window, cx| {
                    if this.can_invite()
                        && let ProjectChoice::Project(project) = &this.launch.project
                    {
                        this.start_membership(
                            membership::Action::Invite(project.clone()),
                            window,
                            cx,
                        );
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &crate::keymap::NewAgent, _, cx| {
                if let LiveState::Open(workspace) = &this.state {
                    workspace.update(cx, |workspace, cx| workspace.open_new_agent(cx));
                }
            }))
            .on_action(
                cx.listener(|this, _: &crate::keymap::OpenProject, window, cx| {
                    this.prompt_project(window, cx)
                }),
            )
            // Cmd+Q dispatches here while this window is being updated, where
            // the app-level handler cannot reach it.
            .on_action(cx.listener(|this, _: &crate::keymap::Quit, window, cx| {
                this.request_quit(window, cx)
            }))
            .on_action(cx.listener(|this, action: &OpenRecent, window, cx| {
                this.open_folder(action.0.clone(), window, cx)
            }))
            .on_action(
                cx.listener(|this, action: &RemoveRecent, _, cx| this.remove_recent(&action.0, cx)),
            )
    }
}

impl Render for LiveRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dialog = self.quit_dialog(cx);
        window.set_rem_size(cx.global::<Appearance>().fonts.ui_size);
        let shell = self.shell(cx);
        let host_failure = matches!(self.state, LiveState::Refused(_))
            && self.launch.socket.with_file_name("host.toml").exists();
        if self.rejected.is_some()
            && !matches!(self.state, LiveState::HostStarting)
            && !host_failure
        {
            let message = self.rejected_message(cx);
            return shell
                .when_some(
                    match &self.state {
                        LiveState::Open(workspace) => Some(workspace.clone()),
                        _ => None,
                    },
                    |this, workspace| this.child(workspace),
                )
                .child(message)
                .children(dialog)
                .into_any_element();
        }
        if let LiveState::Open(workspace) = &self.state {
            let invites = self.invite_controls(cx);
            return shell
                .debug_selector(|| self.state.selector().into())
                .size_full()
                .flex()
                .flex_col()
                .when_some(self.service_notice.clone(), |this, notice| {
                    this.child(Label::new(notice).color(Color::Placeholder))
                })
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
                .child(workspace.clone())
                .child(invites)
                .children(dialog)
                .into_any_element();
        }
        let message = self.message(cx);
        shell
            .size_full()
            .flex()
            .flex_col()
            // The window's buttons sit here; the title bar is see-through.
            .child(div().h(TITLE_BAR_HEIGHT).flex_none())
            .when(self._resolving.is_some(), |this| {
                this.child(
                    div()
                        .debug_selector(|| "live:checking-folder".into())
                        .child(Label::new("Checking project folder…")),
                )
            })
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(SPACE_16)
                    .child(message),
            )
            .children(dialog)
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "live_root_tests.rs"]
mod tests;
