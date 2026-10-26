//! Money, token counts, durations, clock times and other numbers as the
//! views show them.

use agmen_proto::Timestamp;

use crate::local_time::UtcOffset;

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

/// `at` as a 24 hour `15:27` at `offset` from UTC.
#[must_use]
pub(crate) fn clock(at: Timestamp, offset: UtcOffset) -> String {
    let secs = at
        .as_second()
        .saturating_add(i64::from(offset.minutes()) * 60);
    let day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, day % 3600 / 60)
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
    fn clock_shows_the_time_at_the_offset() {
        let at: Timestamp = "2026-10-26T21:27:40Z".parse().unwrap();
        let mdt = UtcOffset::from_minutes(-6 * 60);
        assert_eq!(clock(at, mdt), "15:27");
        assert_eq!(clock(at, UtcOffset::default()), "21:27");
        // 21:27 UTC plus 14 hours wraps to the next day.
        assert_eq!(clock(at, UtcOffset::from_minutes(14 * 60)), "11:27");
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
    fn counts_pick_singular_or_plural() {
        assert_eq!(counted(1, "alert", "alerts"), "1 alert");
        assert_eq!(counted(2, "alert", "alerts"), "2 alerts");
        assert_eq!(
            workers_today(11.2, 20., 3, 4),
            "$11.20 of $20.00 · 3 of 4 running"
        );
    }
}
