//! Tauri command bridge (contracts-m4.md §C6, t-bridge): thin
//! `#[tauri::command]` wrappers over `core.rs`'s Tauri-independent bodies,
//! wired to `AppState` and the `"run://event"` pump.

mod core;

use core::AppState;

use tauri::{AppHandle, Emitter, State};

/// App-default `data_root` (contracts-m4.md §C6): `~/.linkly-crew/app-runs/`.
/// Each run gets its own fresh `<launch-id>` subdirectory (see
/// `core::start_run_core`). Never `/tmp`/`$TMPDIR` (workspace constraints).
fn app_runs_root() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set on this platform");
    std::path::Path::new(&home).join(".linkly-crew").join("app-runs")
}

#[tauri::command]
async fn start_run(
    goal: String,
    scripted: bool,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let data_root = app_runs_root();
    let roster_path = core::roster_path();
    core::start_run_core(&state, goal, scripted, &data_root, &roster_path, move |run_id, event| {
        // Best-effort: a closed/gone window means there is nothing left to
        // notify; the pump keeps draining so the run itself is unaffected.
        // Still logged (plan D5) — a closed window is the only expected
        // cause, and this was previously unobservable (`let _ =` swallowed
        // every emit failure, including real ones). Payload wrapped as
        // `{run_id, event}` (plan D3) so the frontend can route it to the
        // right run's channel.
        if let Err(err) = app.emit("run://event", &serde_json::json!({"run_id": run_id, "event": event})) {
            tracing::warn!(error = %err, "run://event emit failed");
        }
    })
    .await
}

#[tauri::command]
async fn stop_run(run_id: String, state: State<'_, AppState>) -> Result<(), String> {
    core::stop_run_core(&state, &run_id).await
}

#[tauri::command]
async fn run_snapshot(run_id: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    core::run_snapshot_core(&state, &run_id).await
}

#[tauri::command]
async fn list_runs(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    core::list_runs_core(&state).await
}

#[tauri::command]
async fn remove_run(run_id: String, state: State<'_, AppState>) -> Result<(), String> {
    core::remove_run_core(&state, &run_id).await
}

/// contracts-m5.md §C6 verbatim (t-bridge2).
#[tauri::command]
fn detect_harnesses() -> Result<serde_json::Value, String> {
    core::detect_harnesses_core()
}

/// contracts-m5.md §C6 verbatim (t-bridge2).
#[tauri::command]
fn get_roster() -> Result<serde_json::Value, String> {
    core::get_roster_core(&core::roster_path())
}

/// contracts-m5.md §C6 verbatim (t-bridge2).
#[tauri::command]
fn set_roster(roster: serde_json::Value) -> Result<(), String> {
    core::set_roster_core(&core::roster_path(), roster)
}

/// contracts-m5.md §C6 verbatim (t-bridge2).
#[tauri::command]
fn list_presets() -> Result<serde_json::Value, String> {
    core::list_presets_core()
}

/// contracts-m5.md §C6 verbatim (t-bridge2), extended by plan D2: every
/// run-scoped command is now routed by `run_id` — Tauri 2 maps the
/// frontend's camelCase invoke args (`{ runId, agentId, harness }`, see
/// `src/lib/tauri-source.ts`) onto these snake_case parameters
/// automatically — no `#[serde(rename)]` needed.
#[tauri::command]
async fn swap_harness(
    run_id: String,
    agent_id: String,
    harness: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    core::swap_harness_core(&state, &run_id, &agent_id, &harness).await
}

/// contracts-m7.md §E7 (t-bridge3), extended by plan D2 with `run_id`:
/// frontend calls `invoke("resolve_gate", { runId, taskId, decision,
/// reason })` (`tauri-source.ts`, t6-fe-shell owns updating that call site).
#[tauri::command]
async fn resolve_gate(
    run_id: String,
    task_id: String,
    decision: String,
    reason: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    core::resolve_gate_core(&state, &run_id, &task_id, &decision, &reason).await
}

/// contracts-m7.md §E7 (t-bridge3), extended by plan D2 with `run_id`:
/// frontend calls `invoke("search_messages", { runId, query })`
/// (`tauri-source.ts`, t6-fe-shell owns updating that call site).
#[tauri::command]
async fn search_messages(run_id: String, query: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    core::search_messages_core(&state, &run_id, &query).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            start_run,
            stop_run,
            run_snapshot,
            detect_harnesses,
            get_roster,
            set_roster,
            list_presets,
            swap_harness,
            resolve_gate,
            search_messages,
            list_runs,
            remove_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
