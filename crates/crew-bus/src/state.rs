use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use crew_proto::{CorrGuard, Envelope, ServerFrame};
use tokio::sync::{broadcast, mpsc};

use crate::config::BusConfig;
use crate::event::BusEvent;

/// One in-flight `requires_ack` delivery, tracked per (envelope id, recipient)
/// so a multi-recipient envelope's recipients ack independently — plan B4.
pub(crate) struct PendingEntry {
    pub(crate) envelope: Envelope,
    pub(crate) attempts: u32,
}

/// Submission dedup window (plan B5), capacity-bounded so long-running bus
/// processes don't grow this set forever: past `capacity` entries, the
/// oldest id is evicted to make room. An evicted id falls outside the dedup
/// window — a resubmission is treated as new and re-routed, relying on
/// at-least-once semantics (the recipient's `Receipt` is the idempotent
/// terminus) rather than unbounded dedup memory.
pub(crate) struct SeenSet {
    order: VecDeque<String>,
    set: HashSet<String>,
    capacity: usize,
}

impl SeenSet {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            order: VecDeque::new(),
            set: HashSet::new(),
            capacity,
        }
    }

    /// Inserts `id`, evicting the oldest tracked id if at capacity. Returns
    /// `true` if `id` was not already tracked (i.e. it's new).
    pub(crate) fn insert(&mut self, id: String) -> bool {
        if self.set.contains(&id) {
            return false;
        }
        if self.order.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
        self.order.push_back(id.clone());
        self.set.insert(id);
        true
    }
}

/// Bus-wide shared state, reached by every connection task and retry task —
/// plan B1/B3/B4/B5/B6/B9.
pub(crate) struct Shared {
    pub(crate) cfg: BusConfig,
    pub(crate) registry: Mutex<HashMap<String, mpsc::Sender<ServerFrame>>>,
    pub(crate) pending: Mutex<HashMap<(String, String), PendingEntry>>,
    pub(crate) seen: Mutex<SeenSet>,
    pub(crate) guard: Mutex<CorrGuard>,
    pub(crate) events: broadcast::Sender<BusEvent>,
}

/// Enqueues `frame` on `agent_id`'s outbound channel. Plan B3: a full
/// channel (a slow client not draining) closes and unregisters that
/// connection instead of blocking or growing unbounded. Returns whether the
/// frame was handed off.
pub(crate) fn send_to(shared: &Shared, agent_id: &str, frame: ServerFrame) -> bool {
    let mut registry = shared.registry.lock().unwrap();
    let Some(tx) = registry.get(agent_id) else {
        return false;
    };
    match tx.try_send(frame) {
        Ok(()) => true,
        Err(_) => {
            registry.remove(agent_id);
            drop(registry);
            let _ = shared.events.send(BusEvent::Unregistered {
                agent_id: agent_id.to_string(),
            });
            false
        }
    }
}

/// Removes `agent_id` from the registry if present and emits exactly one
/// `Unregistered` event for it — idempotent so both the backpressure path
/// (`send_to`) and the connection's own cleanup can call it safely.
pub(crate) fn unregister(shared: &Shared, agent_id: &str) {
    let removed = shared.registry.lock().unwrap().remove(agent_id).is_some();
    if removed {
        let _ = shared.events.send(BusEvent::Unregistered {
            agent_id: agent_id.to_string(),
        });
    }
}
