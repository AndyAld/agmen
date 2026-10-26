//! The worktree chip at the top of the Project and Git panels: the shown
//! worktree's owner and branch, and a menu of the others.

use std::collections::BTreeMap;

use agmen_proto::IdentityId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::components::{Chip, Color, Dot, Label, menu};
use super::tokens::{RADIUS_MD, SPACE_2, SPACE_3, SPACE_6, SPACE_8, SPACE_10};
use crate::appearance::Appearance;
use crate::backend::{AgentInfo, WorktreeInfo};
use crate::icons::UiIcon;

/// The worktree's head as the scope chip and the Project panel's root row
/// show it.
pub(crate) fn branch_label(worktree: &WorktreeInfo) -> String {
    worktree.head.label()
}

/// The chip's text: the owner's agent name and the branch, such as
/// `fx1 · fix/c015-main-conflict`.
pub(crate) fn scope_label(worktree: &WorktreeInfo) -> String {
    let name = worktree
        .handle
        .rsplit_once('/')
        .map_or(worktree.handle.as_str(), |(_, name)| name);
    format!("{name} · {}", branch_label(worktree))
}

/// One worktree the chip can switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopeOption {
    pub(crate) owner: IdentityId,
    pub(crate) label: SharedString,
    /// The owner's role, which colors the dot; empty for a person.
    pub(crate) role: SharedString,
}

impl ScopeOption {
    /// One option per worktree, in order, with each owner's role from
    /// `agents`.
    pub(crate) fn for_worktrees(
        worktrees: &[WorktreeInfo],
        agents: &BTreeMap<IdentityId, AgentInfo>,
    ) -> Vec<Self> {
        worktrees
            .iter()
            .map(|wt| Self {
                owner: wt.owner.clone(),
                label: scope_label(wt).into(),
                role: agents
                    .get(&wt.owner)
                    .map(|agent| SharedString::from(agent.role.clone()))
                    .unwrap_or_default(),
            })
            .collect()
    }
}

/// What the chip tells the panel that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeChipEvent {
    /// Another worktree was picked from the menu.
    Switch(IdentityId),
}

/// Tests find the chip as `scope-chip` and menu row `i` as `scope-chip:{i}`.
pub(crate) struct ScopeChip {
    options: Vec<ScopeOption>,
    selected: usize,
    open: bool,
}

impl EventEmitter<ScopeChipEvent> for ScopeChip {}

impl ScopeChip {
    /// Shows the first option; an empty list draws an empty chip.
    pub(crate) const fn new(options: Vec<ScopeOption>) -> Self {
        Self {
            options,
            selected: 0,
            open: false,
        }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        self.open = false;
        if let Some(option) = self.options.get(index)
            && index != self.selected
        {
            self.selected = index;
            cx.emit(ScopeChipEvent::Switch(option.owner.clone()));
        }
        cx.notify();
    }

    fn menu_row(
        &self,
        index: usize,
        option: &ScopeOption,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = &cx.global::<Appearance>().theme;
        let hover = theme.element_hover;
        let selector = format!("scope-chip:{index}");
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(|| selector)
            .flex()
            .items_center()
            .gap(SPACE_6)
            .py(SPACE_3)
            .px(SPACE_8)
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .when(index == self.selected, |this| {
                this.bg(theme.element_selected)
            })
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| this.pick(index, cx)))
            .child(Dot::new(Color::Role(option.role.clone())))
            .child(Label::new(option.label.clone()).code())
    }
}

/// The closed chip: the role dot, the label in the code font and the text
/// color (node 4553, V90), and a chevron at the far end.
fn chip(label: SharedString, role: SharedString) -> Chip {
    Chip::new(label)
        .color(Color::Default)
        .dot(Color::Role(role))
        .code()
        .full_width()
        .trailing(UiIcon::ChevronDown)
}

impl Render for ScopeChip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (label, role) = self
            .options
            .get(self.selected)
            .map(|o| (o.label.clone(), o.role.clone()))
            .unwrap_or_default();
        let chip = div()
            .id("scope-chip")
            .debug_selector(|| "scope-chip".into())
            .w_full()
            .cursor_pointer()
            // While open, the menu's mouse down out closes it; a click here
            // too would reopen it on mouse up (C186, as the dropdown's C81).
            .when(!self.open, |chip| {
                chip.on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
            })
            .child(chip(label, role));
        let menu = self.open.then(|| {
            let rows: Vec<_> = self
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| self.menu_row(index, option, cx).into_any_element())
                .collect();
            let this = cx.entity().downgrade();
            menu(
                "scope-chip:menu".into(),
                None,
                rows,
                move |_, cx| {
                    // Gone only while the window closes; nothing to close.
                    if let Some(this) = this.upgrade() {
                        this.update(cx, |this, cx| this.close(cx));
                    }
                },
                cx,
            )
        });
        div()
            .relative()
            .w_full()
            .flex_none()
            .pt(SPACE_2)
            .pb(SPACE_8)
            .px(SPACE_10)
            .child(chip)
            .when_some(menu, |this, menu| {
                this.child(div().absolute().top_full().left(SPACE_10).child(menu))
            })
    }
}

#[cfg(test)]
#[path = "scope_chip_tests.rs"]
mod tests;
