//! Vary-roster crew-run tests (contracts-m7.md §E4, t-rosterrun plan D7):
//! a non-default roster's crew subset drives spawn/routing/DAG, an invalid
//! roster is rejected before any spawn, and `cfg.roster = None`'s unchanged
//! behavior is covered separately (unmodified) by `run_controller.rs`.

use std::path::PathBuf;
use std::time::Duration;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::{Roster, RosterAgent};
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

fn slot(id: &str, role: &str) -> RosterAgent {
    RosterAgent {
        id: id.to_string(),
        role: role.to_string(),
        harness: "claude-code".to_string(),
        model: "default".to_string(),
        instructions: String::new(),
    }
}

/// lead + developer + qa (brief/plan's canonical 3-person team).
fn three_person_roster() -> Roster {
    Roster {
        agents: vec![
            slot("agent:lead", "lead"),
            slot("agent:developer", "developer"),
            slot("agent:qa", "qa"),
        ],
    }
}

fn scripted_config(data_dir: PathBuf, roster: Option<Roster>) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations: vec![] },
        data_dir,
        max_rework: AcceptanceLoop::default_budget(),
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster,
    }
}

async fn start_ok(cfg: RunConfig) -> crew_run::RunHandle {
    tokio::time::timeout(START_TIMEOUT, RunController::start(cfg))
        .await
        .expect("start must finish within the deterministic budget")
        .expect("start must succeed for a well-formed roster")
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

/// Deterministic ①: a 3-person team (lead+developer+qa) completes a full
/// run — exactly the two crew workers (plus lead) register on the bus, the
/// DAG is the developer->qa dep chain (`plan_dag_for`, contracts-m7.md
/// §E1/§E4), both tasks' `TaskStateChanged` is observed, and the run
/// finishes `Completed` with both tasks `Accepted`.
#[tokio::test(flavor = "multi_thread")]
async fn three_person_team_run_completes_with_two_workers_and_a_two_task_dep_chain() {
    let data_dir = test_data_dir("roster-3person");
    let mut handle = start_ok(scripted_config(data_dir.clone(), Some(three_person_roster()))).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    let (dag, sprint) = events
        .iter()
        .find_map(|ev| match ev {
            RunEvent::SpecReady { dag, sprint, .. } => Some((dag.clone(), sprint.clone())),
            _ => None,
        })
        .expect("spec_ready must have been observed");

    let task_ids: Vec<String> = dag.tasks.iter().map(|t| t.id.clone()).collect();
    assert_eq!(
        task_ids,
        vec!["t-dev".to_string(), "t-qa".to_string()],
        "dag must contain exactly the developer/qa chain, in canonical order"
    );
    assert_eq!(sprint, task_ids, "single default sprint covers both tasks");

    let dev_task = dag.tasks.iter().find(|t| t.id == "t-dev").expect("t-dev present");
    assert!(dev_task.deps.is_empty(), "developer is the chain's root");
    let qa_task = dag.tasks.iter().find(|t| t.id == "t-qa").expect("t-qa present");
    assert_eq!(qa_task.deps, vec!["t-dev".to_string()], "qa must depend on dev (deps chain)");

    let registered: std::collections::HashSet<String> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::BusLifecycle { kind, payload, .. } if kind == "Registered" => payload
                .get("Registered")
                .and_then(|v| v.get("agent_id"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(
        registered,
        ["agent:lead", "agent:developer", "agent:qa", "agent:human"]
            .into_iter()
            .map(String::from)
            .collect(),
        // contracts-m7.md §E5 (t-gate-run): the controller now also connects
        // a run-resident `agent:human` proxy at start, so it registers on
        // the bus alongside the lead/crew — this assertion's set is updated
        // to match that new (not weakened) behavior, same as it would be
        // for any RejectedUnknownRecipient assertion.
        "exactly the lead, the two crew workers, and the agent:human proxy must register on the bus (no pm/designer/publisher): {registered:?}"
    );

    let changed_task_ids: std::collections::HashSet<String> = events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::TaskStateChanged { task_id, .. } => Some(task_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        changed_task_ids,
        ["t-dev", "t-qa"].into_iter().map(String::from).collect(),
        "TaskStateChanged must be observed for exactly the two roster tasks: {changed_task_ids:?}"
    );

    assert!(
        matches!(
            events.last(),
            Some(RunEvent::RunFinished { outcome: RunOutcomeDto::Completed, .. })
        ),
        "the last event must be run_finished(completed), got {:?}",
        events.last()
    );

    let snap = handle.snapshot();
    assert_eq!(snap.sprint.len(), 2);
    assert!(
        snap.task_states.iter().all(|(_, state)| *state == TaskStateDto::Accepted),
        "every sprint task must end accepted: {:?}",
        snap.task_states
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Deterministic ②a: a duplicate valid role is rejected before any spawn.
#[tokio::test(flavor = "multi_thread")]
async fn duplicate_role_roster_is_rejected_immediately() {
    let roster = Roster {
        agents: vec![
            slot("agent:lead", "lead"),
            slot("agent:dev-1", "developer"),
            slot("agent:dev-2", "developer"),
        ],
    };
    let cfg = scripted_config(test_data_dir("roster-dup"), Some(roster));

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::RosterInvalid(msg)) => {
            assert!(msg.contains("developer"), "message must name the duplicated role: {msg}");
        }
        Err(other) => panic!("expected RosterInvalid for a duplicated role, got a different RunError: {other}"),
        Ok(_) => panic!("a duplicated role must not start a run"),
    }
}

/// Deterministic ②b: a lead-only roster (crew count 0) is rejected before
/// any spawn.
#[tokio::test(flavor = "multi_thread")]
async fn crew_zero_roster_is_rejected_immediately() {
    let roster = Roster {
        agents: vec![slot("agent:lead", "lead")],
    };
    let cfg = scripted_config(test_data_dir("roster-zero"), Some(roster));

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::RosterInvalid(msg)) => {
            assert!(!msg.is_empty(), "message must state the crew-zero reason");
        }
        Err(other) => panic!("expected RosterInvalid for a crew-less roster, got a different RunError: {other}"),
        Ok(_) => panic!("a crew-less roster must not start a run"),
    }
}

/// Deterministic ②c: an unrecognized role string is rejected before any
/// spawn.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_role_roster_is_rejected_immediately() {
    let roster = Roster {
        agents: vec![slot("agent:lead", "lead"), slot("agent:mage", "wizard")],
    };
    let cfg = scripted_config(test_data_dir("roster-unknown"), Some(roster));

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::RosterInvalid(msg)) => {
            assert!(msg.contains("wizard"), "message must name the unknown role value: {msg}");
        }
        Err(other) => panic!("expected RosterInvalid for an unknown role, got a different RunError: {other}"),
        Ok(_) => panic!("an unrecognized role must not start a run"),
    }
}

// --- real CLI spot check (contracts-m7.md §E4 / pitfall 19 — add only,
// coordinator runs manually, never here) --------------------------------

/// t-rosterrun plan D7④: a 3-person team (lead+developer+qa) `RealCli` run
/// completes one sprint — the vary-roster equivalent of `m5_swap.rs`'s
/// `real_cli_mid_sprint_designer_swap_completes_two_sprints`, same
/// generous timeouts (real CLI turns measured 17-193s, m5a E2E).
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn real_cli_three_person_team_completes_one_sprint() {
    let data_dir = test_data_dir("roster-3person-real-cli");
    let cfg = RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::RealCli,
        data_dir: data_dir.clone(),
        max_rework: AcceptanceLoop::default_budget(),
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster: Some(three_person_roster()),
    };

    let real_start_timeout = Duration::from_secs(300);
    let real_join_timeout = Duration::from_secs(1800);

    let mut handle = tokio::time::timeout(real_start_timeout, RunController::start(cfg))
        .await
        .expect("real-CLI start must finish within the budget")
        .expect("real-CLI start must succeed for a well-formed 3-person roster");

    tokio::time::timeout(real_join_timeout, handle.join())
        .await
        .expect("real-CLI run must finish within the budget")
        .expect("the 3-person real-CLI run must complete its single sprint");

    let snap = handle.snapshot();
    assert_eq!(snap.sprint.len(), 2, "the 3-person roster's DAG must have exactly the dev/qa tasks");

    handle.shutdown().await;
    cleanup(&data_dir);
}
