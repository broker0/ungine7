//! Configuration — CLI arguments + hardcoded defaults.
//!
//! CLI arguments follow the same conventions as other examples in the
//! workspace (`--server`, `--client-version`, `--encrypted`, `--data-dir`).

use clap::Parser;

use common::args::DataDirArgs;
use u_core::ProtocolVersion;

// ── Constants that are not (yet) CLI arguments ────────────────────────────

/// Default account name (pre-filled in login form).
pub const ACCOUNT: &str = "test";
/// Default password (pre-filled in login form).
pub const PASSWORD: &str = "test";
/// Login seed sent in the initial handshake.
pub const LOGIN_SEED: u32 = 0xDEAD_BEEF;
/// Server shard index to select after authentication.
pub const SERVER_INDEX: u16 = 1;

// ── CLI arguments ─────────────────────────────────────────────────────────

#[derive(Debug, Parser)]
#[command(name = "text-client", about = "Minimalist TUI client for Ultima Online")]
pub struct Args {
    /// Server address to connect to (host:port).
    #[arg(long, default_value = "127.0.0.1:2593")]
    pub server: String,

    /// Client version to negotiate (format: major.minor.patch.build).
    #[arg(long = "client-version", default_value = "3.0.8.0")]
    pub client_version: ProtocolVersion,

    /// Use encrypted (official) protocol.
    ///
    /// Use `--encrypted=false` or `--encrypted false` for plain/unencrypted
    /// servers.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set,
          num_args = 0..=1, default_missing_value = "true")]
    pub encrypted: bool,

    /// UO client data directory.
    #[command(flatten)]
    pub data: DataDirArgs,
}
