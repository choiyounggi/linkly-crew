//! Deterministic `RunHandle::resolve_gate` tests (contracts-m7.md §E5,
//! t-gate-run plan D6): Scripted + planted violations drive a task to
//! `Escalated` (budget exhaustion), then `resolve_gate` approve/reject is
//! exercised against the run-resident `agent:human` proxy — plus the
//! post-shutdown `Err(GateUnavailable)` and unknown-`task_id` boundary
//! cases.

use std::path::PathBuf;
use std::time::Duration;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::Role;
use crew_run::{GateDecision, RunConfig, RunController, RunError, RunEvent, RunMode, TaskStateDto};

const GOAL: &str = "간단한 랜딩 페이지";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

fn test_data_dir(label: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap()
        .join(".crew-test")
        .join(format!("{label}-{}", uuid::Uuid::new_v4()))
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

/// max_rework=0 + a planted Designer violation exhausts the rework budget on
/// its first attempt, escalating `t-design` immediately (mirrors
/// m5_multisprint's cascade fixture). `max_per_sprint=0` keeps every task in
/// one sprint so the escalation and the `resolve_gate` call both land while
/// the same sprint (and its bus connections) are still live.
fn config(data_dir: PathBuf) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted {
            planted_violations: vec![(Role::Designer, vec!["REQ-2".to_string()])],
        },
        data_dir,
        max_rework: 0,
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster: None,
    }
}

async fn start(cfg: RunConfig) -> crew_run::RunHandle {
    tokio::time::timeout(START_TIMEOUT, RunController::start(cfg))
        .await
        .expect("start must finish within the deterministic budget")
        .expect("start must succeed for a well-formed config")
}

async fn join_ok(handle: &mut crew_run::RunHandle) {
    tokio::time::timeout(JOIN_TIMEOUT, handle.join())
        .await
        .expect("join must finish within the deterministic budget")
        .expect("lead runner must complete cleanly");
}

/// Waits (bounded) for the first event matching `pred`, buffering everything
/// observed along the way (including the match) so no event is dropped for
/// later inspection (mirrors m5_swap.rs's `wait_for`).
async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<RunEvent>,
    buf: &mut Vec<RunEvent>,
    pred: impl Fn(&RunEvent) -> bool,
) {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let ev = rx.recv().await.expect("broadcast receiver must not lag/close before the match");
            let matched = pred(&ev);
            buf.push(ev);
            if matched {
                return;
            }
        }
    })
    .await
    .expect("expected event did not arrive within the deterministic budget")
}

fn is_task_state(ev: &RunEvent, task_id: &str, state: TaskStateDto) -> bool {
    matches!(ev, RunEvent::TaskStateChanged { task_id: t, state: s, .. } if t == task_id && *s == state)
}

/// Normal (approve): once `t-design` escalates, `resolve_gate(.., Approve,
/// ..)` must drive it back through `Assigned` and on to `Accepted`
/// (`LeadBehavior::approve_escalation` resets the budget and re-dispatches),
/// and the run must still complete normally afterward.
#[tokio::test(flavor = "multi_thread")]
async fn approve_reassigns_the_escalated_task_and_the_run_completes() {
    let data_dir = test_data_dir("gate-approve");
    let mut handle = start(config(data_dir.clone())).await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-design", TaskStateDto::Escalated)).await;

    handle
        .resolve_gate("t-design", GateDecision::Approve, "looks fine on review")
        .await
        .expect("resolve_gate must succeed while the run is live");

    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-design", TaskStateDto::Assigned)).await;
    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-design", TaskStateDto::Accepted)).await;

    join_ok(&mut handle).await;
    let snap = handle.snapshot();
    let state_of = |id: &str| {
        snap.task_states
            .iter()
            .find(|(task_id, _)| task_id == id)
            .map(|(_, state)| *state)
            .unwrap_or_else(|| panic!("task {id} must be present in the final snapshot"))
    };
    assert_eq!(state_of("t-design"), TaskStateDto::Accepted, "approve must let the run finish the task");
    assert_eq!(state_of("t-publish"), TaskStateDto::Accepted, "downstream tasks must proceed once t-design accepts");

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Reject: `resolve_gate(.., Reject, ..)` must Block the escalated task and
/// cascade Blocked to its transitive dependents (mirrors
/// m5_multisprint.rs's cascade assertions), without ever assigning it.
#[tokio::test(flavor = "multi_thread")]
async fn reject_blocks_the_escalated_task_and_cascades_to_dependents() {
    let data_dir = test_data_dir("gate-reject");
    let mut handle = start(config(data_dir.clone())).await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-design", TaskStateDto::Escalated)).await;

    handle
        .resolve_gate("t-design", GateDecision::Reject, "does not meet requirements")
        .await
        .expect("resolve_gate must succeed while the run is live");

    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-design", TaskStateDto::Blocked)).await;

    join_ok(&mut handle).await;
    let snap = handle.snapshot();
    let state_of = |id: &str| {
        snap.task_states
            .iter()
            .find(|(task_id, _)| task_id == id)
            .map(|(_, state)| *state)
            .unwrap_or_else(|| panic!("task {id} must be present in the final snapshot"))
    };
    assert_eq!(state_of("t-design"), TaskStateDto::Blocked);
    assert_eq!(
        state_of("t-publish"),
        TaskStateDto::Blocked,
        "t-publish depends on t-design and must cascade to Blocked"
    );
    assert_eq!(
        state_of("t-dev"),
        TaskStateDto::Blocked,
        "t-dev transitively depends on t-design and must cascade to Blocked"
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// `resolve_gate` after the run has already ended must return
/// `Err(GateUnavailable)`, not hang or panic. `shutdown(self)` consumes the
/// handle (no further calls are even expressible against it), so the only
/// way to observe "run ended" against a still-owned handle is the natural
/// end-of-run path: `join()` takes `&mut self` and, by the time it resolves,
/// the controller has already torn down the human proxy alongside the run
/// (plan D1's "런 종료(join/shutdown) 경로에서 abort").
#[tokio::test(flavor = "multi_thread")]
async fn resolve_gate_after_the_run_has_finished_is_gate_unavailable() {
    let data_dir = test_data_dir("gate-after-join");
    let mut handle = start(RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations: vec![] },
        data_dir: data_dir.clone(),
        max_rework: AcceptanceLoop::default_budget(),
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster: None,
    })
    .await;

    join_ok(&mut handle).await;

    let result = tokio::time::timeout(
        EVENT_TIMEOUT,
        handle.resolve_gate("t-pm", GateDecision::Approve, "too late"),
    )
    .await
    .expect("resolve_gate must not hang once the run has ended");

    assert!(
        matches!(result, Err(RunError::GateUnavailable(_))),
        "resolve_gate after the run ended must be Err(GateUnavailable), got {result:?}"
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Boundary: approving a `task_id` the DAG has never heard of must not
/// panic anything — `LeadBehavior::handle_human_response` silently ignores
/// it (contract §E3, no task is `Escalated` under that id), and the run
/// must keep running/complete normally.
#[tokio::test(flavor = "multi_thread")]
async fn approve_of_unknown_task_id_is_ignored_without_panicking() {
    let data_dir = test_data_dir("gate-unknown-task");
    let mut handle = start(config(data_dir.clone())).await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-design", TaskStateDto::Escalated)).await;

    handle
        .resolve_gate("t-does-not-exist", GateDecision::Approve, "n/a")
        .await
        .expect("resolve_gate must still succeed at the bus-publish level for an unknown task_id");

    // Real gate the run is actually waiting on, so it can still finish.
    handle
        .resolve_gate("t-design", GateDecision::Approve, "correcting after the no-op above")
        .await
        .expect("resolve_gate must succeed while the run is live");

    join_ok(&mut handle).await;
    handle.shutdown().await;
    cleanup(&data_dir);
}
