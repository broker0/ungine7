// detection/version_db.rs

use u_core::ProtocolVersion;

/// Database of known client versions.
#[derive(Debug, Clone)]
pub struct VersionDatabase {
    versions: Vec<ProtocolVersion>,
}

impl VersionDatabase {
    pub fn new(versions: Vec<ProtocolVersion>) -> Self {
        Self { versions }
    }

    /// Test all possible versions (slow, but complete)
    pub fn exhaustive(major_range: std::ops::RangeInclusive<u32>, max_minor: u32, max_rev: u32) -> Self {
        let mut versions = Vec::new();
        for major in major_range {
            for minor in 0..=max_minor {
                for patch in 0..=max_rev {
                    versions.push(ProtocolVersion { major, minor, patch, build: 0 });
                }
            }
        }
        Self { versions }
    }

    /// Pre-installed set of popular versions
    pub fn common() -> Self {
        Self::new(vec![
            ProtocolVersion { major: 1, minor: 26, patch: 4, build: 0  },
            ProtocolVersion { major: 2, minor: 0, patch: 3, build: 0  },
            ProtocolVersion { major: 3, minor: 0, patch: 8, build: 0  },
            ProtocolVersion { major: 7, minor: 0, patch: 95, build: 0  },
            ProtocolVersion { major: 7, minor: 0, patch: 90, build: 0  },
            ProtocolVersion { major: 7, minor: 0, patch: 85, build: 0  },
            ProtocolVersion { major: 7, minor: 0, patch: 80, build: 0  },
            ProtocolVersion { major: 5, minor: 0, patch: 9, build: 0  },
            ProtocolVersion { major: 5, minor: 0, patch: 8, build: 0  },
            ProtocolVersion { major: 4, minor: 0, patch: 11, build: 0  },
        ])
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProtocolVersion> {
        self.versions.iter()
    }
}
