use clap::Parser;
use std::path::PathBuf;

use common::args::VerbosityArgs;
use common::args::DataDirArgs;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Args {
    #[command(flatten)]
    pub verbosity: VerbosityArgs,

    #[command(flatten)]
    pub data: DataDirArgs,

    /// HTTP port for the JSON/PNG API and the `/ws/mirror` WebSocket
    /// (raw S2C UO packet ingest into the shadow world)
    #[arg(long, alias = "port", default_value_t = 8080)]
    pub web_port: u16,

    /// TCP port for UO client connections (0 = disabled)
    #[arg(long, default_value_t = 2593)]
    pub uo_port: u16,
}

impl Args {
    pub fn data_path(&self) -> Option<&PathBuf> {
        self.data.path()
    }
}
