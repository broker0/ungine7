//! Shared parameter types for Lua script bindings.
//!
//! Both the async runtime and the controller-mode runtime expose
//! `effect()`, `animate()`, and `say()` methods to Lua scripts.  The
//! parameter tables have identical structure and defaults — this module
//! provides a single source of truth for parsing them.

// ── Effect parameters ────────────────────────────────────────────────────

/// Parsed parameters for a graphical effect.
#[derive(macros::FromLuaTable)]
pub struct EffectParams {
    #[lua(default = 2)]
    pub direction_type: u8,
    #[lua(default)]
    pub source_serial: u32,
    #[lua(default)]
    pub target_serial: u32,
    #[lua(default)]
    pub graphic: u16,
    #[lua(default)]
    pub x: u16,
    #[lua(default)]
    pub y: u16,
    #[lua(default)]
    pub z: i8,
    #[lua(default)]
    pub target_x: u16,
    #[lua(default)]
    pub target_y: u16,
    #[lua(default)]
    pub target_z: i8,
    #[lua(default = 10)]
    pub speed: u8,
    #[lua(default = 30)]
    pub duration: u8,
    #[lua(default = true)]
    pub fixed_direction: bool,
    #[lua(default)]
    pub explode: bool,
}

// ── Animation options ────────────────────────────────────────────────────

/// Parsed optional parameters for `animate()`.
#[derive(macros::FromLuaTable)]
pub struct AnimateOpts {
    #[lua(default = 1)]
    pub repeat_count: u16,
    #[lua(default)]
    pub reverse: bool,
    #[lua(default)]
    pub repeat: bool,
    #[lua(default)]
    pub frame_delay: u8,
}

// ── Speech options ───────────────────────────────────────────────────────

/// Parsed optional parameters for `say()`.
#[derive(macros::FromLuaTable)]
pub struct SayOpts {
    #[lua(default)]
    pub speech_type: u8,
    #[lua(default = 0x03B2)]
    pub color: u16,
    #[lua(default = 3)]
    pub font: u16,
    #[lua(default)]
    pub name: Option<String>,
}
