//! The take-over terminal (frame "Desktop · chat + terminal", node 1129):
//! between the chat and the composer, a head with the agent chip
//! (claude with a chevron icon, which opens the switch menu), how the session started, the
//! agent's handle, "live · stays on this machine", Open as tab and Hand
//! back, over the agent's own screen as a grid of cells (spec 5.4, 5.5).
//! What is typed here goes to the agent on this machine, never the hub.

use std::cell::Cell;
use std::rc::Rc;

use agmen_proto::IdentityId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, Context, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, RenderOnce, Rgba,
    SharedString, Size, Styled as _, StyledText, TextRun, WeakEntity, Window, anchored, canvas,
    deferred, div, font, point, px, size,
};

use super::add_agent_dialog::{AddAgentDialog, AgentDialog};
use super::components::{
    Button, ButtonSize, ButtonStyle, Chip, ChipButton, Color, Icon, Label, LabelSize, Menu,
};
use super::switch_menu::{SwitchMenu, place_menu};
use super::tokens::{
    BORDER_WIDTH, MONO_CELL_HEIGHT, RADIUS_LG, SPACE_6, SPACE_8, SPACE_10, SPACE_12,
    STATUS_BAR_HEIGHT, SWITCH_MENU_HEIGHT, SWITCH_MENU_WIDTH, TERMINAL_BLOCK_HEIGHT,
    TERMINAL_HEAD_HEIGHT, TINT_PENDING, tint,
};
use crate::appearance::{Appearance, Theme};
use crate::backend::{
    Action, AgentFound, InstalledAgent, SessionStart, TakeOver, TermCell, TermColor, TerminalScreen,
};
use crate::icons::UiIcon;
use crate::keymap::{AGENT_DIALOG_CONTEXT, Cancel, SWITCH_MENU_CONTEXT};

/// What the terminal asks its chat to do through the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalEvent {
    Act(Action),
}

/// One run of cells with the same look; the cursor is a run of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CellRun {
    text: String,
    fg: TermColor,
    bg: TermColor,
    bold: bool,
    /// The block cursor: drawn in the terminal's colors swapped.
    cursor: bool,
}

/// Row `cells` as runs, with the cursor at `cursor_col` when it is on
/// this row.
fn row_runs(cells: &[TermCell], cursor_col: Option<u16>) -> Vec<CellRun> {
    let mut runs: Vec<CellRun> = Vec::new();
    for (col, cell) in (0u16..).zip(cells) {
        let cursor = cursor_col == Some(col);
        match runs.last_mut() {
            Some(run)
                if !cursor
                    && !run.cursor
                    && (run.fg, run.bg, run.bold) == (cell.fg, cell.bg, cell.bold) =>
            {
                run.text.push(cell.ch);
            }
            _ => runs.push(CellRun {
                text: cell.ch.to_string(),
                fg: cell.fg,
                bg: cell.bg,
                bold: cell.bold,
                cursor,
            }),
        }
    }
    runs
}

/// A cell color in `theme`: the default foreground, an ANSI color, or the
/// program's own.
fn term_color(color: TermColor, default: Hsla, theme: &Theme) -> Hsla {
    match color {
        TermColor::Default => default,
        TermColor::Ansi(ix) => theme.ansi.get(usize::from(ix)).copied().unwrap_or(default),
        TermColor::Rgb([r, g, b]) => Rgba {
            r: f32::from(r) / 255.,
            g: f32::from(g) / 255.,
            b: f32::from(b) / 255.,
            a: 1.,
        }
        .into(),
    }
}

/// The agent's screen, row by row in the terminal font.
#[derive(Debug, IntoElement)]
struct TerminalGrid(Rc<TerminalScreen>);

impl RenderOnce for TerminalGrid {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let theme = &appearance.theme;
        let fonts = &appearance.fonts;
        let family = fonts
            .terminal_family
            .clone()
            .unwrap_or_else(|| fonts.code_family.clone());
        let (fg, bg) = (theme.terminal_foreground, theme.terminal_background);
        let screen = self.0;
        let rows = (0..screen.rows()).map(|row| {
            let cursor = screen.cursor().filter(|(r, _)| *r == row).map(|(_, c)| c);
            let runs = row_runs(screen.row(row), cursor);
            let text: String = runs.iter().map(|run| run.text.as_str()).collect();
            let runs = runs
                .iter()
                .map(|run| {
                    let mut face = font(family.clone());
                    if run.bold {
                        face.weight = FontWeight::BOLD;
                    }
                    let (color, background) = if run.cursor {
                        (bg, Some(fg))
                    } else {
                        let back =
                            (run.bg != TermColor::Default).then(|| term_color(run.bg, bg, theme));
                        (term_color(run.fg, fg, theme), back)
                    };
                    TextRun {
                        len: run.text.len(),
                        font: face,
                        color,
                        background_color: background,
                        underline: None,
                        strikethrough: None,
                    }
                })
                .collect();
            div()
                .h(MONO_CELL_HEIGHT)
                .flex_none()
                .whitespace_nowrap()
                .child(StyledText::new(text).with_runs(runs))
        });
        div()
            .debug_selector(|| "terminal:screen".into())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .pt(SPACE_8)
            .pl(SPACE_10)
            .bg(bg)
            .text_size(fonts.terminal_size)
            .line_height(MONO_CELL_HEIGHT)
            .children(rows)
    }
}

/// "--resume 3f9c2e1a" or "new session · handoff brief" (nodes 1137,
/// 2824): a resumed session by its id's first part, as Claude Code shows it.
fn start_label(start: &SessionStart) -> String {
    match start {
        SessionStart::Resumed(id) => {
            let short: String = id.chars().take_while(|c| *c != '-').take(8).collect();
            format!("--resume {short}")
        }
        SessionStart::Handoff => "new session · handoff brief".into(),
    }
}

/// Where the chip and the head were drawn last, for the menu's place.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Anchors {
    chip: Option<Bounds<Pixels>>,
    head: Option<Bounds<Pixels>>,
    /// The agent's screen, as much of it as the window shows.
    screen: Option<Bounds<Pixels>>,
    menu: Option<Pixels>,
}

/// Where the switch menu may go in a `viewport` window: above the status
/// bar, and above the agent's input box on its `screen`, whose top rule is
/// the row over the cursor's, so the menu never covers what the person
/// types into or the composer below. With no cursor, the screen's bottom
/// ends it.
fn menu_area(
    viewport: Size<Pixels>,
    screen: Option<Bounds<Pixels>>,
    cursor_row: Option<u16>,
) -> Bounds<Pixels> {
    let mut bottom = viewport.height - STATUS_BAR_HEIGHT;
    if let Some(screen) = screen {
        // The grid's rows start SPACE_8 below its top (`TerminalGrid`).
        let input = cursor_row.map_or(screen.bottom(), |row| {
            screen.top() + SPACE_8 + MONO_CELL_HEIGHT * f32::from(row.saturating_sub(1))
        });
        bottom = bottom.min(input).min(screen.bottom());
    }
    Bounds::new(point(px(0.), px(0.)), size(viewport.width, bottom))
}

/// The take-over terminal of one agent. Tests find it as `"terminal"`, its
/// parts as `"terminal:agent"`, `":start"`, `":open-as-tab"`,
/// `":hand-back"` and `":screen"`.
pub(crate) struct TerminalView {
    worker: IdentityId,
    handle: SharedString,
    role: SharedString,
    take_over: TakeOver,
    screen: Rc<TerminalScreen>,
    agents: Vec<InstalledAgent>,
    focus: FocusHandle,
    /// The open menu's; it takes Escape however the menu was opened.
    menu_focus: FocusHandle,
    dialog_focus: FocusHandle,
    menu_open: bool,
    dialog: Option<AgentDialog>,
    copied: bool,
    /// How many agent scans had finished when Check again ran, until the
    /// scan's own result comes back.
    checking: Option<u64>,
    /// How many agent scans the backend has finished.
    scans: u64,
    /// What the last Check again found.
    note: Option<SharedString>,
    anchors: Rc<Cell<Anchors>>,
    /// Where keyboard focus moves on the next frame, once the element that
    /// holds it is in the window.
    focus_next: Option<FocusHandle>,
    /// Frames drawn again in a row because what the menu is placed from
    /// moved, as on opening or after a resize.
    menu_frames: u8,
}

/// The most frames in a row the open menu draws again to measure its place:
/// one for the chip and head, one for the menu's own height, one spare.
const MAX_MEASURE_FRAMES: u8 = 3;

impl std::fmt::Debug for TerminalView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalView")
            .field("handle", &self.handle)
            .field("harness", &self.take_over.harness)
            .field("menu_open", &self.menu_open)
            .field("dialog", &self.dialog)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    /// `handle`'s taken-over session, after `scans` agent scans; keyboard
    /// focus moves into it.
    pub(crate) fn new(
        worker: IdentityId,
        handle: impl Into<SharedString>,
        role: impl Into<SharedString>,
        take_over: TakeOver,
        agents: Vec<InstalledAgent>,
        scans: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        Self {
            worker,
            handle: handle.into(),
            role: role.into(),
            screen: Rc::new(take_over.screen.clone()),
            take_over,
            agents,
            focus_next: Some(focus.clone()),
            focus,
            menu_focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            menu_open: false,
            dialog: None,
            copied: false,
            checking: None,
            scans,
            note: None,
            anchors: Rc::default(),
            menu_frames: 0,
        }
    }

    pub(crate) const fn worker(&self) -> &IdentityId {
        &self.worker
    }

    /// The agent's handle, such as `andy/fx1`.
    pub(crate) fn handle(&self) -> &str {
        &self.handle
    }

    /// The agent the session runs, such as `claude`.
    #[cfg(test)]
    pub(crate) fn harness(&self) -> &str {
        &self.take_over.harness
    }

    /// What the last Check again found.
    #[cfg(test)]
    pub(crate) fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Takes the backend's latest state of the session and the agents,
    /// after `scans` agent scans. A pending Check again reads its result
    /// once a scan has finished since it ran, not on any other update: the
    /// dialog closes when the agent is now ready, or says it is not.
    pub(crate) fn set_state(
        &mut self,
        take_over: TakeOver,
        agents: Vec<InstalledAgent>,
        scans: u64,
        cx: &mut Context<Self>,
    ) {
        if take_over != self.take_over {
            self.screen = Rc::new(take_over.screen.clone());
            self.take_over = take_over;
        }
        self.agents = agents;
        self.scans = scans;
        if self.checking.is_some_and(|before| before != scans) {
            self.checking = None;
            self.note = self.check_result();
            if self.note.is_none() {
                // Back to the menu, which takes the keyboard again.
                self.dialog = None;
                self.focus_next = Some(self.menu_focus.clone());
            }
        }
        cx.notify();
    }

    /// Check again: asks the backend for a scan and waits for its result.
    fn check_again(&mut self, cx: &mut Context<Self>) {
        self.checking = Some(self.scans);
        self.act(Action::ScanAgents, cx);
    }

    /// The scan Check again asked for failed: the dialog says why.
    pub(crate) fn scan_failed(&mut self, error: SharedString, cx: &mut Context<Self>) {
        if self.checking.take().is_some() {
            self.note = Some(error);
            cx.notify();
        }
    }

    /// Why the dialog's agent is not ready yet; `None` when it is.
    fn check_result(&self) -> Option<SharedString> {
        let Some(dialog) = &self.dialog else {
            return None;
        };
        let Some(name) = dialog.agent() else {
            return Some("No new terminal agent found on PATH.".into());
        };
        match self
            .agents
            .iter()
            .find(|a| a.name == name)
            .map(|a| &a.found)
        {
            Some(AgentFound::Ready { .. }) => None,
            Some(AgentFound::Outdated {
                version, minimum, ..
            }) => {
                Some(format!("{name} is still {version}; agmen needs {minimum} or later.").into())
            }
            Some(AgentFound::VersionUnknown) => {
                Some(format!("{name}'s version still could not be read.").into())
            }
            Some(AgentFound::Missing) | None => {
                Some(format!("{name} is still not on PATH.").into())
            }
        }
    }

    /// Moves keyboard focus into the terminal on the next frame.
    pub(crate) fn request_focus(&mut self) {
        self.focus_next = Some(self.focus.clone());
    }

    /// Opens or closes the switch menu, as a click on the agent chip. Open,
    /// it takes the keyboard, so Escape closes it however it was opened;
    /// closed, the terminal takes the keyboard back.
    pub(crate) fn set_menu_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.menu_open = open;
        self.menu_frames = 0;
        if open {
            self.focus_next = Some(self.menu_focus.clone());
        } else {
            self.dialog = None;
            self.checking = None;
            self.focus_next = Some(self.focus.clone());
        }
        cx.notify();
    }

    /// Opens `dialog` over the open menu, with keyboard focus in it.
    pub(crate) fn open_dialog(&mut self, dialog: AgentDialog, cx: &mut Context<Self>) {
        self.menu_open = true;
        self.dialog = Some(dialog);
        self.copied = false;
        self.note = None;
        self.checking = None;
        self.focus_next = Some(self.dialog_focus.clone());
        cx.notify();
    }

    /// Closes the dialog and returns to the menu (node 2443 links back to
    /// the switch menu).
    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        self.checking = None;
        self.focus_next = Some(self.menu_focus.clone());
        cx.notify();
    }

    fn act(&self, action: Action, cx: &mut Context<Self>) {
        cx.emit(TerminalEvent::Act(action));
    }

    /// The agent chip, claude with a chevron icon: it opens the menu.
    fn agent_chip(
        &self,
        menu: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let anchors = self.anchors.clone();
        let open = self.menu_open;
        ChipButton::new(
            "terminal:agent",
            Chip::new(self.take_over.harness.clone())
                .picker()
                .code()
                .weight(FontWeight::BOLD)
                .color(Color::Default),
        )
        .flex_none()
        .caret(true)
        .open(open)
        .on_toggle(cx.processor(|this, open, _, cx| this.set_menu_open(open, cx)))
        .menu(menu)
        .on_bounds(move |bounds| {
            anchors.set(Anchors {
                chip: Some(bounds),
                ..anchors.get()
            });
        })
    }

    /// The head row, with the open `menu` hanging from it.
    fn head(&self, menu: Option<AnyElement>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let anchors = self.anchors.clone();
        let worker = self.worker.clone();
        // Each item takes a line as tall as the head, so the ones that do
        // not fit wrap onto a hidden second line, whole and from the end:
        // a narrow pane drops the live chip first and keeps the buttons.
        let line = |item: AnyElement| {
            div()
                .h(TERMINAL_HEAD_HEIGHT)
                .flex()
                .items_center()
                .child(item)
        };
        let info = [
            div()
                .debug_selector(|| "terminal:start".into())
                .child(
                    Label::new(start_label(&self.take_over.start))
                        .code()
                        .color(Color::Placeholder),
                )
                .into_any_element(),
            Label::new(self.handle.clone())
                .code()
                .color(Color::Role(self.role.clone()))
                .into_any_element(),
            Chip::new("live · stays on this machine")
                .inline()
                .tinted(Color::Accent)
                .leading(Label::new("●").size(LabelSize::XSmall).color(Color::Accent))
                .into_any_element(),
        ];
        div()
            .debug_selector(|| "terminal:head".into())
            .relative()
            .h(TERMINAL_HEAD_HEIGHT)
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap(SPACE_10)
            .pl(SPACE_12)
            .pr(SPACE_10)
            .bg(cx.global::<Appearance>().theme.elevated_surface)
            .child(Icon::new(UiIcon::Terminal).color(Color::Accent))
            .child(self.agent_chip(menu, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(TERMINAL_HEAD_HEIGHT)
                    .overflow_hidden()
                    .flex()
                    .flex_wrap()
                    .gap_x(SPACE_10)
                    .children(info.map(line)),
            )
            // shortcut: Open as tab draws but does nothing until the center
            // has a terminal tab (spec 5.4); the pane's TabItem has no
            // Terminal variant yet, and the pane belongs to agmen-c7.
            .child(
                Button::new("terminal:open-as-tab", "Open as tab")
                    .style(ButtonStyle::Secondary)
                    .size(ButtonSize::Compact),
            )
            .child(
                Button::new("terminal:hand-back", "Hand back")
                    .style(ButtonStyle::Secondary)
                    .size(ButtonSize::Compact)
                    .trailing(Label::new("×").size(LabelSize::Small).color(Color::Muted))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.act(Action::HandBack(worker.clone()), cx);
                    })),
            )
            .child(bounds_probe(move |bounds| {
                anchors.set(Anchors {
                    head: Some(bounds),
                    ..anchors.get()
                });
            }))
    }

    /// The switch menu: `place_menu` picks its side and room from the last
    /// frame's chip and head. The chip's current place sets where it draws,
    /// so a resize follows the chip. `None` until both were drawn once.
    fn menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let anchors = self.anchors.get();
        let (chip, head) = (anchors.chip?, anchors.head?);
        let area = menu_area(
            window.viewport_size(),
            anchors.screen,
            self.screen.cursor().map(|(row, _)| row),
        );
        let height = anchors.menu.unwrap_or(SWITCH_MENU_HEIGHT);
        let place = place_menu(chip, head, size(SWITCH_MENU_WIDTH, height), area);
        // From the chip's top left: under the chip, at the head's bottom
        // or top edge. Cut to its side's room, the menu fits, so gpui's
        // snap into the window never moves it over the chip.
        let offset = point(
            place.position.x - chip.left(),
            place.position.y - chip.top(),
        );
        let worker = self.worker.clone();
        let view = cx.entity().downgrade();
        let close_view = view.clone();
        let dialog_view = view.clone();
        let probe = self.anchors.clone();
        let max_height = place.max_height;
        let dialog_open = self.dialog.is_some();
        let menu = SwitchMenu::new(
            self.agents.clone(),
            self.take_over.harness.clone(),
            self.handle.clone(),
        )
        .on_switch(move |agent, _, cx| {
            let action = Action::SwitchAgent {
                worker: worker.clone(),
                agent,
            };
            with_view(&view, cx, |this, cx| {
                this.set_menu_open(false, cx);
                this.act(action, cx);
            });
        })
        .on_dialog(move |dialog, _, cx| {
            with_view(&dialog_view, cx, |this, cx| this.open_dialog(dialog, cx));
        });
        let menu = Menu::from(menu)
            .place(place.anchor, offset)
            .max_height(max_height)
            .track_focus(&self.menu_focus)
            // Escape closes it; src/keymap.rs binds it.
            .key_context(SWITCH_MENU_CONTEXT)
            .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                this.set_menu_open(false, cx);
            }))
            // The dialog over the menu takes the clicks.
            .when(!dialog_open, |menu| {
                menu.on_close(move |_, cx| {
                    with_view(&close_view, cx, |this, cx| this.set_menu_open(false, cx));
                })
            })
            .on_bounds(move |bounds| {
                // A menu cut to its room is not its own height; keep the
                // last whole one, or it would stay on a side too short.
                if bounds.size.height < max_height {
                    probe.set(Anchors {
                        menu: Some(bounds.size.height),
                        ..probe.get()
                    });
                }
            });
        // The offset counts from the chip's top left.
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .child(menu)
                .into_any_element(),
        )
    }

    /// The add agent dialog in a `Modal` over the window (nodes 2417 and
    /// 2418): a click on the dim or Escape closes it.
    fn dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.clone()?;
        let agent = dialog
            .agent()
            .and_then(|name| self.agents.iter().find(|a| a.name == name))
            .cloned();
        let view = cx.entity().downgrade();
        let dismiss_view = view.clone();
        let (copied_view, cancel_view, check_view) = (view.clone(), view.clone(), view);
        let content = AddAgentDialog::new(dialog, agent)
            .copied(self.copied)
            .note(self.note.clone())
            .on_copied(move |_, cx| {
                with_view(&copied_view, cx, |this, cx| {
                    this.copied = true;
                    cx.notify();
                });
            })
            .on_cancel(move |_, cx| with_view(&cancel_view, cx, Self::close_dialog))
            .on_check(move |_, cx| with_view(&check_view, cx, Self::check_again))
            .into_modal()
            .on_dismiss(move |_, _, cx| {
                with_view(&dismiss_view, cx, Self::close_dialog);
            });
        let viewport = window.viewport_size();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .relative()
                        .w(viewport.width)
                        .h(viewport.height)
                        .track_focus(&self.dialog_focus)
                        // Escape closes it; src/keymap.rs binds it.
                        .key_context(AGENT_DIALOG_CONTEXT)
                        .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                            this.close_dialog(cx);
                        }))
                        .child(content),
                ),
            )
            // Over the menu.
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// Runs `update` on the terminal behind `view`, from the menu or the
/// dialog it drew.
fn with_view(
    view: &WeakEntity<TerminalView>,
    cx: &mut App,
    update: impl FnOnce(&mut TerminalView, &mut Context<TerminalView>),
) {
    // The menu and the dialog outlive their view only while the window
    // closes, when there is nothing left to update.
    view.update(cx, update).unwrap_or_default();
}

/// An empty layer over its parent that reports the parent's window bounds
/// each frame.
fn bounds_probe(report: impl Fn(Bounds<Pixels>) + 'static) -> impl IntoElement {
    canvas(move |bounds, _, _| report(bounds), |_, (), _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(focus) = self.focus_next.take() {
            window.defer(cx, move |window, cx| window.focus(&focus, cx));
        }
        let border = tint(cx.global::<Appearance>().theme.text_accent, TINT_PENDING);
        let anchors = self.anchors.clone();
        let menu = if self.menu_open {
            // Placed from what was measured last frame; once this frame is
            // drawn, draw again if the chip, the head or the menu moved.
            let used = self.anchors.get();
            cx.on_next_frame(window, move |this, _, cx| {
                if !this.menu_open || this.anchors.get() == used {
                    this.menu_frames = 0;
                } else if this.menu_frames < MAX_MEASURE_FRAMES {
                    this.menu_frames += 1;
                    cx.notify();
                }
            });
            self.menu(window, cx)
        } else {
            None
        };
        let dialog = if self.menu_open {
            self.dialog(window, cx)
        } else {
            None
        };
        div()
            .debug_selector(|| "terminal".into())
            .track_focus(&self.focus)
            .key_context("Terminal")
            .h(TERMINAL_BLOCK_HEIGHT)
            // A short window shrinks it to its head, so the composer and
            // Hand back stay in the window.
            .flex_shrink_1()
            .min_h(SPACE_6 + BORDER_WIDTH * 2. + TERMINAL_HEAD_HEIGHT)
            .w_full()
            .pt(SPACE_6)
            .px(SPACE_12)
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(RADIUS_LG)
                    .border(BORDER_WIDTH)
                    .border_color(border)
                    .bg(cx.global::<Appearance>().theme.terminal_background)
                    .child(self.head(menu, cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(TerminalGrid(self.screen.clone()))
                            .child(bounds_probe(move |bounds| {
                                anchors.set(Anchors {
                                    screen: Some(bounds),
                                    ..anchors.get()
                                });
                            })),
                    ),
            )
            .children(dialog)
    }
}

#[cfg(test)]
#[path = "terminal_view_tests.rs"]
mod tests;
