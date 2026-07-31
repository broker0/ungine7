use std::sync::Arc;
use anyhow::Result;

use common::uo_engine::StaticData;
use common::uo_engine::serial_alloc::SerialAllocator;
use framework::ecumene::StaticWorldData;
use files::radarcol::RadarColors;
use crate::config::Args;

use crate::worker::PathServerWorkerTx;

pub struct AppState {
    /// Shared static world data (immutable after load).
    pub static_data: Arc<StaticData>,

    /// Radar color table for PNG rendering (optional — only when data dir given).
    pub radar_colors: Option<Arc<RadarColors>>,

    /// Data directory for save/load operations.
    pub data_dir: Option<std::path::PathBuf>,

    /// Channel to the UO worker (for game sessions and in-process client).
    pub worker_tx: PathServerWorkerTx,

    /// Event sender for publishing world events (e.g. Speech from sessions).
    pub event_tx: tokio::sync::mpsc::UnboundedSender<framework::continuum::WorldEvent>,

    /// Centralised serial allocator shared with the worker handler.
    ///
    /// The same `Arc` is handed to `PathServerHandler`/`BaseHandler` in
    /// `main`, so the worker allocates from one source.  Kept on `AppState`
    /// so HTTP/session layers can allocate too if needed in future; player
    /// serials themselves are deterministic (see `handle_spawn`).
    #[allow(dead_code)]
    pub serial_alloc: Arc<SerialAllocator>,
}

impl AppState {
    pub async fn new(
        args: &Args,
        worker_tx: PathServerWorkerTx,
        event_tx: tokio::sync::mpsc::UnboundedSender<framework::continuum::WorldEvent>,
        serial_alloc: Arc<SerialAllocator>,
    ) -> Result<Self> {
        let data_path = args.data_path();

        // Load static world data (tiledata, maps, statics)
        let (static_world, radar_colors) = match data_path {
            Some(dir) => {
                let static_world = match StaticWorldData::load(dir) {
                    Ok(data) => {
                        log::info!("Loaded world data from {}", dir.display());
                        Some(Arc::new(data))
                    }
                    Err(e) => {
                        log::warn!("Failed to load world data: {}. Using empty.", e);
                        None
                    }
                };
                let radar_colors = match RadarColors::read(dir) {
                    Ok(rc) => {
                        log::info!("Loaded RadarColors ({} entries)", rc.len());
                        Some(Arc::new(rc))
                    }
                    Err(e) => {
                        log::warn!("RadarColors not loaded: {e}");
                        None
                    }
                };
                (static_world, radar_colors)
            }
            None => (None, None),
        };

        let static_data = Arc::new(StaticData(static_world));

        Ok(Self {
            static_data,
            radar_colors,
            data_dir: data_path.cloned(),
            worker_tx,
            event_tx,
            serial_alloc,
        })
    }
}
