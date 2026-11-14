//! Wire-level border regressions exercise the real parser and screen lifecycle.
use super::*;
use std::assert_eq;
use wezterm_cell::border::{BorderStyle, CellBorders};
use wezterm_cell::color::ColorAttribute;

fn command(term: &mut TestTerm, body: &str) {
    term.print(format!("\x1bP1+bcell-border;{body}\x1b\\"));
}

fn borders(term: &TestTerm, x: usize, y: usize) -> CellBorders {
    term.screen().visible_lines()[y]
        .cell_borders(x)
        .map(|b| (*b).clone())
        .unwrap_or_default()
}

fn pen(term: &mut TestTerm) {
    command(term, "pen;4095;16;p2;1;0;priority=7;style=double");
}

#[test]
fn border_pen_sgr_rectangles_and_wide_physical_cells() {
    let mut term = TestTerm::new(4, 8, 8);
    pen(&mut term);
    command(&mut term, "pen-round;255;outside=p4;radius=192");
    term.print("A界");
    let a = borders(&term, 0, 0);
    assert_eq!(a.segments[0].unwrap().style, BorderStyle::Double);
    assert_eq!(a.radius, Some(192));
    assert_eq!(a.outside, Some(ColorAttribute::PaletteIndex(4)));
    assert!(borders(&term, 1, 0).segments[2].is_none());
    assert!(borders(&term, 2, 0).segments[6].is_none());
    assert_eq!(borders(&term, 1, 0).joints, 249);
    assert_eq!(borders(&term, 2, 0).joints, 246);
    term.print("\x1b[31mB\x1b[mC");
    assert_eq!(borders(&term, 3, 0), a);
    assert!(borders(&term, 4, 0).is_empty());
    term.cup(2, 0);
    command(&mut term, "clear;4095;1;1");
    assert!(borders(&term, 2, 0).segments.iter().all(Option::is_none));
    assert!(borders(&term, 1, 0).segments[0].is_some());
    command(&mut term, "set;1;32;ff0000;0;1;65535;65535");
    assert!(borders(&term, 1, 0).segments[0].unwrap().width == 16);
    assert!(borders(&term, 7, 3).segments[0].unwrap().above_text);
    term.print("Z");
    assert!(
        borders(&term, 1, 0).is_empty(),
        "overwritten wide counterpart takes empty pen"
    );
    assert!(borders(&term, 2, 0).is_empty());
}

#[test]
fn border_atomic_validation_fragmentation_cancellation_and_limits() {
    let mut term = TestTerm::new(2, 20, 0);
    for b in b"\x1bP1+bcell-border;pen;3;16;fg;0;1;style=double;style=future\x1b\\" {
        term.print([*b]);
    }
    term.print("A");
    let expected = borders(&term, 0, 0);
    assert_eq!(expected.segments[0].unwrap().style, BorderStyle::Double);
    for body in [
        "pen;1;0;fg;0;1",
        "pen;4096;16;fg;0;1",
        "pen;+1;16;fg;0;1",
        "pen;1;16;p256;0;1",
        "pen;1;16;12345;0;1",
        "pen;1;16;fg;2;1",
        "pen;1;16;fg;0;1;style=SOLID",
        "pen;1;16;fg;0;1;bad",
        "pen-clear;4095;extra",
        "set;4095;16;fg;0;0;0;2",
        "pen-round;256",
        "pen-round;1;outside=FF0000",
    ] {
        command(&mut term, body);
        term.print("B");
    }
    // Aborted DCS is not committed by CAN, SUB, an unrelated ESC, or C1.
    for suffix in [b"\x18".as_slice(), b"\x1a", b"\x1b[0A", b"\x85"] {
        term.print(b"\x1bP1+bcell-border;pen-clear;4095");
        term.print(suffix);
    }
    term.print(b"\x1bP1+bcell-border;pen-clear;40\x7f95\x1b\\");
    term.print(format!(
        "\x1bP1+bcell-border;pen-clear;{}4095\x1b\\",
        "0".repeat(256)
    ));
    term.print("Q");
    for line in term.screen().visible_lines() {
        for cell in line.visible_cells().filter(|c| c.str() != " ") {
            assert_eq!(cell.attrs().borders().map(|b| &**b), Some(&expected));
        }
    }
    term.print(b"\x901+bcell-border;pen-clear;4095\x9c");
    term.print("Z");
    let cursor = term.cursor_pos();
    assert!(borders(&term, cursor.x - 1, cursor.y as usize).is_empty());
}

#[test]
fn border_erase_insert_delete_and_selective_protection() {
    let mut term = TestTerm::new(3, 8, 4);
    pen(&mut term);
    term.print("AB");
    let expected = borders(&term, 0, 0);
    term.cup(4, 0);
    term.print("\x1b[2X");
    assert_eq!(borders(&term, 4, 0), expected);
    assert_eq!(borders(&term, 5, 0), expected);
    term.cup(0, 0);
    term.print("\x1b[2@");
    assert_eq!(borders(&term, 0, 0), expected);
    term.print("\x1b[2P");
    assert_eq!(borders(&term, 7, 0), expected);
    term.print("\x1b[m\x1b[2K");
    assert!(!term.screen().visible_lines()[0].has_cell_borders());
    pen(&mut term);
    term.print("\x1b[1\"qP\x1b[0\"qU\x1b[m\x1b[?2K");
    assert_eq!(borders(&term, 0, 0), expected);
    assert!(borders(&term, 1, 0).is_empty());
    assert_eq!(term.screen().visible_lines()[0].as_str().trim_end(), "P");
    term.print("\x1b[2J");
    assert!(!term.screen().visible_lines()[0].has_cell_borders());
}

#[test]
fn border_save_restore_resets_screens_and_scroll() {
    let mut term = TestTerm::new(2, 6, 10);
    pen(&mut term);
    term.print("A\x1b7\x1b[mB\x1b8C");
    let expected = borders(&term, 0, 0);
    assert_eq!(borders(&term, 1, 0), expected);
    term.print("\x1b[?1049hX");
    assert_eq!(borders(&term, 0, 0), expected);
    term.print("\x1b[m\x1b[?1049lD");
    assert_eq!(borders(&term, 2, 0), expected);
    term.print("\r\n\r\n");
    assert!(term.screen().all_lines()[0].has_cell_borders());
    assert_eq!(borders(&term, 5, 1), expected, "new blank line takes pen");
    term.print("\x1b[!pZ");
    assert!(borders(&term, 0, 1).is_empty());
    term.print("\x1bc");
    assert!(term
        .screen()
        .all_lines()
        .iter()
        .all(|l| !l.has_cell_borders()));
    pen(&mut term);
    term.print("A\x1b[?3h");
    assert!(term
        .screen()
        .visible_lines()
        .iter()
        .all(|l| !l.has_cell_borders()));
    term.print("B");
    assert_eq!(
        borders(&term, 0, 0),
        expected,
        "DECCOLM clears cells, not pen"
    );
}

#[test]
fn border_reflow_retains_blank_and_wide_continuation_attributes() {
    let mut term = TestTerm::new(3, 8, 10);
    term.print("界");
    term.cup(1, 0);
    command(&mut term, "set;8;16;p5;1;1;1;1");
    term.cup(6, 0);
    command(&mut term, "set;1024;16;fg;0;0;1;1");
    let second = borders(&term, 1, 0);
    let blank = borders(&term, 6, 0);
    term.resize(TerminalSize {
        rows: 3,
        cols: 4,
        pixel_width: 32,
        pixel_height: 48,
        dpi: 0,
    });
    assert_eq!(borders(&term, 1, 0), second);
    assert_eq!(borders(&term, 2, 1), blank);
    term.resize(TerminalSize {
        rows: 3,
        cols: 8,
        pixel_width: 64,
        pixel_height: 48,
        dpi: 0,
    });
    assert_eq!(borders(&term, 1, 0), second);
    assert_eq!(borders(&term, 6, 0), blank);
    assert_eq!(term.screen().visible_lines()[0].as_str().trim_end(), "界");
}

#[test]
fn border_tmux_native_redraw_matches_direct_application() {
    let root = std::env::var_os("WEZTERM_CELL_BORDER_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/data/cell-borders"
            ))
        });
    for scenario in [
        "state",
        "movement",
        "extensions",
        "wide",
        "screens",
        "reflow",
        "offsets",
        "alignment",
        "horizontal_tab",
        "palette",
        "pending_wrap",
        "save_1048",
        "vs16",
        "one_column",
        "framing",
    ] {
        let metadata: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join(scenario).join("metadata.json")).unwrap(),
        )
        .unwrap();
        let panes = metadata["geometry"].as_array().unwrap().last().unwrap()["panes"]
            .as_array()
            .unwrap();
        let pane = panes.last().unwrap();
        let left = pane["pane_left"].as_u64().unwrap() as usize;
        let top = pane["pane_top"].as_u64().unwrap() as usize;
        let width = pane["pane_width"].as_u64().unwrap() as usize;
        let height = pane["pane_height"].as_u64().unwrap() as usize;
        let mut direct = TestTerm::new(height, width, 100);
        let mut outer = TestTerm::new(24, 80, 100);
        if scenario == "vs16" {
            // Match tmux's emoji-variation width policy; WezTerm's historical
            // Unicode 9 default intentionally does not widen VS16 sequences.
            direct.print("\x1b]1337;UnicodeVersion=14\x1b\\");
            outer.print("\x1b]1337;UnicodeVersion=14\x1b\\");
        }
        direct.print(std::fs::read(root.join(scenario).join("apps.bin")).unwrap());
        outer.print(std::fs::read(root.join(scenario).join("outer.bin")).unwrap());
        let mut differences = Vec::new();
        for y in 0..24 {
            for x in 0..80 {
                let mut a = if (left..left + width).contains(&x) && (top..top + height).contains(&y)
                {
                    borders(&direct, x - left, y - top)
                } else {
                    CellBorders::default()
                };
                let mut b = borders(&outer, x, y);
                if scenario == "palette" {
                    // tmux realizes pane-local OSC colour overrides as RGB on
                    // its outer terminal, without changing other panes' themes.
                    for (set, palette) in [(&mut a, direct.palette()), (&mut b, outer.palette())] {
                        for stroke in set.segments.iter_mut().flatten() {
                            stroke.color = ColorAttribute::TrueColorWithDefaultFallback(
                                palette.resolve_fg(stroke.color),
                            );
                        }
                        set.outside = set.outside.map(|c| {
                            ColorAttribute::TrueColorWithDefaultFallback(palette.resolve_bg(c))
                        });
                    }
                }
                if a != b {
                    differences.push((x, y, a, b));
                }
            }
        }
        assert!(differences.is_empty(), "{}: {:#?}", scenario, differences);
    }
}

/// A private write-only reply collector; never touches a host terminal.
struct ReplySink(std::sync::mpsc::Sender<Vec<u8>>);

impl std::io::Write for ReplySink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(bytes.to_vec());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn border_query_precedes_da1_and_ignores_invalid_queries() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut term = Terminal::new(
        TerminalSize {
            rows: 2,
            cols: 8,
            pixel_width: 64,
            pixel_height: 32,
            dpi: 0,
        },
        Arc::new(TestTermConfig { scrollback: 0 }),
        "WezTerm",
        "test",
        Box::new(ReplySink(tx)),
    );
    term.advance_bytes(b"\x1bP1+bcell-border;q;extra\x1b\\\x1bP2+bcell-border;q\x1b\\\x1bP1+bcell-border;q\x1b\\\x1b[c");
    let mut replies = Vec::new();
    while !replies.ends_with(b"c") {
        replies.extend(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap());
    }
    let prefix = b"\x1bP1+bcell-border;ok;1;pen,rect,half,center,palette,style,round,priority,outside,radius\x1b\\";
    assert!(replies.starts_with(prefix));
    assert!(replies[prefix.len()..].starts_with(b"\x1b[?"));
    assert_eq!(
        replies
            .windows(b"cell-border".len())
            .filter(|w| *w == b"cell-border")
            .count(),
        1
    );
}

#[test]
fn border_bounded_scroll_materializes_new_blank_cells() {
    for (operation, cursor_y, blank_y) in [
        ("\x1b[S", 0, 3),
        ("\x1b[T", 0, 0),
        ("\x1b[L", 1, 1),
        ("\x1b[M", 1, 3),
        ("\n", 3, 3),
        ("\x1bM", 0, 0),
    ] {
        let mut term = TestTerm::new(4, 8, 0);
        term.set_mode("?69", true);
        term.set_left_and_right_margins(2, 5);
        term.cup(2, cursor_y);
        pen(&mut term);
        term.print(operation);
        for x in 0..8 {
            assert_eq!(
                !borders(&term, x, blank_y).is_empty(),
                (2..6).contains(&x),
                "operation {:?}, column {}",
                operation,
                x
            );
        }
    }
}

#[test]
fn border_wide_half_insert_delete_blank_with_current_pen() {
    for operation in ["\x1b[P", "\x1b[@"] {
        let mut term = TestTerm::new(2, 8, 0);
        pen(&mut term);
        term.print("界");
        term.cup(1, 0);
        command(&mut term, "set;1;48;p5;0;0;1;1");
        term.print("\x1b[m");
        term.print(operation);
        assert_eq!(term.screen().visible_lines()[0].as_str().trim_end(), "");
        for x in 0..3 {
            assert!(borders(&term, x, 0).is_empty());
        }
    }
    let mut term = TestTerm::new(2, 8, 0);
    pen(&mut term);
    term.print("界\x1b[m\x1b[H\x1b[P");
    assert!(borders(&term, 0, 0).is_empty());
}
