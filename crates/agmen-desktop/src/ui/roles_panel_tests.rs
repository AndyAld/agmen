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
    // The hub checks verbs by identity kind only, so the panel says where
    // Refused comes from.
    assert!(cx.debug_bounds("role-source").is_some());
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

/// The Roles panel beside each layer title drawn as a reference label.
struct LayerHost(gpui::Entity<RolesPanel>);

impl gpui::Render for LayerHost {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        _: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let reference = |sel: &'static str, text: &'static str, code: bool| {
            let label = Label::new(text).weight(FontWeight::MEDIUM);
            div()
                .flex()
                .debug_selector(move || sel.into())
                .child(if code { label.code() } else { label })
        };
        div()
            .flex()
            .flex_col()
            .items_start()
            .child(self.0.clone())
            .child(reference("want:built-in", "Built-in", false))
            .child(reference("want:project", ".agmen/roles", true))
            .child(reference("want:personal", "~/.config/agmen/roles", true))
    }
}

#[gpui::test]
fn path_layer_headers_use_the_code_font(cx: &mut TestAppContext) {
    // V83: ".agmen/roles" and "~/.config/agmen/roles" are mono 13 Medium
    // (nodes 11599, 11607); "Built-in" stays in the UI font.
    init(cx);
    let roles = chat_snapshot().roles;
    let (_, cx) = cx.add_window_view(|_, cx| LayerHost(cx.new(|_| RolesPanel::new(roles))));
    redraw(cx);
    for slug in ["built-in", "project", "personal"] {
        let sel: &'static str = Box::leak(format!("role-layer:{slug}:title").into_boxed_str());
        let want: &'static str = Box::leak(format!("want:{slug}").into_boxed_str());
        let title = cx.debug_bounds(sel).unwrap_or_else(|| panic!("{sel}"));
        let want = cx.debug_bounds(want).unwrap();
        assert_eq!(title.size.width, want.size.width, "{sel}");
    }
}

#[gpui::test]
fn role_rows_put_a_16_px_icon_at_26_and_the_name_at_49(cx: &mut TestAppContext) {
    // V83 and C216: console.py draws a role file's 16 px icon at x 26 and
    // its name at x 49 (padding 26, gap 7).
    init(cx);
    let roles = chat_snapshot().roles;
    let (_, cx) = cx.add_window_view(|_, _| RolesPanel::new(roles));
    redraw(cx);
    let icon = cx.debug_bounds("role:reviewer:leading").unwrap();
    assert_eq!(icon.left(), gpui::px(26.), "{icon:?}");
    assert_eq!(icon.size.width, gpui::px(16.), "{icon:?}");
    let name = cx.debug_bounds("role:reviewer:name").unwrap();
    assert_eq!(name.left(), gpui::px(49.), "{name:?}");
}

#[test]
fn role_dots_take_the_palette_role_color() {
    // V85: the dot is a Dot in the role's palette color, icon.muted without one.
    assert_eq!(
        role_dot_color(Some("reviewer")),
        Color::Role("reviewer".into())
    );
    assert_eq!(role_dot_color(None), Color::Muted);
}
