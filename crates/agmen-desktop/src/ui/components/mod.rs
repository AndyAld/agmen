//! agmen's UI components: stateless `RenderOnce` building blocks with
//! builder methods. They take sizes from `ui::tokens` and colors only from
//! the theme; views compose them and add layout.

use std::rc::Rc;

use gpui::{App, Window};

mod avatar;
mod badge;
mod breadcrumb;
mod button;
mod card;
mod chip;
mod color;
#[cfg(test)]
mod control_harness;
mod dot;
mod dropdown;
mod icon;
mod icon_button;
mod key_hint;
mod kv;
mod label;
mod list_item;
mod progress;
mod segmented;
mod status_dot;
mod stepper;
mod tab;
mod toggle;
mod tool_card;
// Task 18: the command palette's row label.
mod highlighted_label;
pub(crate) use highlighted_label::HighlightedLabel;

pub(crate) use avatar::Avatar;
pub(crate) use badge::Badge;
pub(crate) use breadcrumb::Breadcrumb;
pub(crate) use button::{Button, ButtonSize, ButtonStyle};
pub(crate) use card::Card;
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
pub(crate) use {dropdown::Dropdown, segmented::Segmented, stepper::Stepper, toggle::Toggle};

// Task 13: the needs-you banner's and the toast's glyph disc.
mod glyph_disc;
pub(crate) use glyph_disc::GlyphDisc;

// Task 14: the switch menu's rows, its and the add agent dialog's
// headings, and the dialog's text with command names in the code font.
mod code_text;
mod menu_row;
mod section_heading;
pub(crate) use code_text::CodeText;
#[cfg(test)]
pub(crate) use code_text::split_code;
pub(crate) use menu_row::MenuRow;
pub(crate) use section_heading::SectionHeading;

/// What a control reports when its value changes: the new value. An `Rc`, not
/// a `Box`, because segments, menu rows and stepper buttons share one handler.
pub(crate) type ChangeHandler<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;
