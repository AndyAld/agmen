//! The take-over terminal (frame "Desktop · chat + terminal", node 1129):
//! between the chat and the composer, a head with the agent chip
//! (`claude ▾`, which opens the switch menu), how the session started, the
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
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, TextRun, WeakEntity,
    Window, anchored, canvas, deferred, div, font, point, px, size,
};

use super::add_agent_dialog::{AddAgentDialog, AgentDialog};
use super::components::{Button, ButtonSize, ButtonStyle, Chip, Color, Icon, Label, LabelSize};
use super::switch_menu::{SwitchMenu, place_menu};
use super::tokens::{
    BORDER_WIDTH, DIALOG_TOP, MONO_CELL_HEIGHT, RADIUS_LG, SPACE_6, SPACE_8, SPACE_10, SPACE_12,
    STATUS_BAR_HEIGHT, SWITCH_MENU_HEIGHT, SWITCH_MENU_WIDTH, TERMINAL_BLOCK_HEIGHT,
    TERMINAL_HEAD_HEIGHT, TINT_DIM, TINT_PENDING, tint,
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
#[derive(Debug, Clone, Copy, Default)]
struct Anchors {
    chip: Option<Bounds<Pixels>>,
    head: Option<Bounds<Pixels>>,
    menu: Option<Pixels>,
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
    /// Frames drawn again since the menu opened, to measure its place.
    menu_frames: u8,
}

/// The most frames the open menu draws again to measure its place: one for
/// the chip and head, one for the menu's own height.
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

    /// The agent chip, `claude ▾`: it opens the menu.
    fn agent_chip(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let anchors = self.anchors.clone();
        let open = self.menu_open;
        div()
            .id("terminal:agent")
            .debug_selector(|| "terminal:agent".into())
            .relative()
            .flex_none()
            .cursor_pointer()
            // While open, the menu's mouse down out closes it; a click here
            // too would reopen it (review C81).
            .when(!open, |this| {
                this.on_click(cx.listener(|this, _, _, cx| this.set_menu_open(true, cx)))
            })
            .child(
                Chip::new(self.take_over.harness.clone())
                    .picker()
                    .code()
                    .weight(FontWeight::BOLD)
                    .color(Color::Default)
                    .trailing(
                        Label::new("▾")
                            .size(LabelSize::XSmall)
                            .color(Color::Placeholder),
                    ),
            )
            .child(bounds_probe(move |bounds| {
                anchors.set(Anchors {
                    chip: Some(bounds),
                    ..anchors.get()
                });
            }))
    }

    fn head(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let anchors = self.anchors.clone();
        let worker = self.worker.clone();
        div()
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
            .child(self.agent_chip(cx))
            .child(
                div().debug_selector(|| "terminal:start".into()).child(
                    Label::new(start_label(&self.take_over.start))
                        .code()
                        .color(Color::Placeholder),
                ),
            )
            .child(
                Label::new(self.handle.clone())
                    .code()
                    .color(Color::Role(self.role.clone())),
            )
            .child(
                Chip::new("live · stays on this machine")
                    .inline()
                    .tinted(Color::Accent)
                    .leading(Label::new("●").size(LabelSize::XSmall).color(Color::Accent)),
            )
            .child(div().flex_1())
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

    /// The switch menu, placed by `place_menu` from the last frame's chip
    /// and head; `None` until both were drawn once.
    fn menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let anchors = self.anchors.get();
        let (chip, head) = (anchors.chip?, anchors.head?);
        let viewport = window.viewport_size();
        // The menu stays above the status bar.
        let area = Bounds::new(
            point(px(0.), px(0.)),
            size(viewport.width, viewport.height - STATUS_BAR_HEIGHT),
        );
        let height = anchors.menu.unwrap_or(SWITCH_MENU_HEIGHT);
        let place = place_menu(chip, head, size(SWITCH_MENU_WIDTH, height), area);
        // Cut to its side's room, the menu fits, so gpui's snap into the
        // window never moves it over the chip.
        let worker = self.worker.clone();
        let view = cx.entity().downgrade();
        let dialog_view = view.clone();
        let menu = SwitchMenu::new(
            self.agents.clone(),
            self.take_over.harness.clone(),
            self.handle.clone(),
        )
        .max_height(place.max_height)
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
        let probe = self.anchors.clone();
        let close =
            cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| this.set_menu_open(false, cx));
        let dialog_open = self.dialog.is_some();
        Some(
            deferred(
                anchored()
                    .position(place.position)
                    .anchor(place.anchor)
                    .snap_to_window()
                    .child(
                        div()
                            .occlude()
                            .relative()
                            .track_focus(&self.menu_focus)
                            // Escape closes it; src/keymap.rs binds it.
                            .key_context(SWITCH_MENU_CONTEXT)
                            .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                                this.set_menu_open(false, cx);
                            }))
                            // The dialog over the menu takes the clicks.
                            .when(!dialog_open, |this| this.on_mouse_down_out(close))
                            .child(menu)
                            .child(bounds_probe(move |bounds| {
                                probe.set(Anchors {
                                    menu: Some(bounds.size.height),
                                    ..probe.get()
                                });
                            })),
                    ),
            )
            .into_any_element(),
        )
    }

    /// The add agent dialog over a dim window (node 2417).
    fn dialog(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.clone()?;
        let agent = dialog
            .agent()
            .and_then(|name| self.agents.iter().find(|a| a.name == name))
            .cloned();
        let theme = &cx.global::<Appearance>().theme;
        let dim = tint(theme.background, TINT_DIM);
        let cancel = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| this.close_dialog(cx));
        let view = cx.entity().downgrade();
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
            .on_check(move |_, cx| with_view(&check_view, cx, Self::check_again));
        let viewport = window.viewport_size();
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .occlude()
                        .w(viewport.width)
                        .h(viewport.height)
                        .bg(dim)
                        .flex()
                        .justify_center()
                        .items_start()
                        .pt(DIALOG_TOP)
                        .child(
                            div()
                                .track_focus(&self.dialog_focus)
                                .on_mouse_down_out(cancel)
                                // Escape closes it; src/keymap.rs binds it.
                                .key_context(AGENT_DIALOG_CONTEXT)
                                .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                                    this.close_dialog(cx);
                                }))
                                .child(content),
                        ),
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
        let menu = if self.menu_open {
            let menu = self.menu(window, cx);
            // The chip and the head are measured as they draw, and the menu
            // after its first frame: draw once more with what was measured.
            let measured = self.anchors.get();
            if (menu.is_none() || measured.menu.is_none()) && self.menu_frames < MAX_MEASURE_FRAMES
            {
                self.menu_frames += 1;
                cx.on_next_frame(window, |_, _, cx| cx.notify());
            }
            menu
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
            .w_full()
            .flex_none()
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
                    .child(self.head(cx))
                    .child(TerminalGrid(self.screen.clone())),
            )
            .children(menu)
            .children(dialog)
    }
}

#[cfg(test)]
#[path = "terminal_view_tests.rs"]
mod tests;
