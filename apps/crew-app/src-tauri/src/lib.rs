// Stub app shell (t-shell scope): default window only, zero commands.
// Tauri commands (start_run/stop_run/run_snapshot) and the crew-run
// path-dep are t-bridge's job.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
