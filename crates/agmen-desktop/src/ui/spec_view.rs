//! A spec's tab (frame "Desktop · spec", node 9089): the breadcrumb with
//! Approve (or "Approved ✓" once I approved) and Request changes, and the
//! spec's text at the commit the approval names. Tests find the buttons as
//! `spec:approve`, `spec:approved` (its check as `spec:approved:check`) and
//! `spec:request-changes`, a refusal as `spec:error` and the text as
//! `spec:code`.

use std::path::Path;

use agmen_proto::GitSha;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Window, div,
};

use super::code_view::CodeView;
use super::components::{Breadcrumb, Button, ButtonStyle, Color, Icon, IconSize, Label, LabelSize};
use super::plan_model::{ErrorSource, PlanModel};
use super::syntax::Language;
use super::tokens::{SPACE_3, SPACE_8, SPACE_14};
use crate::appearance::Appearance;
use crate::backend::Approval;
use crate::icons::UiIcon;

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
    /// The commit whose text the tab shows, which Approve names; `None`
    /// while the backend has sent no text for the spec.
    shown: Option<GitSha>,
    code: Entity<CodeView>,
    _model_changes: Subscription,
}

impl std::fmt::Debug for SpecView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpecView")
            .field("path", &self.path)
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

/// The spec's text and the commit it is at, as the plan last sent it.
fn spec_text(model: &PlanModel, path: &str) -> (Option<GitSha>, String) {
    model
        .plan()
        .and_then(|plan| plan.reviews.iter().find(|review| review.path == path))
        .map_or_else(
            // shortcut: only the fixture's design spec has text; agmend
            // serves the others at their commit once its local API exists.
            || (None, format!("{path} is not loaded yet.\n")),
            |review| (Some(review.sha.clone()), review.text.to_string()),
        )
}

impl SpecView {
    pub(crate) fn new(model: Entity<PlanModel>, path: &str, cx: &mut Context<Self>) -> Self {
        let (shown, text) = spec_text(model.read(cx), path);
        let code = Self::code_view(path, &text, cx);
        // When the backend moves the spec to a new commit, the tab shows the
        // new text before Approve can name that commit.
        let model_changes = cx.observe(&model, |this, model, cx| {
            let (shown, text) = spec_text(model.read(cx), &this.path);
            if shown != this.shown {
                this.code = Self::code_view(&this.path, &text, cx);
                this.shown = shown;
            }
            cx.notify();
        });
        Self {
            model,
            path: path.to_owned().into(),
            shown,
            code,
            _model_changes: model_changes,
        }
    }

    fn code_view(path: &str, text: &str, cx: &mut Context<Self>) -> Entity<CodeView> {
        let language = Language::for_path(Path::new(path));
        cx.new(|_| CodeView::new(text, language, Vec::new()).without_marks())
    }

    /// Approve, or "Approved ✓" once I approved, then Request changes;
    /// none for a draft or a spec I do not approve. The check is Lucide's
    /// in the selected label's accent: the UI font has no "✓" (Task 20).
    fn buttons(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let approval = self.model.read(cx).my_approval(&self.path)?;
        let approve = match (approval, self.shown.clone()) {
            (Approval::Approved, _) => Button::new("spec:approved", "Approved")
                .style(ButtonStyle::Plain)
                .selected(true)
                .trailing(
                    // The icon's 2 px side bearing plus the button's 5 px gap
                    // leave 7 px of air before the mark; the mockup's
                    // "Approved ✓" has about 4 (console.py:2035).
                    div()
                        .debug_selector(|| "spec:approved:check".into())
                        .ml(-SPACE_3)
                        .child(
                            Icon::new(UiIcon::Check)
                                .color(Color::Accent)
                                .size(IconSize::Small),
                        ),
                ),
            (Approval::Waiting, Some(sha)) => {
                let model = self.model.clone();
                let path = self.path.clone();
                Button::new("spec:approve", "Approve")
                    .style(ButtonStyle::Plain)
                    .on_click(move |_, _, cx| {
                        model.update(cx, |model, cx| model.approve_spec(&path, sha.clone(), cx));
                    })
            }
            // Nothing to read, so nothing to approve.
            (Approval::Waiting, None) => Button::new("spec:approve", "Approve")
                .style(ButtonStyle::Plain)
                .disabled(true),
        };
        // shortcut: the backend has no changes-requested action, so Request
        // changes draws disabled; enable it once agmend records one.
        let request = Button::new("spec:request-changes", "Request changes")
            .style(ButtonStyle::Plain)
            .disabled(true);
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
        let error = model
            .error(&ErrorSource::Spec(self.path.to_string()))
            .cloned();
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
