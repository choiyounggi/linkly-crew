//! Lead's `RoleBehavior` implementation (plan D4~D7, DESIGN.md §4.1 ④~⑥):
//! DAG-state dispatch, DoD self-execution on `task.result` (§4.2 — Lead
//! never trusts a worker's self-report), accept/rework/escalate, and
//! `human.gate` escalation that blocks only the affected task while the
//! rest of the DAG keeps moving (§4.4).

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use crew_agent::RoleBehavior;
use crew_proto::{Envelope, MessageKind, Role, TaskDag, TaskSpec};
use serde_json::{json, Value};

use crate::accept::{AcceptDecision, AcceptanceLoop};
use crate::dod_exec::{self, DodVerdict};

const DEADLINE_MS: u64 = 900_000;
const SPRINT_LABEL: &str = "sp-m3";

/// One task's dispatch state within this sprint run (plan D4). `Accepted`,
/// `Escalated`, and `Blocked` (plan M5 D6/contract C3a) are the terminal
/// states — [`LeadBehavior::is_done`] waits for every sprint task to reach
/// one of them. `Blocked` is reached only via cascade (an unresolved
/// `Escalated` task's timeout expiring, or a `with_prior_states` dep that
/// was not `Accepted`) — a task is never dispatched directly into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Assigned,
    Accepted,
    Escalated,
    Blocked,
}

impl TaskState {
    /// Single source of truth for "this task will not change state again"
    /// (plan M5 D6) — used by `is_done`, dep-readiness, and cascade so the
    /// terminal-state list lives in exactly one place.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Accepted | TaskState::Escalated | TaskState::Blocked
        )
    }
}

/// `dep_readiness`'s per-task verdict (plan D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepReadiness {
    Ready,
    Waiting,
    Blocked,
}

/// Lead's execution-layer state (plan D4): DAG-state dispatch + per-task
/// DoD/rework bookkeeping, wired into `AgentRunner` via `RoleBehavior`.
/// `roles` is a `Vec<(Role, String)>` rather than the plan's literal
/// `HashMap<Role, String>` — `crew_proto::Role` (t-proto, consume-only)
/// derives `Eq` but not `Hash`, so it cannot key a `HashMap`; a linear
/// lookup over five roles is the equivalent routing table without touching
/// crew-proto.
pub struct LeadBehavior {
    agent_id: String,
    dag: TaskDag,
    sprint: Vec<String>,
    roles: Vec<(Role, String)>,
    states: HashMap<String, TaskState>,
    loops: HashMap<String, AcceptanceLoop>,
    max_rework: u32,
    thread: String,
    /// 0 = disabled (default, contract C3a) — `on_tick` and `tick_interval`
    /// are no-ops and `pending_escalations` is never populated, so this
    /// task's whole cascade feature is inert unless a caller opts in.
    escalation_timeout_ms: u64,
    /// Injected via `with_prior_states` (contract C3a) — terminal states
    /// from an earlier sprint, consulted only for deps outside this
    /// sprint's own `dag`/`sprint`.
    prior_states: HashMap<String, TaskState>,
    /// task_id -> when its `human.gate` was first observed by `on_tick`
    /// (plan D3): `None` until the first tick after escalation, `Some(t0)`
    /// afterward so later ticks can compare `now_ms - t0` against the
    /// timeout. Only populated when `escalation_timeout_ms > 0`.
    pending_escalations: HashMap<String, Option<u64>>,
}

impl LeadBehavior {
    /// `sprint` is this run's task ids (M3 = one sprint); tasks outside it
    /// are ignored (plan D4). Every `sprint` id starts `Pending`.
    pub fn new(
        agent_id: impl Into<String>,
        dag: TaskDag,
        sprint: Vec<String>,
        roles: Vec<(Role, String)>,
        max_rework: u32,
    ) -> Self {
        let agent_id = agent_id.into();
        let thread = format!("th-{agent_id}");
        let states = sprint
            .iter()
            .map(|id| (id.clone(), TaskState::Pending))
            .collect();
        Self {
            agent_id,
            dag,
            sprint,
            roles,
            states,
            loops: HashMap::new(),
            max_rework,
            thread,
            escalation_timeout_ms: 0,
            prior_states: HashMap::new(),
            pending_escalations: HashMap::new(),
        }
    }

    /// `ms = 0` (default) disables escalation-timeout cascade entirely —
    /// existing behavior is fully preserved (contract C3a).
    pub fn escalation_timeout_ms(mut self, ms: u64) -> Self {
        self.escalation_timeout_ms = ms;
        self
    }

    /// Injects the previous sprint's terminal task states (contract C3a) —
    /// consulted by dispatch-time dep readiness for deps outside this
    /// sprint's own DAG.
    pub fn with_prior_states(mut self, states: HashMap<String, TaskState>) -> Self {
        self.prior_states = states;
        self
    }

    pub fn state_of(&self, task_id: &str) -> Option<TaskState> {
        self.states.get(task_id).copied()
    }

    #[cfg(test)]
    fn has_pending_escalation(&self, task_id: &str) -> bool {
        self.pending_escalations.contains_key(task_id)
    }

    fn task_by_id(&self, id: &str) -> Option<&TaskSpec> {
        self.dag.tasks.iter().find(|t| t.id == id)
    }

    fn role_agent(&self, role: Role) -> Option<&str> {
        self.roles
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, agent)| agent.as_str())
    }

    /// dep 상태 판정 (plan D5): 스프린트 내부 dep은 원래 규칙 그대로
    /// (`Accepted`만 충족, `Blocked`는 이 태스크도 즉시 `Blocked`로 캐스케이드
    /// — `dispatch_ready`의 fixpoint 루프가 여러 홉을 전이적으로 처리한다).
    /// 스프린트 밖 dep은 `prior_states` 미주입 시 기존처럼 자동 충족;
    /// 주입돼 있으면 prior 상태로 판정한다 (`with_prior_states`, 계약 C3a).
    fn dep_readiness(&self, id: &str) -> DepReadiness {
        let Some(task) = self.task_by_id(id) else {
            return DepReadiness::Waiting;
        };
        for dep in &task.deps {
            if self.sprint.contains(dep) {
                match self.states.get(dep) {
                    Some(TaskState::Accepted) => continue,
                    Some(TaskState::Blocked) => return DepReadiness::Blocked,
                    _ => return DepReadiness::Waiting,
                }
            }
            match self.prior_states.get(dep) {
                None | Some(TaskState::Accepted) => continue,
                Some(TaskState::Escalated) | Some(TaskState::Blocked) => {
                    return DepReadiness::Blocked
                }
                Some(TaskState::Pending) | Some(TaskState::Assigned) => {
                    return DepReadiness::Waiting
                }
            }
        }
        DepReadiness::Ready
    }

    fn human_gate(&self, task_id: &str, reason: &str, corr: &str, violations: Vec<String>) -> Envelope {
        Envelope::new(
            SPRINT_LABEL.to_string(),
            self.thread.clone(),
            self.agent_id.clone(),
            vec!["agent:human".to_string()],
            MessageKind::HumanGate,
            None,
            corr.to_string(),
            json!({"task_id": task_id, "reason": reason, "violations": violations}),
            vec![],
            true,
            DEADLINE_MS,
        )
    }

    /// Assigns every newly-ready `Pending` task (plan D5): an unregistered
    /// role escalates that single task immediately via `human.gate` instead
    /// of assigning it, without touching any other task's state. Runs to a
    /// fixpoint (re-scans until a pass changes nothing) so a dep-readiness
    /// `Blocked` verdict cascades across multiple hops within this sprint
    /// in one call, regardless of `sprint`'s iteration order — the common
    /// case (no `Blocked` states, nothing new dispatchable) always settles
    /// after its first dispatching pass plus one confirming no-op pass.
    fn dispatch_ready(&mut self) -> Vec<Envelope> {
        let mut envelopes = Vec::new();
        loop {
            let mut changed = false;
            for id in self.sprint.clone() {
                if self.states.get(&id) != Some(&TaskState::Pending) {
                    continue;
                }
                match self.dep_readiness(&id) {
                    DepReadiness::Waiting => continue,
                    DepReadiness::Blocked => {
                        self.states.insert(id.clone(), TaskState::Blocked);
                        changed = true;
                        continue;
                    }
                    DepReadiness::Ready => {}
                }
                let Some(task) = self.task_by_id(&id).cloned() else {
                    continue;
                };
                changed = true;
                let agent = self.role_agent(task.role).map(|a| a.to_string());
                match agent {
                    Some(agent) => {
                        self.states.insert(id.clone(), TaskState::Assigned);
                        self.loops
                            .entry(id.clone())
                            .or_insert_with(|| AcceptanceLoop::new(self.max_rework));
                        envelopes.push(Envelope::new(
                            SPRINT_LABEL.to_string(),
                            self.thread.clone(),
                            self.agent_id.clone(),
                            vec![agent],
                            MessageKind::TaskAssign,
                            None,
                            format!("corr-{id}"),
                            json!({"task": task}),
                            vec![],
                            true,
                            DEADLINE_MS,
                        ));
                    }
                    None => {
                        self.states.insert(id.clone(), TaskState::Escalated);
                        self.track_pending_escalation(&id);
                        envelopes.push(self.human_gate(
                            &id,
                            &format!("unregistered role: {:?}", task.role),
                            &format!("corr-{id}"),
                            vec![],
                        ));
                    }
                }
            }
            if !changed {
                break;
            }
        }
        envelopes
    }

    /// Starts this task's timeout clock at the first `on_tick` after its
    /// `human.gate` was published (plan D3) — a no-op when the cascade
    /// feature is disabled, so `pending_escalations` stays empty and
    /// `on_tick` remains a true no-op at the default `escalation_timeout_ms
    /// = 0` (contract C3a).
    fn track_pending_escalation(&mut self, task_id: &str) {
        if self.escalation_timeout_ms > 0 {
            self.pending_escalations
                .entry(task_id.to_string())
                .or_insert(None);
        }
    }

    /// `task.result` handling (plan D6): unmatched/unknown/non-`Assigned`
    /// tasks are ignored (defensive — a Lead is a pure state machine with
    /// no requester to reply to when the corr doesn't resolve).
    fn handle_task_result(&mut self, env: Envelope) -> Vec<Envelope> {
        let Some(task_id) = env.corr.strip_prefix("corr-") else {
            return vec![];
        };
        let task_id = task_id.to_string();
        if self.states.get(&task_id) != Some(&TaskState::Assigned) {
            return vec![];
        }
        let Some(task) = self.task_by_id(&task_id).cloned() else {
            return vec![];
        };

        let verdict = dod_exec::judge(&task, &env.body);
        let loop_ = self
            .loops
            .entry(task_id.clone())
            .or_insert_with(|| AcceptanceLoop::new(self.max_rework));

        match loop_.decide(&verdict) {
            AcceptDecision::Accept => {
                self.states.insert(task_id, TaskState::Accepted);
                self.dispatch_ready()
            }
            AcceptDecision::Rework { violations } => vec![Envelope::new(
                env.sprint.clone(),
                env.thread.clone(),
                self.agent_id.clone(),
                vec![env.from.clone()],
                MessageKind::ChangeRequest,
                Some(env.id.clone()),
                env.corr.clone(),
                json!({"violations": violations, "reason": "dod unmet"}),
                vec![],
                true,
                DEADLINE_MS,
            )],
            AcceptDecision::Escalate { reason } => {
                self.states.insert(task_id.clone(), TaskState::Escalated);
                self.track_pending_escalation(&task_id);
                vec![self.human_gate(&task_id, &reason, &env.corr, violation_strings(&verdict))]
            }
        }
    }

    /// Worker-issued `blocked` (plan D6): escalates the task immediately,
    /// bypassing DoD/rework — the worker itself reported it cannot proceed.
    fn handle_blocked(&mut self, env: Envelope) -> Vec<Envelope> {
        let Some(task_id) = env.corr.strip_prefix("corr-") else {
            return vec![];
        };
        let task_id = task_id.to_string();
        if self.states.get(&task_id) != Some(&TaskState::Assigned) {
            return vec![];
        }
        self.states.insert(task_id.clone(), TaskState::Escalated);
        self.track_pending_escalation(&task_id);
        let reason = env
            .body
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("worker blocked")
            .to_string();
        vec![self.human_gate(&task_id, &reason, &env.corr, vec![])]
    }

    /// All sprint task ids transitively depending on any id in `roots`
    /// (plan D4), computed by fixpoint over `self.dag.tasks` restricted to
    /// `self.sprint` — independent of `dispatch_ready`'s pass order, so an
    /// arbitrarily long dependency chain resolves in this one call.
    /// `roots` themselves are included in the returned set.
    fn transitively_blocked(&self, roots: &[String]) -> HashSet<String> {
        let mut blocked: HashSet<String> = roots.iter().cloned().collect();
        loop {
            let mut added = false;
            for task in &self.dag.tasks {
                if !self.sprint.contains(&task.id) || blocked.contains(&task.id) {
                    continue;
                }
                if task.deps.iter().any(|dep| blocked.contains(dep)) {
                    blocked.insert(task.id.clone());
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        blocked
    }

    /// Cascades every non-terminal sprint task transitively depending on
    /// `roots` to `Blocked` (plan D4). `roots` themselves stay `Escalated`
    /// — an expired escalation is not retried, only its dependents are
    /// released from limbo. Already-terminal tasks (e.g. `Accepted`) are
    /// left untouched.
    fn cascade_blocked(&mut self, roots: &[String]) {
        let blocked = self.transitively_blocked(roots);
        for id in blocked {
            if roots.contains(&id) {
                continue;
            }
            if let Some(state) = self.states.get(&id).copied() {
                if !state.is_terminal() {
                    self.states.insert(id, TaskState::Blocked);
                }
            }
        }
    }

    /// `human.response` handling (plan D2~D5, contract E3): acts only when
    /// the referenced task is currently `Escalated` — any other state,
    /// unknown `task_id`, or malformed body is silently ignored (0
    /// envelopes, no state change, no panic). `is_done`/`is_terminal` stay
    /// unchanged by design: a sprint the Lead already considers fully
    /// terminal (every task `Escalated`/`Accepted`/`Blocked`) cannot be
    /// reopened by a late `human.response` — the runner has already
    /// stopped polling for envelopes by then.
    fn handle_human_response(&mut self, env: Envelope) -> Vec<Envelope> {
        let Some(task_id) = env.body.get("task_id").and_then(Value::as_str) else {
            return vec![];
        };
        let decision = env.body.get("decision").and_then(Value::as_str);
        if self.states.get(task_id) != Some(&TaskState::Escalated) {
            return vec![];
        }
        let task_id = task_id.to_string();

        match decision {
            Some("approve") => self.approve_escalation(task_id),
            Some("reject") => {
                self.pending_escalations.remove(&task_id);
                self.states.insert(task_id.clone(), TaskState::Blocked);
                self.cascade_blocked(&[task_id]);
                vec![]
            }
            _ => vec![],
        }
    }

    /// Approve arm of `handle_human_response` (plan D3, contract E3):
    /// resets `task_id`'s `AcceptanceLoop` to a fresh `max_rework` budget
    /// (re-inserted, not `or_insert_with` — the prior exhausted loop must
    /// not survive), clears its pending-escalation bookkeeping, and drops
    /// it back to `Pending` so `dispatch_ready` (the existing dispatch
    /// path) re-assigns it with a fresh `task.assign` on the same
    /// deterministic `corr-{task_id}` — reusing that path rather than
    /// hand-building the envelope also means the pre-existing "role has no
    /// routed agent" fallback (re-escalate via `human_gate`) applies here
    /// for free if the routing table changed underneath an escalation.
    fn approve_escalation(&mut self, task_id: String) -> Vec<Envelope> {
        self.loops
            .insert(task_id.clone(), AcceptanceLoop::new(self.max_rework));
        self.pending_escalations.remove(&task_id);
        self.states.insert(task_id, TaskState::Pending);
        self.dispatch_ready()
    }
}

/// Same violation-string shape as `accept::AcceptanceLoop::decide`'s
/// `Rework` arm (`"REQ-x"` / `"artifact:name"`), rebuilt here because
/// `AcceptDecision::Escalate` (plan D3) carries only a `reason`, not the
/// verdict's violations — and `human.gate`'s body still needs them.
fn violation_strings(verdict: &DodVerdict) -> Vec<String> {
    verdict
        .uncovered
        .iter()
        .cloned()
        .chain(
            verdict
                .missing_artifacts
                .iter()
                .map(|name| format!("artifact:{name}")),
        )
        .collect()
}

#[async_trait]
impl RoleBehavior for LeadBehavior {
    async fn on_start(&mut self) -> Vec<Envelope> {
        self.dispatch_ready()
    }

    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        match env.kind {
            MessageKind::TaskResult => self.handle_task_result(env),
            MessageKind::Blocked => self.handle_blocked(env),
            MessageKind::HumanResponse => self.handle_human_response(env),
            _ => vec![],
        }
    }

    fn is_done(&self) -> bool {
        self.sprint
            .iter()
            .all(|id| self.states.get(id).is_some_and(|s| s.is_terminal()))
    }

    /// `escalation_timeout_ms = 0` (default) disables the tick branch
    /// entirely, preserving the original recv-only runner loop (contract
    /// C3a). Otherwise ticks at `escalation_timeout_ms / 4`, clamped to
    /// `[10ms, 1s]` (plan D7) — frequent enough to notice an expiry
    /// promptly without busy-polling.
    fn tick_interval(&self) -> Option<std::time::Duration> {
        if self.escalation_timeout_ms == 0 {
            return None;
        }
        let quarter = self.escalation_timeout_ms / 4;
        Some(std::time::Duration::from_millis(quarter.clamp(10, 1_000)))
    }

    /// Records each pending escalation's start time on its first tick, then
    /// cascades any that have now expired (plan D3/D4). A no-op at the
    /// default `escalation_timeout_ms = 0` — `pending_escalations` is never
    /// populated in that case either (`track_pending_escalation`), so this
    /// early return is also reachable if a caller invokes `on_tick`
    /// directly without going through `tick_interval`.
    async fn on_tick(&mut self, now_ms: u64) -> Vec<Envelope> {
        if self.escalation_timeout_ms == 0 {
            return Vec::new();
        }

        let mut expired = Vec::new();
        for (task_id, recorded_at) in self.pending_escalations.iter_mut() {
            match recorded_at {
                None => *recorded_at = Some(now_ms),
                Some(started) if now_ms.saturating_sub(*started) >= self.escalation_timeout_ms => {
                    expired.push(task_id.clone());
                }
                Some(_) => {}
            }
        }
        for task_id in &expired {
            self.pending_escalations.remove(task_id);
        }

        if !expired.is_empty() {
            self.cascade_blocked(&expired);
        }

        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::LeadPlanner;
    use serde_json::Value;

    fn roles_all() -> Vec<(Role, String)> {
        vec![
            (Role::Pm, "agent:pm".to_string()),
            (Role::Designer, "agent:designer".to_string()),
            (Role::Publisher, "agent:publisher".to_string()),
            (Role::Developer, "agent:developer".to_string()),
            (Role::Qa, "agent:qa".to_string()),
        ]
    }

    fn fixture_dag() -> TaskDag {
        let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
        LeadPlanner::plan_dag(&spec).unwrap()
    }

    fn lead(max_rework: u32) -> super::LeadBehavior {
        let dag = fixture_dag();
        let sprint = dag.validate().unwrap();
        super::LeadBehavior::new("agent:lead", dag, sprint, roles_all(), max_rework)
    }

    fn passing_result_body(task: &TaskSpec) -> Value {
        let mut covered = Vec::new();
        for check in &task.dod {
            if let crew_proto::DodCheck::ReqCover { ids } = check {
                for id in ids {
                    covered.push(id.as_str().to_string());
                }
            }
        }
        let artifacts: Vec<Value> = task
            .artifacts_expected
            .iter()
            .map(|contract| {
                json!({
                    "name": contract.name,
                    "kind": contract.kind,
                    "req_ids": contract.req_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                    "content": "ok",
                })
            })
            .collect();
        json!({"covered_req_ids": covered, "artifacts": artifacts})
    }

    fn failing_result_body() -> Value {
        json!({"covered_req_ids": [], "artifacts": []})
    }

    fn task_result_envelope(assign: &Envelope, from: &str, body: Value) -> Envelope {
        Envelope::new(
            assign.sprint.clone(),
            assign.thread.clone(),
            from.to_string(),
            vec![assign.from.clone()],
            MessageKind::TaskResult,
            Some(assign.id.clone()),
            assign.corr.clone(),
            body,
            vec![],
            true,
            DEADLINE_MS,
        )
    }

    fn blocked_envelope(assign: &Envelope, from: &str, reason: &str) -> Envelope {
        Envelope::new(
            assign.sprint.clone(),
            assign.thread.clone(),
            from.to_string(),
            vec![assign.from.clone()],
            MessageKind::Blocked,
            Some(assign.id.clone()),
            assign.corr.clone(),
            json!({"reason": reason}),
            vec![],
            true,
            DEADLINE_MS,
        )
    }

    fn human_response_envelope(body: Value) -> Envelope {
        Envelope::new(
            SPRINT_LABEL.to_string(),
            "th-agent:lead".to_string(),
            "agent:human".to_string(),
            vec!["agent:lead".to_string()],
            MessageKind::HumanResponse,
            None,
            "corr-human".to_string(),
            body,
            vec![],
            false,
            DEADLINE_MS,
        )
    }

    /// Drives `t-pm` to `Escalated` via rework-budget exhaustion (mirrors
    /// `budget_exhausted_escalates_via_human_gate_and_rest_of_dag_stays_pending`),
    /// returning the initial `task.assign` for reuse by callers that need
    /// its `corr`/`to`.
    async fn escalate_pm(lead: &mut super::LeadBehavior) -> Envelope {
        let assign = lead.on_start().await.remove(0);
        let first = task_result_envelope(&assign, "agent:pm", failing_result_body());
        let rework = lead.on_envelope(first).await;
        let second = task_result_envelope(&rework[0], "agent:pm", failing_result_body());
        let escalation = lead.on_envelope(second).await;
        assert_eq!(escalation[0].kind, MessageKind::HumanGate);
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
        assign
    }

    #[tokio::test]
    async fn on_start_assigns_only_the_dep_free_task() {
        let mut lead = lead(AcceptanceLoop::default_budget());

        let envelopes = lead.on_start().await;

        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].kind, MessageKind::TaskAssign);
        assert_eq!(envelopes[0].to, vec!["agent:pm".to_string()]);
        assert_eq!(envelopes[0].corr, "corr-t-pm");
        assert!(envelopes[0].requires_ack);
        let task: TaskSpec = serde_json::from_value(envelopes[0].body["task"].clone()).unwrap();
        assert_eq!(task.id, "t-pm");
        assert_eq!(lead.state_of("t-design"), Some(TaskState::Pending));
    }

    #[tokio::test]
    async fn accepted_result_assigns_the_next_task() {
        let mut lead = lead(AcceptanceLoop::default_budget());
        let assign = lead.on_start().await.remove(0);
        let pm_task: TaskSpec = serde_json::from_value(assign.body["task"].clone()).unwrap();

        let result = task_result_envelope(&assign, "agent:pm", passing_result_body(&pm_task));
        let replies = lead.on_envelope(result).await;

        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Accepted));
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::TaskAssign);
        assert_eq!(replies[0].to, vec!["agent:designer".to_string()]);
        let task: TaskSpec = serde_json::from_value(replies[0].body["task"].clone()).unwrap();
        assert_eq!(task.id, "t-design");
    }

    #[tokio::test]
    async fn violating_result_sends_change_request_on_the_same_corr() {
        let mut lead = lead(AcceptanceLoop::default_budget());
        let assign = lead.on_start().await.remove(0);

        let result = task_result_envelope(&assign, "agent:pm", failing_result_body());
        let replies = lead.on_envelope(result.clone()).await;

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::ChangeRequest);
        assert_eq!(replies[0].corr, assign.corr);
        assert_eq!(replies[0].in_reply_to, Some(result.id.clone()));
        let violations = replies[0].body["violations"].as_array().unwrap();
        assert!(!violations.is_empty());
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Assigned));
    }

    #[tokio::test]
    async fn budget_exhausted_escalates_via_human_gate_and_rest_of_dag_stays_pending() {
        let mut lead = lead(1);
        let assign = lead.on_start().await.remove(0);

        let first = task_result_envelope(&assign, "agent:pm", failing_result_body());
        let rework = lead.on_envelope(first).await;
        assert_eq!(rework[0].kind, MessageKind::ChangeRequest);

        let second = task_result_envelope(&rework[0], "agent:pm", failing_result_body());
        let escalation = lead.on_envelope(second).await;

        assert_eq!(escalation.len(), 1);
        assert_eq!(escalation[0].kind, MessageKind::HumanGate);
        assert_eq!(escalation[0].to, vec!["agent:human".to_string()]);
        assert_eq!(escalation[0].body["task_id"], json!("t-pm"));
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
        assert_eq!(lead.state_of("t-design"), Some(TaskState::Pending));
        assert!(!lead.is_done());
    }

    #[tokio::test]
    async fn worker_blocked_escalates_via_human_gate() {
        let mut lead = lead(AcceptanceLoop::default_budget());
        let assign = lead.on_start().await.remove(0);

        let blocked = blocked_envelope(&assign, "agent:pm", "cannot proceed");
        let replies = lead.on_envelope(blocked).await;

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::HumanGate);
        assert_eq!(replies[0].body["task_id"], json!("t-pm"));
        assert_eq!(replies[0].body["reason"], json!("cannot proceed"));
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
    }

    #[tokio::test]
    async fn all_tasks_accepted_marks_the_sprint_done() {
        let mut lead = lead(AcceptanceLoop::default_budget());
        let mut assign = lead.on_start().await.remove(0);
        let agents = [
            "agent:pm",
            "agent:designer",
            "agent:publisher",
            "agent:developer",
            "agent:qa",
        ];

        for (i, from) in agents.iter().enumerate() {
            let task: TaskSpec = serde_json::from_value(assign.body["task"].clone()).unwrap();
            let result = task_result_envelope(&assign, from, passing_result_body(&task));
            let replies = lead.on_envelope(result).await;

            if i + 1 < agents.len() {
                assert!(!lead.is_done());
                assert_eq!(replies.len(), 1);
                assign = replies.into_iter().next().unwrap();
            } else {
                assert!(replies.is_empty());
                assert!(lead.is_done());
            }
        }
    }

    #[tokio::test]
    async fn task_result_with_unknown_corr_produces_no_reply() {
        let mut lead = lead(AcceptanceLoop::default_budget());
        let _ = lead.on_start().await;

        let stray = Envelope::new(
            SPRINT_LABEL.to_string(),
            "th-agent:lead".to_string(),
            "agent:pm".to_string(),
            vec!["agent:lead".to_string()],
            MessageKind::TaskResult,
            None,
            "corr-unknown-task".to_string(),
            json!({"covered_req_ids": [], "artifacts": []}),
            vec![],
            true,
            DEADLINE_MS,
        );

        let replies = lead.on_envelope(stray).await;

        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn unregistered_role_escalates_immediately_via_human_gate() {
        let dag = fixture_dag();
        let sprint = dag.validate().unwrap();
        // Pm intentionally missing from the routing table.
        let roles = vec![
            (Role::Designer, "agent:designer".to_string()),
            (Role::Publisher, "agent:publisher".to_string()),
            (Role::Developer, "agent:developer".to_string()),
            (Role::Qa, "agent:qa".to_string()),
        ];
        let mut lead = super::LeadBehavior::new(
            "agent:lead",
            dag,
            sprint,
            roles,
            AcceptanceLoop::default_budget(),
        );

        let envelopes = lead.on_start().await;

        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].kind, MessageKind::HumanGate);
        assert_eq!(envelopes[0].to, vec!["agent:human".to_string()]);
        assert_eq!(envelopes[0].body["task_id"], json!("t-pm"));
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
    }

    #[test]
    fn is_terminal_covers_exactly_accepted_escalated_and_blocked() {
        assert!(!TaskState::Pending.is_terminal());
        assert!(!TaskState::Assigned.is_terminal());
        assert!(TaskState::Accepted.is_terminal());
        assert!(TaskState::Escalated.is_terminal());
        assert!(TaskState::Blocked.is_terminal());
    }

    #[test]
    fn tick_interval_is_none_at_default_zero_timeout() {
        let lead = lead(AcceptanceLoop::default_budget());

        assert_eq!(lead.tick_interval(), None);
    }

    #[test]
    fn tick_interval_clamps_a_quarter_of_the_timeout_into_ten_ms_to_one_s() {
        let normal = lead(AcceptanceLoop::default_budget()).escalation_timeout_ms(400);
        assert_eq!(
            normal.tick_interval(),
            Some(std::time::Duration::from_millis(100))
        );

        let below_floor = lead(AcceptanceLoop::default_budget()).escalation_timeout_ms(20);
        assert_eq!(
            below_floor.tick_interval(),
            Some(std::time::Duration::from_millis(10))
        );

        let above_ceiling = lead(AcceptanceLoop::default_budget()).escalation_timeout_ms(8_000);
        assert_eq!(
            above_ceiling.tick_interval(),
            Some(std::time::Duration::from_millis(1_000))
        );
    }

    #[tokio::test]
    async fn approve_resets_budget_reassigns_and_reaches_accepted() {
        let mut lead = lead(1).escalation_timeout_ms(1_000);
        let _ = escalate_pm(&mut lead).await;
        let _ = lead.on_tick(0).await;
        assert!(lead.has_pending_escalation("t-pm"));

        let approve = human_response_envelope(
            json!({"task_id": "t-pm", "decision": "approve", "reason": "looks fine now"}),
        );
        let reassign = lead.on_envelope(approve).await;

        assert_eq!(reassign.len(), 1);
        assert_eq!(reassign[0].kind, MessageKind::TaskAssign);
        assert_eq!(reassign[0].to, vec!["agent:pm".to_string()]);
        assert_eq!(reassign[0].corr, "corr-t-pm");
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Assigned));
        assert!(!lead.has_pending_escalation("t-pm"));

        let pm_task: TaskSpec = serde_json::from_value(reassign[0].body["task"].clone()).unwrap();
        let result = task_result_envelope(&reassign[0], "agent:pm", passing_result_body(&pm_task));
        let replies = lead.on_envelope(result).await;

        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Accepted));
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::TaskAssign);
        assert_eq!(replies[0].to, vec!["agent:designer".to_string()]);
    }

    #[tokio::test]
    async fn reject_blocks_target_and_cascades_to_transitive_dependents() {
        let mut lead = lead(1).escalation_timeout_ms(1_000);
        let pm_assign = lead.on_start().await.remove(0);
        let pm_task: TaskSpec = serde_json::from_value(pm_assign.body["task"].clone()).unwrap();
        let pm_result = task_result_envelope(&pm_assign, "agent:pm", passing_result_body(&pm_task));
        let design_assign = lead.on_envelope(pm_result).await.remove(0);
        assert_eq!(design_assign.to, vec!["agent:designer".to_string()]);

        let first = task_result_envelope(&design_assign, "agent:designer", failing_result_body());
        let rework = lead.on_envelope(first).await;
        let second = task_result_envelope(&rework[0], "agent:designer", failing_result_body());
        let escalation = lead.on_envelope(second).await;
        assert_eq!(escalation[0].kind, MessageKind::HumanGate);
        assert_eq!(lead.state_of("t-design"), Some(TaskState::Escalated));
        let _ = lead.on_tick(0).await;
        assert!(lead.has_pending_escalation("t-design"));

        let reject = human_response_envelope(
            json!({"task_id": "t-design", "decision": "reject", "reason": "wrong direction"}),
        );
        let replies = lead.on_envelope(reject).await;

        assert!(replies.is_empty());
        assert!(!lead.has_pending_escalation("t-design"));
        assert_eq!(lead.state_of("t-design"), Some(TaskState::Blocked));
        assert_eq!(lead.state_of("t-publish"), Some(TaskState::Blocked));
        assert_eq!(lead.state_of("t-dev"), Some(TaskState::Blocked));
        assert_eq!(lead.state_of("t-qa"), Some(TaskState::Blocked));
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Accepted));
        assert!(lead.is_done());
    }

    #[tokio::test]
    async fn human_response_for_non_escalated_task_is_ignored() {
        let mut lead = lead(AcceptanceLoop::default_budget());
        let _ = lead.on_start().await;
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Assigned));

        let approve =
            human_response_envelope(json!({"task_id": "t-pm", "decision": "approve", "reason": "premature"}));
        let replies = lead.on_envelope(approve).await;

        assert!(replies.is_empty());
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Assigned));
    }

    #[tokio::test]
    async fn human_response_for_unknown_task_id_produces_no_reply_and_no_panic() {
        let mut lead = lead(1);
        let _ = escalate_pm(&mut lead).await;

        let stray =
            human_response_envelope(json!({"task_id": "t-does-not-exist", "decision": "approve", "reason": "n/a"}));
        let replies = lead.on_envelope(stray).await;

        assert!(replies.is_empty());
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
    }

    #[tokio::test]
    async fn human_response_with_malformed_body_is_ignored() {
        let mut lead = lead(1);
        let _ = escalate_pm(&mut lead).await;

        let unknown_decision =
            human_response_envelope(json!({"task_id": "t-pm", "decision": "maybe", "reason": "unsure"}));
        let replies = lead.on_envelope(unknown_decision).await;
        assert!(replies.is_empty());
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));

        let missing_task_id = human_response_envelope(json!({"decision": "approve"}));
        let replies2 = lead.on_envelope(missing_task_id).await;
        assert!(replies2.is_empty());
        assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
    }
}
