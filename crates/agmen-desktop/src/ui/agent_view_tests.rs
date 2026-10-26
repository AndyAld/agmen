use gpui::TestAppContext;

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{init, redraw};

#[gpui::test]
fn agent_view_lists_tool_calls(cx: &mut TestAppContext) {
    init(cx);
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| AgentView::new("andy/fx1", session, cx));
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
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| AgentView::new("andy/fx1", session, cx));
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
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| AgentView::new("andy/fx1", session, cx));
    redraw(cx);
    let (code, rem) = cx.update(|window, cx| {
        (
            cx.global::<crate::appearance::Appearance>().fonts.code_size,
            window.rem_size(),
        )
    });
    let mut height = |sel: &'static str| cx.debug_bounds(sel).unwrap().size.height;
    let near = |got: gpui::Pixels, want: gpui::Pixels| (got - want).abs() <= gpui::px(0.25);
    let out = height("session:2:out");
    assert!(near(out, code * (14. / 12.)), "{out:?} for code {code:?}");
    let say = height("session:3:say");
    let say_size = crate::ui::tokens::TEXT_DEFAULT.to_pixels(rem);
    assert!(
        near(say, say_size * (16. / 13.)),
        "{say:?} for {say_size:?}"
    );
    let cost = height("session:9:cost");
    let cost_size = crate::ui::tokens::TEXT_SMALL.to_pixels(rem);
    assert!(
        near(cost, cost_size * (13. / 11.)),
        "{cost:?} for {cost_size:?}"
    );
}
