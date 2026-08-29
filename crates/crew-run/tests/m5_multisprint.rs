//! Deterministic multi-sprint `crew-run` tests (brief DoD ④⑤⑥, plan D9):
//! the sequential sprint loop, boundary compression, and cross-sprint
//! `prior_states` cascade, contracts-m5.md §C5a.

use std::path::PathBuf;
use std::time::Duration;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::Role;
use crew_run::{RunConfig, RunController, RunEvent, RunMode, RunOutcomeDto, TaskStateDto};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const GOAL: &str = "간단한 랜딩 페이지";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);
const L1_BUDGET_CHARS: usize = 16_000;

fn test_data_dir(label: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap()
        .join(".crew-test")
        .join(format!("{label}-{}", uuid::Uuid::new_v4()))
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

fn config(
    data_dir: PathBuf,
    planted_violations: Vec<(Role, Vec<String>)>,
    max_per_sprint: usize,
    max_rework: u32,
) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations },
        data_dir,
        max_rework,
        max_per_sprint,
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

fn drain(rx: &mut tokio::sync::broadcast::Receiver<RunEvent>) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

fn parse_ts(ts: &str) -> OffsetDateTime {
    OffsetDateTime::parse(ts, &Rfc3339).unwrap_or_else(|e| panic!("event ts {ts:?} must be RFC3339: {e}"))
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

fn event_ts(ev: &RunEvent) -> &str {
    match ev {
        RunEvent::RunStarted { ts, .. }
        | RunEvent::SpecReady { ts, .. }
        | RunEvent::TaskStateChanged { ts, .. }
        | RunEvent::RunFinished { ts, .. }
        | RunEvent::SprintStarted { ts, .. }
        | RunEvent::SprintFinished { ts, .. }
        | RunEvent::RosterChanged { ts, .. } => ts,
        RunEvent::Message { envelope, .. } => &envelope.ts,
        RunEvent::BusLifecycle { .. } => {
            // `BusLifecycle` carries no `ts` field (contract §C3) — excluded
            // from the monotonic-ts assertion by the caller instead.
            unreachable!("event_ts must not be called on BusLifecycle")
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn three_sprints_happy_path_all_accepted() {
    let data_dir = test_data_dir("multisprint-happy");
    let mut handle = start(config(data_dir.clone(), vec![], 2, AcceptanceLoop::default_budget())).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    let sprint_started: Vec<(u32, Vec<String>)> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::SprintStarted { index, task_ids, .. } => Some((*index, task_ids.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        sprint_started,
        vec![
            (0, vec!["t-pm".to_string(), "t-design".to_string()]),
            (1, vec!["t-publish".to_string(), "t-dev".to_string()]),
            (2, vec!["t-qa".to_string()]),
        ],
        "5 tasks / max_per_sprint=2 must slice into exactly 3 sprints, in order"
    );

    let sprint_finished: Vec<(u32, String)> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::SprintFinished { index, summary, .. } => Some((*index, summary.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(sprint_finished.len(), 3, "SprintFinished must fire once per sprint");
    for (i, (index, summary)) in sprint_finished.iter().enumerate() {
        assert_eq!(*index, i as u32, "SprintFinished indices must be in order");
        assert!(!summary.is_empty(), "sprint {index}'s summary must not be empty");
        assert!(
            summary.chars().count() <= L1_BUDGET_CHARS,
            "sprint {index}'s summary must respect the L1 budget: {} chars",
            summary.chars().count()
        );
    }

    let roster_changed_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, ev)| matches!(ev, RunEvent::RosterChanged { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(roster_changed_positions.len(), 1, "RosterChanged must fire exactly once");
    let first_sprint_started_pos = events
        .iter()
        .position(|ev| matches!(ev, RunEvent::SprintStarted { index: 0, .. }))
        .expect("SprintStarted(0) must be observed");
    assert!(
        roster_changed_positions[0] < first_sprint_started_pos,
        "RosterChanged must precede the first SprintStarted"
    );

    assert!(matches!(
        events.last(),
        Some(RunEvent::RunFinished {
            outcome: RunOutcomeDto::Completed,
            ..
        })
    ));

    let snap = handle.snapshot();
    assert_eq!(snap.task_states.len(), 5);
    assert!(
        snap.task_states.iter().all(|(_, state)| *state == TaskStateDto::Accepted),
        "every task across every sprint must end accepted: {:?}",
        snap.task_states
    );

    let message_seqs: Vec<i64> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Message { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert!(!message_seqs.is_empty());
    assert!(
        message_seqs.windows(2).all(|w| w[0] < w[1]),
        "Message.seq must be strictly increasing: {message_seqs:?}"
    );

    // `Message.ts` mirrors the envelope's own creation time (application
    // data, unrelated to when the controller published the `RunEvent`) —
    // its ordering guarantee is `seq` (asserted above), not `ts`, so it's
    // excluded here alongside `BusLifecycle` (which carries no `ts` at
    // all). Every controller-stamped event type (`now_ts()` at the moment
    // of `run_tx.send`, plan D8/함정 12) must still be non-decreasing.
    let timestamped: Vec<(&'static str, OffsetDateTime)> = events
        .iter()
        .filter(|ev| !matches!(ev, RunEvent::BusLifecycle { .. } | RunEvent::Message { .. }))
        .map(|ev| (event_type_name(ev), parse_ts(event_ts(ev))))
        .collect();
    for w in timestamped.windows(2) {
        assert!(
            w[0].1 <= w[1].1,
            "every controller-stamped event's ts must be monotonically non-decreasing: {:?} (ts={}) then {:?} (ts={})",
            w[0].0,
            w[0].1,
            w[1].0,
            w[1].1
        );
    }

    handle.shutdown().await;
    cleanup(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn escalation_in_an_earlier_sprint_cascades_to_blocked_in_a_later_one() {
    let data_dir = test_data_dir("multisprint-cascade");
    // max_rework=0: Designer's first (planted-violating) result exhausts
    // the rework budget immediately, escalating t-design in sprint 0
    // without ever giving it a chance to self-correct.
    let planted = vec![(Role::Designer, vec!["REQ-2".to_string()])];
    let mut handle = start(config(data_dir.clone(), planted, 2, 0)).await;

    join_ok(&mut handle).await;

    let snap = handle.snapshot();
    let state_of = |id: &str| {
        snap.task_states
            .iter()
            .find(|(task_id, _)| task_id == id)
            .map(|(_, state)| *state)
            .unwrap_or_else(|| panic!("task {id} must be present in the final snapshot"))
    };

    assert_eq!(state_of("t-pm"), TaskStateDto::Accepted, "t-pm has no dependency on the escalated task");
    assert_eq!(state_of("t-design"), TaskStateDto::Escalated, "the planted violation must escalate t-design");
    assert_eq!(
        state_of("t-publish"),
        TaskStateDto::Blocked,
        "t-publish (sprint 1) depends on the escalated t-design via prior_states"
    );
    assert_eq!(
        state_of("t-dev"),
        TaskStateDto::Blocked,
        "t-dev cascades from its blocked in-sprint dependency t-publish"
    );
    assert_eq!(
        state_of("t-qa"),
        TaskStateDto::Blocked,
        "t-qa (sprint 2) depends on the blocked t-dev via prior_states, two sprints removed"
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn max_per_sprint_above_task_count_runs_as_a_single_sprint() {
    let data_dir = test_data_dir("multisprint-oversized");
    let mut handle = start(config(data_dir.clone(), vec![], 100, AcceptanceLoop::default_budget())).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    let sprint_started: Vec<u32> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::SprintStarted { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(sprint_started, vec![0], "max_per_sprint larger than the task count must still yield one sprint");

    let sprint_finished_count = events
        .iter()
        .filter(|ev| matches!(ev, RunEvent::SprintFinished { .. }))
        .count();
    assert_eq!(sprint_finished_count, 1);

    assert!(matches!(
        events.last(),
        Some(RunEvent::RunFinished {
            outcome: RunOutcomeDto::Completed,
            ..
        })
    ));

    handle.shutdown().await;
    cleanup(&data_dir);
}
