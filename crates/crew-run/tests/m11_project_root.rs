//! M11 §I1/§I2 wiring proof: `RunConfig.project_root` validation at
//! `RunController::start`'s leading edge (contracts-m11.md §I1-§I3).
//!
//! Safety note (mirrors `m10_cmd_dod.rs`'s module doc): every `DodCheck::Cmd`
//! this file could reach is never emitted — `dev_cmd_checks` stays empty
//! (out of scope for this task) — so no real subprocess/cargo nesting risk
//! applies here. `mode` is always `Scripted`, so no real CLI/harness spawn
//! is involved either; only `RunController::start`'s own validation and
//! `role_cli_cwd` wiring are under test.
//!
//! The "both call sites see the same `project_root`" assertion (plan step 1
//! priority (a)) lives in `controller.rs`'s inline `role_cli_cwd_tests`
//! module instead of here: `role_cli_cwd` is a pure function, so asserting
//! there is deterministic and doesn't require spawning a run.

use std::path::PathBuf;

use crew_lead::accept::AcceptanceLoop;
use crew_run::{RunConfig, RunController, RunError, RunMode};

const GOAL: &str = "간단한 랜딩 페이지";

fn test_root(label: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap()
        .join(".crew-test")
        .join(format!("{label}-{}", uuid::Uuid::new_v4()))
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

fn config(data_dir: PathBuf, project_root: Option<PathBuf>) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations: vec![] },
        data_dir,
        max_rework: AcceptanceLoop::default_budget(),
        max_per_sprint: 0,
        escalation_timeout_ms: 0,
        roster: None,
        dev_cmd_checks: vec![],
        project_root,
        turn_timeout_secs: 900,
    }
}

fn run_git_ok(cwd: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git").arg("-C").arg(cwd).args(args).status().expect("test setup: git must be on PATH");
    assert!(status.success(), "test setup: `git {args:?}` failed in {cwd:?}");
}

fn init_test_repo(label: &str) -> PathBuf {
    let root = test_root(label);
    std::fs::create_dir_all(&root).expect("test setup: repo root");
    run_git_ok(&root, &["init", "-q"]);
    run_git_ok(&root, &["config", "user.email", "test@example.com"]);
    run_git_ok(&root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("README.md"), b"init").expect("test setup: seed file");
    run_git_ok(&root, &["add", "."]);
    run_git_ok(&root, &["commit", "-q", "-m", "init"]);
    root
}

async fn assert_rejected(project_root: PathBuf, needle: &str) {
    let data_dir = test_root("project-root-reject");
    let cfg = config(data_dir.clone(), Some(project_root));

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::ProjectRootInvalid(msg)) => {
            assert!(
                msg.contains(needle),
                "error message must contain the received value verbatim: msg={msg:?}, needle={needle:?}"
            );
        }
        Err(other) => panic!("expected ProjectRootInvalid, got a different RunError: {other}"),
        Ok(_) => panic!("start must reject this project_root, not succeed"),
    }
    assert!(
        !data_dir.exists(),
        "a rejected project_root must leave zero partial run state (no data_dir side effects): {data_dir:?}"
    );
    cleanup(&data_dir);
}

/// Reject 1/5 (D10): a relative path in two segments.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_relative_path_two_segments() {
    assert_rejected(PathBuf::from("relative/path"), "relative/path").await;
}

/// Reject 2/5 (D10): a bare single-segment relative path.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_relative_path_single_segment() {
    assert_rejected(PathBuf::from("x"), "x").await;
}

/// Reject 3/5 (D10): a dot-relative path.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_relative_path_dot_form() {
    assert_rejected(PathBuf::from("./data/x"), "./data/x").await;
}

/// Reject 4/5 (D10): an absolute path that does not exist.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_absolute_path_that_does_not_exist() {
    let missing = test_root("project-root-missing").join("does-not-exist");
    assert!(missing.is_absolute(), "test setup: must be absolute");
    assert!(!missing.exists(), "test setup: must not pre-exist");
    let needle = missing.to_string_lossy().to_string();
    assert_rejected(missing, &needle).await;
}

/// Reject 5/5 (D10): an absolute path that resolves to a regular file, not
/// a directory.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_absolute_path_that_is_a_file() {
    let dir = test_root("project-root-file-parent");
    std::fs::create_dir_all(&dir).expect("test setup: create parent dir");
    let file = dir.join("not-a-directory.txt");
    std::fs::write(&file, b"not a directory").expect("test setup: write file");
    let needle = file.to_string_lossy().to_string();

    assert_rejected(file, &needle).await;

    cleanup(&dir);
}

/// Boundary (review r1 F1, plan D10 gap): an empty path is neither absolute
/// nor a real directory — `is_absolute()` is the check that must catch it
/// first. An empty string is a substring of every message, so `needle`-style
/// verbatim matching can't prove anything here; this asserts the specific
/// validation message text instead, pinning the rejection to the
/// `is_absolute()` branch rather than some other path through the
/// validation.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_empty_path() {
    let data_dir = test_root("project-root-reject-empty");
    let cfg = config(data_dir.clone(), Some(PathBuf::from("")));

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::ProjectRootInvalid(msg)) => {
            assert!(
                msg.contains("must be an absolute path"),
                "an empty path must be rejected by the is_absolute() check: msg={msg:?}"
            );
        }
        Err(other) => panic!("expected ProjectRootInvalid, got a different RunError: {other}"),
        Ok(_) => panic!("start must reject an empty project_root, not succeed"),
    }
    assert!(
        !data_dir.exists(),
        "a rejected project_root must leave zero partial run state (no data_dir side effects): {data_dir:?}"
    );
    cleanup(&data_dir);
}

/// Boundary (review r2 F2): a relative path that happens to name a real
/// directory at the process cwd (`crates/crew-run`, cargo's integration-test
/// cwd) must still be rejected by the `is_absolute()` check — not
/// incidentally caught later by `is_dir()` failing on a nonexistent path, the
/// way `rejects_relative_path_*` above are. Per
/// infrastructure-config-path-valued-config's edge case ("The relative path
/// happens to work in staging → treat a passing relative path as an
/// accident, not as validation"), a relative path that resolves must be
/// refused for resolving relative to an unspecified cwd, not accepted
/// because it happened to exist there. The premise (`"src"` is a real
/// directory here) is asserted explicitly so a premise break fails loudly
/// instead of the test silently testing nothing.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_real_relative_path_at_the_process_cwd() {
    let relative = PathBuf::from("src");
    assert!(
        relative.is_dir(),
        "test premise: \"src\" must be a real directory at the process cwd (crates/crew-run) for this test to prove anything"
    );
    let data_dir = test_root("project-root-reject-real-relative");
    let cfg = config(data_dir.clone(), Some(relative));

    let result = RunController::start(cfg).await;

    match result {
        Err(RunError::ProjectRootInvalid(msg)) => {
            assert!(
                msg.contains("must be an absolute path"),
                "a relative path that exists at the process cwd must still be rejected by the \
                 is_absolute() branch specifically, not merely rejected by some other check: msg={msg:?}"
            );
        }
        Err(other) => panic!("expected ProjectRootInvalid, got a different RunError: {other}"),
        Ok(_) => panic!("start must reject a relative project_root even when it resolves to a real directory at the process cwd"),
    }
    assert!(
        !data_dir.exists(),
        "a rejected project_root must leave zero partial run state (no data_dir side effects): {data_dir:?}"
    );
    cleanup(&data_dir);
}

/// R6 / design D5 (HANDOFF pitfall 30 fix): a real, existing, absolute
/// directory that is **not** a git repository is now rejected — every role
/// needs its own `git worktree` under `project_root`, so a plain directory
/// can no longer be accepted the way pre-this-task M11 accepted it (the old
/// `accepts_a_real_absolute_directory` test this replaces asserted exactly
/// that now-superseded acceptance). No fallback to a shared cwd.
#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_real_absolute_directory_that_is_not_a_git_repo() {
    let data_dir = test_root("project-root-non-git-data");
    let root = test_root("project-root-non-git-root");
    std::fs::create_dir_all(&root).expect("test setup: create a real, plain (non-git) directory");

    let cfg = config(data_dir.clone(), Some(root.clone()));
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), RunController::start(cfg))
        .await
        .expect("start must finish within the deterministic budget");

    match result {
        Err(RunError::ProjectRootInvalid(msg)) => {
            assert!(msg.contains("git"), "the rejection must explain the root isn't a git repo: msg={msg:?}");
        }
        Err(other) => panic!("expected ProjectRootInvalid, got a different RunError: {other}"),
        Ok(_) => panic!("start must reject a non-git project_root now that every role needs a git worktree under it"),
    }
    assert!(
        !data_dir.exists(),
        "a rejected project_root must leave zero partial run state (no data_dir side effects): {data_dir:?}"
    );

    cleanup(&data_dir);
    cleanup(&root);
}

/// Boundary/unchanged (D10): `project_root: None` leaves `start` behaving
/// exactly as before M11 — no validation applies, `start` succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn none_project_root_leaves_start_unchanged() {
    let data_dir = test_root("project-root-none");

    let cfg = config(data_dir.clone(), None);
    let handle = tokio::time::timeout(std::time::Duration::from_secs(30), RunController::start(cfg))
        .await
        .expect("start must finish within the deterministic budget")
        .expect("start must succeed when project_root is None, exactly as before M11");

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// R2(ii)/R5, plan-reviewer call-1 FAIL fix: proves the `ProjectRootInvalid`
/// message's suggested fix actually works, not just that it reads
/// plausibly -- reject on issue #16's exact `.crew/` rule, then apply the
/// message's own suggested `.crew/*` + `!.crew/artifacts` form and assert
/// `start` now succeeds. This is the ONE test in this file allowed to let
/// `start` reach `resolve_role_worktrees` on a successful path -- doing so
/// creates real out-of-repo worktrees under `$HOME`
/// (`worktree::project_worktrees_base` has no test-injection seam
/// reachable from `start`'s public API, and it is not `pub` from this
/// external integration-test crate to compute/clean up directly) -- this
/// is a documented, singular, deliberate exception (plan D8): these
/// worktrees are persistent-by-design and idempotent (`worktree.rs`'s own
/// module doc, `docs/DESIGN.md`'s M12 "실행 cwd" bullet), not test
/// pollution requiring teardown. `handle.shutdown().await` still cleans
/// up this run's in-process state.
#[tokio::test(flavor = "multi_thread")]
async fn suggested_fix_in_the_rejection_message_actually_un_ignores_the_path() {
    let repo = init_test_repo("artifacts-ignore-fix-proof");
    std::fs::write(repo.join(".gitignore"), b".crew/\n").expect("test setup: write the issue's exact ignore rule");
    run_git_ok(&repo, &["add", ".gitignore"]);
    run_git_ok(&repo, &["commit", "-q", "-m", "add crew ignore rule"]);

    let reject_data_dir = test_root("artifacts-ignore-fix-proof-reject-data");
    let reject_result = RunController::start(config(reject_data_dir.clone(), Some(repo.clone()))).await;
    match reject_result {
        Err(RunError::ProjectRootInvalid(msg)) => {
            assert!(msg.contains(".crew/artifacts"), "message must name the ignored path: {msg}");
            assert!(msg.contains("remove the ignore rule"), "message must name the fix: {msg}");
        }
        Err(other) => panic!("expected ProjectRootInvalid before the fix is applied, got a different RunError: {other}"),
        Ok(_) => panic!("start must reject before the fix is applied, not succeed"),
    }
    assert!(!reject_data_dir.exists(), "a rejected project_root must leave zero partial run state: {reject_data_dir:?}");

    // Apply the message's own suggested fix, verbatim -- this is the
    // assertion that actually proves the advice, not the substring checks
    // above.
    std::fs::write(repo.join(".gitignore"), b".crew/*\n!.crew/artifacts\n").expect("test: apply the suggested fix");

    let accept_data_dir = test_root("artifacts-ignore-fix-proof-accept-data");
    let accept_result = RunController::start(config(accept_data_dir.clone(), Some(repo.clone()))).await;
    let handle = accept_result.expect("start must succeed once the suggested fix is applied -- this is the proof the message's advice actually works");
    handle.shutdown().await;

    let _ = std::fs::remove_dir_all(&repo);
    cleanup(&reject_data_dir);
    cleanup(&accept_data_dir);
}
