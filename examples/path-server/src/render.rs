//! PNG area renderer for the RenderArea API command.
//!
//! Reproduces the rendering logic from `_pf/server.rs`:
//! - Background: top tile in the stack coloured via RadarColors
//!   (topmost static if any, otherwise land tile)
//! - Auto-bounds: if points are supplied, bounds expand to fit them ±100
//! - Overlay color: packed as 0xRRGGBB integer; the original used BGR byte
//!   order so we replicate: byte0=B, byte1=G, byte2=R
//! - color == null → green with Z-based brightness: [0, z+128, 0]
//! - Response: raw PNG bytes (Content-Type: image/png), not base64 JSON

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use serde_json::Value;

use framework::ecumene::StaticTileProvider;
use framework::ecumene::TileProvider;
use framework::vessel::tile_shape::TileShape;
use u_core::Heading;

use crate::state::AppState;

/// Maximum image dimension (pixels / tiles) in either axis.
/// One pixel = one tile — no scaling.  8 192 covers the largest UO map.
const MAX_DIM: isize = 8192;

// ── Public entry point (binary PNG response) ──────────────────────────────

/// Handler for the `RenderArea` command.
///
/// Returns a raw PNG binary response (`Content-Type: image/png`).
/// On error returns a 400 response with a text body.
pub async fn handle_render_area_binary(
    state: Arc<AppState>,
    data: Value,
) -> Response<Body> {
    match render_area_inner(state, &data) {
        Ok(png) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CONTENT_LENGTH, png.len())
            .body(Body::from(png))
            .unwrap(),
        Err(e) => Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(e))
            .unwrap(),
    }
}

// ── Core render logic ─────────────────────────────────────────────────────

fn render_area_inner(state: Arc<AppState>, data: &Value) -> Result<Vec<u8>, String> {
    let world = data.get("world").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

    // Overlay points: either dict-format {"x":x,"y":y,"z":z,"w":w} (new default)
    // or legacy array-format [x, y, z, w] — both are accepted.
    let overlay: Vec<(isize, isize, i8, isize)> = data
        .get("points")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    if let Some(obj) = p.as_object() {
                        // dict format: {"x": x, "y": y, "z": z, "w": w}
                        let x = obj.get("x")?.as_i64()? as isize;
                        let y = obj.get("y")?.as_i64()? as isize;
                        let z = obj.get("z").and_then(|v| v.as_i64()).unwrap_or(0) as i8;
                        let w = obj.get("w").and_then(|v| v.as_i64()).unwrap_or(0) as isize;
                        Some((x, y, z, w))
                    } else if let Some(a) = p.as_array() {
                        // legacy array format: [x, y, z, w]
                        let x = a.get(0)?.as_i64()? as isize;
                        let y = a.get(1)?.as_i64()? as isize;
                        let z = a.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as i8;
                        let w = a.get(3).and_then(|v| v.as_i64()).unwrap_or(0) as isize;
                        Some((x, y, z, w))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Auto-bounds from points (like original: min/max ± 100), then
    // override with explicitly supplied left/top/right/bottom.
    let (auto_left, auto_top, auto_right, auto_bottom) = if !overlay.is_empty() {
        let mut mn_x = isize::MAX;
        let mut mn_y = isize::MAX;
        let mut mx_x = isize::MIN;
        let mut mx_y = isize::MIN;
        for &(x, y, _, _) in &overlay {
            mn_x = mn_x.min(x);
            mn_y = mn_y.min(y);
            mx_x = mx_x.max(x);
            mx_y = mx_y.max(y);
        }
        (mn_x - 100, mn_y - 100, mx_x + 100, mx_y + 100)
    } else {
        // Default: 256×256 block
        (0isize, 0isize, 256isize, 256isize)
    };

    let left   = data.get("left").and_then(|v| v.as_i64()).map(|v| v as isize).unwrap_or(auto_left);
    let top    = data.get("top").and_then(|v| v.as_i64()).map(|v| v as isize).unwrap_or(auto_top);
    let right  = data.get("right").and_then(|v| v.as_i64()).map(|v| v as isize).unwrap_or(auto_right);
    let bottom = data.get("bottom").and_then(|v| v.as_i64()).map(|v| v as isize).unwrap_or(auto_bottom);

    // Normalise
    let (left, right)   = (left.min(right),   left.max(right));
    let (top,  bottom)  = (top.min(bottom),    top.max(bottom));

    let width  = (right - left).clamp(1, MAX_DIM) as usize;
    let height = (bottom - top).clamp(1, MAX_DIM) as usize;

    // color: None → Z-brightness green; Some(n) → BGR-packed RGB
    // Original: `color.unwrap_or(-1)`; -1 means "no fixed color"
    // Our JSON: null → None, integer → Some(n)
    let fixed_color: Option<i64> = data.get("color").and_then(|v| {
        if v.is_null() { None } else { v.as_i64() }
    });

    // ── Build RGB pixel buffer ────────────────────────────────────────
    let mut pixels: Vec<u8> = vec![30u8; width * height * 3]; // dark grey default

    if let Some(rc) = &state.radar_colors {
        let sd_opt: Option<&dyn framework::vessel::traits::StaticDataProvider> =
            state.static_data.0.as_deref()
                .map(|sd| sd as &dyn framework::vessel::traits::StaticDataProvider);

        let provider = StaticTileProvider::new(
            state.static_data.0.as_deref()
                .map(|sd| sd as &dyn framework::vessel::traits::StaticDataProvider),
            world,
        );

        for py in 0..height {
            for px in 0..width {
                let tx = (left + px as isize).clamp(0, u16::MAX as isize) as u16;
                let ty = (top  + py as isize).clamp(0, u16::MAX as isize) as u16;

                let stack = provider.query_tile_stack(tx, ty, Heading::North);

                let color = top_tile_color(&stack, tx, ty, world, rc, sd_opt);

                let idx = (py * width + px) * 3;
                pixels[idx]     = color[0];
                pixels[idx + 1] = color[1];
                pixels[idx + 2] = color[2];
            }
        }
    }

    // ── Overlay path points ───────────────────────────────────────────
    for &(ox, oy, oz, _ow) in &overlay {
        let px = ox - left;
        let py = oy - top;
        if px < 0 || py < 0 || px >= width as isize || py >= height as isize {
            continue;
        }
        // Replicate original color logic:
        //   color == -1  → Rgb([0, (z+128) as u8, 0])
        //   otherwise    → Rgb([color & 0xFF, (color>>8)&0xFF, (color>>16)&0xFF])
        //   which is BGR order (blue in byte 0)
        let [r, g, b] = match fixed_color {
            None => [0u8, (oz as i16).saturating_add(128) as u8, 0u8],
            Some(c) => {
                let b = (c & 0xFF) as u8;
                let g = ((c >> 8) & 0xFF) as u8;
                let r = ((c >> 16) & 0xFF) as u8;
                [r, g, b]
            }
        };
        let idx = (py as usize * width + px as usize) * 3;
        pixels[idx]     = r;
        pixels[idx + 1] = g;
        pixels[idx + 2] = b;
    }

    // ── Encode to PNG ─────────────────────────────────────────────────
    encode_png(width, height, &pixels)
}

/// Pick the color of the topmost visible tile in a stack.
///
/// Mirrors the original `world_tile_color(&top_tile)`:
/// - Prefer the topmost non-Background shape (= topmost static/slope)
///   using its static tile color from RadarColors.
/// - Fall back to the land tile color.
/// - If nothing is available, return dark grey.
fn top_tile_color(
    stack: &[TileShape],
    x: u16,
    y: u16,
    world: u8,
    rc: &files::radarcol::RadarColors,
    sd: Option<&dyn framework::vessel::traits::StaticDataProvider>,
) -> [u8; 3] {
    // The stack is sorted bottom-to-top.  Find the topmost non-Background tile.
    // We distinguish land vs static by checking if it came from a static tile:
    // land tiles are always pushed first (index 0 when present), statics after.
    // A heuristic that works: land tiles have flags = 0 when no tiledata, or
    // their graphic is stored in `land_tile_at`. We use a different approach:
    // query statics separately and use radar color for the topmost one.

    // Try statics: the topmost static graphic above z=0 gives the best color.
    if let Some(sd) = sd {
        if let Some(statics) = sd.statics_at(world, x, y) {
            // statics are sorted by z; take the last (topmost)
            if let Some(st) = statics.last() {
                if let Some(c) = rc.static_color(st.tile_id) {
                    // Skip fully black tiles (often void/transparent)
                    if c.r != 0 || c.g != 0 || c.b != 0 {
                        return [c.r, c.g, c.b];
                    }
                }
            }
        }

        // Fall back to land tile
        if let Some(tile) = sd.land_tile_at(world, x, y) {
            if let Some(c) = rc.land_color(tile.tile_id) {
                return [c.r, c.g, c.b];
            }
        }
    }

    // Last resort: use topmost non-Background shape from stack
    for shape in stack.iter().rev() {
        match shape {
            TileShape::Background { .. } => continue,
            _ => return [60, 60, 60],
        }
    }

    [30, 30, 30]
}

// ── Minimal PNG encoder ───────────────────────────────────────────────────

fn encode_png(width: usize, height: usize, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(width * height * 3 + 1024);

    // PNG signature
    out.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    // IHDR
    let mut ihdr = [0u8; 13];
    ihdr[0..4].copy_from_slice(&(width as u32).to_be_bytes());
    ihdr[4..8].copy_from_slice(&(height as u32).to_be_bytes());
    ihdr[8]  = 8; // bit depth
    ihdr[9]  = 2; // color type RGB
    write_png_chunk(&mut out, b"IHDR", &ihdr);

    // IDAT: filter byte 0 (None) per row + RGB pixels
    let mut raw = Vec::with_capacity((width * 3 + 1) * height);
    for row in 0..height {
        raw.push(0u8); // filter type None
        let row_start = row * width * 3;
        raw.extend_from_slice(&rgb[row_start..row_start + width * 3]);
    }
    let compressed = zlib_deflate(&raw);
    write_png_chunk(&mut out, b"IDAT", &compressed);

    // IEND
    write_png_chunk(&mut out, b"IEND", &[]);

    Ok(out)
}

fn write_png_chunk(out: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(name, data).to_be_bytes());
}

/// zlib stored-block deflate (no compression).
fn zlib_deflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 10);
    out.push(0x78);
    out.push(0x9C);

    const MAX_BLOCK: usize = 65535;
    let mut pos = 0;
    while pos < data.len() {
        let end = (pos + MAX_BLOCK).min(data.len());
        let block = &data[pos..end];
        let is_last = end == data.len();
        out.push(if is_last { 0x01 } else { 0x00 });
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
        pos = end;
    }
    // Edge case: empty input
    if data.is_empty() {
        out.push(0x01); // last block
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(!0u16).to_le_bytes());
    }

    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for &b in data {
        s1 = (s1 + b as u32) % MOD;
        s2 = (s2 + s1) % MOD;
    }
    (s2 << 16) | s1
}

fn crc32(name: &[u8], data: &[u8]) -> u32 {
    let table: [u32; 256] = {
        let mut t = [0u32; 256];
        for n in 0..256u32 {
            let mut c = n;
            for _ in 0..8 { c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 }; }
            t[n as usize] = c;
        }
        t
    };
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in name.iter().chain(data.iter()) {
        crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}
