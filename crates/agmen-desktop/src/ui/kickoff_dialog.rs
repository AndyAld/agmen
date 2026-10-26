//! The kick-off confirm dialog (frame "Desktop · kick off", node 10290):
//! each owner's machine with their agents, spend and approval, what
//! happens on kick-off, and Cancel or Kick off. Kick off sends
//! `Action::KickOff`; every owner, the person included, has approved their
//! own share before the panel's button enables (`plan::kickoff_enabled`).
//!
//! Tests find it as `kickoff-dialog`, owner boxes as `kickoff:owner:{name}`,
//! the buttons as `kickoff:cancel` and `kickoff:go`, a refusal as
//! `kickoff:error`.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Entity, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, Styled as _, Window, div,
};
use jiff::tz::TimeZone;

// Escape closes the dialog as Zed's modals close: `menu::Cancel`, bound in
// the dialog's own context (src/keymap.rs), so an open menu elsewhere still
// takes Escape first.
use crate::keymap::{Cancel, KICKOFF_CONTEXT};

use super::components::{
    Button, ButtonSize, ButtonStyle, Card, Color, Label, LabelSize, Modal, SectionHeading,
};
use super::format::clock;
use super::plan_model::{ErrorSource, PlanModel};
use super::tokens::{MODAL_TOP, MODAL_WIDTH, RADIUS_CARD, SPACE_3, SPACE_8};
use crate::appearance::Appearance;
use crate::backend::PlanInfo;
use crate::local_time::LocalZone;
use crate::plan;

/// One owner's box: the title, their approval, and their agents and spend.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerBox {
    owner: String,
    title: String,
    approval: (String, Color),
    detail: String,
}

/// Each owner's box, mine first, then in team order (nodes 10292, 10298).
fn owner_boxes(plan: &PlanInfo, me: &str, zone: &TimeZone) -> Vec<OwnerBox> {
    let mut owners = plan::owners(plan);
    owners.sort_by_key(|owner| *owner != me);
    owners
        .into_iter()
        .map(|owner| {
            let mine = owner == me;
            let handles: Vec<&str> = plan
                .team
                .iter()
                .filter(|row| row.owner == owner)
                .map(|row| row.handle.as_str())
                .collect();
            let usd = plan::share(plan, owner).usd_per_day;
            let whose = if mine {
                "your".to_owned()
            } else {
                format!("{owner}'s")
            };
            let cap = plan
                .caps_usd
                .get(owner)
                .map(|cap| format!(" of {whose} ${cap} cap"))
                .unwrap_or_default();
            let who = if mine { "you" } else { owner };
            let approved = plan.shares_approved.get(owner).copied().unwrap_or(false);
            let approval = if approved {
                let at = plan
                    .shares_approved_at
                    .get(owner)
                    .map(|at| format!(" {}", clock(*at, zone)))
                    .unwrap_or_default();
                (format!("✓ {who} approved{at}"), Color::Success)
            } else {
                (format!("○ waiting on {who}"), Color::Warning)
            };
            OwnerBox {
                owner: owner.to_owned(),
                title: if mine {
                    format!("{owner}'s machine (you)")
                } else {
                    format!("{owner}'s machine")
                },
                approval,
                detail: format!("{} · ${usd}/day{cap}", handles.join(", ")),
            }
        })
        .collect()
}

/// "What happens" (nodes 10305 to 10314), from the plan's lanes, its plan
/// file and its coordinator.
fn what_happens(plan: &PlanInfo) -> [String; 4] {
    let lanes = plan.lanes.len();
    let plan_file = plan
        .specs
        .iter()
        .find(|spec| spec.path.starts_with("docs/plans/"))
        .map_or("the plan", |spec| spec.path.as_str());
    let coordinator = plan
        .team
        .iter()
        .find(|row| row.role == "coordinator")
        .map_or_else(
            || "The coordinator".to_owned(),
            |row| format!("{}/{}", row.owner, row.handle),
        );
    [
        "The specs are committed to docs/specs/ on main before anyone starts.".into(),
        format!("The hub creates {lanes} lanes and their items from {plan_file}."),
        "Each person's agmend launches only their own agents, each in a new worktree on a \
         {type}/{item}-{slug} branch."
            .into(),
        format!(
            "{coordinator} assigns the first items. From then on the agents coordinate with \
             each other in this project's chat, and anything that needs you arrives as an alert."
        ),
    ]
}

/// The dialog over the window while the plan model has it open.
#[derive(Debug, IntoElement)]
pub(crate) struct KickoffDialog {
    model: Entity<PlanModel>,
}

impl KickoffDialog {
    #[must_use]
    pub(crate) const fn new(model: Entity<PlanModel>) -> Self {
        Self { model }
    }
}

impl KickoffDialog {
    /// The dialog holds focus while open, as a Zed modal does, and gives it
    /// back when it closes (`PlanModel::close_kickoff`).
    fn hold_focus(&self, window: &mut Window, cx: &mut App) -> FocusHandle {
        let focus = self.model.read(cx).dialog_focus().clone();
        if !focus.contains_focused(window, cx) && !focus.is_focused(window) {
            let previous = window.focused(cx);
            self.model.update(cx, |m, _| m.remember_focus(previous));
            window.focus(&focus, cx);
        }
        focus
    }
}

impl RenderOnce for KickoffDialog {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let zone = cx
            .try_global::<LocalZone>()
            .map_or(TimeZone::UTC, |local| local.0.clone());
        let background = cx.global::<Appearance>().theme.background;
        let model = self.model.read(cx);
        let error = model.error(&ErrorSource::KickOff).cloned();
        // With no plan there is nothing to show or to give focus to.
        let Some(plan) = model.plan() else {
            return div();
        };
        let title = format!("Kick off {} (plan v{})", plan.name, plan.version);
        let (boxes, bullets) = (owner_boxes(plan, model.me(), &zone), what_happens(plan));
        let go = format!("Kick off {}", plan::agent_count(plan.team.len()));
        let focus = self.hold_focus(window, cx);
        let boxes = boxes.into_iter().map(|owner| {
            let selector = format!("kickoff:owner:{}", owner.owner);
            let (status, color) = owner.approval;
            let card = Card::new()
                .bg(background)
                .border_0()
                .rounded(RADIUS_CARD)
                .gap(SPACE_3)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(SPACE_8)
                        .child(Label::new(owner.title).weight(FontWeight::SEMIBOLD))
                        .child(div().flex_1())
                        .child(Label::new(status).size(LabelSize::Small).color(color)),
                )
                .child(Label::new(owner.detail).code().color(Color::Placeholder));
            div().debug_selector(|| selector).child(card)
        });
        let bullets = bullets.into_iter().map(|text| {
            div()
                .flex()
                .items_start()
                .gap(SPACE_8)
                .child(Label::new("•").size(LabelSize::Small).color(Color::Accent))
                .child(Label::new(text).size(LabelSize::Small).wrap())
        });
        let (cancel, kick) = (self.model.clone(), self.model.clone());
        let (dismiss, escape) = (self.model.clone(), self.model.clone());
        // Over the whole window: the workspace lays its overlays out as rows
        // of its column, so the dim needs a box of its own to fill.
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .track_focus(&focus)
            .key_context(KICKOFF_CONTEXT)
            .on_action(move |_: &Cancel, window, cx| {
                escape.update(cx, |m, cx| m.close_kickoff(window, cx));
            })
            .child(
                Modal::new("kickoff-dialog", MODAL_WIDTH, MODAL_TOP)
                    .on_dismiss(move |_, window, cx| {
                        dismiss.update(cx, |m, cx| m.close_kickoff(window, cx));
                    })
                    .child(
                        Label::new(title)
                            .size(LabelSize::Large)
                            .weight(FontWeight::SEMIBOLD)
                            .color(Color::TextMuted),
                    )
                    .children(boxes)
                    .child(SectionHeading::new("What happens"))
                    .children(bullets)
                    .when_some(error, |this, error| {
                        this.child(
                            div().debug_selector(|| "kickoff:error".into()).child(
                                Label::new(error).size(LabelSize::Small).color(Color::Error),
                            ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(SPACE_8)
                            .child(
                                Button::new("kickoff:cancel", "Cancel")
                                    .style(ButtonStyle::Inset)
                                    .size(ButtonSize::Large)
                                    .on_click(move |_, window, cx| {
                                        cancel.update(cx, |m, cx| m.close_kickoff(window, cx));
                                    }),
                            )
                            .child(
                                Button::new("kickoff:go", go)
                                    .style(ButtonStyle::Primary)
                                    .size(ButtonSize::Large)
                                    .on_click(move |_, window, cx| {
                                        kick.update(cx, |m, cx| m.kick_off(window, cx));
                                    }),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext, px};

    use super::*;
    use crate::backend::{Action, Screen};
    use crate::ui::plan_test_support::{Recording, mountain, plan, plan_globals};
    use crate::ui::test_support::{click, draw, redraw};

    #[test]
    fn owner_boxes_show_each_share_mine_first() {
        let boxes = owner_boxes(&plan(Screen::KickOff), "andy", &mountain());
        assert_eq!(
            boxes,
            [
                OwnerBox {
                    owner: "andy".into(),
                    title: "andy's machine (you)".into(),
                    approval: ("✓ you approved".into(), Color::Success),
                    detail: "coord, core, app, rev · $65/day of your $80 cap".into(),
                },
                OwnerBox {
                    owner: "sean".into(),
                    title: "sean's machine".into(),
                    approval: ("✓ sean approved 15:52".into(), Color::Success),
                    detail: "sync, qa, tiles · $40/day of sean's $60 cap".into(),
                },
            ]
        );
        let draft = owner_boxes(&plan(Screen::Plan), "andy", &mountain());
        assert_eq!(
            draft[1].approval,
            ("○ waiting on sean".into(), Color::Warning)
        );
    }

    #[test]
    fn what_happens_names_lanes_plan_file_and_coordinator() {
        let lines = what_happens(&plan(Screen::KickOff));
        assert_eq!(
            lines[1],
            "The hub creates 4 lanes and their items from docs/plans/m0.md."
        );
        assert!(lines[3].starts_with("andy/coord assigns the first items."));
    }

    fn open(
        cx: &mut TestAppContext,
    ) -> (
        std::rc::Rc<Recording>,
        Entity<PlanModel>,
        &mut VisualTestContext,
    ) {
        let (recording, model) = plan_globals(cx, Screen::KickOff, None);
        cx.update(|cx| model.update(cx, |model, cx| model.open_kickoff(cx)));
        let dialog = model.clone();
        let cx = draw(cx, move || {
            div()
                .relative()
                .w(px(1440.))
                .h(px(900.))
                .child(KickoffDialog::new(dialog.clone()))
                .into_any_element()
        });
        (recording, model, cx)
    }

    #[gpui::test]
    fn kick_off_sends_kick_off_and_closes(cx: &mut TestAppContext) {
        let (recording, model, cx) = open(cx);
        assert!(cx.debug_bounds("kickoff:owner:andy").is_some());
        click(cx, "kickoff:go");
        assert_eq!(*recording.acts.borrow(), [Action::KickOff]);
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            assert!(!model.kickoff_open());
            assert!(model.plan().unwrap().running);
        });
    }

    #[gpui::test]
    fn escape_closes_the_dialog(cx: &mut TestAppContext) {
        let (recording, model, cx) = open(cx);
        cx.update(|_, cx| crate::keymap::bind_all(cx));
        redraw(cx);
        cx.simulate_keystrokes("escape");
        assert!(recording.acts.borrow().is_empty());
        model.read_with(cx, |model, _| assert!(!model.kickoff_open()));
    }

    // Review minor 2: with no plan the dialog draws nothing, so it must not
    // take focus to a handle that is not in the tree.
    #[gpui::test]
    fn no_plan_takes_no_focus(cx: &mut TestAppContext) {
        let (_, model) = plan_globals(cx, Screen::KickOff, None);
        cx.update(|cx| {
            model.update(cx, |model, cx| {
                model.open_kickoff(cx);
                model.apply(None, cx);
            });
        });
        let dialog = model.clone();
        let cx = draw(cx, move || {
            KickoffDialog::new(dialog.clone()).into_any_element()
        });
        let focused = cx.update(|window, cx| model.read(cx).dialog_focus().is_focused(window));
        assert!(!focused);
    }

    // Shot diff (node 10320): Cancel and Kick off are the add agent dialog's
    // buttons (ButtonSize::Large), not the plan panel's taller ones.
    #[gpui::test]
    fn buttons_are_the_dialog_size(cx: &mut TestAppContext) {
        use crate::ui::test_support::rendered_size;
        let mut cancel_cx = TestAppContext::single();
        let cancel = rendered_size(&mut cancel_cx, || {
            Button::new("c", "Cancel")
                .style(ButtonStyle::Inset)
                .size(ButtonSize::Large)
                .into_any_element()
        });
        let mut go_cx = TestAppContext::single();
        let go = rendered_size(&mut go_cx, || {
            Button::new("g", "Kick off 7 agents")
                .style(ButtonStyle::Primary)
                .size(ButtonSize::Large)
                .into_any_element()
        });
        let (_, _, cx) = open(cx);
        assert_eq!(cx.debug_bounds("kickoff:cancel").unwrap().size, cancel);
        assert_eq!(cx.debug_bounds("kickoff:go").unwrap().size.width, go.width);
    }

    #[gpui::test]
    fn cancel_closes_without_acting(cx: &mut TestAppContext) {
        let (recording, model, cx) = open(cx);
        click(cx, "kickoff:cancel");
        assert!(recording.acts.borrow().is_empty());
        model.read_with(cx, |model, _| assert!(!model.kickoff_open()));
        redraw(cx);
    }
}
