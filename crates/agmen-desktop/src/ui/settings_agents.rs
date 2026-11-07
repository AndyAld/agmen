//! Agent safeguard rows and their immediate saves.

use agmen_local::safeguards::{
    AGENTS_OFF_WARNING, BUDGETS_OFF_WARNING, ChildBudgets, DEPTH_OFF_WARNING, Safeguards,
    SafeguardsError,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _,
    Window, div,
};

use super::SettingsView;
use super::rows::{controls, row, setting};
use crate::appearance::{load_safeguards, save_safeguards};
use crate::ui::components::{Button, Color, Field, InputFrame, Label, Segmented, Stepper, Toggle};

fn warning(selector: &'static str, text: &'static str) -> impl IntoElement {
    div()
        .debug_selector(|| selector.into())
        .w_full()
        .child(Label::new(text).color(Color::Warning).wrap())
}

impl SettingsView {
    fn change_safeguards(
        &mut self,
        change: impl FnOnce(&mut Safeguards),
        cx: &mut Context<Self>,
    ) -> bool {
        let path = self.sources.settings_path();
        let result = load_safeguards(&path).and_then(|mut value| {
            change(&mut value);
            save_safeguards(&path, &value)?;
            Ok(value)
        });
        let saved = match result {
            Ok(value) => {
                self.separate_child_usd.update(cx, |input, cx| {
                    input.set_placeholder(value.separate_child_usd.to_string(), cx);
                });
                self.safeguards = Ok(value);
                self.error = None;
                true
            }
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        };
        cx.notify();
        saved
    }

    fn save_separate_child_usd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let amount = self
            .separate_child_usd
            .read(cx)
            .text()
            .trim()
            .parse::<f64>();
        let amount = amount
            .map_err(|_| SafeguardsError::SeparateChildUsd)
            .and_then(|amount| {
                Safeguards {
                    separate_child_usd: amount,
                    ..Safeguards::default()
                }
                .validate()?;
                Ok(amount)
            });
        match amount {
            Ok(amount) => {
                if self.change_safeguards(|value| value.separate_child_usd = amount, cx) {
                    self.separate_child_usd.update(cx, |input, cx| {
                        input.set_text("", window, cx);
                    });
                }
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
            }
        }
    }

    fn depth_setting(&self, value: &Safeguards, cx: &Context<Self>) -> AnyElement {
        setting("Tree depth", "", cx)
            .child(
                controls()
                    .child(
                        row()
                            .child(
                                Toggle::new("agents:depth:limit", value.limit_tree_depth)
                                    .on_change(Self::handler(cx, |this, enabled, cx| {
                                        this.change_safeguards(
                                            |value| value.limit_tree_depth = enabled,
                                            cx,
                                        );
                                    })),
                            )
                            .when(value.limit_tree_depth, |this| {
                                this.child(
                                    Stepper::new("agents:depth:max", value.max_tree_depth)
                                        .range(1, 256)
                                        .on_change(Self::handler(cx, |this, max, cx| {
                                            this.change_safeguards(
                                                |value| value.max_tree_depth = max,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    )
                    .when(!value.limit_tree_depth, |this| {
                        this.child(warning("agents:depth:warning", DEPTH_OFF_WARNING))
                    }),
            )
            .into_any_element()
    }

    fn count_setting(&self, value: &Safeguards, cx: &Context<Self>) -> AnyElement {
        setting("Agents running per person", "", cx)
            .child(
                controls()
                    .child(
                        row()
                            .child(
                                Toggle::new("agents:count:limit", value.limit_agents).on_change(
                                    Self::handler(cx, |this, enabled, cx| {
                                        this.change_safeguards(
                                            |value| value.limit_agents = enabled,
                                            cx,
                                        );
                                    }),
                                ),
                            )
                            .when(value.limit_agents, |this| {
                                this.child(
                                    Stepper::new("agents:count:max", value.max_agents)
                                        .range(1, 256)
                                        .on_change(Self::handler(cx, |this, max, cx| {
                                            this.change_safeguards(
                                                |value| value.max_agents = max,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    )
                    .when(!value.limit_agents, |this| {
                        this.child(warning("agents:count:warning", AGENTS_OFF_WARNING))
                    }),
            )
            .into_any_element()
    }

    fn separate_budget_setting(&self, cx: &Context<Self>) -> AnyElement {
        setting("Separate child budget (USD)", "", cx)
            .child(
                controls().child(
                    Field::new("agents:separate-usd:field", "Amount").child(
                        row()
                            .w_full()
                            .child(
                                div()
                                    .debug_selector(|| "agents:separate-usd".into())
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        InputFrame::new().child(self.separate_child_usd.clone()),
                                    ),
                            )
                            .child(Button::new("agents:separate-usd:save", "Save").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.save_separate_child_usd(window, cx);
                                }),
                            )),
                    ),
                ),
            )
            .into_any_element()
    }

    pub(super) fn agents_page(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let value = match &self.safeguards {
            Ok(value) => value,
            Err(error) => {
                return vec![
                    div()
                        .debug_selector(|| "agents:error".into())
                        .child(Label::new(error.clone()).color(Color::Error).wrap())
                        .into_any_element(),
                ];
            }
        };
        let mut body = vec![
            self.depth_setting(value, cx),
            self.count_setting(value, cx),
            setting("Budgets", "", cx)
                .child(
                    controls()
                        .child(Toggle::new("agents:budgets", value.budgets).on_change(
                            Self::handler(cx, |this, enabled, cx| {
                                this.change_safeguards(|value| value.budgets = enabled, cx);
                            }),
                        ))
                        .when(!value.budgets, |this| {
                            this.child(warning("agents:budgets:warning", BUDGETS_OFF_WARNING))
                        }),
                )
                .into_any_element(),
        ];
        if value.budgets {
            body.push(
                setting("Child budgets", "", cx)
                    .child(
                        controls().child(
                            Segmented::new(
                                "agents:child-budgets",
                                ["Shared with parent", "Separate"],
                                usize::from(value.child_budgets == ChildBudgets::Separate),
                            )
                            .on_change(Self::handler(
                                cx,
                                |this, index, cx| {
                                    this.change_safeguards(
                                        |value| {
                                            value.child_budgets = if index == 0 {
                                                ChildBudgets::Shared
                                            } else {
                                                ChildBudgets::Separate
                                            };
                                        },
                                        cx,
                                    );
                                },
                            )),
                        ),
                    )
                    .into_any_element(),
            );
            if value.child_budgets == ChildBudgets::Separate {
                body.push(self.separate_budget_setting(cx));
            }
        }
        body
    }
}
