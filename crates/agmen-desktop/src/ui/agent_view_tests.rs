use gpui::{Focusable as _, TestAppContext};

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{assert_breadcrumb_glyphs, init, redraw};

/// andy/fx1's session view over the chat fixture.
fn open_fx1(cx: &mut TestAppContext) -> (Entity<AgentView>, &mut gpui::VisualTestContext) {
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    cx.add_window_view(|window, cx| AgentView::new(&snapshot, &id, session, window, cx))
}

#[gpui::test]
fn agent_view_lists_tool_calls(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = open_fx1(cx);
    redraw(cx);
    // Review C166: every row, in console.py's order.
    let kinds = [
        "tool", "tool", "out", "say", "tool", "tool", "out", "tool", "out", "cost", "tool", "out",
    ];
    for (ix, kind) in kinds.into_iter().enumerate() {
        let sel: &'static str = Box::leak(format!("session:{ix}:{kind}").into_boxed_str());
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
    // Review C177: 12 rows, numbered from 0, whatever the kind.
    for kind in ["tool", "out", "say", "cost"] {
        let sel: &'static str = Box::leak(format!("session:12:{kind}").into_boxed_str());
        assert!(cx.debug_bounds(sel).is_none(), "{sel}");
    }
    assert!(cx.debug_bounds("agent-open-terminal").is_some());
    assert!(cx.debug_bounds("agent-stop").is_some());
}

#[test]
fn plain_output_is_dim() {
    // Mockup check: console.py draws plain tool output in FAINT.
    assert_eq!(tone_color(Tone::Plain), Color::Placeholder);
    assert_eq!(tone_color(Tone::Success), Color::Success);
}

#[test]
fn output_glyph_takes_the_line_color() {
    // V79: "⎿ ok" is all success, "⎿ recorded" all accent, "⎿ 2 files" all
    // text.placeholder.
    assert_eq!(
        out_colors(Tone::Plain),
        (Color::Placeholder, Color::Placeholder)
    );
    assert_eq!(out_colors(Tone::Success), (Color::Success, Color::Success));
    assert_eq!(out_colors(Tone::Accent), (Color::Accent, Color::Accent));
}

#[gpui::test]
fn agent_header_buttons_are_8_px_apart(cx: &mut TestAppContext) {
    // V105: node 607 spaces "Open in terminal" and "Stop" 8 px apart.
    init(cx);
    let (_, cx) = open_fx1(cx);
    redraw(cx);
    let open = cx.debug_bounds("agent-open-terminal").unwrap();
    let stop = cx.debug_bounds("agent-stop").unwrap();
    assert_eq!(
        stop.left() - open.right(),
        gpui::px(8.),
        "{open:?} {stop:?}"
    );
}

#[gpui::test]
fn rows_between_cards_take_the_mockups_line_heights(cx: &mut TestAppContext) {
    // V108: console.py's auto line heights are 14 px for 12 px code (the ⎿
    // rows), 16 px for 13 px text (Say) and 13 px for 11 px text (cost).
    init(cx);
    let (_, cx) = open_fx1(cx);
    redraw(cx);
    let mut height = |sel: &'static str| cx.debug_bounds(sel).unwrap().size.height;
    assert_eq!(height("session:2:out"), gpui::px(14.));
    assert_eq!(height("session:3:say"), gpui::px(16.));
    assert_eq!(height("session:9:cost"), gpui::px(13.));
}

#[gpui::test]
fn breadcrumb_splits_handle_item_and_branch_with_a_glyph(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = open_fx1(cx);
    redraw(cx);
    assert_breadcrumb_glyphs(cx, 3);
}

#[gpui::test]
fn composer_is_the_chat_composer_addressed_to_the_agent(cx: &mut TestAppContext) {
    // V78: Task 9's Composer addressed to the agent, not a box of its own.
    // Node 650 holds only the placeholder and the ⏎ hint: no "to" chip.
    init(cx);
    let (view, cx) = open_fx1(cx);
    redraw(cx);
    view.read_with(cx, |view, cx| {
        let target = view.composer.read(cx).target(cx);
        assert_eq!(target.handle, "andy/fx1");
        assert_eq!(target.id, crate::backend::fixture_id("andy/fx1"));
    });
    assert!(cx.debug_bounds("composer:to").is_none());
    assert!(cx.debug_bounds("composer:input").is_some());
    // The agent runs its own model, so the listener's model chip is not
    // drawn (console.py `build_d_agent`).
    assert!(cx.debug_bounds("composer:model").is_none());
}

/// The agent view under a root that records the take-over actions that
/// reach it, as the workspace's root handles them.
struct Recorder {
    view: Entity<AgentView>,
    seen: Vec<String>,
}

impl Render for Recorder {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::keymap::OpenInTerminal;
        div()
            .size_full()
            .on_action(cx.listener(|this, _: &OpenInTerminal, _, _| {
                this.seen.push("open-in-terminal".into());
            }))
            .on_action(cx.listener(|this, action: &TakeOverAgent, _, _| {
                this.seen.push(format!("take-over {}", action.0));
            }))
            .child(self.view.clone())
    }
}

#[gpui::test]
fn alt_o_in_the_agent_view_takes_over_its_own_agent(cx: &mut TestAppContext) {
    // Review: ⌥O, as the view's button says, acts on this view's agent,
    // not on the agent of the chat's newest DONE.
    init(cx);
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (recorder, cx) = cx.add_window_view(|window, cx| Recorder {
        view: cx.new(|cx| AgentView::new(&snapshot, &id, session, window, cx)),
        seen: Vec::new(),
    });
    redraw(cx);
    let view = recorder.read_with(cx, |r, _| r.view.clone());
    let composer = view.read_with(cx, |v, _| v.composer.clone());
    cx.update(|window, cx| window.focus(&composer.focus_handle(cx), cx));
    redraw(cx);
    cx.dispatch_action(crate::keymap::OpenInTerminal);
    cx.run_until_parked();
    let seen = recorder.read_with(cx, |r, _| r.seen.clone());
    assert_eq!(seen, [format!("take-over {id}")]);
}

#[gpui::test]
fn refused_dm_shows_why_until_the_next_send(cx: &mut TestAppContext) {
    // C234: a DM the backend refuses says why, as the chat does, and the
    // text comes back so Enter can send it again.
    init(cx);
    cx.update(crate::ui::text_input::bind_keys);
    let (view, cx) = open_fx1(cx);
    let composer = view.read_with(cx, |v, _| v.composer.clone());
    cx.update(|window, cx| window.focus(&composer.focus_handle(cx), cx));
    cx.simulate_input("status?");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert!(cx.debug_bounds("agent:error").is_some());
    view.read_with(cx, |view, cx| {
        assert_eq!(view.composer.read(cx).text(cx), "status?");
    });

    let backend = std::rc::Rc::new(
        crate::backend::FixtureBackend::for_screen(crate::backend::Screen::Chat).unwrap(),
    );
    let handle: std::rc::Rc<dyn crate::backend::Backend> = backend;
    cx.update(|_, cx| cx.set_global(BackendHandle(handle)));
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert!(cx.debug_bounds("agent:error").is_none());
    view.read_with(cx, |view, cx| {
        assert_eq!(view.composer.read(cx).text(cx), "")
    });
}

#[gpui::test]
fn output_rows_indent_three_code_cells(cx: &mut TestAppContext) {
    // Nodes 627, 637 and 648 draw "   ⎿ text": three code cells, then ⎿.
    init(cx);
    let (_, cx) = open_fx1(cx);
    redraw(cx);
    let cell = cx.update(|window, cx| {
        crate::ui::code_view::cell_width(window, &cx.global::<Appearance>().fonts)
    });
    let row = cx.debug_bounds("session:2:out").unwrap();
    let glyph = cx.debug_bounds("session:2:glyph").unwrap();
    let indent = glyph.origin.x - row.origin.x;
    assert!(
        (indent - cell * 3.).abs() < gpui::px(0.5),
        "indent {indent:?}, cell {cell:?}"
    );
}
