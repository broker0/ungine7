//! Server list patching for the replay proxy.
//!
//! `patch_game_server_list` takes the original `0xA8 GameServerList` received
//! from the real login server and appends virtual replay entries — one per
//! `.uolog` file found in the logs directory.
//!
//! The real server entries keep their original indices untouched.
//! Replay entries receive consecutive indices starting right after the last
//! real index, so `SelectServer` can be forwarded to the real server as-is
//! for real entries, and handled locally for replay entries.

use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use packets::login::{GameServerEntry, GameServerList};
use packets::traits::BasicPacket;
use packets::u_io::RawBytes;

use crate::packet_log::scan_log_files;

/// Result of patching a `GameServerList`.
pub struct PatchedList {
    /// The patched packet bytes to send to the client.
    pub bytes: Bytes,
    /// Log file paths for replay entries.
    /// `log_files[i]` corresponds to server index `first_replay_index + i`.
    pub log_files: Vec<PathBuf>,
    /// The index of the first replay slot in the patched list.
    /// Indices `0 .. first_replay_index - 1` are real servers.
    pub first_replay_index: u16,
}

/// Patch a `GameServerList` received from the real server by appending
/// one virtual replay entry per `.uolog` file in `logs_dir`.
///
/// Real server entries are kept unchanged (original indices, original IPs).
/// Replay entries use `proxy_addr` as their IP so the client reconnects to us.
///
/// Returns `None` if `original_data` cannot be parsed as a `GameServerList`.
pub fn patch_game_server_list(
    original_data: &[u8],
    proxy_addr: SocketAddrV4,
    logs_dir: &Path,
) -> Option<PatchedList> {
    let original = GameServerList::from_bytes(original_data).ok()?;

    let log_files = scan_log_files(logs_dir).unwrap_or_default();

    // The first replay index follows the last real server index.
    let max_real_index = original.servers.iter().map(|e| e.index).max().unwrap_or(0);
    let first_replay_index = max_real_index + 1;

    // Build the merged entry list: original entries + replay entries.
    let mut entries: Vec<GameServerEntry> = original.servers.to_vec();

    for (i, path) in log_files.iter().enumerate() {
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("replay");
        let display = format!("[R] {label}");

        entries.push(GameServerEntry {
            index: first_replay_index + i as u16,
            name: make_name(&display),
            full_percent: 0,
            timezone: 0,
            ip: proxy_addr.ip().octets().into(),
        });
    }

    let patched = GameServerList::new(original.system_info_flag, entries);
    Some(PatchedList {
        bytes: patched.to_bytes(),
        log_files,
        first_replay_index,
    })
}

/// Build a replay-only `GameServerList` for offline mode.
///
/// Returns `None` if no `.uolog` files are found (and thus no entries to show).
pub fn build_replay_server_list(proxy_addr: SocketAddrV4, logs_dir: &Path) -> Option<PatchedList> {
    let log_files = scan_log_files(logs_dir).unwrap_or_default();
    if log_files.is_empty() {
        return None;
    }

    let mut entries = Vec::with_capacity(log_files.len());
    for (i, path) in log_files.iter().enumerate() {
        let label = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("replay");
        let display = format!("[R] {label}");

        entries.push(GameServerEntry {
            index: i as u16,
            name: make_name(&display),
            full_percent: 0,
            timezone: 0,
            ip: *proxy_addr.ip(),
        });
    }

    let list = GameServerList::new(0x5D, entries);
    Some(PatchedList {
        bytes: list.to_bytes(),
        log_files,
        first_replay_index: 0,
    })
}

/// Convert a display name into a 32-byte [`RawBytes`] field (null-padded ASCII).
fn make_name(name: &str) -> RawBytes<32> {
    let mut buf = [0u8; 32];
    for (dst, src) in buf.iter_mut().zip(name.bytes().take(31)) {
        *dst = src;
    }
    RawBytes(buf)
}
