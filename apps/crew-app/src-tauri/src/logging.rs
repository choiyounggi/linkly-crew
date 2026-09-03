//! Tracing log file sink for the Tauri app (M13 turn-recovery fix D5): a
//! `tracing` subscriber writing to `~/.linkly-crew/logs/crew-app.log.<date>`
//! (daily-rolling, non-blocking) is installed once at `run()`'s start, so
//! `crew-harness`'s `drain_stderr` warnings and the discard-session warnings
//! (M13 D1/D6) are persisted instead of warning into the void — no
//! subscriber was installed anywhere before this (grep across `crates` and
//! `apps/crew-app/src-tauri/src` found zero `tracing_subscriber` hits).

use std::path::{Path, PathBuf};

use tracing_subscriber::EnvFilter;

/// `~/.linkly-crew/logs` — mirrors `lib.rs::app_runs_root`'s HOME-based
/// style (`~/.linkly-crew/app-runs`).
pub fn log_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set on this platform");
    PathBuf::from(home).join(".linkly-crew").join("logs")
}

/// Keeps the `tracing-appender` non-blocking writer's flush thread alive
/// for the process lifetime (D5) — dropping it would silently stop
/// flushing. Held in `tauri::Builder::manage` state, never read.
pub struct LogGuard(#[allow(dead_code)] pub tracing_appender::non_blocking::WorkerGuard);

fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Installs the subscriber against `log_dir()`. `Some(LogGuard)` on success
/// (file sink); `None` if the directory could not be created — falls back
/// to a stderr subscriber rather than panicking at startup. See `init_at`
/// (the testable seam this delegates to) for both outcomes' behavior.
pub fn init() -> Option<LogGuard> {
    init_at(&log_dir())
}

/// `init`'s logic, parameterized on the target directory so tests can point
/// it at a writable or unwritable path without touching the real
/// `~/.linkly-crew/logs`. `Ok(())` from `create_dir_all(dir)` (including
/// "already exists") installs the daily-rolling file sink and returns
/// `Some(LogGuard)`; an `Err` (e.g. `dir`'s parent is a regular file, not a
/// directory) prints the reason to stderr — there is no log file to write
/// it to yet — installs a stderr-only subscriber instead, and returns
/// `None`. `try_init` errors (a subscriber already installed) are ignored
/// either way: this function's `Option` return always reflects directory
/// creation, never subscriber-install success.
fn init_at(dir: &Path) -> Option<LogGuard> {
    match std::fs::create_dir_all(dir) {
        Ok(()) => {
            let file_appender = tracing_appender::rolling::daily(dir, "crew-app.log");
            let (writer, guard) = tracing_appender::non_blocking(file_appender);
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter())
                .with_writer(writer)
                .with_ansi(false)
                .try_init();
            Some(LogGuard(guard))
        }
        Err(e) => {
            eprintln!("crew-app: log dir unavailable ({e}); logging to stderr");
            let _ = tracing_subscriber::fmt()
                .with_env_filter(env_filter())
                .with_writer(std::io::stderr)
                .try_init();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, uniquely-named path under `target/` (never `/tmp`/`$TMPDIR`
    /// — workspace constraints), mirroring `core.rs::tests::TestDataRoot`.
    /// Unlike that helper, this does NOT create anything at `path` — some
    /// tests need it absent (so `init_at` creates it), others need it
    /// occupied by a plain file first (the "cannot create a directory
    /// here" boundary case) — so creation is each test's own job. Contained
    /// entirely under its own uuid-named path, never touching the shared
    /// `target/test-data` parent other tests in this crate also use.
    /// Removed (file or directory, whichever it ended up as) on drop.
    struct TestScratchPath(PathBuf);

    impl TestScratchPath {
        fn new(label: &str) -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-data")
                .join(format!("logging-{label}-{}", uuid::Uuid::new_v4()));
            Self(path)
        }
    }

    impl Drop for TestScratchPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn log_dir_is_under_home_dot_linkly_crew_logs() {
        let home = std::env::var("HOME").expect("HOME must be set for this test");
        assert_eq!(log_dir(), PathBuf::from(home).join(".linkly-crew").join("logs"));
    }

    #[test]
    fn init_at_creates_the_directory_and_returns_a_guard() {
        let scratch = TestScratchPath::new("ok");

        let guard = init_at(&scratch.0);

        assert!(guard.is_some(), "a creatable directory must yield Some(LogGuard)");
        assert!(scratch.0.is_dir(), "init_at must actually create the directory");
    }

    /// Boundary case: a restarted app points `init_at` at the same `dir` a
    /// prior run already created — `create_dir_all` on an already-existing
    /// directory is `Ok(())`, not an error, so this must still succeed.
    #[test]
    fn init_at_succeeds_when_the_directory_already_exists() {
        let scratch = TestScratchPath::new("preexisting");
        std::fs::create_dir_all(&scratch.0).expect("pre-create the directory");

        let guard = init_at(&scratch.0);

        assert!(guard.is_some(), "an already-existing directory must still yield Some(LogGuard)");
    }

    /// Boundary/error case: `dir` is nested under a plain file (not a
    /// directory), so `create_dir_all` cannot create `dir` — `init_at` must
    /// fall back to `None` (stderr subscriber) rather than panicking.
    #[test]
    fn init_at_returns_none_when_the_directory_cannot_be_created() {
        let blocker_file = TestScratchPath::new("blocker");
        std::fs::create_dir_all(blocker_file.0.parent().expect("blocker's parent must exist")).unwrap();
        std::fs::write(&blocker_file.0, b"not a directory").expect("create the blocking file");
        let unreachable_dir = blocker_file.0.join("logs");

        let guard = init_at(&unreachable_dir);

        assert!(guard.is_none(), "a directory that cannot be created must yield None");
    }
}
