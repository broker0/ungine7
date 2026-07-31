//! Shared color types and conversion helpers.
//!
//! UO stores pixel colors as 16-bit RGB555 values (5 bits per channel,
//! high bit unused).  This module provides types and conversion to
//! standard 8-bit-per-channel representations.

/// A 24-bit RGB color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// A 32-bit RGBA color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgb {
    /// Convert to [`Rgba`] with the given alpha value.
    pub const fn with_alpha(self, a: u8) -> Rgba {
        Rgba {
            r: self.r,
            g: self.g,
            b: self.b,
            a,
        }
    }

    /// Convert to [`Rgba`] with full opacity (alpha = 255).
    pub const fn opaque(self) -> Rgba {
        self.with_alpha(255)
    }
}

/// Convert a UO 16-bit RGB555 color to 24-bit [`Rgb`].
///
/// Format: `0RRRRRGGGGGBBBBB` (bit 15 unused).
/// Each 5-bit channel is expanded to 8 bits via `(v << 3) | (v >> 2)`.
pub fn rgb555_to_rgb(color: u16) -> Rgb {
    let r5 = ((color >> 10) & 0x1F) as u8;
    let g5 = ((color >> 5) & 0x1F) as u8;
    let b5 = (color & 0x1F) as u8;
    Rgb {
        r: (r5 << 3) | (r5 >> 2),
        g: (g5 << 3) | (g5 >> 2),
        b: (b5 << 3) | (b5 >> 2),
    }
}
