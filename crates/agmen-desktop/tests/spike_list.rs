//! Spike S8's list check. It opens a real window and must run on a release
//! build with the window visible, so it is ignored by default:
//! `cargo nextest run -p agmen-desktop --release --run-ignored only list_measures_10k_rows`.

use std::process::Command;

#[test]
#[ignore = "opens a window; run with --release --run-ignored only"]
fn list_measures_10k_rows() {
    let out = Command::new(env!("CARGO_BIN_EXE_agmen-desktop"))
        .args(["--spike", "list", "--autoscroll"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = String::from_utf8(out.stdout).unwrap();
    eprintln!("{line}");
    let field = |name: &str| -> f64 {
        line.split_whitespace()
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("no {name} in {line}"))
            .parse()
            .unwrap()
    };
    assert!(
        (field("last_rendered_row") - 9_999.0).abs() < f64::EPSILON,
        "the last row was never rendered"
    );
    assert!(
        field("max_jump_rows") <= 10.0,
        "the list jumped instead of scrolling"
    );
    // macOS paces a window that is not in front at about 35 ms a frame
    // whatever the row cost (ADR-0010), so the spike times only frames shown
    // while the window was active, and needs most of the run in front.
    let (timed, skipped) = (field("frames"), field("inactive_frames"));
    eprintln!("timed {timed} frames, skipped {skipped} while the window was not in front");
    assert!(
        timed >= 2_000.0,
        "too few frames in front to measure; keep the window in front"
    );
    assert!(field("p99_ms") < 16.7, "p99 over one 60 Hz frame: {line}");
}
