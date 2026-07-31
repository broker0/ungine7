use files::color::{Rgb, Rgba};
use files::radarcol::{RadarColors, LAND_ENTRIES};

#[test]
fn land_and_static_lookup() {
    // Build a small table: 3 land + 2 static
    let mut colors = Vec::new();

    // Land entries 0..16384 — fill with black, set a few
    colors.resize(LAND_ENTRIES, Rgb { r: 0, g: 0, b: 0 });
    colors[0] = Rgb { r: 10, g: 20, b: 30 };
    colors[100] = Rgb { r: 40, g: 50, b: 60 };

    // Static entries
    colors.push(Rgb { r: 70, g: 80, b: 90 });   // static 0
    colors.push(Rgb { r: 100, g: 110, b: 120 }); // static 1

    let rc = RadarColors::from_colors(colors);

    assert_eq!(rc.len(), LAND_ENTRIES + 2);
    assert_eq!(rc.land_count(), LAND_ENTRIES);
    assert_eq!(rc.static_count(), 2);

    // Land lookups
    assert_eq!(
        rc.land_color(0),
        Some(Rgba { r: 10, g: 20, b: 30, a: 255 })
    );
    assert_eq!(
        rc.land_color(100),
        Some(Rgba { r: 40, g: 50, b: 60, a: 255 })
    );

    // Static lookups
    assert_eq!(
        rc.static_color(0),
        Some(Rgba { r: 70, g: 80, b: 90, a: 255 })
    );
    assert_eq!(
        rc.static_color(1),
        Some(Rgba { r: 100, g: 110, b: 120, a: 255 })
    );

    // Out of range
    assert_eq!(rc.static_color(2), None);
}

#[test]
fn land_color_rejects_high_ids() {
    let rc = RadarColors::from_colors(vec![Rgb { r: 0, g: 0, b: 0 }; LAND_ENTRIES]);

    // Tile ID >= 16384 is not a land tile
    assert_eq!(rc.land_color(16384), None);
    assert_eq!(rc.land_color(u16::MAX), None);
}

#[test]
fn empty_table() {
    let rc = RadarColors::from_colors(vec![]);
    assert!(rc.is_empty());
    assert_eq!(rc.land_count(), 0);
    assert_eq!(rc.static_count(), 0);
    assert_eq!(rc.land_color(0), None);
    assert_eq!(rc.static_color(0), None);
}
