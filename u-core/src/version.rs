use std::str::FromStr;

/// Client version identifier, used for selecting packet tables,
/// encryption keys, and feature sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub build: u32,
}

impl ProtocolVersion {
    pub const fn new(major: u32, minor: u32, patch: u32, build: u32) -> Self {
        Self { major, minor, patch, build }
    }

    // Well-known version boundaries
    /// Version <= 1.25.35 has a different login crypt, and most likely will not work
    /// Version 2.0.3.0 — boundary between Blowfish-only and Twofish game encryption.
    /// Clients < 2.0.3 use Blowfish only, == 2.0.3 use Blowfish+Twofish, > 2.0.3 use Twofish+MD5.
    pub const MINIMAL_SUPPORTED_CLIENT: Self = Self::new(1, 25, 36, 0);
    pub const DEFAULT: Self = Self::AOS_CLIENT;
    pub const BLOWFISH_TWOFISH_CLIENT: Self = Self::new(2, 0, 3, 0);
    pub const AOS_CLIENT: Self = Self::new(4, 0, 0, 0);  // Age of Shadows
    pub const CV_500A: Self  = Self::new(5, 0, 0, 1);     // packet table changes (0x0B, 0x16, 0x31)
    pub const CV_5090: Self  = Self::new(5, 0, 9, 0);     // packet 0xE1 becomes dynamic
    pub const EXT_SEED_CLIENT: Self = Self::new(6, 0, 5, 0);
    pub const CV_6013: Self  = Self::new(6, 0, 1, 3);     // packet table changes (0xE3, 0xE6-0xEA)
    pub const CV_6017: Self  = Self::new(6, 0, 1, 8);     // DropItem 0x08 = 15, ContainerContents 0x25 = 21
    pub const CV_6060: Self  = Self::new(6, 0, 6, 0);     // packets 0xEE, 0xEF, 0xF1
    /// Version 6.0.14.2+ — EnableFeatures (0xB9) switches from u16 to u32 flags.
    pub const EXT_FEATURES_CLIENT: Self = Self::new(6, 0, 14, 2);
    pub const SA_CLIENT: Self  = Self::new(7, 0, 0, 0);   // Stygian Abyss
    pub const CV_7090: Self    = Self::new(7, 0, 9, 0);    // High Seas (0x24, 0x99, 0xBA, 0xF1-0xF3)
    pub const CV_70180: Self   = Self::new(7, 0, 18, 0);   // CreateCharacter 0x00 = 0x6A
    /// Version 7.0.33.1+ — 0x78/0xD3 equipment list: hue word always present (9 bytes/item).
    pub const CV_70331: Self   = Self::new(7, 0, 33, 1);
    pub const CV_706400: Self  = Self::new(7, 0, 64, 0);   // packets 0xFA, 0xFB
    pub const CV_7010400: Self = Self::new(7, 0, 104, 0);  // packets 0xD5 = Fixed(9), 0xFD = Fixed(2)
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.major, self.minor, self.patch, self.build)
    }
}

impl FromStr for ProtocolVersion {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.split('.');
        let mut parse_next = |name: &str| -> Result<u32, String> {
            let part = parts.next().ok_or_else(|| format!("Missing {} version", name))?;
            part.parse::<u32>()
                .map_err(|_| format!("Invalid {} version '{}'", name, part))
        };

        let major = parse_next("major")?;
        let minor = parse_next("minor")?;
        let patch = parse_next("patch")?;
        let build = parse_next("build")?;

        if parts.next().is_some() {
            return Err(format!("Invalid version format '{}', too many parts", s));
        }
        Ok(Self { major, minor, patch, build })
    }
}
