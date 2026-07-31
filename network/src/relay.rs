use log::{debug, error};
use tokio::sync::mpsc;
use u_core::PacketDirection;

use crate::error;
use crate::logs;
use crate::session::{RecvResult, Session, SessionEvent};

/// Commands that can be sent to a relay via the control channel.
#[derive(Debug)]
pub enum ConnectionControl {
    Disconnect,
}

/// Bidirectional packet relay between client and server sessions.
pub async fn relay(
    tag: &str,
    client: &mut Session,
    server: &mut Session,
    control: Option<mpsc::Receiver<ConnectionControl>>,
) -> error::Result<()> {
    let result = match control {
        Some(rx) => relay_loop_controlled(tag, client, server, rx).await,
        None => relay_loop(tag, client, server).await,
    };

    client.close().await;
    server.close().await;

    result
}

async fn relay_loop(
    tag: &str,
    client: &mut Session,
    server: &mut Session,
) -> error::Result<()> {
    loop {
        tokio::select! {
            recv = client.recv() => if handle_recv(tag, recv, client, server, PacketDirection::ClientToServer).await? { break; },
            recv = server.recv() => if handle_recv(tag, recv, server, client, PacketDirection::ServerToClient).await? { break; },
        }
    }
    Ok(())
}

/// Relay loop with external control channel.
async fn relay_loop_controlled(
    tag: &str,
    client: &mut Session,
    server: &mut Session,
    mut control_rx: mpsc::Receiver<ConnectionControl>,
) -> error::Result<()> {
    loop {
        tokio::select! {
            recv = client.recv() => if handle_recv(tag, recv, client, server, PacketDirection::ClientToServer).await? { break; },
            recv = server.recv() => if handle_recv(tag, recv, server, client, PacketDirection::ServerToClient).await? { break; },
            cmd = control_rx.recv() => match cmd {
                Some(ConnectionControl::Disconnect) => {
                    debug!(target: logs::RELAY, "{tag} disconnected by control channel");
                    break;
                }
                None => debug!(target: logs::RELAY, "{tag} control channel closed"),
            },
        }
    }
    Ok(())
}


/// Handle a recv result: send replies back to source, forward packet to target.
/// Returns `true` if the loop should break.
async fn handle_recv(
    tag: &str,
    recv: RecvResult,
    source: &mut Session,
    target: &mut Session,
    dir: PacketDirection,
) -> error::Result<bool> {
    let side = match dir {
        PacketDirection::ClientToServer => "client",
        PacketDirection::ServerToClient => "server",
    };

    let RecvResult { event, replies } = recv;

    // Send reply packets back to the source session.
    for reply in replies {
        source.send(reply).await?;
    }

    match event {
        SessionEvent::Seed(s) => {
            target.send_seed(s).await?;
            Ok(false)
        }
        SessionEvent::Packet(p) => {
            target.send(p).await?;
            Ok(false)
        }
        SessionEvent::Stopped => {
            debug!(target: logs::RELAY, "{tag} {side} stopped by handler");
            Ok(true)
        }
        SessionEvent::Disconnected => {
            debug!(target: logs::RELAY, "{tag} {side} disconnected");
            Ok(true)
        }
        SessionEvent::Error(e) => {
            error!(target: logs::RELAY, "{tag} {side} error: {e}");
            Ok(true)
        }
    }
}
