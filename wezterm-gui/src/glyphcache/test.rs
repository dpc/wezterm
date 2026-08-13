use super::*;

#[test]
fn centers_using_rendered_cell_width() {
    // 20px span - 14px bitmap leaves 3px on each side. The shaping
    // offset consumes 1px, so the replacement bearing supplies 2px.
    assert_eq!(
        centered_color_glyph_bearing_x(true, 1, true, 2, 10, 14, PixelLength::new(1.0)),
        Some(PixelLength::new(2.0))
    );
}

#[test]
fn does_not_center_ineligible_glyphs() {
    let cell_width = 10;
    let x_offset = PixelLength::new(1.0);

    for (case, actual) in [
        (
            "disabled",
            centered_color_glyph_bearing_x(false, 1, true, 2, cell_width, 14, x_offset),
        ),
        (
            "primary font",
            centered_color_glyph_bearing_x(true, 0, true, 2, cell_width, 14, x_offset),
        ),
        (
            "monochrome",
            centered_color_glyph_bearing_x(true, 1, false, 2, cell_width, 14, x_offset),
        ),
        (
            "one cell",
            centered_color_glyph_bearing_x(true, 1, true, 1, cell_width, 8, x_offset),
        ),
    ] {
        assert_eq!(actual, None, "{case}");
    }
}

#[test]
fn does_not_center_overflowing_glyph() {
    assert_eq!(
        centered_color_glyph_bearing_x(true, 1, true, 2, 10, 21, PixelLength::new(1.0)),
        None
    );
}
