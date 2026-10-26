use std::cell::RefCell;
use std::rc::Rc;

use agmen_proto::{IdentityKind, Payload, StampedEnvelope, Verdict};
use gpui::{AnyElement, Context, Render, TestAppContext, VisualTestContext, Window, div, px};

use super::*;
use crate::backend::{Backend, FixtureBackend, Screen, Snapshot, chat_snapshot};
use crate::refs::RefTarget;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{MENTION_BAR_WIDTH, MOCKUP_REM, REPLY_INDENT};

/// Mountain daylight time, as the mockup's clock shows.
const MDT: i32 = -6 * 60;

fn snapshot_of(screen: Screen) -> Snapshot {
    FixtureBackend::for_screen(screen).unwrap().snapshot()
}

fn context_of(snapshot: Snapshot) -> Rc<ChatContext> {
    Rc::new(ChatContext::new(Rc::new(snapshot)).with_utc_offset(MDT))
}

fn chat() -> (Vec<StampedEnvelope>, Rc<ChatContext>) {
    let snapshot = chat_snapshot();
    (snapshot.messages.clone(), context_of(snapshot))
}

fn sel(env: &StampedEnvelope, part: &str) -> &'static str {
    Box::leak(format!("{}:{part}", env.client.id.as_str()).into_boxed_str())
}

#[test]
fn verb_badge_colors() {
    let (messages, c) = chat();
    let badges: Vec<(String, Color)> = messages.iter().map(|m| verb_badge(m, &c)).collect();
    let expected = [
        ("DONE", Color::Success),
        ("ALERT", Color::Error),
        ("to listen", Color::Accent),
        ("ANSWER", Color::Default),
        ("DELEGATED", Color::Accent),
        ("GRANT", Color::Muted),
        ("DONE", Color::Success),
        // Mockup fix 42: a passing REVIEW is green, not red.
        ("REVIEW", Color::Success),
        ("FYI", Color::Default),
    ];
    let expected: Vec<(String, Color)> = expected
        .into_iter()
        .map(|(l, c)| (l.to_owned(), c))
        .collect();
    assert_eq!(badges, expected);

    let mut changes = messages[7].clone();
    changes.client.payload = Payload::Review {
        verdict: Verdict::Changes,
        findings: Vec::new(),
        traced: None,
    };
    assert_eq!(verb_badge(&changes, &c), ("REVIEW".into(), Color::Error));

    let mut claim = messages[6].clone();
    claim.client.payload = Payload::Claim {
        item: "C-015".parse().unwrap(),
    };
    assert_eq!(verb_badge(&claim, &c), ("CLAIM".into(), Color::Player(3)));

    let running = snapshot_of(Screen::Running);
    let rc = context_of(running.clone());
    let label = |verb: &str| {
        running
            .messages
            .iter()
            .map(|m| verb_badge(m, &rc))
            .find(|(l, _)| l == verb)
    };
    assert_eq!(label("to sean/plan").map(|b| b.1), Some(Color::Accent));
    assert_eq!(label("ASSIGN").map(|b| b.1), Some(Color::Player(1)));
    assert_eq!(label("DECISION").map(|b| b.1), Some(Color::Success));
    assert_eq!(label("PROPOSAL").map(|b| b.1), Some(Color::Accent));
}

#[test]
fn settings_reply_is_labelled_from_its_change_card() {
    // V67: the wire verb is ANSWER; the card makes it a SETTINGS message.
    let snapshot = snapshot_of(Screen::SettingsFromChat);
    let c = context_of(snapshot.clone());
    let (id, _) = snapshot.changes.iter().next().unwrap();
    let m = snapshot
        .messages
        .iter()
        .find(|m| &m.client.id == id)
        .unwrap();
    assert_eq!(m.client.payload.verb(), Some(agmen_proto::Verb::Answer));
    assert_eq!(verb_badge(m, &c), ("SETTINGS".into(), Color::Accent));
}

#[test]
fn rows_take_the_mockup_kinds() {
    let (messages, c) = chat();
    let kinds: Vec<RowKind> = messages.iter().map(|m| row_kind(m, &c)).collect();
    use RowKind::{Ledger, Mention, Message, Reply};
    assert_eq!(
        kinds,
        [
            Message, Message, Message, Reply, Reply, Ledger, Message, Message, Mention
        ]
    );
}

#[test]
fn reply_without_its_parent_is_a_plain_message() {
    let mut snapshot = chat_snapshot();
    snapshot.messages.remove(2);
    let c = context_of(snapshot.clone());
    // The ANSWER that replied to the removed FYI sits at the left; it
    // mentions andy, so it is drawn as a mention.
    assert_eq!(row_kind(&snapshot.messages[2], &c), RowKind::Mention);
}

#[test]
fn senders_show_role_or_human() {
    let (messages, c) = chat();
    let s = sender(&messages[0], &c);
    assert_eq!(
        (s.label.as_str(), s.color, s.agent),
        ("Engineer", Color::Role("engineer".into()), true)
    );
    let listen = sender(&messages[3], &c);
    assert_eq!(listen.label, "Listener");
    let andy = sender(&messages[2], &c);
    assert_eq!(
        (andy.label.as_str(), andy.color, andy.agent),
        ("human", Color::Placeholder, false)
    );
    let mut adam = messages[8].clone();
    adam.from_kind = IdentityKind::Coordinator;
    assert_eq!(sender(&adam, &c).label, "human · coordinator");
}

#[test]
fn times_show_at_the_offset() {
    let (messages, c) = chat();
    assert_eq!(c.clock(messages[0].hub_at), "15:27");
    assert_eq!(c.clock(messages[8].hub_at), "15:45");
    let utc = ChatContext::new(Rc::new(chat_snapshot())).with_utc_offset(-22 * 60);
    // 21:27 UTC minus 22 hours wraps to the day before.
    assert_eq!(utc.clock(messages[0].hub_at), "23:27");
}

type Heard<T> = Rc<RefCell<Vec<T>>>;

/// Draws message rows in a column `width` wide and records what they report.
struct Host {
    width: gpui::Pixels,
    rows: Vec<StampedEnvelope>,
    context: Rc<ChatContext>,
    refs: Heard<RefEvent>,
    actions: Heard<Action>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        let rows = self.rows.iter().map(|env| {
            let (refs, actions) = (self.refs.clone(), self.actions.clone());
            MessageRow::new(env.clone(), self.context.clone())
                .on_ref(move |event, _, _| refs.borrow_mut().push(event))
                .on_action(move |action, _, _| actions.borrow_mut().push(action))
                .into_any_element()
        });
        div()
            .w(self.width)
            .flex()
            .flex_col()
            .children(rows.collect::<Vec<AnyElement>>())
    }
}

struct Opened<'a> {
    cx: &'a mut VisualTestContext,
    refs: Heard<RefEvent>,
    actions: Heard<Action>,
}

fn open(
    cx: &mut TestAppContext,
    width: gpui::Pixels,
    rows: Vec<StampedEnvelope>,
    context: Rc<ChatContext>,
) -> Opened<'_> {
    init(cx);
    let refs: Heard<RefEvent> = Rc::default();
    let actions: Heard<Action> = Rc::default();
    let (r, a) = (refs.clone(), actions.clone());
    let (_, cx) = cx.add_window_view(move |_, _| Host {
        width,
        rows,
        context,
        refs: r,
        actions: a,
    });
    redraw(cx);
    Opened { cx, refs, actions }
}

fn avatar_x(cx: &mut VisualTestContext, env: &StampedEnvelope) -> gpui::Pixels {
    cx.debug_bounds(sel(env, "avatar")).unwrap().origin.x
}

#[gpui::test]
fn reply_indents_under_parent(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    let o = open(cx, px(800.), messages.clone(), c);
    let parent = avatar_x(o.cx, &messages[2]);
    assert_eq!(avatar_x(o.cx, &messages[3]) - parent, REPLY_INDENT);
    assert_eq!(avatar_x(o.cx, &messages[4]) - parent, REPLY_INDENT);
    // A REVIEW stays at the left, as the mockup draws it.
    assert_eq!(avatar_x(o.cx, &messages[7]), parent);
}

#[gpui::test]
fn mention_of_me_gets_yellow_bar(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    assert_eq!(row_kind(&messages[8], &c), RowKind::Mention);
    let o = open(cx, px(800.), messages.clone(), c);
    // The bar sits inside the row, so the avatar moves right by its width.
    let plain = avatar_x(o.cx, &messages[7]);
    assert_eq!(avatar_x(o.cx, &messages[8]) - plain, MENTION_BAR_WIDTH);
    assert!(
        o.cx.debug_bounds(sel(&messages[8], "mention-bar"))
            .is_some()
    );
    assert!(
        o.cx.debug_bounds(sel(&messages[7], "mention-bar"))
            .is_none()
    );
}

#[gpui::test]
fn message_wraps_long_token(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    let short = messages[7].clone();
    let mut long = messages[8].clone();
    long.client.body = "x".repeat(500).try_into().unwrap();
    let o = open(cx, px(600.), vec![short.clone(), long.clone()], c);
    let one_line = o.cx.debug_bounds(sel(&short, "row")).unwrap().size;
    let bounds = o.cx.debug_bounds(sel(&long, "row")).unwrap();
    let row = bounds.size;
    assert!(row.width <= px(600.), "{row:?}");
    // No horizontal overflow: the body ends inside the row.
    let body = o.cx.debug_bounds(sel(&long, "body")).unwrap();
    assert!(body.right() <= bounds.right(), "{body:?} in {bounds:?}");
    // Unwrapped, the token would overflow at one line's height; wrapped, it
    // takes at least one more line (the test text system's glyphs are
    // narrower than a real font's, so it is not six).
    assert!(
        row.height > one_line.height + px(15.),
        "{row:?} vs {one_line:?}"
    );
}

#[gpui::test]
fn fixture_chat_renders_the_sheet_chips(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    // The sheet's section 4 table: chips per CHAT message, the GRANT ledger
    // line drawing none.
    let counts = [4, 3, 0, 4, 2, 0, 2, 0, 2];
    let o = open(cx, px(800.), messages.clone(), c);
    for (env, count) in messages.iter().zip(counts) {
        for i in 0..count {
            let s = sel(env, &format!("ref:{i}"));
            assert!(o.cx.debug_bounds(s).is_some(), "{s} not drawn");
        }
        let past = sel(env, &format!("ref:{count}"));
        assert!(o.cx.debug_bounds(past).is_none(), "{past} drawn");
    }
    assert!(o.cx.debug_bounds(sel(&messages[5], "ledger")).is_some());
    assert!(o.cx.debug_bounds(sel(&messages[5], "avatar")).is_none());
}

#[gpui::test]
fn chip_in_a_row_reports_open_ref(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    let o = open(cx, px(800.), messages.clone(), c);
    click(o.cx, sel(&messages[4], "ref:0"));
    assert_eq!(
        *o.refs.borrow(),
        [RefEvent::OpenRef(RefTarget::Agent("andy/fx1".into()))]
    );
}

#[gpui::test]
fn open_in_terminal_on_a_local_agents_done(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    let o = open(cx, px(800.), messages.clone(), c.clone());
    let fx1_done = &messages[6];
    assert!(
        o.cx.debug_bounds(sel(fx1_done, "open-in-terminal"))
            .is_some()
    );
    // andy/a1 is andy's too; its ALERT is not a DONE.
    assert!(
        o.cx.debug_bounds(sel(&messages[0], "open-in-terminal"))
            .is_some()
    );
    for i in [1, 2, 3, 4, 7, 8] {
        assert!(
            o.cx.debug_bounds(sel(&messages[i], "open-in-terminal"))
                .is_none(),
            "message {i}"
        );
    }
    click(o.cx, sel(fx1_done, "open-in-terminal"));
    assert_eq!(
        *o.actions.borrow(),
        [Action::TakeOver(fx1_done.from.clone())]
    );

    // sean's agents run on sean's machine: no take-over.
    let mut seans = fx1_done.clone();
    seans.from_handle = "sean/a1".into();
    assert!(!offers_take_over(&seans, &c));
}

#[gpui::test]
fn settings_message_hosts_its_change_card(cx: &mut TestAppContext) {
    let snapshot = snapshot_of(Screen::SettingsFromChat);
    let (id, _) = snapshot.changes.iter().next().unwrap();
    let m = snapshot
        .messages
        .iter()
        .find(|m| &m.client.id == id)
        .unwrap()
        .clone();
    let o = open(cx, px(800.), vec![m], context_of(snapshot));
    assert!(o.cx.debug_bounds("chg-fx1-lanes-budget:waiting").is_some());
    click(o.cx, "chg-fx1-lanes-budget:apply");
    assert_eq!(
        *o.actions.borrow(),
        [Action::Apply(crate::backend::ChangeId(
            "chg-fx1-lanes-budget".into()
        ))]
    );
}
