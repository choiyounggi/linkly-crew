//! M10 §4-1 wiring proof: `RunConfig.dev_cmd_checks` reaches the DAG
//! `LeadPlanner` actually builds inside `RunController::start`
//! (contracts-m10.md §H1g/H1h.6).
//!
//! Safety note (load-bearing decision, recorded in decisions.md): the
//! `expect` value used below is deliberately **not** the `"exit <N>"` form.
//! `crew-run`'s `RunController::start` always wires `LeadBehavior::with_cmd_exec`
//! (`controller.rs:335`), so any injected `DodCheck::Cmd` whose `expect`
//! parses is executed for real by `cmd_exec::execute_cmd_checks` via a real
//! `Command::spawn`. This crate's worker cwd (`role_cli_cwd`) nests under
//! `data_dir`, which nests under this crate's own directory inside the repo
//! — so a real `"cargo test"`/`"npm test"` there would have cargo walk up
//! and find *this crate's own* `Cargo.toml`, spawning a real nested `cargo
//! test` of `crew-run` from inside a `crew-run` test binary. That is a
//! recursive-build hazard (target-directory lock contention against the
//! outer `cargo test` process already running this suite, at best adding
//! ~120s of `CmdPolicy`'s timeout, at worst compounding since the nested
//! invocation would itself re-run this very test file). `parse_expect`
//! documents that an unparseable `expect` means "not executed at all"
//! (`cmd_exec.rs::parse_expect`, `execute_cmd_checks` line "if
//! parse_expect(expect).is_none() { continue; }") — so a sentinel `expect`
//! proves the wiring (the check reaches the real DAG built by the real
//! `RunController::start`) while guaranteeing zero subprocess execution.
//! `crates/crew-lead/src/cmd_exec.rs` itself is out of this task's scope
//! (owned by the parallel t-pgroup task) — this test only relies on its
//! already-shipped M9 `parse_expect` skip behavior, never modifies it.

use std::path::PathBuf;
use std::time::Duration;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::{DodCheck, Role, Roster, RosterAgent};
use crew_run::{CmdCheck, RunConfig, RunController, RunError, RunEvent, RunMode, RunOutcomeDto};

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

fn config(data_dir: PathBuf, roster: Option<Roster>, dev_cmd_checks: Vec<CmdCheck>) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations: vec![] },
        data_dir,
        max_rework: AcceptanceLoop::default_budget(),
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster,
        dev_cmd_checks,
    }
}

async fn start_ok(cfg: RunConfig) -> crew_run::RunHandle {
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

fn spec_ready_dag(events: &[RunEvent]) -> crew_proto::TaskDag {
    events
        .iter()
        .find_map(|ev| match ev {
            RunEvent::SpecReady { dag, .. } => Some(dag.clone()),
            _ => None,
        })
        .expect("spec_ready must have been observed")
}

/// Normal: a real `RunConfig.dev_cmd_checks` value reaches the Developer
/// task's `dod` in the DAG the real `RunController::start` → `LeadPlanner`
/// actually built — observed via the real `SpecReady` event of a full,
/// completed run (default 5-role roster, `roster: None`). `expect` is a
/// non-`"exit <N>"` sentinel (see module doc) so `cmd_exec` skips real
/// execution entirely; the run still completes normally, proving an
/// unexecuted Cmd check does not block acceptance (M9 skip semantics,
/// unaffected by this task).
#[tokio::test(flavor = "multi_thread")]
async fn dev_cmd_checks_from_run_config_reach_the_developer_task_in_the_real_dag() {
    let data_dir = test_data_dir("cmd-dod-wiring");
    let injected = vec![CmdCheck {
        run: "cargo test".to_string(),
        expect: "m10-cmd-dod-wiring-sentinel".to_string(),
    }];
    let mut handle = start_ok(config(data_dir.clone(), None, injected.clone())).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);
    let dag = spec_ready_dag(&events);

    let dev_task = dag
        .tasks
        .iter()
        .find(|t| t.role == Role::Developer)
        .expect("developer task must be present in the default 5-role roster");
    assert_eq!(
        dev_task.dod.last(),
        Some(&DodCheck::Cmd {
            run: injected[0].run.clone(),
            expect: injected[0].expect.clone(),
        }),
        "the injected cmd check must be the DAG's own Developer task dod, appended after ReqCover"
    );

    let outcome = events.iter().find_map(|ev| match ev {
        RunEvent::RunFinished { outcome, .. } => Some(*outcome),
        _ => None,
    });
    assert_eq!(
        outcome,
        Some(RunOutcomeDto::Completed),
        "an unparseable-expect cmd check must not block the run from completing (M9 skip semantics)"
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Boundary: an empty `dev_cmd_checks` (the shipped default) leaves the
/// Developer task's `dod` exactly `[ReqCover]`, matching pre-M10 behavior.
#[tokio::test(flavor = "multi_thread")]
async fn empty_dev_cmd_checks_leaves_developer_dod_unchanged() {
    let data_dir = test_data_dir("cmd-dod-empty");
    let mut handle = start_ok(config(data_dir.clone(), None, vec![])).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);
    let dag = spec_ready_dag(&events);

    let dev_task = dag
        .tasks
        .iter()
        .find(|t| t.role == Role::Developer)
        .expect("developer task must be present in the default 5-role roster");
    assert!(
        !dev_task.dod.iter().any(|c| matches!(c, DodCheck::Cmd { .. })),
        "an empty dev_cmd_checks must attach no Cmd check: {:?}",
        dev_task.dod
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Error: an invalid (crew-zero) roster is rejected by `validate_roster`
/// before any DAG/spec work — a non-empty `dev_cmd_checks` does not bypass
/// or short-circuit that existing check (contracts-m7.md §E4 unchanged).
#[tokio::test(flavor = "multi_thread")]
async fn invalid_roster_is_rejected_even_with_dev_cmd_checks_set() {
    let roster = Roster {
        agents: vec![RosterAgent {
            id: "agent:lead".to_string(),
            role: "lead".to_string(),
            harness: "claude-code".to_string(),
            model: "default".to_string(),
            instructions: String::new(),
        }],
    };
    let injected = vec![CmdCheck {
        run: "cargo test".to_string(),
        expect: "exit 0".to_string(),
    }];
    let cfg = config(test_data_dir("cmd-dod-invalid-roster"), Some(roster), injected);

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::RosterInvalid(msg)) => {
            assert!(!msg.is_empty(), "message must state the crew-zero reason");
        }
        Err(other) => panic!("expected RosterInvalid for a crew-less roster, got a different RunError: {other}"),
        Ok(_) => panic!("a crew-less roster must not start a run even with dev_cmd_checks set"),
    }
}
