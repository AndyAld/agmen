//! Add a listener: what one does, and a recommended model to run it on.

use super::components::{Button, Color, DialogFooter, Label, LabelSize, Modal, Radio};
use super::tokens::SPACE_4;
use crate::backend::{Action, BackendHandle};
use gpui::{
    Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div,
};

/// The models offered, as id, name and why: agmend's default first. A
/// listener runs on Claude Code, so these are Anthropic's.
const CHOICES: [(&str, &str, &str); 2] = [
    (
        "claude-haiku-5-5",
        "Haiku 5.5",
        "Recommended. Fast and cheap, and what agmen uses by default.",
    ),
    (
        "claude-sonnet-5-5",
        "Sonnet 5.5",
        "More judgment for a busy chat. Costs more.",
    ),
];

/// What a listener is, in the dialog's own words.
pub(crate) const WHAT_IT_DOES: &str = "A listener is a small agent that reads the chat for you. It answers what it can from the project and hands real work to a bigger agent, inside the spending limits you set. agmen works without one: your messages then go to the project chat as a whole.";

pub(crate) struct Closed;

/// Tests find the card as `listener-dialog:card`, each choice as
/// `listener-dialog:model:<n>` and the buttons as `listener-dialog:add` and
/// `listener-dialog:cancel`.
#[derive(Debug, Default)]
pub(crate) struct ListenerDialog {
    choice: usize,
    error: Option<String>,
}
impl EventEmitter<Closed> for ListenerDialog {}

impl ListenerDialog {
    fn add(&mut self, cx: &mut Context<Self>) {
        let model = CHOICES.get(self.choice).map(|(id, _, _)| (*id).to_owned());
        let result = cx
            .try_global::<BackendHandle>()
            .ok_or_else(|| "No project backend is connected.".to_owned())
            .and_then(|backend| {
                backend
                    .0
                    .act(Action::StartListener { model })
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => cx.emit(Closed),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
}

impl Render for ListenerDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let choices = CHOICES.iter().enumerate().map(|(index, (_, name, why))| {
            div()
                .flex()
                .flex_col()
                .gap(SPACE_4)
                .child(
                    Radio::new(format!("listener-dialog:model:{index}"), *name)
                        .on(index == self.choice)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choice = index;
                            cx.notify();
                        })),
                )
                .child(
                    Label::new(*why)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder)
                        .wrap(),
                )
        });
        Modal::small("listener-dialog")
            .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(Closed)))
            .title(Label::title("Add a listener"))
            .child(
                div()
                    .debug_selector(|| "listener-dialog:about".into())
                    .child(Label::new(WHAT_IT_DOES).color(Color::TextMuted).wrap()),
            )
            .children(choices)
            .error(self.error.clone())
            .footer(
                DialogFooter::new(
                    Button::new("listener-dialog:add", "Add listener")
                        .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                )
                .cancel(
                    Button::new("listener-dialog:cancel", "Cancel")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Screen;
    use crate::ui::plan_test_support::plan_globals;
    use crate::ui::test_support::click;
    use gpui::TestAppContext;

    #[gpui::test]
    fn add_starts_a_listener_on_the_recommended_model_or_the_picked_one(cx: &mut TestAppContext) {
        let (backend, _) = plan_globals(cx, Screen::Plan, None);
        let (_, cx) = cx.add_window_view(|_, _| ListenerDialog::default());
        click(cx, "listener-dialog:add");
        click(cx, "listener-dialog:model:1");
        click(cx, "listener-dialog:add");
        let model = |id: &str| Action::StartListener {
            model: Some(id.into()),
        };
        assert_eq!(
            *backend.acts.borrow(),
            vec![model("claude-haiku-5-5"), model("claude-sonnet-5-5")]
        );
    }

    #[gpui::test]
    fn the_dialog_says_what_a_listener_does_and_that_agmen_works_without_one(
        cx: &mut TestAppContext,
    ) {
        plan_globals(cx, Screen::Plan, None);
        let (_, cx) = cx.add_window_view(|_, _| ListenerDialog::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("listener-dialog:about").is_some());
        assert!(WHAT_IT_DOES.contains("works without one"));
        // The recommended choice is the default one.
        assert!(CHOICES[0].2.starts_with("Recommended"));
    }
}
