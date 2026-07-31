//! Vendor buy/sell windows and transactions.
//!
//! A vendor is an ordinary mobile tagged with `ItemProps.meta["vendor_type"]`.
//! Players open the buy/sell windows by saying `buy` or `sell` near a vendor
//! (see the speech interception in `session_loop.rs`).  This module:
//!
//! * resolves the nearest vendor to the player ([`find_nearest_vendor`]),
//! * sends the buy window (`0x3C` + `0x74` + `0x24`) and sell window (`0x9E`),
//! * handles `BuyItems` (0x3B) and `SellListReply` (0x9F) transactions.
//!
//! Gold is the stackable item graphic [`vendor::GOLD_GRAPHIC`] held in the
//! player's backpack — there is no currency balance on the mobile.
//!
//! Serials for the virtual buy container and its stock items are obtained
//! from the engine serial allocator (no bit tricks).

use std::collections::HashMap;

use log::{info, warn};

use protocol::RawPacket;
use packets::traits::{encode_packet, ManualPacket, BasicPacket};
use packets::interaction::{
    BuyItem, BuyItems, OpenBuyWindow, SellItem, SellList,
};
use packets::speech::{SendSpeech, SpeechType};

use network::error;
use network::session::Session;

use common::uo_engine::entity::DemoEntity;
use common::uo_engine::handler::{ConsumeResult, DropResult, DropTarget, HeldItemInfo};
use common::uo_engine::item_props::ItemProps;
use common::uo_engine::rpc::EngineProxy;

use framework::ecumene::Entity as EngineEntity;

use crate::constants::hue;
use crate::game_util::{chebyshev, engine_for, system_message_gray};
use crate::vendor::{self, VendorEntry, VendorType};
use crate::{DemoCommand, DemoWorkerTx};

use super::PlayerState;

/// Maximum distance (Chebyshev tiles) a player may be from a vendor to
/// trade with it.
const VENDOR_RANGE: u16 = 6;

/// Eye-height offset for LOS checks (matches interaction / combat).
const EYE_HEIGHT: i16 = 14;

/// Gump model id for a vendor buy container.
const VENDOR_GUMP: u16 = 0x0030;

// ── VendorSession ──────────────────────────────────────────────────────────

/// Per-session state for the currently open vendor buy window.
///
/// Stored in `InfraState::open_vendor`.  Maps each stock-item serial
/// (sent in the `0x3C`/`0x74` window) back to the [`VendorEntry`] it
/// represents so a `BuyItems` (0x3B) reply can be priced and fulfilled.
///
/// The buy/sell lists are **not** ordinary container gumps, so they are
/// deliberately not tracked in `OpenContainers` and never closed via
/// `CloseGump`.  The client closes them on its own; the server simply
/// drops this state when the player walks away (see the position check
/// in the rust handler's `post_packet`) or completes a purchase.
#[derive(Debug, Clone)]
pub(super) struct VendorSession {
    /// Serial of the vendor NPC (also the shop "container" serial the
    /// client opens with `DrawContainer`).
    pub vendor_serial: u32,
    /// Player position when the window was opened (for walk-away close).
    pub open_x: u16,
    pub open_y: u16,
    /// Map of stock-item serial → the entry it represents.
    pub stock: HashMap<u32, VendorEntry>,
}

// ── Vendor resolution ──────────────────────────────────────────────────────

/// Read the [`VendorType`] for a mobile, or `None` if it is not a vendor.
async fn vendor_type_of(
    serial: u32,
    engine: &EngineProxy<DemoCommand>,
) -> Option<VendorType> {
    let props = engine.get_item_props(serial).await?;
    let s = props.get_meta_str("vendor_type")?;
    vendor::parse_vendor_type(s)
}

/// Find the nearest tradeable vendor to the player within [`VENDOR_RANGE`].
///
/// Filters mobiles tagged with `meta["vendor_type"]`, requires line of
/// sight, and returns the closest one (Chebyshev distance).
pub(super) async fn find_nearest_vendor(
    player: &PlayerState,
    worker_tx: &DemoWorkerTx,
) -> Option<u32> {
    let engine = engine_for(worker_tx, player.world);

    let area = framework::ecumene::TileRect::from_view(
        player.x, player.y, VENDOR_RANGE,
    );
    let entities = engine.query_area(area).await;

    let mut best: Option<(u16, u32)> = None;
    for ent in &entities {
        let DemoEntity::Mobile(m) = ent else { continue };
        if m.serial == player.serial {
            continue;
        }
        let dist = chebyshev(player.x, player.y, m.x, m.y);
        if dist > VENDOR_RANGE {
            continue;
        }
        if vendor_type_of(m.serial, &engine).await.is_none() {
            continue;
        }
        if !engine.check_los(
            player.x, player.y, player.z as i16 + EYE_HEIGHT,
            m.x, m.y, m.z as i16,
        ).await {
            continue;
        }
        match best {
            Some((bd, _)) if dist >= bd => {}
            _ => best = Some((dist, m.serial)),
        }
    }

    best.map(|(_, serial)| serial)
}

// ── Buy window ─────────────────────────────────────────────────────────────

/// Open the buy window for `vendor_serial` for the given player.
///
/// Mirrors the real UO open-buy sequence:
/// 1. Wear a "restock" container (layer 0x1A) and a "bought" container
///    (layer 0x1B) on the vendor via `EquipItem` (0x2E).
/// 2. Send `ContainerContent` (0x3C) + `OpenBuyWindow` (0x74) for the
///    restock container (the stock), then an empty pair for the bought box.
/// 3. Send `DrawContainer` (0x24, gump 0x0030) whose serial is the
///    **vendor** itself — that is the shop the client opens.
///
/// The buy list is not an ordinary container gump, so it is **not**
/// registered in `OpenContainers`; the stock map is recorded in
/// `open_vendor` instead.
///
/// Returns `true` if the window was opened.
pub(super) async fn open_buy_window(
    vendor_serial: u32,
    player: &PlayerState,
    open_vendor: &mut Option<VendorSession>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    use packets::interaction::EquipItem;
    use packets::layer::Layer;

    let engine = engine_for(worker_tx, player.world);

    let Some(vt) = vendor_type_of(vendor_serial, &engine).await else {
        return Ok(false);
    };

    let entries = vt.buy_list();
    if entries.is_empty() {
        session.send(system_message_gray("That vendor has nothing for sale.")).await?;
        return Ok(true);
    }

    // Allocate item serials for the two shop containers worn by the vendor.
    let restock_serial = engine.allocate_serial().await;
    let buy_box_serial = engine.allocate_serial().await;
    if restock_serial == 0 || buy_box_serial == 0 {
        warn!("[vendor] serial space exhausted — cannot open buy window");
        return Ok(true);
    }

    // 1. Wear the restock (0x1A) and bought (0x1B) containers on the vendor.
    //    Graphic 0x1E5E is the standard vendor container artwork.
    const SHOP_CONTAINER_GFX: u16 = 0x1E5E;
    session.send(RawPacket::s2c(encode_packet(&EquipItem {
        id: EquipItem::ID,
        item_serial: restock_serial,
        graphic: SHOP_CONTAINER_GFX,
        _pad0: (),
        layer: Layer::ShopBuyRestock,
        player_serial: vendor_serial,
        color: 0,
    }))).await?;
    session.send(RawPacket::s2c(encode_packet(&EquipItem {
        id: EquipItem::ID,
        item_serial: buy_box_serial,
        graphic: SHOP_CONTAINER_GFX,
        _pad0: (),
        layer: Layer::ShopBuy,
        player_serial: vendor_serial,
        color: 0,
    }))).await?;

    // Build the stock: one item per entry, each with its own serial, all
    // inside the restock container.
    let mut stock: HashMap<u32, VendorEntry> = HashMap::new();
    let mut content_items: Vec<(u32, u16, u16, u16, u16, u32, u16)> = Vec::with_capacity(entries.len());
    let mut buy_items: Vec<BuyItem> = Vec::with_capacity(entries.len());

    for (i, entry) in entries.iter().enumerate() {
        let item_serial = engine.allocate_serial().await;
        if item_serial == 0 {
            warn!("[vendor] serial space exhausted while building stock");
            break;
        }
        stock.insert(item_serial, *entry);

        content_items.push((
            item_serial,
            entry.graphic,
            1,
            44 + (i as u16 % 5) * 12,
            65 + (i as u16 / 5) * 12,
            restock_serial,
            entry.color,
        ));

        buy_items.push(BuyItem {
            price: entry.price,
            description: entry.name.to_string(),
        });
    }

    // The 0x74 buy list must be in the reversed order of the 0x3C contents.
    buy_items.reverse();

    let version = player.client_version;

    // 2. Restock container contents (0x3C) + buy prices (0x74).
    session.send(common::spawn::build_container_content(&content_items, version)).await?;
    session.send(RawPacket::s2c(
        OpenBuyWindow { container_serial: restock_serial, items: buy_items }.to_bytes(),
    )).await?;

    // 3. Empty bought container (0x3C + 0x74).
    session.send(common::spawn::build_container_content(&[], version)).await?;
    session.send(RawPacket::s2c(
        OpenBuyWindow { container_serial: buy_box_serial, items: Vec::new() }.to_bytes(),
    )).await?;

    // 4. Open the shop gump — the serial is the VENDOR itself.
    session.send(common::spawn::build_draw_container(vendor_serial, VENDOR_GUMP, version)).await?;

    // Track the open window.  The buy list is not a container gump, so it
    // is NOT registered in OpenContainers (which would emit a spurious
    // CloseGump).  We just remember the stock + the open position.
    *open_vendor = Some(VendorSession {
        vendor_serial,
        open_x: player.x,
        open_y: player.y,
        stock,
    });

    info!(
        "[vendor] opened buy window: vendor=0x{:08X} restock=0x{:08X} ({} items)",
        vendor_serial, restock_serial, entries.len(),
    );
    Ok(true)
}

// ── Sell window ────────────────────────────────────────────────────────────

/// Open the sell window for `vendor_serial`: scans the player's backpack for
/// items the vendor will buy and sends a [`SellList`] (0x9E).
///
/// Returns `true` if a sell window was sent (even if empty).
pub(super) async fn open_sell_window(
    vendor_serial: u32,
    player: &PlayerState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<bool> {
    let engine = engine_for(worker_tx, player.world);

    let Some(vt) = vendor_type_of(vendor_serial, &engine).await else {
        return Ok(false);
    };

    // Resolve the player's backpack.
    let Some(bp_serial) = engine.get_entity(player.serial).await
        .and_then(|e| e.backpack_serial())
    else {
        session.send(system_message_gray("You have no backpack.")).await?;
        return Ok(true);
    };

    let mut sell_items: Vec<SellItem> = Vec::new();
    if let Some(container) = engine.get_container(bp_serial).await {
        for item in &container.items {
            let Some(price) = vt.sell_price(item.graphic) else { continue };
            // Resolve a display name from item props, fall back to graphic.
            let name = engine.get_item_props(item.serial).await
                .and_then(|p| p.name_owned())
                .unwrap_or_else(|| format!("item 0x{:04X}", item.graphic));
            sell_items.push(SellItem {
                item_id: item.serial,
                item_model: item.graphic,
                hue: item.color,
                amount: item.amount,
                value: price.min(u16::MAX as u32) as u16,
                name,
            });
        }
    }

    if sell_items.is_empty() {
        session.send(system_message_gray(
            "You have nothing that vendor wishes to buy.",
        )).await?;
        return Ok(true);
    }

    session.send(RawPacket::s2c(
        SellList { shopkeeper_id: vendor_serial, items: sell_items }.to_bytes(),
    )).await?;

    info!("[vendor] opened sell window: vendor=0x{:08X}", vendor_serial);
    Ok(true)
}

// ── Buy transaction (0x3B) ──────────────────────────────────────────────────

/// Handle a `BuyItems` (0x3B) reply: price the requested items, verify the
/// player has enough gold in the backpack, deduct it, and deliver the goods.
pub(super) async fn handle_buy(
    vendor_id: u32,
    items: &[(u32, u16)],
    player: &PlayerState,
    open_vendor: &mut Option<VendorSession>,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    // Empty list = client cancelled the buy window.
    let Some(vs) = open_vendor.as_ref() else {
        return Ok(());
    };
    if vs.vendor_serial != vendor_id {
        // Stale / mismatched window — ignore.
        return Ok(());
    }

    if items.is_empty() {
        // Client cancelled — just drop the server-side state.  No gump to
        // close (the buy list is client-managed).
        *open_vendor = None;
        return Ok(());
    }

    let engine = engine_for(worker_tx, player.world);

    // Compute total cost and gather the (entry, qty) purchases.
    let mut purchases: Vec<(VendorEntry, u16)> = Vec::new();
    let mut total_cost: u64 = 0;
    for &(item_serial, qty) in items {
        if qty == 0 {
            continue;
        }
        let Some(entry) = vs.stock.get(&item_serial).copied() else {
            // Unknown stock serial — ignore that line.
            continue;
        };
        total_cost += entry.price as u64 * qty as u64;
        purchases.push((entry, qty));
    }

    if purchases.is_empty() {
        return Ok(());
    }

    // Resolve the backpack.
    let Some(bp_serial) = engine.get_entity(player.serial).await
        .and_then(|e| e.backpack_serial())
    else {
        session.send(system_message_gray("You have no backpack.")).await?;
        return Ok(());
    };

    // Count gold available in the backpack.
    let gold = backpack_gold_stacks(&engine, bp_serial).await;
    let have: u64 = gold.iter().map(|(_, amt)| *amt as u64).sum();
    if have < total_cost {
        session.send(system_message_gray(&format!(
            "You cannot afford that ({} gold needed, you have {}).",
            total_cost, have,
        ))).await?;
        return Ok(());
    }

    // Deduct gold from the stacks.
    if !deduct_gold(&engine, &gold, total_cost).await {
        session.send(system_message_gray(
            "The transaction failed.",
        )).await?;
        return Ok(());
    }

    // Deliver the goods into the backpack.
    let target = DropTarget::OnEntity { target_serial: bp_serial, x: 0xFFFF, y: 0xFFFF };
    let mut delivered = 0u32;
    for (entry, qty) in &purchases {
        let item_serial = engine.allocate_serial().await;
        if item_serial == 0 {
            warn!("[vendor] serial space exhausted delivering purchase");
            break;
        }
        let held = HeldItemInfo {
            serial: item_serial,
            graphic: entry.graphic,
            color: entry.color,
            amount: *qty,
        };
        match engine.drop_item(player.serial, held, target.clone(), None).await {
            DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. }
            | DropResult::FallbackGround { .. } | DropResult::DroppedOnGround { .. }
            | DropResult::MergedOnGround { .. } => {
                // Name the freshly-created item.
                engine.set_item_props(item_serial, Some(ItemProps::with_name(entry.name))).await;
                delivered += *qty as u32;
            }
            other => {
                warn!("[vendor] failed to deliver {} x{}: {:?}", entry.name, qty, other);
            }
        }
    }

    // Vendor acknowledges the purchase (cosmetic, like the real server).
    vendor_say(
        vendor_id,
        &format!("The total of thy purchase is {} gold.", total_cost),
        player.world, session, worker_tx,
    ).await?;

    // Send the purchase confirmation: an empty BuyItems (0x3B) back to the
    // client.  This is what closes the buy list (it is NOT a gump, so a
    // CloseGump would do nothing).
    session.send(RawPacket::s2c(
        BuyItems::new(vendor_id, Vec::new()).to_bytes(),
    )).await?;

    let _ = delivered;

    // The transaction is complete; the buy list is closed client-side.
    *open_vendor = None;

    // Refresh the status bar gold + weight.
    super::util::send_weight_update(player, None, session, worker_tx).await?;
    Ok(())
}

// ── Sell transaction (0x9F) ──────────────────────────────────────────────────

/// Handle a `SellListReply` (0x9F): consume the sold items and pay the player
/// in gold.
pub(super) async fn handle_sell(
    shopkeeper_id: u32,
    items: &[(u32, u16)],
    player: &PlayerState,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    let engine = engine_for(worker_tx, player.world);

    let Some(vt) = vendor_type_of(shopkeeper_id, &engine).await else {
        return Ok(());
    };

    let Some(bp_serial) = engine.get_entity(player.serial).await
        .and_then(|e| e.backpack_serial())
    else {
        return Ok(());
    };

    // Snapshot the backpack so we can validate item graphics / quantities.
    let Some(container) = engine.get_container(bp_serial).await else {
        return Ok(());
    };

    let mut total_paid: u64 = 0;
    let mut sold = 0u32;
    for &(item_serial, qty) in items {
        if qty == 0 {
            continue;
        }
        let Some(ci) = container.items.iter().find(|c| c.serial == item_serial) else {
            continue;
        };
        let Some(price) = vt.sell_price(ci.graphic) else {
            continue;
        };
        let sell_qty = qty.min(ci.amount);
        if sell_qty == 0 {
            continue;
        }
        // Consume the items.
        match engine.consume_item(item_serial, sell_qty, Some(ci.graphic)).await {
            Some(ConsumeResult { .. }) => {
                total_paid += price as u64 * sell_qty as u64;
                sold += sell_qty as u32;
            }
            None => {
                warn!("[vendor] failed to consume sold item 0x{:08X}", item_serial);
            }
        }
    }

    if total_paid == 0 {
        return Ok(());
    }

    // Pay the player in gold (splitting into stacks ≤ MAX_GOLD_STACK).
    give_gold(&engine, player.serial, bp_serial, total_paid).await;

    // Vendor acknowledges the sale (cosmetic).
    vendor_say(
        shopkeeper_id,
        &format!("I'll give thee {} gold for thy goods.", total_paid),
        player.world, session, worker_tx,
    ).await?;

    let _ = sold;

    super::util::send_weight_update(player, None, session, worker_tx).await?;
    Ok(())
}

// ── Vendor speech ────────────────────────────────────────────────────────

/// Make the vendor say `msg` as overhead speech (0x1C SendSpeech).
///
/// Looks up the vendor's graphic/name so the text appears over the NPC.
async fn vendor_say(
    vendor_serial: u32,
    msg: &str,
    world: u8,
    session: &mut Session,
    worker_tx: &DemoWorkerTx,
) -> error::Result<()> {
    let engine = engine_for(worker_tx, world);
    let (graphic, name) = match engine.get_entity(vendor_serial).await
        .as_ref()
        .and_then(|e| e.mobile())
    {
        Some(m) => (m.graphic, if m.name.is_empty() { "Vendor".to_string() } else { m.name.clone() }),
        None => (0x0190, "Vendor".to_string()),
    };

    session.send(RawPacket::s2c(
        SendSpeech {
            serial: vendor_serial,
            model: graphic,
            speech_type: SpeechType::Normal,
            color: hue::SYSTEM_GRAY,
            font: 3,
            name,
            message: msg.to_string(),
        }
        .to_bytes(),
    )).await?;
    Ok(())
}

// ── Gold helpers ─────────────────────────────────────────────────────────────

/// Return the gold stacks `(serial, amount)` directly inside `bp_serial`.
async fn backpack_gold_stacks(
    engine: &EngineProxy<DemoCommand>,
    bp_serial: u32,
) -> Vec<(u32, u16)> {
    let mut out = Vec::new();
    if let Some(container) = engine.get_container(bp_serial).await {
        for item in &container.items {
            if item.graphic == vendor::GOLD_GRAPHIC {
                out.push((item.serial, item.amount));
            }
        }
    }
    out
}

/// Deduct `amount` gold from the given stacks.  Assumes the caller has
/// already verified the total is sufficient.
async fn deduct_gold(
    engine: &EngineProxy<DemoCommand>,
    stacks: &[(u32, u16)],
    amount: u64,
) -> bool {
    let mut remaining = amount;
    for &(serial, stack_amt) in stacks {
        if remaining == 0 {
            break;
        }
        let take = (stack_amt as u64).min(remaining) as u16;
        if engine.consume_item(serial, take, Some(vendor::GOLD_GRAPHIC)).await.is_none() {
            return false;
        }
        remaining -= take as u64;
    }
    remaining == 0
}

/// Give `amount` gold to the player by dropping stacks (≤ MAX_GOLD_STACK)
/// into the backpack.
async fn give_gold(
    engine: &EngineProxy<DemoCommand>,
    player_serial: u32,
    bp_serial: u32,
    amount: u64,
) {
    let target = DropTarget::OnEntity { target_serial: bp_serial, x: 0xFFFF, y: 0xFFFF };
    let mut remaining = amount;
    while remaining > 0 {
        let chunk = remaining.min(vendor::MAX_GOLD_STACK as u64) as u16;
        let serial = engine.allocate_serial().await;
        if serial == 0 {
            warn!("[vendor] serial space exhausted paying gold");
            return;
        }
        let held = HeldItemInfo {
            serial,
            graphic: vendor::GOLD_GRAPHIC,
            color: 0,
            amount: chunk,
        };
        match engine.drop_item(player_serial, held, target.clone(), None).await {
            DropResult::DroppedInContainer { .. } | DropResult::MergedInContainer { .. }
            | DropResult::FallbackGround { .. } | DropResult::DroppedOnGround { .. }
            | DropResult::MergedOnGround { .. } => {}
            other => {
                warn!("[vendor] failed to pay gold: {:?}", other);
                return;
            }
        }
        remaining -= chunk as u64;
    }
    let _ = engine;
}
