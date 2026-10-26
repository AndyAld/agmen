use std::cell::RefCell;
use std::rc::Rc;

use gpui::TestAppContext;

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{click, init, redraw};

#[gpui::test]
fn roles_panel_lists_layers_and_opens_a_role(cx: &mut TestAppContext) {
    init(cx);
    let roles = chat_snapshot().roles;
    let (panel, cx) = cx.add_window_view(|_, _| RolesPanel::new(roles));
    redraw(cx);
    for sel in [
        "role-layer:built-in",
        "role-layer:project",
        "role-layer:personal",
    ] {
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
    assert!(cx.debug_bounds("role:reviewer").is_some());
    assert!(cx.debug_bounds("role:security-reviewer").is_some());
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &RolesPanelEvent, _| {
            let RolesPanelEvent::Open(name) = event;
            sink.borrow_mut().push(name.clone());
        })
        .detach();
    });
    click(cx, "role:reviewer");
    assert_eq!(*opened.borrow(), vec!["reviewer".to_owned()]);
    panel.read_with(cx, |p, _| assert_eq!(p.selected(), Some("reviewer")));
}

#[gpui::test]
fn role_panel_shows_enforcement(cx: &mut TestAppContext) {
    init(cx);
    let info = chat_snapshot()
        .roles
        .into_iter()
        .find(|r| r.name == "reviewer")
        .and_then(|r| r.info)
        .unwrap();
    let (_, cx) = cx.add_window_view(move |_, _| RoleHost(info.clone()));
    redraw(cx);
    assert!(cx.debug_bounds("role-panel").is_some());
}

struct RoleHost(crate::backend::RoleInfo);

impl gpui::Render for RoleHost {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        RolePanel::new("reviewer", self.0.clone())
    }
}

#[gpui::test]
fn built_in_role_file_offers_duplicate(cx: &mut TestAppContext) {
    init(cx);
    let roles = chat_snapshot().roles;
    let reviewer = roles.iter().find(|r| r.name == "reviewer").unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| RoleFileView::new(&reviewer, cx));
    redraw(cx);
    assert!(cx.debug_bounds("role-file").is_some());
    assert!(cx.debug_bounds("role-duplicate").is_some());
}

#[gpui::test]
fn project_role_file_has_no_duplicate(cx: &mut TestAppContext) {
    init(cx);
    let roles = chat_snapshot().roles;
    let own = roles
        .iter()
        .find(|r| r.name == "security-reviewer")
        .unwrap()
        .clone();
    let (_, cx) = cx.add_window_view(|_, cx| RoleFileView::new(&own, cx));
    redraw(cx);
    assert!(cx.debug_bounds("role-file").is_some());
    assert!(cx.debug_bounds("role-duplicate").is_none());
}

#[gpui::test]
fn role_rows_show_a_role_dot(cx: &mut TestAppContext) {
    init(cx);
    let roles = chat_snapshot().roles;
    let (_, cx) = cx.add_window_view(|_, _| RolesPanel::new(roles));
    redraw(cx);
    for name in ["reviewer", "engineer", "docs-writer"] {
        let sel: &'static str = Box::leak(format!("role-dot:{name}").into_boxed_str());
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
}
