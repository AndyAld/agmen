//! Launch a managed Claude Code agent through the local backend.

use super::components::{Button, DialogFooter, Field, InputFrame, Label, Modal};
use super::text_input::TextInput;
use crate::backend::{Action, BackendHandle};
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Window, div,
};

pub(crate) struct Closed;
pub(crate) struct NewAgentDialog {
    name: Entity<TextInput>,
    model: Entity<TextInput>,
    budget: Entity<TextInput>,
    default_model: String,
    error: Option<String>,
}
impl EventEmitter<Closed> for NewAgentDialog {}

impl NewAgentDialog {
    pub(crate) fn new(
        default_model: String,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            name: cx.new(|cx| TextInput::new(cx).placeholder("worker")),
            model: cx.new(|cx| TextInput::new(cx).placeholder(default_model.clone())),
            budget: cx.new(|cx| TextInput::new(cx).placeholder("5.00")),
            default_model,
            error,
        }
    }

    fn launch(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        let model = self.model.read(cx).text().trim();
        let model = if model.is_empty() {
            self.default_model.clone()
        } else {
            model.to_owned()
        };
        let budget = self.budget.read(cx).text().trim();
        let budget = if budget.is_empty() { "5.00" } else { budget };
        let Some(budget_cents) = budget_cents(budget) else {
            self.error =
                Some("Enter a positive budget in dollars, with at most two decimal places.".into());
            cx.notify();
            return;
        };
        let result = cx
            .try_global::<BackendHandle>()
            .ok_or_else(|| "No project backend is connected.".to_owned())
            .and_then(|backend| {
                backend
                    .0
                    .act(Action::LaunchAgent {
                        name,
                        model,
                        budget_cents,
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

fn budget_cents(raw: &str) -> Option<u32> {
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let dollars = whole.parse::<u32>().ok()?;
    let fraction = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u32>().ok()?.checked_mul(10)?,
        _ => fraction.parse::<u32>().ok()?,
    };
    dollars
        .checked_mul(100)?
        .checked_add(fraction)
        .filter(|cents| *cents > 0)
}

impl Render for NewAgentDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Modal::small("new-agent")
            .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(Closed)))
            .title(Label::title("New Agent"))
            .child(
                Field::new("new-agent:name-field", "Name").child(
                    div()
                        .debug_selector(|| "new-agent:name".into())
                        .child(InputFrame::new().child(self.name.clone())),
                ),
            )
            .child(
                Field::new("new-agent:model-field", "Model").child(
                    div()
                        .debug_selector(|| "new-agent:model".into())
                        .child(InputFrame::new().child(self.model.clone())),
                ),
            )
            .child(
                Field::new("new-agent:budget-field", "Budget (USD)").child(
                    div()
                        .debug_selector(|| "new-agent:budget".into())
                        .child(InputFrame::new().child(self.budget.clone())),
                ),
            )
            .error(self.error.clone())
            .footer(
                DialogFooter::new(
                    Button::new("new-agent:launch", "Launch")
                        .on_click(cx.listener(|this, _, _, cx| this.launch(cx))),
                )
                .cancel(
                    Button::new("new-agent:cancel", "Cancel")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_are_bounded_positive_dollar_amounts() {
        assert_eq!(budget_cents("5"), Some(500));
        assert_eq!(budget_cents("0.01"), Some(1));
        assert_eq!(budget_cents("12.5"), Some(1250));
        for value in ["0", "-1", "NaN", "inf", "1.001", "42949673", "1..2", ""] {
            assert_eq!(budget_cents(value), None, "{value}");
        }
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    use crate::backend::Screen;
    use crate::ui::plan_test_support::plan_globals;
    use crate::ui::test_support::click;
    use gpui::TestAppContext;

    #[gpui::test]
    fn launch_button_passes_name_configured_model_and_budget_to_backend(cx: &mut TestAppContext) {
        let (backend, _) = plan_globals(cx, Screen::Plan, None);
        let (dialog, cx) = cx.add_window_view(|_, cx| NewAgentDialog::new("opus".into(), None, cx));
        dialog.update_in(cx, |dialog, window, cx| {
            dialog
                .name
                .update(cx, |input, cx| input.set_text("worker", window, cx));
            dialog
                .budget
                .update(cx, |input, cx| input.set_text("12.50", window, cx));
        });
        click(cx, "new-agent:launch");
        assert_eq!(
            *backend.acts.borrow(),
            vec![Action::LaunchAgent {
                name: "worker".into(),
                model: "opus".into(),
                budget_cents: 1250
            }]
        );
    }
}
