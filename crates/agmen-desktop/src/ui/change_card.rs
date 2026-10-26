//! The card under a listener message that changed or wants to change
//! settings: "Desktop · settings from chat" (node 7101) and "Desktop ·
//! settings applied" (node 7719).

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use super::components::{Button, ButtonStyle, Card, ChangeHandler, Color, Label, LabelSize};
use super::tokens::{
    CHANGE_CARD_WIDTH, CHANGE_SETTING_WIDTH, CHANGE_VALUE_MIN_WIDTH, SPACE_6, SPACE_8,
    TINT_PENDING, tint,
};
use crate::appearance::Appearance;
use crate::backend::{self, Action, ChangeRow, ChangeState, PersonGroup};

/// A backend change card, drawn by its state: waiting for Apply or Cancel,
/// applied with Undo, or cancelled with no buttons. Tests find the head row
/// as `"{id}:waiting"`, `"{id}:applied"` or `"{id}:cancelled"`, and the
/// card as `"{id}:card"`; `part_selectors` lists the rest.
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
        ChangeState::Undone => "Change undone",
    }
}

const fn title_color(state: ChangeState) -> Color {
    match state {
        ChangeState::Pending => Color::Warning,
        ChangeState::Applied => Color::TextMuted,
        ChangeState::Cancelled | ChangeState::Undone => Color::Placeholder,
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
        ChangeState::Undone => "settings restored".to_owned(),
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

/// Whether the card is settled with nothing in effect: cancelled, or undone
/// (Andy, review C93). Its rows stay for the record, dimmed, with no buttons.
const fn is_void(state: ChangeState) -> bool {
    matches!(state, ChangeState::Cancelled | ChangeState::Undone)
}

/// The old value, struck through; dimmed once the change is void.
fn before_label(row: &ChangeRow, state: ChangeState) -> Label {
    let color = if is_void(state) {
        Color::Placeholder
    } else {
        Color::Error
    };
    Label::new(row.before.clone())
        .code()
        .color(color)
        .strikethrough()
}

/// The debug selector of part `name` of the card `id`.
fn part(id: &backend::ChangeId, name: &str) -> String {
    format!("{id}:{name}")
}

/// Every part of `card` a test checks for overflow: the head's title and
/// subline, each row's target, setting, old value, arrow and new value, and
/// the buttons its state draws.
#[cfg(test)]
pub(super) fn part_selectors(card: &backend::ChangeCard) -> Vec<String> {
    let mut parts = vec![part(&card.id, "title"), part(&card.id, "subline")];
    for i in 0..card.rows.len() {
        for name in ["target", "setting", "before", "arrow", "after"] {
            parts.push(part(&card.id, &format!("{name}:{i}")));
        }
    }
    let buttons: &[&str] = match card.state {
        ChangeState::Pending => &["apply", "cancel"],
        ChangeState::Applied => &["undo", "audit-log"],
        ChangeState::Cancelled | ChangeState::Undone => &[],
    };
    parts.extend(buttons.iter().map(|name| part(&card.id, name)));
    parts
}

/// `child` in a box tests find as `selector`.
fn tagged(selector: String, child: impl IntoElement) -> gpui::Div {
    div().debug_selector(|| selector).child(child)
}

fn row(card: &backend::ChangeCard, i: usize, row: &ChangeRow, target: Color) -> impl IntoElement {
    let state = card.state;
    let (target, after) = if is_void(state) {
        (Color::Placeholder, Color::Placeholder)
    } else {
        (target, Color::Success)
    };
    let name = |name: &str| part(&card.id, &format!("{name}:{i}"));
    // In a narrow card the row wraps rather than squeezing the old value:
    // the new value fills what is left of the line, and moves to a line of
    // its own when less than CHANGE_VALUE_MIN_WIDTH is left.
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(SPACE_8)
        .child(tagged(
            name("target"),
            Label::new(row.target.clone()).code().color(target),
        ))
        .child(
            tagged(
                name("setting"),
                Label::new(row.setting.clone())
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            )
            .w(CHANGE_SETTING_WIDTH)
            .flex_none(),
        )
        .child(
            tagged(name("before"), before_label(row, state).wrap())
                .flex()
                .flex_initial()
                .min_w_0(),
        )
        .child(tagged(
            name("arrow"),
            Label::new("→").size(LabelSize::Small).color(Color::Muted),
        ))
        .child(
            tagged(
                name("after"),
                Label::new(row.after.clone()).code().color(after).wrap(),
            )
            .flex()
            .flex_1()
            .min_w(CHANGE_VALUE_MIN_WIDTH),
        )
}

/// The head row: the title, and the subline after it.
fn head(card: &backend::ChangeCard) -> gpui::Div {
    let state = card.state;
    let state_name = match state {
        ChangeState::Pending => "waiting",
        ChangeState::Applied => "applied",
        ChangeState::Cancelled => "cancelled",
        ChangeState::Undone => "undone",
    };
    let head_selector = part(&card.id, state_name);
    div()
        .debug_selector(|| head_selector)
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(SPACE_8)
        .child(tagged(
            part(&card.id, "title"),
            Label::new(title(state))
                .size(LabelSize::Small)
                .weight(FontWeight::SEMIBOLD)
                .color(title_color(state)),
        ))
        // The subline wraps, below the title when they do not fit on
        // one line, rather than running past a narrow card.
        .child(
            tagged(
                part(&card.id, "subline"),
                Label::new(subline(card))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
            )
            .flex_auto()
            .min_w_0(),
        )
}

impl RenderOnce for ChangeCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let card = self.card;
        let state = card.state;
        let border = if state == ChangeState::Pending {
            tint(theme.warning, TINT_PENDING)
        } else {
            theme.border
        };
        let head = head(&card);
        let rows = card
            .rows
            .iter()
            .zip(self.target_colors)
            .enumerate()
            .map(|(i, (change, target))| row(&card, i, change, target));

        let button = |name: &str, label: &'static str, style: ButtonStyle, action: Action| {
            let handler = self.on_action.clone();
            Button::new(SharedString::from(part(&card.id, name)), label)
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
                let audit_selector = part(&card.id, "audit-log");
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
            ChangeState::Cancelled | ChangeState::Undone => None,
        };

        let card_selector = part(&card.id, "card");
        // Full width up to the mockup's, so a narrow pane does not cut it off.
        div()
            .debug_selector(|| card_selector)
            .w_full()
            .max_w(CHANGE_CARD_WIDTH)
            .child(
                Card::new()
                    .border_color(border)
                    .child(head)
                    .children(rows)
                    .children(buttons.map(|buttons| buttons.flex().items_center().gap(SPACE_6))),
            )
    }
}

#[cfg(test)]
#[path = "change_card_tests.rs"]
mod tests;
