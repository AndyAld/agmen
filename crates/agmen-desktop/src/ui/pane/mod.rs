//! The center: panes with a tab bar and a breadcrumb bar (frame "Desktop ·
//! chat", nodes 115 and 130), and a group that splits them side by side.

mod group;
mod item;
mod names;
mod tabs;

#[cfg(test)]
pub(crate) use group::SplitDirection;
pub(crate) use group::{PaneGroup, PaneGroupEvent};
pub(crate) use item::{SettingsPage, TabItem};
pub(crate) use tabs::Pane;

#[cfg(test)]
mod tests;
