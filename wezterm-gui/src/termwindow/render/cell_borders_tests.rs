//! Pixel composition tests share the GUI's clipping, palette and pass planner.
use super::*;
use crate::quad::SUBLAYER_DRAW_ORDER;
use std::sync::Arc;
use termwiz::cell::border::{BorderStroke, BorderStyle, CellBorders};
use termwiz::cell::{Cell, CellAttributes};
use termwiz::surface::SEQ_ZERO;
use wezterm_term::color::{ColorAttribute, SrgbaTuple};

#[derive(Clone)]
struct Paint {
    layer: usize,
    bounds: RectF,
    color: LinearRgba,
}

fn palette() -> ColorPalette {
    let mut p = ColorPalette::default();
    p.colors.0[1] = SrgbaTuple(1., 0., 0., 1.);
    p.colors.0[2] = SrgbaTuple(0., 1., 0., 1.);
    p.colors.0[3] = SrgbaTuple(0., 0., 1., 1.);
    p
}

fn border(mask: u16, palette: u8, above: bool) -> CellBorders {
    let mut b = CellBorders::default();
    b.set(
        mask,
        Some(BorderStroke {
            width: 64,
            color: ColorAttribute::PaletteIndex(palette),
            centered: true,
            above_text: above,
            priority: 0,
            style: BorderStyle::Solid,
        }),
    );
    b
}

fn line(b: CellBorders) -> Line {
    let mut attrs = CellAttributes::default();
    attrs.set_borders(Some(Arc::new(b)));
    Line::from_cells(vec![Cell::blank_with_attrs(attrs)], SEQ_ZERO)
}

fn scene(lines: &mut [Line], palette: &ColorPalette, origin: (f32, f32)) -> Vec<Paint> {
    let refs: Vec<_> = lines.iter_mut().collect();
    let mut paints = Vec::new();
    paint_cell_borders(
        &refs,
        1,
        (16, 32),
        origin,
        palette,
        |layer, bounds, color| {
            paints.push(Paint {
                layer,
                bounds,
                color,
            });
            Ok(())
        },
    )
    .unwrap();
    paints
}

fn solid(layer: usize, bounds: RectF, rgb: (f32, f32, f32)) -> Paint {
    Paint {
        layer,
        bounds,
        color: LinearRgba::with_components(rgb.0, rgb.1, rgb.2, 1.),
    }
}

fn sample(paints: &[Paint], x: f32, y: f32) -> [f32; 3] {
    let mut pixel = [0.; 3];
    for layer in SUBLAYER_DRAW_ORDER {
        for paint in paints {
            if paint.layer != layer || !paint.bounds.contains(euclid::point2(x + 0.5, y + 0.5)) {
                continue;
            }
            let (r, g, b, a) = paint.color.tuple();
            for (dst, src) in pixel.iter_mut().zip([r, g, b].iter()) {
                *dst = src * a + *dst * (1. - a);
            }
        }
    }
    pixel
}

#[test]
fn cell_border_cross_row_paint_survives_backgrounds_and_pane_clipping() {
    let mut lines = [line(border(48 | 192, 1, false)), Line::new(SEQ_ZERO)];
    let mut paints = scene(&mut lines, &palette(), (4., 5.));
    // Row backgrounds can be appended after cached rows or borders: every
    // background must still draw before the cross-row centered border.
    paints.push(solid(0, euclid::rect(4., 37., 16., 32.), (0., 0., 1.)));
    assert_eq!(sample(&paints, 12., 37.), [1., 0., 0.]);
    assert_eq!(sample(&paints, 12., 40.), [0., 0., 1.]);
    assert_eq!(
        sample(&paints, 3., 20.),
        [0., 0., 0.],
        "no paint into padding/adjacent pane"
    );
    assert_eq!(sample(&paints, 4., 20.), [1., 0., 0.]);
    assert_eq!(sample(&paints, 20., 37.), [0., 0., 0.]);
}

#[test]
fn cell_border_layers_compose_with_text_and_filled_or_outline_cursor() {
    let mut borders = border(768, 1, false);
    borders.set(3072, border(3072, 2, true).segments[10]);
    let mut lines = [line(borders)];
    let mut paints = scene(&mut lines, &palette(), (0., 0.));
    let pixel = euclid::rect(7., 15., 1., 1.);
    paints.push(solid(1, pixel, (1., 1., 1.))); // text
    assert_eq!(sample(&paints, 7., 15.), [0., 1., 0.]);
    paints.push(solid(
        cursor_layer(CursorShape::Default, false),
        pixel,
        (0., 0., 1.),
    ));
    assert_eq!(
        sample(&paints, 7., 15.),
        [0., 1., 0.],
        "above-text border covers filled cursor"
    );
    for shape in [
        CursorShape::SteadyBlock,
        CursorShape::SteadyBar,
        CursorShape::SteadyUnderline,
    ] {
        let mut with_cursor = paints.clone();
        with_cursor.push(solid(cursor_layer(shape, false), pixel, (0., 0., 1.)));
        assert_eq!(
            sample(&with_cursor, 7., 15.),
            [0., 0., 1.],
            "non-filled cursor stays on top"
        );
    }
}

#[test]
fn cell_border_palette_outside_fill_and_border_only_mutation_refresh() {
    let mut borders = border(195, 1, false);
    borders.joints = 1;
    borders.radius = Some(256);
    borders.outside = Some(ColorAttribute::Default);
    let mut lines = [line(borders)];
    let mut palette = palette();
    palette.background = SrgbaTuple(0., 1., 0., 1.);
    let first = scene(&mut lines, &palette, (0., 0.));
    assert_eq!(sample(&first, 0., 0.), [0., 1., 0.]);
    assert_eq!(sample(&first, 0., 30.), [1., 0., 0.]);
    palette.background = SrgbaTuple(0., 0., 1., 1.);
    palette.colors.0[1] = SrgbaTuple(0., 1., 0., 1.);
    let recolored = scene(&mut lines, &palette, (0., 0.));
    assert_eq!(sample(&recolored, 0., 0.), [0., 0., 1.]);
    assert_eq!(sample(&recolored, 0., 30.), [0., 1., 0.]);
    lines[0].cells_mut_for_attr_changes_only()[0]
        .attrs_mut()
        .set_borders(None);
    assert!(scene(&mut lines, &palette, (0., 0.)).is_empty());
}
