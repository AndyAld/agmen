//! agmen's UI components: stateless `RenderOnce` building blocks with
//! builder methods. They take sizes from `ui::tokens` and colors only from
//! the theme; views compose them and add layout.

use std::rc::Rc;

use gpui::{App, Window};

mod avatar;
mod badge;
mod button;
mod chip;
mod color;
#[cfg(test)]
mod control_harness;
mod dot;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the Task 17 settings view")
)]
mod dropdown;
mod icon;
mod icon_button;
mod key_hint;
mod kv;
mod label;
mod list_item;
mod progress;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the Task 17 settings view")
)]
mod segmented;
mod status_dot;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the Task 17 settings view")
)]
mod stepper;
mod tab;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the Task 17 settings view")
)]
mod toggle;
mod tool_card;

pub(crate) use avatar::Avatar;
pub(crate) use badge::Badge;
pub(crate) use button::{Button, ButtonStyle};
pub(crate) use chip::Chip;
pub(crate) use color::Color;
pub(crate) use dot::Dot;
pub(crate) use icon::{Icon, IconSize};
pub(crate) use icon_button::{ClickHandler, IconButton, IconButtonStyle};
pub(crate) use key_hint::KeyHint;
pub(crate) use kv::{KvRow, KvSection};
pub(crate) use label::{Label, LabelSize};
pub(crate) use list_item::{Indent, ListItem};
pub(crate) use progress::ProgressBar;
pub(crate) use status_dot::StatusDot;
pub(crate) use tab::Tab;
pub(crate) use tool_card::ToolCard;
#[expect(unused_imports, reason = "used by the Task 17 settings view")]
pub(crate) use {dropdown::Dropdown, segmented::Segmented, stepper::Stepper, toggle::Toggle};

/// What a control reports when its value changes: the new value. An `Rc`, not
/// a `Box`, because segments, menu rows and stepper buttons share one handler.
pub(crate) type ChangeHandler<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;
