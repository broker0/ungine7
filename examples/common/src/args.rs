//! Shared CLI argument groups for example binaries.
//!
//! These structs are designed for use with clap's `#[command(flatten)]` so
//! that every proxy example gets a consistent set of flags without
//! copy-pasting the definitions.
//!
//! # Usage
//!
//! ```ignore
//! use clap::Parser;
//! use common::args::{ProxyArgs, Socks5Args, VerbosityArgs};
//!
//! #[derive(Debug, Parser)]
//! struct Args {
//!     #[command(flatten)]
//!     proxy: ProxyArgs,
//!
//!     #[command(flatten)]
//!     socks5: Socks5Args,
//!
//!     #[command(flatten)]
//!     verbosity: VerbosityArgs,
//!
//!     // ... example-specific args ...
//! }
//! ```

use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;

use clap::Args;
use log::info;

use protocol::connector::ConnectorConfig;
use u_core::ProtocolVersion;

use crate::logging::{init_logger, LoggerBuilder};

// ── ProxyArgs ─────────────────────────────────────────────────────────────

/// Common proxy connection arguments.
#[derive(Debug, Args)]
pub struct ProxyArgs {
    /// Real UO server address to connect to.
    #[cfg_attr(feature = "default-server", arg(long, default_value = "127.0.0.1:2593"))]
    #[cfg_attr(not(feature = "default-server"), arg(long))]
    pub server: String,

    /// Client version to expect (format: major.minor.patch.build).
    #[cfg_attr(feature = "default-server", arg(long = "client-version", default_value = "3.0.8.0"))]
    #[cfg_attr(not(feature = "default-server"), arg(long = "client-version"))]
    pub client_version: ProtocolVersion,

    /// Accept encrypted client connections.
    ///
    /// Use `--encrypted=false` or `--encrypted false` for plain/unencrypted
    /// clients.
    #[cfg_attr(feature = "default-server",
        arg(long, default_value_t = true, action = clap::ArgAction::Set,
            num_args = 0..=1, default_missing_value = "true"))]
    #[cfg_attr(not(feature = "default-server"),
        arg(long, action = clap::ArgAction::Set,
            num_args = 0..=1, default_missing_value = "true", required = true))]
    pub encrypted: bool,

    /// Port to listen for UO client connections.
    #[arg(long, default_value_t = 2593)]
    pub proxy_port: u16,

    /// Public IPv4 address of this proxy, written into the 0x8C redirect
    /// packet that is sent to the UO client.
    ///
    /// Set this to the LAN/external IP when the proxy runs on a host other
    /// than the client machine.
    #[arg(long = "proxy-host", default_value = "127.0.0.1")]
    pub proxy_host: Ipv4Addr,
}

impl ProxyArgs {
    /// Build a `SocketAddrV4` from `proxy_host` and `proxy_port`.
    pub fn proxy_addr(&self) -> SocketAddrV4 {
        SocketAddrV4::new(self.proxy_host, self.proxy_port)
    }

    /// Log the proxy configuration at INFO level.
    pub fn log_info(&self) {
        info!("Server:     {}", self.server);
        info!("Version:    {}", self.client_version);
        info!("Encrypted:  {}", self.encrypted);
        info!("Proxy port: {}", self.proxy_port);
        info!("Proxy host: {}", self.proxy_host);
    }
}

// ── Socks5Args ────────────────────────────────────────────────────────────

/// SOCKS5 proxy configuration.
#[derive(Debug, Args)]
pub struct Socks5Args {
    /// SOCKS5 proxy address to route server connections through
    /// (e.g. 127.0.0.1:1080).
    #[arg(long)]
    pub socks5: Option<String>,

    /// Username for SOCKS5 authentication (requires --socks5).
    #[arg(long, requires = "socks5")]
    pub socks5_user: Option<String>,

    /// Password for SOCKS5 authentication (requires --socks5-user).
    #[arg(long, requires = "socks5_user")]
    pub socks5_pass: Option<String>,
}

impl Socks5Args {
    /// Convert into a [`ConnectorConfig`] and log the result.
    pub fn into_connector(self) -> ConnectorConfig {
        match self.socks5 {
            Some(proxy_addr) => {
                let auth = match (self.socks5_user, self.socks5_pass) {
                    (Some(user), Some(pass)) => Some((user, pass)),
                    _ => None,
                };
                info!(
                    "SOCKS5:     {} (auth: {})",
                    proxy_addr,
                    if auth.is_some() { "yes" } else { "no" }
                );
                ConnectorConfig::Socks5 { proxy_addr, auth }
            }
            None => {
                info!("SOCKS5:     disabled (direct TCP)");
                ConnectorConfig::Direct
            }
        }
    }
}

// ── VerbosityArgs ─────────────────────────────────────────────────────────

/// Verbosity control (`-v` / `-vv`).
#[derive(Debug, Args)]
pub struct VerbosityArgs {
    /// Increase log verbosity (`-v`: debug all targets, `-vv`: trace all
    /// targets).
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

impl VerbosityArgs {
    /// Create a [`LoggerBuilder`] with the verbosity applied.
    ///
    /// `own_targets` are the crate-local log targets that should be at
    /// `Debug` level even when the global level is `Warn` (the base
    /// level).
    ///
    /// ```ignore
    /// args.verbosity
    ///     .logger(["replay_proxy"])
    ///     .build()?;
    /// ```
    pub fn logger<I, S>(&self, own_targets: I) -> LoggerBuilder
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut logger = init_logger()
            .level(log::LevelFilter::Warn)
            .targets(own_targets, log::LevelFilter::Debug);
        match self.verbose {
            0 => {}
            1 => { logger = logger.level(log::LevelFilter::Debug); }
            _ => { logger = logger.verbose(); }
        }
        logger
    }
}

// ── DataDirArgs ───────────────────────────────────────────────────────────

/// UO client data directory arguments.
///
/// Provides `--data-dir` (default: current directory) and `--no-data` to
/// skip loading entirely.
#[derive(Debug, Args)]
pub struct DataDirArgs {
    /// UO client data directory containing MUL/IDX files.
    ///
    /// Defaults to the current working directory (`.`).
    #[arg(long, default_value = ".")]
    pub data_dir: PathBuf,

    /// Disable loading world data files even if they exist in --data-dir.
    #[arg(long)]
    pub no_data: bool,
}

impl DataDirArgs {
    /// Returns the data directory path, or `None` if `--no-data` was given.
    pub fn path(&self) -> Option<&PathBuf> {
        if self.no_data { None } else { Some(&self.data_dir) }
    }
}
