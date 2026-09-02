//! Tauri-independent command bodies (plan D4): `lib.rs`'s `#[tauri::command]`
//! wrappers are thin adapters over these; Rust tests target this module only.

use std::path::{Path, PathBuf};

use crew_lead::accept::AcceptanceLoop;
use crew_proto::{Role, Roster, RosterAgent};
use crew_run::{validate_roster, GateDecision, RunConfig, RunController, RunEvent, RunHandle, RunMode, StoredMessageDto};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// One running crew's handle plus its event pump (plan D1).
pub struct ActiveRun {
    pub handle: RunHandle,
    pump: JoinHandle<()>,
}

/// `tauri::State`-managed app state (plan D1): at most one run at a time.
#[derive(Default)]
pub struct AppState {
    pub current: Mutex<Option<ActiveRun>>,
}

/// `scripted=true`'s `RunMode` (contracts-m4.md §C6 verbatim): a designer
/// rework round trip is planted so the demo run shows the rework loop.
fn scripted_mode() -> RunMode {
    RunMode::Scripted {
        planted_violations: vec![(Role::Designer, vec!["REQ-2".to_string()])],
    }
}

/// Starts a run and stores it in `state` (plan D1/D5/D9). Errs
/// `"run_in_progress"` if a run is already active — never replaces it.
/// `data_root` is the parent the caller creates the run's own fresh
/// `<launch-id>` directory under (contracts-m4.md §C6 2026-08-28 amendment:
/// `<launch-id>` need not equal `RunHandle::run_id()`). `roster_path` is the
/// saved roster to load and forward as `RunConfig.roster` (contracts-m5.md
/// §C6, plan D6) — a corrupted file falls back to "클로드 5인팀" rather than
/// blocking the run (`get_roster` surfaces that corruption separately).
/// `emit` receives every `RunEvent` from the pump — production wires it to
/// `AppHandle::emit`, tests inject a channel/closure (plan D4).
pub async fn start_run_core<F>(
    state: &AppState,
    goal: String,
    scripted: bool,
    data_root: &Path,
    roster_path: &Path,
    emit: F,
) -> Result<String, String>
where
    F: Fn(RunEvent) + Send + 'static,
{
    let mut guard = state.current.lock().await;
    if guard.is_some() {
        return Err("run_in_progress".to_string());
    }

    let mode = if scripted {
        scripted_mode()
    } else {
        RunMode::RealCli
    };
    let launch_id = uuid::Uuid::new_v4().to_string();
    let roster = match load_roster(roster_path) {
        Ok(roster) => roster,
        Err(err) => {
            tracing::warn!(error = %err, "roster.json unreadable; starting with the default roster");
            claude_five_team()
        }
    };
    let cfg = RunConfig {
        goal,
        mode,
        data_dir: data_root.join(launch_id),
        max_rework: AcceptanceLoop::default_budget(),
        // contracts-m5.md §C5a defaults: single sprint, escalation cascade
        // off — current bridge behavior unchanged.
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster: Some(roster),
        // contracts-m10.md §H1g / D8: cmd DoD emission is off by default.
        // To turn it on, pass crew_run::default_dev_cmd_checks_rust() etc.
        dev_cmd_checks: Vec::new(),
        // contracts-m11.md §I1/D12: no GUI project_root picker yet (out of
        // scope) — every role's CLI cwd and the Cmd DoD exec cwd stay the
        // per-role scratch dir, unchanged from pre-M11 behavior.
        project_root: None,
    };

    let handle = RunController::start(cfg).await.map_err(|e| e.to_string())?;
    let run_id = handle.run_id().to_string();
    let pump = spawn_pump(&handle, emit);

    *guard = Some(ActiveRun { handle, pump });
    Ok(run_id)
}

/// Subscribes to `handle` and forwards every `RunEvent` to `emit` until the
/// broadcast channel closes (plan D3). `Lagged` is logged and skipped, never
/// fatal; `Closed` ends the pump — this is the run's natural termination,
/// not an error.
fn spawn_pump<F>(handle: &RunHandle, emit: F) -> JoinHandle<()>
where
    F: Fn(RunEvent) + Send + 'static,
{
    let mut rx = handle.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => emit(event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "tauri bridge pump lagged; events dropped");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Stops the active run, if any, and clears `state` so a new run can start
/// (plan D1/D9's "이중 시작 방지" boundary: this is what makes the next
/// `start_run_core` call succeed again). A no-op (not an error) when no run
/// is active — `stop_run` is idempotent.
pub async fn stop_run_core(state: &AppState) -> Result<(), String> {
    let active = state.current.lock().await.take();
    if let Some(active) = active {
        active.pump.abort();
        active.handle.shutdown().await;
    }
    Ok(())
}

/// Contract §C6: `Err("no_active_run")` when nothing is running — the
/// frontend's `run_snapshot` only makes sense against a live run.
pub async fn run_snapshot_core(state: &AppState) -> Result<serde_json::Value, String> {
    let guard = state.current.lock().await;
    match guard.as_ref() {
        Some(active) => serde_json::to_value(active.handle.snapshot()).map_err(|e| e.to_string()),
        None => Err("no_active_run".to_string()),
    }
}

// --- Roster persistence, presets, detection, swap (contracts-m5.md §C6) ---

/// One built-in named roster layout (contract verbatim: `RosterPreset {
/// name, roster }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterPreset {
    pub name: String,
    pub roster: Roster,
}

fn preset_slot(id: &str, role: &str, harness: &str, model: &str) -> RosterAgent {
    RosterAgent {
        id: id.to_string(),
        role: role.to_string(),
        harness: harness.to_string(),
        model: model.to_string(),
        instructions: String::new(),
    }
}

/// The 6 fixed roster slots, contract order (lead + 5 roles) — team size is
/// fixed (contracts-m5.md §C0); only harness/model assignment varies per
/// preset.
const ROSTER_SLOTS: [(&str, &str); 6] = [
    ("agent:lead", "lead"),
    ("agent:pm", "pm"),
    ("agent:designer", "designer"),
    ("agent:publisher", "publisher"),
    ("agent:developer", "developer"),
    ("agent:qa", "qa"),
];

/// "클로드 5인팀": every slot `claude-code`/`"default"` — same shape as
/// crew-run's private `default_roster` (duplicated here since that fn isn't
/// exported and `crates/**` is out of scope for this task).
fn claude_five_team() -> Roster {
    Roster {
        agents: ROSTER_SLOTS
            .iter()
            .map(|(id, role)| preset_slot(id, role, "claude-code", "default"))
            .collect(),
    }
}

/// "절약 모드": lead/developer -> `"opus"`, everyone else `"sonnet"`, all
/// `claude-code`.
fn saving_mode() -> Roster {
    Roster {
        agents: ROSTER_SLOTS
            .iter()
            .map(|(id, role)| {
                let model = if *role == "lead" || *role == "developer" { "opus" } else { "sonnet" };
                preset_slot(id, role, "claude-code", model)
            })
            .collect(),
    }
}

/// "혼합 실험": publisher -> `"opencode"` (registry stub — swap/spawn against
/// it is expected to fail until a real adapter exists), everyone else
/// `claude-code`.
fn mixed_experiment() -> Roster {
    Roster {
        agents: ROSTER_SLOTS
            .iter()
            .map(|(id, role)| {
                let harness = if *role == "publisher" { "opencode" } else { "claude-code" };
                preset_slot(id, role, harness, "default")
            })
            .collect(),
    }
}

/// "미니 2인팀" slots (contracts-m7.md §E7, t-bridge3 plan D4): only 3 of
/// the 5 roles — lead/developer/qa — unlike the other 3 presets' fixed
/// 6-slot `ROSTER_SLOTS`.
const MINI_TEAM_SLOTS: [(&str, &str); 3] = [
    ("agent:lead", "lead"),
    ("agent:developer", "developer"),
    ("agent:qa", "qa"),
];

/// "미니 2인팀": every slot `claude-code`/`"default"` (contract verbatim).
fn mini_two_person_team() -> Roster {
    Roster {
        agents: MINI_TEAM_SLOTS
            .iter()
            .map(|(id, role)| preset_slot(id, role, "claude-code", "default"))
            .collect(),
    }
}

/// The 4 built-in presets, contract order — pure, no I/O.
fn built_in_presets() -> Vec<RosterPreset> {
    vec![
        RosterPreset { name: "클로드 5인팀".to_string(), roster: claude_five_team() },
        RosterPreset { name: "절약 모드".to_string(), roster: saving_mode() },
        RosterPreset { name: "혼합 실험".to_string(), roster: mixed_experiment() },
        RosterPreset { name: "미니 2인팀".to_string(), roster: mini_two_person_team() },
    ]
}

/// App-default roster persistence path (contracts-m5.md §C6):
/// `~/.linkly-crew/roster.json`. Never `/tmp`/`$TMPDIR`.
pub fn roster_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set on this platform");
    Path::new(&home).join(".linkly-crew").join("roster.json")
}

/// Loads the roster at `path` (path injected for testability — plan D1, so
/// tests never touch the real home directory). Missing file -> creates,
/// persists, and returns "클로드 5인팀" (contracts-m5.md §C6). Malformed
/// JSON -> `Err` string with the file left untouched (plan D3: never
/// overwrite data the user might still recover by hand).
fn load_roster(path: &Path) -> Result<Roster, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let roster = claude_five_team();
            save_roster(path, &roster)?;
            Ok(roster)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Persists `roster` as pretty JSON at `path`, creating the parent
/// directory if needed.
fn save_roster(path: &Path, roster: &Roster) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(roster).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// `get_roster` command body.
pub fn get_roster_core(path: &Path) -> Result<serde_json::Value, String> {
    let roster = load_roster(path)?;
    serde_json::to_value(roster).map_err(|e| e.to_string())
}

/// `set_roster` command body: validates before persisting (contracts-m7.md
/// §E7, t-bridge3 plan D3) — `validate_roster` rejection leaves the file
/// untouched. An already-running run's roster changes only through
/// `swap_harness_core` (plan D5) or the next `start_run` (contracts-m5.md
/// §C6).
pub fn set_roster_core(path: &Path, roster: serde_json::Value) -> Result<(), String> {
    let roster: Roster = serde_json::from_value(roster).map_err(|e| e.to_string())?;
    validate_roster(&roster)?;
    save_roster(path, &roster)
}

/// `list_presets` command body.
pub fn list_presets_core() -> Result<serde_json::Value, String> {
    serde_json::to_value(built_in_presets()).map_err(|e| e.to_string())
}

/// `detect_harnesses` command body.
pub fn detect_harnesses_core() -> Result<serde_json::Value, String> {
    serde_json::to_value(crew_harness::HarnessRegistry::detect()).map_err(|e| e.to_string())
}

/// `swap_harness` command body: `Err("no_active_run")` when nothing is
/// running (contracts-m5.md §C6); otherwise delegates to
/// `RunHandle::swap_harness` (contracts-m5.md §C5c), converting its
/// `RunError` to a string.
pub async fn swap_harness_core(state: &AppState, agent_id: &str, harness: &str) -> Result<(), String> {
    let guard = state.current.lock().await;
    match guard.as_ref() {
        Some(active) => active.handle.swap_harness(agent_id, harness).await.map_err(|e| e.to_string()),
        None => Err("no_active_run".to_string()),
    }
}

/// `resolve_gate` command body (contracts-m7.md §E7, t-bridge3 plan D2):
/// `decision` must parse as `"approve"`/`"reject"` — anything else is a
/// parse `Err` string, checked before touching `state` so an unparseable
/// decision never depends on whether a run is active. `Err("no_active_run")`
/// when nothing is running (existing convention, see `swap_harness_core`
/// above); otherwise delegates to `RunHandle::resolve_gate`, converting its
/// `RunError` to a string.
pub async fn resolve_gate_core(
    state: &AppState,
    task_id: &str,
    decision: &str,
    reason: &str,
) -> Result<(), String> {
    let decision = match decision {
        "approve" => GateDecision::Approve,
        "reject" => GateDecision::Reject,
        other => return Err(format!("unknown decision \"{other}\" (expected \"approve\" or \"reject\")")),
    };
    let guard = state.current.lock().await;
    match guard.as_ref() {
        Some(active) => active.handle.resolve_gate(task_id, decision, reason).await.map_err(|e| e.to_string()),
        None => Err("no_active_run".to_string()),
    }
}

/// `search_messages` command body (contracts-m7.md §E7, t-bridge3 plan D2):
/// limit fixed at 50 (not caller-controlled), `Err("no_active_run")` when
/// nothing is running, results serialized in the same `{seq, envelope}`
/// shape `snapshot`'s `messages` field already uses (`StoredMessageDto`).
/// Runs on the async state lock (`tokio::sync::Mutex`) like `run_snapshot_core`/
/// `swap_harness_core` above — deviates from the contract sketch's bare `fn`
/// (Tauri's JS `invoke` returns a `Promise` either way, so callers are
/// unaffected; see decisions.md).
const SEARCH_MESSAGES_LIMIT: usize = 50;

pub async fn search_messages_core(state: &AppState, query: &str) -> Result<serde_json::Value, String> {
    let guard = state.current.lock().await;
    match guard.as_ref() {
        Some(active) => {
            let results = active
                .handle
                .search_messages(query, SEARCH_MESSAGES_LIMIT)
                .map_err(|e| e.to_string())?;
            let dtos: Vec<StoredMessageDto> = results
                .into_iter()
                .map(|m| StoredMessageDto { seq: m.seq, envelope: m.envelope })
                .collect();
            serde_json::to_value(dtos).map_err(|e| e.to_string())
        }
        None => Err("no_active_run".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    /// A fresh directory under `target/` (never `/tmp`/`$TMPDIR` — workspace
    /// constraints) for this test's `data_root`; removed on drop.
    struct TestDataRoot(std::path::PathBuf);

    impl TestDataRoot {
        fn new(label: &str) -> Self {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-data")
                .join(format!("{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDataRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Short variant name for failure messages (plan D1) — avoids dumping
    /// full `RunEvent` payloads (spec/dag bodies) into assertion output.
    fn event_kind(event: &RunEvent) -> &'static str {
        match event {
            RunEvent::RunStarted { .. } => "RunStarted",
            RunEvent::SpecReady { .. } => "SpecReady",
            RunEvent::Message { .. } => "Message",
            RunEvent::TaskStateChanged { .. } => "TaskStateChanged",
            RunEvent::BusLifecycle { .. } => "BusLifecycle",
            RunEvent::RunFinished { .. } => "RunFinished",
            RunEvent::SprintStarted { .. } => "SprintStarted",
            RunEvent::SprintFinished { .. } => "SprintFinished",
            RunEvent::RosterChanged { .. } => "RosterChanged",
        }
    }

    /// Collects emitted `RunEvent`s into a shared `Vec` for assertions.
    fn collecting_emit() -> (impl Fn(RunEvent) + Send + 'static, Arc<StdMutex<Vec<RunEvent>>>) {
        let collected = Arc::new(StdMutex::new(Vec::new()));
        let sink = collected.clone();
        let emit = move |ev: RunEvent| {
            sink.lock().unwrap().push(ev);
        };
        (emit, collected)
    }

    #[tokio::test]
    async fn normal_scripted_run_starts_and_pumps_events() {
        let root = TestDataRoot::new("normal");
        let state = AppState::default();
        let (emit, collected) = collecting_emit();

        let run_id = start_run_core(
            &state,
            "landing page".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit,
        )
        .await
        .expect("scripted run should start");
        assert!(!run_id.is_empty());

        // Wait for at least RunStarted + SpecReady to reach the pump.
        let mut waited = 0;
        while collected.lock().unwrap().len() < 2 && waited < 200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waited += 1;
        }
        let events = collected.lock().unwrap().clone();
        assert!(
            matches!(events.first(), Some(RunEvent::RunStarted { run_id: id, .. }) if id == &run_id),
            "expected RunStarted first, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::SpecReady { .. })),
            "expected a SpecReady event, got {events:?}"
        );

        stop_run_core(&state).await.expect("stop should succeed");
    }

    /// Reproduction test (plan D1): the existing
    /// `normal_scripted_run_starts_and_pumps_events` only waits for
    /// `RunStarted`+`SpecReady` (2 events) — it has never proven that
    /// anything *after* `SpecReady` reaches the pump, which is exactly
    /// where the live app is reported stuck. Bounded polling (up to ~20s,
    /// no fixed sleep) waits for at least one `TaskStateChanged` and one
    /// `Message` to arrive; `crew-run`'s own `happy_path_...` integration
    /// test proves the engine emits these in well under a second, so this
    /// is a generous bound, not a tight one.
    #[tokio::test]
    async fn scripted_run_pumps_task_state_changed_and_message_after_spec_ready() {
        let root = TestDataRoot::new("post-spec-ready");
        let state = AppState::default();
        let (emit, collected) = collecting_emit();

        let run_id = start_run_core(
            &state,
            "landing page".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit,
        )
        .await
        .expect("scripted run should start");
        assert!(!run_id.is_empty());

        let has_task_state_changed = |events: &[RunEvent]| {
            events.iter().any(|e| matches!(e, RunEvent::TaskStateChanged { .. }))
        };
        let has_message = |events: &[RunEvent]| events.iter().any(|e| matches!(e, RunEvent::Message { .. }));

        let mut waited = 0;
        while waited < 2000 {
            let events = collected.lock().unwrap().clone();
            if has_task_state_changed(&events) && has_message(&events) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waited += 1;
        }

        let events = collected.lock().unwrap().clone();
        assert!(
            events.iter().any(|e| matches!(e, RunEvent::SpecReady { .. })),
            "precondition: expected a SpecReady event, got {events:?}"
        );
        assert!(
            has_task_state_changed(&events),
            "expected >=1 TaskStateChanged after SpecReady within the timeout, got {} events: {:?}",
            events.len(),
            events.iter().map(event_kind).collect::<Vec<_>>()
        );
        assert!(
            has_message(&events),
            "expected >=1 Message after SpecReady within the timeout, got {} events: {:?}",
            events.len(),
            events.iter().map(event_kind).collect::<Vec<_>>()
        );

        stop_run_core(&state).await.expect("stop should succeed");
    }

    #[tokio::test]
    async fn double_start_while_running_errors_without_replacing_the_run() {
        let root = TestDataRoot::new("double-start");
        let state = AppState::default();
        let (emit1, _c1) = collecting_emit();
        let first_id = start_run_core(
            &state,
            "goal".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit1,
        )
        .await
        .expect("first run should start");

        let (emit2, _c2) = collecting_emit();
        let err = start_run_core(
            &state,
            "goal 2".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit2,
        )
        .await
        .expect_err("second start while a run is active must error");
        assert_eq!(err, "run_in_progress");

        // The original run is untouched.
        let guard = state.current.lock().await;
        assert_eq!(guard.as_ref().unwrap().handle.run_id(), first_id);
        drop(guard);

        stop_run_core(&state).await.expect("stop should succeed");
    }

    #[tokio::test]
    async fn stop_then_restart_boundary_allows_a_fresh_run() {
        let root = TestDataRoot::new("stop-restart");
        let state = AppState::default();

        let (emit1, _c1) = collecting_emit();
        let first_id = start_run_core(
            &state,
            "goal".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit1,
        )
        .await
        .expect("first run should start");

        stop_run_core(&state).await.expect("stop should succeed");
        assert!(state.current.lock().await.is_none(), "state must clear after stop");

        let (emit2, _c2) = collecting_emit();
        let second_id = start_run_core(
            &state,
            "goal".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit2,
        )
        .await
        .expect("a new run must be startable after stop");
        assert_ne!(first_id, second_id, "each run gets its own run_id");

        // No active run after a second stop -> Err, not a panic (boundary case).
        stop_run_core(&state).await.expect("stop should succeed");
        let snap_err = run_snapshot_core(&state).await.expect_err("no active run after stop");
        assert_eq!(snap_err, "no_active_run");
    }

    #[test]
    fn list_presets_returns_the_four_built_in_presets_with_fixed_slots() {
        let presets = built_in_presets();
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["클로드 5인팀", "절약 모드", "혼합 실험", "미니 2인팀"]);
        // contracts-m7.md §E7: preset len==6 fixed assertion updated to
        // per-preset expected lengths (not a weakening — contract verbatim).
        let expected_lens = [6, 6, 6, 3];
        for (preset, expected) in presets.iter().zip(expected_lens) {
            assert_eq!(preset.roster.agents.len(), expected, "{} must have {expected} fixed slots", preset.name);
        }

        let claude_team = &presets[0].roster;
        assert!(
            claude_team.agents.iter().all(|a| a.harness == "claude-code" && a.model == "default"),
            "클로드 5인팀 must be all claude-code/default"
        );

        let saving = &presets[1].roster;
        let lead = saving.agents.iter().find(|a| a.id == "agent:lead").unwrap();
        let developer = saving.agents.iter().find(|a| a.id == "agent:developer").unwrap();
        let pm = saving.agents.iter().find(|a| a.id == "agent:pm").unwrap();
        assert_eq!(lead.model, "opus");
        assert_eq!(developer.model, "opus");
        assert_eq!(pm.model, "sonnet");
        assert!(saving.agents.iter().all(|a| a.harness == "claude-code"), "절약 모드 stays on claude-code");

        let mixed = &presets[2].roster;
        let publisher = mixed.agents.iter().find(|a| a.id == "agent:publisher").unwrap();
        assert_eq!(publisher.harness, "opencode");
        assert!(
            mixed.agents.iter().filter(|a| a.id != "agent:publisher").all(|a| a.harness == "claude-code"),
            "혼합 실험 swaps only publisher"
        );

        let mini = &presets[3].roster;
        let mini_ids: Vec<&str> = mini.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(mini_ids, vec!["agent:lead", "agent:developer", "agent:qa"]);
        assert!(
            mini.agents.iter().all(|a| a.harness == "claude-code" && a.model == "default"),
            "미니 2인팀 must be all claude-code/default"
        );
    }

    #[test]
    fn roster_round_trips_through_save_and_get_after_missing_file_creates_default() {
        let root = TestDataRoot::new("roster-normal");
        let path = root.0.join("roster.json");
        assert!(!path.exists());

        let created = get_roster_core(&path).expect("missing file must yield the default roster");
        let created: Roster = serde_json::from_value(created).unwrap();
        assert_eq!(created, claude_five_team());
        assert!(path.exists(), "load must persist the default it created");

        let mut custom = created.clone();
        custom.agents[0].model = "opus".to_string();
        set_roster_core(&path, serde_json::to_value(&custom).unwrap()).expect("set_roster should save");

        let fetched = get_roster_core(&path).expect("get_roster should read back what was saved");
        let fetched: Roster = serde_json::from_value(fetched).unwrap();
        assert_eq!(fetched, custom);
    }

    #[test]
    fn corrupted_roster_file_errors_without_overwriting_it() {
        let root = TestDataRoot::new("roster-corrupt");
        let path = root.0.join("roster.json");
        std::fs::write(&path, "not valid json").unwrap();

        let err = get_roster_core(&path).expect_err("malformed JSON must not be silently replaced");
        assert!(!err.is_empty());

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw, "not valid json", "the original file must be preserved, not overwritten");
    }

    /// Boundary case, updated for contracts-m7.md §E7/t-bridge3 plan D3:
    /// `set_roster_core` now runs `validate_roster` before saving, so an
    /// empty-agents roster (no "lead" slot) is rejected rather than
    /// round-tripping — this is the intentional behavior `validate_roster`
    /// (contracts-m7.md §E4, already merged) always specified; `set_roster`
    /// simply never enforced it until this task. Not a weakening: the
    /// boundary (empty collection) is still exercised, now proving it is
    /// guarded.
    #[test]
    fn empty_agents_roster_is_rejected_by_validate_roster_and_leaves_file_unchanged() {
        let root = TestDataRoot::new("roster-empty");
        let path = root.0.join("roster.json");
        assert!(!path.exists());
        let empty = Roster { agents: vec![] };

        let err = set_roster_core(&path, serde_json::to_value(&empty).unwrap())
            .expect_err("an empty roster has no \"lead\" slot and must be rejected");
        assert!(!err.is_empty());
        assert!(!path.exists(), "a rejected roster must never create/overwrite the file");
    }

    /// Error case (contracts-m7.md §E7, t-bridge3 plan D5 ②): a roster with
    /// a duplicate non-lead role is rejected by `validate_roster` before
    /// any write, and the previously-saved valid roster is left byte-for-
    /// byte unchanged.
    #[test]
    fn set_roster_with_duplicate_role_errors_and_leaves_file_unchanged() {
        let root = TestDataRoot::new("roster-dup-role");
        let path = root.0.join("roster.json");
        let valid = claude_five_team();
        set_roster_core(&path, serde_json::to_value(&valid).unwrap()).expect("a valid roster must save");
        let before = std::fs::read_to_string(&path).unwrap();

        let invalid = Roster {
            agents: vec![
                RosterAgent {
                    id: "agent:lead".to_string(),
                    role: "lead".to_string(),
                    harness: "claude-code".to_string(),
                    model: "default".to_string(),
                    instructions: String::new(),
                },
                RosterAgent {
                    id: "agent:developer".to_string(),
                    role: "developer".to_string(),
                    harness: "claude-code".to_string(),
                    model: "default".to_string(),
                    instructions: String::new(),
                },
                RosterAgent {
                    id: "agent:developer-2".to_string(),
                    role: "developer".to_string(),
                    harness: "claude-code".to_string(),
                    model: "default".to_string(),
                    instructions: String::new(),
                },
            ],
        };

        let err = set_roster_core(&path, serde_json::to_value(&invalid).unwrap())
            .expect_err("a duplicate non-lead role must be rejected");
        assert!(err.contains("duplicate role"), "error must name the violation, got {err:?}");

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, before, "a rejected roster must leave the previously-saved file unchanged");
    }

    #[tokio::test]
    async fn swap_harness_without_an_active_run_errors() {
        let state = AppState::default();
        let err = swap_harness_core(&state, "agent:designer", "opencode")
            .await
            .expect_err("swap with no active run must error, not panic");
        assert_eq!(err, "no_active_run");
    }

    /// Boundary case (contracts-m7.md §E7, t-bridge3 plan D5 ④): an
    /// unparseable `decision` errors even with no active run — the parse
    /// check runs before the run-state lookup.
    #[tokio::test]
    async fn resolve_gate_with_an_unparseable_decision_errors() {
        let state = AppState::default();
        let err = resolve_gate_core(&state, "t-dev", "banana", "reason")
            .await
            .expect_err("an unrecognized decision string must error, not panic");
        assert!(err.contains("unknown decision"), "error must name the violation, got {err:?}");
    }

    /// Error case (contracts-m7.md §E7, t-bridge3 plan D5 ④): a valid
    /// decision with no active run errors via the existing "no_active_run"
    /// convention (`swap_harness_core` above), not a panic.
    #[tokio::test]
    async fn resolve_gate_without_an_active_run_errors() {
        let state = AppState::default();
        let err = resolve_gate_core(&state, "t-dev", "approve", "looks good")
            .await
            .expect_err("resolve_gate with no active run must error, not panic");
        assert_eq!(err, "no_active_run");
    }

    /// Error case (contracts-m7.md §E7, t-bridge3 plan D5 ③): no active run
    /// errors via the existing "no_active_run" convention, not a panic.
    #[tokio::test]
    async fn search_messages_without_an_active_run_errors() {
        let state = AppState::default();
        let err = search_messages_core(&state, "anything")
            .await
            .expect_err("search_messages with no active run must error, not panic");
        assert_eq!(err, "no_active_run");
    }

    #[tokio::test]
    async fn swap_harness_with_an_active_run_delegates_to_run_handle() {
        let root = TestDataRoot::new("swap-active");
        let state = AppState::default();
        let (emit, collected) = collecting_emit();
        start_run_core(
            &state,
            "goal".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit,
        )
        .await
        .expect("scripted run should start");

        swap_harness_core(&state, "agent:designer", "opencode")
            .await
            .expect("swap against an active run must delegate to RunHandle::swap_harness");

        let mut waited = 0;
        let roster_changed_count = |events: &[RunEvent]| {
            events.iter().filter(|e| matches!(e, RunEvent::RosterChanged { .. })).count()
        };
        while roster_changed_count(&collected.lock().unwrap()) < 2 && waited < 200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waited += 1;
        }
        let events = collected.lock().unwrap().clone();
        let swapped = events
            .iter()
            .filter_map(|e| match e {
                RunEvent::RosterChanged { agents, .. } => Some(agents),
                _ => None,
            })
            .last()
            .expect("swap_harness must emit a second RosterChanged");
        let designer = swapped.iter().find(|a| a.id == "agent:designer").unwrap();
        assert_eq!(designer.harness, "opencode", "the delegated swap must be reflected in the roster");

        stop_run_core(&state).await.expect("stop should succeed");
    }

    #[tokio::test]
    async fn start_run_forwards_the_saved_roster_into_run_config() {
        let root = TestDataRoot::new("roster-into-config");
        let roster_path = root.0.join("roster.json");
        save_roster(&roster_path, &mixed_experiment()).expect("seed a non-default roster");

        let state = AppState::default();
        let (emit, collected) = collecting_emit();
        start_run_core(&state, "goal".to_string(), true, &root.0, &roster_path, emit)
            .await
            .expect("scripted run should start");

        let mut waited = 0;
        while !collected.lock().unwrap().iter().any(|e| matches!(e, RunEvent::RosterChanged { .. })) && waited < 200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waited += 1;
        }
        let events = collected.lock().unwrap().clone();
        let roster_changed = events
            .iter()
            .find_map(|e| match e {
                RunEvent::RosterChanged { agents, .. } => Some(agents),
                _ => None,
            })
            .expect("RosterChanged must fire right after RunStarted");
        let publisher = roster_changed.iter().find(|a| a.id == "agent:publisher").unwrap();
        assert_eq!(
            publisher.harness, "opencode",
            "the saved roster.json's roster (혼합 실험) must reach RunConfig.roster, not crew-run's own default"
        );

        stop_run_core(&state).await.expect("stop should succeed");
    }
}
