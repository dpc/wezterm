//! Cell-border protocol version 1 data, independent of SGR colours.
use crate::color::ColorAttribute;
#[cfg(feature = "use_serde")]
use serde::{Deserialize, Serialize};

/// The line pattern carried by a stroke.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BorderStyle {
    /// One continuous line.
    #[default]
    Solid,
    /// Two parallel lines separated by a gap.
    Double,
    /// One dash in the middle half of each segment.
    Dashed,
    /// One square dot at the middle of each segment.
    Dotted,
}

/// One stroke, measured in 1/256 of the cell width.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BorderStroke {
    /// Total stroke width, from 1 through 64.
    pub width: u8,
    /// RGB, palette, or terminal default foreground.
    pub color: ColorAttribute,
    /// Whether an outer stroke straddles its cell edge.
    pub centered: bool,
    /// Whether the stroke draws above rather than below text.
    pub above_text: bool,
    /// Shared-edge arbitration priority, from 0 through 15.
    pub priority: u8,
    /// Pattern within the stroke width.
    pub style: BorderStyle,
}

/// The twelve independent half edges and centre arms of a physical cell.
#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct CellBorders {
    /// Segments in protocol bit order; absent segments have no stroke.
    pub segments: [Option<BorderStroke>; 12],
    /// Rounded quadrants, in protocol joint-bit order.
    pub joints: u8,
    /// Optional fill outside rounded corners; Default means background.
    pub outside: Option<ColorAttribute>,
    /// Requested arc radius in 1/256 cell width, absent for terminal default.
    pub radius: Option<u16>,
}

impl CellBorders {
    /// Replace the segments selected by the protocol mask.
    pub fn set(&mut self, mask: u16, stroke: Option<BorderStroke>) {
        for (bit, segment) in self.segments.iter_mut().enumerate() {
            if mask & (1 << bit) != 0 {
                *segment = stroke;
            }
        }
    }

    /// Return whether this set has no visible content.
    pub fn is_empty(&self) -> bool {
        self.segments.iter().all(Option::is_none)
            && self.joints == 0
            && self.outside.is_none()
            && self.radius.is_none()
    }
}
