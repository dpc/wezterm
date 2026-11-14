//! Native mux framing must preserve both physical halves of a wide glyph.
use super::*;
use termwiz::cell::border::{BorderStroke, BorderStyle, CellBorders};
use termwiz::cell::{Cell, CellAttributes};
use termwiz::color::ColorAttribute;

#[test]
fn cell_borders_survive_native_mux_line_encoding() {
    let mut a = CellBorders::default();
    a.set(
        195,
        Some(BorderStroke {
            width: 64,
            color: ColorAttribute::PaletteIndex(7),
            centered: true,
            above_text: true,
            priority: 15,
            style: BorderStyle::Double,
        }),
    );
    a.joints = 1;
    a.outside = Some(ColorAttribute::Default);
    a.radius = Some(256);
    let mut b = a.clone();
    b.set(4095, None);
    b.joints = 4;
    let mut left = CellAttributes::default();
    left.set_borders(Some(Arc::new(a.clone())));
    let mut right = CellAttributes::default();
    right.set_borders(Some(Arc::new(b.clone())));
    let mut line = Line::from_cells(
        vec![
            Cell::new_grapheme_with_width("界", 2, left),
            Cell::blank_with_attrs(right),
        ],
        1,
    );
    line.compress_for_scrollback();
    let pdu = Pdu::GetLinesResponse(GetLinesResponse {
        pane_id: 1,
        lines: vec![(0, line)].into(),
    });
    let mut encoded = Vec::new();
    pdu.encode(&mut encoded, 7).unwrap();
    let decoded = Pdu::try_read_and_decode(&mut Cursor::new(encoded), &mut Vec::new())
        .unwrap()
        .unwrap();
    assert_eq!(decoded.serial, 7);
    let Pdu::GetLinesResponse(response) = decoded.pdu else {
        panic!("wrong response")
    };
    let (lines, images) = response.lines.extract_data();
    assert!(images.is_empty());
    assert_eq!(lines[0].1.cell_borders(0).as_deref(), Some(&a));
    assert_eq!(lines[0].1.cell_borders(1).as_deref(), Some(&b));
    assert_eq!(lines[0].1.as_str(), "界");
}
