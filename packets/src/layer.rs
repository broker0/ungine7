//! Equipment layer enum.
//!
//! [`Layer`] identifies the body slot an item occupies on a mobile.
//! Used in [`DrawMobile`](crate::world::DrawMobile), [`EquipItem`](crate::interaction::EquipItem),
//! [`WearItem`](crate::interaction::WearItem), [`CorpseClothing`](crate::interaction::CorpseClothing),
//! and [`Particle3DEffect`](crate::world::Particle3DEffect).

use macros::WireEnum;

/// Equipment layer / body slot.
///
/// | Wire | Variant          | Notes                              |
/// |------|------------------|------------------------------------|
/// | 0x00 | `Invalid`        | No layer / list terminator (0x89)  |
/// | 0x01 | `RightHand`      | Right-hand weapon                  |
/// | 0x02 | `LeftHand`       | Left-hand item / two-handed weapon |
/// | 0x03 | `Shoes`          |                                    |
/// | 0x04 | `Pants`          |                                    |
/// | 0x05 | `Shirt`          |                                    |
/// | 0x06 | `Helmet`         |                                    |
/// | 0x07 | `Gloves`         |                                    |
/// | 0x08 | `Ring`           |                                    |
/// | 0x09 | `Talisman`       |                                    |
/// | 0x0A | `Necklace`       |                                    |
/// | 0x0B | `Hair`           |                                    |
/// | 0x0C | `Waist`          |                                    |
/// | 0x0D | `Torso`          |                                    |
/// | 0x0E | `Bracelet`       |                                    |
/// | 0x0F | `Face`           |                                    |
/// | 0x10 | `Beard`          |                                    |
/// | 0x11 | `Tunic`          |                                    |
/// | 0x12 | `Earrings`       |                                    |
/// | 0x13 | `Arms`           |                                    |
/// | 0x14 | `Cloak`          |                                    |
/// | 0x15 | `Backpack`       |                                    |
/// | 0x16 | `Robe`           |                                    |
/// | 0x17 | `Skirt`          |                                    |
/// | 0x18 | `Legs`           |                                    |
/// | 0x19 | `Mount`          | Mount / riding layer               |
/// | 0x1A | `ShopBuyRestock` | Vendor restock container           |
/// | 0x1B | `ShopBuy`        | Vendor buy container               |
/// | 0x1C | `ShopSell`       | Vendor sell container              |
/// | 0x1D | `Bank`           | Bank container                     |
/// | 0xFF | `MovingEffect`   | `Particle3DEffect`: moving / non-char target |
#[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[repr(u8)]
pub enum Layer {
    /// 0x00 — no layer; also used as list terminator in [`CorpseClothing`](crate::interaction::CorpseClothing).
    #[wire_enum(0x00, "invalid")]
    Invalid,
    /// 0x01 — right-hand (one-handed) weapon slot.
    #[wire_enum(0x01, "right_hand")]
    RightHand,
    /// 0x02 — left-hand / two-handed weapon slot.
    #[wire_enum(0x02, "left_hand")]
    LeftHand,
    /// 0x03 — shoes.
    #[wire_enum(0x03, "shoes")]
    Shoes,
    /// 0x04 — pants / trousers.
    #[wire_enum(0x04, "pants")]
    Pants,
    /// 0x05 — shirt.
    #[wire_enum(0x05, "shirt")]
    Shirt,
    /// 0x06 — helmet.
    #[wire_enum(0x06, "helmet")]
    Helmet,
    /// 0x07 — gloves.
    #[wire_enum(0x07, "gloves")]
    Gloves,
    /// 0x08 — ring.
    #[wire_enum(0x08, "ring")]
    Ring,
    /// 0x09 — talisman.
    #[wire_enum(0x09, "talisman")]
    Talisman,
    /// 0x0A — necklace.
    #[wire_enum(0x0A, "necklace")]
    Necklace,
    /// 0x0B — hair.
    #[wire_enum(0x0B, "hair")]
    Hair,
    /// 0x0C — waist / belt.
    #[wire_enum(0x0C, "waist")]
    Waist,
    /// 0x0D — torso / inner armour.
    #[wire_enum(0x0D, "torso")]
    Torso,
    /// 0x0E — bracelet.
    #[wire_enum(0x0E, "bracelet")]
    Bracelet,
    /// 0x0F — face.
    #[wire_enum(0x0F, "face")]
    Face,
    /// 0x10 — beard / facial hair.
    #[wire_enum(0x10, "beard")]
    Beard,
    /// 0x11 — tunic / mid-layer.
    #[wire_enum(0x11, "tunic")]
    Tunic,
    /// 0x12 — earrings.
    #[wire_enum(0x12, "earrings")]
    Earrings,
    /// 0x13 — arms.
    #[wire_enum(0x13, "arms")]
    Arms,
    /// 0x14 — cloak.
    #[wire_enum(0x14, "cloak")]
    Cloak,
    /// 0x15 — backpack.
    #[wire_enum(0x15, "backpack")]
    Backpack,
    /// 0x16 — robe / outer layer.
    #[wire_enum(0x16, "robe")]
    Robe,
    /// 0x17 — skirt / kilt.
    #[wire_enum(0x17, "skirt")]
    Skirt,
    /// 0x18 — legs / greaves.
    #[wire_enum(0x18, "legs")]
    Legs,
    /// 0x19 — mount (riding layer).
    #[wire_enum(0x19, "mount")]
    Mount,
    /// 0x1A — vendor restock container.
    #[wire_enum(0x1A, "shop_buy_restock")]
    ShopBuyRestock,
    /// 0x1B — vendor buy container.
    #[wire_enum(0x1B, "shop_buy")]
    ShopBuy,
    /// 0x1C — vendor sell container.
    #[wire_enum(0x1C, "shop_sell")]
    ShopSell,
    /// 0x1D — bank box.
    #[wire_enum(0x1D, "bank")]
    Bank,
    /// 0xFF — moving effect or target is not a character
    /// ([`Particle3DEffect`](crate::world::Particle3DEffect) only).
    #[wire_enum(0xFF, "moving_effect")]
    MovingEffect,
    /// Unknown / unrecognised layer byte.
    #[wire_enum(unknown)]
    Unknown(u8),
}
