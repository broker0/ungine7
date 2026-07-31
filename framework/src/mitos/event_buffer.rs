//! Buffered broadcast event receiver for Lua scripts.
//!
//! [`BroadcastEventBuffer`] wraps a [`broadcast::Receiver`] and buffers events
//! into a [`VecDeque`] so that `poll_event()` never misses events
//! between calls.  An optional spatial filter restricts which events
//! are delivered.
//!
//! Used by both the async runtime (`runtime.rs`) and the controller
//! mode (`LuaController`), with different event types.

use std::collections::VecDeque;

use tokio::sync::broadcast;

// ── Spatial filter ──────────────────────────────────────────────────────

/// Optional tile-aligned bounding box for spatial event filtering.
#[derive(Clone, Copy, Debug)]
pub struct SpatialFilter {
    pub x_min: u16,
    pub y_min: u16,
    pub x_max: u16,
    pub y_max: u16,
}

impl SpatialFilter {
    /// Returns `true` if the point `(x, y)` falls within this rectangle.
    #[inline]
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x_min && x <= self.x_max && y >= self.y_min && y <= self.y_max
    }
}

// ── EventBuffer (broadcast-backed) ──────────────────────────────────────

/// Buffered broadcast receiver with optional spatial filtering.
///
/// Generic over the event type `E`.  The filter predicate is provided
/// via a closure so that each backend can define its own spatial logic.
pub struct BroadcastEventBuffer<E: Clone> {
    rx: broadcast::Receiver<E>,
    buffer: VecDeque<E>,
    filter: Option<SpatialFilter>,
    /// User-provided function that extracts (x, y) from an event,
    /// or returns `None` for events that always pass through (e.g.
    /// map-wide ambient events like light/weather).
    position_fn: Option<Box<dyn Fn(&E) -> EventPosition + Send>>,
}

/// Position information extracted from an event for spatial filtering.
pub enum EventPosition {
    /// Event has a single position.
    Single(u16, u16),
    /// Event has two positions (e.g. movement with old + new pos).
    /// Passes filter if *either* position is inside the rect.
    Dual(u16, u16, u16, u16),
    /// Event is map-wide or has no position — always passes.
    Always,
    /// Event should never be delivered to standalone scripts.
    Never,
}

impl<E: Clone> BroadcastEventBuffer<E> {
    /// Create a new buffer wrapping the given broadcast receiver.
    pub fn new(rx: broadcast::Receiver<E>) -> Self {
        Self {
            rx,
            buffer: VecDeque::new(),
            filter: None,
            position_fn: None,
        }
    }

    /// Create a new buffer with a spatial filter position extractor.
    pub fn with_position_fn(
        rx: broadcast::Receiver<E>,
        position_fn: impl Fn(&E) -> EventPosition + Send + 'static,
    ) -> Self {
        Self {
            rx,
            buffer: VecDeque::new(),
            filter: None,
            position_fn: Some(Box::new(position_fn)),
        }
    }

    /// Set a spatial filter.  Only events whose position overlaps
    /// the rectangle will be buffered from this point on.
    /// Existing buffered events outside the new rect are discarded.
    pub fn set_filter(&mut self, filter: SpatialFilter) {
        if let Some(ref pos_fn) = self.position_fn {
            self.buffer.retain(|e| event_passes_filter(pos_fn(e), &filter));
        }
        self.filter = Some(filter);
    }

    /// Remove the spatial filter — all events are buffered again.
    pub fn clear_filter(&mut self) {
        self.filter = None;
    }

    /// Drain all available events from broadcast into the internal buffer,
    /// applying the spatial filter when set.
    pub fn drain_broadcast(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    if self.should_accept(&event) {
                        self.buffer.push_back(event);
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    log::warn!("[event_buffer] event broadcast lagged, lost {} events", n);
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => break,
            }
        }
    }

    /// Pop one event from the internal buffer.
    pub fn pop(&mut self) -> Option<E> {
        self.buffer.pop_front()
    }

    /// Non-blocking: drain broadcast then pop one buffered event.
    pub fn try_recv(&mut self) -> Option<E> {
        self.drain_broadcast();
        self.buffer.pop_front()
    }

    /// Returns `true` if the buffer is empty (does NOT drain broadcast).
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn should_accept(&self, event: &E) -> bool {
        match (&self.filter, &self.position_fn) {
            (Some(filter), Some(pos_fn)) => event_passes_filter(pos_fn(event), filter),
            _ => true, // No filter or no position extractor — accept all.
        }
    }
}

fn event_passes_filter(pos: EventPosition, filter: &SpatialFilter) -> bool {
    match pos {
        EventPosition::Single(x, y) => filter.contains(x, y),
        EventPosition::Dual(x1, y1, x2, y2) => {
            filter.contains(x1, y1) || filter.contains(x2, y2)
        }
        EventPosition::Always => true,
        EventPosition::Never => false,
    }
}

// ── SimpleEventBuffer (push-based, no broadcast) ────────────────────────

/// Simple capped FIFO buffer for events delivered via push (not broadcast).
///
/// Used by controller mode where events arrive via `on_event()` callback
/// rather than a broadcast channel.
pub struct SimpleEventBuffer<E> {
    events: VecDeque<E>,
    capacity: usize,
}

impl<E> SimpleEventBuffer<E> {
    /// Create a new buffer with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            capacity,
        }
    }

    /// Push an event, dropping the oldest if at capacity.
    pub fn push(&mut self, event: E) {
        if self.events.len() >= self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Pop one event from the front.
    pub fn pop(&mut self) -> Option<E> {
        self.events.pop_front()
    }

    /// Returns `true` if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Number of buffered events.
    pub fn len(&self) -> usize {
        self.events.len()
    }
}
