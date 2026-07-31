//! Axis-aligned rectangle in tile coordinates.

use u_core::{MobilePos, Pos3D};

/// Axis-aligned rectangle in tile coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    pub x_min: u16,
    pub y_min: u16,
    pub x_max: u16,
    pub y_max: u16,
}

impl TileRect {
    /// Single-tile rectangle at `(x, y)`.
    pub fn point(x: u16, y: u16) -> Self {
        Self { x_min: x, y_min: y, x_max: x, y_max: y }
    }

    /// Build the tile rectangle covering the Chebyshev `range` around `(cx, cy)`.
    pub fn from_view(cx: u16, cy: u16, range: u16) -> Self {
        Self {
            x_min: cx.saturating_sub(range),
            y_min: cy.saturating_sub(range),
            x_max: cx.saturating_add(range),
            y_max: cy.saturating_add(range),
        }
    }

    /// Whether a 3D position falls within this rectangle (Z is ignored).
    pub fn contains_pos(&self, pos: &Pos3D) -> bool {
        pos.x >= self.x_min
            && pos.x <= self.x_max
            && pos.y >= self.y_min
            && pos.y <= self.y_max
    }

    /// Whether a mobile position falls within this rectangle (Z and facing are ignored).
    pub fn contains_mpos(&self, pos: &MobilePos) -> bool {
        pos.x >= self.x_min
            && pos.x <= self.x_max
            && pos.y >= self.y_min
            && pos.y <= self.y_max
    }

    /// Whether the two rectangles overlap at all.
    pub fn overlaps(&self, other: &TileRect) -> bool {
        self.x_min <= other.x_max
            && self.x_max >= other.x_min
            && self.y_min <= other.y_max
            && self.y_max >= other.y_min
    }

    /// Compute `self ∩ other` — the overlapping area.
    ///
    /// Returns `None` if the rectangles do not overlap.
    pub fn intersection(&self, other: &TileRect) -> Option<TileRect> {
        if !self.overlaps(other) {
            return None;
        }
        Some(TileRect {
            x_min: self.x_min.max(other.x_min),
            y_min: self.y_min.max(other.y_min),
            x_max: self.x_max.min(other.x_max),
            y_max: self.y_max.min(other.y_max),
        })
    }

    /// Compute `self \ other` — tiles in `self` not in `other`.
    ///
    /// Returns up to 4 non-overlapping rectangular strips that together
    /// cover exactly the set difference.
    pub fn difference(&self, other: &TileRect) -> Vec<TileRect> {
        if !self.overlaps(other) {
            return vec![*self];
        }

        let mut strips = Vec::with_capacity(4);

        if self.y_min < other.y_min {
            strips.push(TileRect {
                x_min: self.x_min,
                x_max: self.x_max,
                y_min: self.y_min,
                y_max: other.y_min - 1,
            });
        }

        if self.y_max > other.y_max {
            strips.push(TileRect {
                x_min: self.x_min,
                x_max: self.x_max,
                y_min: other.y_max + 1,
                y_max: self.y_max,
            });
        }

        let oy_min = self.y_min.max(other.y_min);
        let oy_max = self.y_max.min(other.y_max);

        if self.x_min < other.x_min {
            strips.push(TileRect {
                x_min: self.x_min,
                x_max: other.x_min - 1,
                y_min: oy_min,
                y_max: oy_max,
            });
        }

        if self.x_max > other.x_max {
            strips.push(TileRect {
                x_min: other.x_max + 1,
                x_max: self.x_max,
                y_min: oy_min,
                y_max: oy_max,
            });
        }

        strips
    }
}
