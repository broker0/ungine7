//! UO data file inspector.
//!
//! Loads MUL/IDX files from a given directory, prints meta information
//! and summary tables for each supported file type.
//!
//! Currently supported:
//! - `multi.idx` + `multi.mul` -- multi-object definitions (houses, boats, etc.)
//! - `tiledata.mul` -- land and static tile metadata (flags, height, name)
//! - `artidx.mul` + `art.mul` -- art sprites (land tiles and static item graphics)
//! - `map{N}.mul` / `map{N}LegacyMUL.uop` -- world map terrain
//! - `Cliloc*.enu` (etc.) -- localised string tables
//!
//! Usage:
//!   cargo run -p data-files [-- --data-dir `<path-to-uo-data>`]
//!
//! If no `--data-dir` is given, the current working directory is used.
//!
//! Example:
//!   cargo run -p data-files -- --data-dir "C:\Games\Ultima Online\data"

use std::path::Path;
use std::time::Instant;
use std::fs;

use clap::Parser;
use log::info;

use common::args::DataDirArgs;
use common::logging::init_logger;
use files::art::{ArtReader, LAND_TILE_LIMIT};
use files::cliloc::ClilocTable;
use files::hues::HueTable;
use files::map;
use files::multi::MultiCollection;
use files::radarcol::RadarColors;
use files::statics::StaticData;
use files::tiledata::{TileData, TileFlags};

/// UO data file inspector.
#[derive(Debug, Parser)]
#[command(version, about = "Loads MUL/IDX files and prints summary information")]
struct Args {
    #[command(flatten)]
    data: DataDirArgs,
}

fn main() {
    init_logger().quiet().build().unwrap();

    let args = Args::parse();
    let data_dir = &args.data.data_dir;

    println!("==========================================================");
    println!("  UO Data File Inspector");
    println!("==========================================================");
    println!("  Path: {}", data_dir.display());
    println!();

    // ── Multi objects ──────────────────────────────────────────────────
    report_multis(&data_dir);

    // ── Tile data ───────────────────────────────────────────────────
    report_tiledata(&data_dir);

    // ── Art sprites ────────────────────────────────────────────────
    report_art(&data_dir);

    // ── Maps ───────────────────────────────────────────────────────
    report_maps(&data_dir);

    // ── Radar colors ──────────────────────────────────────────────
    report_radarcol(&data_dir);

    // ── Hues ───────────────────────────────────────────────────────
    report_hues(&data_dir);

    // ── Statics ────────────────────────────────────────────────────
    report_statics(&data_dir);

    // ── Cliloc ─────────────────────────────────────────────────────
    report_cliloc(&data_dir);

    // Future file types will be added here:
    // report_gumps(&data_dir);
    // report_animations(&data_dir);
    // ...

    println!("==========================================================");
    println!("  Done.");
    println!("==========================================================");
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Format a byte size as a human-readable string.
fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Get file size, or None if file doesn't exist.
fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

/// Print a section header.
fn section(title: &str) {
    println!("----------------------------------------------------------");
    println!("  {title}");
    println!("----------------------------------------------------------");
}

// ── Multi objects ──────────────────────────────────────────────────────────

fn report_multis(dir: &Path) {
    let idx_path = dir.join("multi.idx");
    let mul_path = dir.join("multi.mul");

    section("Multi Objects  (multi.idx + multi.mul)");

    // Check file existence
    let idx_exists = idx_path.exists();
    let mul_exists = mul_path.exists();

    if !idx_exists || !mul_exists {
        if !idx_exists {
            println!("  [skip] multi.idx not found");
        }
        if !mul_exists {
            println!("  [skip] multi.mul not found");
        }
        println!();
        return;
    }

    // File sizes
    let idx_size = file_size(&idx_path).unwrap_or(0);
    let mul_size = file_size(&mul_path).unwrap_or(0);

    println!("  multi.idx  {:>10}    multi.mul  {:>10}",
        fmt_size(idx_size), fmt_size(mul_size));
    println!();

    // Load
    let start = Instant::now();
    let multis = match MultiCollection::read(dir) {
        Ok(m) => m,
        Err(e) => {
            println!("  [error] Failed to load: {e}");
            println!();
            return;
        }
    };
    let elapsed = start.elapsed();
    info!("multi loaded in {elapsed:?}");

    // Summary
    let total_slots = multis.len();
    let valid = multis.valid_count();
    let empty = total_slots - valid;
    let total_parts = multis.total_parts();
    let avg_parts = if valid > 0 { total_parts as f64 / valid as f64 } else { 0.0 };

    println!("  Format:       {:?}", multis.format());
    println!("  Entry size:   {} bytes", multis.format().part_size());
    println!("  Loaded in:    {elapsed:.2?}");
    println!();
    println!("  Index slots:  {total_slots}");
    println!("    valid:      {valid}");
    println!("    empty:      {empty}");
    println!("  Total parts:  {total_parts}");
    println!("  Avg parts:    {avg_parts:.1}");
    println!();

    // Part-count distribution
    print_part_distribution(&multis);

    // Top 10 largest multis
    print_top_multis(&multis, 10);

    // Bounding box of the largest multi
    print_largest_bbox(&multis);
}

fn print_part_distribution(multis: &MultiCollection) {
    // Buckets: 1, 2-5, 6-10, 11-50, 51-100, 101-500, 500+
    let buckets: &[(usize, usize, &str)] = &[
        (1, 1, "     1"),
        (2, 5, "   2-5"),
        (6, 10, "  6-10"),
        (11, 50, " 11-50"),
        (51, 100, "51-100"),
        (101, 500, "101-500"),
        (501, usize::MAX, "  500+"),
    ];

    let mut counts = vec![0usize; buckets.len()];
    for (_, parts) in multis.iter() {
        let n = parts.len();
        for (i, &(lo, hi, _)) in buckets.iter().enumerate() {
            if n >= lo && n <= hi {
                counts[i] += 1;
                break;
            }
        }
    }

    println!("  Parts distribution:");
    println!("  {:<10} {:>8}", "Range", "Count");
    println!("  {:<10} {:>8}", "-----", "-----");
    for (i, &(_, _, label)) in buckets.iter().enumerate() {
        if counts[i] > 0 {
            println!("  {:<10} {:>8}", label, counts[i]);
        }
    }
    println!();
}

fn print_top_multis(multis: &MultiCollection, n: usize) {
    // Collect (id, part_count) and sort by part_count descending
    let mut ranked: Vec<(u16, usize)> = multis.iter()
        .map(|(id, parts)| (id, parts.len()))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.truncate(n);

    if ranked.is_empty() {
        return;
    }

    println!("  Top {} largest multis:", ranked.len());
    println!("  {:>8}  {:>8}  {}", "ID", "Parts", "ID (hex)");
    println!("  {:>8}  {:>8}  {}", "------", "------", "--------");
    for &(id, count) in &ranked {
        println!("  {:>8}  {:>8}  0x{:04X}", id, count, id);
    }
    println!();
}

fn print_largest_bbox(multis: &MultiCollection) {
    // Find the multi with the most parts
    let largest = multis.iter()
        .max_by_key(|(_, parts)| parts.len());

    let Some((id, parts)) = largest else { return };
    if parts.is_empty() { return; }

    let mut min_x = i16::MAX;
    let mut max_x = i16::MIN;
    let mut min_y = i16::MAX;
    let mut max_y = i16::MIN;
    let mut min_z = i16::MAX;
    let mut max_z = i16::MIN;

    for p in parts {
        min_x = min_x.min(p.x);
        max_x = max_x.max(p.x);
        min_y = min_y.min(p.y);
        max_y = max_y.max(p.y);
        min_z = min_z.min(p.z);
        max_z = max_z.max(p.z);
    }

    let width = (max_x - min_x + 1) as i32;
    let height = (max_y - min_y + 1) as i32;

    println!("  Largest multi (ID {} / 0x{:04X}) bounding box:", id, id);
    println!("    X: {} .. {}  (width: {})", min_x, max_x, width);
    println!("    Y: {} .. {}  (height: {})", min_y, max_y, height);
    println!("    Z: {} .. {}", min_z, max_z);
    println!("    Parts: {}", parts.len());

    // Count unique tile IDs
    let mut tile_ids: Vec<u16> = parts.iter().map(|p| p.tile_id).collect();
    tile_ids.sort_unstable();
    tile_ids.dedup();
    println!("    Unique tiles: {}", tile_ids.len());
    println!();
}

// ── Tile data ──────────────────────────────────────────────────────────────

fn report_tiledata(dir: &Path) {
    let path = dir.join("tiledata.mul");

    section("Tile Data  (tiledata.mul)");

    if !path.exists() {
        println!("  [skip] tiledata.mul not found");
        println!();
        return;
    }

    let size = file_size(&path).unwrap_or(0);
    println!("  tiledata.mul  {:>10}", fmt_size(size));
    println!();

    let start = Instant::now();
    let td = match TileData::read(dir) {
        Ok(t) => t,
        Err(e) => {
            println!("  [error] Failed to load: {e}");
            println!();
            return;
        }
    };
    let elapsed = start.elapsed();
    info!("tiledata loaded in {elapsed:?}");

    let land = td.land_tiles();
    let statics = td.static_tiles();

    // Count named tiles (non-empty name)
    let named_land = land.iter().filter(|t| !t.name.is_empty()).count();
    let named_statics = statics.iter().filter(|t| !t.name.is_empty()).count();

    println!("  Format:          {:?}", td.format());
    println!("  Loaded in:       {elapsed:.2?}");
    println!();
    println!("  Land tiles:      {}", land.len());
    println!("    named:         {named_land}");
    println!("  Static tiles:    {}", statics.len());
    println!("    named:         {named_statics}");
    println!();

    // Flag distribution for static tiles
    print_flag_distribution(statics);

    // Height distribution for statics
    print_height_distribution(statics);

    // Top named statics by flag count
    print_top_flagged_statics(statics, 10);
}

fn print_flag_distribution(statics: &[files::tiledata::StaticTileDef]) {
    let flags_of_interest: &[(u64, &str)] = &[
        (TileFlags::IMPASSABLE,  "Impassable"),
        (TileFlags::SURFACE,     "Surface"),
        (TileFlags::BRIDGE,      "Bridge"),
        (TileFlags::WALL,        "Wall"),
        (TileFlags::DOOR,        "Door"),
        (TileFlags::ROOF,        "Roof"),
        (TileFlags::CONTAINER,   "Container"),
        (TileFlags::WEARABLE,    "Wearable"),
        (TileFlags::WEAPON,      "Weapon"),
        (TileFlags::ARMOR,       "Armor"),
        (TileFlags::LIGHT_SOURCE, "LightSource"),
        (TileFlags::ANIMATED,    "Animated"),
        (TileFlags::TRANSPARENT, "Transparent"),
        (TileFlags::WET,         "Wet"),
        (TileFlags::FOLIAGE,     "Foliage"),
        (TileFlags::STACKABLE,   "Stackable"),
    ];

    println!("  Static tile flags:");
    println!("  {:<14} {:>8}", "Flag", "Count");
    println!("  {:<14} {:>8}", "--------", "-----");
    for &(flag, name) in flags_of_interest {
        let count = statics.iter().filter(|t| t.flags.has(flag)).count();
        if count > 0 {
            println!("  {:<14} {:>8}", name, count);
        }
    }
    println!();
}

fn print_height_distribution(statics: &[files::tiledata::StaticTileDef]) {
    let buckets: &[(u8, u8, &str)] = &[
        (0, 0,   "     0"),
        (1, 5,   "   1-5"),
        (6, 10,  "  6-10"),
        (11, 20, " 11-20"),
        (21, 50, " 21-50"),
        (51, 255, "  51+"),
    ];

    let mut counts = vec![0usize; buckets.len()];
    for t in statics {
        for (i, &(lo, hi, _)) in buckets.iter().enumerate() {
            if t.height >= lo && t.height <= hi {
                counts[i] += 1;
                break;
            }
        }
    }

    println!("  Static tile height distribution:");
    println!("  {:<10} {:>8}", "Range", "Count");
    println!("  {:<10} {:>8}", "-----", "-----");
    for (i, &(_, _, label)) in buckets.iter().enumerate() {
        if counts[i] > 0 {
            println!("  {:<10} {:>8}", label, counts[i]);
        }
    }
    println!();
}

fn print_top_flagged_statics(statics: &[files::tiledata::StaticTileDef], n: usize) {
    // Rank by number of flag bits set
    let mut ranked: Vec<(usize, u32, &str)> = statics.iter()
        .enumerate()
        .filter(|(_, t)| !t.name.is_empty())
        .map(|(id, t)| (id, t.flags.raw().count_ones(), &*t.name))
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    ranked.truncate(n);

    if ranked.is_empty() {
        return;
    }

    println!("  Top {} static tiles by flag count:", ranked.len());
    println!("  {:>8}  {:>6}  {}", "ID", "Flags", "Name");
    println!("  {:>8}  {:>6}  {}", "------", "-----", "----");
    for &(id, bits, name) in &ranked {
        println!("  {:>8}  {:>6}  {}", id, bits, name);
    }
    println!();
}

// ── Art sprites ────────────────────────────────────────────────────────────

fn report_art(dir: &Path) {
    let idx_path = dir.join("artidx.mul");
    let mul_path = dir.join("art.mul");

    section("Art Sprites  (artidx.mul + art.mul)");

    let idx_exists = idx_path.exists();
    let mul_exists = mul_path.exists();

    if !idx_exists || !mul_exists {
        if !idx_exists {
            println!("  [skip] artidx.mul not found");
        }
        if !mul_exists {
            println!("  [skip] art.mul not found");
        }
        println!();
        return;
    }

    let idx_size = file_size(&idx_path).unwrap_or(0);
    let mul_size = file_size(&mul_path).unwrap_or(0);

    println!("  artidx.mul {:>10}    art.mul    {:>10}",
        fmt_size(idx_size), fmt_size(mul_size));
    println!();

    let start = Instant::now();
    let mut art = match ArtReader::read(dir) {
        Ok(a) => a,
        Err(e) => {
            println!("  [error] Failed to open: {e}");
            println!();
            return;
        }
    };
    let index_elapsed = start.elapsed();

    let total_slots = art.index().len();
    let valid_count = art.index().iter().filter(|e| e.is_valid()).count();
    let land_valid = art.index().iter().take(LAND_TILE_LIMIT as usize)
        .filter(|e| e.is_valid()).count();
    let static_valid = valid_count - land_valid;

    println!("  Index loaded in:  {index_elapsed:.2?}");
    println!();
    println!("  Index slots:  {total_slots}");
    println!("    valid:      {valid_count}");
    println!("    land:       {land_valid}  (ID 0..0x{:04X})", LAND_TILE_LIMIT - 1);
    println!("    static:     {static_valid}  (ID 0x{:04X}+)", LAND_TILE_LIMIT);
    println!();

    // Data size distribution from index entries
    print_art_data_sizes(art.index());

    // Sample some sprites to show actual dimensions
    print_art_sample_sprites(&mut art);
}

fn print_art_data_sizes(index: &files::mul::MulIndex) {
    let buckets: &[(u32, u32, &str)] = &[
        (1, 100, "    1-100 B"),
        (101, 500, "  101-500 B"),
        (501, 1024, " 501-1024 B"),
        (1025, 4096, "   1-4 KiB"),
        (4097, 16384, "  4-16 KiB"),
        (16385, u32::MAX, "   16+ KiB"),
    ];

    let mut counts = vec![0usize; buckets.len()];
    let mut total_bytes: u64 = 0;

    for entry in index.iter().filter(|e| e.is_valid()) {
        let len = entry.length;
        total_bytes += len as u64;
        for (i, &(lo, hi, _)) in buckets.iter().enumerate() {
            if len >= lo && len <= hi {
                counts[i] += 1;
                break;
            }
        }
    }

    println!("  Entry data size distribution:");
    println!("  {:<14} {:>8}", "Range", "Count");
    println!("  {:<14} {:>8}", "--------", "-----");
    for (i, &(_, _, label)) in buckets.iter().enumerate() {
        if counts[i] > 0 {
            println!("  {:<14} {:>8}", label, counts[i]);
        }
    }
    println!("  Total data:   {}", fmt_size(total_bytes));
    println!();
}

fn print_art_sample_sprites(art: &mut ArtReader<std::io::BufReader<std::fs::File>>) {
    // Try to decode a few land tiles and static sprites to show dimensions.
    let mut land_samples: Vec<(u32, u16, u16, usize)> = Vec::new();
    let mut static_samples: Vec<(u32, u16, u16, usize)> = Vec::new();

    // Sample first few valid land tiles
    for id in 0..LAND_TILE_LIMIT {
        if land_samples.len() >= 3 { break; }
        if let Ok(sprite) = art.read_sprite(id) {
            land_samples.push((id, sprite.width, sprite.height, sprite.opaque_count()));
        }
    }

    // Sample first few valid static sprites
    let total = art.index().len() as u32;
    for id in LAND_TILE_LIMIT..total {
        if static_samples.len() >= 5 { break; }
        if let Ok(sprite) = art.read_sprite(id) {
            static_samples.push((id, sprite.width, sprite.height, sprite.opaque_count()));
        }
    }

    if !land_samples.is_empty() {
        println!("  Sample land tiles:");
        println!("  {:>8}  {:>5}  {:>5}  {:>8}", "ID", "W", "H", "Opaque");
        println!("  {:>8}  {:>5}  {:>5}  {:>8}", "------", "---", "---", "------");
        for &(id, w, h, opaque) in &land_samples {
            println!("  {:>8}  {:>5}  {:>5}  {:>8}", id, w, h, opaque);
        }
        println!();
    }

    if !static_samples.is_empty() {
        println!("  Sample static sprites:");
        println!("  {:>8}  {:>5}  {:>5}  {:>8}  {}", "ID", "W", "H", "Opaque", "ID (hex)");
        println!("  {:>8}  {:>5}  {:>5}  {:>8}  {}", "------", "---", "---", "------", "--------");
        for &(id, w, h, opaque) in &static_samples {
            println!("  {:>8}  {:>5}  {:>5}  {:>8}  0x{:04X}", id, w, h, opaque, id);
        }
        println!();
    }
}

// ── Radar colors ──────────────────────────────────────────────────────────

fn report_radarcol(dir: &Path) {
    let path = dir.join("Radarcol.mul");

    section("Radar Colors  (Radarcol.mul)");

    if !path.exists() {
        println!("  [skip] Radarcol.mul not found");
        println!();
        return;
    }

    let size = file_size(&path).unwrap_or(0);
    println!("  Radarcol.mul  {:>10}", fmt_size(size));
    println!();

    let start = Instant::now();
    let rc = match RadarColors::read(dir) {
        Ok(r) => r,
        Err(e) => {
            println!("  [error] Failed to load: {e}");
            println!();
            return;
        }
    };
    let elapsed = start.elapsed();
    info!("radarcol loaded in {elapsed:?}");

    println!("  Loaded in:       {elapsed:.2?}");
    println!("  Total entries:   {}", rc.len());
    println!("    land colors:   {}", rc.land_count());
    println!("    static colors: {}", rc.static_count());

    // Count unique colors
    let mut unique: Vec<_> = (0..rc.len())
        .filter_map(|i| {
            if i < 0x4000 {
                rc.land_color(i as u16)
            } else {
                rc.static_color((i - 0x4000) as u16)
            }
        })
        .collect();
    unique.sort_by_key(|c| (c.r, c.g, c.b));
    unique.dedup();
    println!("  Unique colors:   {}", unique.len());

    // Count black (0,0,0) entries — these are typically unused/invisible tiles
    let black_count = (0..rc.len())
        .filter_map(|i| {
            if i < 0x4000 {
                rc.land_color(i as u16)
            } else {
                rc.static_color((i - 0x4000) as u16)
            }
        })
        .filter(|c| c.r == 0 && c.g == 0 && c.b == 0)
        .count();
    println!("  Black (unused):  {black_count}");
    println!();
}

// ── Hues ──────────────────────────────────────────────────────────────────

fn report_hues(dir: &Path) {
    let path = dir.join("hues.mul");

    section("Hues  (hues.mul)");

    if !path.exists() {
        println!("  [skip] hues.mul not found");
        println!();
        return;
    }

    let size = file_size(&path).unwrap_or(0);
    println!("  hues.mul  {:>10}", fmt_size(size));
    println!();

    let start = Instant::now();
    let table = match HueTable::read(dir) {
        Ok(t) => t,
        Err(e) => {
            println!("  [error] Failed to load: {e}");
            println!();
            return;
        }
    };
    let elapsed = start.elapsed();
    info!("hues loaded in {elapsed:?}");

    println!("  Loaded in:      {elapsed:.2?}");
    println!("  Total entries:  {}", table.len());
    println!("  Named entries:  {}", table.named_count());

    // Show a few sample named hues
    let samples: Vec<_> = table.iter()
        .enumerate()
        .filter(|(_, e)| !e.name.is_empty())
        .take(5)
        .collect();

    if !samples.is_empty() {
        println!();
        println!("  Sample hues (first -> last color in gradient):");
        println!("  {:>6}  {:>15}  {:>15}  {}", "Index", "First RGB", "Last RGB", "Name");
        println!("  {:>6}  {:>15}  {:>15}  {}", "-----", "---------", "--------", "----");
        for (i, entry) in &samples {
            let first = entry.colors[0];
            let last = entry.colors[31];
            println!(
                "  {:>6}  ({:>3},{:>3},{:>3})  ({:>3},{:>3},{:>3})  {}",
                i,
                first.r, first.g, first.b,
                last.r, last.g, last.b,
                entry.name,
            );
        }
    }
    println!();
}

// ── Maps ──────────────────────────────────────────────────────────────────

const MAP_NAMES: &[&str] = &[
    "Felucca",
    "Trammel",
    "Ilshenar",
    "Malas",
    "Tokuno Islands",
    "Ter Mur",
];

fn report_maps(dir: &Path) {
    section("Maps  (map{N}.mul / map{N}LegacyMUL.uop)");

    let mut any_found = false;

    for world in 0..6u8 {
        let mul_path = dir.join(format!("map{world}.mul"));
        let uop_path = dir.join(format!("map{world}LegacyMUL.uop"));

        let (source_label, size) = if uop_path.exists() {
            let s = file_size(&uop_path).unwrap_or(0);
            (format!("map{world}LegacyMUL.uop"), s)
        } else if mul_path.exists() {
            let s = file_size(&mul_path).unwrap_or(0);
            (format!("map{world}.mul"), s)
        } else {
            continue;
        };

        any_found = true;
        let name = MAP_NAMES.get(world as usize).unwrap_or(&"Unknown");

        let start = Instant::now();
        let map = match map::read(dir, world) {
            Ok(m) => m,
            Err(e) => {
                println!("  Map {world} ({name}):  {source_label}  {}", fmt_size(size));
                println!("    [error] {e}");
                println!();
                continue;
            }
        };
        let elapsed = start.elapsed();
        info!("map{world} loaded in {elapsed:?}");

        let (xb, yb) = (map.x_blocks(), map.y_blocks());

        println!("  Map {world} ({name}):");
        println!("    Source:     {source_label}  ({})", fmt_size(size));
        println!("    Blocks:    {xb} x {yb} = {}", map.total_blocks());
        println!("    Tiles:     {} x {}", map.width(), map.height());
        println!("    Loaded in: {elapsed:.2?}");

        // Z range across all tiles
        let mut min_z = i8::MAX;
        let mut max_z = i8::MIN;
        for (_, _, block) in map.iter_blocks() {
            for col in &block.cells {
                for tile in col {
                    min_z = min_z.min(tile.z);
                    max_z = max_z.max(tile.z);
                }
            }
        }
        println!("    Z range:   {} .. {}", min_z, max_z);
        println!();
    }

    if !any_found {
        println!("  [skip] No map files found");
        println!();
    }
}

// ── Statics ───────────────────────────────────────────────────────────────

fn report_statics(dir: &Path) {
    section("Statics  (staidx{N}.mul + statics{N}.mul)");

    let mut any_found = false;

    for world in 0..6u8 {
        let idx_path = dir.join(format!("staidx{world}.mul"));
        let mul_path = dir.join(format!("statics{world}.mul"));

        if !idx_path.exists() || !mul_path.exists() {
            continue;
        }

        any_found = true;
        let name = MAP_NAMES.get(world as usize).unwrap_or(&"Unknown");
        let idx_size = file_size(&idx_path).unwrap_or(0);
        let mul_size = file_size(&mul_path).unwrap_or(0);

        let start = Instant::now();
        let sd = match StaticData::read(dir, world) {
            Ok(s) => s,
            Err(e) => {
                println!("  Statics {world} ({name}):");
                println!("    [error] {e}");
                println!();
                continue;
            }
        };
        let elapsed = start.elapsed();
        info!("statics{world} loaded in {elapsed:?}");

        println!("  Statics {world} ({name}):");
        println!("    Index:       staidx{world}.mul  ({})", fmt_size(idx_size));
        println!("    Data:        statics{world}.mul  ({})", fmt_size(mul_size));
        println!("    Blocks:      {} x {} = {}", sd.x_blocks(), sd.y_blocks(), sd.total_blocks());
        println!("    Non-empty:   {}", sd.non_empty_blocks());
        println!("    Total tiles: {}", sd.total_tiles());
        println!("    Loaded in:   {elapsed:.2?}");
        println!();
    }

    if !any_found {
        println!("  [skip] No statics files found");
        println!();
    }
}

// ── Cliloc ────────────────────────────────────────────────────────────────

/// Known cliloc language extensions, in a rough prevalence order.
const CLILOC_LANGS: &[(&str, &str)] = &[
    ("enu", "English"),
    ("deu", "German"),
    ("esp", "Spanish"),
    ("fra", "French"),
    ("jpn", "Japanese"),
    ("kor", "Korean"),
    ("cht", "Chinese (Traditional)"),
    ("chs", "Chinese (Simplified)"),
    ("ptb", "Portuguese (Brazilian)"),
    ("ita", "Italian"),
    ("rus", "Russian"),
];

fn report_cliloc(dir: &Path) {
    section("Cliloc  (Cliloc*.enu / .deu / …)");

    // Discover which languages are present by scanning the directory.
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            println!("  [error] Cannot read directory: {e}");
            println!();
            return;
        }
    };

    // Collect cliloc files grouped by extension (language).
    let mut lang_files: std::collections::BTreeMap<String, Vec<(String, u64, &str)>> =
        std::collections::BTreeMap::new();

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let is_cliloc = path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.len() >= 6 && s[..6].eq_ignore_ascii_case("cliloc"));

        if !is_cliloc {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if ext.is_empty() {
            continue;
        }

        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        let size = file_size(&path).unwrap_or(0);

        // Detect format from first 4 bytes.
        let format_label = if size >= 4 {
            if let Ok(bytes) = fs::read(&path) {
                let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if magic == 0x4D52_4F46 {
                    "FORM"
                } else if magic <= 0xFF {
                    "plain"
                } else {
                    "encrypted"
                }
            } else {
                "?"
            }
        } else {
            "?"
        };

        lang_files.entry(ext).or_default().push((name, size, format_label));
    }

    if lang_files.is_empty() {
        println!("  [skip] No cliloc files found");
        println!();
        return;
    }

    // Print file inventory.
    println!("  Languages found: {}", lang_files.len());
    println!();

    for (ext, files) in &lang_files {
        let lang_name = CLILOC_LANGS
            .iter()
            .find(|(e, _)| *e == ext.as_str())
            .map(|(_, name)| *name)
            .unwrap_or("Unknown");

        let total_size: u64 = files.iter().map(|(_, s, _)| s).sum();

        println!("  .{ext} ({lang_name}):  {} file(s), {}", files.len(), fmt_size(total_size));

        for (name, size, format) in files {
            println!("    {name:<30} {:>10}  [{format}]", fmt_size(*size));
        }
        println!();
    }

    // Load and report the first available language in preference order,
    // falling back to whatever is present.
    let lang_to_load = CLILOC_LANGS
        .iter()
        .map(|(ext, _)| *ext)
        .find(|ext| lang_files.contains_key(*ext))
        .unwrap_or_else(|| {
            lang_files.keys().next().unwrap().as_str()
        });

    // We need a &str that outlives the call — lang_to_load is either a
    // static &str from CLILOC_LANGS or from the BTreeMap key.  In the
    // fallback case, convert to an owned String first.
    let lang_owned: String;
    let lang: &str = if CLILOC_LANGS.iter().any(|(e, _)| *e == lang_to_load) {
        lang_to_load
    } else {
        lang_owned = lang_files.keys().next().unwrap().clone();
        &lang_owned
    };

    let lang_label = CLILOC_LANGS
        .iter()
        .find(|(e, _)| *e == lang)
        .map(|(_, n)| *n)
        .unwrap_or("Unknown");

    println!("  Loading .{lang} ({lang_label}) …");

    let start = Instant::now();
    let table = match ClilocTable::read(dir, lang) {
        Ok(t) => t,
        Err(e) => {
            println!("  [error] Failed to load: {e}");
            println!();
            return;
        }
    };
    let elapsed = start.elapsed();
    info!("cliloc ({lang}) loaded in {elapsed:?}");

    println!("  Loaded in:       {elapsed:.2?}");
    println!("  Total entries:   {}", table.len());

    // ID range.
    if !table.is_empty() {
        let min_id = table.iter().map(|(id, _)| id).min().unwrap();
        let max_id = table.iter().map(|(id, _)| id).max().unwrap();
        println!("  ID range:        {} .. {}", min_id, max_id);

        // Length distribution of text values.
        print_cliloc_length_distribution(&table);

        // Count entries with placeholders (~N_tag~).
        let with_placeholders = table
            .iter()
            .filter(|(_, text)| text.contains('~'))
            .count();
        println!("  With placeholders (~N~):  {with_placeholders}");

        // Show a few sample entries.
        print_cliloc_samples(&table);
    }

    println!();
}

fn print_cliloc_length_distribution(table: &ClilocTable) {
    let buckets: &[(usize, usize, &str)] = &[
        (0, 0, "    empty"),
        (1, 20, "    1-20"),
        (21, 50, "   21-50"),
        (51, 100, "  51-100"),
        (101, 250, " 101-250"),
        (251, 500, " 251-500"),
        (501, usize::MAX, "   500+"),
    ];

    let mut counts = vec![0usize; buckets.len()];
    for (_, text) in table.iter() {
        let len = text.len();
        for (i, &(lo, hi, _)) in buckets.iter().enumerate() {
            if len >= lo && len <= hi {
                counts[i] += 1;
                break;
            }
        }
    }

    println!();
    println!("  Text length distribution:");
    println!("  {:<12} {:>8}", "Range", "Count");
    println!("  {:<12} {:>8}", "--------", "-----");
    for (i, &(_, _, label)) in buckets.iter().enumerate() {
        if counts[i] > 0 {
            println!("  {:<12} {:>8}", label, counts[i]);
        }
    }
    println!();
}

fn print_cliloc_samples(table: &ClilocTable) {
    // Well-known IDs that are interesting if present.
    let well_known: &[(u32, &str)] = &[
        (500000, "system message"),
        (502570, "system message"),
        (1042971, "generic ~1_val~"),
        (1050045, "item name"),
        (1060393, "charges"),
        (1061837, "use count"),
        (1070722, "item name"),
        (1153561, "cliloc entry"),
    ];

    let mut shown = Vec::new();
    for &(id, label) in well_known {
        if let Some(text) = table.get(id) {
            let display = if text.len() > 60 {
                format!("{}…", &text[..60])
            } else {
                text.to_string()
            };
            shown.push((id, label, display));
        }
    }

    // If we didn't find enough well-known entries, fill with first few entries
    // sorted by ID.
    if shown.len() < 5 {
        let mut sorted: Vec<_> = table.iter().collect();
        sorted.sort_by_key(|(id, _)| *id);

        for (id, text) in sorted.into_iter().take(8) {
            if shown.iter().any(|(sid, _, _)| *sid == id) {
                continue;
            }
            let display = if text.len() > 60 {
                format!("{}…", &text[..60])
            } else {
                text.to_string()
            };
            shown.push((id, "entry", display));
            if shown.len() >= 8 {
                break;
            }
        }
    }

    if !shown.is_empty() {
        println!("  Sample entries:");
        println!("  {:>10}  {:<16}  {}", "ID", "Type", "Text");
        println!("  {:>10}  {:<16}  {}", "--------", "--------", "----");
        for (id, label, text) in &shown {
            println!("  {:>10}  {:<16}  {text}", id, label);
        }
    }
}
