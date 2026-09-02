//! Role-scoped git worktrees for `RunConfig.project_root` (design D1/D5,
//! HANDOFF pitfall 30): every role gets its own out-of-repo `git worktree`
//! checked out onto a `crew/<role>` branch, so the agent CLI cwd and the
//! Cmd DoD exec cwd never share a directory across roles — the bug the
//! pre-fix `role_cli_cwd` had (returning `project_root` verbatim for every
//! role).
//!
//! In-repo placement (e.g. `<root>/.crew/worktrees/<role>`) was rejected by
//! plan review (r1): `.crew/artifacts` is committed content with no
//! `.gitignore` entry, so a nested worktree under the tracked tree corrupts
//! it (embedded-repo warnings, `git status` pollution in the main
//! checkout) — reproduced locally by the reviewer.

use std::path::{Path, PathBuf};
use std::process::Command;

use crew_proto::Role;

use crate::controller::role_dir_name;

/// Production default for `ensure_role_worktree`'s `worktrees_base` (design
/// D1): `~/.linkly-crew/projects/<basename>-<fnv1a-hash8hex of the absolute
/// project_root>/worktrees`. The hash disambiguates two different projects
/// that happen to share a basename; tests inject their own tempdir base
/// instead of calling this (tools_guidance: never touch `$HOME` from a
/// worktree unit test).
pub(crate) fn project_worktrees_base(project_root: &Path) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME must be set on this platform");
    let basename = project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    let hash = fnv1a_hex8(project_root.to_string_lossy().as_bytes());
    Path::new(&home)
        .join(".linkly-crew")
        .join("projects")
        .join(format!("{basename}-{hash}"))
        .join("worktrees")
}

/// FNV-1a, 32-bit, hex-encoded — a small, dependency-free hash good enough
/// to disambiguate `project_worktrees_base` directory names (not a security
/// boundary).
fn fnv1a_hex8(bytes: &[u8]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

/// Ensures `role`'s worktree exists under `worktrees_base` for the git repo
/// at `project_root`, creating it (`git worktree add -B crew/<role>`, base =
/// `project_root`'s current `HEAD`) if it isn't already there, or reusing it
/// idempotently if `git worktree list` already registers it. `project_root`
/// not being a git repository, or the `git worktree add` invocation itself
/// failing, is returned as `Err` — no fallback to a shared cwd (design D5).
pub(crate) fn ensure_role_worktree(project_root: &Path, worktrees_base: &Path, role: Role) -> Result<PathBuf, String> {
    let canonical_root = std::fs::canonicalize(project_root)
        .map_err(|e| format!("project_root_not_a_git_repo: {}: {e}", project_root.display()))?;
    // `rev-parse --show-toplevel`, not `--git-dir`: git commands search
    // *upward* through parent directories for a `.git`, so a plain
    // subdirectory nested inside some unrelated repo (e.g. a tempdir under
    // this very crate's own checkout) would otherwise be misreported as
    // "a git repo" — comparing the toplevel to `project_root` itself
    // catches that case too, matching design D1's in-repo-placement
    // rejection: `project_root` must be a repo's own root, not merely
    // somewhere inside one.
    let toplevel_output = Command::new("git")
        .arg("-C")
        .arg(&canonical_root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| format!("project_root_not_a_git_repo: {}: failed to run git: {e}", project_root.display()))?;
    if !toplevel_output.status.success() {
        return Err(format!(
            "project_root_not_a_git_repo: {}: {}",
            project_root.display(),
            String::from_utf8_lossy(&toplevel_output.stderr).trim()
        ));
    }
    let toplevel = PathBuf::from(String::from_utf8_lossy(&toplevel_output.stdout).trim());
    let canonical_toplevel = std::fs::canonicalize(&toplevel)
        .map_err(|e| format!("project_root_not_a_git_repo: {}: could not resolve git toplevel {toplevel:?}: {e}", project_root.display()))?;
    if canonical_toplevel != canonical_root {
        return Err(format!(
            "project_root_not_a_git_repo: {} is not the top level of a git working tree (found {} instead)",
            project_root.display(),
            toplevel.display()
        ));
    }

    std::fs::create_dir_all(worktrees_base)
        .map_err(|e| format!("failed to create worktrees_base {worktrees_base:?}: {e}"))?;
    let base = std::fs::canonicalize(worktrees_base)
        .map_err(|e| format!("failed to resolve worktrees_base {worktrees_base:?}: {e}"))?;
    let role_path = base.join(role_dir_name(role));

    if role_path.is_dir() && worktree_is_registered(&canonical_root, &role_path)? {
        return Ok(role_path);
    }

    let branch = format!("crew/{}", role_dir_name(role));
    let (added, stderr) = run_git(
        &canonical_root,
        &["worktree", "add", "-B", &branch, &role_path.to_string_lossy()],
    )?;
    if !added {
        return Err(format!(
            "git worktree add failed for role {role:?} at {}: {}",
            role_path.display(),
            stderr.trim()
        ));
    }
    Ok(role_path)
}

/// Whether `git -C project_root worktree list` already registers a worktree
/// at exactly `role_path` (idempotent-reuse check, design D1). `role_path`
/// is always canonicalized by the caller before this runs, matching the
/// canonicalized path `ensure_role_worktree` itself passed to the earlier
/// `git worktree add` — git records that same canonical form, so a plain
/// string comparison (no re-canonicalization of git's own output) is
/// reliable.
fn worktree_is_registered(project_root: &Path, role_path: &Path) -> Result<bool, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|e| format!("failed to run git worktree list: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let target = role_path.to_string_lossy();
    Ok(stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|p| p == target))
}

/// Runs `git -C cwd <args>`, returning `(exit_success, stderr)`. A failure
/// to even spawn `git` (not on `PATH`) is itself an `Err`, distinct from git
/// running and reporting a non-zero exit.
fn run_git(cwd: &Path, args: &[&str]) -> Result<(bool, String), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git {args:?}: {e}"))?;
    Ok((output.status.success(), String::from_utf8_lossy(&output.stderr).into_owned()))
}

#[cfg(test)]
mod tests {
    //! Real `git` subprocess coverage (tools_guidance: `--ignored` real-CLI
    //! tests stay out of scope, but this is plain `git`, not an agent CLI —
    //! fast and deterministic). Every repo/`worktrees_base` lives under a
    //! `.crew-test/` tempdir at the crate's own cwd (existing repo
    //! convention — see `controller.rs`'s `role_cli_cwd_tests`), never
    //! `/tmp`/`$TMPDIR` and never real `$HOME`.

    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join(".crew-test")
            .join(format!("{label}-{}", uuid::Uuid::new_v4()))
    }

    fn run_git_ok(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .expect("test setup: git must be on PATH");
        assert!(status.success(), "test setup: `git {args:?}` failed in {cwd:?}");
    }

    fn init_test_repo(label: &str) -> PathBuf {
        let root = test_dir(label);
        std::fs::create_dir_all(&root).expect("test setup: create repo root");
        run_git_ok(&root, &["init", "-q"]);
        run_git_ok(&root, &["config", "user.email", "test@example.com"]);
        run_git_ok(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("README.md"), b"init").expect("test setup: seed file");
        run_git_ok(&root, &["add", "."]);
        run_git_ok(&root, &["commit", "-q", "-m", "init"]);
        root
    }

    fn current_branch(worktree_path: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(worktree_path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("test: git rev-parse must run");
        assert!(output.status.success(), "test: git rev-parse failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn main_checkout_status_is_clean(repo: &Path) -> bool {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["status", "--porcelain"])
            .output()
            .expect("test: git status must run");
        assert!(output.status.success(), "test: git status failed");
        output.stdout.is_empty()
    }

    /// Normal: a fresh role worktree is created out-of-repo, on its own
    /// `crew/<role>` branch.
    #[test]
    fn creates_a_role_worktree_on_the_role_branch() {
        let repo = init_test_repo("wt-create-repo");
        let base = test_dir("wt-create-base");

        let path = ensure_role_worktree(&repo, &base, Role::Developer)
            .expect("worktree creation must succeed for a real git repo");

        assert!(path.is_dir(), "worktree path must exist: {path:?}");
        assert!(path.join(".git").is_file(), "a linked worktree has a .git file, not a directory: {path:?}");
        assert!(!path.starts_with(&repo), "the worktree must live outside the repo (design D1): {path:?}");
        assert_eq!(current_branch(&path), "crew/developer");

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Boundary: calling it again for the same role returns the same path
    /// without erroring or creating a second worktree registration.
    #[test]
    fn reuses_an_already_registered_worktree_idempotently() {
        let repo = init_test_repo("wt-idem-repo");
        let base = test_dir("wt-idem-base");

        let first = ensure_role_worktree(&repo, &base, Role::Qa).expect("first call must succeed");
        let second = ensure_role_worktree(&repo, &base, Role::Qa).expect("second call must succeed and reuse");

        assert_eq!(first, second, "a repeat call for the same role must return the same path");
        assert!(second.is_dir(), "the reused worktree must still exist: {second:?}");

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Error: a plain directory that is not a git repository is refused, no
    /// fallback (design D5).
    #[test]
    fn errors_when_project_root_is_not_a_git_repo() {
        let non_repo = test_dir("wt-non-git-root");
        std::fs::create_dir_all(&non_repo).expect("test setup: plain (non-git) directory");
        let base = test_dir("wt-non-git-base");

        let result = ensure_role_worktree(&non_repo, &base, Role::Pm);

        let err = result.expect_err("a non-git project_root must be rejected");
        assert!(err.contains("not_a_git_repo"), "error must name the git-repo problem: {err}");
        assert!(!base.join("pm").exists(), "no worktree may be created for a rejected project_root: {base:?}");

        let _ = std::fs::remove_dir_all(&non_repo);
    }

    /// Boundary (design D1 regression guard): two different roles get two
    /// different worktrees, and creating them leaves the main checkout's
    /// `git status` clean — the in-repo placement this design rejected
    /// (r1) polluted exactly this.
    #[test]
    fn different_roles_get_different_worktrees_and_main_checkout_stays_clean() {
        let repo = init_test_repo("wt-multi-role-repo");
        let base = test_dir("wt-multi-role-base");

        let dev = ensure_role_worktree(&repo, &base, Role::Developer).expect("developer worktree");
        let qa = ensure_role_worktree(&repo, &base, Role::Qa).expect("qa worktree");

        assert_ne!(dev, qa, "different roles must not share a worktree (pitfall 30)");
        assert!(
            main_checkout_status_is_clean(&repo),
            "creating role worktrees must not dirty the main checkout's status"
        );

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `project_worktrees_base` itself: deterministic per `project_root`,
    /// distinct across different roots, and always under `$HOME` — never
    /// `/tmp`/`$TMPDIR`. No filesystem writes happen here (pure path
    /// construction), so this is safe to run without touching real `$HOME`
    /// on disk.
    #[test]
    fn project_worktrees_base_is_deterministic_and_distinguishes_projects() {
        let a = test_dir("base-project-a");
        let b = test_dir("base-project-b");

        let base_a1 = project_worktrees_base(&a);
        let base_a2 = project_worktrees_base(&a);
        let base_b = project_worktrees_base(&b);

        assert_eq!(base_a1, base_a2, "the same project_root must hash to the same worktrees_base");
        assert_ne!(base_a1, base_b, "different project_roots must not collide");
        let home = std::env::var("HOME").expect("test setup: HOME must be set");
        assert!(base_a1.starts_with(&home), "must live under $HOME, never /tmp: {base_a1:?}");
        assert!(
            !base_a1.starts_with(std::env::temp_dir()),
            "must not live under the system temp dir: {base_a1:?}"
        );
    }
}
