use u_core::ProtocolVersion;
use u_io::packet::PacketSize;
use u_io::PacketSize::{Dynamic, Fixed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketLengthError {
    InsufficientData,
    UnknownPacket,
    InvalidDynamicLength(u16),
}

impl std::fmt::Display for PacketLengthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientData => write!(f, "insufficient data to determine packet length"),
            Self::UnknownPacket => write!(f, "unknown packet command byte"),
            Self::InvalidDynamicLength(len) =>
                write!(f, "dynamic packet length {len} is less than minimum (3)"),
        }
    }
}

impl std::error::Error for PacketLengthError {}


/// UO packet size table with support for versioned patches.
///
/// Each entry is `Option<PacketSize>` — `None` means unknown packet.
#[derive(Debug, Copy, Clone)]
pub struct PacketTable {
    table: &'static PacketTableData,
}

impl PacketTable {
    /// Select the appropriate packet table for a client version.
    pub fn new(version: ProtocolVersion) -> Self {
        let table = match version {
            v if v >= ProtocolVersion::CV_7010400 => &CV_7010400_TABLE,
            v if v >= ProtocolVersion::CV_706400  => &CV_706400_TABLE,
            v if v >= ProtocolVersion::CV_70180   => &CV_70180_TABLE,
            v if v >= ProtocolVersion::CV_7090    => &CV_7090_TABLE,
            v if v >= ProtocolVersion::SA_CLIENT  => &SA_TABLE,
            v if v >= ProtocolVersion::EXT_FEATURES_CLIENT => &CV_60142_TABLE,
            v if v >= ProtocolVersion::CV_6060    => &CV_6060_TABLE,
            v if v >= ProtocolVersion::CV_6017    => &CV_6017_TABLE,
            v if v >= ProtocolVersion::CV_6013    => &CV_6013_TABLE,
            v if v >= ProtocolVersion::CV_5090    => &CV_5090_TABLE,
            v if v >= ProtocolVersion::CV_500A    => &CV_500A_TABLE,
            v if v >= ProtocolVersion::AOS_CLIENT => &AOS_TABLE,
            _ => &LEGACY_TABLE,
        };
        Self { table }
    }

    #[inline]
    pub fn is_dynamic(&self, cmd: u8) -> bool {
        matches!(self.get(cmd), Some(PacketSize::Dynamic))
    }

    #[inline]
    pub fn is_unknown(&self, cmd: u8) -> bool {
        self.get(cmd).is_none()
    }

    #[inline]
    pub fn get(&self, cmd: u8) -> Option<PacketSize> {
        self.table[cmd as usize]
    }

    pub fn get_packet_length(&self, data: &[u8]) -> Result<usize, PacketLengthError> {
        if data.is_empty() {
            return Err(PacketLengthError::InsufficientData);
        }

        match self.get(data[0]) {
            Some(PacketSize::Fixed(size)) => Ok(size as usize),
            None => Err(PacketLengthError::UnknownPacket),
            Some(PacketSize::Dynamic) => {
                let length_bytes = data.get(1..3).ok_or(PacketLengthError::InsufficientData)?;
                let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]);
                if length < 3 {
                    Err(PacketLengthError::InvalidDynamicLength(length))
                } else {
                    Ok(length as usize)
                }
            }
        }
    }
}


// ── Raw Data & Compile-Time Conversion ──────────────────────────────────────

type PacketTableData = [Option<PacketSize>; 256];

const fn parse_raw_table(raw: &[u16; 256]) -> PacketTableData {
    const DYNAMIC: u16 = 0x0000;
    const UNKNOWN: u16 = 0xFFFF;

    let mut table = [None; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = match raw[i] {
            DYNAMIC => Some(PacketSize::Dynamic),
            UNKNOWN => None,
            fixed   => Some(PacketSize::Fixed(fixed)),
        };
        i += 1;
    }
    table
}

const RAW_LEGACY: [u16; 256] = [
    0x0068, 0x0005, 0x0007, 0x0000, 0x0002, 0x0005, 0x0005, 0x0007, // 0x00
    0x000e, 0x0005, 0x0007, 0x0007, 0x0000, 0x0003, 0x0000, 0x003d, // 0x08
    0x00d7, 0x0000, 0x0000, 0x000a, 0x0006, 0x0009, 0x0001, 0x0000, // 0x10
    0x0000, 0x0000, 0x0000, 0x0025, 0x0000, 0x0005, 0x0004, 0x0008, // 0x18
    0x0013, 0x0008, 0x0003, 0x001a, 0x0007, 0x0014, 0x0005, 0x0002, // 0x20
    0x0005, 0x0001, 0x0005, 0x0002, 0x0002, 0x0011, 0x000f, 0x000a, // 0x28
    0x0005, 0x0001, 0x0002, 0x0002, 0x000a, 0x028d, 0x0000, 0x0008, // 0x30
    0x0007, 0x0009, 0x0000, 0x0000, 0x0000, 0x0002, 0x0025, 0x0000, // 0x38
    0x00c9, 0x0000, 0x0000, 0x0229, 0x02c9, 0x0005, 0x0000, 0x000b, // 0x40
    0x0049, 0x005d, 0x0005, 0x0009, 0x0000, 0x0000, 0x0006, 0x0002, // 0x48
    0x0000, 0x0000, 0x0000, 0x0002, 0x000c, 0x0001, 0x000b, 0x006e, // 0x50
    0x006a, 0x0000, 0x0000, 0x0004, 0x0002, 0x0049, 0x0000, 0x0031, // 0x58
    0x0005, 0x0009, 0x000f, 0x000d, 0x0001, 0x0004, 0x0000, 0x0015, // 0x60
    0x0000, 0x0000, 0x0003, 0x0009, 0x0013, 0x0003, 0x000e, 0x0000, // 0x68
    0x001c, 0x0000, 0x0005, 0x0002, 0x0000, 0x0023, 0x0010, 0x0011, // 0x70
    0x0000, 0x0009, 0x0000, 0x0002, 0x0000, 0x000d, 0x0002, 0x0000, // 0x78
    0x003e, 0x0000, 0x0002, 0x0027, 0x0045, 0x0002, 0x0000, 0x0000, // 0x80
    0x0042, 0x0000, 0x0000, 0x0000, 0x000b, 0x0000, 0x0000, 0x0000, // 0x88
    0x0013, 0x0041, 0x0000, 0x0063, 0x0000, 0x0009, 0x0000, 0x0002, // 0x90
    0x0000, 0x001a, 0x0000, 0x0102, 0x0135, 0x0033, 0x0000, 0x0000, // 0x98
    0x0003, 0x0009, 0x0009, 0x0009, 0x0095, 0x0000, 0x0000, 0x0004, // 0xA0
    0x0000, 0x0000, 0x0005, 0x0000, 0x0000, 0x0000, 0x0000, 0x000d, // 0xA8
    0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0040, 0x0009, 0x0000, // 0xB0
    0x0000, 0x0003, 0x0006, 0x0009, 0x0003, 0x0000, 0x0000, 0x0000, // 0xB8
    0x0024, 0x0000, 0x0000, 0x0000, 0x0006, 0x00cb, 0x0001, 0x0031, // 0xC0
    0x0002, 0x0006, 0x0006, 0x0007, 0x0000, 0x0001, 0x0000, 0x004e, // 0xC8
    0x0000, 0x0002, 0x0019, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, // 0xD0
    0x0000, 0x010C, 0xFFFF, 0xFFFF, 0x0009, 0x0000, 0xFFFF, 0x0000, // 0xD8
    0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, // 0xE0
    0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0x0015, // 0xE8
    0x0000, 0x0009, 0xFFFF, 0x001a, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, // 0xF0
    0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF, // 0xF8
];

const LEGACY_TABLE: PacketTableData = parse_raw_table(&RAW_LEGACY);

const AOS_TABLE: PacketTableData = {
    let /*mut*/ table = LEGACY_TABLE;
    // TODO changes
    table
};

/// >= 5.0.0.1 (CV_500A)
const CV_500A_TABLE: PacketTableData = {
    let mut table = AOS_TABLE;
    table[0x0B] = Some(Fixed(7));     // CharacterStatus
    table[0x16] = Some(Dynamic);      // NewHealthBarStatus (SA)
    table[0x17] = Some(Fixed(12));    // OldHealthBarStatus (KR)
    table[0x31] = Some(Dynamic);      // unknown packet
    table[0x3B] = Some(Dynamic);      // BuyItems (C→S)
    table[0x9F] = Some(Dynamic);      // SellListReply (C→S)
    table
};

/// >= 5.0.9.0 (CV_5090)
const CV_5090_TABLE: PacketTableData = {
    let mut table = CV_500A_TABLE;
    table[0xE1] = Some(Dynamic);      // unknown packet
    table
};

/// >= 6.0.1.3 (CV_6013)
const CV_6013_TABLE: PacketTableData = {
    let mut table = CV_5090_TABLE;
    table[0xE3] = Some(Dynamic);      // unknown packet
    table[0xE6] = Some(Fixed(5));     // RemoveWaypoint
    table[0xE7] = Some(Fixed(12));    // unknown packet
    table[0xE8] = Some(Fixed(13));    // unknown packet
    table[0xE9] = Some(Fixed(75));    // unknown packet
    table[0xEA] = Some(Fixed(3));     // unknown packet
    table
};

/// >= 6.0.1.8 (CV_6017)
const CV_6017_TABLE: PacketTableData = {
    let mut table = CV_6013_TABLE;
    table[0x08] = Some(Fixed(15));    // DropItem (SA format)
    table[0x25] = Some(Fixed(21));    // ContainerContents
    table
};

/// >= 6.0.6.0 (CV_6060)
const CV_6060_TABLE: PacketTableData = {
    let mut table = CV_6017_TABLE;
    table[0xEE] = Some(Fixed(0x2000)); // unknown packet
    table[0xEF] = Some(Fixed(0x2000)); // unknown packet
    table[0xF1] = Some(Fixed(9));      // unknown packet
    table
};

/// >= 6.0.14.2 (CV_60142) — EnableFeatures (0xB9) switches from u16 to u32.
const CV_60142_TABLE: PacketTableData = {
    let mut table = CV_6060_TABLE;
    table[0xB9] = Some(Fixed(5));     // EnableFeatures (extended)
    table
};

/// >= 7.0.0.0 (SA_CLIENT) — Stygian Abyss.
const SA_TABLE: PacketTableData = {
    let mut table = CV_60142_TABLE;
    table[0x00] = None;               // CreateCharacter (handled separately as seed)
    table[0x43] = Some(Fixed(524));   // unknown packet
    table[0xDE] = Some(Dynamic);      // UpdateMobileStatus
    table[0xE2] = Some(Fixed(10));    // NewCharacterAnimation (KR)
    table[0xE5] = Some(Dynamic);      // DisplayWaypoint
    table[0xEB] = Some(Dynamic);      // unknown packet
    table[0xEE] = Some(Fixed(10));    // unknown packet (overrides CV_6060 value)
    table[0xEF] = Some(Fixed(21));    // unknown packet (overrides CV_6060 value)
    table[0xF8] = Some(Fixed(106));   // CharacterCreation (7.0.16.0)
    table[0xF9] = Some(Dynamic);      // unknown packet
    table
};

/// >= 7.0.9.0 (CV_7090) — High Seas.
const CV_7090_TABLE: PacketTableData = {
    let mut table = SA_TABLE;
    table[0x24] = Some(Fixed(9));     // DrawContainer
    table[0x99] = Some(Fixed(30));    // BoatHousePlacementView
    table[0xBA] = Some(Fixed(10));    // QuestArrow
    table[0xF1] = Some(Fixed(9));     // unknown packet
    table[0xF2] = Some(Fixed(25));    // unknown packet
    table[0xF3] = Some(Fixed(26));    // ObjectInfoSA
    table[0xF5] = Some(Fixed(21));    // NewMapMessage
    table[0xF7] = Some(Dynamic);      // PacketList
    table
};

/// >= 7.0.18.0 (CV_70180)
const CV_70180_TABLE: PacketTableData = {
    let mut table = CV_7090_TABLE;
    table[0x00] = Some(Fixed(106));   // CreateCharacter (extended)
    table
};

/// >= 7.0.64.0 (CV_706400)
const CV_706400_TABLE: PacketTableData = {
    let mut table = CV_70180_TABLE;
    table[0xFA] = Some(Fixed(1));     // unknown packet
    table[0xFB] = Some(Fixed(2));     // UpdateViewPublicHouseContents
    table
};

/// >= 7.0.104.0 (CV_7010400)
const CV_7010400_TABLE: PacketTableData = {
    let mut table = CV_706400_TABLE;
    table[0xD5] = Some(Fixed(9));     // unknown packet
    table[0xFD] = Some(Fixed(2));     // unknown packet
    table
};
