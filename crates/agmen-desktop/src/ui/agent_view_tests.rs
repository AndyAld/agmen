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
    for (ix, kind) in [(0, "tool"), (2, "out"), (3, "say"), (9, "cost")] {
        let sel: &'static str = Box::leak(format!("session:{ix}:{kind}").into_boxed_str());
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
    assert!(
        cx.debug_bounds("session:12:tool").is_none(),
        "12 rows, numbered from 0"
    );
    assert!(cx.debug_bounds("agent-open-terminal").is_some());
    assert!(cx.debug_bounds("agent-stop").is_some());
}
