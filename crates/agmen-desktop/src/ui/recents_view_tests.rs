use std::cell::RefCell;
use std::rc::Rc;

use futures::channel::mpsc::UnboundedReceiver;
use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};

use super::{RecentsView, descendants, recent_rows};
use crate::backend::{
    Action, Backend, BackendError, BackendHandle, Draft, FixtureBackend, MAX_DEPTH, RecentAgent,
    Screen, Snapshot, Update, fixture_id,
};
use crate::ui::Workspace;
use crate::ui::test_support::{click, init, redraw};

fn recent(name: &str, parent: Option<&str>, time: Option<&str>) -> RecentAgent {
    RecentAgent {
        id: fixture_id(name),
        handle: name.into(),
        role: Some("engineer".into()),
        branch: Some("agent/saved".into()),
        parent: parent.map(fixture_id),
        stopped_at: time.map(|at| at.parse().unwrap()),
        resumable: true,
    }
}

fn tree() -> Vec<RecentAgent> {
    vec![
        recent("andy/root", None, Some("2026-11-07T12:00:00Z")),
        recent("andy/old", Some("andy/root"), None),
        recent("andy/new", Some("andy/root"), Some("2026-11-07T14:00:00Z")),
        recent("andy/leaf", Some("andy/new"), None),
        recent("andy/other", None, Some("2026-11-07T13:00:00Z")),
        recent("andy/unknown", None, None),
    ]
}

#[test]
fn recent_rows_are_trees_newest_first() {
    assert_eq!(
        recent_rows(&tree()),
        [(4, 0), (0, 0), (2, 1), (3, 2), (1, 1), (5, 0)]
    );
    assert_eq!(
        descendants(&tree(), &recent_rows(&tree())),
        [3, 0, 1, 0, 0, 0]
    );
}

#[test]
fn recent_rows_put_orphans_at_the_root() {
    let rows = vec![
        recent("andy/orphan", Some("andy/running"), None),
        recent("andy/child", Some("andy/orphan"), None),
    ];
    assert_eq!(recent_rows(&rows), [(0, 0), (1, 1)]);
}

#[test]
fn recent_rows_end_on_a_parent_cycle() {
    let rows = vec![
        recent("andy/a", Some("andy/b"), None),
        recent("andy/b", Some("andy/a"), None),
        recent("andy/self", Some("andy/self"), None),
    ];
    let shown = recent_rows(&rows);
    assert_eq!(shown.len(), rows.len());
    assert_eq!(descendants(&rows, &shown), [1, 0, 0]);
    let mut indices: Vec<_> = shown.iter().map(|(ix, _)| *ix).collect();
    indices.sort_unstable();
    assert_eq!(indices, [0, 1, 2]);
    assert!(shown.iter().all(|(_, indent)| *indent <= MAX_DEPTH));
}

#[test]
fn recent_rows_bound_deep_indents() {
    let rows: Vec<_> = (0..32)
        .map(|ix| {
            recent(
                &format!("andy/agent{ix}"),
                (ix > 0).then(|| format!("andy/agent{}", ix - 1)).as_deref(),
                None,
            )
        })
        .collect();
    let shown = recent_rows(&rows);
    assert_eq!(shown.len(), rows.len());
    assert_eq!(descendants(&rows, &shown)[0], rows.len() - 1);
    for (ix, (index, indent)) in shown.into_iter().enumerate() {
        assert_eq!(index, ix);
        assert_eq!(indent, u8::try_from(ix).unwrap().min(MAX_DEPTH));
    }
}

struct Recording {
    inner: FixtureBackend,
    acts: RefCell<Vec<Action>>,
}

impl Recording {
    fn edit(&self, edit: impl FnOnce(&mut Snapshot)) {
        self.inner.edit(edit);
        // The fixture's existing scan action publishes its current snapshot.
        self.inner.act(Action::ScanAgents).unwrap();
    }
}

impl Backend for Recording {
    fn snapshot(&self) -> Snapshot {
        self.inner.snapshot()
    }

    fn send(&self, draft: Draft) -> Result<(), BackendError> {
        self.inner.send(draft)
    }

    fn act(&self, action: Action) -> Result<(), BackendError> {
        self.acts.borrow_mut().push(action.clone());
        self.inner.act(action)
    }

    fn subscribe(&self) -> UnboundedReceiver<Update> {
        self.inner.subscribe()
    }
}

fn backend(cx: &mut TestAppContext, recents: Vec<RecentAgent>) -> Rc<Recording> {
    init(cx);
    let backend = Rc::new(Recording {
        inner: FixtureBackend::for_screen(Screen::Chat).unwrap(),
        acts: RefCell::new(Vec::new()),
    });
    backend.inner.edit(|s| s.recents = recents);
    let handle: Rc<dyn Backend> = backend.clone();
    cx.update(|cx| cx.set_global(BackendHandle(handle)));
    backend
}

fn open(
    cx: &mut TestAppContext,
    recents: Vec<RecentAgent>,
) -> (Rc<Recording>, Entity<RecentsView>, &mut VisualTestContext) {
    let backend = backend(cx, recents);
    let snapshot = Rc::new(backend.snapshot());
    let (view, cx) = cx.add_window_view(|window, cx| RecentsView::new(&snapshot, window, cx));
    redraw(cx);
    (backend, view, cx)
}

fn selector(kind: &str, name: &str) -> &'static str {
    Box::leak(format!("recents:{kind}:{}", fixture_id(name)).into_boxed_str())
}

#[gpui::test]
fn recents_tab_lists_stopped_agents(cx: &mut TestAppContext) {
    let mut local = recent("andy/local", None, Some("2026-11-07T12:00:00Z"));
    local.id = "localagent01".parse().unwrap();
    let hub = recent("andy/hub", None, None);
    let (_, _, cx) = open(cx, vec![local.clone(), hub.clone()]);
    for agent in [local, hub] {
        assert!(
            cx.debug_bounds(Box::leak(
                format!("recents:row:{}", agent.id).into_boxed_str()
            ))
            .is_some()
        );
        assert!(
            cx.debug_bounds(Box::leak(
                format!("recents:resume:{}", agent.id).into_boxed_str()
            ))
            .is_some()
        );
    }
}

#[gpui::test]
fn resume_sends_start_agent(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, vec![recent("andy/root", None, None)]);
    click(cx, selector("resume", "andy/root"));
    assert_eq!(
        *backend.acts.borrow(),
        [Action::StartAgent {
            identity: fixture_id("andy/root"),
            root: None,
        }]
    );
    assert!(
        cx.debug_bounds(selector("resume-tree", "andy/root"))
            .is_none()
    );
}

#[gpui::test]
fn resume_tree_sends_resume_tree(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, tree());
    click(cx, selector("resume-tree", "andy/root"));
    assert_eq!(
        *backend.acts.borrow(),
        [Action::ResumeTree(fixture_id("andy/root"))]
    );
}

#[gpui::test]
fn recents_empty_state(cx: &mut TestAppContext) {
    let (_, _, cx) = open(cx, Vec::new());
    assert!(cx.debug_bounds("recents:empty").is_some());
}

#[gpui::test]
fn recents_follow_new_snapshots(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, vec![recent("andy/root", None, None)]);
    assert!(cx.debug_bounds(selector("row", "andy/root")).is_some());
    backend.edit(|s| s.recents.clear());
    cx.run_until_parked();
    redraw(cx);
    assert!(cx.debug_bounds(selector("row", "andy/root")).is_none());
    assert!(cx.debug_bounds("recents:empty").is_some());
}

fn workspace(
    cx: &mut TestAppContext,
    recents: Vec<RecentAgent>,
) -> (Rc<Recording>, Entity<Workspace>, &mut VisualTestContext) {
    let backend = backend(cx, recents);
    cx.update(crate::keymap::bind_all);
    let (workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::new(backend.snapshot(), window, cx));
    workspace.update_in(cx, |view, window, cx| {
        view.focus_handle(cx).focus(window, cx)
    });
    redraw(cx);
    (backend, workspace, cx)
}

#[gpui::test]
fn palette_opens_recents(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace(cx, vec![recent("andy/root", None, None)]);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("agmen: open recents");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert!(cx.debug_bounds("command-palette").is_none());
    assert!(cx.debug_bounds(selector("row", "andy/root")).is_some());
}

#[gpui::test]
fn agents_panel_link_opens_recents(cx: &mut TestAppContext) {
    let (_, _, cx) = workspace(cx, vec![recent("andy/root", None, None)]);
    click(cx, "agents:recents");
    assert!(cx.debug_bounds(selector("row", "andy/root")).is_some());
}

#[gpui::test]
fn recents_revoked_agent_has_remove_without_resume(cx: &mut TestAppContext) {
    let mut rows = tree();
    rows[0].resumable = false;
    let (_, _, cx) = open(cx, rows);
    assert!(cx.debug_bounds(selector("row", "andy/root")).is_some());
    assert!(cx.debug_bounds(selector("remove", "andy/root")).is_some());
    assert!(cx.debug_bounds(selector("revoked", "andy/root")).is_some());
    assert!(cx.debug_bounds(selector("resume", "andy/root")).is_none());
    assert!(
        cx.debug_bounds(selector("resume-tree", "andy/root"))
            .is_none()
    );
}

#[gpui::test]
fn recents_remove_frees_name_and_sends_remove_action(cx: &mut TestAppContext) {
    let (backend, _, cx) = open(cx, tree());
    for agent in tree() {
        assert!(cx.debug_bounds(selector("remove", &agent.handle)).is_some());
        assert!(
            cx.debug_bounds(selector("remove-hint", &agent.handle))
                .is_some()
        );
    }
    click(cx, selector("remove", "andy/root"));
    assert_eq!(
        *backend.acts.borrow(),
        [Action::RemoveAgent {
            identity: fixture_id("andy/root"),
            root: None,
        }]
    );
}

#[gpui::test]
fn recents_show_action_and_snapshot_errors(cx: &mut TestAppContext) {
    let (backend, view, cx) = open(cx, vec![recent("andy/root", None, None)]);
    click(cx, selector("resume", "andy/root"));
    assert!(cx.debug_bounds("recents:error").is_some());
    view.read_with(cx, |view, _| assert!(view.error.is_some()));
    backend.edit(|s| s.agent_error = Some("start refused".into()));
    cx.run_until_parked();
    redraw(cx);
    assert!(cx.debug_bounds("recents:error").is_some());
    view.read_with(cx, |view, _| {
        assert_eq!(view.snapshot.agent_error.as_deref(), Some("start refused"));
    });
}

#[gpui::test]
fn recents_panel_link_follows_snapshots(cx: &mut TestAppContext) {
    let (backend, _, cx) = workspace(cx, Vec::new());
    assert!(cx.debug_bounds("agents:recents").is_none());
    backend.edit(|s| s.recents = vec![recent("andy/root", None, None)]);
    cx.run_until_parked();
    redraw(cx);
    click(cx, "agents:recents");
    assert!(cx.debug_bounds(selector("row", "andy/root")).is_some());
    backend.edit(|s| s.recents.clear());
    cx.run_until_parked();
    redraw(cx);
    assert!(cx.debug_bounds("agents:recents").is_none());
    assert!(cx.debug_bounds("recents:empty").is_some());
}
