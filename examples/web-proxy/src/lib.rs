//! UO proxy with a web-based packet inspector — core library.
//!
//! This crate provides the proxy logic, session registry, packet observer,
//! and an embedded Axum web server for real-time inspection.
//!
//! Two binary front-ends consume this library:
//!   - `web-proxy-cli` — classic CLI runner (clap + Ctrl-C shutdown)
//!   - `web-proxy-gui` — eframe/egui configurator (feature `gui`)

pub mod instance_manager;
pub mod logging_stream;
pub mod packet_observer;
pub mod session_registry;
pub mod web;
mod proxy;

pub use proxy::{ProxyConfig, run_proxy};
