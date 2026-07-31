//! ASCII map widget — renders visible world as colored characters.
//!
//! - `@` = player (cyan)
//! - `M` = mobile (yellow/red depending on notoriety)
//! - `I` = item (fg from radarcol static colors; see `item_char` for future
//!         per-graphic symbols)
//! - `*` = impassable (dark red)
//! - `#` = door (orange)
//! - `~` = water (blue)
//! - `.` = passable ground

use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

use files::tiledata::TileFlags;
use framework::vessel::Entity;

use crate::app::App;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Map ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(session) = app.active_session() else {
        return;
    };

    let (px, py, _pz) = session.position();
    let view_range = session.observer.view_range() as i32;

    // Map dimensions in cells.
    let map_w = inner.width as i32;
    let map_h = inner.height as i32;
    let center_x = map_w / 2;
    let center_y = map_h / 2;

    let buf = frame.buffer_mut();

    // Draw terrain + entities.
    for dy in 0..map_h {
        for dx in 0..map_w {
            let wx = px as i32 + (dx - center_x);
            let wy = py as i32 + (dy - center_y);

            if wx < 0 || wy < 0 || wx > 0xFFFF || wy > 0xFFFF {
                continue;
            }

            let cell_x = inner.x + dx as u16;
            let cell_y = inner.y + dy as u16;

            if cell_x >= inner.x + inner.width || cell_y >= inner.y + inner.height {
                continue;
            }

            let (ch, fg, bg) = tile_appearance(
                wx as u16,
                wy as u16,
                app,
            );

            // Dim tiles outside the client view range.
            let chebyshev = (dx - center_x).abs().max((dy - center_y).abs());
            let (fg, bg) = if chebyshev > view_range {
                (dim_color(fg), dim_color(bg))
            } else {
                (fg, bg)
            };

            let cell = &mut buf[(cell_x, cell_y)];
            cell.set_char(ch);
            cell.set_fg(fg);
            cell.set_bg(bg);
        }
    }

    // Draw player at center.
    let pcx = inner.x + center_x as u16;
    let pcy = inner.y + center_y as u16;
    if pcx < inner.x + inner.width && pcy < inner.y + inner.height {
        let cell = &mut buf[(pcx, pcy)];
        cell.set_char('@');
        cell.set_fg(Color::Cyan);
    }
}

/// Determine what to draw at a given world tile.
fn tile_appearance(wx: u16, wy: u16, app: &App) -> (char, Color, Color) {
    let session = match app.active_session() {
        Some(s) => s,
        None => return ('.', Color::DarkGray, Color::Black),
    };

    // ── 1. Resolve terrain background first ───────────────────────────
    let terrain_bg = terrain_bg(wx, wy, session, app);

    // ── 2. Check for entities at this tile ────────────────────────────
    let mut mobile_here = false;
    let mut item_here = false;
    let mut mobile_color = Color::Yellow;
    let mut item_graphic: u16 = 0;

    for entity in session.observer.session.visible.iter() {
        if entity.x() == wx && entity.y() == wy {
            if entity.is_mobile() {
                mobile_here = true;
                if let Some(noto) = entity.notoriety() {
                    mobile_color = notoriety_color(noto);
                }
            } else {
                item_here = true;
                item_graphic = entity.graphic();
            }
        }
    }

    if mobile_here {
        return ('M', mobile_color, terrain_bg);
    }
    if item_here {
        let fg = item_color(item_graphic, app);
        let ch = item_char(item_graphic);
        return (ch, fg, terrain_bg);
    }

    // ── 3. Terrain features (statics, land flags) ─────────────────────
    terrain_appearance(wx, wy, session, terrain_bg)
}

/// Compute the terrain background color for a world tile (from radarcol land
/// colors).  This is used as the bg behind entities as well.
fn terrain_bg(wx: u16, wy: u16, session: &crate::game_session::GameSession, app: &App) -> Color {
    if let Some(sd) = &session.static_data {
        let world = session.world();
        if let Some(map_tile) = sd.land_tile_at(world, wx, wy) {
            return land_tile_bg(map_tile.tile_id, app);
        }
    }
    Color::Black
}

/// Terrain character/colors (statics and land flags), given an already-
/// computed background.
fn terrain_appearance(
    wx: u16,
    wy: u16,
    session: &crate::game_session::GameSession,
    bg: Color,
) -> (char, Color, Color) {
    if let Some(sd) = &session.static_data {
        let world = session.world();
        if sd.land_tile_at(world, wx, wy).is_some() {
            // Check statics on top.
            if let Some(statics) = sd.statics_at(world, wx, wy) {
                if let Some(top) = statics.iter().max_by_key(|s| s.z) {
                    let flags = sd.static_tile_def(top.tile_id)
                        .map(|d| d.flags)
                        .unwrap_or(TileFlags(0));

                    if flags.has(TileFlags::DOOR) {
                        return ('#', Color::Rgb(192, 128, 0), bg);
                    }
                    if flags.has(TileFlags::IMPASSABLE) && !flags.has(TileFlags::WET) {
                        return ('*', Color::Rgb(255, 0, 64), bg);
                    }
                    if flags.has(TileFlags::WET) {
                        return ('~', Color::Rgb(64, 128, 255), Color::Rgb(0, 0, 64));
                    }
                }
            }

            // Check land tile flags.
            if let Some(map_tile) = sd.land_tile_at(world, wx, wy) {
                if let Some(land_def) = sd.land_tile_def(map_tile.tile_id) {
                    if land_def.flags.has(TileFlags::WET) {
                        return ('~', Color::Rgb(64, 128, 255), Color::Rgb(0, 0, 64));
                    }
                }
            }

            return ('.', Color::DarkGray, bg);
        }
    }

    ('.', Color::DarkGray, Color::Black)
}

/// Foreground color for a dynamic item, based on its graphic via radarcol.
fn item_color(graphic: u16, app: &App) -> Color {
    if let Some(rc) = &app.radar_colors {
        if let Some(rgba) = rc.static_color(graphic) {
            return Color::Rgb(rgba.r, rgba.g, rgba.b);
        }
    }
    // Fallback when radarcol is not loaded.
    Color::Green
}

/// Map an item graphic to a display character.
///
/// This is the extension point for per-graphic symbols.  Add entries to
/// the match as needed; unrecognized graphics fall back to `'I'`.
fn item_char(_graphic: u16) -> char {
    // TODO: add per-graphic character mappings, e.g.:
    //   0x0E75..=0x0E7A => '$',  // gold coins
    //   0x0A12..=0x0A18 => '!',  // potions
    //   0x0F51..=0x0F52 => '+',  // daggers
    'I'
}

/// Map land tile ID to a background color.
fn land_tile_bg(tile_id: u16, app: &App) -> Color {
    // Use radarcol if loaded.
    if let Some(rc) = &app.radar_colors {
        if let Some(rgba) = rc.land_color(tile_id) {
            return Color::Rgb(rgba.r, rgba.g, rgba.b);
        }
    }
    // Fallback: dark green for grass-like, gray otherwise.
    Color::Rgb(20, 40, 20)
}

/// Map notoriety byte to a color.
fn notoriety_color(noto: u8) -> Color {
    match noto {
        1 => Color::Blue,       // Innocent
        2 => Color::Green,      // Friend / Ally
        3 => Color::Gray,       // Can be attacked (gray)
        4 => Color::Gray,       // Criminal
        5 => Color::Yellow,     // Enemy
        6 => Color::Red,        // Murderer
        7 => Color::Yellow,     // Invulnerable
        _ => Color::White,
    }
}

/// Dim a color to muted gray tones for tiles outside the view range.
fn dim_color(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            // Convert to approximate luminance then scale down heavily.
            let lum = (r as u16 * 2 + g as u16 * 5 + b as u16) / 8;
            let grey = (lum / 3).min(50) as u8;
            Color::Rgb(grey, grey, grey)
        }
        Color::Black => Color::Black,
        // Named colors: map to a dark gray.
        _ => Color::Rgb(30, 30, 30),
    }
}
