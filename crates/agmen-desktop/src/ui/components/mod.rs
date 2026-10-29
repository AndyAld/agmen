//! agmen's UI components: stateless `RenderOnce` building blocks with
//! builder methods. They take sizes from `ui::tokens` and colors only from
//! the theme; views compose them and add layout.

use std::rc::Rc;

use gpui::{App, Window};

mod avatar;
mod badge;
mod breadcrumb;
mod button;
#[cfg(test)]
mod button_interaction_tests;
mod card;
mod chip;
mod chip_button;
mod color;
#[cfg(test)]
mod control_harness;
mod detail_row;
mod dialog_footer;
mod dot;
mod dropdown;
// Task 7: the live panels' empty states.
mod empty_state;
mod field;
mod icon;
mod icon_button;
mod input_frame;
mod key_hint;
mod kv;
mod label;
mod link;
mod list_item;
mod menu;
mod progress;
mod segmented;
mod setting_text;
mod status_dot;
mod status_item;
mod stepper;
mod tab;
mod toggle;
mod tool_card;
mod tooltip;
mod word_wrap;
// Task 18: the command palette's row label.
mod highlighted_label;
pub(crate) use highlighted_label::HighlightedLabel;
// Task 15: the model picker's scope options, and the message tag.
mod model_tag;
mod radio;
pub(crate) use {model_tag::ModelTag, radio::Radio};
// Task 19: the planning room, plan panel and kick-off dialog.
mod check_row;
mod modal;
mod section_header;
#[cfg(test)]
pub(crate) use section_header::count_badge;
mod table_card;

pub(crate) use avatar::Avatar;
pub(crate) use badge::Badge;
pub(crate) use breadcrumb::Breadcrumb;
pub(crate) use button::{Button, ButtonSize, ButtonStyle};
pub(crate) use card::Card;
pub(crate) use chip::Chip;
pub(crate) use chip_button::ChipButton;
pub(crate) use color::Color;
pub(crate) use detail_row::DetailRow;
pub(crate) use dialog_footer::DialogFooter;
pub(crate) use dot::Dot;
pub(crate) use empty_state::EmptyState;
pub(crate) use field::Field;
pub(crate) use icon::{Icon, IconSize};
pub(crate) use icon_button::{ClickHandler, IconButton, IconButtonStyle};
pub(crate) use input_frame::InputFrame;
pub(crate) use key_hint::KeyHint;
pub(crate) use kv::{KvRow, KvSection};
pub(crate) use label::{Label, LabelSize};
pub(crate) use link::Link;
pub(crate) use list_item::{Indent, ListItem};
pub(crate) use menu::Menu;
pub(crate) use progress::ProgressBar;
pub(crate) use setting_text::SettingText;
pub(crate) use status_dot::StatusDot;
pub(crate) use status_item::StatusItem;
pub(crate) use tab::Tab;
pub(crate) use tool_card::ToolCard;
pub(crate) use tooltip::Tooltip;
pub(crate) use word_wrap::WordWrap;
// Task 19.
pub(crate) use {
    check_row::{CheckMark, CheckRow, CheckState},
    modal::Modal,
    section_header::{SectionHeader, SectionTitleStyle},
    table_card::TableCard,
};
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
