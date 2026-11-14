//! Atomic parsing and application of the draft cell-border DCS.
use super::TerminalState;
use std::sync::Arc;
use wezterm_cell::border::{BorderStroke, BorderStyle, CellBorders};
use wezterm_cell::color::{ColorAttribute, SrgbaTuple};

/// A fully validated operation; construction has no terminal side effects.
enum Command {
    /// Ask for supported features.
    Query,
    /// Change selected pen or rectangle segments.
    Segments {
        mask: u16,
        stroke: Option<BorderStroke>,
        rect: Option<(usize, usize)>,
    },
    /// Change selected pen or rectangle joints and optional joint attributes.
    Joints {
        mask: u8,
        round: bool,
        outside: Option<ColorAttribute>,
        radius: Option<u16>,
        rect: Option<(usize, usize)>,
    },
}

fn decimal(s: &str, min: u16, max: u16) -> Option<u16> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = s.bytes().try_fold(0u16, |n, b| {
        n.checked_mul(10)?.checked_add((b - b'0') as u16)
    })?;
    (min..=max).contains(&value).then_some(value)
}

fn color(s: &str, outside: bool) -> Option<ColorAttribute> {
    if (!outside && s == "fg") || (outside && s == "default") {
        return Some(ColorAttribute::Default);
    }
    if let Some(index) = s.strip_prefix('p') {
        return Some(ColorAttribute::PaletteIndex(decimal(index, 0, 255)? as u8));
    }
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rgb = u32::from_str_radix(s, 16).ok()?;
    Some(ColorAttribute::TrueColorWithDefaultFallback(SrgbaTuple(
        ((rgb >> 16) & 255) as f32 / 255.,
        ((rgb >> 8) & 255) as f32 / 255.,
        (rgb & 255) as f32 / 255.,
        1.,
    )))
}

impl Command {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 256 {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let mut fields = text.split(';');
        if fields.next()? != "cell-border" {
            return None;
        }
        let op = fields.next()?;
        let mut command = match op {
            "q" => Self::Query,
            "pen" | "set" | "pen-clear" | "clear" => {
                let mask = decimal(fields.next()?, 1, 4095)?;
                let stroke = if op == "pen" || op == "set" {
                    Some(BorderStroke {
                        width: decimal(fields.next()?, 1, 64)? as u8,
                        color: color(fields.next()?, false)?,
                        centered: decimal(fields.next()?, 0, 1)? != 0,
                        above_text: decimal(fields.next()?, 0, 1)? != 0,
                        priority: 0,
                        style: BorderStyle::Solid,
                    })
                } else {
                    None
                };
                let rect = if op == "set" || op == "clear" {
                    Some((
                        decimal(fields.next()?, 1, 65535)? as usize,
                        decimal(fields.next()?, 1, 65535)? as usize,
                    ))
                } else {
                    None
                };
                Self::Segments { mask, stroke, rect }
            }
            "pen-round" | "pen-square" | "round" | "square" => {
                let mask = decimal(fields.next()?, 1, 255)? as u8;
                let rect = if op == "round" || op == "square" {
                    Some((
                        decimal(fields.next()?, 1, 65535)? as usize,
                        decimal(fields.next()?, 1, 65535)? as usize,
                    ))
                } else {
                    None
                };
                Self::Joints {
                    mask,
                    round: op == "round" || op == "pen-round",
                    outside: None,
                    radius: None,
                    rect,
                }
            }
            _ => return None,
        };
        for field in fields {
            let extensible = matches!(
                command,
                Self::Segments {
                    stroke: Some(_),
                    ..
                } | Self::Joints { .. }
            );
            if !extensible {
                return None;
            }
            let (key, value) = field.split_once('=')?;
            if key.is_empty()
                || value.is_empty()
                || !key.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return None;
            }
            match &mut command {
                Self::Segments {
                    stroke: Some(stroke),
                    ..
                } => match key {
                    "style" => {
                        if let Some(style) = match value {
                            "solid" => Some(BorderStyle::Solid),
                            "double" => Some(BorderStyle::Double),
                            "dashed" => Some(BorderStyle::Dashed),
                            "dotted" => Some(BorderStyle::Dotted),
                            _ => None,
                        } {
                            stroke.style = style;
                        }
                    }
                    "priority" => {
                        if let Some(priority) = decimal(value, 0, 15) {
                            stroke.priority = priority as u8;
                        }
                    }
                    _ => {}
                },
                Self::Joints {
                    outside, radius, ..
                } => match key {
                    "outside" => {
                        if let Some(c) = color(value, true) {
                            *outside = Some(c);
                        }
                    }
                    "radius" => {
                        if let Some(r) = decimal(value, 1, 256) {
                            *radius = Some(r);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        Some(command)
    }

    fn apply(&self, borders: &mut CellBorders) {
        match self {
            Self::Segments { mask, stroke, .. } => borders.set(*mask, *stroke),
            Self::Joints {
                mask,
                round,
                outside,
                radius,
                ..
            } => {
                if *round {
                    borders.joints |= mask;
                } else {
                    borders.joints &= !mask;
                }
                if outside.is_some() {
                    borders.outside = *outside;
                }
                if radius.is_some() {
                    borders.radius = *radius;
                }
            }
            Self::Query => {}
        }
    }
}

impl TerminalState {
    /// Handle DEC protection and selective erase, preserving protected borders.
    pub(super) fn selective_erase(
        &mut self,
        csi: &wezterm_escape_parser::csi::Unspecified,
    ) -> bool {
        use wezterm_escape_parser::csi::CsiParam;
        if csi.parameters_truncated {
            return false;
        }
        let mode = match (csi.control, csi.params.as_slice()) {
            ('q', [CsiParam::Integer(n), CsiParam::P(b'"')]) if (0..=2).contains(n) => {
                self.pen.set_protected(*n == 1);
                return true;
            }
            ('q', [CsiParam::P(b'"')]) => {
                self.pen.set_protected(false);
                return true;
            }
            ('J' | 'K', [CsiParam::P(b'?')]) => 0,
            ('J' | 'K', [CsiParam::P(b'?'), CsiParam::Integer(n)]) if (0..=2).contains(n) => *n,
            _ => return false,
        };
        let seqno = self.seqno;
        let blank = wezterm_cell::Cell::blank_with_attrs(self.pen.clone_sgr_only());
        let cursor_x = self.cursor.x;
        let cursor_y = self.cursor.y as usize;
        let rows = self.screen().physical_rows;
        let cols = self.screen().physical_cols;
        for y in 0..rows {
            if csi.control == 'K' && y != cursor_y {
                continue;
            }
            let line_idx = self.screen().phys_row(y as i64);
            let line = self.screen_mut().line_mut(line_idx);
            for x in 0..cols {
                let selected = match mode {
                    0 => (y, x) >= (cursor_y, cursor_x),
                    1 => (y, x) <= (cursor_y, cursor_x),
                    _ => true,
                };
                if !selected {
                    continue;
                }
                let protected = {
                    let cells = line.cells_mut();
                    cells.get(x).map(|c| c.attrs().protected()).unwrap_or(false)
                };
                if !protected {
                    line.set_cell(x, blank.clone(), seqno);
                }
            }
        }
        true
    }

    /// Apply a bounded, validated version 1 border command without changing SGR.
    pub(super) fn cell_border(&mut self, bytes: &[u8]) {
        let Some(command) = Command::parse(bytes) else {
            return;
        };
        let rect = match command {
            Command::Query => {
                use std::io::Write;
                let _ = self.writer.write_all(
                    b"\x1bP1+bcell-border;ok;1;pen,rect,half,center,palette,style,round,priority,outside,radius\x1b\\"
                );
                let _ = self.writer.flush();
                return;
            }
            Command::Segments { rect, .. } | Command::Joints { rect, .. } => rect,
        };
        if let Some((rows, cols)) = rect {
            let seqno = self.seqno;
            let x = self.cursor.x;
            let y = self.cursor.y;
            let end_y = (y as usize + rows).min(self.screen().physical_rows);
            let end_x = (x + cols).min(self.screen().physical_cols);
            for row in y as usize..end_y {
                let screen = self.screen_mut();
                let phys = screen.phys_row(row as i64);
                let line = screen.line_mut(phys);
                if line.len() < end_x {
                    line.resize(end_x, seqno);
                }
                for cell in &mut line.cells_mut_for_attr_changes_only()[x..end_x] {
                    let mut borders = cell
                        .attrs()
                        .borders()
                        .map(|b| (**b).clone())
                        .unwrap_or_default();
                    command.apply(&mut borders);
                    cell.attrs_mut().set_borders(Some(Arc::new(borders)));
                }
                line.update_last_change_seqno(seqno);
            }
        } else {
            let mut borders = self
                .pen
                .borders()
                .map(|b| (**b).clone())
                .unwrap_or_default();
            command.apply(&mut borders);
            self.pen.set_borders(Some(Arc::new(borders)));
        }
    }
}
