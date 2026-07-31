//! Log preprocessor — reads a `.uolog` file and processes packets through
//! configurable handlers.
//!
//! Each handler processes packets and outputs information based on its settings.
//!
//! # Usage
//!
//! ```
//! log-preprocessor --input session.uolog --data-dir /path/to/uo/data
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;

use u_core::PacketDirection;
use common::args::DataDirArgs;
use framework::rythmos::PositionTracker;
use framework::diorama::ObserverPipeline;
use framework::ecumene::{StaticDataProvider, StaticWorldData};
use packets::character::CharacterLocaleAndBody;
use packets::login::CharacterList;
use packets::traits::BasicPacket;

use replay_proxy::packet_log;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MovementSource {
    CharacterLocaleAndBody,
    DrawGamePlayer,
    MoveReject,
    MoveAck,
    UpdateMobile,
    DrawMobile,
    Unknown,
}

impl std::fmt::Display for MovementSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MovementSource::CharacterLocaleAndBody => write!(f, "CharacterLocaleAndBody"),
            MovementSource::DrawGamePlayer => write!(f, "DrawGamePlayer"),
            MovementSource::MoveReject => write!(f, "MoveReject"),
            MovementSource::MoveAck => write!(f, "MoveAck"),
            MovementSource::UpdateMobile => write!(f, "UpdateMobile"),
            MovementSource::DrawMobile => write!(f, "DrawMobile"),
            MovementSource::Unknown => write!(f, "unknown"),
        }
    }
}

impl std::str::FromStr for MovementSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "characterlocaleandbody" => Ok(MovementSource::CharacterLocaleAndBody),
            "drawgameplayer" => Ok(MovementSource::DrawGamePlayer),
            "movereject" => Ok(MovementSource::MoveReject),
            "moveack" => Ok(MovementSource::MoveAck),
            "updatemobile" => Ok(MovementSource::UpdateMobile),
            "drawmobile" => Ok(MovementSource::DrawMobile),
            "unknown" => Ok(MovementSource::Unknown),
            _ => Err(format!("Unknown movement source: {}", s)),
        }
    }
}

pub trait PacketHandler {
    fn name(&self) -> &str;
    fn handle(&mut self, entry: &packet_log::LogEntry) -> bool;
    fn summary(&self);
}

pub struct MovementHandler {
    enabled: bool,
    verbose: bool,
    filter: Vec<MovementSource>,
    observer: ObserverPipeline,
    move_count: u32,
    warn_count: u32,
}

impl MovementHandler {
    pub fn new(enabled: bool, verbose: bool, filter: Vec<MovementSource>, static_data: Option<Arc<dyn StaticDataProvider>>) -> Self {
        Self {
            enabled,
            verbose,
            filter,
            observer: ObserverPipeline::new(static_data),
            move_count: 0,
            warn_count: 0,
        }
    }

    fn source_allowed(&self, source: MovementSource) -> bool {
        if self.filter.is_empty() {
            return true;
        }
        self.filter.contains(&source)
    }

    fn print_move(
        us: u64,
        world: u8,
        pos: &PositionTracker,
        _prev_x: u16,
        _prev_y: u16,
        prev_z: i8,
        source: MovementSource,
        warn_count: &mut u32,
        dx: i32,
        dy: i32,
    ) {
        let dz = (pos.z as i32 - prev_z as i32).abs();

        if dx > 1 || dy > 1 {
            *warn_count += 1;
            println!(
                "[{:>12}µs] World {} ({})  ({}, {}, {})  facing={}  source={}  \
                 WARNING: jump dx={} dy={} dz={}",
                us,
                world, world_name(world),
                pos.x, pos.y, pos.z,
                pos.facing.heading(),
                source,
                dx, dy, dz,
            );
        } else {
            println!(
                "[{:>12}µs] World {} ({})  ({}, {}, {})  facing={}  source={}",
                us,
                world, world_name(world),
                pos.x, pos.y, pos.z,
                pos.facing.heading(),
                source,
            );
        }
    }

    fn source_from_packet(pkt_id: u8, last_move_accepted: bool) -> MovementSource {
        match pkt_id {
            0x1B => MovementSource::CharacterLocaleAndBody,
            0x20 => MovementSource::DrawGamePlayer,
            0x21 => MovementSource::MoveReject,
            0x22 if last_move_accepted => MovementSource::MoveAck,
            0x77 => MovementSource::UpdateMobile,
            0x78 => MovementSource::DrawMobile,
            _ => MovementSource::Unknown,
        }
    }
}

impl PacketHandler for MovementHandler {
    fn name(&self) -> &str {
        "movement"
    }

    fn handle(&mut self, entry: &packet_log::LogEntry) -> bool {
        if entry.direction == PacketDirection::ClientToServer {
            self.observer.ingest_c2s(&entry.data);
            return false;
        }

        if !self.enabled {
            return false;
        }

        if entry.data.is_empty() {
            return false;
        }

        let prev_x = self.observer.pos.x;
        let prev_y = self.observer.pos.y;
        let prev_z = self.observer.pos.z;
        let prev_world = self.observer.session.current_world;

        self.observer.ingest_s2c(&entry.data);

        let world = self.observer.session.current_world;

        if world != prev_world {
            println!(
                "[{:>12}µs] *** World change: {} ({}) -> {} ({}) ***",
                entry.us_offset,
                prev_world, world_name(prev_world),
                world, world_name(world),
            );
        }

        let pkt_id = entry.data[0];

        let pos_changed = self.observer.pos.x != prev_x
            || self.observer.pos.y != prev_y
            || self.observer.pos.z != prev_z;

        if !pos_changed {
            return false;
        }

        let source = Self::source_from_packet(pkt_id, self.observer.last_move_accepted);

        if !self.source_allowed(source) {
            return false;
        }

        let dx = (self.observer.pos.x as i32 - prev_x as i32).abs();
        let dy = (self.observer.pos.y as i32 - prev_y as i32).abs();
        let _dz = (self.observer.pos.z as i32 - prev_z as i32).abs();

        if self.verbose {
            Self::print_move(
                entry.us_offset,
                world,
                &self.observer.pos,
                prev_x,
                prev_y,
                prev_z,
                source,
                &mut self.warn_count,
                dx,
                dy,
            );
        } else if dx > 1 || dy > 1 {
            self.warn_count += 1;
        }

        self.move_count += 1;
        true
    }

    fn summary(&self) {
        if !self.enabled {
            return;
        }
        println!("Total position changes: {}", self.move_count);
        if self.warn_count > 0 {
            println!("Jumps (dx>1 or dy>1): {}", self.warn_count);
        }
        if self.verbose && self.observer.pos.is_ready() {
            let world = self.observer.session.current_world;
            println!(
                "Final position: World {} ({})  ({}, {}, {})  facing={}",
                world, world_name(world),
                self.observer.pos.x,
                self.observer.pos.y,
                self.observer.pos.z,
                self.observer.pos.facing.heading(),
            );
        }
    }
}

pub struct CharacterHandler {
    printed_name: bool,
    printed_serial: bool,
    player_serial: u32,
    enabled: bool,
}

impl CharacterHandler {
    pub fn new(enabled: bool) -> Self {
        Self {
            printed_name: false,
            printed_serial: false,
            player_serial: 0,
            enabled,
        }
    }

    pub fn player_serial(&self) -> u32 {
        self.player_serial
    }
}

impl PacketHandler for CharacterHandler {
    fn name(&self) -> &str {
        "character"
    }

    fn handle(&mut self, entry: &packet_log::LogEntry) -> bool {
        if !self.enabled {
            return false;
        }

        if entry.direction != PacketDirection::ServerToClient || entry.data.is_empty() {
            return false;
        }

        let pkt_id = entry.data[0];

        if pkt_id == 0xA9 && !self.printed_name {
            if let Ok(cl) = CharacterList::from_bytes(&entry.data) {
                for slot in cl.characters.iter() {
                    if !slot.is_empty() {
                        println!("Character: {}", slot.name.to_string());
                        println!();
                        self.printed_name = true;
                        break;
                    }
                }
            }
            return true;
        }

        if pkt_id == CharacterLocaleAndBody::ID && !self.printed_serial {
            if let Ok(pkt) = CharacterLocaleAndBody::from_bytes(&entry.data) {
                self.player_serial = pkt.serial;
                self.printed_serial = true;
                println!(
                    "[{:>12}µs] Player serial: {:#010X}",
                    entry.us_offset, self.player_serial,
                );
                return true;
            }
        }

        false
    }

    fn summary(&self) {
        if !self.enabled {
            return;
        }
    }
}

// ── CLI ──────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "log-preprocessor", about = "Print player movement trace from a .uolog file")]
struct Args {
    /// Path to the .uolog file to analyse.
    #[arg(short, long)]
    input: PathBuf,

    #[command(flatten)]
    handlers: HandlerArgs,

    #[command(flatten)]
    data: DataDirArgs,
}

#[derive(Parser)]
#[command(name = "handlers")]
struct HandlerArgs {
    /// Disable movement handler
    #[arg(long)]
    no_movement: bool,

    /// Verbose movement output (print every move)
    #[arg(long, default_value_t = false)]
    movement_verbose: bool,

    /// Disable character info handler
    #[arg(long)]
    no_character: bool,

    /// Filter movement sources (comma-separated, e.g. "MoveAck,DrawMobile")
    /// Empty = print all.
    #[arg(long, value_parser = clap::value_parser!(MovementSource), value_delimiter = ',')]
    movement_filter: Vec<MovementSource>,
}

impl HandlerArgs {
    fn movement(&self) -> bool {
        !self.no_movement
    }

    fn character(&self) -> bool {
        !self.no_character
    }

    fn movement_filter(&self) -> &[MovementSource] {
        &self.movement_filter
    }
}

// ── map names ────────────────────────────────────────────────────────────

fn world_name(id: u8) -> &'static str {
    match id {
        0 => "Felucca",
        1 => "Trammel",
        2 => "Ilshenar",
        3 => "Malas",
        4 => "Tokuno Islands",
        5 => "Ter Mur",
        _ => "Unknown",
    }
}

// ── main ─────────────────────────────────────────────────────────────────

fn main() {
    let args = Args::parse();

    // ── load static world data (optional) ────────────────────────────────
    let static_data: Option<Arc<dyn StaticDataProvider>> = match args.data.path() {
        Some(dir) => match StaticWorldData::load(dir) {
            Ok(sd) => {
                println!("World data: loaded from {}", dir.display());
                Some(Arc::new(sd))
            }
            Err(e) => {
                eprintln!("warning: failed to load world data from {}: {e}", dir.display());
                eprintln!("         Z values will not be resolved (use --data-dir)");
                None
            }
        },
        None => {
            println!("World data: disabled (--no-data)");
            None
        }
    };

    // ── load log ─────────────────────────────────────────────────────────
    let log = match packet_log::read_log(&args.input) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("error: cannot read {:?}: {e}", args.input);
            std::process::exit(1);
        }
    };

    println!("Loaded {} entries from {:?}", log.len(), args.input);
    println!();

// ── init handlers ────────────────────────────────────────────────────
    let mut movement_handler = if args.handlers.movement() {
        Some(MovementHandler::new(true, args.handlers.movement_verbose, args.handlers.movement_filter().to_vec(), static_data.clone()))
    } else {
        None
    };
    let mut character_handler = if args.handlers.character() {
        Some(CharacterHandler::new(true))
    } else {
        None
    };

    // ── process entries ──────────────────────────────────────────────────
    for entry in &log {
        // Pass to handlers
        if let Some(h) = &mut character_handler {
            h.handle(entry);
        }
        if let Some(h) = &mut movement_handler {
            h.handle(entry);
        }
    }

    // ── summary ──────────────────────────────────────────────────────────
    println!();
    if let Some(last) = log.last() {
        let total_secs = last.us_offset / 1_000_000;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        println!("Session duration: {}m {}s ({} µs)", mins, secs, last.us_offset);
    }

    if let Some(h) = &character_handler {
        h.summary();
    }
    if let Some(h) = &movement_handler {
        h.summary();
    }
}