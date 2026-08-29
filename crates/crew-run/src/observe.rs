//! `ObservingLead` — `RecordingLead` pattern (m3_sprint.rs:51) promoted to
//! app code (plan D4): wraps `LeadBehavior`, polls `state_of()` for every
//! sprint task id after each `RoleBehavior` call, and forwards only the
//! ids whose state actually changed to the controller's relay channel. The
//! controller (not this wrapper) owns the single `RunEvent` broadcast
//! point, so this only sends — it never emits `RunEvent` itself.
//!
//! contracts-m5.md §C5a's "ObservingLead 틱 위임 의무": `tick_interval`/
//! `on_tick` are `RoleBehavior` default methods (contract §C2), so a
//! wrapper that doesn't forward them silently disables the escalation-
//! timeout feature end to end even though `LeadBehavior` itself implements
//! it correctly. Both are delegated to `inner` here, and `on_tick`'s
//! cascade side effects go through the same `observe()` diff extraction
//! `on_start`/`on_envelope` already use (plan D3 — one policy, one call
//! site: testing/quality/policy-at-several-return-sites.md).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    /// This sprint's task ids' raw `TaskState`, mirrored here (in addition
    /// to `tx`'s `TaskStateDto` relay) so the controller can read a just-
    /// finished sprint's terminal states straight back out — without a
    /// lossy `TaskStateDto` round trip — to build the next sprint's
    /// `with_prior_states` map (contracts-m5.md §C5a). Shared across every
    /// sprint's `ObservingLead` for the run, so it accumulates.
    cumulative: Arc<Mutex<HashMap<String, TaskState>>>,
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
        cumulative: Arc<Mutex<HashMap<String, TaskState>>>,
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
            cumulative,
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
            self.cumulative
                .lock()
                .expect("cumulative state mutex poisoned")
                .insert(id.clone(), state);
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

    fn tick_interval(&self) -> Option<std::time::Duration> {
        self.inner.tick_interval()
    }

    async fn on_tick(&mut self, now_ms: u64) -> Vec<Envelope> {
        let envs = self.inner.on_tick(now_ms).await;
        self.observe();
        envs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_lead::accept::AcceptanceLoop;
    use crew_lead::plan::LeadPlanner;
    use crew_proto::Role;

    fn fixture_sprint() -> (crew_proto::TaskDag, Vec<String>) {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        let dag = LeadPlanner::plan_dag(&spec).unwrap();
        (dag, vec!["t-pm".to_string(), "t-design".to_string()])
    }

    /// contracts-m5.md §C5a's tick-delegation obligation, proven directly:
    /// `t-pm`'s role is left out of the routing table so `on_start`
    /// escalates it immediately (`unregistered_role_escalates_...` pattern
    /// in `crew_lead::dispatch`'s own tests); once its escalation timeout
    /// expires on a later `on_tick`, its dependent `t-design` cascades to
    /// `Blocked` — and that cascade must reach `TaskStateChanged` only
    /// because `ObservingLead::on_tick` delegates to `inner.on_tick` and
    /// re-runs `observe()` afterward (plan D3/contract C5a's explicit
    /// warning that a non-delegating wrapper silently kills this feature).
    #[tokio::test]
    async fn on_tick_cascade_is_observed_as_task_state_changed() {
        let (dag, sprint) = fixture_sprint();
        let roles = vec![
            // `Role::Pm` intentionally missing.
            (Role::Designer, "agent:designer".to_string()),
            (Role::Publisher, "agent:publisher".to_string()),
            (Role::Developer, "agent:developer".to_string()),
            (Role::Qa, "agent:qa".to_string()),
        ];
        let inner = LeadBehavior::new(
            "agent:lead",
            dag,
            sprint.clone(),
            roles,
            AcceptanceLoop::default_budget(),
        )
        .escalation_timeout_ms(50);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cumulative = Arc::new(Mutex::new(HashMap::new()));
        let mut observing = ObservingLead::new(inner, sprint, tx, cumulative.clone());

        let _ = observing.on_start().await;
        let first = rx.try_recv().expect("t-pm's escalation must be observed");
        assert_eq!(first.task_id, "t-pm");
        assert_eq!(first.state, TaskStateDto::Escalated);

        // Starts the timeout clock; not yet expired.
        assert!(observing.on_tick(0).await.is_empty());
        assert!(
            rx.try_recv().is_err(),
            "no state changed on the tick that only starts the clock"
        );

        // Timeout has now elapsed: `t-design` (depends on the expired
        // `t-pm` escalation) cascades to `Blocked`.
        assert!(observing.on_tick(50).await.is_empty());
        let cascaded = rx
            .try_recv()
            .expect("the cascade during on_tick must be observed as a TaskStateChange");
        assert_eq!(cascaded.task_id, "t-design");
        assert_eq!(cascaded.state, TaskStateDto::Blocked);

        assert_eq!(
            cumulative.lock().unwrap().get("t-design").copied(),
            Some(TaskState::Blocked),
            "the shared cumulative map must reflect the cascade too (feeds the next sprint's with_prior_states)"
        );
    }

    #[test]
    fn tick_interval_delegates_to_inner() {
        let (dag, sprint) = fixture_sprint();
        let roles = vec![
            (Role::Pm, "agent:pm".to_string()),
            (Role::Designer, "agent:designer".to_string()),
            (Role::Publisher, "agent:publisher".to_string()),
            (Role::Developer, "agent:developer".to_string()),
            (Role::Qa, "agent:qa".to_string()),
        ];
        let disabled = LeadBehavior::new(
            "agent:lead",
            dag.clone(),
            sprint.clone(),
            roles.clone(),
            AcceptanceLoop::default_budget(),
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        let observing = ObservingLead::new(disabled, sprint.clone(), tx, Arc::new(Mutex::new(HashMap::new())));
        assert_eq!(
            observing.tick_interval(),
            None,
            "escalation_timeout_ms=0 (default) must stay a no-op end to end"
        );

        let enabled = LeadBehavior::new("agent:lead", dag, sprint.clone(), roles, AcceptanceLoop::default_budget())
            .escalation_timeout_ms(400);
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let observing_enabled = ObservingLead::new(enabled, sprint, tx2, Arc::new(Mutex::new(HashMap::new())));
        assert_eq!(
            observing_enabled.tick_interval(),
            Some(std::time::Duration::from_millis(100))
        );
    }
}
