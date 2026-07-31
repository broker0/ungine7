//! Packet log file format.
//!
//! Each line in a `.uolog` file represents one packet:
//!
//! ```text
//! <seconds> <direction> <hex>
//! ```
//!
//! - `seconds`    — seconds (with 4 decimal places, i.e. 100 µs precision)
//!                  since the start of the recording session
//! - `direction`  — `S2C` (server->client) or `C2S` (client->server)
//! - `hex`        — raw packet bytes as an uppercase hex string (no spaces)
//!
//! Example:
//! ```text
//! 0.0000 S2C 1B00250000002A0000640064006400000000000000
//! 1.2500 S2C 20000000012345680000006400640000
//! ```

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use u_core::PacketDirection;

// -- LogEntry -----------------------------------------------------------------

/// A single entry in a `.uolog` file.
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Microseconds from the start of the recording.
    pub us_offset: u64,
    pub direction: PacketDirection,
    /// Raw packet bytes (including ID byte).
    pub data: Vec<u8>,
}

// -- LogWriter ----------------------------------------------------------------

/// Writes packet entries to a `.uolog` file.
pub struct LogWriter {
    writer: BufWriter<File>,
    start: std::time::Instant,
}

impl LogWriter {
    /// Open (or create) a log file for writing.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
            start: std::time::Instant::now(),
        })
    }

    /// Append a packet entry to the log.
    pub fn write_packet(&mut self, direction: PacketDirection, data: &[u8]) -> io::Result<()> {
        let elapsed = self.start.elapsed();
        let secs = elapsed.as_secs();
        let frac = elapsed.subsec_micros() / 100; // 0..9999 (100 µs units)
        let dir_str = match direction {
            PacketDirection::ServerToClient => "S2C",
            PacketDirection::ClientToServer => "C2S",
        };
        let hex: String = data.iter().map(|b| format!("{b:02X}")).collect();
        writeln!(self.writer, "{secs}.{frac:04} {dir_str} {hex}")
    }

    /// Flush and close the log file.
    pub fn finish(mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

// -- log file name ------------------------------------------------------------

/// Generate a log file name from the current system time and account name.
///
/// Format: `<account>_HH-MM-SS.uolog`
///
/// If `account` is empty, falls back to `unknown`.
/// Non-ASCII and filesystem-unsafe characters are replaced with `_`.
pub fn log_file_name(account: &str) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    let (_year, _month, _day, hour, min, sec) = epoch_secs_to_datetime(secs);

    let safe_account = sanitize_account(account);

    format!("{safe_account}_{hour:02}-{min:02}-{sec:02}.uolog")
}

/// Replace characters that are unsafe for filenames with `_`.
fn sanitize_account(account: &str) -> String {
    let trimmed = account.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    trimmed
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Decompose Unix timestamp (seconds) into (year, month, day, hour, min, sec).
///
/// Gregorian calendar proleptic; no leap seconds.
fn epoch_secs_to_datetime(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = secs % 60;
    let mins = secs / 60;
    let min = mins % 60;
    let hours = mins / 60;
    let hour = hours % 24;
    let days = hours / 24; // days since 1970-01-01

    // Gregorian calendar algorithm (days since epoch -> date)
    // Using the algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    (year, month, day, hour, min, sec)
}

// -- log reading --------------------------------------------------------------

/// Parse a `.uolog` file and return all entries.
///
/// Lines that cannot be parsed are silently skipped.
pub fn read_log(path: &Path) -> io::Result<Vec<LogEntry>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(entry) = parse_line(line) {
            entries.push(entry);
        }
    }

    Ok(entries)
}

/// Parse a time string into microseconds.
///
/// Accepts two formats:
/// - `"s.ssss"` — seconds with up to 4 decimal places (100 µs precision),
///   e.g. `"1.2500"` → 1_250_000 µs. Fewer decimals are zero-padded on the
///   right (`"1.25"` → `"1.2500"` → 1_250_000 µs). More than 4 decimals
///   are truncated (not rounded).
/// - `"N"` (integer, no dot) — legacy millisecond format for backward
///   compatibility, e.g. `"1250"` → 1_250_000 µs.
fn parse_time_to_us(s: &str) -> Option<u64> {
    if let Some((int_part, frac_part)) = s.split_once('.') {
        let secs: u64 = int_part.parse().ok()?;
        if frac_part.is_empty() {
            // "1." is treated as "1.0"
            return Some(secs * 1_000_000);
        }
        // Normalize fractional part to exactly 4 digits (100 µs units).
        // Truncate if longer, pad with trailing zeros if shorter.
        let frac_str = if frac_part.len() >= 4 {
            &frac_part[..4]
        } else {
            frac_part
        };
        let mut frac: u64 = frac_str.parse().ok()?;
        // Pad short fractions: "25" → "2500" (× 10^(4-len))
        for _ in frac_str.len()..4 {
            frac *= 10;
        }
        // frac is now in units of 100 µs (0..9999).
        Some(secs * 1_000_000 + frac * 100)
    } else {
        // Legacy format: plain integer = milliseconds.
        let ms: u64 = s.parse().ok()?;
        Some(ms * 1_000)
    }
}

/// Parse a single log line. Returns `None` if the line is malformed.
fn parse_line(line: &str) -> Option<LogEntry> {
    let mut parts = line.splitn(3, ' ');
    let time_str = parts.next()?;
    let dir_str = parts.next()?;
    let hex_str = parts.next()?;

    let us_offset = parse_time_to_us(time_str)?;
    let direction = match dir_str {
        "S2C" => PacketDirection::ServerToClient,
        "C2S" => PacketDirection::ClientToServer,
        _ => return None,
    };

    if hex_str.len() % 2 != 0 || hex_str.is_empty() {
        return None;
    }

    let data: Option<Vec<u8>> = (0..hex_str.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex_str[i..i + 2], 16).ok())
        .collect();

    Some(LogEntry {
        us_offset,
        direction,
        data: data?,
    })
}

// -- scan logs dir ------------------------------------------------------------

/// List all `.uolog` files in a directory, sorted by name (newest last with
/// the timestamp-based naming scheme).
pub fn scan_log_files(dir: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("uolog"))
        .collect();

    files.sort();
    Ok(files)
}
