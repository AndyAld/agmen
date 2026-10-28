//! The right dock's Review panel beside a spec tab (frame "Desktop · spec",
//! node 9283): who approved the spec and when, and the comments on it.

use agmen_proto::Timestamp;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};
use jiff::tz::TimeZone;

use super::components::{Color, KvRow, KvSection};
use super::dock::RightDockHeader;
use super::format::clock;
use super::tokens::{SPACE_14, SPACE_16};
use crate::backend::{Approval, PlanInfo, SpecComment};

/// An approver's line: "✓ approved 15:47" or "○ waiting" (nodes 9294, 9297).
#[must_use]
fn approval_line(approval: Approval, at: Option<Timestamp>, zone: &TimeZone) -> (String, Color) {
    match (approval, at) {
        (Approval::Approved, Some(at)) => {
            (format!("✓ approved {}", clock(at, zone)), Color::Success)
        }
        (Approval::Approved, None) => ("✓ approved".into(), Color::Success),
        (Approval::Waiting, _) => ("○ waiting".into(), Color::Warning),
    }
}

/// Tests find it as `review-panel`.
#[derive(Debug, IntoElement)]
pub(crate) struct ReviewPanel {
    file: SharedString,
    approvals: Vec<(String, String, Color)>,
    comments: Vec<SpecComment>,
}

impl ReviewPanel {
    /// The review of the spec at `path`, with times in `zone`; `None` when
    /// the plan does not list it.
    #[must_use]
    pub(crate) fn new(plan: &PlanInfo, path: &str, zone: &TimeZone) -> Option<Self> {
        let spec = plan.specs.iter().find(|spec| spec.path == path)?;
        let review = plan.reviews.iter().find(|review| review.path == path);
        let approvals = spec
            .approvals
            .iter()
            .map(|(person, approval)| {
                let at = review.and_then(|r| r.approved_at.get(person)).copied();
                let (line, color) = approval_line(*approval, at, zone);
                (person.clone(), line, color)
            })
            .collect();
        let file = path.rsplit('/').next().unwrap_or(path);
        Some(Self {
            file: file.to_owned().into(),
            approvals,
            comments: review.map(|r| r.comments.clone()).unwrap_or_default(),
        })
    }
}

impl RenderOnce for ReviewPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let approvals = self.approvals.into_iter().fold(
            KvSection::new("Approvals"),
            |section, (person, line, color)| section.row(KvRow::new(person, line).color(color)),
        );
        let comments = self
            .comments
            .into_iter()
            .fold(KvSection::new("Comments"), |section, comment| {
                section.row(KvRow::new(comment.author, comment.body))
            });
        div()
            .debug_selector(|| "review-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .child(RightDockHeader::new("Review", self.file))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(SPACE_16)
                    .p(SPACE_14)
                    .child(approvals)
                    .child(comments),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, px};

    use super::*;
    use crate::backend::Screen;
    use crate::ui::plan_test_support::{mountain, plan};
    use crate::ui::test_support::rendered_size;

    const DESIGN: &str = "docs/specs/ridgeline-design.md";

    #[test]
    fn approvals_say_who_and_when() {
        let panel = ReviewPanel::new(&plan(Screen::Spec), DESIGN, &mountain()).unwrap();
        assert_eq!(panel.file.as_ref(), "ridgeline-design.md");
        assert_eq!(
            panel.approvals,
            [
                ("andy".into(), "✓ approved 15:47".into(), Color::Success),
                ("sean".into(), "○ waiting".into(), Color::Warning),
            ]
        );
        assert_eq!(panel.comments.len(), 3);
        assert_eq!(panel.comments[1].author, "andy");
        // Mockup fix 33: once sean approved, the review says so.
        let ready = ReviewPanel::new(&plan(Screen::PlanReady), DESIGN, &mountain()).unwrap();
        assert_eq!(ready.approvals[1].1, "✓ approved 15:52");
    }

    #[test]
    fn an_unlisted_spec_has_no_review() {
        assert!(ReviewPanel::new(&plan(Screen::Spec), "docs/nope.md", &mountain()).is_none());
    }

    #[gpui::test]
    fn review_panel_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .h(px(600.))
                .child(ReviewPanel::new(&plan(Screen::Spec), DESIGN, &mountain()).unwrap())
                .into_any_element()
        });
        assert!(size.height > px(100.));
    }
}
