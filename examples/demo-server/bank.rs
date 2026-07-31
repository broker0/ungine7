//! NPC banker definitions: body graphics, names, and identification helpers.
//!
//! Bankers are ordinary [`MobileData`](common::uo_engine::entity::MobileData)
//! mobiles tagged with `ItemProps.meta["npc_type"] = "banker"`.  All
//! interaction logic lives in `game_session::bank_session`; this
//! module only provides the static data and identification utilities.

use common::uo_engine::item_props::ItemProps;

/// Meta-key used to tag a mobile as a banker NPC.
pub const META_NPC_TYPE: &str = "npc_type";

/// Meta-value that identifies a banker NPC.
pub const META_BANKER: &str = "banker";

/// Gump model for the bank box container.
///
/// `0x003C` is the standard large wooden chest gump used for bank boxes
/// in classic Ultima Online.
pub const BANK_GUMP_MODEL: u16 = 0x003C;

/// Default display name for a freshly spawned banker NPC.
pub const BANKER_NAME: &str = "the banker";

/// Body graphic for the banker (male human).
pub fn banker_body_graphic() -> u16 {
    crate::constants::body::MALE_HUMAN
}

/// Check whether `props` marks the entity as a banker NPC.
pub fn is_banker(props: &ItemProps) -> bool {
    props
        .get_meta_str(META_NPC_TYPE)
        .is_some_and(|s| s == META_BANKER)
}
