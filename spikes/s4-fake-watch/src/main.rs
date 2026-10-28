//! `fake-watch`: spike S4's stand-in for `agmen watch --json`.
//!
//! Prints one DM line addressed to this session every 5 seconds, forever, so
//! the Claude Code mod in `mods/agmen-claude` has a child process to stream.

use std::io::Write as _;
use std::thread;
use std::time::Duration;

use anyhow::Context as _;

const TICK: Duration = Duration::from_secs(5);

fn main() -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    // Ends when the parent kills it or closes the pipe (the write fails).
    for seq in 1..=u64::MAX {
        writeln!(stdout, "{}", dm_line(seq)).context("writing a line to stdout")?;
        stdout.flush().context("flushing stdout")?;
        thread::sleep(TICK);
    }
    Ok(())
}

/// The JSON line for tick `seq`: a DM to this session whose body is `tick <seq>`.
fn dm_line(seq: u64) -> String {
    format!(r#"{{"seq":{seq},"verb":"DM","body":"tick {seq}","to_me":true}}"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_line_for_tick_one() {
        assert_eq!(
            dm_line(1),
            r#"{"seq":1,"verb":"DM","body":"tick 1","to_me":true}"#
        );
    }
}
