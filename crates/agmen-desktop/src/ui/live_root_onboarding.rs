//! In-app sign-in and join, using the shared bounded bundled-helper runner.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Styled as _, Task, Window, div,
};

use super::LiveRoot;
use crate::ui::components::{Button, ButtonStyle, Label};
use crate::ui::text_input::{TextInput, TextInputEvent};

pub(super) type Helper = dyn Fn(
        &Path,
        &[OsString],
        Duration,
        Option<&Path>,
        &[u8],
        &mut dyn FnMut(&[u8]),
    ) -> anyhow::Result<Output>
    + Send
    + Sync;

pub(super) struct Onboarding {
    pub(super) program: Option<PathBuf>,
    pub(super) signed_in: bool,
    pub(super) hub: Entity<TextInput>,
    pub(super) invite: Entity<TextInput>,
    pub(super) login_path: Option<PathBuf>,
    pub(super) generation: u64,
    pub(super) login_revision: u64,
    pub(super) helper: Arc<Helper>,
    pub(super) task: Option<Task<()>>,
    pub(super) device: Option<(String, String)>,
    pub(super) projects: Option<Vec<super::membership::Membership>>,
    pub(super) selected: Option<agmen_proto::ProjectId>,
    pub(super) project_name: Entity<TextInput>,
    pub(super) tenant: Entity<TextInput>,
    pub(super) invite_code: Option<String>,
    pub(super) join_notice: Option<(agmen_proto::ProjectId, PathBuf, String)>,
    _submits: [gpui::Subscription; 2],
}

impl Onboarding {
    pub(super) fn new(
        program: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<LiveRoot>,
    ) -> Self {
        let hub = cx.new(|cx| TextInput::new(cx).placeholder("Hub address"));
        hub.update(cx, |input, cx| {
            input.set_text(&std::env::var("AGMEN_HUB").unwrap_or_default(), window, cx)
        });
        let invite = cx.new(|cx| TextInput::new(cx).placeholder("Invite code"));
        let hub_submit = cx.subscribe_in(&hub, window, |root, _, event, window, cx| {
            let TextInputEvent::Submit(text) = event;
            root.onboarding
                .hub
                .update(cx, |input, cx| input.set_text(text, window, cx));
            root.start_onboarding(false, window, cx);
        });
        let invite_submit = cx.subscribe_in(&invite, window, |root, _, event, window, cx| {
            let TextInputEvent::Submit(text) = event;
            root.onboarding
                .invite
                .update(cx, |input, cx| input.set_text(text, window, cx));
            root.start_onboarding(true, window, cx);
        });
        Self {
            program,
            signed_in: false,
            hub,
            invite,
            // Tests inject a temporary login file and never inspect the user's config.
            login_path: if cfg!(test) {
                None
            } else {
                agmen_client::login_file().ok()
            },
            generation: 0,
            login_revision: 0,
            helper: Arc::new(crate::service::run_with_input),
            task: None,
            device: None,
            projects: None,
            selected: None,
            project_name: cx.new(|cx| TextInput::new(cx).placeholder("Project name")),
            tenant: cx.new(|cx| TextInput::new(cx).placeholder("Tenant id (optional)")),
            invite_code: None,
            join_notice: None,
            _submits: [hub_submit, invite_submit],
        }
    }
    pub(super) fn change_folder(&mut self, folder: Option<&Path>) {
        self.generation = self.generation.wrapping_add(1);
        self.device = None;
        self.invite_code = None;
        self.selected = None;
        if self
            .join_notice
            .as_ref()
            .is_none_or(|(_, target, _)| Some(target.as_path()) != folder)
        {
            self.join_notice = None;
            self.projects = None;
        }
    }

    fn request(&self, join: bool, cx: &gpui::App) -> anyhow::Result<(Vec<OsString>, Vec<u8>)> {
        if join {
            let code = self.invite.read(cx).text().trim();
            anyhow::ensure!(
                !code.is_empty() && code.len() <= 8192 && !code.chars().any(char::is_whitespace),
                "Enter one invite code (at most 8 KiB)."
            );
            Ok((
                vec!["join".into(), "--stdin".into()],
                code.as_bytes().to_vec(),
            ))
        } else {
            let hub = self.hub.read(cx).text().trim();
            anyhow::ensure!(
                reqwest::Url::parse(hub)
                    .ok()
                    .is_some_and(|url| agmen_client::check_hub_url(&url).is_ok()),
                "Enter a valid hub address: HTTPS, or HTTP on localhost."
            );
            Ok((vec!["login".into(), "--hub".into(), hub.into()], Vec::new()))
        }
    }
}

enum Event {
    Device(String, String),
    Done(anyhow::Result<Output>),
}

/// Parse only the CLI's public device prompt, never its tokens or other output.
fn device_prompt(bytes: &[u8]) -> Option<(String, String)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let code = text
        .lines()
        .find_map(|line| line.strip_prefix("Your code is "))?;
    let url = text.lines().find_map(|line| {
        line.strip_prefix("Open ")?
            .split_once(" to confirm it.")
            .map(|(url, _)| url)
    })?;
    let parsed = reqwest::Url::parse(url).ok()?;
    agmen_client::check_hub_url(&parsed).ok()?;
    Some((code.to_owned(), url.to_owned()))
}

impl LiveRoot {
    pub(super) fn onboarding_controls(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let busy = self.onboarding.task.is_some();
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::tokens::SPACE_10)
            .when(!self.onboarding.signed_in, |this| {
                this.child(Label::new("Sign in to join this folder."))
                    .child(Label::new("Hub address"))
                    .child(self.onboarding.hub.clone())
                    .child(
                        Button::new("live:sign-in", "Sign in")
                            .style(ButtonStyle::Primary)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_onboarding(false, window, cx)
                            })),
                    )
            })
            .when(self.onboarding.signed_in, |this| {
                this.child(self.project_controls(cx))
                    .child(Label::new("Have an invite code?"))
                    .child(Label::new("Invite code"))
                    .child(
                        div()
                            .debug_selector(|| "live:invite-code".into())
                            .child(self.onboarding.invite.clone()),
                    )
                    .child(
                        Button::new("live:join", "Join")
                            .style(ButtonStyle::Primary)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_onboarding(true, window, cx)
                            })),
                    )
            })
            .when_some(self.onboarding.device.clone(), |this, (code, url)| {
                this.child(
                    div()
                        .debug_selector(|| "live:device-code".into())
                        .child(Label::new(format!("Your code is {code}"))),
                )
                .child(Label::new(url.clone()).wrap())
                .child(
                    Button::new("live:verification-browser", "Open in Browser")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
            })
            .when(busy, |this| {
                this.child(
                    Label::new("Waiting for the bundled agmen helper (up to 120 seconds)…").wrap(),
                )
            })
    }

    pub(super) fn start_onboarding(
        &mut self,
        join: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.onboarding.task.is_some() {
            return;
        }
        let Some(rejected) = &self.rejected else {
            return;
        };
        let Some(program) = self.onboarding.program.clone() else {
            self.service_error = Some("The bundled agmen helper is missing. Install agmen.app from https://getagmen.ai/download/mac".into());
            cx.notify();
            return;
        };
        let folder = rejected.folder.clone();
        let (args, input) = match self.onboarding.request(join, cx) {
            Ok(request) => request,
            Err(error) => {
                self.service_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.service_error = None;
        self.onboarding.device = None;
        let helper = self.onboarding.helper.clone();
        let working = folder.clone();
        let generation = self.onboarding.generation;
        let (mut send, mut receive) = futures::channel::mpsc::channel(4);
        let spawned = std::thread::Builder::new()
            .name("project-onboarding".into())
            .spawn(move || {
                let mut shown = false;
                let result = helper(
                    &program,
                    &args,
                    Duration::from_secs(120),
                    Some(&working),
                    &input,
                    &mut |bytes| {
                        if !join
                            && !shown
                            && let Some((code, url)) = device_prompt(bytes)
                        {
                            shown = true;
                            drop(send.try_send(Event::Device(code, url)));
                        }
                    },
                ).and_then(|output| {
                    if join && output.status.success() {
                        anyhow::ensure!(matches!(crate::project::choose_folder(&working).0, crate::project::ProjectChoice::Project(_)),
                            "agmen join did not create a trusted project identity in this folder; check the folder and try again");
                    }
                    Ok(output)
                });
                // At most one prompt and one completion fit the four-event channel.
                drop(send.try_send(Event::Done(result)));
            });
        if let Err(error) = spawned {
            self.service_error = Some(format!("Starting bundled agmen failed: {error}"));
            cx.notify();
            return;
        }
        self.onboarding.task = Some(cx.spawn_in(window, async move |this, cx| {
            for _ in 0..2 {
                let Some(event) = receive.next().await else {
                    break;
                };
                let done = matches!(event, Event::Done(_));
                drop(this.update_in(cx, |this, window, cx| {
                    if done {
                        this.onboarding.task = None;
                        cx.notify();
                    }
                    // Login belongs to the session; only joins belong to a folder.
                    if !join && let Event::Done(result) = event {
                        this.finish_onboarding(false, &folder, result, window, cx);
                        cx.notify();
                        return;
                    }
                    if this.onboarding.generation != generation
                        || this
                            .rejected
                            .as_ref()
                            .is_none_or(|rejected| rejected.folder != folder)
                    {
                        if done {
                            this.list_memberships(window, cx);
                        }
                        return;
                    }
                    match event {
                        Event::Device(code, url) => this.onboarding.device = Some((code, url)),
                        Event::Done(result) => {
                            this.finish_onboarding(join, &folder, result, window, cx)
                        }
                    }
                    cx.notify();
                }));
                if done {
                    break;
                }
            }
        }));
        cx.notify();
    }
    fn finish_onboarding(
        &mut self,
        join: bool,
        folder: &Path,
        result: anyhow::Result<Output>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(output) if output.status.success() => {
                self.onboarding.device = None;
                if join {
                    self.onboarding
                        .invite
                        .update(cx, |input, cx| input.set_text("", window, cx));
                    self.open_folder(folder.to_owned(), window, cx);
                } else {
                    self.onboarding.login_revision = self.onboarding.login_revision.wrapping_add(1);
                    self.onboarding.signed_in = true;
                    self.list_memberships(window, cx);
                }
            }
            result => {
                self.onboarding.device = None;
                self.service_error = Some(match result {
                    Ok(output) => {
                        if join && output.status.code() == Some(3) {
                            self.onboarding.login_revision =
                                self.onboarding.login_revision.wrapping_add(1);
                            self.onboarding.signed_in = false;
                        }
                        let text = String::from_utf8_lossy(&output.stderr);
                        if text.trim().is_empty() {
                            format!("Bundled agmen exited with {}", output.status)
                        } else {
                            text.into_owned()
                        }
                    }
                    Err(error) => format!("Bundled agmen failed: {error:#}"),
                });
            }
        }
    }
}
