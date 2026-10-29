//! Shared component adoption and form spacing regressions.

use gpui::{
    InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, TestAppContext, div, px,
};

use super::components::{
    Button, ButtonSize, ButtonStyle, Color, EmptyState, Field, Label, StatusItem,
};
use super::new_agent_dialog::NewAgentDialog;
use super::test_support::{draw, init, redraw};
use super::tokens::{SPACE_4, SPACE_12};

#[gpui::test]
fn field_puts_its_label_close_to_its_control(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, cx| NewAgentDialog::new("opus".into(), None, cx));
    redraw(cx);
    for (label, control) in [
        ("new-agent:name-field:label", "new-agent:name"),
        ("new-agent:model-field:label", "new-agent:model"),
        ("new-agent:budget-field:label", "new-agent:budget"),
    ] {
        let label = cx.debug_bounds(label).expect("the Field label");
        let control = cx.debug_bounds(control).expect("the control");
        assert_eq!(control.top() - label.bottom(), SPACE_4);
    }
}

#[gpui::test]
fn new_agent_labels_sit_closer_to_their_own_field_than_to_the_one_above(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, cx| NewAgentDialog::new("opus".into(), None, cx));
    redraw(cx);
    for (above, label, control) in [
        (
            "new-agent:name",
            "new-agent:model-field:label",
            "new-agent:model",
        ),
        (
            "new-agent:model",
            "new-agent:budget-field:label",
            "new-agent:budget",
        ),
    ] {
        let above = cx.debug_bounds(above).unwrap();
        let label = cx.debug_bounds(label).expect("the Field label");
        let control = cx.debug_bounds(control).unwrap();
        assert_eq!(label.top() - above.bottom(), SPACE_12);
        assert!(control.top() - label.bottom() < label.top() - above.bottom());
    }
}

#[test]
fn status_items_share_one_component() {
    let source = include_str!("status_bar.rs");
    let compact = source.split_whitespace().collect::<String>();
    for id in [
        "status-link",
        "status-online",
        "status-alerts",
        "status-worktrees",
    ] {
        assert!(
            compact.contains(&format!("StatusItem::new(\"{id}\"")),
            "{id} must use StatusItem"
        );
    }
    assert!(!source.contains("fn item("));
}

#[test]
fn settings_and_about_share_setting_text() {
    assert!(include_str!("settings_rows.rs").contains("SettingText::new("));
    assert!(include_str!("settings_view.rs").contains("SettingText::new("));
}

#[test]
fn people_and_role_layers_share_section_headers() {
    assert!(include_str!("agents_panel.rs").contains("SectionHeader::new("));
    assert!(include_str!("roles_panel.rs").contains("SectionHeader::new("));
}

#[test]
fn settings_model_and_theme_search_use_fields() {
    let source = include_str!("settings_view.rs")
        .split_whitespace()
        .collect::<String>();
    for id in ["default-agent-model:field", "theme-search:field"] {
        assert!(source.contains(&format!("Field::new(\"{id}\"")));
    }
}

#[test]
fn empty_states_use_the_empty_state_component() {
    for (source, selector) in [
        (include_str!("diff_view.rs"), "diff-empty"),
        (include_str!("settings_view.rs"), "settings-placeholder"),
    ] {
        assert!(source.contains(&format!("EmptyState::new(\"{selector}\"")));
    }
}

#[gpui::test]
fn field_orders_optional_help_and_error_after_its_control(cx: &mut TestAppContext) {
    let cx = draw(cx, || {
        div()
            .w(px(240.))
            .child(
                Field::new("field", "Name")
                    .hint("Pick a name.")
                    .error("That name is taken.")
                    .child(div().debug_selector(|| "control".into()).h(px(24.))),
            )
            .into_any_element()
    });
    for (above, below) in [
        ("field:label", "control"),
        ("control", "field:hint"),
        ("field:hint", "field:error"),
    ] {
        assert_eq!(
            cx.debug_bounds(below).unwrap().top() - cx.debug_bounds(above).unwrap().bottom(),
            SPACE_4
        );
    }
}

#[gpui::test]
fn field_without_help_or_error_has_no_extra_gap(cx: &mut TestAppContext) {
    let cx = draw(cx, || {
        div()
            .w(px(240.))
            .child(
                Field::new("field", "Name")
                    .child(div().debug_selector(|| "control".into()).h(px(24.))),
            )
            .into_any_element()
    });
    assert_eq!(
        cx.debug_bounds("field").unwrap().bottom(),
        cx.debug_bounds("control").unwrap().bottom()
    );
    assert!(cx.debug_bounds("field:hint").is_none());
    assert!(cx.debug_bounds("field:error").is_none());
}

#[gpui::test]
fn status_item_keeps_compact_button_geometry_with_or_without_an_action(cx: &mut TestAppContext) {
    let cx = draw(cx, || {
        div()
            .flex()
            .flex_col()
            .items_start()
            .child(
                Button::new("reference", "2 alerts")
                    .style(ButtonStyle::Plain)
                    .size(ButtonSize::Compact)
                    .leading(
                        Label::new("⚠")
                            .size(super::components::LabelSize::Small)
                            .color(Color::Warning),
                    ),
            )
            .child(StatusItem::new("inert", "⚠", Color::Warning, "2 alerts").button())
            .child(
                StatusItem::new("active", "⚠", Color::Warning, "2 alerts").on_click(|_, _, _| {}),
            )
            .into_any_element()
    });
    let reference = cx.debug_bounds("reference").unwrap().size;
    assert_eq!(cx.debug_bounds("inert").unwrap().size, reference);
    assert_eq!(cx.debug_bounds("active").unwrap().size, reference);
}

#[gpui::test]
fn clickable_status_item_can_truncate_and_still_activate(cx: &mut TestAppContext) {
    use super::test_support::{assert_control_feedback, click, draw_with_tab, press};
    use std::{cell::Cell, rc::Rc};
    let clicks = Rc::new(Cell::new(0));
    let seen = clicks.clone();
    let cx = draw_with_tab(cx, move || {
        let seen = seen.clone();
        div()
            .debug_selector(|| "row".into())
            .w(px(200.))
            .flex()
            .child(
                StatusItem::new(
                    "item",
                    "●",
                    Color::Error,
                    "offline: connection refused ".repeat(100),
                )
                .label_color(Color::Error)
                .truncate()
                .on_click(move |_, _, _| seen.set(seen.get() + 1)),
            )
            .child(
                div()
                    .debug_selector(|| "after".into())
                    .flex_none()
                    .w(px(20.))
                    .h(px(20.)),
            )
            .into_any_element()
    });
    assert_eq!(
        cx.debug_bounds("after").unwrap().right(),
        cx.debug_bounds("row").unwrap().right()
    );
    assert_control_feedback(cx, "item", "item", 1);
    press(cx, "enter");
    press(cx, "space");
    click(cx, "item");
    assert_eq!(clicks.get(), 3);
}

#[gpui::test]
fn empty_state_uses_its_default_or_requested_inset(cx: &mut TestAppContext) {
    use super::tokens::{SPACE_10, SPACE_16};
    let cx = draw(cx, || {
        div()
            .w(px(240.))
            .flex()
            .flex_col()
            .child(EmptyState::new("default", "Empty"))
            .child(EmptyState::new("code", "Empty").padding(SPACE_16, SPACE_16))
            .child(
                div()
                    .debug_selector(|| "text".into())
                    .child(Label::new("Empty").size(super::components::LabelSize::Small)),
            )
            .into_any_element()
    });
    let default = cx.debug_bounds("default").unwrap();
    let code = cx.debug_bounds("code").unwrap();
    let text = cx.debug_bounds("text").unwrap();
    assert_eq!(default.size.height, text.size.height + SPACE_10 * 2.);
    assert_eq!(code.size.height, text.size.height + SPACE_16 * 2.);
    assert_eq!(default.size.width, code.size.width);
    assert_eq!(default.size.width, px(240.));
}
