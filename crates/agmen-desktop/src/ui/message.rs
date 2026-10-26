//! One chat message as a row (`zmessage` in
//! `docs/mockups/scripts/console.py`; frame "Desktop · chat", rows 138 to
//! 350): avatar, handle, role, verb badge, time and body with reference
//! chips. Replies indent under their parent, a message that mentions me gets
//! a yellow bar, and ledger verbs such as GRANT draw as one dim line.

use std::rc::Rc;

use agmen_proto::{IdentityKind, Payload, StampedEnvelope, Verb, Verdict};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::change_card::ChangeCard;
use super::components::{Avatar, Badge, ChangeHandler, Color, Icon, IconSize, Label, LabelSize};
use super::format::title_case;
use super::ref_chip::{ChatContext, RefEvent};
use super::rich_text::RichText;
use super::tokens::{
    BORDER_WIDTH, MENTION_BAR_WIDTH, MESSAGE_AVATAR_SIZE, RADIUS_SM, REPLY_INDENT, SPACE_1,
    SPACE_2, SPACE_3, SPACE_5, SPACE_6, SPACE_7, SPACE_8, SPACE_10, TINT_BORDER, TINT_FILL,
    TINT_MENTION, tint,
};
use crate::appearance::Appearance;
use crate::backend::Action;
use crate::icons::UiIcon;

/// How a message row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RowKind {
    Message,
    /// Indented under the message it answers.
    Reply,
    /// Mentions me: yellow bar and tint.
    Mention,
    /// A bookkeeping verb, as one dim line with no avatar or chips.
    Ledger,
}

/// Whether `env` is ledger bookkeeping rather than conversation: anything
/// the hub sends, and the verbs that track presence, claims and state.
#[must_use]
pub(crate) fn is_ledger(env: &StampedEnvelope) -> bool {
    env.from_kind == IdentityKind::Hub
        || matches!(
            env.client.payload.verb(),
            Some(
                Verb::Hello
                    | Verb::Bye
                    | Verb::Rollcall
                    | Verb::Grant
                    | Verb::Deny
                    | Verb::Heartbeat
                    | Verb::Release
                    | Verb::State
            )
        )
}

/// How `env` is drawn. A reply indents only when its parent is in the chat;
/// a REVIEW or DECISION is a verdict and stays at the left, as the mockup
/// draws them (CHAT and PLAN_READY in console.py). A reply that mentions me
/// stays a reply: the hub mentions a reply's human parent author, so every
/// answer to me would otherwise turn yellow.
#[must_use]
pub(crate) fn row_kind(env: &StampedEnvelope, context: &ChatContext) -> RowKind {
    let me = &context.snapshot().me_id;
    let verdict = matches!(
        env.client.payload.verb(),
        Some(Verb::Review | Verb::Decision)
    );
    if is_ledger(env) {
        RowKind::Ledger
    } else if !verdict
        && env
            .client
            .reply_to
            .as_ref()
            .is_some_and(|parent| context.has_message(parent))
    {
        RowKind::Reply
    } else if &env.from != me && env.mentioned.contains(me) {
        RowKind::Mention
    } else {
        RowKind::Message
    }
}

/// The verb badge's label and color (`VERB_COLOR` in console.py). A
/// message carrying a change card reads SETTINGS though the wire verb is
/// ANSWER (V67); a person's FYI addressed to someone reads "to" and the target,
/// the sender's own prefix dropped ("to listen" for andy to andy/listen); a
/// REVIEW is green for pass and red for changes (mockup fix 42).
#[must_use]
pub(crate) fn verb_badge(env: &StampedEnvelope, context: &ChatContext) -> (String, Color) {
    if context.snapshot().changes.contains_key(&env.client.id) {
        return ("SETTINGS".to_owned(), Color::Accent);
    }
    let payload = &env.client.payload;
    if matches!(payload, Payload::Fyi {}) && !env.client.to.is_empty() {
        let own = format!("{}/", env.from_handle);
        let targets: Vec<String> = env
            .client
            .to
            .iter()
            .map(|id| {
                let handle = context.handle(id).unwrap_or(id.as_str());
                handle.strip_prefix(&own).unwrap_or(handle).to_owned()
            })
            .collect();
        return (format!("to {}", targets.join(", ")), Color::Accent);
    }
    let Some(verb) = payload.verb() else {
        let label = match payload {
            Payload::Unknown { verb, .. } => verb.clone(),
            _ => String::new(),
        };
        return (label, Color::Default);
    };
    let color = match (verb, payload) {
        (Verb::Done | Verb::Decision, _)
        | (
            Verb::Review,
            Payload::Review {
                verdict: Verdict::Pass,
                ..
            },
        ) => Color::Success,
        (Verb::Alert | Verb::Review, _) => Color::Error,
        (Verb::Delegated | Verb::Proposal, _) => Color::Accent,
        (Verb::Grant | Verb::State, _) => Color::Muted,
        (Verb::Assign, _) => Color::Player(1),
        (Verb::Claim, _) => Color::Player(3),
        _ => Color::Default,
    };
    (verb.as_str().to_owned(), color)
}

/// Who sent a message, as the row's head shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sender {
    /// "Engineer", "Listener", "human" or "human · coordinator".
    pub(crate) label: String,
    pub(crate) color: Color,
    pub(crate) agent: bool,
}

/// An agent by its roster role (or its kind when the roster lacks it, and
/// as an engineer when nothing says, as console.py does); a person as
/// "human".
#[must_use]
pub(crate) fn sender(env: &StampedEnvelope, context: &ChatContext) -> Sender {
    if !env.from_handle.contains('/') {
        let label = if env.from_kind == IdentityKind::Coordinator {
            "human · coordinator"
        } else {
            "human"
        };
        return Sender {
            label: label.to_owned(),
            color: Color::Placeholder,
            agent: false,
        };
    }
    let role = context
        .role(&env.from_handle)
        .or(match env.from_kind {
            IdentityKind::Listener => Some("listener"),
            IdentityKind::Coordinator => Some("coordinator"),
            IdentityKind::Human | IdentityKind::Agent | IdentityKind::Hub => None,
        })
        .map(str::to_owned);
    let (label, color) = role.map_or_else(
        || ("agent".to_owned(), Color::Role("engineer".into())),
        |role| (title_case(&role), Color::Role(role.into())),
    );
    Sender {
        label,
        color,
        agent: true,
    }
}

/// Whether the row offers "Open in terminal": a DONE from an agent this
/// person's agmend runs (plan Task 9; spec 5.4 allows take-over only there).
#[must_use]
pub(crate) fn offers_take_over(env: &StampedEnvelope, context: &ChatContext) -> bool {
    matches!(env.client.payload, Payload::Done { .. }) && context.is_local_agent(&env.from_handle)
}

/// One message. Tests find its parts as `"{message id}:row"` (or
/// `":ledger"`), `":avatar"`, `":verb"`, `":mention-bar"`,
/// `":open-in-terminal"` and `":ref:{n}"`.
#[derive(IntoElement)]
pub(crate) struct MessageRow {
    env: StampedEnvelope,
    context: Rc<ChatContext>,
    on_ref: Option<ChangeHandler<RefEvent>>,
    on_action: Option<ChangeHandler<Action>>,
}

impl std::fmt::Debug for MessageRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessageRow")
            .field("id", &self.env.client.id)
            .finish_non_exhaustive()
    }
}

impl MessageRow {
    #[must_use]
    pub(crate) const fn new(env: StampedEnvelope, context: Rc<ChatContext>) -> Self {
        Self {
            env,
            context,
            on_ref: None,
            on_action: None,
        }
    }

    /// Called when a reference chip in the body is clicked or ⌘-clicked.
    #[must_use]
    pub(crate) fn on_ref(
        mut self,
        handler: impl Fn(RefEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_ref = Some(Rc::new(handler));
        self
    }

    /// Called with "Open in terminal" (`Action::TakeOver`) and the change
    /// card's Apply, Cancel and Undo; the caller passes it to `Backend::act`.
    #[must_use]
    pub(crate) fn on_action(
        mut self,
        handler: impl Fn(Action, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }

    fn ledger(self, id: &str) -> gpui::AnyElement {
        let env = &self.env;
        let selector = format!("{id}:ledger");
        let line = format!(
            "{} {} {}",
            env.from_handle,
            verb_name(env),
            env.client.body.as_str()
        );
        div()
            .debug_selector(|| selector)
            .w_full()
            .flex()
            .items_start()
            .gap(SPACE_10)
            .py(SPACE_2)
            .pr(SPACE_10)
            .pl(SPACE_10 + REPLY_INDENT)
            .child(
                Label::new(self.context.clock(env.hub_at))
                    .code()
                    .color(Color::Muted),
            )
            // Mockup fix 32: the ledger line wraps like a body.
            .child(Label::new(line).code().color(Color::Muted).wrap())
            .into_any_element()
    }

    fn take_over_chip(&self, id: &str, cx: &App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let selector = format!("{id}:open-in-terminal");
        let element_id = SharedString::from(selector.clone());
        let worker = self.env.from.clone();
        div()
            .id(element_id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5)
            .py(SPACE_1)
            .px(SPACE_7)
            .rounded(RADIUS_SM)
            .bg(tint(theme.text_accent, TINT_FILL))
            .border(BORDER_WIDTH)
            .border_color(tint(theme.text_accent, TINT_BORDER))
            .when_some(self.on_action.clone(), |this, handler| {
                this.cursor_pointer().on_click(move |_, window, cx| {
                    handler(Action::TakeOver(worker.clone()), window, cx);
                })
            })
            .child(
                Icon::new(UiIcon::Terminal)
                    .size(IconSize::Small)
                    .color(Color::Accent),
            )
            .child(
                Label::new("Open in terminal")
                    .size(LabelSize::Small)
                    .color(Color::Accent),
            )
            .child(Label::new("⌥O").size(LabelSize::Small).color(Color::Accent))
    }
}

/// The verb as the wire names it, for a ledger line.
fn verb_name(env: &StampedEnvelope) -> String {
    match &env.client.payload {
        Payload::Unknown { verb, .. } => verb.clone(),
        payload => payload
            .verb()
            .map(Verb::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

impl MessageRow {
    /// The avatar: an agent's role initial on a square in its role color, a
    /// person's initial on a round dim fill (node 139 and 207).
    fn avatar(&self, who: &Sender, id: &str) -> impl IntoElement {
        let avatar = if who.agent {
            Avatar::new(&who.label)
                .color(who.color.clone())
                .fill(who.color.clone())
                .square()
        } else {
            Avatar::new(&self.env.from_handle)
                .color(Color::Default)
                .fill(Color::Placeholder)
        };
        let selector = format!("{id}:avatar");
        div()
            .debug_selector(|| selector)
            .flex_none()
            .child(avatar.size(MESSAGE_AVATAR_SIZE))
    }

    /// Handle, role, verb badge, time and the take-over chip (node 142).
    fn head(&self, who: &Sender, id: &str, cx: &App) -> impl IntoElement {
        let env = &self.env;
        let (verb, verb_color) = verb_badge(env, &self.context);
        let verb_sel = format!("{id}:verb");
        div()
            .flex()
            .items_center()
            .gap(SPACE_8)
            .child(
                Label::new(env.from_handle.clone())
                    .code()
                    .weight(FontWeight::BOLD)
                    .color(Color::TextMuted),
            )
            .child(
                Label::new(who.label.clone())
                    .size(LabelSize::Small)
                    .color(who.color.clone()),
            )
            .child(
                div()
                    .debug_selector(|| verb_sel)
                    .child(Badge::new(verb, verb_color).code()),
            )
            .child(
                Label::new(self.context.clock(env.hub_at))
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .when(offers_take_over(env, &self.context), |this| {
                this.child(self.take_over_chip(id, cx))
            })
    }
}

impl RenderOnce for MessageRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.env.client.id.as_str().to_owned();
        let context = self.context.clone();
        let kind = row_kind(&self.env, &context);
        if kind == RowKind::Ledger {
            return self.ledger(&id);
        }
        let env = &self.env;
        let warning = cx.global::<Appearance>().theme.warning;
        let who = sender(env, &context);
        let avatar = self.avatar(&who, &id);
        let head = self.head(&who, &id, cx);
        let body = RichText::new(
            id.clone(),
            env.client.body.as_str().to_owned(),
            &env.refs,
            context.clone(),
        )
        .when_some(self.on_ref.clone(), RichText::on_ref);
        let card = context.snapshot().changes.get(&env.client.id).map(|card| {
            ChangeCard::new(card.clone(), &context.snapshot().roster).when_some(
                self.on_action.clone(),
                |card, handler| {
                    card.on_action(move |action, window, cx| handler(action, window, cx))
                },
            )
        });
        let indent = if kind == RowKind::Reply {
            SPACE_10 + REPLY_INDENT
        } else {
            SPACE_10
        };
        let (row_sel, bar_sel) = (format!("{id}:row"), format!("{id}:mention-bar"));
        let inner = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_start()
            .gap(SPACE_10)
            .py(SPACE_6)
            .pr(SPACE_10)
            .pl(indent)
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(SPACE_3)
                    .child(head)
                    .child(body)
                    .children(card),
            );
        div()
            .debug_selector(|| row_sel)
            .w_full()
            .flex()
            .when(kind == RowKind::Mention, |this| {
                this.bg(tint(warning, TINT_MENTION)).child(
                    div()
                        .debug_selector(|| bar_sel)
                        .w(MENTION_BAR_WIDTH)
                        .flex_none()
                        .bg(warning),
                )
            })
            .child(inner)
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "message_tests.rs"]
mod tests;
