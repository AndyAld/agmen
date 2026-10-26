//! GPUI views.

mod dock;
mod text_edit;
mod text_input;
mod workspace;

pub(crate) use text_input::{TextInput, bind_keys};
pub(crate) use workspace::{Quit, ToggleLeftDock, ToggleRightDock, Workspace};
