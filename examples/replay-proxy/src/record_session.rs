//! Record session — transparent proxy with packet logging.
//!
//! When a client selects the real server from the list, this module takes over
//! the game phase.  It acts as a plain relay between the client and the real
//! game server, logging **all** packets (both directions) to a `.uolog` file.
//!
//! The log file is created when the session starts and flushed/closed when the
//! session ends (either side disconnects).

use std::collections::HashSet;
use std::path::PathBuf;

use log::{error, info};
use u_core::PacketDirection;
use network::error as fw_error;
use network::session::{Session, SessionEvent};
use packets::login::GameLogin;
use packets::system::GeneralInfo;
use packets::traits::{ManualPacket, BasicPacket};
use protocol::RawPacket;

use crate::packet_log::{LogWriter, log_file_name};

/// Run the record session loop.
///
/// `client` — session with the UO client (already in game phase).
/// `server` — session with the real game server (already in game phase).
/// `logs_dir` — directory where the `.uolog` file will be created.
///
/// The log file is **not** created immediately — we wait for the first
/// `0x91 GameLogin` packet so we can extract the account name and include
/// it in the filename.
pub async fn run(
    mut client: Session,
    mut server: Session,
    logs_dir: PathBuf,
) -> fw_error::Result<()> {
    let mut writer: Option<LogWriter> = None;
    let mut log_path: Option<PathBuf> = None;
    let mut requested_houses: HashSet<u32> = HashSet::new();

    let result: fw_error::Result<()> = async {
        loop {
            tokio::select! {
                event = client.recv() => {
                    match event.event {
                        SessionEvent::Packet(p) => {
                            // 0x91 GameLogin — extract account, create log file,
                            // and strip credentials before writing.
                            if p.id() == GameLogin::ID {
                                if let Ok(g) = GameLogin::from_bytes(&p.data) {
                                    // Create log file on first 0x91 — uses account for the filename.
                                    if writer.is_none() {
                                        let account: &str = &g.account;
                                        let file_name = log_file_name(account);
                                        let path = logs_dir.join(&file_name);
                                        match LogWriter::create(&path) {
                                            Ok(w) => {
                                                info!("[record] logging to {}", path.display());
                                                writer = Some(w);
                                                log_path = Some(path);
                                            }
                                            Err(e) => {
                                                error!("[record] failed to create log file {}: {e}", path.display());
                                            }
                                        }
                                    }

                                    // Write a blank 0x91 (credentials stripped).
                                    if let Some(ref mut w) = writer {
                                        let blank = GameLogin::new(g.auth_key, "", "");
                                        if let Err(e) = w.write_packet(PacketDirection::ClientToServer, &blank.to_bytes()) {
                                            error!("[record] write error (C2S 0x91): {e}");
                                        }
                                    }
                                }
                            } else {
                                // Log all other C→S packets as-is.
                                if let Some(ref mut w) = writer {
                                    if let Err(e) = w.write_packet(PacketDirection::ClientToServer, &p.data) {
                                        error!("[record] write error (C2S 0x{:02X}): {e}", p.id());
                                    }
                                }
                            }
                            server.send(p).await?;
                        }
                        SessionEvent::Seed(data) => {
                            server.send_seed(data).await?;
                        }
                        SessionEvent::Stopped | SessionEvent::Disconnected => break,
                        SessionEvent::Error(e) => return Err(e.into()),
                    }
                }
                event = server.recv() => {
                    match event.event {
                        SessionEvent::Packet(p) => {
                            // Log all S→C packets before forwarding.
                            if let Some(ref mut w) = writer {
                                if let Err(e) = w.write_packet(PacketDirection::ServerToClient, &p.data) {
                                    error!("[record] write error (S2C 0x{:02X}): {e}", p.id());
                                }
                            }

                            // When the server tells the client about a custom
                            // house revision, proactively request the full
                            // design so it gets recorded in the log.  The
                            // client may already have the house cached and
                            // would never send the request itself.
                            if p.id() == 0xBF && p.data.len() >= 13 {
                                let sub = u16::from_be_bytes([p.data[3], p.data[4]]);
                                if sub == 0x001D {
                                    let serial = u32::from_be_bytes([
                                        p.data[5], p.data[6], p.data[7], p.data[8],
                                    ]);
                                    if requested_houses.insert(serial) {
                                        let req = GeneralInfo::RequestHouseState { house_serial: serial };
                                        server.send(RawPacket::c2s(req.to_bytes())).await?;
                                        info!("[record] requesting house design for serial={:#010X}", serial);
                                    }
                                }
                            }

                            client.send(p).await?;
                        }
                        SessionEvent::Seed(data) => {
                            client.send_seed(data).await?;
                        }
                        SessionEvent::Stopped | SessionEvent::Disconnected => break,
                        SessionEvent::Error(e) => return Err(e.into()),
                    }
                }
            }
        }
        Ok(())
    }.await;

    client.close().await;
    server.close().await;

    if let Some(w) = writer {
        if let Err(e) = w.finish() {
            error!("[record] failed to flush log: {e}");
        } else if let Some(path) = log_path {
            info!("[record] log saved: {}", path.display());
        }
    }

    result
}
