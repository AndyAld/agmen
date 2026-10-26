//! The local time zone's offset from UTC, so the chat shows times as the
//! person's clock does.

/// Minutes east of UTC, as a GPUI global the chat reads; without it the
/// chat shows UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct UtcOffset {
    minutes: i32,
}

impl gpui::Global for UtcOffset {}

/// No zone is more than 14 hours from UTC (Pacific/Kiritimati).
const MAX_OFFSET_MINUTES: i32 = 14 * 60;

impl UtcOffset {
    /// `minutes` east of UTC, or UTC when that is past any real zone.
    #[must_use]
    pub(crate) const fn from_minutes(minutes: i32) -> Self {
        if minutes.unsigned_abs() > MAX_OFFSET_MINUTES.unsigned_abs() {
            Self { minutes: 0 }
        } else {
            Self { minutes }
        }
    }

    pub(crate) const fn minutes(self) -> i32 {
        self.minutes
    }

    /// The system time zone's offset now.
    // shortcut: read once at launch, so a daylight saving change while the
    // app runs, or a message from before one, shows an hour off; use a
    // per-timestamp offset once a time zone source is in the tree.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub(crate) fn system() -> Self {
        let seconds = objc2_foundation::NSTimeZone::localTimeZone().secondsFromGMT();
        i32::try_from(seconds / 60).map_or(Self::default(), Self::from_minutes)
    }

    /// UTC: the S8 spike reads the zone on macOS only.
    // shortcut: Windows and Linux show UTC; read their zone when the app
    // ships there (after S8).
    #[cfg(not(target_os = "macos"))]
    #[must_use]
    pub(crate) fn system() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_past_any_zone_read_as_utc() {
        assert_eq!(UtcOffset::from_minutes(-360).minutes(), -360);
        assert_eq!(UtcOffset::from_minutes(14 * 60).minutes(), 840);
        assert_eq!(UtcOffset::from_minutes(15 * 60).minutes(), 0);
        assert_eq!(UtcOffset::from_minutes(i32::MIN).minutes(), 0);
    }

    #[test]
    fn system_offset_is_a_real_zone() {
        assert!(UtcOffset::system().minutes().abs() <= MAX_OFFSET_MINUTES);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn system_offset_is_utc_off_macos() {
        assert_eq!(UtcOffset::system(), UtcOffset::default());
    }
}
