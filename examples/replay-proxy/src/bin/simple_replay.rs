//! Simple replay — a minimal UO game server that plays back a single `.uolog`
//! file to a connecting client.
//!
//! Unlike the full `replay-proxy`, this binary:
//!
//! - Does **not** act as a login proxy — the client connects directly to the
//!   game port (no server-list selection step).
//! - Does **not** support recording.
//! - Does **not** have dot-commands or the action-menu gump.
//! - Does **not** support free-move after playback ends.
//! - Only tracks the player Z coordinate for collision (no full continuum worker).
//!
//! It is intended as the simplest possible host for playback — useful for
//! automated tooling, testing, or embedding.
//!
//! # Usage
//!
//! ```
//! simple-replay --log session.uolog --port 2593
//! ```

// TODO: implement using:
//   - replay_proxy::packet_log::read_log
//   - replay_proxy::replay_session::run  (or a stripped-down variant)
//   - replay_proxy::ecumene::StaticWorldData  (optional, for Z tracking only)

fn main() {
    eprintln!("simple-replay: not yet implemented");
    std::process::exit(1);
}
