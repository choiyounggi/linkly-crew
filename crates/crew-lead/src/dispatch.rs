//! Lead's `RoleBehavior` implementation (plan D4~D7, DESIGN.md §4.1 ④~⑥):
//! DAG-state dispatch, DoD self-execution on `task.result` (§4.2 — Lead
//! never trusts a worker's self-report), accept/rework/escalate, and
//! `human.gate` escalation that blocks only the affected task while the
//! rest of the DAG keeps moving (§4.4).

use std::collections::HashMap;

use async_trait::async_trait;
use crew_agent::RoleBehavior;
use crew_proto::{Envelope, MessageKind, Role, TaskDag, TaskSpec};
use serde_json::{json, Value};

use crate::accept::{AcceptDecision, AcceptanceLoop};
use crate::dod_exec::{self, DodVerdict};

const DEADLINE_MS: u64 = 900_000;
const SPRINT_LABEL: &str = "sp-m3";

/// One task's dispatch state within this sprint run (plan D4). `Accepted`
/// and `Escalated` are the only terminal states — [`LeadBehavior::is_done`]
/// (plan D7) waits for every sprint task to reach one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Assigned,
    Accepted,
    Escalated,
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
        }
    }

    pub fn state_of(&self, task_id: &str) -> Option<TaskState> {
        self.states.get(task_id).copied()
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

    /// ready = Pending ∧ (모든 dep이 Accepted ∨ 스프린트 밖) — plan D5.
    fn is_ready(&self, id: &str) -> bool {
        let Some(task) = self.task_by_id(id) else {
            return false;
        };
        task.deps.iter().all(|dep| {
            !self.sprint.contains(dep) || self.states.get(dep) == Some(&TaskState::Accepted)
        })
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
    /// of assigning it, without touching any other task's state.
    fn dispatch_ready(&mut self) -> Vec<Envelope> {
        let mut envelopes = Vec::new();
        for id in self.sprint.clone() {
            if self.states.get(&id) != Some(&TaskState::Pending) || !self.is_ready(&id) {
                continue;
            }
            let Some(task) = self.task_by_id(&id).cloned() else {
                continue;
            };
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
                    envelopes.push(self.human_gate(
                        &id,
                        &format!("unregistered role: {:?}", task.role),
                        &format!("corr-{id}"),
                        vec![],
                    ));
                }
            }
        }
        envelopes
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
        let reason = env
            .body
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("worker blocked")
            .to_string();
        vec![self.human_gate(&task_id, &reason, &env.corr, vec![])]
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
            _ => vec![],
        }
    }

    fn is_done(&self) -> bool {
        self.sprint.iter().all(|id| {
            matches!(
                self.states.get(id),
                Some(TaskState::Accepted) | Some(TaskState::Escalated)
            )
        })
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
}
