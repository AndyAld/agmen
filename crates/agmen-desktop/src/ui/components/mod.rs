//! agmen's UI components: stateless `RenderOnce` building blocks with
//! builder methods. They take sizes from `ui::tokens` and colors only from
//! the theme; views compose them and add layout.

mod avatar;
mod badge;
mod button;
mod chip;
mod color;
mod icon;
mod icon_button;
mod key_hint;
mod kv;
mod label;
mod list_item;
mod progress;
mod status_dot;
mod tab;

pub(crate) use avatar::Avatar;
pub(crate) use badge::Badge;
pub(crate) use button::{Button, ButtonStyle};
pub(crate) use chip::Chip;
pub(crate) use color::Color;
pub(crate) use icon::{Icon, IconSize};
pub(crate) use icon_button::{ClickHandler, IconButton, IconButtonStyle};
pub(crate) use key_hint::KeyHint;
pub(crate) use kv::{KvRow, KvSection};
pub(crate) use label::{Label, LabelSize};
pub(crate) use list_item::{Indent, ListItem};
pub(crate) use progress::ProgressBar;
pub(crate) use status_dot::StatusDot;
pub(crate) use tab::Tab;
