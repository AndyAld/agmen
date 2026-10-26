//! A spec's tab (frame "Desktop · spec", node 9089): the breadcrumb with
//! Approve (or "Approved ✓" once I approved) and Request changes, and the
//! spec's text at the commit the approval names. Tests find the buttons as
//! `spec:approve`, `spec:approved` and `spec:request-changes`, a refusal as
//! `spec:error` and the text as `spec:code`.

use std::path::Path;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div,
};

use super::code_view::CodeView;
use super::components::{Breadcrumb, Button, ButtonStyle, Color, Label, LabelSize};
use super::plan_model::PlanModel;
use super::syntax::Language;
use super::tokens::{SPACE_8, SPACE_14};
use crate::appearance::Appearance;
use crate::backend::Approval;

/// The breadcrumb's parts: the project, then each part of the path.
fn breadcrumb_parts(project: &str, path: &str) -> Vec<SharedString> {
    std::iter::once(project)
        .chain(path.split('/').filter(|part| !part.is_empty()))
        .map(|part| SharedString::from(part.to_owned()))
        .collect()
}

pub(crate) struct SpecView {
    model: Entity<PlanModel>,
    path: SharedString,
    code: Entity<CodeView>,
    _model_changes: Subscription,
}

impl std::fmt::Debug for SpecView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpecView")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl SpecView {
    pub(crate) fn new(model: Entity<PlanModel>, path: &str, cx: &mut Context<Self>) -> Self {
        let text = model
            .read(cx)
            .plan()
            .and_then(|plan| plan.reviews.iter().find(|review| review.path == path))
            .map(|review| review.text.to_string())
            // shortcut: only the fixture's design spec has text; agmend
            // serves the others at their commit once its local API exists.
            .unwrap_or_else(|| format!("{path} is not loaded yet.\n"));
        let language = Language::for_path(Path::new(path));
        let code = cx.new(|_| CodeView::new(&text, language, Vec::new()));
        let model_changes = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            path: path.to_owned().into(),
            code,
            _model_changes: model_changes,
        }
    }

    /// Approve, or "Approved ✓" once I approved, then Request changes;
    /// none for a draft or a spec I do not approve.
    fn buttons(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let approval = self.model.read(cx).my_approval(&self.path)?;
        let approve = match approval {
            Approval::Approved => Button::new("spec:approved", "Approved ✓")
                .style(ButtonStyle::Subtle)
                .selected(true),
            Approval::Waiting => {
                let model = self.model.clone();
                let path = self.path.clone();
                Button::new("spec:approve", "Approve")
                    .style(ButtonStyle::Subtle)
                    .on_click(move |_, _, cx| {
                        model.update(cx, |model, cx| model.approve_spec(&path, cx));
                    })
            }
        };
        // shortcut: the backend has no changes-requested action yet; the
        // planner hears it through the chat until agmend records one.
        let request =
            Button::new("spec:request-changes", "Request changes").style(ButtonStyle::Subtle);
        Some(
            div()
                .flex()
                .items_center()
                .gap(SPACE_8)
                .child(approve)
                .child(request),
        )
    }
}

impl Render for SpecView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let background = theme.editor_background;
        let model = self.model.read(cx);
        let parts = breadcrumb_parts(model.project(), &self.path);
        let error = model.error().cloned();
        let mut crumb = Breadcrumb::new(parts);
        if let Some(buttons) = self.buttons(cx) {
            crumb = crumb.end(buttons);
        }
        div()
            .debug_selector(|| "spec-view".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .child(crumb)
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .debug_selector(|| "spec:error".into())
                        .px(SPACE_14)
                        .child(Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
            })
            .child(
                div()
                    .debug_selector(|| "spec:code".into())
                    .flex_1()
                    .min_h_0()
                    .child(self.code.clone()),
            )
    }
}

#[cfg(test)]
#[path = "spec_view_tests.rs"]
mod tests;
