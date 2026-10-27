//! GPUI views.

mod agent_panel;
mod agent_view;
mod agents_panel;
mod change_card;
mod chat;
mod chat_context;
mod code_view;
mod components;
mod composer;
mod dev_views;
mod diff_view;
mod dock;
mod format;
mod git_panel;
// agmen-c7 fix-hosting: the file icons a workspace's first frame draws.
mod icon_preload;
// C287: the clip truncated labels and the composer's chips paint under.
mod ink_clip;
mod inline_flow;
mod left_dock;
mod listener_panel;
mod message;
mod palette;
mod palette_filter;
mod pane;
mod project_panel;
mod ref_chip;
mod rich_text;
mod roles_panel;
mod scope_chip;
mod settings_view;
mod status_bar;
mod syntax;
mod tab_views;
#[cfg(test)]
mod test_support;
mod text_edit;
mod text_input;
mod title_bar;
mod tokens;
mod workspace;
// Task 15: the model picker and ASK cards.
mod ask_card;
mod model_picker;

// Task 13: needs you (the alert row's state, the banner and the toast).
mod alert_banner;
mod alert_row;
mod needs_you;
mod toast;
pub(crate) use needs_you::AlarmHandle;

// Task 14: take-over in the chat, the switch agent menu and the add agent
// dialog.
mod add_agent_dialog;
mod switch_menu;
mod terminal_view;

// Task 19: the planning room, plan panel, spec tab and kick-off dialog.
mod kickoff_dialog;
mod plan_model;
mod plan_panel;
mod plan_room;
#[cfg(test)]
mod plan_test_support;
mod review_panel;
mod spec_view;

pub(crate) use agent_view::AgentView;
pub(crate) use code_view::{CodeView, GutterMark};
pub(crate) use dev_views::{DevFrame, GitDev, GitDevLayout, ProjectDev, ProjectTests, RolesDev};
pub(crate) use scope_chip::ScopeOption;
pub(crate) use settings_view::{SettingsSources, SettingsView};
pub(crate) use syntax::Language;
pub(crate) use text_input::TextInput;
pub(crate) use title_bar::TitleBar;
pub(crate) use workspace::{Quit, Workspace};

use gpui::App;

// Task 18: the text input's actions, which src/keymap.rs binds.
pub(crate) mod text_input_actions {
    pub(crate) use super::text_input::{
        Backspace, Copy, Cut, Delete, Down, End, Home, Left, Newline, Paste, Right, SelectAll,
        SelectLeft, SelectRight, Submit, Up,
    };
}

/// `child` in a box tests find as `selector`.
fn tagged(selector: String, child: impl gpui::IntoElement) -> gpui::Div {
    use gpui::{InteractiveElement as _, ParentElement as _};
    gpui::div().debug_selector(|| selector).child(child)
}

/// The color of the rules between chrome regions (mockup BORDER2).
fn rule_color(cx: &App) -> gpui::Hsla {
    cx.global::<crate::appearance::Appearance>()
        .theme
        .border_variant
}
