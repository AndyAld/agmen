//! One line of a checklist: a check in the success color and the label in
//! text, or "○" in the warning color and the label dimmed (the plan panel's
//! "Before kick-off", nodes 9031 to 9043). The check is Lucide's, since the
//! UI font has no "✓" and its fallback draws a square-root tick (Task 20).
//! Tests find a done mark as `check:mark`.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::{Color, Icon, IconSize, Label, LabelSize};
use crate::icons::UiIcon;
use crate::ui::tokens::{ICON_SM, SPACE_8};

/// Whether a check passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CheckState {
    Done,
    Open,
}

impl From<bool> for CheckState {
    fn from(ok: bool) -> Self {
        if ok { Self::Done } else { Self::Open }
    }
}

impl CheckState {
    /// The mark's color, and a checklist label's color.
    const fn look(self) -> (Color, Color) {
        match self {
            Self::Done => (Color::Success, Color::Default),
            Self::Open => (Color::Warning, Color::Placeholder),
        }
    }

    /// The mark's color, which text beside it may share.
    pub(crate) fn color(self) -> Color {
        self.look().0
    }
}

/// A check's mark alone: Lucide's check at 12 px when done, the mockup's
/// 12 px "○" when open, each in its state's color.
#[derive(Debug, IntoElement)]
pub(crate) struct CheckMark {
    state: CheckState,
}

impl CheckMark {
    #[must_use]
    pub(crate) fn new(state: impl Into<CheckState>) -> Self {
        Self {
            state: state.into(),
        }
    }
}

impl RenderOnce for CheckMark {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.state.color();
        match self.state {
            CheckState::Done => div()
                .debug_selector(|| "check:mark".into())
                .flex_none()
                .child(Icon::new(UiIcon::Check).color(color).size(IconSize::Small))
                .into_any_element(),
            CheckState::Open => Label::new("○")
                .size(LabelSize::Small)
                .color(color)
                .into_any_element(),
        }
    }
}

#[derive(Debug, IntoElement)]
pub(crate) struct CheckRow {
    label: SharedString,
    state: CheckState,
}

impl CheckRow {
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>, state: impl Into<CheckState>) -> Self {
        Self {
            label: label.into(),
            state: state.into(),
        }
    }
}

impl RenderOnce for CheckRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let label_color = self.state.look().1;
        div()
            .flex()
            .items_center()
            .gap(SPACE_8)
            .child(
                // A minimum box, as the mockup's 12 px "○" (node 9038), so the
                // narrower "○" of the UI font keeps every label at one x.
                div()
                    .debug_selector(|| "check:glyph".into())
                    .flex()
                    .flex_none()
                    .items_center()
                    .min_w(ICON_SM)
                    .child(CheckMark::new(self.state)),
            )
            .child(
                div().debug_selector(|| "check:label".into()).child(
                    Label::new(self.label)
                        .size(LabelSize::Small)
                        .color(label_color),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::{draw, rendered_size};
    use gpui::{TestAppContext, px};

    #[test]
    fn done_and_open_look_like_the_mockup() {
        assert_eq!(
            CheckState::from(true).look(),
            (Color::Success, Color::Default)
        );
        assert_eq!(
            CheckState::from(false).look(),
            (Color::Warning, Color::Placeholder)
        );
    }

    #[gpui::test]
    fn done_mark_is_the_lucide_check(cx: &mut TestAppContext) {
        // Task 20 shot diff (kick-off, spec): the UI font has no ✓, and its
        // fallback drew a square-root tick, so the done mark is Lucide's.
        let cx = draw(cx, || CheckMark::new(CheckState::Done).into_any_element());
        let mark = cx.debug_bounds("check:mark").unwrap();
        assert_eq!((mark.size.width, mark.size.height), (ICON_SM, ICON_SM));
    }

    #[gpui::test]
    fn check_row_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            CheckRow::new("Lanes don't overlap", true).into_any_element()
        });
        assert!(size.width > px(40.));
    }

    /// Where the label of a row in `state` starts, from the row's left.
    fn label_start(cx: &mut TestAppContext, state: CheckState) -> gpui::Pixels {
        let cx = draw(cx, move || {
            CheckRow::new("Lanes don't overlap", state).into_any_element()
        });
        let row = cx.debug_bounds("smoke").unwrap();
        let label = cx.debug_bounds("check:label").unwrap();
        label.left() - row.left()
    }

    #[gpui::test]
    fn labels_start_at_one_x_whatever_the_glyph(cx: &mut TestAppContext) {
        // Shot diff (plan, nodes 9037 and 9043): the narrower open glyph
        // pulled its label 5 px left of the done rows' labels. The glyph
        // sits in a 12 px box (node 9038), then the 8 px gap.
        let want = ICON_SM + SPACE_8;
        assert_eq!(label_start(cx, CheckState::Done), want);
        assert_eq!(label_start(cx, CheckState::Open), want);
    }
}
