//! Pane-wide cell-border composition, independent of cached glyph rows.
use super::cell_border_geometry::geometry;
use crate::quad::TripleLayerQuadAllocator;
use termwiz::surface::CursorShape;
use wezterm_term::color::ColorPalette;
use wezterm_term::Line;
use window::color::LinearRgba;
use window::RectF;

#[cfg(test)]
#[path = "cell_borders_tests.rs"]
mod tests;

/// Place a filled cursor under above-text borders, all other cursor marks above.
pub(super) fn cursor_layer(shape: CursorShape, password_input: bool) -> usize {
    // compute_cell_fg_bg maps a focused filled block to Default; explicit block
    // shapes at this point are the unfocused outline.
    match shape {
        CursorShape::Default if !password_input => 1,
        _ => 2,
    }
}

/// Resolve and compose a pane's border rectangles using the renderer's layers.
fn paint_cell_borders(
    lines: &[&mut Line],
    columns: usize,
    (width, height): (i32, i32),
    (left, top): (f32, f32),
    palette: &ColorPalette,
    mut paint: impl FnMut(usize, RectF, LinearRgba) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if !lines.iter().any(|line| line.has_cell_borders()) {
        return Ok(());
    }
    let cells: Vec<_> = lines
        .iter()
        .map(|line| {
            (0..columns)
                .map(|x| line.cell_borders(x))
                .collect::<Vec<_>>()
        })
        .collect();
    let viewport = euclid::rect(
        left,
        top,
        columns as f32 * width as f32,
        lines.len() as f32 * height as f32,
    );
    for (y, row) in cells.iter().enumerate() {
        for (x, cell) in row.iter().enumerate() {
            let Some(cell) = cell.as_deref() else {
                continue;
            };
            let neighbors = [
                x.checked_sub(1).and_then(|x| row[x].as_deref()),
                row.get(x + 1).and_then(Option::as_deref),
                y.checked_sub(1).and_then(|y| cells[y][x].as_deref()),
                cells.get(y + 1).and_then(|row| row[x].as_deref()),
            ];
            for rect in geometry(cell, neighbors, width, height) {
                let [x0, y0, x1, y1] = rect.bounds;
                let bounds = euclid::rect(
                    left + (x as i32 * width + x0) as f32,
                    top + (y as i32 * height + y0) as f32,
                    (x1 - x0) as f32,
                    (y1 - y0) as f32,
                );
                let Some(bounds) = bounds.intersection(&viewport) else {
                    continue;
                };
                let (layer, color) = if let Some(outside) = rect.outside {
                    (3, palette.resolve_bg(outside))
                } else if let Some(stroke) = rect.stroke {
                    (
                        if stroke.above_text { 5 } else { 4 },
                        palette.resolve_fg(stroke.color),
                    )
                } else {
                    continue;
                };
                paint(
                    layer,
                    bounds,
                    color.to_linear().mul_alpha(f32::from(rect.alpha) / 255.),
                )?;
            }
        }
    }
    Ok(())
}

impl crate::TermWindow {
    /// Draw physical-cell borders after collecting the entire visible pane.
    pub(super) fn render_cell_borders(
        &self,
        layers: &mut TripleLayerQuadAllocator,
        lines: &[&mut Line],
        columns: usize,
        origin: (f32, f32),
        palette: &ColorPalette,
    ) -> anyhow::Result<()> {
        paint_cell_borders(
            lines,
            columns,
            (
                self.render_metrics.cell_size.width as i32,
                self.render_metrics.cell_size.height as i32,
            ),
            origin,
            palette,
            |layer, bounds, color| {
                self.filled_rectangle(layers, layer, bounds, color)?;
                Ok(())
            },
        )
    }
}
