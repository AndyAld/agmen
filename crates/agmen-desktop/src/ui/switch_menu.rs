//! The take-over terminal's switch agent menu (frame "Desktop · switch
//! agent", node 1802): the terminal agents on this machine with their
//! versions, a check on the current session's agent, Update… on an
//! outdated one, Add… on a missing one, and "Add a terminal agent…"
//! (spec 5.2, 5.5). It hangs from the agent chip without covering it
//! (mockup fix 27): [`place_menu`] picks the side.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, App, Bounds, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Point, RenderOnce, SharedString, Size, Styled as _, Window, div, point,
};

use super::add_agent_dialog::AgentDialog;
use super::components::{
    Button, ButtonSize, ButtonStyle, ChangeHandler, Color, Label, LabelSize, Menu, MenuRow,
    SectionHeading,
};
use super::tokens::{AUTO_LINE_11, SPACE_8, SWITCH_MENU_WIDTH};
use crate::backend::{AgentFound, InstalledAgent};

/// The button at the end of an agent's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowButton {
    /// The agent is older than agmen needs.
    Update,
    /// The agent is not on this machine.
    Add,
}

/// The note before a row's button, and its color.
#[must_use]
pub(crate) fn note(agent: &InstalledAgent) -> Option<(String, Color)> {
    let color = if agent.is_installed() {
        Color::Warning
    } else {
        Color::Muted
    };
    agent.not_ready().map(|text| (text, color))
}

#[must_use]
pub(crate) const fn row_button(agent: &InstalledAgent) -> Option<RowButton> {
    match agent.found {
        AgentFound::Ready { .. } | AgentFound::VersionUnknown => None,
        AgentFound::Outdated { .. } => Some(RowButton::Update),
        AgentFound::Missing => Some(RowButton::Add),
    }
}

/// Where the menu goes, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub(crate) position: Point<Pixels>,
    /// The menu's corner at `position`.
    pub(crate) anchor: Anchor,
    /// The room on the chosen side; a taller menu is cut to it and its
    /// list scrolls.
    pub(crate) max_height: Pixels,
}

/// The menu's place for the agent `chip` in the terminal `head`: its top
/// at the head's bottom when the menu fits above `area`'s bottom, else its
/// bottom at the head's top; when neither fits, the roomier side, with
/// `max_height` cutting the menu to that side's room. Either way it never
/// covers the chip (mockup fix 27), which gpui's own anchor flip would,
/// since it flips around one point, and which gpui's snap into the window
/// would for a menu taller than its room. Its left edge is the chip's,
/// moved left as far as it must to stay in `area`.
#[must_use]
pub(crate) fn place_menu(
    chip: Bounds<Pixels>,
    head: Bounds<Pixels>,
    menu: Size<Pixels>,
    area: Bounds<Pixels>,
) -> Placement {
    let below = area.bottom() - head.bottom();
    let above = head.top() - area.top();
    let x = chip.left().min(area.right() - menu.width).max(area.left());
    if menu.height <= below || below >= above {
        Placement {
            position: point(x, head.bottom()),
            anchor: Anchor::TopLeft,
            max_height: below.max(Pixels::ZERO),
        }
    } else {
        Placement {
            position: point(x, head.top()),
            anchor: Anchor::BottomLeft,
            max_height: above.max(Pixels::ZERO),
        }
    }
}

/// The menu, drawn in the shared [`Menu`] frame; a caller that places it
/// converts it with `Menu::from`. Tests find it as `"switch-menu"`, a row as
/// `"switch-menu:{agent}"` with `":check"`, `":version"`, `":current"`,
/// `":note"`, `":update"` and `":add"`, and the last row as
/// `"switch-menu:add-other"`.
#[derive(IntoElement)]
pub(crate) struct SwitchMenu {
    agents: Vec<InstalledAgent>,
    current: SharedString,
    handle: SharedString,
    on_switch: Option<ChangeHandler<String>>,
    on_dialog: Option<ChangeHandler<AgentDialog>>,
}

impl std::fmt::Debug for SwitchMenu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SwitchMenu")
            .field("agents", &self.agents.len())
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl SwitchMenu {
    /// The menu for `handle`'s session, which runs `current`.
    #[must_use]
    pub(crate) fn new(
        agents: Vec<InstalledAgent>,
        current: impl Into<SharedString>,
        handle: impl Into<SharedString>,
    ) -> Self {
        Self {
            agents,
            current: current.into(),
            handle: handle.into(),
            on_switch: None,
            on_dialog: None,
        }
    }

    /// Called with the agent of a row clicked to switch to; never the
    /// current one or a missing one.
    #[must_use]
    pub(crate) fn on_switch(
        mut self,
        handler: impl Fn(String, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_switch = Some(Rc::new(handler));
        self
    }

    /// Called with the dialog that Update…, Add… or "Add a terminal
    /// agent…" opens (mockup fix 38).
    #[must_use]
    pub(crate) fn on_dialog(
        mut self,
        handler: impl Fn(AgentDialog, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dialog = Some(Rc::new(handler));
        self
    }

    fn row(&self, agent: &InstalledAgent) -> MenuRow {
        let id = format!("switch-menu:{}", agent.name);
        let current = agent.name == self.current.as_ref();
        let version = agent.version().map(str::to_owned);
        let (name_color, weight) = if agent.is_installed() {
            (Color::Default, FontWeight::BOLD)
        } else {
            (Color::Muted, FontWeight::NORMAL)
        };
        let end = if current {
            Some(
                div()
                    .debug_selector(|| format!("{id}:current"))
                    .child(
                        Label::new("current session")
                            .size(LabelSize::Small)
                            .color(Color::Accent),
                    )
                    .into_any_element(),
            )
        } else {
            note(agent).map(|(text, color)| {
                let button = row_button(agent).map(|kind| self.button(agent, kind, &id));
                div()
                    .flex()
                    .items_center()
                    .gap(SPACE_8)
                    .child(
                        div()
                            .debug_selector(|| format!("{id}:note"))
                            .child(Label::new(text).size(LabelSize::Small).color(color)),
                    )
                    .children(button)
                    .into_any_element()
            })
        };
        let row = MenuRow::new(id.clone())
            .checked(current)
            .child(
                Label::new(agent.name.clone())
                    .code()
                    .color(name_color)
                    .weight(weight),
            )
            .when_some(version, |row, version| {
                row.child(
                    div()
                        .debug_selector(|| format!("{id}:version"))
                        .child(Label::new(version).code().color(Color::Placeholder)),
                )
            })
            .when_some(end, MenuRow::end);
        // The current row and a missing agent's row switch to nothing. A
        // take-over is an interactive terminal, so an agent too old for a
        // headless worker, or of unknown version, can still run it.
        let switchable = !current && agent.is_installed();
        match (&self.on_switch, switchable) {
            (Some(handler), true) => {
                let (handler, name) = (handler.clone(), agent.name.clone());
                row.on_click(move |_, window, cx| handler(name.clone(), window, cx))
            }
            _ => row,
        }
    }

    fn button(&self, agent: &InstalledAgent, kind: RowButton, id: &str) -> Button {
        let (label, style, selector, dialog) = match kind {
            RowButton::Update => (
                "Update…",
                ButtonStyle::Secondary,
                format!("{id}:update"),
                AgentDialog::Update(agent.name.clone()),
            ),
            RowButton::Add => (
                "Add…",
                ButtonStyle::Primary,
                format!("{id}:add"),
                AgentDialog::Add(agent.name.clone()),
            ),
        };
        let button = Button::new(SharedString::from(selector), label)
            .style(style)
            .size(ButtonSize::Compact)
            .weight(FontWeight::SEMIBOLD);
        match self.on_dialog.clone() {
            Some(handler) => button.on_click(move |_, window, cx| {
                // The row under the button must not switch too.
                cx.stop_propagation();
                handler(dialog.clone(), window, cx);
            }),
            None => button,
        }
    }
}

impl From<SwitchMenu> for Menu {
    fn from(menu: SwitchMenu) -> Self {
        let rows: Vec<MenuRow> = menu.agents.iter().map(|agent| menu.row(agent)).collect();
        let add_other = MenuRow::new("switch-menu:add-other")
            .leading(Label::new("+").color(Color::Accent))
            .child(Label::new("Add a terminal agent…").color(Color::Accent))
            .end(
                Label::new("any ACP agent, or a command")
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            )
            .when_some(menu.on_dialog.clone(), |row, handler| {
                row.on_click(move |_, window, cx| handler(AgentDialog::Other, window, cx))
            });
        let foot = format!(
            "Switching starts a new session of that agent in the same worktree, as {}, with a \
             handoff brief. The current session stays resumable. Found by checking PATH and \
             each tool's --version.",
            menu.handle
        );
        Self::new("switch-menu")
            .width(SWITCH_MENU_WIDTH)
            .header(SectionHeading::new("Terminal agents on this machine"))
            .children(rows)
            .child(add_other)
            .footer(
                Label::new(foot)
                    .size(LabelSize::Small)
                    .line_height(AUTO_LINE_11)
                    .color(Color::Placeholder)
                    .wrap(),
            )
    }
}

impl RenderOnce for SwitchMenu {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        Menu::from(self)
    }
}

#[cfg(test)]
#[path = "switch_menu_tests.rs"]
mod tests;
