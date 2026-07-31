//! Cliloc string table reader.
//!
//! Cliloc files (`Cliloc.enu`, `Cliloc.deu`, `Cliloc.jpn`, …) contain
//! the localised text strings used by the Ultima Online client.  Each
//! entry maps a numeric ID (`u32`) to a UTF-8 text string that may
//! contain interpolation placeholders such as `~1_val~`, `~2_name~`.
//!
//! A single UO client installation may ship **many** files with a
//! language extension (`.enu`, `.deu`, …).  Only files whose stem
//! starts with `Cliloc` (case-insensitive) contain the ID→text table;
//! other files (`Chat.enu`, `Tilehelp.enu`, `Skill*.enu`, …) use an
//! IFF/FORM container format and are silently skipped.
//!
//! # File formats
//!
//! ## Plain cliloc (magic ≤ 0xFF, typically 0x01 or 0x02)
//!
//! ```text
//! Header (6 bytes):
//!   magic1  : u32   (version / signature, ≤ 0xFF)
//!   magic2  : u16   (additional marker)
//!
//! Entries (repeat until EOF):
//!   id      : u32   (cliloc number)
//!   flag    : u8    (unused, typically 0)
//!   length  : u16   (byte length of text)
//!   text    : [u8; length]  (UTF-8, NOT null-terminated)
//! ```
//!
//! ## BWT-encrypted cliloc (magic > 0xFF, magic ≠ 0x4D524F46)
//!
//! The entire file is compressed with a Move-To-Front + BWT transform.
//! The first `u32` XOR `0x8E2C9A3D` yields the decompressed size;
//! bytes `[4..]` are the compressed payload (1024-byte frequency table
//! followed by the MTF-encoded body).  After decompression the result
//! is a plain cliloc stream.
//!
//! ## IFF/FORM container (magic = 0x4D524F46 = `"FORM"`)
//!
//! Files like `Chat.enu`, `Tilehelp.enu`, `Skill*.enu`, `Intloc*.enu`,
//! and some numbered `Cliloc00.enu` / `CLILOC02.ENU` patches use an
//! IFF container with `LANGINFO` + `TEXT` chunks containing HTML-like
//! content.  These are **not** cliloc ID→text tables and are silently
//! skipped.
//!
//! # Usage
//!
//! ```rust,ignore
//! use files::cliloc::ClilocTable;
//! use std::path::Path;
//!
//! // Load all cliloc files for a language from the client directory.
//! let table = ClilocTable::read(Path::new("path/to/client"), "enu")?;
//!
//! // Look up a string by ID.
//! if let Some(text) = table.get(1042971) {
//!     println!("{text}");  // "~1_val~"
//! }
//!
//! // Format with arguments.
//! let formatted = ClilocTable::format("You see: ~1_val~", &["a longsword"]);
//! assert_eq!(formatted, "You see: a longsword");
//! ```

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use log::{debug, warn};
use u_io::{Decode, DecodeError, ReadPrimitives, StreamReader, LE};

// ── Constants ──────────────────────────────────────────────────────────────

/// Size of the plain-format file header in bytes.
const HEADER_SIZE: usize = 6;

/// IFF/FORM magic (`"FORM"` as big-endian u32, stored LE on disk →
/// bytes `46 4F 52 4D` → LE u32 = `0x4D524F46`).
const FORM_MAGIC: u32 = 0x4D52_4F46;

/// XOR mask applied to the first DWORD of a BWT-encrypted cliloc to
/// recover the decompressed size.
const BWT_XOR_MASK: u32 = 0x8E2C_9A3D;

/// Size of the BWT frequency table in bytes (256 × 4-byte LE integers).
const BWT_FREQ_TABLE_SIZE: usize = 256 * 4;

// ── ClilocTable ────────────────────────────────────────────────────────────

/// Loaded cliloc string table.
///
/// Provides O(1) lookup of localised text strings by their numeric ID,
/// as well as argument interpolation for placeholder strings.
#[derive(Debug, Clone)]
pub struct ClilocTable {
    entries: HashMap<u32, String>,
}

impl ClilocTable {
    /// Load **all** cliloc files for the given language from a directory.
    ///
    /// Scans `dir` for files matching the pattern `Cliloc*.<lang>`
    /// (case-insensitive), reads each one, and merges the results.
    /// Later files overwrite earlier entries with the same ID.
    ///
    /// Files that use the IFF/FORM container format (e.g. patched
    /// `Cliloc00.enu`) are silently skipped — they do not contain
    /// cliloc ID→text data.
    ///
    /// `lang` is the file extension without the dot, e.g. `"enu"`,
    /// `"deu"`, `"jpn"`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let table = ClilocTable::read(Path::new("C:/UO/client"), "enu")?;
    /// ```
    pub fn read(dir: &Path, lang: &str) -> io::Result<Self> {
        let lang_lower = lang.to_ascii_lowercase();
        let mut paths: Vec<_> = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let matches = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case(&lang_lower));

            if !matches {
                continue;
            }

            let is_cliloc = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    stem.len() >= 6 && stem[..6].eq_ignore_ascii_case("cliloc")
                });

            if is_cliloc {
                paths.push(path);
            }
        }

        paths.sort();

        if paths.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no cliloc files found in {} for language '{lang}'", dir.display()),
            ));
        }

        let mut table = Self::empty();
        let mut loaded = 0usize;
        let mut skipped_form = 0usize;

        for path in &paths {
            match Self::read_file(path) {
                Ok(single) => {
                    debug!(
                        "{}: {} entries",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        single.len(),
                    );
                    table.merge(single);
                    loaded += 1;
                }
                Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
                    // IFF/FORM file — silently skip (not a cliloc table).
                    debug!(
                        "{}: skipped (FORM/IFF container)",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                    );
                    skipped_form += 1;
                }
                Err(e) => {
                    warn!(
                        "{}: skipped ({})",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        e,
                    );
                }
            }
        }

        if loaded == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "none of the {} cliloc files in {} could be loaded \
                     ({skipped_form} FORM/IFF skipped)",
                    paths.len(),
                    dir.display(),
                ),
            ));
        }

        debug!(
            "cliloc ({lang}): loaded {loaded}/{} files, {} total entries from {} \
             ({skipped_form} FORM/IFF skipped)",
            paths.len(),
            table.len(),
            dir.display(),
        );

        Ok(table)
    }

    /// Load a single cliloc file from the given path.
    ///
    /// Handles three formats:
    /// - **Plain** (magic ≤ 0xFF) — parsed directly.
    /// - **BWT-encrypted** (magic > 0xFF, ≠ FORM) — decrypted, then parsed.
    /// - **IFF/FORM** (magic = `"FORM"`) — returns `ErrorKind::InvalidInput`
    ///   (these are not cliloc tables).
    pub fn read_file(path: &Path) -> io::Result<Self> {
        let data = fs::read(path)?;

        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "cliloc file too small ({} bytes): {}",
                    data.len(),
                    path.display(),
                ),
            ));
        }

        let magic1 = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);

        if magic1 == FORM_MAGIC {
            // IFF/FORM container — not a cliloc ID→text table.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("IFF/FORM container, not a cliloc table: {}", path.display()),
            ));
        }

        if magic1 <= 0xFF {
            // Plain cliloc — parse directly.
            if data.len() < HEADER_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "cliloc file too small ({} bytes, minimum {HEADER_SIZE}): {}",
                        data.len(),
                        path.display(),
                    ),
                ));
            }
            return Ok(Self::from_stream(io::Cursor::new(data))?);
        }

        // BWT-encrypted cliloc.
        let decrypted = bwt_decrypt(&data).map_err(|msg| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {}", path.display(), msg),
            )
        })?;

        if decrypted.len() < HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "decrypted cliloc too small ({} bytes): {}",
                    decrypted.len(),
                    path.display(),
                ),
            ));
        }

        Ok(Self::from_stream(io::Cursor::new(decrypted))?)
    }

    /// Parse a cliloc table from any [`Read`] stream containing the
    /// **plain** (unencrypted) format.
    pub fn from_stream<R: Read>(stream: R) -> Result<Self, DecodeError> {
        let mut reader = StreamReader::<_, LE>::new(stream);
        parse(&mut reader)
    }

    /// Create an empty table.
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Build from a pre-populated map (useful for testing or programmatic
    /// construction).
    pub fn from_entries(entries: HashMap<u32, String>) -> Self {
        Self { entries }
    }

    // ── Lookup ─────────────────────────────────────────────────────────

    /// Look up a cliloc string by its numeric ID.
    pub fn get(&self, id: u32) -> Option<&str> {
        self.entries.get(&id).map(String::as_str)
    }

    /// Look up a cliloc string and interpolate `args` into its placeholders.
    ///
    /// Placeholders have the form `~N_tag~` where `N` is a 1-based index.
    /// `args` are tab-separated values matching the UO protocol convention;
    /// this method accepts a pre-split slice.
    ///
    /// Returns `None` if `id` is not present in the table.
    pub fn get_formatted(&self, id: u32, args: &[&str]) -> Option<String> {
        self.get(id).map(|text| Self::format(text, args))
    }

    // ── Interpolation ──────────────────────────────────────────────────

    /// Substitute `~N_tag~` placeholders in `text` with positional `args`.
    ///
    /// `N` is a **1-based** index into `args`.  The `_tag` suffix is
    /// arbitrary and ignored — only the leading digit(s) select the
    /// argument.  If `N` is out of range the placeholder is left as-is.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// assert_eq!(
    ///     ClilocTable::format("~1_val~ gold coins", &["500"]),
    ///     "500 gold coins",
    /// );
    /// assert_eq!(
    ///     ClilocTable::format("~1_NAME~ hits ~2_NAME~ for ~3_val~ damage", &["You", "orc", "15"]),
    ///     "You hits orc for 15 damage",
    /// );
    /// ```
    pub fn format(text: &str, args: &[&str]) -> String {
        let mut result = String::with_capacity(text.len() + 32);
        let mut chars = text.char_indices().peekable();

        while let Some((i, ch)) = chars.next() {
            if ch != '~' {
                result.push(ch);
                continue;
            }

            // Try to parse `~N_tag~` starting after the opening `~`.
            // Find the closing `~`.
            let rest = &text[i + 1..];
            if let Some(close) = rest.find('~') {
                let placeholder = &rest[..close];

                // Extract the leading digits to determine the argument index.
                let digit_end = placeholder
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(placeholder.len());

                if digit_end > 0 {
                    if let Ok(n) = placeholder[..digit_end].parse::<usize>() {
                        if n >= 1 && n <= args.len() {
                            result.push_str(args[n - 1]);
                            // Advance past the closing `~`.
                            for _ in 0..close + 1 {
                                chars.next();
                            }
                            continue;
                        }
                    }
                }

                // Not a valid placeholder — emit the `~` literally.
                result.push('~');
            } else {
                // No closing `~` — emit literally.
                result.push('~');
            }
        }

        result
    }

    /// Parse a tab-separated argument string (as sent on the UO wire)
    /// and format a cliloc template with the resulting values.
    ///
    /// Returns `None` if `id` is not present in the table.
    pub fn get_formatted_raw(&self, id: u32, tab_args: &str) -> Option<String> {
        let args: Vec<&str> = if tab_args.is_empty() {
            Vec::new()
        } else {
            tab_args.split('\t').collect()
        };
        self.get_formatted(id, &args)
    }

    // ── Mutation ────────────────────────────────────────────────────────

    /// Merge another table into this one.
    ///
    /// Entries from `other` overwrite entries in `self` when IDs collide.
    pub fn merge(&mut self, other: ClilocTable) {
        self.entries.extend(other.entries);
    }

    /// Insert a single entry.  Returns the previous text for this ID, if
    /// any.
    pub fn insert(&mut self, id: u32, text: String) -> Option<String> {
        self.entries.insert(id, text)
    }

    // ── Accessors ──────────────────────────────────────────────────────

    /// Total number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all `(id, text)` pairs in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &str)> {
        self.entries.iter().map(|(&id, text)| (id, text.as_str()))
    }

    /// Get a reference to the underlying map.
    pub fn as_map(&self) -> &HashMap<u32, String> {
        &self.entries
    }
}

// ── Plain-format parser ────────────────────────────────────────────────────

/// Try to read one entry.  Returns `Ok(Some(...))` on success,
/// `Ok(None)` on clean EOF, or `Err` on a real decode failure.
fn read_entry<R: ReadPrimitives<LE>>(
    reader: &mut R,
) -> Result<Option<(u32, String)>, DecodeError> {
    let id = match u32::decode(reader) {
        Ok(v) => v,
        Err(DecodeError::Truncated) | Err(DecodeError::Io(_)) => return Ok(None),
        Err(e) => return Err(e),
    };

    let _flag = match u8::decode(reader) {
        Ok(v) => v,
        Err(DecodeError::Truncated) | Err(DecodeError::Io(_)) => return Ok(None),
        Err(e) => return Err(e),
    };

    let length = match u16::decode(reader) {
        Ok(v) => v as usize,
        Err(DecodeError::Truncated) | Err(DecodeError::Io(_)) => return Ok(None),
        Err(e) => return Err(e),
    };

    let mut buf = vec![0u8; length];
    match reader.read_bytes(&mut buf) {
        Ok(()) => {}
        Err(DecodeError::Truncated) | Err(DecodeError::Io(_)) => return Ok(None),
        Err(e) => return Err(e),
    }

    // Cliloc files are nominally UTF-8, but some entries (especially
    // in older or modded clients) contain raw Windows-1252 or other
    // non-UTF-8 bytes.  Use lossy conversion so a few bad entries
    // don't prevent loading the entire file.
    let text = String::from_utf8_lossy(&buf).into_owned();

    Ok(Some((id, text)))
}

fn parse<R: ReadPrimitives<LE>>(reader: &mut R) -> Result<ClilocTable, DecodeError> {
    // Header: magic1 (u32) + magic2 (u16) = 6 bytes.
    let _magic1 = u32::decode(reader)?;
    let _magic2 = u16::decode(reader)?;

    let mut entries = HashMap::new();

    while let Some((id, text)) = read_entry(reader)? {
        entries.insert(id, text);
    }

    Ok(ClilocTable { entries })
}

// ── BWT decryption ─────────────────────────────────────────────────────────

/// Decrypt a BWT-compressed cliloc file.
///
/// The algorithm is a two-stage inverse:
///
/// 1. **Stage 1 — Move-To-Front decode** on a 256-entry `u16` table.
///    Each input byte indexes into the table, the value at that index
///    becomes the output byte, and the entry is moved to the front.
///
/// 2. **Stage 2 — Inverse Burrows-Wheeler Transform** using a
///    frequency table embedded in the first 1024 bytes of the Stage-1
///    output.  The remaining bytes are the BWT-permuted data which is
///    un-permuted into the final plaintext.
///
/// Input layout: `[encrypted_size: u32 LE] [payload...]`
/// where `encrypted_size XOR 0x8E2C9A3D` = decompressed size.
pub fn bwt_decrypt(input: &[u8]) -> Result<Vec<u8>, String> {
    if input.len() < 4 + BWT_FREQ_TABLE_SIZE {
        return Err("BWT decryption: input too small".into());
    }

    // Read and unmask the decompressed size.
    let raw_size = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
    let decrypted_size = (raw_size ^ BWT_XOR_MASK) as usize;

    let payload = &input[4..];

    if decrypted_size == 0 || decrypted_size > payload.len().saturating_sub(BWT_FREQ_TABLE_SIZE) {
        return Err(format!(
            "BWT decryption: invalid decompressed size {decrypted_size} \
             (payload {} bytes, freq table {BWT_FREQ_TABLE_SIZE} bytes)",
            payload.len(),
        ));
    }

    // ── Stage 1: Move-To-Front decode ──────────────────────────────────
    //
    // Maintain a 256-entry u16 table initialised to [0, 1, 2, …, 255].
    // For each input byte `b`: output = table[b], then move table[b]
    // to the front (shift elements 0..b right by one).

    let mut mtf_table: [u16; 256] = {
        let mut t = [0u16; 256];
        for i in 0..256 {
            t[i] = i as u16;
        }
        t
    };

    let mut work = vec![0u8; payload.len()];

    for (i, &b) in payload.iter().enumerate() {
        let idx = b as usize;
        let value = mtf_table[idx];
        work[i] = value as u8;

        // Move to front: shift elements [0..idx) right by 1, put value at [0].
        if idx > 0 {
            mtf_table.copy_within(0..idx, 1);
            mtf_table[0] = value;
        }
    }

    // ── Stage 2: Inverse BWT ───────────────────────────────────────────
    //
    // The first 1024 bytes of `work` are the frequency table:
    // 256 × i32 (LE) giving the count of each byte value in the output.
    // The remaining bytes are the BWT-permuted data.

    // Parse frequency table.
    let mut freq = [0i32; 256];
    for i in 0..256 {
        let off = i * 4;
        freq[i] = i32::from_le_bytes([
            work[off],
            work[off + 1],
            work[off + 2],
            work[off + 3],
        ]);
    }

    // Validate frequencies: all non-negative, sum == decrypted_size.
    let mut sum: u64 = 0;
    for (i, &f) in freq.iter().enumerate() {
        if f < 0 {
            return Err(format!(
                "BWT decryption: negative frequency at index {i}: {f}"
            ));
        }
        sum += f as u64;
        if sum > decrypted_size as u64 {
            return Err("BWT decryption: frequency sum exceeds decompressed size".into());
        }
    }
    if sum != decrypted_size as u64 {
        return Err(format!(
            "BWT decryption: frequency sum {sum} != decompressed size {decrypted_size}"
        ));
    }

    let bwt_data = &work[BWT_FREQ_TABLE_SIZE..];

    // Sort symbols by descending frequency → arr3.
    let mut arr3 = [0u8; 256];
    {
        let mut tmp_freq = freq;
        for slot in arr3.iter_mut() {
            let mut best_val = 0i32;
            let mut best_sym = 0u8;
            for (j, &f) in tmp_freq.iter().enumerate() {
                if f > best_val {
                    best_sym = j as u8;
                    best_val = f;
                }
            }
            if best_val == 0 {
                break;
            }
            *slot = best_sym;
            tmp_freq[best_sym as usize] = 0;
        }
    }

    let mut non_zero_count: usize = freq.iter().filter(|&&f| f != 0).count();

    // Build range tables: arr6[sym] = start+1, arr7[sym] = end (exclusive).
    let mut arr6 = [0i32; 256]; // per-symbol read cursor (1-based into bwt_data)
    let mut arr7 = [0i32; 256]; // per-symbol range end

    // symbol_table: running "context" stack for the inverse BWT.
    let mut symbol_table = [0u8; 256];

    // Initialise symbol_table[0] from the first byte of bwt_data.
    if decrypted_size > 0 {
        // Safety: decrypted_size > 0 means bwt_data is non-empty.
        symbol_table[bwt_data[0] as usize] = arr3[0]; // will be overwritten below
    }

    // Build ranges.
    let mut count: usize = 0;
    for i in 0..non_zero_count {
        let sym = arr3[i];

        if count >= decrypted_size {
            return Err("BWT decryption: range overflow".into());
        }

        symbol_table[bwt_data[count] as usize] = sym;

        arr6[sym as usize] = count as i32 + 1;

        let f = freq[sym as usize] as usize;
        if f == 0 || count + f > decrypted_size {
            return Err("BWT decryption: bad frequency range".into());
        }

        count += f;
        arr7[sym as usize] = count as i32;
    }

    // Reconstruct output.
    let mut output = vec![0u8; decrypted_size];
    let mut cur_sym = symbol_table[0];

    for out_byte in output.iter_mut() {
        let first_val = arr6[cur_sym as usize];
        *out_byte = cur_sym;

        if first_val >= arr7[cur_sym as usize] {
            // This symbol is exhausted — remove it from the context stack.
            non_zero_count -= 1;
            if non_zero_count > 0 {
                // Shift symbol_table left by 1 (remove position 0).
                symbol_table.copy_within(1..non_zero_count + 1, 0);
                cur_sym = symbol_table[0];
            }
        } else {
            // Advance read cursor for this symbol.
            if first_val < 0 || (first_val as usize) >= decrypted_size {
                return Err("BWT decryption: index out of range".into());
            }

            let idx = bwt_data[first_val as usize];
            arr6[cur_sym as usize] = first_val + 1;

            if idx > 0 {
                let idx = idx as usize;
                // Move elements [1..idx+1) → [0..idx), put cur_sym at [idx].
                symbol_table.copy_within(1..idx + 1, 0);
                // (this is shift-left of first idx+1 elements, then cur_sym
                //  goes where element [idx] was; but we shifted [1..idx+1)→[0..idx),
                //  which means [0] is lost — that was the old front. So now
                //  [idx] is free for cur_sym.)
                //
                // Actually: shift [0..idx) right by... no, let's follow Delphi:
                //   Move(symbolTable[1], symbolTable[0], idx);
                //   symbolTable[idx] := curSym;
                // Move(src, dst, count) copies count bytes from src to dst.
                // So it copies bytes [1..1+idx) to [0..idx) — shift left by 1.
                // Then symbolTable[idx] = curSym.
                symbol_table[idx] = cur_sym;
                cur_sym = symbol_table[0];
            }
            // If idx == 0, cur_sym stays the same (it's already at front).
        }
    }

    Ok(output)
}
