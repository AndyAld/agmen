//! The add agent dialog (frame "Desktop · add agent", node 2418): the
//! agent's official install source, a prompt to paste into an agent that
//! has a shell, and Check again (spec 5.2). Add… on a missing agent opens
//! it; Update… on an outdated one and "Add a terminal agent…" open their
//! own variants (mockup fix 38). agmen runs no installer itself.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClipboardItem, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::code_view::cell_width;
use super::components::{
    Button, ButtonSize, ButtonStyle, CodeText, Color, Icon, IconSize, Label, LabelSize,
    SectionHeading,
};
use super::tokens::{
    BORDER_WIDTH, PROMPT_COLUMNS, RADIUS_CARD, RADIUS_REF, SPACE_4, SPACE_8, SPACE_10, SPACE_12,
};
use crate::appearance::Appearance;
use crate::backend::{AgentFound, InstalledAgent};

/// Which dialog is open, by the agent's command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum AgentDialog {
    /// Add… on a missing agent.
    Add(String),
    /// Update… on an outdated agent.
    Update(String),
    /// "Add a terminal agent…": any ACP agent or a command.
    Other,
}

impl AgentDialog {
    /// The agent the dialog is about, if any.
    pub(crate) fn agent(&self) -> Option<&str> {
        match self {
            Self::Add(name) | Self::Update(name) => Some(name),
            Self::Other => None,
        }
    }
}

/// The dialog's title.
#[must_use]
pub(crate) fn title(dialog: &AgentDialog, agent: Option<&InstalledAgent>) -> String {
    let name = |fallback: &str| agent.map_or_else(|| fallback.to_owned(), |a| a.title.clone());
    match dialog {
        AgentDialog::Add(cmd) => format!("Add {}", name(cmd)),
        AgentDialog::Update(cmd) => format!("Update {}", name(cmd)),
        AgentDialog::Other => "Add a terminal agent".into(),
    }
}

/// The line after the title, with command names in backticks.
fn subtitle(dialog: &AgentDialog, agent: Option<&InstalledAgent>) -> String {
    match (dialog, agent) {
        (AgentDialog::Other, _) | (_, None) => "any ACP agent, or a command".into(),
        (AgentDialog::Add(_), Some(a)) => match &a.source {
            Some(source) => format!(
                "{}'s {} · `{}` not found on PATH",
                source.vendor, source.about, a.name
            ),
            None => format!("`{}` not found on PATH", a.name),
        },
        (AgentDialog::Update(_), Some(a)) => match &a.found {
            AgentFound::Outdated {
                version,
                minimum,
                why,
            } => format!(
                "`{}` {version} found · agmen needs {minimum}+ for {why}",
                a.name
            ),
            AgentFound::Ready { version } => format!("`{}` {version} found", a.name),
            AgentFound::Missing => format!("`{}` not found on PATH", a.name),
        },
    }
}

/// The prompt to give an agent that has a shell (spec 5.2): the official
/// source when agmend knows it, the look-alike to avoid, and ending with
/// `<agent> --version` and `agmen agents scan`. Commands keep their
/// backticks: this is text to paste. `None` when no agent is named.
#[must_use]
pub(crate) fn prompt(dialog: &AgentDialog, agent: Option<&InstalledAgent>) -> Option<String> {
    let a = agent?;
    let name = &a.name;
    let first = match (dialog, &a.source) {
        (AgentDialog::Other, _) => return None,
        (AgentDialog::Add(_), Some(source)) => format!(
            "Install {vendor}'s {title} CLI (the `{name}` command) on this machine from \
             {vendor}'s official instructions at {url}.",
            vendor = source.vendor,
            title = a.title,
            url = source.url,
        ),
        (AgentDialog::Add(_), None) => format!(
            "Install {} (the `{name}` command) on this machine from its maker's official \
             instructions.",
            a.title
        ),
        (AgentDialog::Update(_), source) => {
            let minimum = match &a.found {
                AgentFound::Outdated { minimum, .. } => format!(" to {minimum} or later"),
                AgentFound::Ready { .. } | AgentFound::Missing => String::new(),
            };
            let from = source.as_ref().map_or_else(
                || "the same way it was installed".to_owned(),
                |s| format!("from {}'s official instructions at {}", s.vendor, s.url),
            );
            format!(
                "Update {} (the `{name}` command) on this machine{minimum}, {from}.",
                a.title
            )
        }
    };
    let warning = a
        .source
        .as_ref()
        .and_then(|s| s.look_alike.as_ref())
        .map(|other| format!(" Do not use {other}; it also installs a `{name}` command."))
        .unwrap_or_default();
    let done = if matches!(dialog, AgentDialog::Update(_)) {
        "updated"
    } else {
        "installed"
    };
    Some(format!(
        "{first}{warning} When it is {done}, run `{name} --version` and `agmen agents scan`, \
         and tell me both outputs."
    ))
}

/// What Check again does after the scan: the line under the buttons.
pub(crate) type CheckHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The dialog. Tests find it as `"add-agent"` and its parts as
/// `"add-agent:link"`, `":prompt"`, `":copy"`, `":note"`, `":cancel"` and
/// `":check"`.
#[derive(IntoElement)]
pub(crate) struct AddAgentDialog {
    dialog: AgentDialog,
    agent: Option<InstalledAgent>,
    copied: bool,
    note: Option<SharedString>,
    on_copied: Option<CheckHandler>,
    on_cancel: Option<CheckHandler>,
    on_check: Option<CheckHandler>,
}

impl std::fmt::Debug for AddAgentDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddAgentDialog")
            .field("dialog", &self.dialog)
            .field("copied", &self.copied)
            .field("note", &self.note)
            .finish_non_exhaustive()
    }
}

impl AddAgentDialog {
    /// `dialog` about `agent`, which agmend's scan describes; `None` for
    /// "Add a terminal agent…".
    #[must_use]
    pub(crate) const fn new(dialog: AgentDialog, agent: Option<InstalledAgent>) -> Self {
        Self {
            dialog,
            agent,
            copied: false,
            note: None,
            on_copied: None,
            on_cancel: None,
            on_check: None,
        }
    }

    /// Shows Copy prompt as done.
    #[must_use]
    pub(crate) const fn copied(mut self, copied: bool) -> Self {
        self.copied = copied;
        self
    }

    /// What the last Check again found, such as "grok is still not on PATH".
    #[must_use]
    pub(crate) fn note(mut self, note: Option<SharedString>) -> Self {
        self.note = note;
        self
    }

    /// Called after Copy prompt wrote the prompt to the clipboard.
    #[must_use]
    pub(crate) fn on_copied(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_copied = Some(Rc::new(handler));
        self
    }

    #[must_use]
    pub(crate) fn on_cancel(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_cancel = Some(Rc::new(handler));
        self
    }

    #[must_use]
    pub(crate) fn on_check(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_check = Some(Rc::new(handler));
        self
    }

    /// "Follow xAI's official instructions: github.com/... ↗", or what
    /// to do when agmend knows no source.
    fn source_row(&self) -> impl IntoElement + use<> {
        let source = self.agent.as_ref().and_then(|a| a.source.clone());
        let row = div().flex().items_center().gap(SPACE_8);
        match (source, &self.dialog) {
            (Some(source), _) => {
                let url = format!("https://{}", source.url);
                row.child(Label::new(format!(
                    "Follow {}'s official instructions:",
                    source.vendor
                )))
                .child(
                    div()
                        .id("add-agent:link")
                        .debug_selector(|| "add-agent:link".into())
                        .px(SPACE_4)
                        .rounded(RADIUS_REF)
                        .cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .child(
                            Label::new(format!("{} ↗", source.url))
                                .code()
                                .color(Color::Accent)
                                .underline(),
                        ),
                )
            }
            (None, AgentDialog::Other) => row.child(
                Label::new(
                    "Install it from its maker's official instructions, so its command is on PATH.",
                )
                .wrap(),
            ),
            (None, _) => row.child(
                Label::new(
                    "agmen has no official source on record for it; use the one it came from.",
                )
                .wrap(),
            ),
        }
    }

    /// The prompt box, Copy prompt and its hint.
    fn prompt_block(&self, prompt: String) -> impl IntoElement + use<> {
        let copied = self.copied;
        let on_copied = self.on_copied.clone();
        let text = prompt.clone();
        let copy = Button::new(
            "add-agent:copy",
            if copied { "Copied" } else { "Copy prompt" },
        )
        .style(ButtonStyle::Secondary)
        .size(ButtonSize::Medium)
        .leading(
            Icon::new(if copied {
                crate::icons::UiIcon::Check
            } else {
                crate::icons::UiIcon::Copy
            })
            .size(IconSize::Small)
            .color(Color::Accent),
        )
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            if let Some(handler) = &on_copied {
                handler(window, cx);
            }
        });
        div()
            .flex()
            .flex_col()
            .gap(SPACE_12)
            .child(SectionHeading::new(
                "Or give this prompt to an agent you already run",
            ))
            .child(PromptBox(prompt))
            .child(
                div().flex().items_center().gap(SPACE_8).child(copy).child(
                    Label::new(
                        "paste it into Claude Code, Codex or any agent with a shell; your \
                             listener has none",
                    )
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
                ),
            )
    }

    /// "agmen looks for grok on PATH, ...", with its commands in code.
    fn then_text(&self) -> String {
        match &self.agent {
            Some(a) => format!(
                "agmen looks for `{name}` on PATH, runs `{name} --version`, and checks that \
                 `{acp}` starts over ACP. It appears in this list once both pass.",
                name = a.name,
                acp = a.acp
            ),
            None => "agmen looks on PATH, runs the command's `--version`, and checks that it \
                     starts over ACP. It appears in this list once both pass."
                .into(),
        }
    }
}

/// The prompt, as text to select and paste (node 2428).
#[derive(Debug, IntoElement)]
struct PromptBox(String);

impl RenderOnce for PromptBox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let theme = &appearance.theme;
        // The mockup sets the prompt in lines narrower than the box.
        let measure = cell_width(window, &appearance.fonts) * PROMPT_COLUMNS;
        div()
            .debug_selector(|| "add-agent:prompt".into())
            .w_full()
            .flex()
            .py(SPACE_10)
            .px(SPACE_12)
            .rounded(RADIUS_CARD)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                div()
                    .debug_selector(|| "add-agent:prompt-text".into())
                    .max_w(measure)
                    .min_w_0()
                    .child(Label::new(self.0).code().wrap()),
            )
    }
}

impl RenderOnce for AddAgentDialog {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let agent = self.agent.as_ref();
        let heading = if matches!(self.dialog, AgentDialog::Update(_)) {
            "Update it yourself"
        } else {
            "Install it yourself"
        };
        let prompt = prompt(&self.dialog, agent);
        let has_prompt = prompt.is_some();
        let handler = |h: Option<CheckHandler>| {
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                if let Some(h) = &h {
                    h(window, cx);
                }
            }
        };
        // The terminal hosts this in a `Modal` DIALOG_WIDTH wide.
        div()
            .debug_selector(|| "add-agent".into())
            .flex()
            .flex_col()
            .gap(SPACE_12)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(SPACE_10)
                    .child(
                        Label::new(title(&self.dialog, agent))
                            .size(LabelSize::Title)
                            .weight(FontWeight::SEMIBOLD)
                            .color(Color::TextMuted),
                    )
                    .child(
                        CodeText::new(subtitle(&self.dialog, agent))
                            .size(LabelSize::Small)
                            .color(Color::Placeholder),
                    ),
            )
            .child(SectionHeading::new(heading))
            .child(self.source_row())
            .when_some(prompt, |this, prompt| this.child(self.prompt_block(prompt)))
            .child(SectionHeading::new("Then"))
            .child(CodeText::new(self.then_text()).size(LabelSize::Small))
            .child(
                Label::new(if has_prompt {
                    "agmen runs no installers itself: install steps change, and the prompt \
                     names the official source so an agent does not pick up a look-alike \
                     package."
                } else {
                    "agmen runs no installers itself: install steps change between releases."
                })
                .size(LabelSize::Small)
                .color(Color::Placeholder)
                .wrap(),
            )
            .when_some(self.note.clone(), |this, note| {
                this.child(
                    div().debug_selector(|| "add-agent:note".into()).child(
                        Label::new(note)
                            .size(LabelSize::Small)
                            .color(Color::Warning),
                    ),
                )
            })
            .child(
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    // Check again keeps its height beside the bordered Cancel.
                    .items_center()
                    .gap(SPACE_8)
                    .child(
                        Button::new("add-agent:cancel", "Cancel")
                            .style(ButtonStyle::Inset)
                            .size(ButtonSize::Large)
                            .on_click(handler(self.on_cancel.clone())),
                    )
                    .child(
                        Button::new("add-agent:check", "Check again")
                            .style(ButtonStyle::Primary)
                            .size(ButtonSize::Large)
                            .on_click(handler(self.on_check.clone())),
                    ),
            )
    }
}

#[cfg(test)]
#[path = "add_agent_dialog_tests.rs"]
mod tests;
