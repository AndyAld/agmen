//! The same optional command installation in welcome and Settings.

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, Task, Window, div,
};

use super::components::{Button, ButtonStyle, Color, Label};
use super::tokens::SPACE_10;
use crate::bundle::{self, Bundle};
use crate::cli_link::{self, Offer};

pub(crate) struct CliInstall {
    bundle: Bundle,
    home: PathBuf,
    text: String,
    available: bool,
    working: bool,
    task: Option<Task<()>>,
}

impl CliInstall {
    pub(crate) fn for_app(cx: &mut App) -> Option<Entity<Self>> {
        let bundle = bundle::locate(&std::env::current_exe().ok()?)?;
        let home = std::env::home_dir()?;
        Some(cx.new(|cx| {
            let mut view = Self {
                bundle,
                home,
                text: "Checking the agmen command…".into(),
                available: false,
                working: false,
                task: None,
            };
            view.start(false, cx);
            view
        }))
    }

    fn start(&mut self, install: bool, cx: &mut Context<Self>) {
        if self.working {
            return;
        }
        self.working = true;
        self.available = false;
        let bundle = self.bundle.clone();
        let home = self.home.clone();
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("agmen-cli-check".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<(String, bool)> {
                    // Re-read on the button action before making any ownership change.
                    let receipt = agmen_client::read_install_receipt()?;
                    match cli_link::inspect(&home, receipt.as_ref(), &mut crate::service::run)? {
                        Offer::Available { hint } if install => {
                            cli_link::install(&bundle, &home, receipt.as_ref(), None)?;
                            Ok((
                                format!(
                                    "Linked ~/.local/bin/agmen to {}. {hint}",
                                    bundle.agmen.display()
                                ),
                                false,
                            ))
                        }
                        Offer::Available { hint } => Ok((
                            format!("Agents need the agmen command on PATH. {hint}"),
                            true,
                        )),
                        Offer::Existing { path, version } => {
                            Ok((format!("Agents use {} ({version}).", path.display()), false))
                        }
                        Offer::Blocked(text) => Ok((text, false)),
                    }
                })();
                drop(done.send(result));
            });
        if let Err(error) = spawned {
            self.text = format!("Starting the CLI check failed: {error}");
            self.working = false;
            return;
        }
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = result.await;
            drop(this.update(cx, |this, cx| {
                this.working = false;
                this.task = None;
                match result {
                    Ok(Ok((text, available))) => {
                        this.text = text;
                        this.available = available;
                    }
                    Ok(Err(error)) => {
                        this.text = format!("Checking the agmen command failed: {error:#}")
                    }
                    Err(_) => {
                        this.text =
                            "The agmen command check stopped. Reopen this screen to retry.".into()
                    }
                }
                cx.notify();
            }));
        }));
    }
}

impl Render for CliInstall {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "cli-install-offer".into())
            .flex()
            .flex_col()
            .items_start()
            .gap(SPACE_10)
            .child(
                Label::new(self.text.clone())
                    .color(Color::Placeholder)
                    .wrap(),
            )
            .when(self.available && !self.working, |this| {
                this.child(
                    Button::new("install-agmen-command", "Install the agmen command")
                        .style(ButtonStyle::Filled)
                        .on_click(cx.listener(|this, _, _, cx| this.start(true, cx))),
                )
            })
    }
}
