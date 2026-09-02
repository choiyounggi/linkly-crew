//! `create_project` backend (plan D5-D7): name validation, `gh repo create
//! --clone`, `.crew/artifacts/` scaffold + initial commit. `lib.rs`'s
//! `#[tauri::command]` wrapper is a thin adapter over `create_project_core`;
//! Rust tests target this module only (mirrors `core.rs`'s convention).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// `create_project(name)`'s success payload — field shape is the
/// `ProjectInfo` contract stub in `apps/crew-app/src/lib/types.ts` verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    pub path: String,
}

/// Project-name validation (plan D6): `^[a-z0-9][a-z0-9-]{0,62}$` — one to
/// 63 lowercase-alnum/hyphen characters, must not start with a hyphen.
/// Written by hand rather than pulling in the `regex` crate for one pattern.
fn validate_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("invalid_name".to_string());
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err("invalid_name".to_string());
    }
    if name.chars().count() > 63 {
        return Err("invalid_name".to_string());
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err("invalid_name".to_string());
    }
    Ok(())
}

/// `.crew/artifacts/README.md` contents (plan D7 — project-local convention,
/// not sourced from the wiki).
const ARTIFACTS_README: &str =
    "에이전트 공용 산출물 폴더 — 산출물·전달 문서·이미지·디자인 토큰을 여기서 공유한다. 하위: docs/ images/ design-tokens/ deliverables/\n";

/// Creates `.crew/artifacts/{README.md,.gitkeep}` under `project_path` and
/// commits them via `git add -A && git commit` (plan D7 — `std::process`,
/// not `git2`, matching this crate's existing convention of shelling out to
/// `git`/`gh` rather than linking a git library).
fn scaffold_and_commit(project_path: &Path) -> Result<(), String> {
    let artifacts_dir = project_path.join(".crew").join("artifacts");
    std::fs::create_dir_all(&artifacts_dir).map_err(|e| format!("scaffold_failed: {e}"))?;
    std::fs::write(artifacts_dir.join("README.md"), ARTIFACTS_README).map_err(|e| format!("scaffold_failed: {e}"))?;
    std::fs::write(artifacts_dir.join(".gitkeep"), b"").map_err(|e| format!("scaffold_failed: {e}"))?;

    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(project_path)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("commit_failed: git add: {e}"))?;
    if !add.success() {
        return Err("commit_failed: git add exited non-zero".to_string());
    }
    let commit = Command::new("git")
        .args(["commit", "-m", "chore: crew scaffold"])
        .current_dir(project_path)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("commit_failed: git commit: {e}"))?;
    if !commit.success() {
        return Err("commit_failed: git commit exited non-zero".to_string());
    }
    Ok(())
}

/// Runs `cmd` to completion, killing it if it runs past `timeout` (plan D5).
/// `std::process::Command` has no built-in wait-timeout, so this polls
/// `try_wait` — acceptable here since `create_project` already runs off the
/// async runtime via `spawn_blocking` (see `lib.rs`).
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Real `gh repo create <name> --private --clone` (plan D5): cwd is the
/// workspace root, stdin closed (never prompts), 120s timeout. Wired as the
/// production `create_and_clone` in `lib.rs`; the mocked/injected variant is
/// what `create_project_core`'s unit tests use instead — real-`gh` coverage
/// is a separate `#[ignore]`d test below (`tools_guidance`: real `gh` calls
/// stay isolated from the default suite).
pub fn real_create_and_clone(name: &str, cwd: &Path) -> Result<(), String> {
    let mut cmd = Command::new("gh");
    cmd.args(["repo", "create", name, "--private", "--clone"]).current_dir(cwd);
    let output = run_with_timeout(cmd, Duration::from_secs(120))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let summary = stderr.lines().next().unwrap_or("gh repo create failed").to_string();
        Err(summary)
    }
}

/// `create_project` command body (plan D5/D6/D7): validate name -> reject if
/// a same-named local folder already exists -> confirm `gh` is installed and
/// authenticated (both detected by the caller, plan Task 03 step 2's
/// "감지 재사용" — reuses `onboarding::detect_single("gh", ..)`, not a
/// second ad hoc check) -> `create_and_clone` -> scaffold + commit.
/// `create_and_clone` is injected (plan: "gh 호출부는 trait/함수 주입으로
/// 모킹") so the default test suite never shells out to the real `gh`.
pub fn create_project_core<C>(
    name: &str,
    workspace_root: &Path,
    gh_installed: bool,
    gh_authenticated: bool,
    create_and_clone: C,
) -> Result<ProjectInfo, String>
where
    C: FnOnce(&str, &Path) -> Result<(), String>,
{
    validate_name(name)?;

    let project_path = workspace_root.join(name);
    if project_path.exists() {
        return Err("project_exists".to_string());
    }

    if !gh_installed {
        return Err("gh_missing".to_string());
    }
    if !gh_authenticated {
        return Err("gh_unauthenticated".to_string());
    }

    std::fs::create_dir_all(workspace_root).map_err(|e| format!("create_failed: {e}"))?;
    create_and_clone(name, workspace_root).map_err(|e| format!("create_failed: {e}"))?;
    if !project_path.is_dir() {
        return Err("clone_failed".to_string());
    }

    scaffold_and_commit(&project_path)?;

    Ok(ProjectInfo { name: name.to_string(), path: project_path.to_string_lossy().into_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A fresh directory under `target/` (never `/tmp`/`$TMPDIR` — workspace
    /// constraints), removed on drop.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("test-project")
                .join(format!("{label}-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A `create_and_clone` stub that mimics `gh repo create --clone` by
    /// creating `<cwd>/<name>/` locally, with an initialized git repo inside
    /// (scaffold_and_commit needs `git add`/`git commit` to succeed).
    fn fake_clone_ok(name: &str, cwd: &Path) -> Result<(), String> {
        let project_dir = cwd.join(name);
        std::fs::create_dir_all(&project_dir).map_err(|e| e.to_string())?;
        let init = Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&project_dir)
            .status()
            .map_err(|e| e.to_string())?;
        if !init.success() {
            return Err("git init failed".to_string());
        }
        // A commit needs an identity; set one local to this throwaway repo
        // only (never touches the developer's real git config).
        for (key, value) in [("user.email", "test@example.invalid"), ("user.name", "Test")] {
            Command::new("git").args(["config", key, value]).current_dir(&project_dir).status().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    // -- validate_name: boundary (4 distinct rejection shapes, R7) --------

    #[test]
    fn validate_name_rejects_empty_string() {
        assert_eq!(validate_name(""), Err("invalid_name".to_string()));
    }

    #[test]
    fn validate_name_rejects_a_leading_hyphen() {
        assert_eq!(validate_name("-my-app"), Err("invalid_name".to_string()));
    }

    #[test]
    fn validate_name_rejects_uppercase_and_other_disallowed_characters() {
        assert_eq!(validate_name("My App"), Err("invalid_name".to_string()));
        assert_eq!(validate_name("한글"), Err("invalid_name".to_string()));
    }

    #[test]
    fn validate_name_rejects_over_63_characters() {
        let too_long: String = std::iter::repeat('a').take(64).collect();
        assert_eq!(validate_name(&too_long), Err("invalid_name".to_string()));
    }

    #[test]
    fn validate_name_accepts_the_boundary_63_character_name() {
        let max_len: String = std::iter::repeat('a').take(63).collect();
        assert!(validate_name(&max_len).is_ok(), "exactly 63 chars is the inclusive boundary, must be accepted");
    }

    // -- create_project_core: error (gh missing / unauthenticated, R6) ----

    #[test]
    fn create_project_errors_gh_missing_when_gh_is_not_installed() {
        let dir = TestDir::new("gh-missing");
        let err = create_project_core("my-app", &dir.0, false, false, |_, _| panic!("create_and_clone must not run when gh is missing"))
            .expect_err("gh missing must error before any gh call");
        assert_eq!(err, "gh_missing");
    }

    #[test]
    fn create_project_errors_gh_unauthenticated_when_installed_but_not_logged_in() {
        let dir = TestDir::new("gh-unauth");
        let err = create_project_core("my-app", &dir.0, true, false, |_, _| panic!("create_and_clone must not run when gh is unauthenticated"))
            .expect_err("unauthenticated gh must error before any gh call");
        assert_eq!(err, "gh_unauthenticated");
    }

    // -- create_project_core: error (name invalid short-circuits, R7) -----

    #[test]
    fn create_project_rejects_invalid_name_before_touching_gh_or_the_filesystem() {
        let dir = TestDir::new("invalid-name");
        let err = create_project_core("Invalid Name", &dir.0, true, true, |_, _| panic!("create_and_clone must not run for an invalid name"))
            .expect_err("invalid name must error first");
        assert_eq!(err, "invalid_name");
    }

    // -- create_project_core: error (project already exists locally) ------

    #[test]
    fn create_project_errors_project_exists_when_the_local_folder_is_already_there() {
        let dir = TestDir::new("exists");
        std::fs::create_dir_all(dir.0.join("my-app")).unwrap();

        let err = create_project_core("my-app", &dir.0, true, true, |_, _| panic!("create_and_clone must not run when the folder already exists"))
            .expect_err("an existing local folder must error before calling gh");
        assert_eq!(err, "project_exists");
    }

    // -- create_project_core: normal (full scaffold + commit flow, R4/R5) -

    #[test]
    fn create_project_scaffolds_artifacts_and_commits_on_a_successful_clone() {
        let dir = TestDir::new("success");

        let info = create_project_core("my-app", &dir.0, true, true, fake_clone_ok).expect("mocked gh success must produce a ProjectInfo");

        assert_eq!(info.name, "my-app");
        let project_path = PathBuf::from(&info.path);
        assert_eq!(project_path, dir.0.join("my-app"));

        let artifacts_dir = project_path.join(".crew").join("artifacts");
        assert!(artifacts_dir.join("README.md").is_file(), "README.md must be scaffolded");
        assert!(artifacts_dir.join(".gitkeep").is_file(), ".gitkeep must be scaffolded");
        let readme = std::fs::read_to_string(artifacts_dir.join("README.md")).unwrap();
        assert!(readme.contains("에이전트 공용 산출물 폴더"), "README must state the shared-artifacts convention");

        // The scaffold must actually be committed, not just written to disk.
        let log = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&project_path)
            .output()
            .expect("git log must run");
        let log_text = String::from_utf8_lossy(&log.stdout);
        assert!(log_text.contains("chore: crew scaffold"), "expected a scaffold commit, got: {log_text}");
    }

    // -- create_project_core: error (create_and_clone failure -> create_failed) --

    #[test]
    fn create_project_wraps_a_create_and_clone_failure_as_create_failed() {
        let dir = TestDir::new("create-fail");
        let err = create_project_core("my-app", &dir.0, true, true, |_, _| Err("boom".to_string()))
            .expect_err("a failing create_and_clone must surface as an error");
        assert!(err.starts_with("create_failed"), "expected a create_failed-prefixed error, got: {err}");
    }

    // -- create_project_core: error (clone reports success but folder is absent -> clone_failed) --

    #[test]
    fn create_project_errors_clone_failed_when_create_and_clone_reports_ok_but_leaves_no_folder() {
        let dir = TestDir::new("clone-fail");
        let err = create_project_core("my-app", &dir.0, true, true, |_, _| Ok(())).expect_err("a missing cloned folder must error, not silently succeed");
        assert_eq!(err, "clone_failed");
    }

    // -- real gh integration (plan tools_guidance: isolated, never run by default) --

    #[test]
    #[ignore = "creates a real private GitHub repo via the authenticated gh CLI; run deliberately, never in the default suite"]
    fn real_create_and_clone_against_the_actual_gh_cli() {
        let dir = TestDir::new("real-gh");
        let name = format!("linkly-crew-t3-test-{}", uuid::Uuid::new_v4());
        real_create_and_clone(&name, &dir.0).expect("a real gh repo create --clone should succeed when gh is installed and authenticated");
        assert!(dir.0.join(&name).is_dir());
    }
}
