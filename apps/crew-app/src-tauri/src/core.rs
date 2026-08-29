//! Tauri-independent command bodies (plan D4): `lib.rs`'s `#[tauri::command]`
//! wrappers are thin adapters over these; Rust tests target this module only.

use std::path::Path;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::Role;
use crew_run::{RunConfig, RunController, RunEvent, RunHandle, RunMode};
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
/// `<launch-id>` need not equal `RunHandle::run_id()`). `emit` receives every
/// `RunEvent` from the pump — production wires it to `AppHandle::emit`,
/// tests inject a channel/closure (plan D4).
pub async fn start_run_core<F>(
    state: &AppState,
    goal: String,
    scripted: bool,
    data_root: &Path,
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
    let cfg = RunConfig {
        goal,
        mode,
        data_dir: data_root.join(launch_id),
        max_rework: AcceptanceLoop::default_budget(),
        // contracts-m5.md §C5a defaults: single sprint, escalation cascade
        // off, default roster — current bridge behavior unchanged.
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster: None,
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

        let run_id = start_run_core(&state, "landing page".to_string(), true, &root.0, emit)
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

    #[tokio::test]
    async fn double_start_while_running_errors_without_replacing_the_run() {
        let root = TestDataRoot::new("double-start");
        let state = AppState::default();
        let (emit1, _c1) = collecting_emit();
        let first_id = start_run_core(&state, "goal".to_string(), true, &root.0, emit1)
            .await
            .expect("first run should start");

        let (emit2, _c2) = collecting_emit();
        let err = start_run_core(&state, "goal 2".to_string(), true, &root.0, emit2)
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
        let first_id = start_run_core(&state, "goal".to_string(), true, &root.0, emit1)
            .await
            .expect("first run should start");

        stop_run_core(&state).await.expect("stop should succeed");
        assert!(state.current.lock().await.is_none(), "state must clear after stop");

        let (emit2, _c2) = collecting_emit();
        let second_id = start_run_core(&state, "goal".to_string(), true, &root.0, emit2)
            .await
            .expect("a new run must be startable after stop");
        assert_ne!(first_id, second_id, "each run gets its own run_id");

        // No active run after a second stop -> Err, not a panic (boundary case).
        stop_run_core(&state).await.expect("stop should succeed");
        let snap_err = run_snapshot_core(&state).await.expect_err("no active run after stop");
        assert_eq!(snap_err, "no_active_run");
    }
}
