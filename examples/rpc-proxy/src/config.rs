use std::path::PathBuf;

use clap::Parser;
use common::args::{DataDirArgs, ProxyArgs, VerbosityArgs};
use u_core::ProtocolVersion;

#[derive(Parser, Debug)]
#[command(name = "rpc-proxy", about = "UO Managed Proxy with RPC control via WebSocket")]
pub struct Config {
    #[command(flatten)]
    pub proxy: ProxyArgs,

    /// Port to listen for Mirror UO client connections.
    #[arg(long, default_value_t = 2594)]
    pub mirror_port: u16,

    /// HTTP + WebSocket API port.
    #[arg(long, default_value_t = 8080)]
    pub http_port: u16,

    /// Serve HTML from this directory instead of the embedded copy.
    /// Changes take effect on every page reload — useful during UI development.
    #[arg(long, value_name = "DIR")]
    pub dev_html: Option<PathBuf>,

    #[command(flatten)]
    pub verbosity: VerbosityArgs,

    #[command(flatten)]
    pub data: DataDirArgs,

    /// Lua script to run automatically when the first session is created.
    #[cfg(feature = "lua")]
    #[arg(long, value_name = "PATH")]
    pub lua_script: Option<PathBuf>,

    /// WebSocket URL of a path-server mirror endpoint to stream S2C packets to.
    ///
    /// When set, every new Source session will open an outgoing WebSocket
    /// connection to this URL and forward all server-to-client packets as
    /// binary frames.  Example: `ws://127.0.0.1:3000/ws/mirror`
    #[arg(long, value_name = "URL")]
    pub mirror_url: Option<String>,
}

impl Config {
    /// Build the `with_allowed` list for [`network::listener::ListenerConfig`].
    pub fn allowed(&self) -> Vec<(Option<ProtocolVersion>, Option<bool>)> {
        vec![(Some(self.proxy.client_version), Some(self.proxy.encrypted))]
    }
}
