//! NPC vendor definitions: vendor types, body graphics, and static
//! price tables (buy list = what the vendor sells to players, sell prices
//! = what the vendor pays players for items).
//!
//! Vendors are ordinary [`MobileData`](common::uo_engine::entity::MobileData)
//! mobiles tagged with `ItemProps.meta["vendor_type"]`.  All transaction
//! logic lives in `game_session::vendor_session`; this module only
//! provides the static data.
//!
//! Gold is the item graphic `0x0EED` (weightless) held physically in the
//! player's backpack — there is no currency balance on the mobile.

use crate::constants::item;

/// Gold-coin item graphic.  Gold is a stackable, weightless item.
///
/// Re-exported from the canonical [`crate::constants::item::GOLD`] so the
/// many call sites that already use `vendor::GOLD_GRAPHIC` keep working.
pub const GOLD_GRAPHIC: u16 = item::GOLD;

/// Maximum size of a single gold stack before it must be split.
pub const MAX_GOLD_STACK: u16 = 60_000;

// ── VendorType ─────────────────────────────────────────────────────────────

/// Kinds of vendor the demo server can spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorType {
    /// Sells reagents.
    Mage,
    /// Sells spell scrolls.
    Scribe,
    /// Sells bandages and buys back common goods.
    Healer,
    /// Sells potions.
    Alchemist,
}

impl VendorType {
    /// Stable string id stored in `ItemProps.meta["vendor_type"]`.
    pub fn as_str(self) -> &'static str {
        match self {
            VendorType::Mage => "mage",
            VendorType::Scribe => "scribe",
            VendorType::Healer => "healer",
            VendorType::Alchemist => "alchemist",
        }
    }

    /// Default display name for a freshly spawned vendor of this type.
    pub fn default_name(self) -> &'static str {
        match self {
            VendorType::Mage => "the mage",
            VendorType::Scribe => "the scribe",
            VendorType::Healer => "the healer",
            VendorType::Alchemist => "the alchemist",
        }
    }

    /// Body graphic to use when spawning this vendor.
    ///
    /// All demo vendors are humanoid (0x0190 male human).
    pub fn body_graphic(self) -> u16 {
        crate::constants::body::MALE_HUMAN
    }

    /// The list of items this vendor sells to players.
    pub fn buy_list(self) -> &'static [VendorEntry] {
        match self {
            VendorType::Mage => MAGE_STOCK,
            VendorType::Scribe => SCRIBE_STOCK,
            VendorType::Healer => HEALER_STOCK,
            VendorType::Alchemist => ALCHEMIST_STOCK,
        }
    }

    /// Price (gold) the vendor pays when buying `graphic` from a player,
    /// or `None` if this vendor does not buy that item.
    ///
    /// The vendor buys back anything in its own buy-list at half the
    /// selling price (minimum 1 gold).
    pub fn sell_price(self, graphic: u16) -> Option<u32> {
        self.buy_list()
            .iter()
            .find(|e| e.graphic == graphic)
            .map(|e| (e.price / 2).max(1))
    }
}

/// Parse a vendor type from a `.vendor <type>` argument or a
/// `meta["vendor_type"]` string.
pub fn parse_vendor_type(s: &str) -> Option<VendorType> {
    match s.trim().to_ascii_lowercase().as_str() {
        "mage" => Some(VendorType::Mage),
        "scribe" => Some(VendorType::Scribe),
        "healer" => Some(VendorType::Healer),
        "alchemist" => Some(VendorType::Alchemist),
        _ => None,
    }
}

// ── VendorEntry ──────────────────────────────────────────────────────────

/// A single item a vendor offers for sale.
#[derive(Debug, Clone, Copy)]
pub struct VendorEntry {
    /// Item graphic / artwork id.
    pub graphic: u16,
    /// Item hue (0 = default).
    pub color: u16,
    /// Display name / description shown in the buy window.
    pub name: &'static str,
    /// Price in gold for one unit.
    pub price: u32,
}

// ── Static stock tables ────────────────────────────────────────────────────

const MAGE_STOCK: &[VendorEntry] = &[
    VendorEntry { graphic: item::REAGENT_BLACK_PEARL,    color: 0, name: "Black Pearl",     price: 5 },
    VendorEntry { graphic: item::REAGENT_BLOOD_MOSS,     color: 0, name: "Blood Moss",      price: 5 },
    VendorEntry { graphic: item::REAGENT_GARLIC,         color: 0, name: "Garlic",          price: 3 },
    VendorEntry { graphic: item::REAGENT_GINSENG,        color: 0, name: "Ginseng",         price: 3 },
    VendorEntry { graphic: item::REAGENT_MANDRAKE_ROOT,  color: 0, name: "Mandrake Root",   price: 6 },
    VendorEntry { graphic: item::REAGENT_NIGHTSHADE,     color: 0, name: "Nightshade",      price: 4 },
    VendorEntry { graphic: item::REAGENT_SULPHUROUS_ASH, color: 0, name: "Sulphurous Ash",  price: 4 },
    VendorEntry { graphic: item::REAGENT_SPIDERS_SILK,   color: 0, name: "Spider's Silk",   price: 5 },
];

const SCRIBE_STOCK: &[VendorEntry] = &[
    VendorEntry { graphic: item::SCROLL_HEAL,         color: 0, name: "Scroll of Heal",          price: 18 },
    VendorEntry { graphic: item::SCROLL_MAGIC_ARROW,  color: 0, name: "Scroll of Magic Arrow",   price: 18 },
    VendorEntry { graphic: item::SCROLL_BLESS,        color: 0, name: "Scroll of Bless",         price: 35 },
    VendorEntry { graphic: item::SCROLL_CURSE,        color: 0, name: "Scroll of Curse",         price: 35 },
    VendorEntry { graphic: item::SCROLL_GREATER_HEAL, color: 0, name: "Scroll of Greater Heal",  price: 45 },
    VendorEntry { graphic: item::SCROLL_LIGHTNING,    color: 0, name: "Scroll of Lightning",     price: 45 },
    VendorEntry { graphic: item::SCROLL_ENERGY_BOLT,  color: 0, name: "Scroll of Energy Bolt",   price: 60 },
    VendorEntry { graphic: item::SCROLL_FLAMESTRIKE,  color: 0, name: "Scroll of Flamestrike",   price: 80 },
];

const HEALER_STOCK: &[VendorEntry] = &[
    VendorEntry { graphic: item::BANDAGE, color: 0, name: "Bandage", price: 2 },
];

const ALCHEMIST_STOCK: &[VendorEntry] = &[
    VendorEntry { graphic: item::POTION_GREATER_HEAL, color: 0,      name: "Greater Heal Potion",     price: 25 },
    VendorEntry { graphic: item::POTION_REFRESH,      color: 0x002D, name: "Greater Refresh Potion",  price: 25 },
    VendorEntry { graphic: item::POTION_MANA,         color: 0x0005, name: "Greater Mana Potion",     price: 30 },
    VendorEntry { graphic: item::POTION_CURE,         color: 0,      name: "Greater Cure Potion",     price: 15 },
    VendorEntry { graphic: item::POTION_STRENGTH,     color: 0x0035, name: "Greater Strength Potion", price: 35 },
    VendorEntry { graphic: item::POTION_AGILITY,      color: 0,      name: "Greater Agility Potion",  price: 35 },
];
