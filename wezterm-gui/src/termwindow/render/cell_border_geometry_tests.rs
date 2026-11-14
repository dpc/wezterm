//! Pixel regressions adapted from the MIT-licensed pinned Ghostty reference.
//! See the parent module for the retained copyright and license notice.
use super::*;

fn stroke(style: BorderStyle) -> BorderStroke {
    BorderStroke {
        width: 32,
        color: ColorAttribute::Default,
        centered: false,
        above_text: false,
        priority: 0,
        style,
    }
}

fn set(mask: u16, style: BorderStyle, joints: u8) -> CellBorders {
    let mut borders = CellBorders::default();
    borders.set(mask, Some(stroke(style)));
    borders.joints = joints;
    borders
}

fn raster(rects: &[BorderRect], outside: bool) -> [[u8; 16]; 32] {
    let mut pixels = [[0u8; 16]; 32];
    for rect in rects {
        if rect.outside.is_some() != outside {
            continue;
        }
        let [x0, y0, x1, y1] = rect.bounds;
        for y in y0.max(0)..y1.min(32) {
            for x in x0.max(0)..x1.min(16) {
                let pixel = &mut pixels[y as usize][x as usize];
                *pixel = (u32::from(rect.alpha)
                    + u32::from(*pixel) * (255 - u32::from(rect.alpha)) / 255)
                    as u8;
            }
        }
    }
    pixels
}

fn draw(borders: &CellBorders) -> [[u8; 16]; 32] {
    raster(&geometry(borders, [None; 4], 16, 32), false)
}

#[test]
fn cell_border_double_junctions_keep_the_interior_open() {
    let mut double = stroke(BorderStyle::Double);
    double.width = 48;
    let mut borders = CellBorders::default();
    borders.set(0xf00, Some(double));
    let cross = draw(&borders);
    assert!(cross[15][0] != 0 && cross[17][0] != 0 && cross[16][0] == 0);
    assert!(cross[0][7] != 0 && cross[0][9] != 0 && cross[0][8] == 0);
    assert_eq!(cross[16][8], 0);
    assert_eq!(cross[10][8], 0);
    borders.set(4095, None);
    borders.set(195, Some(double));
    let corner = draw(&borders);
    assert!(corner[0][0] != 0 && corner[2][2] != 0 && corner[1][1] == 0);
    assert_eq!(corner[5][1], 0);
    borders.set(4095, None);
    borders.set(768, Some(double));
    let mut single = stroke(BorderStyle::Solid);
    single.width = 16;
    borders.set(2048, Some(single));
    let tee = draw(&borders);
    assert!(tee[18][8] != 0 && tee[17][8] != 0 && tee[16][8] == 0);
}

#[test]
fn cell_border_dashes_dots_and_half_edges() {
    let dashed = draw(&set(768, BorderStyle::Dashed, 0));
    assert!(dashed[15][0] == 0 && dashed[15][2] != 0 && dashed[15][5] != 0 && dashed[15][7] == 0);
    assert!(dashed[15][10] != 0 && dashed[15][15] == 0);
    let dotted = draw(&set(768, BorderStyle::Dotted, 0));
    assert!(dotted[15][3] != 0 && dotted[16][4] != 0 && dotted[15][2] == 0 && dotted[15][5] == 0);
    let half = draw(&set(1, BorderStyle::Solid, 0));
    assert!(half[0][0] != 0 && half[0][7] != 0 && half[0][8] == 0);
}

#[test]
fn cell_border_round_corners_forks_and_fillets() {
    let corner = draw(&set(512 | 2048, BorderStyle::Solid, 32));
    assert!(corner[16][8] == 0 && corner[15][9] == 0);
    assert!(corner[15][15] != 0 && corner[31][8] != 0);
    let square = draw(&set(512 | 2048, BorderStyle::Solid, 0));
    assert!(square[16][8] != 0);
    let fork = draw(&set(768 | 2048, BorderStyle::Solid, 32));
    assert!(fork[15].iter().all(|p| *p != 0));
    assert!(fork[18][7] == 0 && fork[31][7] != 0);
    let fillet = draw(&set(3840, BorderStyle::Solid, 240));
    assert!(fillet[15].iter().all(|p| *p != 0));
    assert!(fillet.iter().all(|row| row[7] != 0));
    let tl = draw(&set(195, BorderStyle::Solid, 1));
    assert!(tl[0][0] == 0 && tl[0][15] != 0 && tl[31][0] != 0);
}

#[test]
fn cell_border_radius_continues_only_identical_strokes() {
    let mut borders = set(195, BorderStyle::Solid, 1);
    let default = draw(&borders);
    borders.radius = Some(128);
    assert_eq!(default, draw(&borders));
    borders.radius = Some(256);
    let full = draw(&borders);
    assert!(full[10][0] < 128 && full[4][8] < 128 && full[16][0] >= 128 && full[31][0] >= 128);
    assert!(full[0][12] >= 128 && full[4][5] >= 128);
    borders.segments[1].as_mut().unwrap().color = ColorAttribute::PaletteIndex(1);
    let clamped = draw(&borders);
    assert!(clamped[10][0] != 0 && clamped[0][12] != 0 && clamped[1][15] != 0);
    assert_eq!(clamped[2][0], 0);
}

#[test]
fn cell_border_outside_fill_and_arc_antialiasing() {
    let mut borders = set(195, BorderStyle::Solid, 1);
    borders.outside = Some(ColorAttribute::Default);
    let rects = geometry(&borders, [None; 4], 16, 32);
    let fill = raster(&rects, true);
    assert_eq!(fill[0][0], 255);
    assert_eq!(fill[4][4], 0);
    assert_eq!(fill[0][10], 0);
    for rect in rects.iter().filter(|r| r.outside.is_some()) {
        let [x0, y0, x1, y1] = rect.bounds;
        assert!(x0 >= 0 && y0 >= 0 && x1 <= 16 && y1 <= 32);
    }
    let partial: Vec<_> = rects.iter().filter(|r| r.alpha < 255).collect();
    assert!(partial.len() > 4);
    assert!(partial
        .iter()
        .all(|r| r.alpha > 0 && r.bounds[2] - r.bounds[0] == 1 && r.bounds[3] - r.bounds[1] == 1));
    for style in [
        BorderStyle::Double,
        BorderStyle::Dashed,
        BorderStyle::Dotted,
    ] {
        let styled = geometry(&set(195, style, 1), [None; 4], 16, 32);
        assert!(styled.iter().any(|r| r.alpha < 255));
    }
}

#[test]
fn cell_border_shared_edge_priority_width_tie_and_layer() {
    let mut left = set(12, BorderStyle::Solid, 0);
    let mut right = set(192, BorderStyle::Double, 0);
    for stroke in left
        .segments
        .iter_mut()
        .chain(right.segments.iter_mut())
        .flatten()
    {
        stroke.centered = true;
    }
    assert!(resolve(&left, [None, Some(&right), None, None])[2].is_none());
    assert!(resolve(&right, [Some(&left), None, None, None])[6].is_some());
    left.segments[2].as_mut().unwrap().width = 48;
    left.segments[2].as_mut().unwrap().above_text = true;
    assert!(resolve(&left, [None, Some(&right), None, None])[2].is_some());
    right.segments[6].as_mut().unwrap().priority = 1;
    assert!(resolve(&left, [None, Some(&right), None, None])[2].is_none());
    right.segments[6].as_mut().unwrap().centered = false;
    assert!(resolve(&left, [None, Some(&right), None, None])[2].is_some());
    assert!(resolve(&right, [Some(&left), None, None, None])[6].is_some());
}
