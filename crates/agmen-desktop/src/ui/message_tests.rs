use std::cell::RefCell;
use std::rc::Rc;

use agmen_proto::{IdentityKind, Payload, StampedEnvelope, Verdict};
use gpui::{AnyElement, Context, Render, TestAppContext, VisualTestContext, Window, div, px};

use super::*;
use crate::backend::{Backend, FixtureBackend, Screen, Snapshot, chat_snapshot};
use crate::refs::RefTarget;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{MENTION_BAR_WIDTH, MOCKUP_REM, REPLY_INDENT};
use jiff::tz::{TimeZone, offset};

/// Mountain daylight time, as the mockup's clock shows.
const MDT: TimeZone = TimeZone::fixed(offset(-6));

fn snapshot_of(screen: Screen) -> Snapshot {
    FixtureBackend::for_screen(screen).unwrap().snapshot()
}

fn context_of(snapshot: Snapshot) -> Rc<ChatContext> {
    Rc::new(ChatContext::new(Rc::new(snapshot)).with_zone(MDT))
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

// Shot diff (running, nodes 10739, 10776, 10806): a team agent's row shows
// its plan role, as the roster would once it runs.
#[test]
fn team_agents_show_their_plan_role() {
    let snapshot = snapshot_of(Screen::Running);
    let messages = snapshot.messages.clone();
    let c = context_of(snapshot);
    let of = |handle: &str| {
        let env = messages.iter().find(|m| m.from_handle == handle).unwrap();
        let who = sender(env, &c);
        (who.label, who.color)
    };
    assert_eq!(
        of("andy/core"),
        ("Engineer".into(), Color::Role("engineer".into()))
    );
    assert_eq!(
        of("sean/sync"),
        ("Engineer".into(), Color::Role("engineer".into()))
    );
    assert_eq!(
        of("sean/tiles"),
        ("Researcher".into(), Color::Role("researcher".into()))
    );
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
fn times_show_in_the_zone() {
    let (messages, c) = chat();
    assert_eq!(c.clock(messages[0].hub_at), "15:27");
    assert_eq!(c.clock(messages[8].hub_at), "15:45");
    let east = ChatContext::new(Rc::new(chat_snapshot())).with_zone(TimeZone::fixed(offset(14)));
    // 21:27 UTC plus 14 hours wraps to the next day.
    assert_eq!(east.clock(messages[0].hub_at), "11:27");
}

#[test]
fn times_follow_daylight_saving_per_message() {
    // Each time takes its own offset in the zone, not one read at launch:
    // Denver is UTC-6 in October and UTC-7 in December.
    let denver = TimeZone::get("America/Denver").unwrap();
    let c = ChatContext::new(Rc::new(chat_snapshot())).with_zone(denver);
    assert_eq!(c.clock("2026-10-26T21:27:00Z".parse().unwrap()), "15:27");
    assert_eq!(c.clock("2026-12-26T21:27:00Z".parse().unwrap()), "14:27");
}

type Heard<T> = Rc<RefCell<Vec<T>>>;

/// Draws the context's messages at `rows` in a column `width` wide and
/// records what they report.
struct Host {
    width: gpui::Pixels,
    rows: Vec<usize>,
    context: Rc<ChatContext>,
    refs: Heard<RefEvent>,
    actions: Heard<Action>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        let rows = self.rows.iter().map(|&index| {
            let (refs, actions) = (self.refs.clone(), self.actions.clone());
            MessageRow::new(index, self.context.clone())
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
    rows: Vec<usize>,
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
    let o = open(cx, px(800.), (0..messages.len()).collect(), c);
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
    let o = open(cx, px(800.), (0..messages.len()).collect(), c);
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
    let mut snapshot = chat_snapshot();
    snapshot.messages[8].client.body = "x".repeat(500).try_into().unwrap();
    let (short, long) = (snapshot.messages[7].clone(), snapshot.messages[8].clone());
    let o = open(cx, px(600.), vec![7, 8], context_of(snapshot));
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
    let o = open(cx, px(800.), (0..messages.len()).collect(), c);
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
    let o = open(cx, px(800.), (0..messages.len()).collect(), c);
    click(o.cx, sel(&messages[4], "ref:0"));
    assert_eq!(
        *o.refs.borrow(),
        [RefEvent::OpenRef(RefTarget::Agent("andy/fx1".into()))]
    );
}

#[gpui::test]
fn open_in_terminal_on_a_local_agents_done(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    let o = open(cx, px(800.), (0..messages.len()).collect(), c.clone());
    let fx1_done = &messages[6];
    assert!(
        o.cx.debug_bounds(sel(fx1_done, "open-in-terminal"))
            .is_some()
    );
    // andy/a1 is andy's too, but has no live session on this machine to
    // take over, so its DONE offers nothing: the mockup draws the button on
    // fx1's DONE only (Task 14, review C224).
    for i in [0, 1, 2, 3, 4, 7, 8] {
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
    let index = snapshot
        .messages
        .iter()
        .position(|e| e.client.id == m.client.id);
    let o = open(cx, px(800.), vec![index.unwrap()], context_of(snapshot));
    assert!(o.cx.debug_bounds("chg-fx1-lanes-budget:waiting").is_some());
    click(o.cx, "chg-fx1-lanes-budget:apply");
    assert_eq!(
        *o.actions.borrow(),
        [Action::Apply(crate::backend::ChangeId(
            "chg-fx1-lanes-budget".into()
        ))]
    );
}

/// Asserts the element `part` of `env` ends inside its row.
fn inside_row(cx: &mut VisualTestContext, env: &StampedEnvelope, part: &str) {
    let row = cx.debug_bounds(sel(env, "row")).unwrap();
    let part_bounds = cx
        .debug_bounds(sel(env, part))
        .unwrap_or_else(|| panic!("{part} not drawn"));
    assert!(
        part_bounds.right() <= row.right(),
        "{part}: {part_bounds:?} past {row:?}"
    );
}

// Head 142 is 15 px: the 13 px bold handle (node 143) on its auto line of
// 15 and the verb badge (node 145), mono 11 on an auto line of 13 inside
// 1 px padding, so one-line messages repeat every 48 px. Ledger row 287 is
// 18 px: 12 px mono on an auto line of 14, padded 2 and 2. The app's code
// text stays 13 px (C242); only the line heights follow the mockup (V113).
#[gpui::test]
fn the_head_and_the_ledger_take_the_mockups_line_heights(cx: &mut TestAppContext) {
    let (messages, c) = chat();
    let o = open(cx, px(800.), vec![0, 5], c);
    let head = o.cx.debug_bounds(sel(&messages[0], "head")).unwrap();
    let verb = o.cx.debug_bounds(sel(&messages[0], "verb")).unwrap();
    assert_eq!(verb.size.height, px(15.), "{verb:?}");
    assert_eq!(head.size.height, px(15.), "{head:?}");
    let ledger = o.cx.debug_bounds(sel(&messages[5], "ledger")).unwrap();
    assert_eq!(ledger.size.height, px(18.), "{ledger:?}");
}

#[gpui::test]
fn narrow_row_wraps_the_head_instead_of_cutting_it(cx: &mut TestAppContext) {
    // A two-pane split at 1280 px leaves the chat column about 256 px.
    let (messages, c) = chat();
    let o = open(cx, px(300.), vec![0, 6], c);
    for env in [&messages[0], &messages[6]] {
        inside_row(o.cx, env, "verb");
    }
    // Only fx1's DONE offers Open in terminal (Task 14); its chip wraps the
    // head to a second line.
    let fx1 = &messages[6];
    inside_row(o.cx, fx1, "open-in-terminal");
    let head = o.cx.debug_bounds(sel(fx1, "head")).unwrap();
    let verb = o.cx.debug_bounds(sel(fx1, "verb")).unwrap();
    assert!(head.size.height > verb.size.height * 1.5, "{head:?}");
}

#[gpui::test]
fn narrow_row_keeps_a_long_file_chip_inside(cx: &mut TestAppContext) {
    let path = "crates/agmen-desktop/src/backend/fixture_data.rs";
    let mut snapshot = chat_snapshot();
    snapshot.git.files.insert(path.into());
    snapshot.messages[8].client.body = format!("see {path}:239 now").try_into().unwrap();
    snapshot.messages[8].refs.clear();
    let env = snapshot.messages[8].clone();
    let o = open(cx, px(300.), vec![8], context_of(snapshot));
    inside_row(o.cx, &env, "ref:0");
    inside_row(o.cx, &env, "body");
    // The chip wraps inside the column rather than running past it.
    let chip = o.cx.debug_bounds(sel(&env, "ref:0")).unwrap();
    let verb = o.cx.debug_bounds(sel(&env, "verb")).unwrap();
    assert!(chip.size.height > verb.size.height * 1.5, "{chip:?}");
}

/// Draws the settings card's message and andy/fx1's DONE in a column
/// `width` wide and asserts every part of both rows, the card's included,
/// ends inside its row.
fn assert_rows_fit(cx: &mut TestAppContext, width: gpui::Pixels) {
    let snapshot = snapshot_of(Screen::SettingsFromChat);
    let (id, card) = snapshot.changes.iter().next().unwrap();
    let index = snapshot
        .messages
        .iter()
        .position(|m| &m.client.id == id)
        .unwrap();
    let done = 6;
    assert!(offers_take_over(
        &snapshot.messages[done],
        &context_of(snapshot.clone())
    ));
    let (settings, done_env) = (
        snapshot.messages[index].clone(),
        snapshot.messages[done].clone(),
    );
    let card_parts = crate::ui::change_card::part_selectors(card);
    let card_sel = format!("{}:card", card.id);
    let o = open(cx, width, vec![done, index], context_of(snapshot.clone()));
    let mut parts: Vec<(&StampedEnvelope, String)> = Vec::new();
    for env in [&settings, &done_env] {
        for name in ["avatar", "head", "handle", "role", "verb", "time", "body"] {
            parts.push((env, format!("{}:{name}", env.client.id.as_str())));
        }
    }
    parts.push((&done_env, sel(&done_env, "open-in-terminal").to_owned()));
    parts.push((&settings, card_sel));
    parts.extend(card_parts.into_iter().map(|part| (&settings, part)));
    for (env, part) in parts {
        let row = o.cx.debug_bounds(sel(env, "row")).unwrap();
        let selector: &'static str = Box::leak(part.into_boxed_str());
        let bounds =
            o.cx.debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} not drawn"));
        assert!(
            bounds.right() <= row.right(),
            "at {width:?}: {selector} {bounds:?} past {row:?}"
        );
    }
}

#[gpui::test]
fn narrow_row_keeps_the_change_card_inside(cx: &mut TestAppContext) {
    // 300 px: a two-pane split; 276 px: three panes in a 1440 px window
    // with both docks open.
    assert_rows_fit(cx, px(300.));
    assert_rows_fit(&mut TestAppContext::single(), px(276.));
}

#[gpui::test]
fn change_card_stays_on_the_listener_message_it_belongs_to(cx: &mut TestAppContext) {
    // C222: the hub keeps client ids unique per sender only, so another
    // sender can reuse the settings message's id, before it or after it.
    let mut snapshot = snapshot_of(Screen::SettingsFromChat);
    let (id, card) = snapshot
        .changes
        .iter()
        .next()
        .map(|(id, card)| (id.clone(), card.clone()))
        .unwrap();
    let real = snapshot
        .messages
        .iter()
        .find(|m| m.client.id == id)
        .unwrap()
        .clone();
    let sean = snapshot
        .messages
        .iter()
        .find(|m| m.from_handle == "sean")
        .unwrap()
        .clone();
    let spoof = |seq: u64| {
        let mut spoof = real.clone();
        spoof.from = sean.from.clone();
        spoof.from_handle = sean.from_handle.clone();
        spoof.from_kind = sean.from_kind;
        spoof.seq = agmen_proto::Seq(seq);
        spoof
    };
    let (before, after) = (spoof(0), spoof(real.seq.0 + 1000));
    snapshot.messages.insert(0, before.clone());
    snapshot.messages.push(after.clone());
    let real_ix = snapshot
        .messages
        .iter()
        .position(|m| m.client.id == id && m.from == real.from)
        .unwrap();
    let last = snapshot.messages.len() - 1;
    let c = context_of(snapshot);

    assert_eq!(verb_badge(&real, &c), ("SETTINGS".into(), Color::Accent));
    for env in [&before, &after] {
        assert_eq!(verb_badge(env, &c), ("ANSWER".into(), Color::Default));
    }
    let card_sel: &'static str = Box::leak(format!("{}:card", card.id).into_boxed_str());
    let o = open(cx, px(800.), vec![real_ix], c.clone());
    assert!(o.cx.debug_bounds(card_sel).is_some());
    for ix in [0, last] {
        let mut spoof_cx = TestAppContext::single();
        let o = open(&mut spoof_cx, px(800.), vec![ix], c.clone());
        assert!(o.cx.debug_bounds(sel(&real, "row")).is_some(), "row {ix}");
        assert!(o.cx.debug_bounds(card_sel).is_none(), "row {ix}");
    }
}

#[gpui::test]
fn the_take_over_chip_is_the_mockups_17_px(cx: &mut TestAppContext) {
    // Node 300: a 1 px border and 1 px padding around the label's 13 px
    // line (node 302, SF 11), 17 px in all. The `>_` glyph (node 301) is
    // 10 px mono on a 12 px line; at the code size on its own line it
    // made the chip 20 px.
    let (messages, c) = chat();
    let o = open(cx, px(800.), (0..messages.len()).collect(), c);
    let chip =
        o.cx.debug_bounds(sel(&messages[6], "open-in-terminal"))
            .unwrap();
    assert_eq!(chip.size.height, px(17.));
}
