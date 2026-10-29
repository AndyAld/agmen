//! Remove…'s confirmation: says what goes (the agent and its identity) and
//! names the folder and branch that stay.

use std::path::PathBuf;

use agmen_proto::IdentityId;
use gpui::{Context, EventEmitter, IntoElement, ParentElement as _, Render, Window};

use super::components::{Button, DialogFooter, Label, Modal};
use crate::backend::{Action, BackendHandle};

pub(crate) struct Closed;

/// What Remove leaves on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kept {
    /// A worktree and branch New Agent made.
    Launched {
        folder: Option<String>,
        branch: Option<String>,
    },
    /// A folder the person joined by hand.
    Joined { folder: String },
}

pub(crate) struct RemoveAgentDialog {
    identity: IdentityId,
    handle: String,
    /// The hand-joined folder agmend is told about, when it may not know it.
    root: Option<PathBuf>,
    kept: Kept,
    error: Option<String>,
}

impl EventEmitter<Closed> for RemoveAgentDialog {}

impl RemoveAgentDialog {
    pub(crate) const fn new(
        identity: IdentityId,
        handle: String,
        root: Option<PathBuf>,
        kept: Kept,
    ) -> Self {
        Self {
            identity,
            handle,
            root,
            kept,
            error: None,
        }
    }

    /// The dialog's text under its title, one sentence a line.
    pub(crate) fn message(&self) -> String {
        let goes = format!(
            "{} stops, and its identity is revoked on the hub.",
            self.handle
        );
        let stays = match &self.kept {
            Kept::Launched {
                folder: Some(folder),
                branch: Some(branch),
            } => format!("Its folder {folder} and branch {branch} are kept."),
            Kept::Launched {
                folder: Some(folder),
                branch: None,
            } => format!("Its folder {folder} is kept."),
            Kept::Launched {
                folder: None,
                branch: Some(branch),
            } => format!("Its branch {branch} is kept."),
            Kept::Launched {
                folder: None,
                branch: None,
            } => "Its folder and branch are kept.".to_owned(),
            Kept::Joined { folder } => {
                format!("Only its identity file leaves {folder}; the folder itself is not deleted.")
            }
        };
        format!("{goes}\n{stays}")
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let result = cx
            .try_global::<BackendHandle>()
            .ok_or_else(|| "No project backend is connected.".to_owned())
            .and_then(|backend| {
                backend
                    .0
                    .act(Action::RemoveAgent {
                        identity: self.identity.clone(),
                        root: self.root.clone(),
                    })
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => cx.emit(Closed),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
}

impl Render for RemoveAgentDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Modal::small("remove-agent")
            .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(Closed)))
            .title(Label::title("Remove agent?"))
            .children(
                self.message()
                    .lines()
                    .map(|line| Label::new(line.to_owned()).wrap())
                    .collect::<Vec<_>>(),
            )
            .error(self.error.clone())
            .footer(
                DialogFooter::new(
                    Button::new("remove-agent:confirm", "Remove")
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                )
                .destructive(true)
                .cancel(
                    Button::new("remove-agent:cancel", "Cancel")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn remove_agent_uses_the_danger_style(cx: &mut gpui::TestAppContext) {
        crate::ui::test_support::init(cx);
        let (_, cx) = cx.add_window_view(|_, _| {
            dialog(Kept::Joined {
                folder: "/r".into(),
            })
        });
        crate::ui::test_support::redraw(cx);
        let error = cx.update(|_, cx| cx.global::<crate::appearance::Appearance>().theme.error);
        assert_eq!(
            crate::ui::test_support::fills_at(cx, "remove-agent:confirm"),
            [error]
        );
    }

    fn dialog(kept: Kept) -> RemoveAgentDialog {
        RemoveAgentDialog::new(
            crate::backend::fixture_id("andy/x"),
            "andy/x".into(),
            None,
            kept,
        )
    }

    #[test]
    fn the_message_names_what_is_kept() {
        assert_eq!(
            dialog(Kept::Launched {
                folder: Some("/r/.worktrees/agent/x-1234".into()),
                branch: Some("agent/x-1234".into()),
            })
            .message(),
            "andy/x stops, and its identity is revoked on the hub.\nIts folder /r/.worktrees/agent/x-1234 and branch agent/x-1234 are kept."
        );
        assert_eq!(
            dialog(Kept::Joined {
                folder: "/Users/andy/Marked-sol".into()
            })
            .message(),
            "andy/x stops, and its identity is revoked on the hub.\nOnly its identity file leaves /Users/andy/Marked-sol; the folder itself is not deleted."
        );
    }
}
