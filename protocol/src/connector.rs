//! TCP connection abstraction with optional proxy support.
//!
//! Provides a unified [`ConnectorConfig`] enum that determines how outgoing
//! TCP connections are established — either directly or through a proxy.
//!
//! Proxy variants (e.g. SOCKS5) are only available when the `proxy`
//! cargo feature is enabled.

use std::io;

use tokio::net::TcpStream;
#[cfg(feature = "proxy")]
use tokio_socks::tcp::Socks5Stream;


/// Connection configuration — determines how outgoing connections are established.
#[derive(Debug, Clone, Default)]
pub enum ConnectorConfig {
    /// Direct TCP connection (no proxy).
    #[default]
    Direct,

    /// SOCKS5 proxy (RFC 1928, RFC 1929 for auth).
    #[cfg(feature = "proxy")]
    Socks5 {
        /// Proxy address in `"host:port"` format (e.g. `"127.0.0.1:1080"`).
        proxy_addr: String,
        /// Optional username/password authentication credentials.
        auth: Option<(String, String)>,
    },
    // Future variants:
    // #[cfg(feature = "proxy")]
    // HttpConnect { ... },
}


/// Establish a TCP connection to `addr` using the given connector configuration.
///
/// - [`ConnectorConfig::Direct`] — connects via [`TcpStream::connect`].
/// - [`ConnectorConfig::Socks5`] — tunnels through a SOCKS5 proxy (requires `proxy` feature).
///
/// `addr` should be in `"host:port"` format (e.g. `"127.0.0.1:2593"`).
///
/// Returns a [`TcpStream`] ready for normal I/O.
pub async fn connect(
    config: &ConnectorConfig,
    addr: &str,
) -> io::Result<TcpStream> {
    match config {
        ConnectorConfig::Direct => {
            TcpStream::connect(addr).await
        }
        #[cfg(feature = "proxy")]
        ConnectorConfig::Socks5 { proxy_addr, auth } => {
            let stream = match auth {
                Some((username, password)) => {
                    Socks5Stream::connect_with_password(proxy_addr.as_str(), addr, username, password).await
                }
                None => {
                    Socks5Stream::connect(proxy_addr.as_str(), addr).await
                }
            };
            stream
                .map(|s| s.into_inner())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
    }
}
