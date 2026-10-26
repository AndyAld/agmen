//! Spike S8's list check. It opens a real window, so it runs only with
//! `AGMEN_GUI=1`: `AGMEN_GUI=1 cargo nextest run -p agmen-desktop --release list_measures_10k_rows`.

use std::process::Command;

#[test]
fn list_measures_10k_rows() {
    if std::env::var_os("AGMEN_GUI").is_none() {
        eprintln!("skipped: set AGMEN_GUI=1 to open a window");
        return;
    }
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
            .unwrap()
            .parse()
            .unwrap()
    };
    assert!((field("frames") - 3000.0).abs() < f64::EPSILON);
    assert!(field("lowest_row_reached") >= 9_990.0);
    // macOS paces a hidden or tiled-away window at about 35 ms a frame
    // whatever the row cost (ADR-0010), so this bounds stalls instead of
    // asserting the 144 Hz p99 a visible window gets.
    assert!(field("max_ms") < 100.0, "a frame stalled: {line}");
}
