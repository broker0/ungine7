//! Centralised serial allocator for UO entity serials.
//!
//! Replaces the scattered `AtomicU32` counters with a single allocator
//! that knows about occupied serials, reuses holes in the sequence, and
//! can hand out contiguous ranges in one call.
//!
//! ## UO serial convention
//!
//! | Range | Kind |
//! |---|---|
//! | `0x00000001..=0x3FFFFFFF` | Mobiles |
//! | `0x40000001..=0x7FFFFFFF` | Items |
//!
//! ## Thread safety
//!
//! All public methods take `&self` and lock an internal [`Mutex`].
//! Allocation is not a hot path, so lock contention is negligible.
//!
//! ## Snapshot / cleanup
//!
//! [`SerialAllocator::snapshot`] records the current allocation cursor.
//! [`SerialAllocator::free_since`] releases everything allocated after
//! the snapshot — used by the Lua script manager to clean up entities
//! on script stop/reload.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::Mutex;

// ── Constants ─────────────────────────────────────────────────────────────

/// Start of the mobile serial range (inclusive).
const MOBILE_MIN: u32 = 0x0000_0001;
/// End of the mobile serial range (exclusive).
const MOBILE_MAX: u32 = 0x4000_0000;

/// Start of the item serial range (inclusive).
const ITEM_MIN: u32 = 0x4000_0001;
/// End of the item serial range (exclusive).
const ITEM_MAX: u32 = 0x8000_0000;

/// Boundary between mobile and item ranges.
const ITEM_BOUNDARY: u32 = 0x4000_0000;

// ── RangeAllocator ────────────────────────────────────────────────────────

/// Manages a set of free serial ranges within `[range_start..range_end)`.
///
/// Internally stores free ranges as a `BTreeMap<start, end>` where each
/// entry represents a contiguous free interval `[start..end)`.  Adjacent
/// ranges are merged on `free()`.
#[derive(Debug)]
struct RangeAllocator {
    /// Free ranges: key = range start, value = range end (exclusive).
    free: BTreeMap<u32, u32>,
    /// Total number of free serials (sum of all range lengths).
    free_count: u64,
    /// Lower bound of the managed range (inclusive).
    range_start: u32,
    /// Upper bound of the managed range (exclusive).
    range_end: u32,
}

impl RangeAllocator {
    /// Create a new allocator owning `[start..end)` with everything free.
    fn new(start: u32, end: u32) -> Self {
        let mut free = BTreeMap::new();
        let count = (end as u64).saturating_sub(start as u64);
        if count > 0 {
            free.insert(start, end);
        }
        Self {
            free,
            free_count: count,
            range_start: start,
            range_end: end,
        }
    }

    /// Mark a single serial as occupied.  Splits the containing free range
    /// if necessary.  No-op if the serial is already occupied or out of range.
    fn mark_occupied(&mut self, serial: u32) {
        if serial < self.range_start || serial >= self.range_end {
            return;
        }

        // Find the free range that contains `serial`.
        // The range whose start is <= serial.
        let entry = self.free.range(..=serial).next_back().map(|(&s, &e)| (s, e));
        if let Some((start, end)) = entry {
            if serial >= start && serial < end {
                // Remove the containing range entirely from free_count.
                let old_len = (end - start) as u64;
                self.free.remove(&start);
                self.free_count -= old_len;

                // Re-insert left portion [start..serial) if non-empty.
                if serial > start {
                    let left_len = (serial - start) as u64;
                    self.free.insert(start, serial);
                    self.free_count += left_len;
                }
                // Re-insert right portion [serial+1..end) if non-empty.
                if serial + 1 < end {
                    let right_len = (end - serial - 1) as u64;
                    self.free.insert(serial + 1, end);
                    self.free_count += right_len;
                }
            }
        }
    }

    /// Allocate a single serial.  Returns the lowest available serial,
    /// or `None` if exhausted.
    fn alloc_one(&mut self) -> Option<u32> {
        // Take the first free range (lowest serial).
        let (&start, &end) = self.free.iter().next()?;

        let serial = start;
        // Shrink or remove the range.
        self.free.remove(&start);
        if start + 1 < end {
            self.free.insert(start + 1, end);
        }
        self.free_count -= 1;
        Some(serial)
    }

    /// Allocate a contiguous range of `count` serials.
    ///
    /// Searches for the first free range large enough to satisfy the
    /// request.  Returns `Some(start..start+count)` on success, `None`
    /// if no single contiguous block is large enough.
    fn alloc_contiguous(&mut self, count: u32) -> Option<Range<u32>> {
        if count == 0 {
            return Some(0..0);
        }

        // Find first range that fits.
        let (&start, &end) = self.free.iter()
            .find(|&(&s, &e)| (e - s) >= count)?;

        let alloc_end = start + count;
        self.free.remove(&start);
        if alloc_end < end {
            self.free.insert(alloc_end, end);
        }
        self.free_count -= count as u64;
        Some(start..alloc_end)
    }

    /// Allocate `count` serials, not necessarily contiguous.
    ///
    /// Tries to get a contiguous block first.  Falls back to allocating
    /// one-by-one from the smallest free ranges.
    fn alloc_many(&mut self, count: u32) -> Vec<u32> {
        if count == 0 {
            return Vec::new();
        }

        // Try contiguous first.
        if let Some(range) = self.alloc_contiguous(count) {
            return range.collect();
        }

        // Fall back: allocate one-by-one.
        let mut result = Vec::with_capacity(count as usize);
        for _ in 0..count {
            match self.alloc_one() {
                Some(s) => result.push(s),
                None => break,
            }
        }
        result
    }

    /// Return a serial to the free pool.  Merges with adjacent free
    /// ranges if they exist.
    fn free_one(&mut self, serial: u32) {
        if serial < self.range_start || serial >= self.range_end {
            return;
        }

        let mut new_start = serial;
        let mut new_end = serial + 1;

        // Check if the range immediately after [serial+1..?) exists.
        if let Some(&after_end) = self.free.get(&(serial + 1)) {
            new_end = after_end;
            self.free.remove(&(serial + 1));
            self.free_count -= (after_end - serial - 1) as u64;
        }

        // Check if a range ending at `serial` exists (i.e., some range
        // [prev_start..serial) is free).
        let before = self.free.range(..=serial).next_back().map(|(&s, &e)| (s, e));
        if let Some((prev_start, prev_end)) = before {
            if prev_end == serial {
                // Merge with the range before.
                new_start = prev_start;
                self.free.remove(&prev_start);
                self.free_count -= (prev_end - prev_start) as u64;
            }
        }

        self.free_count += (new_end - new_start) as u64;
        self.free.insert(new_start, new_end);
    }
}

// ── AllocSnapshot ─────────────────────────────────────────────────────────

/// Opaque snapshot of the allocator state at a point in time.
///
/// Used with [`SerialAllocator::free_since`] to release all serials
/// allocated after this snapshot was taken.
#[derive(Debug, Clone)]
pub struct AllocSnapshot {
    log_pos: usize,
}

// ── SerialAllocator ───────────────────────────────────────────────────────

struct Inner {
    mobiles: RangeAllocator,
    items: RangeAllocator,
    /// Log of all allocated serials (in allocation order).
    /// Used by snapshot/free_since for bulk cleanup.
    alloc_log: Vec<u32>,
    /// Serials marked as persistent — they survive `free_since()` cleanup.
    ///
    /// Entities spawned by init/utility scripts should be marked
    /// persistent so that reloading/stopping managed scripts does not
    /// remove them from the world.
    persistent: HashSet<u32>,
}

/// Thread-safe centralised serial allocator for UO entity serials.
///
/// Divides the 32-bit serial space into mobile (`0x00000001..0x3FFFFFFF`)
/// and item (`0x40000001..0x7FFFFFFF`) ranges.  Tracks occupied serials,
/// reuses holes, and can hand out contiguous ranges.
pub struct SerialAllocator {
    inner: Mutex<Inner>,
}

impl SerialAllocator {
    /// Create a new allocator with the full mobile and item ranges free.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                mobiles: RangeAllocator::new(MOBILE_MIN, MOBILE_MAX),
                items: RangeAllocator::new(ITEM_MIN, ITEM_MAX),
                alloc_log: Vec::new(),
                persistent: HashSet::new(),
            }),
        }
    }

    /// Reset the allocator to its initial state.
    ///
    /// All serials become free, the allocation log is cleared, and
    /// persistent markers are removed.  Use this after a full world
    /// restore (`.load`) where all entity serials will be re-registered
    /// via [`mark_occupied`](Self::mark_occupied).
    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.mobiles = RangeAllocator::new(MOBILE_MIN, MOBILE_MAX);
        inner.items = RangeAllocator::new(ITEM_MIN, ITEM_MAX);
        inner.alloc_log.clear();
        inner.persistent.clear();
    }

    /// Mark a serial as occupied (e.g. loaded from `.uolog`).
    ///
    /// Automatically routes to the mobile or item range based on the
    /// serial value.  Does **not** record in the alloc log (these are
    /// pre-existing entities, not allocator-managed).
    pub fn mark_occupied(&self, serial: u32) {
        let mut inner = self.inner.lock().unwrap();
        if serial < ITEM_BOUNDARY {
            inner.mobiles.mark_occupied(serial);
        } else {
            inner.items.mark_occupied(serial);
        }
    }

    /// Bulk-mark multiple serials as occupied.
    pub fn mark_occupied_many(&self, serials: impl IntoIterator<Item = u32>) {
        let mut inner = self.inner.lock().unwrap();
        for serial in serials {
            if serial < ITEM_BOUNDARY {
                inner.mobiles.mark_occupied(serial);
            } else {
                inner.items.mark_occupied(serial);
            }
        }
    }

    // ── Single allocation ────────────────────────────────────────────

    /// Allocate a single mobile serial.
    pub fn alloc_mobile(&self) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap();
        let serial = inner.mobiles.alloc_one()?;
        inner.alloc_log.push(serial);
        Some(serial)
    }

    /// Allocate a single item serial.
    pub fn alloc_item(&self) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap();
        let serial = inner.items.alloc_one()?;
        inner.alloc_log.push(serial);
        Some(serial)
    }

    // ── Batch allocation ─────────────────────────────────────────────

    /// Allocate `count` mobile serials (not necessarily contiguous).
    pub fn alloc_mobiles(&self, count: u32) -> Vec<u32> {
        let mut inner = self.inner.lock().unwrap();
        let serials = inner.mobiles.alloc_many(count);
        inner.alloc_log.extend_from_slice(&serials);
        serials
    }

    /// Allocate `count` item serials (not necessarily contiguous).
    pub fn alloc_items(&self, count: u32) -> Vec<u32> {
        let mut inner = self.inner.lock().unwrap();
        let serials = inner.items.alloc_many(count);
        inner.alloc_log.extend_from_slice(&serials);
        serials
    }

    // ── Contiguous range allocation ──────────────────────────────────

    /// Allocate a contiguous range of `count` mobile serials.
    ///
    /// Returns `None` if no single contiguous block is large enough.
    pub fn alloc_mobile_range(&self, count: u32) -> Option<Range<u32>> {
        let mut inner = self.inner.lock().unwrap();
        let range = inner.mobiles.alloc_contiguous(count)?;
        for s in range.clone() {
            inner.alloc_log.push(s);
        }
        Some(range)
    }

    /// Allocate a contiguous range of `count` item serials.
    ///
    /// Returns `None` if no single contiguous block is large enough.
    pub fn alloc_item_range(&self, count: u32) -> Option<Range<u32>> {
        let mut inner = self.inner.lock().unwrap();
        let range = inner.items.alloc_contiguous(count)?;
        for s in range.clone() {
            inner.alloc_log.push(s);
        }
        Some(range)
    }

    // ── Free ─────────────────────────────────────────────────────────

    /// Return a serial to the free pool.
    ///
    /// Automatically routes to mobile or item range.  The serial is
    /// **not** removed from the alloc log (the log is append-only;
    /// cleanup uses `free_since`).
    pub fn free(&self, serial: u32) {
        let mut inner = self.inner.lock().unwrap();
        if serial < ITEM_BOUNDARY {
            inner.mobiles.free_one(serial);
        } else {
            inner.items.free_one(serial);
        }
    }

    /// Bulk-free multiple serials.
    pub fn free_many(&self, serials: impl IntoIterator<Item = u32>) {
        let mut inner = self.inner.lock().unwrap();
        for serial in serials {
            if serial < ITEM_BOUNDARY {
                inner.mobiles.free_one(serial);
            } else {
                inner.items.free_one(serial);
            }
        }
    }

    // ── Persistence ────────────────────────────────────────────────

    /// Mark a serial as persistent.
    ///
    /// Persistent serials are **skipped** by [`free_since`](Self::free_since)
    /// — they remain allocated and their entities survive script
    /// reload/stop cleanup.
    pub fn mark_persistent(&self, serial: u32) {
        self.inner.lock().unwrap().persistent.insert(serial);
    }

    /// Check whether a serial is marked persistent.
    pub fn is_persistent(&self, serial: u32) -> bool {
        self.inner.lock().unwrap().persistent.contains(&serial)
    }

    // ── Snapshot / cleanup ───────────────────────────────────────────

    /// Take a snapshot of the current allocation state.
    ///
    /// All serials allocated after this call can be released via
    /// [`free_since`](Self::free_since).
    pub fn snapshot(&self) -> AllocSnapshot {
        let inner = self.inner.lock().unwrap();
        AllocSnapshot {
            log_pos: inner.alloc_log.len(),
        }
    }

    /// Release all serials allocated since `snapshot` was taken.
    ///
    /// Persistent serials (marked via [`mark_persistent`](Self::mark_persistent))
    /// are **skipped** — they remain allocated and are not included in
    /// the returned list.
    ///
    /// Returns the list of freed serials (for entity removal).
    pub fn free_since(&self, snapshot: &AllocSnapshot) -> Vec<u32> {
        let mut inner = self.inner.lock().unwrap();
        let candidates: Vec<u32> = inner.alloc_log[snapshot.log_pos..].to_vec();
        // Truncate the log back to the snapshot point …
        inner.alloc_log.truncate(snapshot.log_pos);
        // … but re-append persistent serials so they remain tracked.
        let mut freed = Vec::with_capacity(candidates.len());
        for serial in candidates {
            if inner.persistent.contains(&serial) {
                // Keep it allocated — push back into the log.
                inner.alloc_log.push(serial);
            } else {
                // Free it.
                if serial < ITEM_BOUNDARY {
                    inner.mobiles.free_one(serial);
                } else {
                    inner.items.free_one(serial);
                }
                freed.push(serial);
            }
        }
        freed
    }

    // ── Stats ────────────────────────────────────────────────────────

    /// Number of free mobile serials.
    pub fn free_mobiles(&self) -> u64 {
        self.inner.lock().unwrap().mobiles.free_count
    }

    /// Number of free item serials.
    pub fn free_items(&self) -> u64 {
        self.inner.lock().unwrap().items.free_count
    }

    /// Total allocations recorded in the log.
    pub fn alloc_log_len(&self) -> usize {
        self.inner.lock().unwrap().alloc_log.len()
    }
}

impl Default for SerialAllocator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- RangeAllocator unit tests ------------------------------------------

    #[test]
    fn range_alloc_basic() {
        let mut ra = RangeAllocator::new(1, 11); // [1..11) = 10 serials
        assert_eq!(ra.free_count, 10);

        assert_eq!(ra.alloc_one(), Some(1));
        assert_eq!(ra.free_count, 9);
        assert_eq!(ra.alloc_one(), Some(2));
        assert_eq!(ra.free_count, 8);
    }

    #[test]
    fn range_alloc_contiguous() {
        let mut ra = RangeAllocator::new(100, 200); // 100 serials
        let range = ra.alloc_contiguous(10).unwrap();
        assert_eq!(range, 100..110);
        assert_eq!(ra.free_count, 90);

        // Next alloc should start at 110.
        assert_eq!(ra.alloc_one(), Some(110));
    }

    #[test]
    fn range_alloc_contiguous_too_large() {
        let mut ra = RangeAllocator::new(100, 110); // 10 serials
        assert!(ra.alloc_contiguous(11).is_none());
        // Exact fit should work.
        let range = ra.alloc_contiguous(10).unwrap();
        assert_eq!(range, 100..110);
        assert_eq!(ra.free_count, 0);
    }

    #[test]
    fn range_free_and_merge() {
        let mut ra = RangeAllocator::new(1, 6); // [1..6) = 5 serials

        // Allocate all.
        for _ in 0..5 {
            ra.alloc_one().unwrap();
        }
        assert_eq!(ra.free_count, 0);
        assert!(ra.alloc_one().is_none());

        // Free serials 2, 3, 4 — should merge into one range [2..5).
        ra.free_one(3);
        assert_eq!(ra.free_count, 1);
        ra.free_one(2);
        assert_eq!(ra.free_count, 2);
        ra.free_one(4);
        assert_eq!(ra.free_count, 3);

        // Should now have merged: [2..5).
        assert_eq!(ra.free.len(), 1);
        assert_eq!(ra.free.get(&2), Some(&5));
    }

    #[test]
    fn range_free_merge_with_both_neighbors() {
        let mut ra = RangeAllocator::new(1, 6);
        // Allocate all 5.
        for _ in 0..5 { ra.alloc_one().unwrap(); }

        // Free 1, 3, 5 (non-adjacent).
        ra.free_one(1);
        ra.free_one(3);
        ra.free_one(5);
        assert_eq!(ra.free_count, 3);
        assert_eq!(ra.free.len(), 3); // three separate ranges

        // Free 2 — should merge [1..2) + [2] + [3..4) = [1..4)
        ra.free_one(2);
        assert_eq!(ra.free_count, 4);
        // Now we have [1..4) and [5..6) = 2 ranges.
        assert_eq!(ra.free.len(), 2);
        assert_eq!(ra.free.get(&1), Some(&4));
        assert_eq!(ra.free.get(&5), Some(&6));

        // Free 4 — should merge [1..4) + [4] + [5..6) = [1..6)
        ra.free_one(4);
        assert_eq!(ra.free_count, 5);
        assert_eq!(ra.free.len(), 1);
        assert_eq!(ra.free.get(&1), Some(&6));
    }

    #[test]
    fn mark_occupied_splits_range() {
        let mut ra = RangeAllocator::new(1, 11); // [1..11)
        ra.mark_occupied(5);
        assert_eq!(ra.free_count, 9);
        // Should have [1..5) and [6..11).
        assert_eq!(ra.free.len(), 2);
        assert_eq!(ra.free.get(&1), Some(&5));
        assert_eq!(ra.free.get(&6), Some(&11));
    }

    #[test]
    fn mark_occupied_at_range_start() {
        let mut ra = RangeAllocator::new(1, 11);
        ra.mark_occupied(1);
        assert_eq!(ra.free_count, 9);
        assert_eq!(ra.free.len(), 1);
        assert_eq!(ra.free.get(&2), Some(&11));
    }

    #[test]
    fn mark_occupied_at_range_end() {
        let mut ra = RangeAllocator::new(1, 11);
        ra.mark_occupied(10);
        assert_eq!(ra.free_count, 9);
        assert_eq!(ra.free.len(), 1);
        assert_eq!(ra.free.get(&1), Some(&10));
    }

    #[test]
    fn mark_occupied_already_occupied_is_noop() {
        let mut ra = RangeAllocator::new(1, 11);
        ra.mark_occupied(5);
        assert_eq!(ra.free_count, 9);
        ra.mark_occupied(5); // again
        assert_eq!(ra.free_count, 9); // unchanged
    }

    #[test]
    fn alloc_many_fallback() {
        let mut ra = RangeAllocator::new(1, 6); // 5 serials
        // Mark 3 as occupied → [1..3) and [4..6) — two ranges of 2 each.
        ra.mark_occupied(3);
        assert_eq!(ra.free_count, 4);

        // Request 3 — no contiguous block of 3 exists, falls back to one-by-one.
        let serials = ra.alloc_many(3);
        assert_eq!(serials.len(), 3);
        assert_eq!(serials, vec![1, 2, 4]);
    }

    #[test]
    fn exhaustion() {
        let mut ra = RangeAllocator::new(1, 4); // 3 serials
        assert_eq!(ra.alloc_one(), Some(1));
        assert_eq!(ra.alloc_one(), Some(2));
        assert_eq!(ra.alloc_one(), Some(3));
        assert_eq!(ra.alloc_one(), None);

        // Free one and reallocate.
        ra.free_one(2);
        assert_eq!(ra.alloc_one(), Some(2));
        assert_eq!(ra.alloc_one(), None);
    }

    // -- SerialAllocator integration tests ----------------------------------

    #[test]
    fn serial_alloc_mobile_vs_item() {
        let alloc = SerialAllocator::new();

        let m1 = alloc.alloc_mobile().unwrap();
        assert!(m1 >= MOBILE_MIN && m1 < MOBILE_MAX, "mobile serial {:#010X} out of range", m1);

        let i1 = alloc.alloc_item().unwrap();
        assert!(i1 >= ITEM_MIN && i1 < ITEM_MAX, "item serial {:#010X} out of range", i1);
    }

    #[test]
    fn serial_alloc_mark_occupied() {
        let alloc = SerialAllocator::new();

        // Mark serial 1 as occupied.
        alloc.mark_occupied(1);

        // Next mobile alloc should skip 1.
        let m = alloc.alloc_mobile().unwrap();
        assert_ne!(m, 1);
        assert_eq!(m, 2);
    }

    #[test]
    fn serial_alloc_snapshot_and_free_since() {
        let alloc = SerialAllocator::new();

        // Allocate some serials before the snapshot.
        let _pre = alloc.alloc_mobile().unwrap();

        let snap = alloc.snapshot();

        // Allocate after snapshot.
        let m1 = alloc.alloc_mobile().unwrap();
        let m2 = alloc.alloc_mobile().unwrap();
        let i1 = alloc.alloc_item().unwrap();

        // Free since snapshot.
        let freed = alloc.free_since(&snap);
        assert_eq!(freed.len(), 3);
        assert!(freed.contains(&m1));
        assert!(freed.contains(&m2));
        assert!(freed.contains(&i1));

        // Freed serials should be re-allocatable.
        let m_new = alloc.alloc_mobile().unwrap();
        assert_eq!(m_new, m1); // re-allocated lowest free = m1
    }

    #[test]
    fn serial_alloc_contiguous_range() {
        let alloc = SerialAllocator::new();

        let range = alloc.alloc_mobile_range(10).unwrap();
        assert_eq!(range.len(), 10);
        assert_eq!(range.start, MOBILE_MIN);
        assert_eq!(range.end, MOBILE_MIN + 10);

        let range2 = alloc.alloc_item_range(5).unwrap();
        assert_eq!(range2.len(), 5);
        assert_eq!(range2.start, ITEM_MIN);
    }

    #[test]
    fn serial_alloc_free_enables_reuse() {
        let alloc = SerialAllocator::new();

        let m1 = alloc.alloc_mobile().unwrap();
        let m2 = alloc.alloc_mobile().unwrap();

        // Free m1.
        alloc.free(m1);

        // Next alloc should return m1 (lowest free).
        let m3 = alloc.alloc_mobile().unwrap();
        assert_eq!(m3, m1);

        // And then the next should be after m2.
        let m4 = alloc.alloc_mobile().unwrap();
        assert_eq!(m4, m2 + 1);
    }

    #[test]
    fn serial_alloc_bulk_mark_occupied() {
        let alloc = SerialAllocator::new();
        alloc.mark_occupied_many(vec![1, 2, 3, 0x4000_0001, 0x4000_0002]);

        let m = alloc.alloc_mobile().unwrap();
        assert_eq!(m, 4); // 1,2,3 occupied

        let i = alloc.alloc_item().unwrap();
        assert_eq!(i, 0x4000_0003); // 0x4000_0001, 0x4000_0002 occupied
    }

    #[test]
    fn range_alloc_zero_count() {
        let mut ra = RangeAllocator::new(1, 11);
        assert_eq!(ra.alloc_contiguous(0), Some(0..0));
        assert_eq!(ra.alloc_many(0), Vec::<u32>::new());
        assert_eq!(ra.free_count, 10); // unchanged
    }

    #[test]
    fn serial_alloc_double_free_does_not_corrupt() {
        let alloc = SerialAllocator::new();
        let m1 = alloc.alloc_mobile().unwrap();
        alloc.free(m1);
        // Double-free: serial is already free, free_one should be safe.
        // The serial is already in the free set, so this is a no-op
        // (or idempotent merge). We just verify no panic/corruption.
        alloc.free(m1);

        let m2 = alloc.alloc_mobile().unwrap();
        assert_eq!(m2, m1);
    }
}
