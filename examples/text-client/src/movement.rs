//! Movement logic — turn-then-step, passability checks, diagonal fallback.
//!
//! Mirrors the approach from the doryen-rs viewer: time-gated steps with
//! a 50 ms turn delay and a configurable step delay (walk vs. run).

use std::sync::Arc;
use std::time::{Duration, Instant};

use packets::movement::MoveRequest;
use packets::traits::BasicPacket;
use protocol::RawPacket;
use u_core::position::Heading;

use framework::diorama::ObserverPipeline;
use framework::diorama::CompositeTileProvider;
use framework::ecumene::{MovementValidator, StaticDataProvider};

// ── Timing constants ──────────────────────────────────────────────────────

const TURN_DELAY: Duration = Duration::from_millis(50);
const WALK_DELAY: Duration = Duration::from_millis(200);
const RUN_DELAY: Duration = Duration::from_millis(100);

// ── MovementState ─────────────────────────────────────────────────────────

/// Tracks move-request sequence numbers and timing for one character.
pub struct MovementState {
    /// MoveRequest sequence counter (1–255, wraps).
    sequence: u8,
    /// Next allowed step instant.
    next_step: Instant,
}

impl MovementState {
    pub fn new() -> Self {
        Self {
            sequence: 0,
            next_step: Instant::now(),
        }
    }

    /// Attempt a step.  Returns a `RawPacket` to send to the server,
    /// or `None` if the step is blocked or rate-limited.
    ///
    /// This performs:
    /// 1. Rate-limit check (turn / step timing)
    /// 2. Passability check with diagonal fallback
    /// 3. MoveRequest encoding
    pub fn try_step(
        &mut self,
        heading: Heading,
        running: bool,
        observer: &ObserverPipeline,
        static_data: Option<&Arc<dyn StaticDataProvider>>,
    ) -> Option<RawPacket> {
        let now = Instant::now();
        if now < self.next_step {
            return None;
        }

        let current_heading = observer.pos.facing.heading();
        let (x, y, z) = (observer.pos.x, observer.pos.y, observer.pos.z);
        let world = observer.session.current_world;

        // Determine the effective heading (with passability + diagonal fallback).
        let effective = if let Some(sd) = static_data {
            self.resolve_heading(heading, x, y, z, world, observer, sd.as_ref())
        } else {
            // No static data — skip passability, just send raw heading.
            Some(heading)
        };

        let effective = effective?;

        // Turn-then-step: if changing direction, apply turn delay.
        let is_turn = effective != current_heading;
        if is_turn {
            self.next_step = now + TURN_DELAY;
        } else {
            self.next_step = now + if running { RUN_DELAY } else { WALK_DELAY };
        }

        // Increment sequence (1–255, skip 0).
        self.sequence = self.sequence.wrapping_add(1);
        if self.sequence == 0 {
            self.sequence = 1;
        }

        let direction = effective as u8 | if running { 0x80 } else { 0x00 };

        let req = MoveRequest {
            id: MoveRequest::ID,
            direction,
            sequence: self.sequence,
            fastwalk_key: 0,
        };

        Some(RawPacket::c2s(req.to_bytes()))
    }

    /// Resolve heading with passability check and diagonal fallback.
    ///
    /// If the desired heading is blocked:
    /// - For diagonal headings, try both cardinal decompositions.
    /// - For cardinal headings, return None (blocked).
    fn resolve_heading(
        &self,
        heading: Heading,
        x: u16,
        y: u16,
        z: i8,
        world: u8,
        observer: &ObserverPipeline,
        sd: &dyn StaticDataProvider,
    ) -> Option<Heading> {
        let provider = CompositeTileProvider::new(
            sd,
            world,
            &observer.session.visible,
            &observer.session.registry,
        );
        let validator = MovementValidator::new(&provider);

        // Try direct heading.
        if validator.test_step(x, y, z, heading).is_some() {
            return Some(heading);
        }

        // Diagonal fallback: try the two adjacent cardinal directions.
        if let Some((card_a, card_b)) = heading.adjacent_straight() {
            if validator.test_step(x, y, z, card_a).is_some() {
                return Some(card_a);
            }
            if validator.test_step(x, y, z, card_b).is_some() {
                return Some(card_b);
            }
        }

        None // Blocked.
    }
}
