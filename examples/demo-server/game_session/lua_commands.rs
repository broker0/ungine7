//! Lua dot-command handling and gump menu (`#[cfg(feature = "lua")]`).

use protocol::RawPacket;
use packets::traits::ManualPacket;

use network::error;
use network::session::Session;

use common::dot_commands::{extract_speech_text, system_message_packet, CMD_GUMP_BASE};

/// Gump ID for the Lua script action menu.
const LUA_MENU_GUMP_ID: u32 = CMD_GUMP_BASE | 0x800;

/// Check if a C→S packet is a dot-command or a gump response, and handle it.
///
/// Returns `true` if the packet was consumed (caller should `continue`),
/// `false` if it should be processed normally.
pub(super) async fn handle_lua_dot_command(
    packet: &RawPacket,
    session: &mut Session,
    lua_cmd_tx: &tokio::sync::mpsc::Sender<crate::lua_script::LuaCommand>,
    scripts_dir: &std::path::Path,
) -> error::Result<bool> {
    use packets::gump::GumpMenuSelection;
    use packets::traits::ManualPacket as _;

    // ── GumpMenuSelection (0xB1) ──────────────────────────────────────
    if packet.id() == GumpMenuSelection::ID {
        if let Ok(resp) = GumpMenuSelection::from_bytes(&packet.data) {
            if resp.gump_id == LUA_MENU_GUMP_ID {
                handle_lua_gump_response(&resp, session, lua_cmd_tx, scripts_dir).await?;
                return Ok(true);
            }
        }
        return Ok(false);
    }

    // ── Speech-based dot-commands ─────────────────────────────────────
    let text = match extract_speech_text(packet) {
        Some(t) => t,
        None => return Ok(false),
    };

    let text = text.trim();
    if !text.starts_with('.') {
        return Ok(false);
    }

    // .menu or .lua (without args) — show gump
    if text == ".menu" || text == ".lua" {
        send_lua_menu_gump(session).await?;
        return Ok(true);
    }

    // .lua <path> | .lua reload | .lua stop
    if let Some(rest) = text.strip_prefix(".lua ") {
        let arg = rest.trim();
        let (cmd, reply_msg) = match arg {
            "reload" => (
                crate::lua_script::LuaCommand::Reload,
                "[lua] reloading script",
            ),
            "stop" => (
                crate::lua_script::LuaCommand::Stop,
                "[lua] script stopped",
            ),
            path => (
                crate::lua_script::LuaCommand::RunFile(std::path::PathBuf::from(path)),
                "[lua] loading script",
            ),
        };

        if lua_cmd_tx.send(cmd).await.is_err() {
            session.send(system_message_packet("[lua] script manager unavailable")).await?;
        } else {
            session.send(system_message_packet(reply_msg)).await?;
        }

        return Ok(true);
    }

    // Unknown dot-command — let it pass through (could be speech).
    Ok(false)
}

/// Send the Lua script action menu gump to the client.
async fn send_lua_menu_gump(session: &mut Session) -> error::Result<()> {
    use packets::gump::{GumpTextLine, SendGumpDialog};

    let commands = "\
        { page 0 }{ noclose }{ nodispose }\
        { resizepic 0 0 2620 260 265 }\
        { text 10 10 2100 0 }\
        { button 10  40 2117 2118 1 0 1 }{ text 45  40 2100 1 }\
        { button 10  70 2117 2118 1 0 2 }{ text 45  70 2100 2 }\
        { button 10 100 2117 2118 1 0 3 }{ text 45 100 2100 3 }\
        { button 10 130 2117 2118 1 0 4 }{ text 45 130 2100 4 }\
        { button 10 160 2117 2118 1 0 5 }{ text 45 160 2100 5 }\
        { button 10 195 2117 2118 1 0 6 }{ text 45 195 2100 6 }\
        { button 10 225 2117 2118 1 0 7 }{ text 45 225 2100 7 }";

    let text_lines = &[
        GumpTextLine("Lua Scripts".to_string()),                // 0 — title
        GumpTextLine("Run wander.lua".to_string()),             // 1
        GumpTextLine("Run custom script...".to_string()),       // 2
        GumpTextLine("Reload script".to_string()),              // 3
        GumpTextLine("Stop script".to_string()),                // 4
        GumpTextLine("Where am I?".to_string()),                // 5
        GumpTextLine("Entity info".to_string()),                // 6
        GumpTextLine("Close".to_string()),                      // 7
    ];

    let dialog = SendGumpDialog {
        serial: 0,
        gump_id: LUA_MENU_GUMP_ID,
        x: 0,
        y: 0,
        layout: commands.to_string(),
        text_lines: text_lines.to_vec(),
        trailing_pad: vec![],
    };
    session
        .send(RawPacket::s2c(dialog.to_bytes()))
        .await?;
    Ok(())
}

/// Handle a gump response for the Lua menu.
async fn handle_lua_gump_response(
    resp: &packets::gump::GumpMenuSelection,
    session: &mut Session,
    lua_cmd_tx: &tokio::sync::mpsc::Sender<crate::lua_script::LuaCommand>,
    scripts_dir: &std::path::Path,
) -> error::Result<()> {
    match resp.button_id {
        0 | 7 => {
            // Close — no action.
        }
        1 => {
            // Run wander.lua
            let cmd = crate::lua_script::LuaCommand::RunFile(
                scripts_dir.join("wander.lua"),
            );
            if lua_cmd_tx.send(cmd).await.is_ok() {
                session.send(system_message_packet("[lua] loading wander.lua")).await?;
            }
            send_lua_menu_gump(session).await?;
        }
        2 => {
            // Run custom script — prompt via system message.
            session.send(system_message_packet(
                "[lua] type: .lua <path>  (e.g. .lua scripts/patrol.lua)"
            )).await?;
        }
        3 => {
            // Reload
            if lua_cmd_tx.send(crate::lua_script::LuaCommand::Reload).await.is_ok() {
                session.send(system_message_packet("[lua] reloading script")).await?;
            }
            send_lua_menu_gump(session).await?;
        }
        4 => {
            // Stop
            if lua_cmd_tx.send(crate::lua_script::LuaCommand::Stop).await.is_ok() {
                session.send(system_message_packet("[lua] script stopped")).await?;
            }
            send_lua_menu_gump(session).await?;
        }
        5 => {
            // Where am I — show player position (placeholder).
            session.send(system_message_packet("[info] use .where for position")).await?;
            send_lua_menu_gump(session).await?;
        }
        6 => {
            // Entity info — placeholder.
            session.send(system_message_packet("[info] use .lua <serial> for entity info")).await?;
            send_lua_menu_gump(session).await?;
        }
        _ => {}
    }
    Ok(())
}
