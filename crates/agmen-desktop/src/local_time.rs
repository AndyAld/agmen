//! The person's time zone, so the chat shows times as their clock does.

use jiff::tz::TimeZone;

/// The system time zone, as a GPUI global the chat reads; without it the
/// chat shows UTC. Each time takes its own offset in the zone, so messages
/// from before a daylight saving change show the time they were sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalZone(pub(crate) TimeZone);

impl gpui::Global for LocalZone {}

impl LocalZone {
    /// The system time zone, or UTC when it cannot be read (jiff's
    /// `TimeZone::system`).
    #[must_use]
    pub(crate) fn system() -> Self {
        Self(TimeZone::system())
    }
}
