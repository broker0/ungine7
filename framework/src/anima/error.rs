use std::fmt;

/// Errors that a controller or context may return.
#[derive(Debug, Clone)]
pub enum ControllerError {
    /// The requested entity was not found in the zone.
    EntityNotFound(u32),

    /// Movement rejected by the validator (impassable cell).
    MovementBlocked {
        serial: u32,
        x: u16,
        y: u16,
    },

    /// Action is prohibited for the current access level.
    AccessDenied {
        action: &'static str,
        level: super::traits::AccessLevel,
    },

    /// Arbitrary error with description (for script backends).
    Custom(String),
}

impl fmt::Display for ControllerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntityNotFound(serial) => {
                write!(f, "entity 0x{serial:08X} not found")
            }
            Self::MovementBlocked { serial, x, y } => {
                write!(f, "movement blocked for 0x{serial:08X} at ({x}, {y})")
            }
            Self::AccessDenied { action, level } => {
                write!(f, "access denied: '{action}' requires higher than {level:?}")
            }
            Self::Custom(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ControllerError {}
