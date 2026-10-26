use super::*;

fn text(screen: &TerminalScreen, row: u16) -> String {
    screen.row(row).iter().map(|cell| cell.ch).collect()
}

#[test]
fn put_clips_at_the_edge_and_new_caps_the_size() {
    let mut g = TerminalScreen::new(4, 2);
    assert_eq!(g.put(0, 2, "abc", TermColor::Ansi(1), true), 5);
    assert_eq!(text(&g, 0), "  ab");
    assert_eq!(g.row(0)[2].fg, TermColor::Ansi(1));
    assert!(g.row(0)[2].bold);
    assert!(g.row(2).is_empty());
    g.set_cursor(5, 0);
    assert_eq!(g.cursor(), None);
    let big = TerminalScreen::new(u16::MAX, u16::MAX);
    assert_eq!((big.cols(), big.rows()), (MAX_COLS, MAX_ROWS));
}
