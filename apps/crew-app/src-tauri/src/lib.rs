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
    core::start_run_core(&state, goal, scripted, &data_root, move |event| {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![start_run, stop_run, run_snapshot])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
