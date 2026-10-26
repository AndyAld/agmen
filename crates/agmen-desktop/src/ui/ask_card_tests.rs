use std::cell::RefCell;
use std::rc::Rc;

use agmen_proto::StampedEnvelope;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use gpui::{
    AppContext as _, Context, Entity, Render, TestAppContext, VisualTestContext, Window, px,
};

use super::*;
use crate::backend::{
    Action, Backend, BackendError, BackendHandle, Update, chat_snapshot, fixture_id,
};
use crate::ui::chat::ChatView;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::text_input::bind_keys;
use crate::ui::tokens::MOCKUP_REM;

/// A backend that keeps what is sent.
#[derive(Default)]
struct Recorder(RefCell<Vec<Draft>>);

impl Backend for Recorder {
    fn snapshot(&self) -> Snapshot {
        chat_snapshot()
    }
    fn send(&self, draft: Draft) -> Result<(), BackendError> {
        self.0.borrow_mut().push(draft);
        Ok(())
    }
    fn act(&self, _: Action) -> Result<(), BackendError> {
        Ok(())
    }
    fn subscribe(&self) -> UnboundedReceiver<Update> {
        unbounded().1
    }
}

/// `msg` turned into a message from andy/fx1 with `verb` and `data`,
/// numbered after `after`.
fn from_fx1(
    after: &[StampedEnvelope],
    verb: &str,
    data: serde_json::Value,
    reply_to: Option<&StampedEnvelope>,
) -> StampedEnvelope {
    let mut value = serde_json::to_value(after.last().unwrap()).unwrap();
    let seq = after.len() + 1;
    value["id"] = format!("01JC{seq:022}").into();
    value["seq"] = seq.into();
    value["verb"] = verb.into();
    value["data"] = data;
    value["body"] = "Rebase onto main or merge it?".into();
    value["reply_to"] = reply_to.map_or(serde_json::Value::Null, |r| {
        serde_json::to_value(&r.client.id).unwrap()
    });
    value["to"] = serde_json::json!([]);
    value["mentioned"] = serde_json::json!([]);
    value["refs"] = serde_json::json!([]);
    value["from"] = serde_json::to_value(fixture_id("andy/fx1")).unwrap();
    value["from_handle"] = "andy/fx1".into();
    value["from_kind"] = "agent".into();
    serde_json::from_value(value).unwrap()
}

/// The chat fixture plus an ASK from andy/fx1 whose `of` is `to`.
fn with_ask(to: &str) -> (Snapshot, StampedEnvelope) {
    let mut snapshot = chat_snapshot();
    let of = serde_json::to_value(fixture_id(to)).unwrap();
    let ask = from_fx1(
        &snapshot.messages,
        "ASK",
        serde_json::json!({ "of": of }),
        None,
    );
    snapshot.messages.push(ask.clone());
    (snapshot, ask)
}

fn sel(env: &StampedEnvelope, part: &str) -> &'static str {
    Box::leak(format!("{}:{part}", env.client.id.as_str()).into_boxed_str())
}

struct Host(Entity<ChatView>);

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div()
            .w(px(800.))
            .h(px(900.))
            .flex()
            .flex_col()
            .child(self.0.clone())
    }
}

fn open_chat(
    cx: &mut TestAppContext,
    snapshot: Snapshot,
) -> (Rc<Recorder>, &mut VisualTestContext) {
    init(cx);
    cx.update(bind_keys);
    let recorder = Rc::new(Recorder::default());
    let handle: Rc<dyn Backend> = recorder.clone();
    cx.update(|cx| cx.set_global(BackendHandle(handle)));
    let (_, cx) = cx.add_window_view(|window, cx| {
        Host(cx.new(|cx| ChatView::new(std::rc::Rc::new(snapshot), window, cx)))
    });
    redraw(cx);
    (recorder, cx)
}

#[gpui::test]
fn ask_card_answer_sends_reply_to(cx: &mut TestAppContext) {
    let (snapshot, ask) = with_ask("andy");
    let (recorder, cx) = open_chat(cx, snapshot);
    click(cx, sel(&ask, "answer:1"));
    assert_eq!(
        *recorder.0.borrow(),
        [Draft {
            to: vec![fixture_id("andy/fx1")],
            body: "No".into(),
            reply_to: Some(ask.client.id.clone()),
            model: None,
        }]
    );
}

#[gpui::test]
fn an_ask_for_someone_else_has_no_buttons(cx: &mut TestAppContext) {
    let (snapshot, ask) = with_ask("sean");
    let (_, cx) = open_chat(cx, snapshot);
    assert!(cx.debug_bounds(sel(&ask, "row")).is_some());
    assert!(cx.debug_bounds(sel(&ask, "answer:0")).is_none());
}

#[gpui::test]
fn an_answered_ask_says_so(cx: &mut TestAppContext) {
    let (mut snapshot, ask) = with_ask("andy");
    let reply = from_fx1(
        &snapshot.messages,
        "ANSWER",
        serde_json::json!({}),
        Some(&ask),
    );
    snapshot.messages.push(reply);
    let (_, cx) = open_chat(cx, snapshot);
    assert!(cx.debug_bounds(sel(&ask, "answer:0")).is_none());
    assert!(cx.debug_bounds(sel(&ask, "answered")).is_some());
}

#[test]
fn an_ask_to_me_by_to_counts() {
    let (mut snapshot, mut ask) = with_ask("sean");
    assert!(!asks_me(&ask, &snapshot));
    ask.client.to = vec![snapshot.me_id.clone()];
    assert!(asks_me(&ask, &snapshot));
    snapshot.messages.clear();
    assert!(!answered(&ask, &snapshot));
}
