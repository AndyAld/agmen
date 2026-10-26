//! GPUI views.

mod agent_panel;
mod agent_view;
mod agents_panel;
mod breadcrumb;
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
mod keys;
mod left_dock;
mod listener_panel;
mod message;
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

pub(crate) use agent_view::AgentView;
pub(crate) use code_view::{CodeView, GutterMark};
pub(crate) use dev_views::{DevFrame, GitDev, GitDevLayout, ProjectDev, ProjectTests, RolesDev};
pub(crate) use dock::DockSide;
pub(crate) use keys::bind_workspace_keys;
pub(crate) use scope_chip::ScopeOption;
pub(crate) use settings_view::{SettingsSources, SettingsView};
pub(crate) use syntax::Language;
pub(crate) use text_input::{TextInput, bind_keys};
pub(crate) use title_bar::TitleBar;
pub(crate) use workspace::{Quit, Workspace};

use gpui::App;

/// The color of the rules between chrome regions (mockup BORDER2).
fn rule_color(cx: &App) -> gpui::Hsla {
    cx.global::<crate::appearance::Appearance>()
        .theme
        .border_variant
}
