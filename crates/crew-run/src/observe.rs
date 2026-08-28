//! `ObservingLead` — `RecordingLead` pattern (m3_sprint.rs:51) promoted to
//! app code (plan D4): wraps `LeadBehavior`, polls `state_of()` for every
//! sprint task id after each `RoleBehavior` call, and forwards only the
//! ids whose state actually changed to the controller's relay channel. The
//! controller (not this wrapper) owns the single `RunEvent` broadcast
//! point, so this only sends — it never emits `RunEvent` itself.

use std::collections::HashMap;

use async_trait::async_trait;
use crew_agent::RoleBehavior;
use crew_lead::dispatch::{LeadBehavior, TaskState};
use crew_proto::Envelope;
use tokio::sync::mpsc;

use crate::events::TaskStateDto;

/// One task's state change, as observed after a `RoleBehavior` call.
pub struct TaskStateChange {
    pub task_id: String,
    pub state: TaskStateDto,
}

pub struct ObservingLead {
    inner: LeadBehavior,
    task_ids: Vec<String>,
    prev: HashMap<String, TaskState>,
    tx: mpsc::UnboundedSender<TaskStateChange>,
}

impl ObservingLead {
    /// Every sprint task id starts at `TaskState::Pending` (same as
    /// `LeadBehavior::new`'s own initial state), so the first `observe()`
    /// only reports tasks that have actually left `Pending` — not a
    /// spurious "changed to Pending" for every task on `on_start`.
    pub fn new(
        inner: LeadBehavior,
        task_ids: Vec<String>,
        tx: mpsc::UnboundedSender<TaskStateChange>,
    ) -> Self {
        let prev = task_ids
            .iter()
            .map(|id| (id.clone(), TaskState::Pending))
            .collect();
        Self {
            inner,
            task_ids,
            prev,
            tx,
        }
    }

    /// Diffs every sprint task id's `state_of()` against the last-observed
    /// value, forwarding only the ones that changed — mirrors
    /// `RecordingLead::record`'s "observe after every call" shape, but
    /// diffs state instead of mirroring emitted envelopes.
    fn observe(&mut self) {
        for id in &self.task_ids {
            let Some(state) = self.inner.state_of(id) else {
                continue;
            };
            if self.prev.get(id) == Some(&state) {
                continue;
            }
            self.prev.insert(id.clone(), state);
            // The relay task may already be gone (controller shutdown
            // mid-run) — dropping the update is fine, there is no reader
            // left to observe it.
            let _ = self.tx.send(TaskStateChange {
                task_id: id.clone(),
                state: state.into(),
            });
        }
    }
}

#[async_trait]
impl RoleBehavior for ObservingLead {
    async fn on_start(&mut self) -> Vec<Envelope> {
        let envs = self.inner.on_start().await;
        self.observe();
        envs
    }

    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        let envs = self.inner.on_envelope(env).await;
        self.observe();
        envs
    }

    fn is_done(&self) -> bool {
        self.inner.is_done()
    }
}
