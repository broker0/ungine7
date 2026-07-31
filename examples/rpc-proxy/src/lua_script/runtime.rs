//! Proxy-specific Lua bindings: `World` userdata, RPC helpers.
//!
//! The shared runtime (globals, event handling, script lifecycle) is
//! provided by [`framework::mitos`].  This module defines only the
//! proxy-specific `World` object and its methods.

use mlua::prelude::*;
use tokio::sync::{mpsc, oneshot};

use crate::rpc::protocol::{EquippedItemEntry, WorldItemData, WorldMobileData};
use crate::session::commands::ClientCommand;
use crate::types::FullSessionState;
use framework::diorama::ObserverEvent;

// ── ScriptingBackend implementation ───────────────────────────────────────

/// Proxy scripting backend — connects Lua scripts to the headless session.
#[derive(Clone)]
pub(crate) struct ProxyBackend {
    pub command_tx: mpsc::Sender<ClientCommand>,
}

impl framework::mitos::ScriptingBackend for ProxyBackend {
    type Event = ObserverEvent;

    fn event_to_lua(&self, lua: &Lua, event: &Self::Event) -> LuaResult<LuaValue> {
        event.clone().into_lua(lua)
    }

    fn create_world_constructor(&self, lua: &Lua) -> LuaResult<LuaFunction> {
        let tx = self.command_tx.clone();
        lua.create_function(move |_, ()| {
            Ok(LuaProxyWorld { command_tx: tx.clone() })
        })
    }

    fn log_prefix(&self) -> &str {
        "lua"
    }
}

// ── World userdata ────────────────────────────────────────────────────────

/// Lua userdata wrapping a clone of the session's command channel.
///
/// All RPC methods on `World` send [`ClientCommand`]s through this channel
/// and await the oneshot reply, exactly like the WebSocket handler does.
#[derive(Clone)]
struct LuaProxyWorld {
    command_tx: mpsc::Sender<ClientCommand>,
}

impl LuaUserData for LuaProxyWorld {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        // ── get_state() → table ───────────────────────────────────────
        methods.add_async_method("get_state", |lua, this, ()| async move {
            let state = rpc_get_state(&this.command_tx).await;
            state_to_lua(&lua, &state)
        });

        // ── get_items() → table of item tables ────────────────────────
        methods.add_async_method("get_items", |lua, this, ()| async move {
            let items: Vec<WorldItemData> = rpc_get_items(&this.command_tx).await;
            items.into_lua(&lua)
        });

        // ── get_mobiles() → table of mobile tables ────────────────────
        methods.add_async_method("get_mobiles", |lua, this, ()| async move {
            let mobiles: Vec<WorldMobileData> = rpc_get_mobiles(&this.command_tx).await;
            mobiles.into_lua(&lua)
        });

        // ── get_mobile(serial) → table | nil ──────────────────────────
        methods.add_async_method("get_mobile", |lua, this, serial: u32| async move {
            let mob: Option<WorldMobileData> = rpc_get_mobile(&this.command_tx, serial).await;
            match mob {
                Some(m) => m.into_lua(&lua),
                None => Ok(LuaValue::Nil),
            }
        });

        // ── get_equipment(serial) → table of equipment tables ─────────
        methods.add_async_method("get_equipment", |lua, this, serial: u32| async move {
            let equip: Vec<EquippedItemEntry> = rpc_get_equipment(&this.command_tx, serial).await;
            equip.into_lua(&lua)
        });

        // ── step(heading) → boolean (true=queued, false=blocked) ──────
        methods.add_async_method("step", |_lua, this, heading: u8| async move {
            Ok(rpc_step(&this.command_tx, heading, false).await)
        });

        // ── raw_step(heading) → boolean ───────────────────────────────
        methods.add_async_method("raw_step", |_lua, this, heading: u8| async move {
            Ok(rpc_step(&this.command_tx, heading, true).await)
        });

        // ── use_object(serial) ────────────────────────────────────────
        methods.add_async_method("use_object", |_lua, this, serial: u32| async move {
            rpc_use_object(&this.command_tx, serial).await;
            Ok(())
        });

        // ── say(text) ─────────────────────────────────────────────────
        methods.add_async_method("say", |_lua, this, text: String| async move {
            rpc_say(&this.command_tx, &text).await;
            Ok(())
        });

        // ── inject(hex_string) ────────────────────────────────────────
        methods.add_async_method("inject", |_lua, this, hex: String| async move {
            rpc_inject(&this.command_tx, &hex).await;
            Ok(())
        });
    }
}

// ── RPC helpers ───────────────────────────────────────────────────────────

async fn rpc_get_state(tx: &mpsc::Sender<ClientCommand>) -> FullSessionState {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = tx.send(ClientCommand::GetState { reply: reply_tx }).await;
    reply_rx.await.unwrap_or(FullSessionState {
        character: None,
        position: (0, 0, 0),
        world: 0,
    })
}

async fn rpc_get_items(tx: &mpsc::Sender<ClientCommand>) -> Vec<WorldItemData> {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = tx.send(ClientCommand::GetItems { reply: reply_tx }).await;
    reply_rx.await.unwrap_or_default()
}

async fn rpc_get_mobiles(tx: &mpsc::Sender<ClientCommand>) -> Vec<WorldMobileData> {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = tx.send(ClientCommand::GetMobiles { reply: reply_tx }).await;
    reply_rx.await.unwrap_or_default()
}

async fn rpc_get_mobile(
    tx: &mpsc::Sender<ClientCommand>,
    serial: u32,
) -> Option<WorldMobileData> {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = tx
        .send(ClientCommand::GetMobile { serial, reply: reply_tx })
        .await;
    reply_rx.await.ok().flatten()
}

async fn rpc_get_equipment(
    tx: &mpsc::Sender<ClientCommand>,
    serial: u32,
) -> Vec<EquippedItemEntry> {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = tx
        .send(ClientCommand::GetEquipment { serial, reply: reply_tx })
        .await;
    reply_rx.await.unwrap_or_default()
}

async fn rpc_step(tx: &mpsc::Sender<ClientCommand>, heading: u8, raw: bool) -> bool {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = tx
        .send(ClientCommand::Step {
            heading,
            raw,
            reply: reply_tx,
        })
        .await;
    reply_rx.await.unwrap_or(false)
}

async fn rpc_use_object(tx: &mpsc::Sender<ClientCommand>, serial: u32) {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = tx
        .send(ClientCommand::UseObject { serial, reply: reply_tx })
        .await;
    let _ = reply_rx.await;
}

async fn rpc_say(tx: &mpsc::Sender<ClientCommand>, text: &str) {
    // Build a 0xAD SpeechRequest packet (Plain, Normal, hue=0x0034, font=3).
    let msg_bytes = text.encode_utf16().collect::<Vec<u16>>();
    let language = b"ENU\0";
    let body_len: u16 = (12 + (msg_bytes.len() + 1) * 2) as u16;

    let mut data = Vec::with_capacity(body_len as usize);
    data.push(0xAD);
    data.extend_from_slice(&body_len.to_be_bytes());
    data.push(0x00); // speech type = Normal
    data.extend_from_slice(&0x0034u16.to_be_bytes()); // hue
    data.extend_from_slice(&0x0003u16.to_be_bytes()); // font
    data.extend_from_slice(language);
    for ch in &msg_bytes {
        data.extend_from_slice(&ch.to_be_bytes());
    }
    data.extend_from_slice(&[0x00, 0x00]);

    use protocol::RawPacket;
    use u_core::PacketDirection;

    let pkt = RawPacket::new(data.into(), PacketDirection::ClientToServer);
    let _ = tx
        .send(ClientCommand::RawPacket {
            client_id: 0,
            data: pkt,
        })
        .await;
}

async fn rpc_inject(tx: &mpsc::Sender<ClientCommand>, hex: &str) {
    let bytes: Result<Vec<u8>, _> = (0..hex.len())
        .step_by(2)
        .map(|i| {
            hex.get(i..i + 2)
                .ok_or("odd length")
                .and_then(|s| u8::from_str_radix(s, 16).map_err(|_| "invalid hex"))
        })
        .collect();

    if let Ok(data) = bytes {
        use protocol::RawPacket;
        use u_core::PacketDirection;

        let pkt = RawPacket::new(data.into(), PacketDirection::ClientToServer);
        let _ = tx
            .send(ClientCommand::RawPacket {
                client_id: 0,
                data: pkt,
            })
            .await;
    }
}

// ── Lua table conversion helpers ──────────────────────────────────────────

fn state_to_lua(lua: &Lua, state: &FullSessionState) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    if let Some(ref name) = state.character {
        t.set("character", name.as_str())?;
    }
    t.set("x", state.position.0)?;
    t.set("y", state.position.1)?;
    t.set("z", state.position.2)?;
    t.set("world", state.world)?;
    Ok(t)
}
