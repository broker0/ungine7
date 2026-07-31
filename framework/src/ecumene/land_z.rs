//! Land tile vertex Z computation — shared helpers.
//!
//! These functions compute `(z_base, z_stand, z_top)` for a land tile
//! from its four vertex heights.  They are extracted as free functions so
//! that both [`StaticWorldData`](super::static_world_data::StaticWorldData)
//! and [`DiffAwareDataProvider`](super::diff_provider::DiffAwareDataProvider)
//! can reuse the same math without duplication.

use u_core::Heading;

/// Compute direction-dependent `(z_base, z_stand, z_top)` for a land tile.
///
/// # Parameters
///
/// - `left`   — vertex Z at `(x, y)`.
/// - `bottom` — vertex Z at `(x+1, y)`.
/// - `right`  — vertex Z at `(x+1, y+1)`.
/// - `top`    — vertex Z at `(x, y+1)`.
/// - `direction` — approach direction (determines exit Z).
///
/// # Returns
///
/// `(z_base, z_stand, z_top)` where:
/// - `z_base`  — minimum of the four vertices.
/// - `z_stand` — standing Z (average of vertex pair with least height delta).
/// - `z_top`   — maximum of `z_stand` and the direction-dependent exit Z.
pub fn compute_land_z_stand(
    left: i8,
    bottom: i8,
    right: i8,
    top: i8,
    direction: Heading,
) -> (i8, i8, i8) {
    let left = left as i16;
    let bottom = bottom as i16;
    let right = right as i16;
    let top = top as i16;

    let min_z = left.min(bottom).min(right).min(top);

    // Standing Z: average of vertex pair with least height difference.
    let standing_z = if (left - right).abs() > (top - bottom).abs() {
        top + bottom
    } else {
        left + right
    };
    let standing_z = if standing_z < 0 { standing_z - 1 } else { standing_z } / 2;

    // Exit Z: depends on direction (1-2 vertices on the exit edge).
    let exit_z = match direction {
        Heading::North     => (left + bottom) / 2,
        Heading::NorthEast => bottom,
        Heading::East      => (bottom + right) / 2,
        Heading::SouthEast => right,
        Heading::South     => (right + top) / 2,
        Heading::SouthWest => top,
        Heading::West      => (top + left) / 2,
        Heading::NorthWest => left,
    };

    let z_top = if exit_z > standing_z { exit_z } else { standing_z };

    (min_z as i8, standing_z as i8, z_top as i8)
}

/// Compute direction-agnostic `(z_base, z_stand, z_top)` for a land tile.
///
/// Unlike [`compute_land_z_stand`], `z_top` is computed as the average of
/// all four vertices (rounded towards zero), making the result independent
/// of approach direction.  Used by block-level queries where a single
/// direction is not meaningful.
///
/// # Parameters
///
/// Same as [`compute_land_z_stand`] except no `direction`.
pub fn compute_land_z_range(left: i8, bottom: i8, right: i8, top: i8) -> (i8, i8, i8) {
    let left = left as i16;
    let bottom = bottom as i16;
    let right = right as i16;
    let top = top as i16;

    let min_z = left.min(bottom).min(right).min(top);

    // Standing Z: same formula as direction-dependent version.
    let standing_z = if (left - right).abs() > (top - bottom).abs() {
        top + bottom
    } else {
        left + right
    };
    let standing_z = if standing_z < 0 { standing_z - 1 } else { standing_z } / 2;

    // Direction-agnostic z_top: average of all four vertices.
    let avg = left + bottom + right + top;
    let avg = if avg < 0 { avg - 3 } else { avg } / 4;
    let z_top = avg.max(standing_z);

    (min_z as i8, standing_z as i8, z_top as i8)
}
