//! The title bar: app name, project, listener chip and my avatar.

use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, div,
};

use super::components::{Avatar, Chip, Color, Label};
use super::format::listener_label;
use super::tokens::{SPACE_10, SPACE_12, TITLE_BAR_HEIGHT, TRAFFIC_LIGHTS_INSET};
use crate::backend::Snapshot;

#[derive(Debug, IntoElement)]
pub(crate) struct TitleBar {
    project: SharedString,
    listener: SharedString,
    me: SharedString,
}

impl TitleBar {
    #[must_use]
    pub(crate) fn new(snapshot: &Snapshot) -> Self {
        Self {
            project: snapshot.project.clone().into(),
            listener: listener_label(&snapshot.listener.agent, &snapshot.listener.state).into(),
            me: snapshot.me.clone().into(),
        }
    }
}

impl RenderOnce for TitleBar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .h(TITLE_BAR_HEIGHT)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_10)
            .pl(TRAFFIC_LIGHTS_INSET)
            .pr(SPACE_12)
            // The title bar is transparent, so the app moves the window.
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(Label::new("agmen").weight(FontWeight::SEMIBOLD))
            .child(Label::new("›").color(Color::Muted))
            .child(Label::new(self.project))
            .child(Chip::new(self.listener).dot(Color::Placeholder))
            .child(div().flex_1())
            .child(Avatar::new(&self.me))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::chat_snapshot;
    use crate::ui::test_support::rendered_size;
    use gpui::TestAppContext;

    #[test]
    fn title_bar_names_the_listener_with_a_colon() {
        let bar = TitleBar::new(&chat_snapshot());
        assert_eq!(bar.listener.as_ref(), "listener: codex · idle");
        assert_eq!(bar.project.as_ref(), "pr-3331");
    }

    #[gpui::test]
    fn title_bar_renders_at_its_height(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || TitleBar::new(&chat_snapshot()).into_any_element());
        assert_eq!(size.height, TITLE_BAR_HEIGHT);
    }
}
