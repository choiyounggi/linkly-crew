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
    core::start_run_core(&state, goal, scripted, &data_root, &roster_path, move |event| {
        // Best-effort: a closed/gone window means there is nothing left to
        // notify; the pump keeps draining so the run itself is unaffected.
        let _ = app.emit("run://event", &event);
    })
    .await
}

#[tauri::command]
async fn stop_run(state: State<'_, AppState>) -> Result<(), String> {
    core::stop_run_core(&state).await
}

#[tauri::command]
async fn run_snapshot(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    core::run_snapshot_core(&state).await
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

/// contracts-m5.md §C6 verbatim (t-bridge2): Tauri 2 maps the frontend's
/// camelCase invoke args (`{ agentId, harness }`, see
/// `src/lib/tauri-source.ts`) onto these snake_case parameters
/// automatically — no `#[serde(rename)]` needed.
#[tauri::command]
async fn swap_harness(agent_id: String, harness: String, state: State<'_, AppState>) -> Result<(), String> {
    core::swap_harness_core(&state, &agent_id, &harness).await
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
            swap_harness
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
