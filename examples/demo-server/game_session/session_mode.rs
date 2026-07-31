//! Runtime-selectable session mode.
//!
//! Replaces the old mutually-exclusive compile-time features
//! (`rust-session` / `lua-session` / `controller-session`).  The mode is
//! chosen per-session when a client connects, based on the server's current
//! default (see [`WorldData::default_session_mode`](crate::WorldData)).  An
//! administrator can change that default at runtime with the `.session`
//! dot-command; the new mode applies to subsequently-connecting sessions
//! only — already-running sessions keep their mode until they reconnect.
//!
//! The `Rust` mode is always available.  The `Lua` and `Controller` modes
//! require the crate to be built with the `lua` feature; in a build without
//! `lua` they do not exist (and `.session lua` / `.session controller`
//! report that the mode is unavailable).

use std::fmt;
use std::str::FromStr;

/// Which game-logic handler a session runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum SessionMode {
    /// All game logic implemented in Rust sub-modules.  Always available.
    #[default]
    Rust,
    /// Game logic lives in a per-session Lua VM.  Requires `lua` feature.
    #[cfg(feature = "lua")]
    Lua,
    /// Game logic driven by a Lua controller running in the worker tick.
    /// Requires `lua` feature.
    #[cfg(feature = "lua")]
    Controller,
}

impl SessionMode {
    /// Stable wire/storage encoding (used by the atomic default in
    /// `WorldData`).  Round-trips with [`from_u8`](Self::from_u8).
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            SessionMode::Rust => 0,
            #[cfg(feature = "lua")]
            SessionMode::Lua => 1,
            #[cfg(feature = "lua")]
            SessionMode::Controller => 2,
        }
    }

    /// Decode from [`as_u8`](Self::as_u8).  Unknown / unavailable values
    /// fall back to [`SessionMode::Rust`].
    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            #[cfg(feature = "lua")]
            1 => SessionMode::Lua,
            #[cfg(feature = "lua")]
            2 => SessionMode::Controller,
            _ => SessionMode::Rust,
        }
    }

    /// Lower-case canonical name, as accepted by `.session` and `--session-mode`.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SessionMode::Rust => "rust",
            #[cfg(feature = "lua")]
            SessionMode::Lua => "lua",
            #[cfg(feature = "lua")]
            SessionMode::Controller => "controller",
        }
    }
}

impl fmt::Display for SessionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a session mode from a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseSessionModeError {
    /// The name is not a known mode at all.
    Unknown(String),
    /// The name is a valid mode, but unavailable in this build (no `lua`).
    // Only constructed in builds without the `lua` feature.
    #[allow(dead_code)]
    Unavailable(String),
}

impl fmt::Display for ParseSessionModeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseSessionModeError::Unknown(s) => {
                write!(f, "unknown session mode {:?} (expected rust|lua|controller)", s)
            }
            ParseSessionModeError::Unavailable(s) => {
                write!(f, "session mode {:?} is unavailable in this build (rebuild with the `lua` feature)", s)
            }
        }
    }
}

impl std::error::Error for ParseSessionModeError {}

impl FromStr for SessionMode {
    type Err = ParseSessionModeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "rust" => Ok(SessionMode::Rust),
            "lua" => {
                #[cfg(feature = "lua")]
                {
                    Ok(SessionMode::Lua)
                }
                #[cfg(not(feature = "lua"))]
                {
                    Err(ParseSessionModeError::Unavailable("lua".to_string()))
                }
            }
            "controller" => {
                #[cfg(feature = "lua")]
                {
                    Ok(SessionMode::Controller)
                }
                #[cfg(not(feature = "lua"))]
                {
                    Err(ParseSessionModeError::Unavailable("controller".to_string()))
                }
            }
            other => Err(ParseSessionModeError::Unknown(other.to_string())),
        }
    }
}
