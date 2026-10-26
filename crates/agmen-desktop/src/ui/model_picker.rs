//! The model picker the composer's model chip opens (frame "Desktop · model
//! picker", node 3355; spec 4 "Model and effort per message"): models
//! grouped by installed agent, an effort row for agents that take one,
//! "this message" or "until I change it", and what the choice means for
//! the listener.
//!
//! [`ModelMenu`] is the composer's state: the listener's own model, the
//! choice the chip shows and the one it returns to after a send.
//! [`ModelPicker`] draws it and reports each click as a [`PickerEvent`].

use std::rc::Rc;

use gpui::{
    App, BoxShadow, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, black, div,
    point,
};

use super::components::{ChangeHandler, Color, Label, LabelSize, MenuRow, Radio, Segmented};
use super::tokens::{
    BORDER_WIDTH, DROPDOWN_MENU_MAX_HEIGHT, MENU_SHADOW_ALPHA, MENU_SHADOW_BLUR,
    MENU_SHADOW_OFFSET, MODEL_PICKER_WIDTH, RADIUS_LG, SPACE_2, SPACE_4, SPACE_6, SPACE_8,
    SPACE_10, SPACE_12, SPACE_14,
};
use crate::appearance::Appearance;
use crate::backend::{ChoiceScope, Effort, ModelChoice, ModelGroup, Snapshot};

/// The most models one agent's group lists; `opencode models` can print
/// dozens, and the list scrolls past the menu's height anyway.
const MAX_MODELS_PER_AGENT: usize = 64;

/// The composer's model state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelMenu {
    groups: Rc<[ModelGroup]>,
    /// "claude 2.1.310" per group, in the same order.
    headings: Rc<[SharedString]>,
    /// The listener's own settings: its agent's configured model.
    listener: ModelChoice,
    listener_handle: SharedString,
    /// What the chip returns to after a "this message" send: the
    /// listener's own model, or the last "until I change it" choice.
    standing: ModelChoice,
    /// What the chip shows and the next message runs on.
    current: ModelChoice,
    open: bool,
}

impl ModelMenu {
    /// Starts on the listener's own agent and configured model (fix 31).
    #[must_use]
    pub(crate) fn new(snapshot: &Snapshot) -> Self {
        let listener = ModelChoice::configured(snapshot.listener.agent.clone());
        let mut menu = Self {
            groups: Rc::from([]),
            headings: Rc::from([]),
            standing: listener.clone(),
            current: listener.clone(),
            listener,
            listener_handle: SharedString::default(),
            open: false,
        };
        menu.refresh(snapshot);
        menu
    }

    /// Takes a new snapshot's agents, models and listener. A choice the
    /// picker still offers stays; the listener's old settings, or a model
    /// no longer offered, become the new listener's settings.
    pub(crate) fn refresh(&mut self, snapshot: &Snapshot) {
        let groups: Rc<[ModelGroup]> = snapshot
            .models
            .iter()
            .map(|group| ModelGroup {
                models: group
                    .models
                    .iter()
                    .take(MAX_MODELS_PER_AGENT)
                    .cloned()
                    .collect(),
                ..group.clone()
            })
            .collect();
        let headings = groups
            .iter()
            .map(|group| {
                let version = snapshot
                    .installed_agents
                    .iter()
                    .find(|agent| agent.name == group.agent)
                    .and_then(|agent| agent.version());
                match version {
                    Some(version) => format!("{} {version}", group.agent).into(),
                    None => group.agent.clone().into(),
                }
            })
            .collect();
        self.groups = groups;
        self.headings = headings;
        self.listener_handle = snapshot.listener.handle.clone().into();
        let old = std::mem::replace(
            &mut self.listener,
            ModelChoice::configured(snapshot.listener.agent.clone()),
        );
        let stays =
            |menu: &Self, choice: &ModelChoice| !choice.same_model(&old) && menu.offers(choice);
        if !stays(self, &self.current) {
            self.current = ModelChoice {
                scope: self.current.scope,
                ..self.listener.clone()
            };
        }
        if !stays(self, &self.standing) {
            self.standing = ModelChoice {
                scope: self.standing.scope,
                ..self.listener.clone()
            };
        }
    }

    /// The choice the chip shows.
    pub(crate) const fn current(&self) -> &ModelChoice {
        &self.current
    }

    pub(crate) const fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) const fn set_open(&mut self, open: bool) {
        self.open = open;
    }

    fn group(&self, agent: &str) -> Option<&ModelGroup> {
        self.groups.iter().find(|group| group.agent == agent)
    }

    /// Whether `group` lists a "configured model" row: an agent that lists
    /// no models, and the listener's own agent, so its settings can be
    /// picked again.
    fn offers_configured(&self, group: &ModelGroup) -> bool {
        group.models.is_empty() || group.agent == self.listener.agent
    }

    /// Whether the picker offers `choice`'s model and effort.
    fn offers(&self, choice: &ModelChoice) -> bool {
        self.group(&choice.agent).is_some_and(|group| {
            let model = match &choice.model {
                None => self.offers_configured(group),
                Some(model) => group.models.contains(model),
            };
            model && choice.effort.is_none_or(|e| group.efforts.contains(&e))
        })
    }

    /// Does what a click in the picker asks.
    pub(crate) fn apply(&mut self, event: &PickerEvent) {
        match event {
            PickerEvent::Model { agent, model } => self.pick(agent, model.as_deref()),
            PickerEvent::Effort(effort) => {
                self.current.effort = Some(*effort);
                self.keep_if_sticky();
            }
            PickerEvent::Scope(scope) => self.set_scope(*scope),
        }
    }

    /// Picks `agent`'s `model` (`None` for its configured model). The
    /// effort carries over within an agent and starts at the agent's
    /// default otherwise; an agent without levels gets none.
    fn pick(&mut self, agent: &str, model: Option<&str>) {
        // The listener's configured model is its own settings, effort too.
        if model.is_none() && agent == self.listener.agent {
            self.current = ModelChoice {
                scope: self.current.scope,
                ..self.listener.clone()
            };
            self.keep_if_sticky();
            return;
        }
        let effort = self.group(agent).and_then(|group| {
            let kept = self
                .current
                .effort
                .filter(|effort| self.current.agent == agent && group.efforts.contains(effort));
            kept.or_else(|| group.default_effort())
        });
        self.current = ModelChoice {
            agent: agent.to_owned(),
            model: model.map(str::to_owned),
            effort,
            scope: self.current.scope,
        };
        self.keep_if_sticky();
    }

    /// "until I change it" makes the choice the standing one; "this
    /// message" drops a standing choice, so the listener's own model
    /// returns after the next send.
    fn set_scope(&mut self, scope: ChoiceScope) {
        self.current.scope = scope;
        match scope {
            ChoiceScope::UntilChanged => self.standing = self.current.clone(),
            ChoiceScope::ThisMessage => self.standing = self.listener.clone(),
        }
    }

    fn keep_if_sticky(&mut self) {
        if self.current.scope == ChoiceScope::UntilChanged {
            self.standing = self.current.clone();
        }
    }

    /// The model a message sent now carries: `None` for the listener's own
    /// settings.
    #[must_use]
    pub(crate) fn outgoing(&self) -> Option<ModelChoice> {
        (!self.current.same_model(&self.listener)).then(|| self.current.clone())
    }

    /// After a send: a "this message" choice goes back to the standing one.
    pub(crate) fn sent(&mut self) {
        if self.current.scope == ChoiceScope::ThisMessage {
            self.current = self.standing.clone();
        }
    }

    /// The picker over this state, reporting clicks to `on_event`.
    #[must_use]
    pub(crate) fn picker(
        &self,
        on_event: impl Fn(PickerEvent, &mut Window, &mut App) + 'static,
    ) -> ModelPicker {
        ModelPicker {
            menu: self.clone(),
            on_event: Rc::new(on_event),
        }
    }
}

/// A click in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PickerEvent {
    /// A model row: the agent and its model, `None` for its configured one.
    Model {
        agent: String,
        model: Option<String>,
    },
    Effort(Effort),
    Scope(ChoiceScope),
}

impl PickerEvent {
    /// Whether the picker closes after it, as Zed's model selector does on
    /// a pick and the mockup's prototype links do (rows and effort lead to
    /// "Desktop · ask"). Changing the scope keeps it open.
    pub(crate) const fn closes(&self) -> bool {
        !matches!(self, Self::Scope(_))
    }
}

/// "claude takes low to max and agy low to high; codex, opencode and
/// gemini take a model only." from the groups' effort levels.
#[must_use]
pub(crate) fn effort_hint(groups: &[ModelGroup]) -> String {
    // The verb comes once: "claude takes low to max and agy low to high".
    let ranges: Vec<String> = groups
        .iter()
        .filter_map(|group| Some((group, group.efforts.first()?, group.efforts.last()?)))
        .enumerate()
        .map(|(i, (group, low, high))| {
            let verb = if i == 0 { " takes" } else { "" };
            format!("{}{verb} {low} to {high}", group.agent)
        })
        .collect();
    let plain: Vec<&str> = groups
        .iter()
        .filter(|group| group.efforts.is_empty())
        .map(|group| group.agent.as_str())
        .collect();
    let mut hint = and_list(&ranges.iter().map(String::as_str).collect::<Vec<_>>());
    if !plain.is_empty() {
        if !hint.is_empty() {
            hint.push_str("; ");
        }
        let verb = if plain.len() == 1 { "takes" } else { "take" };
        hint.push_str(&format!("{} {verb} a model only", and_list(&plain)));
    }
    hint.push('.');
    hint
}

/// "a", "a and b", "a, b and c".
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The picker (node 3355). Tests find it as `"model-picker"`, a model row as
/// `"model-picker:{agent}:{model}"` (`default` for the configured model),
/// the effort section as `"model-picker:effort"` and its levels as
/// `"model-picker:effort-level:{i}"`, the scope options as
/// `"model-picker:scope:this"` and `":sticky"`, and the line about another
/// agent's one-off turn as `"model-picker:one-off"`.
#[derive(IntoElement)]
pub(crate) struct ModelPicker {
    menu: ModelMenu,
    on_event: ChangeHandler<PickerEvent>,
}

impl std::fmt::Debug for ModelPicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelPicker")
            .field("menu", &self.menu)
            .finish_non_exhaustive()
    }
}

impl ModelPicker {
    fn send(
        &self,
        event: PickerEvent,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + use<> {
        let handler = self.on_event.clone();
        move |_, window, cx| handler(event.clone(), window, cx)
    }

    /// A small uppercase section title (node 3357), as `KvSection`'s.
    fn title(text: &str) -> Label {
        Label::new(text.to_uppercase())
            .size(LabelSize::Small)
            .weight(FontWeight::SEMIBOLD)
            .color(Color::Muted)
    }

    fn list(&self) -> impl IntoElement {
        let current = &self.menu.current;
        let groups = self.menu.groups.iter().zip(self.menu.headings.iter());
        let children = groups.flat_map(|(group, heading)| {
            let heading = div()
                .pt(SPACE_6)
                .px(SPACE_10)
                .pb(SPACE_2)
                .child(Label::new(heading.clone()).code().color(Color::Placeholder))
                .into_any_element();
            let configured = self.menu.offers_configured(group).then_some(None);
            let names = group.models.iter().map(|m| Some(m.as_str()));
            let rows = configured.into_iter().chain(names).map(|model| {
                let id = format!(
                    "model-picker:{}:{}",
                    group.agent,
                    model.unwrap_or("default")
                );
                let checked = current.agent == group.agent && current.model.as_deref() == model;
                // The configured model's row reads in the placeholder color.
                let label = Label::new(model.unwrap_or("configured model"))
                    .code()
                    .color(if model.is_none() {
                        Color::Placeholder
                    } else {
                        Color::Default
                    });
                MenuRow::new(id)
                    .picker()
                    .checked(checked)
                    .on_click(self.send(PickerEvent::Model {
                        agent: group.agent.clone(),
                        model: model.map(str::to_owned),
                    }))
                    .child(label)
                    .into_any_element()
            });
            std::iter::once(heading).chain(rows).collect::<Vec<_>>()
        });
        div()
            .id("model-picker:list")
            .flex()
            .flex_col()
            .min_h_0()
            .max_h(DROPDOWN_MENU_MAX_HEIGHT)
            .overflow_y_scroll()
            .p(SPACE_4)
            .children(children)
    }

    fn scope_row(&self) -> impl IntoElement {
        let scope = self.menu.current.scope;
        div()
            .flex()
            .items_center()
            .gap(SPACE_14)
            .child(
                Radio::new("model-picker:scope:this", "this message")
                    .on(scope == ChoiceScope::ThisMessage)
                    .on_click(self.send(PickerEvent::Scope(ChoiceScope::ThisMessage))),
            )
            .child(
                Radio::new("model-picker:scope:sticky", "until I change it")
                    .on(scope == ChoiceScope::UntilChanged)
                    .on_click(self.send(PickerEvent::Scope(ChoiceScope::UntilChanged))),
            )
    }

    /// The effort section (node 3390) for an agent with levels; otherwise
    /// the scope row alone, which the mockup draws only inside it.
    fn effort(&self, rule: gpui::Hsla) -> impl IntoElement {
        let current = &self.menu.current;
        let levels = self
            .menu
            .group(&current.agent)
            .map(|group| group.efforts.clone())
            .unwrap_or_default();
        let section = div()
            .flex()
            .flex_col()
            .gap(SPACE_6)
            .py(SPACE_8)
            .px(SPACE_12)
            .border_t(BORDER_WIDTH)
            .border_color(rule);
        if levels.is_empty() {
            return section.child(self.scope_row());
        }
        let selected = current
            .effort
            .and_then(|effort| levels.iter().position(|level| *level == effort))
            .unwrap_or(usize::MAX);
        let handler = self.on_event.clone();
        let options: Vec<&str> = levels.iter().map(|level| level.as_str()).collect();
        section
            .debug_selector(|| "model-picker:effort".into())
            .child(Self::title("Effort"))
            // Node 3392 hugs its options; the column alone would stretch it.
            .child(
                div().flex().child(
                    Segmented::new("model-picker:effort-level", options, selected)
                        .code()
                        .on_change(move |index, window, cx| {
                            if let Some(level) = levels.get(index) {
                                handler(PickerEvent::Effort(*level), window, cx);
                            }
                        }),
                ),
            )
            .child(
                Label::new(effort_hint(&self.menu.groups))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder)
                    .wrap(),
            )
            .child(self.scope_row())
    }

    /// Fix 31: a model of another agent runs one turn as a one-off process
    /// of that agent, under the listener's identity (spec 4).
    fn one_off(&self) -> Option<Label> {
        let (current, listener) = (&self.menu.current, &self.menu.listener);
        (current.agent != listener.agent).then(|| {
            let model = current.model.as_deref().unwrap_or("its configured model");
            let span = match current.scope {
                ChoiceScope::ThisMessage => "for this message",
                ChoiceScope::UntilChanged => "until you change it",
            };
            Label::new(format!(
                "{model} runs on {} {span}, as {}. The listener stays on {}.",
                current.agent, self.menu.listener_handle, listener.agent
            ))
            .size(LabelSize::Small)
            .color(Color::Placeholder)
            .wrap()
        })
    }
}

impl RenderOnce for ModelPicker {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let rule = theme.border;
        let title = match self.menu.current.scope {
            ChoiceScope::ThisMessage => "Model for this message",
            ChoiceScope::UntilChanged => "Model until I change it",
        };
        let foot = format!(
            "Runs as {} with the listener's tools: it can look things up, answer, post and \
             delegate, but never edits. Spend counts against the listener's daily cap.",
            self.menu.listener_handle
        );
        let one_off = self.one_off().map(|line| {
            div()
                .debug_selector(|| "model-picker:one-off".into())
                .child(line)
        });
        div()
            .id("model-picker")
            .debug_selector(|| "model-picker".into())
            .occlude()
            .w(MODEL_PICKER_WIDTH)
            .flex()
            .flex_col()
            .rounded(RADIUS_LG)
            .border(BORDER_WIDTH)
            .border_color(rule)
            .bg(theme.elevated_surface)
            .shadow(vec![BoxShadow {
                color: black().opacity(MENU_SHADOW_ALPHA),
                offset: point(gpui::Pixels::ZERO, MENU_SHADOW_OFFSET),
                blur_radius: MENU_SHADOW_BLUR,
                spread_radius: gpui::Pixels::ZERO,
                inset: false,
            }])
            .overflow_hidden()
            .child(
                div()
                    .py(SPACE_8)
                    .px(SPACE_12)
                    .border_b(BORDER_WIDTH)
                    .border_color(rule)
                    .child(Self::title(title)),
            )
            .child(self.list())
            .child(self.effort(rule))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_8)
                    .pt(SPACE_8)
                    .px(SPACE_12)
                    .pb(SPACE_10)
                    .border_t(BORDER_WIDTH)
                    .border_color(rule)
                    .children(one_off)
                    .child(
                        Label::new(foot)
                            .size(LabelSize::Small)
                            .color(Color::Placeholder)
                            .wrap(),
                    ),
            )
    }
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
