use files::color::{rgb555_to_rgb, Rgb, Rgba};

#[test]
fn rgb555_conversion() {
    // Pure white: 0b0_11111_11111_11111 = 0x7FFF
    let white = rgb555_to_rgb(0x7FFF);
    assert_eq!(white, Rgb { r: 255, g: 255, b: 255 });

    // Pure black: 0x0000
    let black = rgb555_to_rgb(0x0000);
    assert_eq!(black, Rgb { r: 0, g: 0, b: 0 });

    // Pure red: 0b0_11111_00000_00000 = 0x7C00
    let red = rgb555_to_rgb(0x7C00);
    assert_eq!(red, Rgb { r: 255, g: 0, b: 0 });

    // Pure green: 0b0_00000_11111_00000 = 0x03E0
    let green = rgb555_to_rgb(0x03E0);
    assert_eq!(green, Rgb { r: 0, g: 255, b: 0 });

    // Pure blue: 0b0_00000_00000_11111 = 0x001F
    let blue = rgb555_to_rgb(0x001F);
    assert_eq!(blue, Rgb { r: 0, g: 0, b: 255 });
}

#[test]
fn rgb_to_rgba() {
    let c = Rgb { r: 100, g: 150, b: 200 };
    assert_eq!(c.opaque(), Rgba { r: 100, g: 150, b: 200, a: 255 });
    assert_eq!(c.with_alpha(128), Rgba { r: 100, g: 150, b: 200, a: 128 });
}
