//! Weight and armor computation helpers.
//!
//! These are pure functions that query a [`DemoZone`] to compute carried
//! weight and equipped armor ratings.  Called from
//! `intercept_engine_command` in `handler.rs`.

use framework::continuum::container::ZoneContainers;
use framework::continuum::{HashItemProps, ZoneItemProps};
use framework::ecumene::StaticDataProvider;

use common::uo_engine::handler::ArmorProfile;
use common::uo_engine::item_props::ItemProps;

use crate::constants::{armor, weight};
use crate::handler::DemoZone;
use crate::vendor;

// ── Gold computation ───────────────────────────────────────────────────────

/// Compute total gold (graphic `0x0EED`) carried by a mobile, recursively
/// scanning the backpack and all nested sub-containers.
///
/// `held_item` is `Some((serial, graphic, amount))` if the player has an
/// item on the drag-and-drop cursor (already removed from the world but
/// still logically carried).
///
/// Returns `None` if the entity is not found or not a mobile.
pub(crate) fn compute_backpack_gold(
    zone: &DemoZone,
    serial: u32,
    held_item: Option<(u32, u16, u16)>,
) -> Option<u32> {
    let entity = zone.store.get(serial)?;
    let m = entity.mobile()?;

    let mut total: u32 = 0;

    // Backpack contents (recursive).
    if let Some(bp) = m.items.iter().find(|eq| eq.layer == packets::layer::Layer::Backpack) {
        total += container_gold(bp.serial, &zone.containers, 0);
    }

    // Held item on cursor (already removed from world, still carried).
    if let Some((held_serial, held_graphic, held_amount)) = held_item {
        if held_graphic == vendor::GOLD_GRAPHIC {
            total += held_amount as u32;
        }
        // If the held item is a container, include gold inside it.
        if zone.containers.get(held_serial).is_some() {
            total += container_gold(held_serial, &zone.containers, 0);
        }
    }

    Some(total)
}

/// Recursively sum gold (graphic `0x0EED`) inside a container and all
/// nested sub-containers.
fn container_gold(
    container_serial: u32,
    containers: &framework::continuum::HashContainerStore,
    depth: u8,
) -> u32 {
    if depth > 16 {
        return 0;
    }

    let Some(info) = containers.get(container_serial) else {
        return 0;
    };

    let mut total: u32 = 0;

    for item in &info.items {
        if item.graphic == vendor::GOLD_GRAPHIC {
            total += item.amount as u32;
        }
        // If this item is itself a container, recurse.
        if containers.get(item.serial).is_some() {
            total += container_gold(item.serial, containers, depth + 1);
        }
    }

    total
}

// ── Weight computation ─────────────────────────────────────────────────────

/// Compute total carried weight for a mobile entity.
///
/// `held_item` is `Some((serial, graphic, amount))` if the player has an
/// item on the drag-and-drop cursor (already removed from the world but
/// still logically carried).
///
/// Returns `(current_weight_stones, max_weight_stones)` or `None` if the
/// entity is not found or not a mobile.
pub(crate) fn compute_mobile_weight(
    zone: &DemoZone,
    serial: u32,
    held_item: Option<(u32, u16, u16)>,
) -> Option<(u16, u16)> {
    let entity = zone.store.get(serial)?;
    let m = entity.mobile()?;

    let sd = zone.static_data();
    let sd_ref = sd.as_deref().map(|a| a.as_ref());

    let mut total_tenths: u32 = 0;

    // 1. Equipped items weight.
    for eq in &m.items {
        // The mount (rideable) carries itself — its weight is not borne by
        // the rider, so it must not count toward the player's carry weight.
        if eq.layer == packets::layer::Layer::Mount {
            continue;
        }
        let unit_wt = resolve_item_weight_tenths(
            eq.serial, eq.graphic, &zone.item_props, sd_ref,
        );
        // Equipped items have amount = 1.
        total_tenths += unit_wt as u32;
    }

    // 2. Backpack contents (recursive).
    if let Some(bp) = m.items.iter().find(|eq| eq.layer == packets::layer::Layer::Backpack) {
        total_tenths += container_weight_tenths(
            bp.serial, &zone.containers, &zone.item_props, sd_ref, 0,
        );
    }

    // 3. Held item on cursor (already removed from world, still carried).
    if let Some((held_serial, held_graphic, held_amount)) = held_item {
        let unit_wt = resolve_item_weight_tenths(
            held_serial, held_graphic, &zone.item_props, sd_ref,
        );
        total_tenths += unit_wt as u32 * held_amount as u32;

        // If the held item is a container, include its contents weight.
        if zone.containers.get(held_serial).is_some() {
            total_tenths += container_weight_tenths(
                held_serial, &zone.containers, &zone.item_props, sd_ref, 0,
            );
        }
    }

    let current_stones = weight::tenths_to_stones(total_tenths);
    let max_stones = weight::max_carry_weight(m.str_);

    Some((current_stones, max_stones))
}

/// Resolve the weight (in tenths) of one unit of a specific item instance.
///
/// Priority:
/// 1. `ItemProps::weight_override` (per-instance override).
/// 2. Server weight override table.
/// 3. `tiledata.mul` (via `StaticDataProvider`).
/// 4. Fallback: 10 (= 1.0 stone).
fn resolve_item_weight_tenths(
    serial: u32,
    graphic: u16,
    item_props: &HashItemProps<ItemProps>,
    static_data: Option<&dyn StaticDataProvider>,
) -> u16 {
    // 1. Per-instance override.
    if let Some(props) = item_props.get(serial) {
        if let Some(wt) = props.weight_override {
            return wt;
        }
    }

    // 2 + 3 + 4. Server table → tiledata → fallback.
    weight::item_weight_tenths(graphic, static_data)
}

/// Recursively compute the total weight (in tenths) of all items inside a
/// container, including nested sub-containers.
fn container_weight_tenths(
    container_serial: u32,
    containers: &framework::continuum::HashContainerStore,
    item_props: &HashItemProps<ItemProps>,
    static_data: Option<&dyn StaticDataProvider>,
    depth: u8,
) -> u32 {
    // Guard against infinite recursion (shouldn't happen, but be safe).
    if depth > 16 {
        return 0;
    }

    let Some(info) = containers.get(container_serial) else {
        return 0;
    };

    let mut total: u32 = 0;

    for item in &info.items {
        let unit_wt = resolve_item_weight_tenths(
            item.serial, item.graphic, item_props, static_data,
        );
        total += unit_wt as u32 * item.amount as u32;

        // If this item is itself a container, add its contents.
        if containers.get(item.serial).is_some() {
            total += container_weight_tenths(
                item.serial, containers, item_props, static_data, depth + 1,
            );
        }
    }

    total
}

// ── Armor computation ──────────────────────────────────────────────────────

/// Compute the per-zone armor profile for a mobile entity.
///
/// For each equipped item, resolves its AR using a two-level fallback:
/// 1. `ItemProps.meta["armor_rating"]` — per-instance override.
/// 2. Static `ARMOR_TEMPLATES` table lookup by `(graphic, color)`.
/// 3. Fallback by `graphic` only (for replay entities with unknown colors).
///
/// Returns `None` if the entity is not found or not a mobile.
pub(crate) fn compute_armor_profile(
    zone: &DemoZone,
    serial: u32,
) -> Option<ArmorProfile> {
    let entity = zone.store.get(serial)?;
    let m = entity.mobile()?;

    let mut profile = ArmorProfile::default();

    for eq in &m.items {
        let ar = resolve_piece_ar(eq, &zone.item_props);
        if ar == 0 {
            continue;
        }

        // Determine which zone this piece protects based on its layer.
        match eq.layer {
            packets::layer::Layer::Helmet   => { profile.head  = profile.head.max(ar); }
            packets::layer::Layer::Necklace => { profile.neck  = profile.neck.max(ar); }
            packets::layer::Layer::Torso
            | packets::layer::Layer::Tunic  => { profile.chest = profile.chest.max(ar); }
            packets::layer::Layer::Arms
            | packets::layer::Layer::Gloves => { profile.arms  = profile.arms.max(ar); }
            packets::layer::Layer::Legs
            | packets::layer::Layer::Pants  => { profile.legs  = profile.legs.max(ar); }
            packets::layer::Layer::LeftHand => {
                // Only count as shield if the graphic is in the armor table.
                if armor::is_shield(eq.graphic) || ar > 0 {
                    // Check if it's actually armor (not a weapon in LeftHand).
                    if armor::lookup_template(eq.graphic, eq.color.unwrap_or(0)).is_some()
                        || armor::lookup_template_by_graphic(eq.graphic)
                            .map_or(false, |t| t.tier == armor::ArmorTier::Shield)
                    {
                        profile.shield = profile.shield.max(ar);
                        profile.has_shield = true;
                    }
                }
            }
            _ => {} // Other layers (Ring, Bracelet, etc.) — no physical AR.
        }
    }

    Some(profile)
}

/// Resolve the armor rating of a single equipped item.
///
/// Priority:
/// 1. `ItemProps.meta["armor_rating"]` — per-instance override.
/// 2. Static template lookup by `(graphic, color)` — exact match.
/// 3. Static template lookup by `graphic` only — fallback for replay entities.
/// 4. `0` — item has no armor value.
fn resolve_piece_ar(
    eq: &packets::world::EquippedItem,
    item_props: &HashItemProps<ItemProps>,
) -> u16 {
    // 1. Per-instance override from meta.
    if let Some(props) = item_props.get(eq.serial) {
        if let Some(ar) = props.get_meta_int("armor_rating") {
            return ar.max(0) as u16;
        }
    }

    // 2. Exact (graphic, color) match in static table.
    let color = eq.color.unwrap_or(0);
    if let Some(template) = armor::lookup_template(eq.graphic, color) {
        return template.armor_rating;
    }

    // 3. Graphic-only fallback (replay entities with unknown color).
    if let Some(template) = armor::lookup_template_by_graphic(eq.graphic) {
        return template.armor_rating;
    }

    // 4. Unknown — no armor.
    0
}

// ── Skill bonus computation ────────────────────────────────────────────────

/// Compute the total skill bonus (in tenths) granted by a mobile's equipped
/// items, keyed by skill id.
///
/// "Plus" weapons store their bonus per-instance in `ItemProps.meta`:
/// - [`meta_key::SKILL_BONUS_ID`](crate::constants::meta_key::SKILL_BONUS_ID) — the UO skill id the bonus applies to.
/// - [`meta_key::SKILL_BONUS_AMOUNT`](crate::constants::meta_key::SKILL_BONUS_AMOUNT) — the bonus, in tenths (e.g. `50` = +5.0).
///
/// Multiple equipped items affecting the same skill are summed.  Items
/// without both meta keys contribute nothing.
///
/// Returns an empty map if the entity is not found, is not a mobile, or has
/// no skill-bonus items equipped.
pub(crate) fn compute_skill_bonuses(
    zone: &DemoZone,
    serial: u32,
) -> std::collections::BTreeMap<u16, u16> {
    use crate::constants::meta_key;

    let mut bonuses: std::collections::BTreeMap<u16, u16> = std::collections::BTreeMap::new();

    let Some(entity) = zone.store.get(serial) else {
        return bonuses;
    };
    let Some(m) = entity.mobile() else {
        return bonuses;
    };

    for eq in &m.items {
        let Some(props) = zone.item_props.get(eq.serial) else {
            continue;
        };
        let (Some(id), Some(amount)) = (
            props.get_meta_int(meta_key::SKILL_BONUS_ID),
            props.get_meta_int(meta_key::SKILL_BONUS_AMOUNT),
        ) else {
            continue;
        };
        // Skill ids are u16; bonus amounts are non-negative tenths.
        if id < 0 || amount <= 0 {
            continue;
        }
        let skill_id = id as u16;
        let add = amount.min(u16::MAX as i64) as u16;
        let entry = bonuses.entry(skill_id).or_insert(0);
        *entry = entry.saturating_add(add);
    }

    bonuses
}

