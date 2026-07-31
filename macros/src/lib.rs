//! Procedural macros for binary serialization of UO packet structures
//! and Lua table conversions.
//!
//! Provides derive macros:
//!
//! - `#[derive(Decode)]` — generates `impl Decode<E>` for a struct
//! - `#[derive(Encode)]` — generates `impl Encode<E>` for a struct
//! - `#[derive(WireEnum)]` — generates a u8/u16/u32-backed enum with wire conversions
//! - `#[derive(Packet)]` — all-in-one for UO packet structs
//! - `#[derive(IntoLuaTable)]` — generates `impl IntoLua` that converts a struct/enum into a Lua table
//! - `#[derive(FromLuaTable)]` — generates `impl FromLua` that reads a struct from a Lua table
//!
//! Binary macros depend on types from the `io` crate at the path `::u_io`.
//! Lua macros depend on types from `mlua` at the path `::mlua`.

mod attrs;
mod wire_enum;
mod codegen;
mod decode;
mod encode;
mod from_lua_table;
mod into_lua_table;
mod lua_attrs;
mod packet;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derive `Encode<E>` for a struct with named fields.
///
/// # Struct-level attributes
///
/// - `#[binary(endian = "be")]` — generate `impl Encode<BE>`
/// - `#[binary(endian = "le")]` — generate `impl Encode<LE>`
/// - `#[binary(endian = "generic")]` — generate `impl<E: ByteOrder> Encode<E>` (default)
///
/// # Field-level attributes
///
/// - *(none)* — standard `T::encode(&self.field, writer)`
/// - `#[binary(pad = N)]` — write N zero bytes (field type must be `()`)
/// - `#[binary(const_value = EXPR)]` — write EXPR, ignore field value
/// - `#[binary(skip)]` — write nothing
/// - `#[binary(encode_with = "path")]` — call `path(&self.field, writer)`
/// - `#[binary(with = "path")]` — call `path::encode(&self.field, writer)`
#[proc_macro_derive(Encode, attributes(binary))]
pub fn derive_encode(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match encode::derive(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `Decode<E>` for a struct with named fields.
///
/// # Struct-level attributes
///
/// - `#[binary(endian = "be")]` — generate `impl Decode<BE>`
/// - `#[binary(endian = "le")]` — generate `impl Decode<LE>`
/// - `#[binary(endian = "generic")]` — generate `impl<E: ByteOrder> Decode<E>` (default)
///
/// # Field-level attributes
///
/// - *(none)* — standard `T::decode(reader)?`
/// - `#[binary(pad = N)]` — skip N bytes (field type must be `()`)
/// - `#[binary(const_value = EXPR)]` — read and validate == EXPR, error on mismatch
/// - `#[binary(skip)]` — `Default::default()`
/// - `#[binary(decode_with = "path")]` — call `path(reader)?`
/// - `#[binary(with = "path")]` — call `path::decode(reader)?`
#[proc_macro_derive(Decode, attributes(binary))]
pub fn derive_decode(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match decode::derive(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive a wire-typed enum with conversions, `Display`, `Decode`, `Encode`.
///
/// Requires `#[repr(u8)]`, `#[repr(u16)]`, or `#[repr(u32)]` on the enum.
///
/// # Variant attributes
///
/// - `#[wire_enum(VALUE)]` — known value; `Display` uses the variant name
/// - `#[wire_enum(VALUE, "display text")]` — known value with explicit display string
/// - `#[wire_enum(unknown)]` — catch-all variant (must be `VariantName(T)` where T matches repr)
///
/// # Generated API
///
/// - `from_wire(T) -> Self` — infallible when `unknown` variant is present
/// - `from_wire(T) -> Result<Self, DecodeError>` — fallible when no `unknown` variant
/// - `to_wire(&self) -> T`
/// - `Display` — writes display string or variant name; unknown → `"unknown (0xNNNN)"`
/// - `Decode<E>` — reads T via `read_u8/read_u16/read_u32`
/// - `Encode<E>` — writes T via `put_u8/put_u16/put_u32`
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, Clone, Copy, PartialEq, Eq, WireEnum)]
/// #[repr(u16)]
/// pub enum ChatSpeakerType {
///     #[wire_enum(0x0030, "User")]
///     User,
///     #[wire_enum(0x0031)]
///     Moderator,
///     #[wire_enum(unknown)]
///     Unknown(u16),
/// }
/// ```
#[proc_macro_derive(WireEnum, attributes(wire_enum))]
pub fn derive_wire_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match wire_enum::derive(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `Decode<E>`, `Encode<E>`, and `BasicPacket` for a UO packet struct.
///
/// This is an all-in-one derive that replaces separate `Decode` + `Encode`
/// derives plus a manual `impl BasicPacket`.
///
/// # Struct-level attributes
///
/// - `#[packet(id = 0xNN)]` — the packet command byte (**required**)
/// - `#[packet(size = fixed(N))]` — fixed-size packet of N bytes
/// - `#[packet(size = dynamic)]` — variable-length packet (bytes 1–2 = u16 length)
/// - `#[packet(endian = "be")]` — byte order: `"be"` (default) or `"le"`
///
/// # Field-level attributes
///
/// Same `#[binary(...)]` attributes as `Decode`/`Encode`:
/// - `#[binary(pad = N)]`, `#[binary(const_value = EXPR)]`, `#[binary(skip)]`
/// - `#[binary(with = "path")]` (uses `path::decode` + `path::encode`)
/// - `#[binary(decode_with = "path")]`, `#[binary(encode_with = "path")]`
///
/// # Example
///
/// ```ignore
/// #[derive(Debug, Clone, PartialEq, Eq, Packet)]
/// #[packet(id = 0x73, size = fixed(2), endian = "be")]
/// pub struct Ping {
///     pub id: u8,
///     pub sequence: u8,
/// }
/// ```
#[proc_macro_derive(Packet, attributes(packet, binary))]
pub fn derive_packet(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match packet::derive(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `IntoLua` for a struct or enum, converting it into a Lua table.
///
/// # Container-level attributes
///
/// - `#[lua(tag = "type")]` — (enums) key name for the discriminant string (default: `"type"`)
/// - `#[lua(rename_all = "snake_case")]` — (enums) auto-convert variant names
///
/// # Field-level attributes
///
/// - `#[lua(rename = "name")]` — use a different key name in the Lua table
/// - `#[lua(skip)]` — do not include this field
/// - `#[lua(skip_none)]` — only set the key when the `Option` is `Some`
/// - `#[lua(into_via = "path")]` — convert the value through `path(value)` before setting
///
/// # Variant-level attributes (enums)
///
/// - `#[lua(rename = "name")]` — override the auto-generated tag value
/// - `#[lua(skip)]` — generate `unreachable!()` for this variant
///
/// # Example
///
/// ```ignore
/// #[derive(IntoLuaTable)]
/// struct MobileData {
///     serial: u32,
///     #[lua(rename = "str")]
///     str_: u16,
///     #[lua(skip)]
///     internal: Vec<u8>,
///     #[lua(skip_none)]
///     color: Option<u16>,
///     #[lua(into_via = "notoriety_to_u8")]
///     notoriety: Notoriety,
/// }
///
/// #[derive(IntoLuaTable)]
/// #[lua(tag = "type", rename_all = "snake_case")]
/// enum WorldEvent {
///     EntityMoved { serial: u32, x: u16, y: u16 },
///     SoundPlayed { sound_id: u16, x: u16, y: u16, z: i16 },
/// }
/// ```
#[proc_macro_derive(IntoLuaTable, attributes(lua))]
pub fn derive_into_lua_table(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match into_lua_table::derive(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derive `FromLua` for a struct, reading its fields from a Lua table.
///
/// # Field-level attributes
///
/// - `#[lua(rename = "name")]` — read from a different key name
/// - `#[lua(skip)]` — use `Default::default()` instead of reading
/// - `#[lua(default)]` — use `Default::default()` when the key is absent or nil
/// - `#[lua(default = EXPR)]` — use `EXPR` when the key is absent or nil
/// - `#[lua(from_via = "path")]` — convert the raw value through `path(value)` after reading
///
/// When **all** fields have defaults (`default` or `default = ...`), the generated
/// `FromLua` also accepts `Value::Nil`, returning a struct with all defaults.
///
/// # Example
///
/// ```ignore
/// #[derive(FromLuaTable)]
/// struct EffectParams {
///     #[lua(default = 2)]
///     direction_type: u8,
///     #[lua(default = 10)]
///     speed: u8,
///     #[lua(default)]
///     x: u16,
/// }
/// ```
#[proc_macro_derive(FromLuaTable, attributes(lua))]
pub fn derive_from_lua_table(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match from_lua_table::derive(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
