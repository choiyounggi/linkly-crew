//! M5 escalation timeout + cascade (t-escal brief, contract C3a): proves
//! that once `escalation_timeout_ms` is configured, an unresolved
//! `Escalated` task's timeout expiry cascades `Blocked` to every sprint
//! task that (transitively) depends on it, so the sprint always reaches
//! `is_done()` instead of stranding forever (HANDOFF §5 pitfall 9) — and
//! that at the default `escalation_timeout_ms = 0`, none of this fires
//! (existing behavior fully preserved). `crew-lead/tests/m3_sprint.rs` is
//! untouched; this is a new file per that constraint.
//!
//! `on_tick(now_ms)` is called directly with an injected clock rather than
//! driven through `AgentRunner`/a real bus — the cascade/prior-state logic
//! is pure `LeadBehavior` state, so this keeps every scenario below
//! deterministic with no async timing at all (`AgentRunner`'s own tick
//! *loop* mechanics are covered separately by
//! `crew-agent/src/runner.rs`'s `tick_interval_some_drives_on_tick_and_sends_its_envelopes`).

use std::collections::HashMap;

use crew_agent::RoleBehavior;
use crew_lead::accept::AcceptanceLoop;
use crew_lead::dispatch::{LeadBehavior, TaskState};
use crew_lead::plan::LeadPlanner;
use crew_proto::{DodCheck, Envelope, MessageKind, ReqId, Role, TaskDag, TaskSpec};
use serde_json::json;

const DEADLINE_MS: u64 = 900_000;

fn roles_all() -> Vec<(Role, String)> {
    vec![
        (Role::Pm, "agent:pm".to_string()),
        (Role::Designer, "agent:designer".to_string()),
        (Role::Publisher, "agent:publisher".to_string()),
        (Role::Developer, "agent:developer".to_string()),
        (Role::Qa, "agent:qa".to_string()),
    ]
}

/// The M3 planner's fixed linear chain t-pm -> t-design -> t-publish ->
/// t-dev -> t-qa — a direct-plus-transitive dependency chain, exactly what
/// the cascade needs to exercise.
fn linear_chain_dag() -> (TaskDag, Vec<String>) {
    let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
    let dag = LeadPlanner::plan_dag(&spec).unwrap();
    let sprint = dag.validate().unwrap();
    (dag, sprint)
}

/// `dod` requires one REQ id so `failing_result_body()` (empty
/// `covered_req_ids`) actually fails DoD judgment — an empty `dod` list
/// would trivially pass any result, masking the escalation path this file
/// tests.
fn bare_task(id: &str, role: Role, deps: &[&str]) -> TaskSpec {
    TaskSpec {
        id: id.to_string(),
        role,
        title: id.to_string(),
        brief: id.to_string(),
        dod: vec![DodCheck::ReqCover {
            ids: vec![ReqId::new("REQ-1").unwrap()],
        }],
        deps: deps.iter().map(|d| d.to_string()).collect(),
        artifacts_expected: vec![],
    }
}

fn failing_result_body() -> serde_json::Value {
    json!({"covered_req_ids": [], "artifacts": []})
}

fn task_result_envelope(assign: &Envelope, from: &str, body: serde_json::Value) -> Envelope {
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

fn find_assign<'a>(envelopes: &'a [Envelope], to: &str) -> &'a Envelope {
    envelopes
        .iter()
        .find(|e| e.kind == MessageKind::TaskAssign && e.to == vec![to.to_string()])
        .unwrap_or_else(|| panic!("no TaskAssign to {to}"))
}

/// Escalates `task_id` on the first (and only) attempt by using
/// `max_rework = 0` (accept.rs: budget exhausted on round 0 escalates
/// immediately) so tests don't need to thread multiple rework rounds
/// through just to reach `Escalated`.
async fn escalate_immediately(lead: &mut LeadBehavior, assign: &Envelope, from: &str) {
    let result = task_result_envelope(assign, from, failing_result_body());
    let replies = lead.on_envelope(result).await;
    assert_eq!(
        replies.len(),
        1,
        "budget-0 failure must escalate in one round"
    );
    assert_eq!(replies[0].kind, MessageKind::HumanGate);
}

/// ① normal: timeout expiry cascades Blocked to every direct + transitive
/// dependent, and the sprint reaches `is_done()`.
#[tokio::test]
async fn expired_escalation_cascades_blocked_to_direct_and_transitive_dependents() {
    let (dag, sprint) = linear_chain_dag();
    let mut lead =
        LeadBehavior::new("agent:lead", dag, sprint, roles_all(), 0).escalation_timeout_ms(1_000);

    let assign = lead.on_start().await.remove(0);
    assert_eq!(assign.to, vec!["agent:pm".to_string()]);
    escalate_immediately(&mut lead, &assign, "agent:pm").await;
    assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));

    // First tick only records the escalation's start time.
    let first_tick = lead.on_tick(1_000).await;
    assert!(first_tick.is_empty());
    assert_eq!(lead.state_of("t-design"), Some(TaskState::Pending));
    assert!(!lead.is_done());

    // 1000ms later == the configured timeout: expires and cascades.
    let expiry_tick = lead.on_tick(2_000).await;
    assert!(
        expiry_tick.is_empty(),
        "cascade publishes no envelopes, only state changes"
    );

    assert_eq!(
        lead.state_of("t-pm"),
        Some(TaskState::Escalated),
        "root stays Escalated, not retried"
    );
    assert_eq!(lead.state_of("t-design"), Some(TaskState::Blocked));
    assert_eq!(lead.state_of("t-publish"), Some(TaskState::Blocked));
    assert_eq!(lead.state_of("t-dev"), Some(TaskState::Blocked));
    assert_eq!(lead.state_of("t-qa"), Some(TaskState::Blocked));
    assert!(lead.is_done(), "every sprint task must now be terminal");
}

/// ② boundary: `now_ms` short of the deadline leaves every state
/// unchanged.
#[tokio::test]
async fn tick_before_deadline_leaves_states_unchanged() {
    let (dag, sprint) = linear_chain_dag();
    let mut lead =
        LeadBehavior::new("agent:lead", dag, sprint, roles_all(), 0).escalation_timeout_ms(1_000);

    let assign = lead.on_start().await.remove(0);
    escalate_immediately(&mut lead, &assign, "agent:pm").await;

    let _ = lead.on_tick(1_000).await; // records start=1000
    let _ = lead.on_tick(1_500).await; // 500ms elapsed < 1000ms timeout

    assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
    assert_eq!(lead.state_of("t-design"), Some(TaskState::Pending));
    assert!(!lead.is_done());
}

/// ③ boundary: `escalation_timeout_ms = 0` (default) — `on_tick` is a
/// permanent no-op no matter how large `now_ms` is, exactly preserving the
/// pre-M5 behavior of a stranded downstream task.
#[tokio::test]
async fn zero_timeout_default_never_cascades_even_with_a_huge_now() {
    let (dag, sprint) = linear_chain_dag();
    let mut lead = LeadBehavior::new("agent:lead", dag, sprint, roles_all(), 0);

    let assign = lead.on_start().await.remove(0);
    escalate_immediately(&mut lead, &assign, "agent:pm").await;

    let ticks = lead.on_tick(10_000_000).await;

    assert!(ticks.is_empty());
    assert_eq!(lead.state_of("t-pm"), Some(TaskState::Escalated));
    assert_eq!(lead.state_of("t-design"), Some(TaskState::Pending));
    assert!(!lead.is_done(), "stranded by design at the default timeout");
}

/// ④ (normal + error/adverse path): `with_prior_states` — a dep outside
/// this sprint that was `Escalated` in the prior sprint blocks the new
/// task immediately at dispatch time with no envelope; an `Accepted` prior
/// dep dispatches normally.
#[tokio::test]
async fn with_prior_states_blocks_on_escalated_dep_and_proceeds_on_accepted_dep() {
    let roles = vec![(Role::Pm, "agent:pm".to_string())];

    // Error/adverse path: prior dep never resolved.
    let dag = TaskDag {
        tasks: vec![bare_task("t-new", Role::Pm, &["t-legacy"])],
    };
    let mut prior = HashMap::new();
    prior.insert("t-legacy".to_string(), TaskState::Escalated);
    let mut blocked_lead = LeadBehavior::new(
        "agent:lead",
        dag,
        vec!["t-new".to_string()],
        roles.clone(),
        AcceptanceLoop::default_budget(),
    )
    .with_prior_states(prior);

    let envelopes = blocked_lead.on_start().await;
    assert!(
        envelopes.is_empty(),
        "cascade is a silent state change, not a message"
    );
    assert_eq!(blocked_lead.state_of("t-new"), Some(TaskState::Blocked));
    assert!(blocked_lead.is_done());

    // Normal path: prior dep was Accepted.
    let dag = TaskDag {
        tasks: vec![bare_task("t-new", Role::Pm, &["t-legacy"])],
    };
    let mut prior = HashMap::new();
    prior.insert("t-legacy".to_string(), TaskState::Accepted);
    let mut ready_lead = LeadBehavior::new(
        "agent:lead",
        dag,
        vec!["t-new".to_string()],
        roles,
        AcceptanceLoop::default_budget(),
    )
    .with_prior_states(prior);

    let envelopes = ready_lead.on_start().await;
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].kind, MessageKind::TaskAssign);
    assert_eq!(ready_lead.state_of("t-new"), Some(TaskState::Assigned));
}

/// ⑤ boundary: cascading from one escalated task must not touch a sibling
/// task in the same sprint that shares no dependency edge with it.
#[tokio::test]
async fn cascade_does_not_touch_an_unrelated_dependency_free_task() {
    let dag = TaskDag {
        tasks: vec![
            bare_task("t-a", Role::Pm, &[]),
            bare_task("t-b", Role::Designer, &[]),
            bare_task("t-c", Role::Publisher, &["t-a"]),
        ],
    };
    let sprint = vec!["t-a".to_string(), "t-b".to_string(), "t-c".to_string()];
    let roles = vec![
        (Role::Pm, "agent:pm".to_string()),
        (Role::Designer, "agent:designer".to_string()),
        (Role::Publisher, "agent:publisher".to_string()),
    ];
    let mut lead =
        LeadBehavior::new("agent:lead", dag, sprint, roles, 0).escalation_timeout_ms(1_000);

    let start = lead.on_start().await;
    assert_eq!(start.len(), 2, "t-a and t-b are both dep-free roots");
    let assign_a = find_assign(&start, "agent:pm").clone();

    escalate_immediately(&mut lead, &assign_a, "agent:pm").await;
    let _ = lead.on_tick(1_000).await;
    let _ = lead.on_tick(2_000).await;

    assert_eq!(lead.state_of("t-a"), Some(TaskState::Escalated));
    assert_eq!(
        lead.state_of("t-c"),
        Some(TaskState::Blocked),
        "t-c depends on t-a"
    );
    assert_eq!(
        lead.state_of("t-b"),
        Some(TaskState::Assigned),
        "t-b shares no dependency edge with t-a and must be untouched"
    );
    assert!(
        !lead.is_done(),
        "t-b is still in flight, unrelated to the cascade"
    );
}
