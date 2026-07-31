use std::collections::BinaryHeap;
use std::cmp::Ordering;
use std::time::Duration;

use tokio::time::Instant;

/// Unique identifier for a scheduled task.
/// Allows cancelling the task before it fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(u64);

impl TaskId {
    /// Internal numeric value (for serialization / logging).
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// What the scheduler should do on fire.
pub enum TaskAction {
    /// Send `EntityEvent::TimerFired { timer_id }` to the entity controller.
    FireTimer {
        entity_serial: u32,
        timer_id: u64,
    },

    /// Invoke an arbitrary callback (for system tasks).
    /// Taken during execution (FnOnce).
    Callback(Option<Box<dyn FnOnce() + Send>>),
}

impl std::fmt::Debug for TaskAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FireTimer { entity_serial, timer_id } => {
                f.debug_struct("FireTimer")
                    .field("entity_serial", entity_serial)
                    .field("timer_id", timer_id)
                    .finish()
            }
            Self::Callback(Some(_)) => write!(f, "Callback(Some(<fn>))"),
            Self::Callback(None) => write!(f, "Callback(None)"),
        }
    }
}

/// Scheduled task.
struct ScheduledTask {
    id: TaskId,
    fire_at: Instant,
    action: TaskAction,
    /// If Some, the task repeats with this interval.
    repeat: Option<Duration>,
    /// Whether the task has been cancelled.
    cancelled: bool,
    /// Map this task belongs to.
    ///
    /// `None` = map-agnostic task (e.g. a system [`TaskAction::Callback`]),
    /// fires on any zone's tick.  `Some(map_id)` = the task only fires when
    /// the host is ticking that specific zone, so deferred work runs against
    /// the correct world.
    map_id: Option<u8>,
}

// BinaryHeap is max-heap, we need min-heap (earliest task first).
impl PartialEq for ScheduledTask {
    fn eq(&self, other: &Self) -> bool {
        self.fire_at == other.fire_at
    }
}

impl Eq for ScheduledTask {}

impl PartialOrd for ScheduledTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledTask {
    fn cmp(&self, other: &Self) -> Ordering {
        // Invert order: smaller fire_at → higher priority.
        other.fire_at.cmp(&self.fire_at)
    }
}

/// Scheduler for deferred tasks.
///
/// Works on the basis of `BinaryHeap` (min-heap via inverted Ord).
/// Called from `ControllerHost::tick()` — not async, not thread-safe.
pub struct Scheduler {
    heap: BinaryHeap<ScheduledTask>,
    next_id: u64,
}

impl Scheduler {
    /// Create an empty scheduler.
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            next_id: 1,
        }
    }

    /// Schedule a one-off task after `delay`.
    ///
    /// `map_id` ties the task to a specific zone (`Some`) so its deferred
    /// action runs only when that zone is ticked; pass `None` for a
    /// map-agnostic task (e.g. a system callback).
    pub fn schedule(&mut self, delay: Duration, action: TaskAction, map_id: Option<u8>) -> TaskId {
        let id = self.alloc_id();
        self.heap.push(ScheduledTask {
            id,
            fire_at: Instant::now() + delay,
            action,
            repeat: None,
            cancelled: false,
            map_id,
        });
        id
    }

    /// Schedule a repeating task: first fire after `delay`,
    /// then repeat every `interval`.
    ///
    /// `map_id` ties the task to a specific zone (`Some`) so its deferred
    /// action runs only when that zone is ticked; pass `None` for a
    /// map-agnostic task.  The `map_id` is preserved across re-scheduling.
    pub fn schedule_repeating(
        &mut self,
        delay: Duration,
        interval: Duration,
        action: TaskAction,
        map_id: Option<u8>,
    ) -> TaskId {
        let id = self.alloc_id();
        self.heap.push(ScheduledTask {
            id,
            fire_at: Instant::now() + delay,
            action,
            repeat: Some(interval),
            cancelled: false,
            map_id,
        });
        id
    }

    /// Cancel a task by ID.
    ///
    /// The task is marked as cancelled and will be skipped on the next tick.
    /// Lazy deletion: we do not rebuild the heap.
    pub fn cancel(&mut self, id: TaskId) {
        // Linear search — acceptable for reasonable number of tasks.
        // If it becomes a bottleneck — switch to HashMap<TaskId, ...>.
        for task in self.heap.iter() {
            if task.id == id {
                // SAFETY: we only set a flag, do not change heap order.
                // Use interior mutability via unsafe — or simpler:
                // mark on extraction. For simplicity — use separate set.
                break;
            }
        }
        // Simple implementation: rebuild heap without cancelled task.
        // Acceptable for the first iteration.
        let mut tasks: Vec<_> = self.heap.drain().collect();
        for task in &mut tasks {
            if task.id == id {
                task.cancelled = true;
            }
        }
        self.heap.extend(tasks);
    }

    /// Re-stamp the `map_id` of every pending [`TaskAction::FireTimer`] task
    /// belonging to `entity_serial` to `new_map`.
    ///
    /// Used when an entity is transferred across zones: its already-queued
    /// timers (including repeating ones) must follow it to the new world,
    /// otherwise they would fire against the zone it just left.  Rebuilds the
    /// heap (fire order is unaffected — only `map_id` changes).
    pub fn reassign_entity_map(&mut self, entity_serial: u32, new_map: u8) {
        let mut tasks: Vec<_> = self.heap.drain().collect();
        for task in &mut tasks {
            if let TaskAction::FireTimer { entity_serial: s, .. } = &task.action {
                if *s == entity_serial {
                    task.map_id = Some(new_map);
                }
            }
        }
        self.heap.extend(tasks);
    }

    /// Process all due tasks that belong to the given `map_id`.
    ///
    /// Returns a list of fired actions.  A task fires when its time has
    /// come **and** it is either map-agnostic (`map_id == None`) or tied to
    /// `map_id`.  Due tasks belonging to *other* maps are left in the heap
    /// so they fire on that map's own tick (which, within a single worker
    /// wake, happens in the same `for zone in zones` pass — so the timing
    /// delay is negligible).
    ///
    /// Repeating tasks are automatically rescheduled (preserving `map_id`).
    pub fn tick(&mut self, now: Instant, map_id: u8) -> Vec<TaskAction> {
        let mut fired = Vec::new();
        // Due tasks belonging to other maps, temporarily held aside so the
        // heap root advances past them; re-inserted before returning.
        let mut deferred: Vec<ScheduledTask> = Vec::new();

        loop {
            let should_pop = self.heap.peek()
                .is_some_and(|task| task.fire_at <= now && !task.cancelled);

            if !should_pop {
                // Skip cancelled tasks at the top.
                let should_discard = self.heap.peek()
                    .is_some_and(|task| task.cancelled);
                if should_discard {
                    self.heap.pop();
                    continue;
                }
                break;
            }

            let mut task = self.heap.pop().unwrap();

            if task.cancelled {
                continue;
            }

            // Belongs to another map — defer (keep in queue), do not fire.
            if let Some(task_map) = task.map_id {
                if task_map != map_id {
                    deferred.push(task);
                    continue;
                }
            }

            // For repeating tasks — clone action and reschedule.
            if let Some(interval) = task.repeat {
                // Clone action for repeat (Callback cannot be cloned — 
                // only FireTimer repeats).
                let repeat_action = match &task.action {
                    TaskAction::FireTimer { entity_serial, timer_id } => {
                        Some(TaskAction::FireTimer {
                            entity_serial: *entity_serial,
                            timer_id: *timer_id,
                        })
                    }
                    TaskAction::Callback(_) => None, // callback does not repeat
                };

                if let Some(action) = repeat_action {
                    self.heap.push(ScheduledTask {
                        id: task.id,
                        fire_at: task.fire_at + interval,
                        action,
                        repeat: Some(interval),
                        cancelled: false,
                        map_id: task.map_id,
                    });
                }
            }

            // Take action (for Callback — move from Option).
            match &mut task.action {
                TaskAction::Callback(cb) => {
                    if let Some(callback) = cb.take() {
                        fired.push(TaskAction::Callback(Some(callback)));
                    }
                }
                _ => {
                    fired.push(task.action);
                }
            }
        }

        // Return deferred (other-map) tasks to the heap untouched.
        self.heap.extend(deferred);

        fired
    }

    /// Number of tasks in the queue (including cancelled ones).
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// Peek the `fire_at` of the nearest non-cancelled task.
    ///
    /// Performs lazy cleanup: cancelled tasks at the top of the heap are
    /// popped (O(log n) each).  In practice this is O(1) because cancelled
    /// tasks rarely accumulate at the heap root.
    ///
    /// Returns `None` if the queue is empty or contains only cancelled tasks.
    pub fn next_fire_at(&mut self) -> Option<Instant> {
        // Discard cancelled tasks sitting at the top of the heap.
        while self.heap.peek().is_some_and(|t| t.cancelled) {
            self.heap.pop();
        }
        self.heap.peek().map(|t| t.fire_at)
    }

    fn alloc_id(&mut self) -> TaskId {
        let id = TaskId(self.next_id);
        self.next_id += 1;
        id
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
