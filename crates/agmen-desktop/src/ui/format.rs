//! Money, token counts, durations, clock times and other numbers as the
//! views show them.

use agmen_proto::Timestamp;
use jiff::tz::TimeZone;

/// Dollars with cents: `$14.20`. Values that are not finite show `$0.00`.
#[must_use]
pub(crate) fn money(usd: f64) -> String {
    let usd = if usd.is_finite() { usd } else { 0. };
    format!("${usd:.2}")
}

/// The status bar's spend: `$14.20 today`.
#[must_use]
pub(crate) fn spend_today(usd: f64) -> String {
    format!("{} today", money(usd))
}

/// `$11.20 of $20.00`.
#[must_use]
pub(crate) fn money_of(spent: f64, cap: f64) -> String {
    format!("{} of {}", money(spent), money(cap))
}

/// A token count: `950`, `8k`, `1.5k`, `96k`, `418k`, `1.9M`.
#[must_use]
pub(crate) fn tokens(count: u64) -> String {
    let (unit, suffix) = match count {
        0..1_000 => return count.to_string(),
        // 999.5k and up rounds to 1M, not 1000k.
        1_000..999_500 => (1_000, "k"),
        _ => (1_000_000, "M"),
    };
    // Integer rounding, half up, so no float is involved.
    let tenths = count.saturating_add(unit / 20) / (unit / 10);
    if tenths >= 100 || tenths.is_multiple_of(10) {
        format!("{}{suffix}", count.saturating_add(unit / 2) / unit)
    } else {
        format!("{}.{}{suffix}", tenths / 10, tenths % 10)
    }
}

/// Minutes as `41 min`, `1 h`, `6 h 40 min`.
#[must_use]
pub(crate) fn duration(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The Listener panel's token line: `96k in · 8k out · up 6 h 40 min`.
#[must_use]
pub(crate) fn listener_tokens(tokens_in: u64, tokens_out: u64, uptime_minutes: u32) -> String {
    format!(
        "{} in · {} out · up {}",
        tokens(tokens_in),
        tokens(tokens_out),
        duration(uptime_minutes)
    )
}

/// The Agent panel's token line: `418k in · 36k out · 1.9M cache read`.
#[must_use]
pub(crate) fn agent_tokens(tokens_in: u64, tokens_out: u64, cache_read: u64) -> String {
    format!(
        "{} in · {} out · {} cache read",
        tokens(tokens_in),
        tokens(tokens_out),
        tokens(cache_read)
    )
}

/// `1 h 12 min since launch · 41 min working`.
#[must_use]
pub(crate) fn wall_clock(since_launch_minutes: u32, working_minutes: u32) -> String {
    format!(
        "{} since launch · {} working",
        duration(since_launch_minutes),
        duration(working_minutes)
    )
}

/// `$11.20 of $20.00 · 3 of 4 running`.
#[must_use]
pub(crate) fn workers_today(today: f64, cap: f64, running: u32, max: u32) -> String {
    format!("{} · {running} of {max} running", money_of(today, cap))
}

/// The listener as the title bar and status bar name it:
/// `listener: codex · idle`.
#[must_use]
pub(crate) fn listener_label(agent: &str, state: &str) -> String {
    format!("listener: {agent} · {state}")
}

/// `1 alert`, `2 alerts`.
#[must_use]
pub(crate) fn counted(count: u32, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

// Task 13: alert counts cap at 99+.
/// The largest count a badge or the status bar spells out; more reads `99+`.
pub(crate) const MAX_SHOWN_COUNT: usize = 99;

/// A count as a badge shows it: `7`, or `99+` past [`MAX_SHOWN_COUNT`].
#[must_use]
pub(crate) fn count_label(count: usize) -> String {
    if count > MAX_SHOWN_COUNT {
        format!("{MAX_SHOWN_COUNT}+")
    } else {
        count.to_string()
    }
}

/// [`counted`] with the count capped as [`count_label`] caps it:
/// `1 needs you`, `99+ need you`.
#[must_use]
pub(crate) fn counted_capped(count: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        count_label(count),
        if count == 1 { one } else { many }
    )
}

/// A lowercase name with its first letter capitalized: `engineer` to
/// `Engineer`.
#[must_use]
pub(crate) fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `at` as a 24 hour `15:31` in `tz`, at the offset `tz` has at that moment.
#[must_use]
pub(crate) fn clock(at: Timestamp, tz: &TimeZone) -> String {
    at.to_zoned(tz.clone()).strftime("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spend_formats_cents() {
        assert_eq!(spend_today(14.2), "$14.20 today");
        assert_eq!(money(5.0), "$5.00");
        assert_eq!(money(0.42), "$0.42");
        assert_eq!(money(f64::NAN), "$0.00");
        assert_eq!(money_of(11.2, 20.0), "$11.20 of $20.00");
    }

    #[test]
    fn tokens_format_like_mockup() {
        assert_eq!(
            listener_tokens(96_000, 8_000, 400),
            "96k in · 8k out · up 6 h 40 min"
        );
        assert_eq!(
            agent_tokens(418_000, 36_000, 1_900_000),
            "418k in · 36k out · 1.9M cache read"
        );
    }

    #[test]
    fn token_counts_round() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1_500), "1.5k");
        assert_eq!(tokens(9_960), "10k");
        assert_eq!(tokens(12_400), "12k");
        assert_eq!(tokens(999_499), "999k");
        assert_eq!(tokens(999_500), "1M");
        assert_eq!(tokens(2_000_000), "2M");
        assert_eq!(tokens(u64::MAX), "18446744073709M");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(0), "0 min");
        assert_eq!(duration(41), "41 min");
        assert_eq!(duration(60), "1 h");
        assert_eq!(duration(72), "1 h 12 min");
        assert_eq!(
            wall_clock(72, 41),
            "1 h 12 min since launch · 41 min working"
        );
    }

    #[test]
    fn role_names_title_case() {
        assert_eq!(title_case("engineer"), "Engineer");
        assert_eq!(title_case(""), "");
    }

    #[test]
    fn listener_label_has_colon() {
        assert_eq!(listener_label("codex", "idle"), "listener: codex · idle");
    }

    #[test]
    fn clock_shows_hours_and_minutes_in_the_zone() {
        let at: Timestamp = "2026-10-25T21:31:40Z".parse().unwrap();
        let denver = TimeZone::get("America/Denver").unwrap();
        assert_eq!(clock(at, &denver), "15:31");
        assert_eq!(clock(at, &TimeZone::UTC), "21:31");
        let morning: Timestamp = "2026-10-25T07:05:00Z".parse().unwrap();
        assert_eq!(clock(morning, &TimeZone::UTC), "07:05");
        // 21:31 UTC plus 14 hours wraps to the next day.
        let east = TimeZone::fixed(jiff::tz::offset(14));
        assert_eq!(clock(at, &east), "11:31");
    }

    #[test]
    fn large_counts_cap_at_99() {
        assert_eq!(count_label(0), "0");
        assert_eq!(count_label(99), "99");
        assert_eq!(count_label(100), "99+");
        assert_eq!(count_label(usize::MAX), "99+");
        assert_eq!(counted_capped(1, "needs you", "need you"), "1 needs you");
        assert_eq!(counted_capped(2, "alert", "alerts"), "2 alerts");
        assert_eq!(counted_capped(256, "needs you", "need you"), "99+ need you");
    }

    #[test]
    fn counts_pick_singular_or_plural() {
        assert_eq!(counted(1, "alert", "alerts"), "1 alert");
        assert_eq!(counted(2, "alert", "alerts"), "2 alerts");
        assert_eq!(
            workers_today(11.2, 20., 3, 4),
            "$11.20 of $20.00 · 3 of 4 running"
        );
    }
}
