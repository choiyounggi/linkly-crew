//! Tauri command bridge (contracts-m4.md §C6, t-bridge): thin
//! `#[tauri::command]` wrappers over `core.rs`'s Tauri-independent bodies,
//! wired to `AppState` and the `"run://event"` pump.

mod core;
mod logging;
mod onboarding;
mod project;
mod pty;

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
    project_root: Option<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let data_root = app_runs_root();
    let roster_path = core::roster_path();
    core::start_run_core(
        &state,
        goal,
        scripted,
        &data_root,
        &roster_path,
        move |run_id, event| {
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
        },
        project_root.as_deref(),
    )
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

/// `t3-be-project` (`ProjectApi`/`OnboardingStatusApi` contract stub,
/// `apps/crew-app/src/lib/types.ts`): plain-object argument mirroring
/// `AppSettings` — accepted as-is (not yet `~`-expanded/validated) so
/// `onboarding::set_settings_core` stays the single place that does so.
#[derive(serde::Deserialize)]
struct SetSettingsArgs {
    workspace_root: String,
}

#[tauri::command]
fn get_settings() -> onboarding::Settings {
    onboarding::get_settings_core(&onboarding::settings_path())
}

#[tauri::command]
fn set_settings(settings: SetSettingsArgs) -> Result<onboarding::Settings, String> {
    onboarding::set_settings_core(&onboarding::settings_path(), &settings.workspace_root)
}

#[tauri::command]
async fn onboarding_status() -> Vec<onboarding::ToolStatus> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    onboarding::onboarding_status_core(&path_env).await
}

#[tauri::command]
async fn create_project(name: String) -> Result<project::ProjectInfo, String> {
    let settings = onboarding::get_settings_core(&onboarding::settings_path());
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    let gh_status = onboarding::detect_single("gh", &path_env).await;
    let gh_installed = gh_status.installed;
    let gh_authenticated = gh_status.authenticated.unwrap_or(false);
    let workspace_root = settings.workspace_root;
    // `create_project_core` shells out (git/gh) and can block up to gh's
    // 120s timeout — off the async runtime via `spawn_blocking` so it never
    // stalls other in-flight commands.
    tokio::task::spawn_blocking(move || {
        project::create_project_core(&name, &workspace_root, gh_installed, gh_authenticated, project::real_create_and_clone)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `list_projects()` (issue #13, plan D5/D7, task 03): no arguments — the
/// workspace root is read from settings, same as `create_project` above.
/// Filesystem scan, so it runs off the async runtime via `spawn_blocking`
/// too. Frontend consumer is `t2-fe-picker` (declared intent, not yet wired
/// as of this task).
#[tauri::command]
async fn list_projects() -> Result<Vec<project::ProjectInfo>, String> {
    let settings = onboarding::get_settings_core(&onboarding::settings_path());
    tokio::task::spawn_blocking(move || project::list_projects_core(&settings.workspace_root))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let log_guard = logging::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "crew-app started");

    tauri::Builder::default()
        // Native folder picker for the workspace path field — the frontend
        // calls this plugin's `open({directory: true})`, so the permission
        // it needs (`dialog:allow-open`) is declared in
        // `capabilities/default.json`.
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .manage(pty::PtyRegistry::default())
        .manage(log_guard)
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
            remove_run,
            get_settings,
            set_settings,
            onboarding_status,
            create_project,
            list_projects,
            pty::pty_open,
            pty::pty_write,
            pty::pty_resize,
            pty::pty_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
