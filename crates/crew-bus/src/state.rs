use std::collections::{HashMap, HashSet};
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

/// Bus-wide shared state, reached by every connection task and retry task —
/// plan B1/B3/B4/B5/B6/B9.
pub(crate) struct Shared {
    pub(crate) cfg: BusConfig,
    pub(crate) registry: Mutex<HashMap<String, mpsc::Sender<ServerFrame>>>,
    pub(crate) pending: Mutex<HashMap<(String, String), PendingEntry>>,
    pub(crate) seen: Mutex<HashSet<String>>,
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
