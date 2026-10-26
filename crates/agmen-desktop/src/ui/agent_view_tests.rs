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
    for tone in [Tone::Plain, Tone::Success, Tone::Accent] {
        assert_eq!(out_colors(tone), (tone_color(tone), tone_color(tone)));
    }
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
    // V78: Task 9's Composer, its "to" chip on the agent, not a box of its
    // own.
    init(cx);
    let (view, cx) = open_fx1(cx);
    redraw(cx);
    view.read_with(cx, |view, cx| {
        let target = view.composer.read(cx).target(cx);
        assert_eq!(target.handle, "andy/fx1");
        assert_eq!(target.id, crate::backend::fixture_id("andy/fx1"));
    });
    assert!(cx.debug_bounds("composer:to").is_some());
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
        use crate::ui::keys::OpenInTerminal;
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
    cx.dispatch_action(crate::ui::keys::OpenInTerminal);
    cx.run_until_parked();
    let seen = recorder.read_with(cx, |r, _| r.seen.clone());
    assert_eq!(seen, [format!("take-over {id}")]);
}
