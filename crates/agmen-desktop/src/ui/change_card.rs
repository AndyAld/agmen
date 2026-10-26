//! The card under a listener message that changed or wants to change
//! settings: "Desktop · settings from chat" (node 7101) and "Desktop ·
//! settings applied" (node 7719).

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::components::{Button, ButtonStyle, ChangeHandler, Color, Label, LabelSize};
use super::tokens::{
    BORDER_WIDTH, CHANGE_CARD_WIDTH, CHANGE_SETTING_WIDTH, PENDING_BORDER_TINT, RADIUS_LG, SPACE_6,
    SPACE_8, SPACE_10, SPACE_12,
};
use crate::appearance::Appearance;
use crate::backend::{self, Action, ChangeRow, ChangeState, PersonGroup};

/// A backend change card, drawn by its state: waiting for Apply or Cancel,
/// applied with Undo, or cancelled with no buttons. Tests find the head row
/// as `"{id}:waiting"`, `"{id}:applied"` or `"{id}:cancelled"`, and the
/// buttons as `"{id}:apply"`, `"{id}:cancel"`, `"{id}:undo"` and
/// `"{id}:audit-log"`.
#[derive(IntoElement)]
pub(crate) struct ChangeCard {
    card: backend::ChangeCard,
    /// Each row's target color, from its agent's role.
    target_colors: Vec<Color>,
    on_action: Option<ChangeHandler<Action>>,
}

impl std::fmt::Debug for ChangeCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeCard")
            .field("card", &self.card)
            .finish_non_exhaustive()
    }
}

impl ChangeCard {
    /// `roster` colors each row's target by its agent's role; a target not
    /// in it is drawn in body text.
    #[must_use]
    pub(crate) fn new(card: backend::ChangeCard, roster: &[PersonGroup]) -> Self {
        let target_colors = card
            .rows
            .iter()
            .map(|row| {
                roster
                    .iter()
                    .flat_map(|group| &group.agents)
                    .find(|agent| agent.handle == row.target)
                    .map_or(Color::Default, |agent| {
                        Color::Role(agent.role.clone().into())
                    })
            })
            .collect();
        Self {
            card,
            target_colors,
            on_action: None,
        }
    }

    /// Called with Apply, Cancel or Undo for this card when its button is
    /// clicked; the caller passes it to `Backend::act`.
    #[must_use]
    pub(crate) fn on_action(
        mut self,
        handler: impl Fn(Action, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }
}

/// The head row's title.
fn title(state: ChangeState) -> &'static str {
    match state {
        ChangeState::Pending => "Waiting for you",
        ChangeState::Applied => "Settings changed",
        ChangeState::Cancelled => "Change cancelled",
    }
}

const fn title_color(state: ChangeState) -> Color {
    match state {
        ChangeState::Pending => Color::Warning,
        ChangeState::Applied => Color::TextMuted,
        ChangeState::Cancelled => Color::Placeholder,
    }
}

/// The head row's line after the title, such as "requested by andy · widens
/// lanes and budget".
fn subline(card: &backend::ChangeCard) -> String {
    let what = match card.state {
        ChangeState::Pending => {
            let verb = if card.widens { "widens" } else { "narrows" };
            let settings = join_settings(card.rows.iter().map(|row| row.setting.as_str()));
            format!("{verb} {settings}")
        }
        ChangeState::Applied => "applied by agmend".to_owned(),
        ChangeState::Cancelled => "nothing changed".to_owned(),
    };
    format!("requested by {} · {what}", card.requested_by)
}

/// Each setting once, in order: "lanes", "lanes and budget", "lanes, budget
/// and tools".
fn join_settings<'a>(settings: impl IntoIterator<Item = &'a str>) -> String {
    let mut unique: Vec<&str> = Vec::new();
    for setting in settings {
        if !unique.contains(&setting) {
            unique.push(setting);
        }
    }
    match unique.split_last() {
        None => String::new(),
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// The old value, struck through; dimmed once the change is cancelled.
fn before_label(row: &ChangeRow, state: ChangeState) -> Label {
    let color = if state == ChangeState::Cancelled {
        Color::Placeholder
    } else {
        Color::Error
    };
    Label::new(row.before.clone())
        .code()
        .color(color)
        .strikethrough()
}

fn row(row: &ChangeRow, target: Color, state: ChangeState) -> impl IntoElement {
    // A cancelled card keeps its rows for the record, dimmed.
    let (target, after) = if state == ChangeState::Cancelled {
        (Color::Placeholder, Color::Placeholder)
    } else {
        (target, Color::Success)
    };
    div()
        .flex()
        .items_center()
        .gap(SPACE_8)
        .child(Label::new(row.target.clone()).code().color(target))
        .child(
            div().w(CHANGE_SETTING_WIDTH).flex_none().child(
                Label::new(row.setting.clone())
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            ),
        )
        .child(before_label(row, state))
        .child(Label::new("→").size(LabelSize::Small).color(Color::Muted))
        .child(Label::new(row.after.clone()).code().color(after).wrap())
}

impl RenderOnce for ChangeCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let card = self.card;
        let state = card.state;
        let border = if state == ChangeState::Pending {
            theme.warning.opacity(PENDING_BORDER_TINT)
        } else {
            theme.border
        };
        let state_name = match state {
            ChangeState::Pending => "waiting",
            ChangeState::Applied => "applied",
            ChangeState::Cancelled => "cancelled",
        };
        let head_selector = format!("{}:{state_name}", card.id);
        let head = div()
            .debug_selector(|| head_selector)
            .flex()
            .items_center()
            .gap(SPACE_8)
            .child(
                Label::new(title(state))
                    .size(LabelSize::Small)
                    .weight(FontWeight::SEMIBOLD)
                    .color(title_color(state)),
            )
            .child(
                Label::new(subline(&card))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            );
        let rows = card
            .rows
            .iter()
            .zip(self.target_colors)
            .map(|(change, target)| row(change, target, state));

        let button = |name: &str, label: &'static str, style: ButtonStyle, action: Action| {
            let handler = self.on_action.clone();
            Button::new(SharedString::from(format!("{}:{name}", card.id)), label)
                .style(style)
                .when_some(handler, |button, handler| {
                    button.on_click(move |_, window, cx| handler(action.clone(), window, cx))
                })
        };
        let buttons = match state {
            ChangeState::Pending => Some(
                div()
                    .child(button(
                        "apply",
                        "Apply",
                        ButtonStyle::Primary,
                        Action::Apply(card.id.clone()),
                    ))
                    .child(button(
                        "cancel",
                        "Cancel",
                        ButtonStyle::Secondary,
                        Action::Cancel(card.id.clone()),
                    )),
            ),
            // Mockup fix 44: Audit log is plain text, not a link.
            ChangeState::Applied => {
                let audit_selector = format!("{}:audit-log", card.id);
                Some(
                    div()
                        .child(button(
                            "undo",
                            "Undo",
                            ButtonStyle::Secondary,
                            Action::Undo(card.id.clone()),
                        ))
                        .child(
                            div().debug_selector(|| audit_selector).child(
                                Label::new("Audit log")
                                    .size(LabelSize::Small)
                                    .color(Color::Placeholder),
                            ),
                        ),
                )
            }
            ChangeState::Cancelled => None,
        };

        div()
            .w(CHANGE_CARD_WIDTH)
            .flex()
            .flex_col()
            .gap(SPACE_6)
            .py(SPACE_10)
            .px(SPACE_12)
            .rounded(RADIUS_LG)
            .border(BORDER_WIDTH)
            .border_color(border)
            .bg(theme.elevated_surface)
            .child(head)
            .children(rows)
            .children(buttons.map(|buttons| buttons.flex().items_center().gap(SPACE_6)))
    }
}

#[cfg(test)]
#[path = "change_card_tests.rs"]
mod tests;
