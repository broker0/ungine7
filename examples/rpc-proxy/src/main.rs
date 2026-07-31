use std::sync::Arc;

use clap::Parser;
use log::{info, warn};

use rpc_proxy::config::Config;
use rpc_proxy::registry::SessionRegistry;
use rpc_proxy::rpc;
use rpc_proxy::session;
use framework::ecumene::StaticWorldData;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config: Config = Config::parse();
    config.verbosity.logger(["rpc_proxy", "common"]).build()?;

    info!("=== rpc-proxy starting ===");
    config.proxy.log_info();
    info!("Mirror port:  {}", config.mirror_port);
    info!("HTTP port:    {}", config.http_port);
    match &config.mirror_url {
        Some(url) => info!("Mirror URL:   {}", url),
        None => info!("Mirror URL:   disabled"),
    }

    // ── Load static world data (optional) ────────────────────────────────
    let static_data = match config.data.path() {
        Some(dir) => match StaticWorldData::load(dir) {
            Ok(sd) => {
                info!("World data: loaded from {}", dir.display());
                Some(Arc::new(sd) as Arc<dyn framework::ecumene::StaticDataProvider>)
            }
            Err(e) => {
                warn!("World data: failed to load from {}: {e}", dir.display());
                warn!("            Z values will not be resolved — use --data-dir");
                None
            }
        },
        None => {
            info!("World data: disabled (--no-data)");
            None
        }
    };

    let data_dir = config.data.path().cloned();
    let mirror_url = config.mirror_url.clone();
    let registry = SessionRegistry::new(static_data, config.proxy.client_version, data_dir, mirror_url).shared();

    tokio::select! {
        res = session::start_listeners(&config, registry.clone()) => {
            if let Err(e) = res {
                log::error!("Listener error: {}", e);
            }
        }
        res = rpc::start_http(&config, registry) => {
            if let Err(e) = res {
                log::error!("HTTP server error: {}", e);
            }
        }
    }

    Ok(())
}
