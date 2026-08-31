//! Deterministic `crew-run` end-to-end tests (brief DoD ①②③): scripted
//! 5-role sprint through `RunController`/`RunHandle`, contract §C3.
//!
//! Every `subscribe()` call happens on a handle whose broadcast channel's
//! "genesis" receiver (created alongside the sender, before any `RunEvent`
//! is sent) is handed out first — so the collected event history always
//! starts at `RunStarted`, regardless of how far the run has already
//! progressed by the time the test subscribes (see `controller.rs`'s
//! `RunHandle::subscribe` doc comment).

use std::path::PathBuf;
use std::time::Duration;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::{MessageKind, Role};
use crew_run::{RunConfig, RunController, RunError, RunEvent, RunMode, RunOutcomeDto, TaskStateDto};

const GOAL: &str = "간단한 랜딩 페이지";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);

fn test_data_dir(label: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap()
        .join(".crew-test")
        .join(format!("{label}-{}", uuid::Uuid::new_v4()))
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

fn scripted_config(data_dir: PathBuf, planted_violations: Vec<(Role, Vec<String>)>) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations },
        data_dir,
        max_rework: AcceptanceLoop::default_budget(),
        // contracts-m5.md §C5a defaults: single sprint, escalation cascade
        // off, default roster — preserves this file's pre-M5 behavior.
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster: None,
        dev_cmd_checks: Vec::new(),
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

/// Drains every event currently buffered on `rx` without blocking — safe
/// to call once the run has already finished (`join()` returned), since no
/// further sends can race with the drain.
fn drain(rx: &mut tokio::sync::broadcast::Receiver<RunEvent>) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

fn event_type_name(ev: &RunEvent) -> &'static str {
    match ev {
        RunEvent::RunStarted { .. } => "run_started",
        RunEvent::SpecReady { .. } => "spec_ready",
        RunEvent::Message { .. } => "message",
        RunEvent::TaskStateChanged { .. } => "task_state_changed",
        RunEvent::BusLifecycle { .. } => "bus_lifecycle",
        RunEvent::RunFinished { .. } => "run_finished",
        RunEvent::SprintStarted { .. } => "sprint_started",
        RunEvent::SprintFinished { .. } => "sprint_finished",
        RunEvent::RosterChanged { .. } => "roster_changed",
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn happy_path_five_roles_accepted_run_started_to_run_finished_in_order() {
    let data_dir = test_data_dir("happy");
    let mut handle = start(scripted_config(data_dir.clone(), vec![])).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    assert!(!events.is_empty(), "the run must have emitted events");
    assert_eq!(
        event_type_name(&events[0]),
        "run_started",
        "the first observed event must be run_started"
    );
    assert_eq!(
        event_type_name(&events[1]),
        "spec_ready",
        "spec_ready must immediately follow run_started"
    );
    assert!(
        matches!(
            events.last(),
            Some(RunEvent::RunFinished {
                outcome: RunOutcomeDto::Completed,
                ..
            })
        ),
        "the last event must be run_finished(completed), got {:?}",
        events.last()
    );

    let message_seqs: Vec<i64> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Message { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert!(!message_seqs.is_empty(), "at least one Message must have been observed");
    assert!(
        message_seqs.windows(2).all(|w| w[0] < w[1]),
        "Message.seq must be strictly increasing: {message_seqs:?}"
    );

    let change_requests = events
        .iter()
        .filter(|ev| matches!(ev, RunEvent::Message { envelope, .. } if envelope.kind == MessageKind::ChangeRequest))
        .count();
    assert_eq!(change_requests, 0, "happy path must not need any rework");

    let snap = handle.snapshot();
    assert_eq!(snap.sprint.len(), 5);
    assert_eq!(snap.task_states.len(), 5);
    assert!(
        snap.task_states.iter().all(|(_, state)| *state == TaskStateDto::Accepted),
        "every sprint task must end accepted: {:?}",
        snap.task_states
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn single_rework_round_then_accepted() {
    let data_dir = test_data_dir("rework");
    let planted = vec![(Role::Designer, vec!["REQ-2".to_string()])];
    let mut handle = start(scripted_config(data_dir.clone(), planted)).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    let change_request_recipients: Vec<Vec<String>> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Message { envelope, .. } if envelope.kind == MessageKind::ChangeRequest => {
                Some(envelope.to.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        change_request_recipients.len(),
        1,
        "exactly one planted violation must trigger exactly one rework round"
    );
    assert_eq!(change_request_recipients[0], vec!["agent:designer".to_string()]);

    assert!(matches!(
        events.last(),
        Some(RunEvent::RunFinished {
            outcome: RunOutcomeDto::Completed,
            ..
        })
    ));

    let snap = handle.snapshot();
    assert!(snap.task_states.iter().all(|(_, state)| *state == TaskStateDto::Accepted));

    handle.shutdown().await;
    cleanup(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_goal_propagates_spec_failed_run_error() {
    let cfg = scripted_config(test_data_dir("empty-goal"), vec![]);
    let cfg = RunConfig {
        goal: "   ".to_string(),
        ..cfg
    };

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::SpecFailed(_)) => {}
        Err(other) => panic!("expected RunError::SpecFailed, got a different RunError: {other}"),
        Ok(_) => panic!("an empty/whitespace-only goal must not succeed"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn join_is_idempotent_and_shutdown_after_completion_does_not_panic() {
    let data_dir = test_data_dir("join-shutdown");
    let mut handle = start(scripted_config(data_dir.clone(), vec![])).await;

    join_ok(&mut handle).await;
    let second_join = handle.join().await;
    assert!(second_join.is_ok(), "a second join() after completion must not panic and must return the cached outcome");

    handle.shutdown().await;
    cleanup(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_mid_flight_without_joining_does_not_panic() {
    let data_dir = test_data_dir("mid-shutdown");
    let handle = start(scripted_config(data_dir.clone(), vec![])).await;

    handle.shutdown().await;
    cleanup(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_matches_c3_shape_with_last_seq_and_messages() {
    let data_dir = test_data_dir("snapshot");
    let mut handle = start(scripted_config(data_dir.clone(), vec![])).await;

    join_ok(&mut handle).await;
    let snap = handle.snapshot();

    assert_eq!(snap.run_id, handle.run_id());
    assert_eq!(snap.goal, GOAL);
    assert!(snap.spec.is_some());
    assert!(snap.dag.is_some());
    assert_eq!(snap.sprint.len(), 5);
    assert!(snap.last_seq > 0, "last_seq must advance past its zero boundary");
    assert!(!snap.messages.is_empty());
    assert_eq!(
        snap.messages.iter().map(|m| m.seq).max().unwrap(),
        snap.last_seq,
        "last_seq must match the highest messages-table seq (contract §C3 seq-space rule)"
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Normal case (contracts-m7.md §E7, t-bridge3 plan D1): `RunHandle::
/// search_messages` delegates to the run's own ledger against a real
/// completed run. Every scripted `task.result` carries at least one
/// artifact whose `content` is built as `"[{role:?}] {name} — covers
/// {req_ids}"` (`crew_agent::ScriptedCrewMember::build_artifacts`), so the
/// literal "covers" is guaranteed present without depending on the goal
/// text or requirement ids.
#[tokio::test(flavor = "multi_thread")]
async fn search_messages_matches_a_real_runs_task_result() {
    let data_dir = test_data_dir("search");
    let mut handle = start(scripted_config(data_dir.clone(), vec![])).await;
    join_ok(&mut handle).await;

    let results = handle
        .search_messages("covers", 50)
        .expect("search_messages must delegate to the ledger without erroring");
    assert!(
        !results.is_empty(),
        "a completed scripted run's task.result artifacts must contain \"covers\""
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}
