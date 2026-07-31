/// Logging targets for the `network` crate.
///
/// Each sub-module has its own target so that noisy per-packet logging
/// can be disabled independently of higher-level lifecycle messages.
///
/// # Examples
///
/// Show only listener lifecycle and client flow, silence per-packet noise:
/// ```text
/// network::listener=info,network::client=debug
/// ```

/// Root target — used for crate-level messages (rarely used directly).
pub const ROOT: &str = "network";

/// Listener lifecycle: bind, accept, shutdown, version checks.
pub const LISTENER: &str = "network::listener";

/// Per-packet session recv/send logging.
pub const SESSION: &str = "network::session";

/// Handler chain processing: drop / replace / stop decisions.
pub const HANDLER: &str = "network::handler";

/// Built-in filter handlers (`LogHandler`, `SubcommandFilter`).
pub const FILTER: &str = "network::filter";

/// `RedirectHandler`: 0x8C interception and rewrite.
pub const REDIRECT: &str = "network::redirect";

/// Bidirectional relay (`relay::relay`).
pub const RELAY: &str = "network::relay";

/// High-level client (`PacketClient`, `LoginConnection`, `GameConnection`).
pub const CLIENT: &str = "network::client";

/// Proxy utilities (`proxy::connect_upstream`).
pub const PROXY: &str = "network::proxy";
