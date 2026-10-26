//! GPUI views.

mod agent_panel;
mod agents_panel;
mod components;
mod dock;
mod format;
mod listener_panel;
mod palette_filter;
mod status_bar;
#[cfg(test)]
mod test_support;
mod text_edit;
mod text_input;
mod title_bar;
mod tokens;
mod workspace;

pub(crate) use dock::DockSide;
pub(crate) use text_input::{TextInput, bind_keys};
pub(crate) use workspace::{Quit, ToggleLeftDock, ToggleRightDock, Workspace};

use gpui::App;

/// The color of the rules between chrome regions (mockup BORDER2).
fn rule_color(cx: &App) -> gpui::Hsla {
    cx.global::<crate::appearance::Appearance>()
        .theme
        .border_variant
}
