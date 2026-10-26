//! GPUI views.

mod agent_panel;
mod agents_panel;
#[cfg_attr(not(test), expect(dead_code, reason = "hosted by the Task 9 chat"))]
mod change_card;
mod components;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the integrator hosts it in workspace.rs")
)]
mod composer;
mod dock;
mod format;
mod listener_panel;
mod palette_filter;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the integrator hosts it in workspace.rs")
)]
mod pane;
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
