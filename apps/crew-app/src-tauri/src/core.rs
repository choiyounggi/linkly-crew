//! Tauri-independent command bodies (plan D4): `lib.rs`'s `#[tauri::command]`
//! wrappers are thin adapters over these; Rust tests target this module only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crew_lead::accept::AcceptanceLoop;
use crew_proto::{Role, Roster, RosterAgent};
use crew_run::{validate_roster, GateDecision, RunConfig, RunController, RunEvent, RunHandle, RunMode, StoredMessageDto};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Finished runs kept per `AppState` before the oldest is evicted (plan D4) —
/// checked inside `start_run_core` on every insert, never mid-`stop_run_core`.
const MAX_FINISHED_RUNS: usize = 20;

/// One run's handle plus its event pump (plan D1/D3), or — once
/// `stop_run_core` has shut it down — the metadata `list_runs`/`run_snapshot`
/// still need after `handle`/`pump` are gone (`RunHandle::shutdown` consumes
/// its receiver by value, so there is no live handle left to snapshot).
pub struct ActiveRun {
    handle: Option<RunHandle>,
    pump: Option<JoinHandle<()>>,
    pub goal: String,
    /// `None` while running; `Some("completed")` once `stop_run_core` has
    /// shut this run down (plan D4). Only an explicit `stop_run` call marks
    /// a run finished in this task's scope — a run that completes on its
    /// own (`RunEvent::RunFinished`) is not auto-detected here.
    pub finished: Option<String>,
    /// Insertion-order tiebreak for "oldest finished" eviction (plan D4) —
    /// an `AppState`-wide counter, not a wall-clock timestamp, so eviction
    /// order is deterministic regardless of clock resolution.
    finished_seq: Option<u64>,
}

/// `list_runs` command body's row shape (plan D5 / TS `RunSummary`).
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub goal: String,
    pub finished: Option<String>,
}

/// `tauri::State`-managed app state (plan D1): any number of concurrent runs,
/// keyed by `run_id`.
#[derive(Default)]
pub struct AppState {
    pub runs: Mutex<HashMap<String, ActiveRun>>,
    finished_counter: AtomicU64,
}

/// Evicts the oldest finished runs (by `finished_seq`) until at most
/// `MAX_FINISHED_RUNS` remain (plan D4 4b). Running runs (`finished: None`)
/// are never candidates. Called only from `start_run_core`, on insert.
fn evict_oldest_finished_if_needed(runs: &mut HashMap<String, ActiveRun>) {
    while runs.values().filter(|r| r.finished.is_some()).count() > MAX_FINISHED_RUNS {
        let oldest = runs
            .iter()
            .filter(|(_, r)| r.finished.is_some())
            .min_by_key(|(_, r)| r.finished_seq.unwrap_or(u64::MAX))
            .map(|(id, _)| id.clone());
        match oldest {
            Some(id) => {
                runs.remove(&id);
            }
            None => break,
        }
    }
}

/// `scripted=true`'s `RunMode` (contracts-m4.md §C6 verbatim): a designer
/// rework round trip is planted so the demo run shows the rework loop.
fn scripted_mode() -> RunMode {
    RunMode::Scripted {
        planted_violations: vec![(Role::Designer, vec!["REQ-2".to_string()])],
    }
}

/// Starts a run and stores it in `state`, keyed by its own `run_id` (plan
/// D1/D2): any number of runs may be live at once — starting a second run
/// while a first is still running is no longer an error. `data_root` is the
/// parent the caller creates the run's own fresh `<launch-id>` directory
/// under (contracts-m4.md §C6 2026-08-28 amendment: `<launch-id>` need not
/// equal `RunHandle::run_id()`). `roster_path` is the saved roster to load
/// and forward as `RunConfig.roster` (contracts-m5.md §C6, plan D6) — a
/// corrupted file falls back to "클로드 5인팀" rather than blocking the run
/// (`get_roster` surfaces that corruption separately). `emit` receives every
/// `RunEvent` from the pump alongside the run's own `run_id` (plan D3) —
/// production wires it to a `{run_id, event}`-wrapped `AppHandle::emit`,
/// tests inject a channel/closure.
pub async fn start_run_core<F>(
    state: &AppState,
    goal: String,
    scripted: bool,
    data_root: &Path,
    roster_path: &Path,
    emit: F,
) -> Result<String, String>
where
    F: Fn(String, RunEvent) + Send + 'static,
{
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
    let goal_for_meta = goal.clone();
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

    let mut guard = state.runs.lock().await;
    evict_oldest_finished_if_needed(&mut guard);
    guard.insert(
        run_id.clone(),
        ActiveRun {
            handle: Some(handle),
            pump: Some(pump),
            goal: goal_for_meta,
            finished: None,
            finished_seq: None,
        },
    );
    Ok(run_id)
}

/// Subscribes to `handle` and forwards every `RunEvent`, tagged with its
/// `run_id`, to `emit` until the broadcast channel closes (plan D3).
/// `Lagged` is logged and skipped, never fatal; `Closed` ends the pump —
/// this is the run's natural termination, not an error.
fn spawn_pump<F>(handle: &RunHandle, emit: F) -> JoinHandle<()>
where
    F: Fn(String, RunEvent) + Send + 'static,
{
    let run_id = handle.run_id().to_string();
    let mut rx = handle.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => emit(run_id.clone(), event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "tauri bridge pump lagged; events dropped");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Stops the run named by `run_id`, if it exists, and marks it finished
/// (plan D1/D4) — the entry stays in `state.runs` for `list_runs`, only
/// `remove_run_core` deletes it. `Err("run_not_found")` when `run_id` is
/// absent from the map entirely; idempotent (`Ok(())`, no re-marking) when
/// it is present but already finished. The lock is not held across
/// `handle.shutdown().await` (plan D1 lock-scope rule) — `handle`/`pump` are
/// taken out of the map first, under a short-lived guard.
pub async fn stop_run_core(state: &AppState, run_id: &str) -> Result<(), String> {
    let (handle, pump) = {
        let mut guard = state.runs.lock().await;
        let active = guard.get_mut(run_id).ok_or_else(|| "run_not_found".to_string())?;
        (active.handle.take(), active.pump.take())
    };
    if let Some(pump) = pump {
        pump.abort();
    }
    if let Some(handle) = handle {
        handle.shutdown().await;
    }

    let mut guard = state.runs.lock().await;
    if let Some(active) = guard.get_mut(run_id) {
        if active.finished.is_none() {
            active.finished = Some("completed".to_string());
            active.finished_seq = Some(state.finished_counter.fetch_add(1, Ordering::SeqCst));
        }
    }
    Ok(())
}

/// Removes `run_id` from `state.runs` entirely, shutting it down first if it
/// is still running (plan D1/D4). `Err("run_not_found")` when `run_id` was
/// never present.
pub async fn remove_run_core(state: &AppState, run_id: &str) -> Result<(), String> {
    stop_run_core(state, run_id).await?;
    state.runs.lock().await.remove(run_id);
    Ok(())
}

/// Every run currently known to `state`, running or finished (plan D1/D5) —
/// used by `list_runs`.
pub async fn list_runs_core(state: &AppState) -> Result<serde_json::Value, String> {
    let guard = state.runs.lock().await;
    let summaries: Vec<RunSummary> = guard
        .iter()
        .map(|(run_id, active)| RunSummary {
            run_id: run_id.clone(),
            goal: active.goal.clone(),
            finished: active.finished.clone(),
        })
        .collect();
    serde_json::to_value(summaries).map_err(|e| e.to_string())
}

/// `run_id`'s snapshot (plan D1/D2). `Err("run_not_found")` both when
/// `run_id` is absent from the map and when it is present but no longer has
/// a live handle (already stopped) — `RunHandle::shutdown` consumes the
/// handle, so a finished run has nothing left to snapshot.
pub async fn run_snapshot_core(state: &AppState, run_id: &str) -> Result<serde_json::Value, String> {
    let guard = state.runs.lock().await;
    match guard.get(run_id).and_then(|active| active.handle.as_ref()) {
        Some(handle) => serde_json::to_value(handle.snapshot()).map_err(|e| e.to_string()),
        None => Err("run_not_found".to_string()),
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

/// `swap_harness` command body: `Err("run_not_found")` when `run_id` has no
/// live run (plan D1/D2 — every run-scoped command is now routed by
/// `run_id`, not just the `MultiRunApi` surface); otherwise delegates to
/// `RunHandle::swap_harness` (contracts-m5.md §C5c), converting its
/// `RunError` to a string.
pub async fn swap_harness_core(state: &AppState, run_id: &str, agent_id: &str, harness: &str) -> Result<(), String> {
    let guard = state.runs.lock().await;
    match guard.get(run_id).and_then(|active| active.handle.as_ref()) {
        Some(handle) => handle.swap_harness(agent_id, harness).await.map_err(|e| e.to_string()),
        None => Err("run_not_found".to_string()),
    }
}

/// `resolve_gate` command body (contracts-m7.md §E7, t-bridge3 plan D2):
/// `decision` must parse as `"approve"`/`"reject"` — anything else is a
/// parse `Err` string, checked before touching `state` so an unparseable
/// decision never depends on whether `run_id` has a live run.
/// `Err("run_not_found")` when `run_id` has no live run (plan D1/D2, see
/// `swap_harness_core` above); otherwise delegates to
/// `RunHandle::resolve_gate`, converting its `RunError` to a string.
pub async fn resolve_gate_core(
    state: &AppState,
    run_id: &str,
    task_id: &str,
    decision: &str,
    reason: &str,
) -> Result<(), String> {
    let decision = match decision {
        "approve" => GateDecision::Approve,
        "reject" => GateDecision::Reject,
        other => return Err(format!("unknown decision \"{other}\" (expected \"approve\" or \"reject\")")),
    };
    let guard = state.runs.lock().await;
    match guard.get(run_id).and_then(|active| active.handle.as_ref()) {
        Some(handle) => handle.resolve_gate(task_id, decision, reason).await.map_err(|e| e.to_string()),
        None => Err("run_not_found".to_string()),
    }
}

/// `search_messages` command body (contracts-m7.md §E7, t-bridge3 plan D2):
/// limit fixed at 50 (not caller-controlled), `Err("run_not_found")` when
/// `run_id` has no live run, results serialized in the same `{seq, envelope}`
/// shape `snapshot`'s `messages` field already uses (`StoredMessageDto`).
/// Runs on the async state lock (`tokio::sync::Mutex`) like `run_snapshot_core`/
/// `swap_harness_core` above — deviates from the contract sketch's bare `fn`
/// (Tauri's JS `invoke` returns a `Promise` either way, so callers are
/// unaffected; see decisions.md).
const SEARCH_MESSAGES_LIMIT: usize = 50;

pub async fn search_messages_core(state: &AppState, run_id: &str, query: &str) -> Result<serde_json::Value, String> {
    let guard = state.runs.lock().await;
    match guard.get(run_id).and_then(|active| active.handle.as_ref()) {
        Some(handle) => {
            let results = handle.search_messages(query, SEARCH_MESSAGES_LIMIT).map_err(|e| e.to_string())?;
            let dtos: Vec<StoredMessageDto> = results
                .into_iter()
                .map(|m| StoredMessageDto { seq: m.seq, envelope: m.envelope })
                .collect();
            serde_json::to_value(dtos).map_err(|e| e.to_string())
        }
        None => Err("run_not_found".to_string()),
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
            RunEvent::Presence { .. } => "Presence",
        }
    }

    /// Collects emitted `RunEvent`s into a shared `Vec` for assertions —
    /// ignores the `run_id` the pump now passes alongside each event (plan
    /// D3); `start_run_emit_callback_receives_the_run_id_alongside_each_event`
    /// below is what asserts on that `run_id`.
    fn collecting_emit() -> (impl Fn(String, RunEvent) + Send + 'static, Arc<StdMutex<Vec<RunEvent>>>) {
        let collected = Arc::new(StdMutex::new(Vec::new()));
        let sink = collected.clone();
        let emit = move |_run_id: String, ev: RunEvent| {
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

        stop_run_core(&state, &run_id).await.expect("stop should succeed");
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

        stop_run_core(&state, &run_id).await.expect("stop should succeed");
    }

    /// R1: starting a second run while a first is still running no longer
    /// errors — each concurrent `start_run_core` call gets its own live
    /// `run_id`, and neither run displaces the other in `state.runs`.
    #[tokio::test]
    async fn concurrent_start_run_calls_each_get_their_own_live_run_id() {
        let root = TestDataRoot::new("concurrent-start");
        let state = AppState::default();
        let (emit1, _c1) = collecting_emit();
        let (emit2, _c2) = collecting_emit();

        let first_id = start_run_core(&state, "goal A".to_string(), true, &root.0, &root.0.join("roster.json"), emit1)
            .await
            .expect("first run should start");
        let second_id = start_run_core(&state, "goal B".to_string(), true, &root.0, &root.0.join("roster.json"), emit2)
            .await
            .expect("second run should start concurrently, not error with run_in_progress");

        assert_ne!(first_id, second_id, "each concurrent run gets its own run_id");
        let guard = state.runs.lock().await;
        assert!(guard.contains_key(&first_id), "first run must still be live");
        assert!(guard.contains_key(&second_id), "second run must still be live");
        assert!(guard.get(&first_id).unwrap().finished.is_none());
        assert!(guard.get(&second_id).unwrap().finished.is_none());
        drop(guard);

        stop_run_core(&state, &first_id).await.expect("stop A should succeed");
        stop_run_core(&state, &second_id).await.expect("stop B should succeed");
    }

    /// R2: `stop_run_core` targets only its own `run_id` — a sibling run
    /// stays live and untouched.
    #[tokio::test]
    async fn stop_run_targets_only_its_run_id_leaving_others_live() {
        let root = TestDataRoot::new("stop-targets-one");
        let state = AppState::default();
        let (emit_a, _ca) = collecting_emit();
        let (emit_b, _cb) = collecting_emit();
        let a_id = start_run_core(&state, "goal A".to_string(), true, &root.0, &root.0.join("roster.json"), emit_a)
            .await
            .expect("A should start");
        let b_id = start_run_core(&state, "goal B".to_string(), true, &root.0, &root.0.join("roster.json"), emit_b)
            .await
            .expect("B should start");

        stop_run_core(&state, &a_id).await.expect("stop A should succeed");

        let guard = state.runs.lock().await;
        assert_eq!(guard.get(&a_id).unwrap().finished.as_deref(), Some("completed"), "A must be finished");
        assert!(guard.get(&b_id).unwrap().finished.is_none(), "B must still be running, untouched by stop(A)");
        drop(guard);

        stop_run_core(&state, &b_id).await.expect("cleanup stop B should succeed");
    }

    /// R3: the pump's `emit` callback receives each event's own `run_id`
    /// alongside it (plan D3) — this is exactly what `lib.rs`'s `start_run`
    /// command wraps into the `run://event` `{run_id, event}` payload;
    /// mirroring that wrapping here at the core level is what the payload
    /// shape assertion targets, without needing a live `AppHandle`.
    #[tokio::test]
    async fn start_run_emit_callback_receives_the_run_id_alongside_each_event() {
        let root = TestDataRoot::new("wrap-r3");
        let state = AppState::default();
        let wrapped: Arc<StdMutex<Vec<serde_json::Value>>> = Arc::new(StdMutex::new(Vec::new()));
        let sink = wrapped.clone();
        let emit = move |run_id: String, event: RunEvent| {
            sink.lock().unwrap().push(serde_json::json!({"run_id": run_id, "event": event}));
        };

        let run_id = start_run_core(&state, "goal".to_string(), true, &root.0, &root.0.join("roster.json"), emit)
            .await
            .expect("scripted run should start");

        let mut waited = 0;
        while wrapped.lock().unwrap().is_empty() && waited < 200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waited += 1;
        }
        let payloads = wrapped.lock().unwrap().clone();
        assert!(!payloads.is_empty(), "expected at least one wrapped event");
        for payload in &payloads {
            assert_eq!(
                payload.get("run_id").and_then(|v| v.as_str()),
                Some(run_id.as_str()),
                "every wrapped payload must carry this run's run_id, got {payload:?}"
            );
            assert!(payload.get("event").is_some(), "wrapped payload must carry the event, got {payload:?}");
        }

        stop_run_core(&state, &run_id).await.expect("stop should succeed");
    }

    /// Boundary: `stop_run_core`/`run_snapshot_core`/`remove_run_core` all
    /// error `"run_not_found"` for a `run_id` that was never started.
    #[tokio::test]
    async fn stop_snapshot_and_remove_error_on_a_nonexistent_run_id() {
        let state = AppState::default();
        let err = stop_run_core(&state, "does-not-exist").await.expect_err("stop must error on unknown run_id");
        assert_eq!(err, "run_not_found");
        let err = run_snapshot_core(&state, "does-not-exist")
            .await
            .expect_err("snapshot must error on unknown run_id");
        assert_eq!(err, "run_not_found");
        let err = remove_run_core(&state, "does-not-exist")
            .await
            .expect_err("remove must error on unknown run_id");
        assert_eq!(err, "run_not_found");
    }

    /// R5: `remove_run_core` shuts a still-running run down and deletes it
    /// from `state.runs` — a snapshot taken afterward errors, same as for a
    /// wholly unknown `run_id`.
    #[tokio::test]
    async fn remove_run_deletes_the_entry_and_subsequent_snapshot_errors() {
        let root = TestDataRoot::new("remove-run");
        let state = AppState::default();
        let (emit, _c) = collecting_emit();
        let run_id = start_run_core(&state, "goal".to_string(), true, &root.0, &root.0.join("roster.json"), emit)
            .await
            .expect("run should start");

        remove_run_core(&state, &run_id)
            .await
            .expect("remove should succeed even while running (shuts down first)");

        let err = run_snapshot_core(&state, &run_id)
            .await
            .expect_err("removed run must not be snapshot-able");
        assert_eq!(err, "run_not_found");
        let guard = state.runs.lock().await;
        assert!(!guard.contains_key(&run_id), "removed run must be gone from the map");
    }

    /// R5: `list_runs_core` reports each run's `goal` and `finished` status.
    #[tokio::test]
    async fn list_runs_reports_running_and_finished_status() {
        let root = TestDataRoot::new("list-runs");
        let state = AppState::default();
        let (emit_a, _ca) = collecting_emit();
        let (emit_b, _cb) = collecting_emit();
        let a_id = start_run_core(&state, "goal A".to_string(), true, &root.0, &root.0.join("roster.json"), emit_a)
            .await
            .expect("A should start");
        let b_id = start_run_core(&state, "goal B".to_string(), true, &root.0, &root.0.join("roster.json"), emit_b)
            .await
            .expect("B should start");
        stop_run_core(&state, &a_id).await.expect("stop A should succeed");

        let summaries = list_runs_core(&state).await.expect("list_runs should not error");
        let arr = summaries.as_array().expect("list_runs must return an array");
        let find = |run_id: &str| arr.iter().find(|s| s["run_id"] == run_id).expect("run must be listed").clone();

        let a = find(&a_id);
        assert_eq!(a["goal"], serde_json::json!("goal A"));
        assert_eq!(a["finished"], serde_json::json!("completed"));

        let b = find(&b_id);
        assert_eq!(b["goal"], serde_json::json!("goal B"));
        assert_eq!(b["finished"], serde_json::json!(null));

        stop_run_core(&state, &b_id).await.expect("cleanup stop B should succeed");
    }

    /// D4 boundary: once more than `MAX_FINISHED_RUNS` (20) finished runs
    /// exist, the next `start_run_core` call evicts the oldest finished
    /// entries down to the cap — and never evicts a still-running run, even
    /// though it is that very `start_run_core` call that triggers the sweep.
    #[tokio::test]
    async fn finished_runs_beyond_the_cap_evict_oldest_first_and_never_evict_a_running_run() {
        let root = TestDataRoot::new("evict-cap");
        let state = AppState::default();

        let mut finished_ids = Vec::new();
        for i in 0..21 {
            let (emit, _c) = collecting_emit();
            let id = start_run_core(&state, format!("goal {i}"), true, &root.0, &root.0.join("roster.json"), emit)
                .await
                .expect("run should start");
            stop_run_core(&state, &id).await.expect("stop should succeed");
            finished_ids.push(id);
        }
        {
            let guard = state.runs.lock().await;
            assert_eq!(
                guard.values().filter(|r| r.finished.is_some()).count(),
                21,
                "no eviction should happen until the next start_run_core call (plan D4 4b)"
            );
        }

        let (emit_running, _c) = collecting_emit();
        let running_id = start_run_core(
            &state,
            "still running".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit_running,
        )
        .await
        .expect("22nd run should start and trigger the eviction sweep");

        let guard = state.runs.lock().await;
        let finished_count = guard.values().filter(|r| r.finished.is_some()).count();
        assert_eq!(finished_count, 20, "finished runs must be capped at MAX_FINISHED_RUNS (20) after the sweep");
        assert!(!guard.contains_key(&finished_ids[0]), "the oldest finished run must be the one evicted");
        assert!(guard.contains_key(&finished_ids[20]), "the most recently finished run must survive");
        assert!(
            guard.get(&running_id).unwrap().finished.is_none(),
            "the just-started (still running) run must never be evicted"
        );
        drop(guard);

        stop_run_core(&state, &running_id).await.expect("cleanup stop should succeed");
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

        stop_run_core(&state, &first_id).await.expect("stop should succeed");
        assert_eq!(
            state.runs.lock().await.get(&first_id).unwrap().finished.as_deref(),
            Some("completed"),
            "the stopped run must be marked finished, not removed"
        );

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

        // A second stop of the same run_id is idempotent, not an error.
        stop_run_core(&state, &second_id).await.expect("re-stop should be idempotent, not error");
        let snap_err = run_snapshot_core(&state, &second_id)
            .await
            .expect_err("a finished run has no live handle left to snapshot");
        assert_eq!(snap_err, "run_not_found");
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
        let err = swap_harness_core(&state, "does-not-exist", "agent:designer", "opencode")
            .await
            .expect_err("swap against an unknown run_id must error, not panic");
        assert_eq!(err, "run_not_found");
    }

    /// Boundary case (contracts-m7.md §E7, t-bridge3 plan D5 ④): an
    /// unparseable `decision` errors even against an unknown `run_id` — the
    /// parse check runs before the run-state lookup.
    #[tokio::test]
    async fn resolve_gate_with_an_unparseable_decision_errors() {
        let state = AppState::default();
        let err = resolve_gate_core(&state, "does-not-exist", "t-dev", "banana", "reason")
            .await
            .expect_err("an unrecognized decision string must error, not panic");
        assert!(err.contains("unknown decision"), "error must name the violation, got {err:?}");
    }

    /// Error case (contracts-m7.md §E7, t-bridge3 plan D5 ④): a valid
    /// decision against an unknown `run_id` errors via the "run_not_found"
    /// convention (`swap_harness_core` above), not a panic.
    #[tokio::test]
    async fn resolve_gate_without_an_active_run_errors() {
        let state = AppState::default();
        let err = resolve_gate_core(&state, "does-not-exist", "t-dev", "approve", "looks good")
            .await
            .expect_err("resolve_gate against an unknown run_id must error, not panic");
        assert_eq!(err, "run_not_found");
    }

    /// Error case (contracts-m7.md §E7, t-bridge3 plan D5 ③): an unknown
    /// run_id errors via the "run_not_found" convention, not a panic.
    #[tokio::test]
    async fn search_messages_without_an_active_run_errors() {
        let state = AppState::default();
        let err = search_messages_core(&state, "does-not-exist", "anything")
            .await
            .expect_err("search_messages against an unknown run_id must error, not panic");
        assert_eq!(err, "run_not_found");
    }

    #[tokio::test]
    async fn swap_harness_with_an_active_run_delegates_to_run_handle() {
        let root = TestDataRoot::new("swap-active");
        let state = AppState::default();
        let (emit, collected) = collecting_emit();
        let run_id = start_run_core(
            &state,
            "goal".to_string(),
            true,
            &root.0,
            &root.0.join("roster.json"),
            emit,
        )
        .await
        .expect("scripted run should start");

        swap_harness_core(&state, &run_id, "agent:designer", "opencode")
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

        stop_run_core(&state, &run_id).await.expect("stop should succeed");
    }

    #[tokio::test]
    async fn start_run_forwards_the_saved_roster_into_run_config() {
        let root = TestDataRoot::new("roster-into-config");
        let roster_path = root.0.join("roster.json");
        save_roster(&roster_path, &mixed_experiment()).expect("seed a non-default roster");

        let state = AppState::default();
        let (emit, collected) = collecting_emit();
        let run_id = start_run_core(&state, "goal".to_string(), true, &root.0, &roster_path, emit)
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

        stop_run_core(&state, &run_id).await.expect("stop should succeed");
    }
}
