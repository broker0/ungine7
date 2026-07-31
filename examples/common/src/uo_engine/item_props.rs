//! Per-item property storage types for the demo server.
//!
//! [`ItemProps`] holds an [`ObjectText`] (unified name + tooltip lines +
//! revision), optional weight override, and free-form metadata for a single
//! item identified by its serial.
//!
//! These types are stored in a [`HashItemProps`](framework::continuum::HashItemProps)`<ItemProps>` inside the zone
//! and accessed via [`EngineCommand::GetItemProps`](crate::uo_engine::handler::EngineCommand::GetItemProps) /
//! [`EngineCommand::SetItemProps`](crate::uo_engine::handler::EngineCommand::SetItemProps).
//!
//! ## Text / tooltip model
//!
//! All text about an object (display name, property lines, localized cliloc
//! entries) lives in [`ObjectText`].  The first line is the title / name; the
//! remaining lines are tooltip property rows.
//!
//! [`ObjectText`] can be rendered two ways:
//! - `to_mega_cliloc` → `MegaClilocResponse` (0xD6) for AOS+ clients.
//! - `to_speech_lines` → one [`packets::speech::SendSpeech`] per line
//!   (classic / pre-AOS clients, including multi-line overhead or
//!   single-click responses).
//!
//! The demo-server currently uses `SingleClick` → `Speech` for names, and the
//! tooltip path is ready for `MegaClilocRequest` (0xD6) when the client
//! supports it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ── ObjectText ────────────────────────────────────────────────────────────

/// Unified text / tooltip model for any in-game object (item, mobile, multi).
///
/// Stores an ordered list of [`TextLine`] entries:
/// - `lines[0]` (if present) is the **title** / display name.
/// - `lines[1..]` are property rows (tooltip body).
///
/// A revision counter is bumped every time the text changes so that clients
/// can cache tooltips by `(serial, revision)`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ObjectText {
    /// Ordered text / cliloc lines.  May be empty.
    pub lines: Vec<TextLine>,
    /// Revision counter — bump on every change so clients re-fetch.
    pub revision: u32,
}

impl ObjectText {
    // ── Constructors ──────────────────────────────────────────────────────

    /// Create with a single title line (free text).
    pub fn with_title(title: impl Into<String>) -> Self {
        Self {
            lines: vec![TextLine::Text(title.into())],
            revision: 0,
        }
    }

    /// Create with a single title cliloc line.
    pub fn with_cliloc(id: u32, args: Option<String>) -> Self {
        Self {
            lines: vec![TextLine::Cliloc { id, args }],
            revision: 0,
        }
    }

    // ── Accessors ─────────────────────────────────────────────────────────

    /// `true` if there are no lines at all.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The title / display name, if present.
    ///
    /// Returns the text for a `Text` title, or a `[cliloc #{id}]` placeholder
    /// for a `Cliloc` title (full resolution requires a cliloc dictionary).
    pub fn title(&self) -> Option<&str> {
        self.lines.first().map(|l| l.as_display_str())
    }

    /// Title as an owned `String`, falling back to `None` if no lines.
    pub fn title_string(&self) -> Option<String> {
        self.title().map(str::to_string)
    }

    // ── Mutation ──────────────────────────────────────────────────────────

    /// Set or replace the title (first line), bumping the revision.
    pub fn set_title(&mut self, title: impl Into<String>) {
        let line = TextLine::Text(title.into());
        if self.lines.is_empty() {
            self.lines.push(line);
        } else {
            self.lines[0] = line;
        }
        self.revision += 1;
    }

    /// Set or replace the title cliloc, bumping the revision.
    pub fn set_title_cliloc(&mut self, id: u32, args: Option<String>) {
        let line = TextLine::Cliloc { id, args };
        if self.lines.is_empty() {
            self.lines.push(line);
        } else {
            self.lines[0] = line;
        }
        self.revision += 1;
    }

    /// Append a free-text property line, bumping the revision.
    pub fn push_text(&mut self, text: impl Into<String>) {
        self.lines.push(TextLine::Text(text.into()));
        self.revision += 1;
    }

    /// Append a cliloc property line, bumping the revision.
    pub fn push_cliloc(&mut self, id: u32, args: Option<String>) {
        self.lines.push(TextLine::Cliloc { id, args });
        self.revision += 1;
    }

    /// Clear all lines, bumping the revision.
    pub fn clear(&mut self) {
        if !self.lines.is_empty() {
            self.lines.clear();
            self.revision += 1;
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Build a `MegaClilocResponse` (0xD6) packet for this object.
    ///
    /// - `TextLine::Cliloc { id, args }` → `ClilocEntry { cliloc_id: id, text: args }`.
    /// - `TextLine::Text(s)` → `ClilocEntry { cliloc_id: 1042971, text: Some(s) }`.
    ///   Cliloc 1042971 is the generic `~1_val~` template; the client displays
    ///   the argument verbatim.
    pub fn to_mega_cliloc(
        &self,
        serial: u32,
    ) -> packets::tooltip::MegaClilocResponse {
        use packets::tooltip::{ClilocEntry, MegaClilocResponse};

        let entries: Vec<ClilocEntry> = self
            .lines
            .iter()
            .map(|l| match l {
                TextLine::Cliloc { id, args } => ClilocEntry {
                    cliloc_id: *id,
                    text: args.clone(),
                },
                TextLine::Text(s) => ClilocEntry {
                    cliloc_id: 1_042_971,
                    text: Some(s.clone()),
                },
            })
            .collect();

        MegaClilocResponse::new(serial, entries)
    }

    /// Build one `SendSpeech` (0x1C) overhead message per line.
    ///
    /// Suitable for pre-AOS clients: sends the title as normal overhead text
    /// and any property lines as system messages.
    ///
    /// The `serial`, `graphic`, and `color` parameters are forwarded into the
    /// packet so the message floats over the correct object.
    pub fn to_speech_lines(
        &self,
        serial: u32,
        graphic: u16,
        color: u16,
    ) -> Vec<packets::speech::SendSpeech> {
        use packets::speech::{SendSpeech, SpeechType};

        if self.lines.is_empty() {
            return Vec::new();
        }

        self.lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let text = line.as_display_str().to_string();
                let (speech_type, effective_serial, effective_graphic) = if i == 0 {
                    // Title: overhead on the object.
                    (SpeechType::Normal, serial, graphic)
                } else {
                    // Property rows: system corner message.
                    (SpeechType::System, 0xFFFF_FFFF, 0)
                };
                SendSpeech {
                    serial: effective_serial,
                    model: effective_graphic,
                    speech_type,
                    color,
                    font: 3,
                    name: String::new(),
                    message: text,
                }
            })
            .collect()
    }
}

// ── TextLine ──────────────────────────────────────────────────────────────

/// A single display / tooltip line — either a real UO cliloc or free text.
///
/// When serialized for `MegaClilocResponse` (0xD6):
/// - `Cliloc { id, args }` → sends the real cliloc ID + optional args.
/// - `Text(s)` → sends cliloc `1042971` (`~1_val~`) with the text as its
///   argument, so the client renders it verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TextLine {
    /// Real UO cliloc entry.
    ///
    /// `args` uses the UO tab-separated format: `"arg1\targ2\t..."`.
    Cliloc { id: u32, args: Option<String> },

    /// Free-form text (rendered via cliloc 1042971 on 0xD6 clients).
    Text(String),
}

impl TextLine {
    /// Returns the displayable string for this line.
    ///
    /// - `Text(s)` → `s.as_str()`
    /// - `Cliloc { id, args: None }` → `"[cliloc #<id>]"`  (no dict available)
    /// - `Cliloc { id, args: Some(a) }` → `a.as_str()`     (args as fallback)
    pub fn as_display_str(&self) -> &str {
        match self {
            TextLine::Text(s) => s.as_str(),
            TextLine::Cliloc { args: Some(a), .. } => a.as_str(),
            // Return a static placeholder; we need a &str lifetime here so
            // we cannot build an owned string.  The Display impl below can
            // be used when ownership is acceptable.
            TextLine::Cliloc { args: None, .. } => "[cliloc]",
        }
    }
}

impl std::fmt::Display for TextLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TextLine::Text(s) => f.write_str(s),
            TextLine::Cliloc { id: _, args: Some(a) } => write!(f, "{a}"),
            TextLine::Cliloc { id, args: None } => write!(f, "[cliloc #{id}]"),
        }
    }
}

// ── ItemProps ─────────────────────────────────────────────────────────────

/// Per-item property storage.
///
/// Keyed by item serial in a `HashItemProps<ItemProps>` inside the zone.
/// Applies to items in any tier (ground, equipped, container).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemProps {
    /// Unified text model: name (title) + tooltip body + revision counter.
    ///
    /// - `text.title()` → display name for SingleClick / vendor windows.
    /// - `text.lines` → ordered property rows for `MegaClilocResponse` (0xD6).
    /// - `text.revision` → client cache key; bumped on every change.
    pub text: ObjectText,

    /// Per-instance weight override in **1/10ths of a stone**.
    ///
    /// - `Some(15)` → this specific item weighs 1.5 stones per unit.
    /// - `None` → use the server's weight table / tiledata default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight_override: Option<u16>,

    /// Opaque key-value metadata.
    ///
    /// Game logic uses typed keys to store per-item state that doesn't
    /// map to a UO packet field.  Examples:
    ///
    /// - `"poison_charges"` → `MetaValue::Int(3)`
    /// - `"poison_type"`    → `MetaValue::Str("Lesser")`
    /// - `"crafted_by"`     → `MetaValue::Str("Blackthorn")`
    /// - `"durability"`     → `MetaValue::Int(50)`
    /// - `"durability_max"` → `MetaValue::Int(50)`
    /// - `"blessed"`        → `MetaValue::Bool(true)`
    pub meta: HashMap<String, MetaValue>,
}

impl ItemProps {
    // ── Constructors ──────────────────────────────────────────────────────

    /// Create props with just a name (title text line).
    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            text: ObjectText::with_title(name),
            ..Default::default()
        }
    }

    // ── Name helpers (convenience shims) ─────────────────────────────────

    /// Display name / title, if any.
    ///
    /// Mirrors the old `props.name` field for call-site compatibility.
    pub fn name(&self) -> Option<&str> {
        self.text.title()
    }

    /// Owned display name / title, if any.
    pub fn name_owned(&self) -> Option<String> {
        self.text.title_string()
    }

    /// Set or replace the display name (title), bumping the text revision.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.text.set_title(name);
    }

    // ── Tooltip revision (convenience shim) ──────────────────────────────

    /// Current revision of the text / tooltip.
    ///
    /// Mirrors the old `props.tooltip_revision` field.
    pub fn tooltip_revision(&self) -> u32 {
        self.text.revision
    }

    // ── Metadata ─────────────────────────────────────────────────────────

    /// Get a metadata value by key.
    pub fn get_meta(&self, key: &str) -> Option<&MetaValue> {
        self.meta.get(key)
    }

    /// Get a metadata integer value, or `None` if missing or wrong type.
    pub fn get_meta_int(&self, key: &str) -> Option<i64> {
        match self.meta.get(key)? {
            MetaValue::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// Get a metadata string value, or `None` if missing or wrong type.
    pub fn get_meta_str(&self, key: &str) -> Option<&str> {
        match self.meta.get(key)? {
            MetaValue::Str(v) => Some(v),
            _ => None,
        }
    }

    /// Get a metadata bool value, or `None` if missing or wrong type.
    pub fn get_meta_bool(&self, key: &str) -> Option<bool> {
        match self.meta.get(key)? {
            MetaValue::Bool(v) => Some(*v),
            _ => None,
        }
    }

    /// Set a metadata value.  Bumps `text.revision` automatically.
    pub fn set_meta(&mut self, key: impl Into<String>, value: MetaValue) {
        self.meta.insert(key.into(), value);
        self.text.revision += 1;
    }

    /// Remove a metadata value.  Bumps `text.revision` if the key existed.
    pub fn remove_meta(&mut self, key: &str) -> Option<MetaValue> {
        let removed = self.meta.remove(key);
        if removed.is_some() {
            self.text.revision += 1;
        }
        removed
    }
}

// ── MetaValue ─────────────────────────────────────────────────────────────

/// Typed metadata value for per-item properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MetaValue {
    /// Integer value (damage, charges, weight, …).
    Int(i64),
    /// Floating-point value (speed modifier, resist percentage, …).
    Float(f64),
    /// String value (creator name, inscription, …).
    Str(String),
    /// Boolean flag (blessed, insured, newbied, …).
    Bool(bool),
}
