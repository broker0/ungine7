//! Ultima Online data file readers (`.mul` / `.idx` / `.uop`).
//!
//! All MUL data files use **little-endian** byte order.
//!
//! # Architecture
//!
//! - [`color`] — shared color types (`Rgb`, `Rgba`) and RGB555 conversion
//! - [`mul`] — low-level MUL/IDX primitives: index parsing
//! - [`uop`] — UOP container reader (`.uop` package format)
//! - [`art`] — art sprite reader (`artidx.mul` + `art.mul`)
//! - [`cliloc`] — cliloc string table reader (`Cliloc*.enu`, `Cliloc*.deu`, …)
//! - [`hues`] — hue/palette table reader (`hues.mul`)
//! - [`map`] — map/terrain reader (`map{N}.mul` or `map{N}LegacyMUL.uop`)
//! - [`map_diff`] — map & statics diff reader (`mapdif{N}.mul`, `stadif{N}.mul`, etc.)
//! - [`multi`] — multi-object collection reader (`multi.mul` + `multi.idx`)
//! - [`radarcol`] — radar color table reader (`Radarcol.mul`)
//! - [`statics`] — world statics reader (`staidx{N}.mul` + `statics{N}.mul`)
//! - [`tiledata`] — tile metadata reader (`tiledata.mul`)

pub mod art;
pub mod cliloc;
pub mod color;
pub mod hues;
pub mod map;
pub mod map_diff;
pub mod mul;
pub mod multi;
pub mod radarcol;
pub mod statics;
pub mod tiledata;
pub mod uop;
