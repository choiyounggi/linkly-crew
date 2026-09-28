//! The run's git flow (issue #31c, t7 design D2-D23): commit each accepted
//! task on its role branch, merge an accepted sprint's role branches into the
//! user's main branch, push main to `origin` when the run completed, and gate
//! `Completed` on an executed-and-passed DoD check.
//!
//! Every git command goes through one bounded, non-interactive, hook-free
//! runner (`run_git_bounded`, D6/D7). Nothing here runs when `project_root`
//! is `None`: `GitFlowCtx::for_run` returns `None` without spawning anything
//! (D12). Never issued anywhere by this module: a force push, `worktree add
//! -B`, `branch -f/-d/-D`, reset, checkout, restore, stash, clean, `merge -X`
//! or an ours/theirs resolution (design "Every git command the run issues").

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crew_lead::browser_exec::parse_browser_expect;
use crew_lead::cmd_exec::parse_expect;
use crew_lead::dispatch::TaskState;
use crew_proto::{DodCheck, Role, TaskDag, TaskSpec};
use tokio::sync::broadcast;

use crate::controller::{role_dir_name, ROLE_ORDER};
use crate::events::{now_ts, GitFlowKindDto, RunEvent};

/// Bound on every local git command (D7).
pub(crate) const GIT_LOCAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Bound on the push and the pre-push history scan (D7/D9).
pub(crate) const PUSH_TIMEOUT: Duration = Duration::from_secs(120);
/// Cap on the pre-push history scan's output (D9).
pub(crate) const SCAN_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Cap on one commit's added content (D14).
pub(crate) const GUARD_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// Upper bound on `RunHandle::shutdown_and_wait`'s settle (D23).
pub(crate) const SETTLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Directory names never staged by a flow commit (D14). A path is excluded
/// when one of its DIRECTORY components (any component but the last) is in
/// this list; a plain file named `dist` is staged.
const EXCLUDED_DIRS: [&str; 9] = ["target", "node_modules", "dist", "build", ".next", "out", "coverage", ".venv", ".claude"];

/// Inherited variables that would redirect a flow command to another
/// repository, index, object store or config, or run an external diff
/// program (review r1 F2/N6); removed from every flow command.
const INHERITED_GIT_ENV: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXTERNAL_DIFF",
];

/// The flow's fallback identity (D15), used only for a key the user's own
/// config does not set.
const FALLBACK_NAME: &str = "user.name=linkly-crew";
const FALLBACK_EMAIL: &str = "user.email=crew@linkly-crew.invalid";

// --- the bounded runner (D7) ------------------------------------------------

/// One finished git (or test) process.
#[derive(Debug)]
pub(crate) struct GitOut {
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitOut {
    fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// Runs `git -C <cwd> -c core.hooksPath=/dev/null -c core.fsmonitor=false
/// -c commit.gpgsign=false -c tag.gpgSign=false <args>` in its own process
/// group, stdin detached, never prompting, bounded by `deadline` (D6/D7).
/// Neither a hook nor a configured fsmonitor program runs, and no inherited
/// `GIT_*` variable can point the command at another repository, index or
/// config (review r1 F2/N6). `max_stdout` caps stdout: exceeding it kills
/// the group and is an `Err`.
pub(crate) fn run_git_bounded(
    cwd: &Path,
    args: &[&str],
    deadline: Duration,
    extra_env: &[(String, String)],
    max_stdout: Option<usize>,
) -> Result<GitOut, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args([
            "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "commit.gpgsign=false", "-c",
            "tag.gpgSign=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_MERGE_AUTOEDIT", "no");
    for key in INHERITED_GIT_ENV {
        cmd.env_remove(key);
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    // Name the subcommand in errors, skipping `-c key=value` pairs.
    let mut rest = args.iter();
    let mut subcommand = "";
    while let Some(arg) = rest.next() {
        if *arg == "-c" {
            rest.next();
        } else {
            subcommand = arg;
            break;
        }
    }
    run_bounded(cmd, deadline, max_stdout).map_err(|e| format!("git {subcommand}: {e}"))
}

/// The process half of `run_git_bounded`, separate so the group-kill and
/// detached-pipe behaviour is testable with plain `sh`/`perl` commands.
fn run_bounded(mut cmd: Command, deadline: Duration, max_stdout: Option<usize>) -> Result<GitOut, String> {
    let started = Instant::now();
    cmd.process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn: {e}"))?;
    let pgid = child.id();
    let overflow = Arc::new(AtomicBool::new(false));

    // Two reader threads so neither pipe can fill and stall the child; each
    // reports over the channel and is abandoned (never joined) if it cannot
    // finish within the bound.
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let out_tx = tx.clone();
    let out_overflow = overflow.clone();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if max_stdout.is_some_and(|cap| buf.len() > cap) {
                        out_overflow.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
        }
        let _ = out_tx.send((true, buf));
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = tx.send((false, buf));
    });

    let over_cap = |cap: Option<usize>| format!("output exceeded {} bytes", cap.unwrap_or(0));
    let status = loop {
        if overflow.load(Ordering::SeqCst) {
            kill_group(pgid);
            let _ = child.wait();
            return Err(over_cap(max_stdout));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_group(pgid);
                let _ = child.wait();
                return Err(format!("failed to wait: {e}"));
            }
        }
        if started.elapsed() >= deadline {
            kill_group(pgid);
            let _ = child.wait();
            return Err(format!("timed out after {}s", deadline.as_secs_f64()));
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let mut out = None;
    let mut err = None;
    while out.is_none() || err.is_none() {
        let wait = deadline.saturating_sub(started.elapsed()).max(Duration::from_secs(1));
        match rx.recv_timeout(wait) {
            Ok((true, buf)) => out = Some(buf),
            Ok((false, buf)) => err = Some(buf),
            Err(_) => return Err("output reader did not finish".to_string()),
        }
    }
    if overflow.load(Ordering::SeqCst) {
        return Err(over_cap(max_stdout));
    }
    Ok(GitOut {
        ok: status.success(),
        code: status.code(),
        stdout: out.unwrap_or_default(),
        stderr: String::from_utf8_lossy(&err.unwrap_or_default()).trim().to_string(),
    })
}

/// Kills the whole process group via `/bin/kill` (crew-run has no libc
/// dependency).
fn kill_group(pgid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pgid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// --- the per-run context (D12/D13/D19) --------------------------------------

/// Everything the flow needs for one run with a `project_root`. Built once in
/// `RunController::start`; `None` there means the run issues no git command.
pub(crate) struct GitFlowCtx {
    root: PathBuf,
    role_worktrees: Vec<(Role, PathBuf)>,
    main_branch: Option<String>,
    leftover_dirty: Vec<Role>,
    browser_wired: bool,
    git_calls: AtomicUsize,
    cancelled: AtomicBool,
    /// Set when a `merge --abort` could not be verified (D5); stays set for
    /// the rest of the run.
    merges_blocked: AtomicBool,
    /// Roles whose worktree `.git` pointer does not belong to this repository
    /// (review r1 F2): (role, error detail, already reported). No git command
    /// runs against such a worktree and the role is neither committed nor
    /// merged for the rest of the run.
    excluded: std::sync::Mutex<Vec<(Role, String, bool)>>,
    /// Held across every blocking git phase (D19) so shutdown can wait for an
    /// in-flight phase before releasing the run lock.
    pub(crate) phase: Arc<tokio::sync::Mutex<()>>,
    #[cfg(test)]
    extra_env: Vec<(String, String)>,
    /// Test-only failure injection (review r4): after a real `merge
    /// --abort`, put MERGE_HEAD back, so the abort cannot be verified.
    #[cfg(test)]
    abort_leaves_merge_head: bool,
}

/// What one sprint's merge phase did (D4/D5).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MergeReport {
    /// Error events and real merge Conflict events this phase emitted. A
    /// merge refused because it would overwrite an untracked or ignored file
    /// (review r1 F1) is a Conflict event counted in `not_landed` instead.
    pub failures: usize,
    pub merged_any: bool,
    /// Set when a sprint-level skip left role work that was ahead of main
    /// unmerged (D4 landing rule).
    pub not_landed: Option<String>,
}

impl GitFlowCtx {
    /// `None` (and no git command) unless both `project_root` and the role
    /// worktrees are present (D12); otherwise resolves the main branch once
    /// (D13) and records role worktrees that were already dirty (D14).
    pub(crate) fn for_run(
        project_root: Option<&Path>,
        role_worktrees: Option<&[(Role, PathBuf)]>,
        browser_wired: bool,
    ) -> Option<GitFlowCtx> {
        let (root, role_worktrees) = (project_root?, role_worktrees?);
        let mut ctx = GitFlowCtx {
            root: root.to_path_buf(),
            role_worktrees: role_worktrees.to_vec(),
            main_branch: None,
            leftover_dirty: Vec::new(),
            browser_wired,
            git_calls: AtomicUsize::new(0),
            cancelled: AtomicBool::new(false),
            merges_blocked: AtomicBool::new(false),
            excluded: std::sync::Mutex::new(Vec::new()),
            phase: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            extra_env: Vec::new(),
            #[cfg(test)]
            abort_leaves_merge_head: false,
        };
        ctx.main_branch = ctx.resolve_main_branch();
        let mut leftover_dirty = Vec::new();
        for (role, wt) in &ctx.role_worktrees {
            // Filesystem-only check BEFORE the first git command against the
            // worktree: a redirected `.git` would hand its config to git.
            if ctx.trusted_worktree(*role, wt).is_err() {
                continue;
            }
            if ctx
                .git(wt, &["status", "--porcelain"], GIT_LOCAL_TIMEOUT)
                .is_ok_and(|out| out.ok && !out.stdout.is_empty())
            {
                leftover_dirty.push(*role);
            }
        }
        ctx.leftover_dirty = leftover_dirty;
        Some(ctx)
    }

    pub(crate) fn git_calls(&self) -> usize {
        self.git_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) fn browser_wired(&self) -> bool {
        self.browser_wired
    }

    pub(crate) fn main_branch(&self) -> Option<&str> {
        self.main_branch.as_deref()
    }

    fn git(&self, cwd: &Path, args: &[&str], deadline: Duration) -> Result<GitOut, String> {
        self.git_with(cwd, args, deadline, &[], None)
    }

    fn git_with(
        &self,
        cwd: &Path,
        args: &[&str],
        deadline: Duration,
        env: &[(String, String)],
        max_stdout: Option<usize>,
    ) -> Result<GitOut, String> {
        self.git_calls.fetch_add(1, Ordering::SeqCst);
        #[allow(unused_mut)]
        let mut all_env = env.to_vec();
        #[cfg(test)]
        all_env.extend(self.extra_env.iter().cloned());
        run_git_bounded(cwd, args, deadline, &all_env, max_stdout)
    }

    /// D13: origin/HEAD's branch when it exists locally, else `main`, else
    /// `master`, else `None`.
    fn resolve_main_branch(&self) -> Option<String> {
        let local = |name: &str| {
            self.git(&self.root, &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")], GIT_LOCAL_TIMEOUT)
                .is_ok_and(|out| out.ok)
        };
        if let Ok(out) = self.git(
            &self.root,
            &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"],
            GIT_LOCAL_TIMEOUT,
        ) {
            let short = out.stdout_str();
            let name = short.strip_prefix("origin/").unwrap_or(&short);
            if out.ok && !name.is_empty() && local(name) {
                return Some(name.to_string());
            }
        }
        ["main", "master"].into_iter().find(|name| local(name)).map(str::to_string)
    }

    /// Review r1 F2: Ok when `wt`'s `.git` pointer belongs to this repository
    /// and the role was not excluded earlier; otherwise the role is (or
    /// stays) excluded and the error detail is returned. Runs no git command.
    fn trusted_worktree(&self, role: Role, wt: &Path) -> Result<(), String> {
        let mut excluded = self.excluded.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, detail, _)) = excluded.iter().find(|(r, _, _)| *r == role) {
            return Err(detail.clone());
        }
        let root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        role_worktree_is_ours(&root, wt).map_err(|why| {
            let detail = format!("role worktree {} is not a worktree of this repository: {why}", wt.display());
            excluded.push((role, detail.clone(), false));
            detail
        })
    }

    fn is_excluded(&self, role: Role) -> bool {
        self.excluded.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|(r, _, _)| *r == role)
    }

    /// Marks `role`'s exclusion as reported (its Error event was sent).
    fn mark_reported(&self, role: Role) {
        for entry in self.excluded.lock().unwrap_or_else(|p| p.into_inner()).iter_mut() {
            if entry.0 == role {
                entry.2 = true;
            }
        }
    }

    /// One Error event per excluded role not yet reported; returns how many.
    fn report_excluded(&self, tx: &broadcast::Sender<RunEvent>) -> usize {
        let mut excluded = self.excluded.lock().unwrap_or_else(|p| p.into_inner());
        let mut sent = 0;
        for (role, detail, reported) in excluded.iter_mut().filter(|(_, _, reported)| !*reported) {
            let branch = format!("crew/{}", role_dir_name(*role));
            let _ = tx.send(event(GitFlowKindDto::Error, Some(*role), Some(branch), None, None, detail.clone()));
            *reported = true;
            sent += 1;
        }
        sent
    }

    /// Review r1 F1: paths a merge of `branch` may write that exist on disk
    /// in the main checkout without being tracked there (untracked or
    /// ignored user files git would silently overwrite). Examined: every
    /// changed path; every ancestor of it that exists as a non-directory (a
    /// file or symlink standing where the branch adds a directory); and
    /// (review r3) every sibling named `<name>~…` of a changed path or of any
    /// of its ancestors — where ort moves a side aside on a file/directory or
    /// file-type conflict (`<path>~HEAD`, `<path>~<branch>`, `…_<n>`).
    fn untracked_collisions(&self, main: &str, branch: &str) -> Result<Vec<String>, String> {
        let root = &self.root;
        let (main_ref, branch_ref) = (format!("refs/heads/{main}"), format!("refs/heads/{branch}"));
        let base = self.git(root, &["merge-base", &main_ref, &branch_ref], GIT_LOCAL_TIMEOUT)?;
        if !base.ok {
            return Err(format!("git merge-base {main} {branch} failed: {}", base.stderr));
        }
        let range = format!("{}..{branch_ref}", base.stdout_str());
        let changed = self.git(root, &["diff", "--name-only", "-z", "--no-renames", &range], GIT_LOCAL_TIMEOUT)?;
        if !changed.ok {
            return Err(format!("git diff --name-only {range} failed: {}", changed.stderr));
        }
        let tracked = self.git(root, &["ls-files", "-z"], GIT_LOCAL_TIMEOUT)?;
        if !tracked.ok {
            return Err(format!("git ls-files failed in the main checkout: {}", tracked.stderr));
        }
        let tracked: std::collections::HashSet<String> = split_z(&tracked.stdout).collect();
        let mut collisions: Vec<String> = Vec::new();
        for path in split_z(&changed.stdout) {
            let ancestors: Vec<String> = path.match_indices('/').map(|(at, _)| path[..at].to_string()).collect();
            let mut candidates: Vec<String> = ancestors
                .iter()
                .filter(|ancestor| std::fs::symlink_metadata(root.join(ancestor)).is_ok_and(|m| !m.is_dir()))
                .cloned()
                .collect();
            for name in std::iter::once(&path).chain(&ancestors) {
                candidates.extend(relocation_siblings(root, name));
            }
            if std::fs::symlink_metadata(root.join(&path)).is_ok() {
                candidates.push(path);
            }
            for candidate in candidates {
                if !tracked.contains(&candidate) && !collisions.contains(&candidate) {
                    collisions.push(candidate);
                }
            }
        }
        collisions.sort();
        Ok(collisions)
    }

    /// `git config --get <key>` in `cwd`: Ok(true) when set, Ok(false) when
    /// unset (exit 1), Err otherwise (D15).
    fn has_config(&self, cwd: &Path, key: &str) -> Result<bool, String> {
        let out = self.git(cwd, &["config", "--get", key], GIT_LOCAL_TIMEOUT)?;
        match out.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(format!("git config --get {key} failed: {}", out.stderr)),
        }
    }

    fn identity_args(&self, cwd: &Path) -> Result<Vec<&'static str>, String> {
        Ok(identity_overrides(self.has_config(cwd, "user.name")?, self.has_config(cwd, "user.email")?))
    }

    /// Commits one Accepted task's work on `crew/<role>` (D14/D15) and emits
    /// exactly one GitFlow event (commit, skip or error). Returns true when
    /// that event was an error. Does nothing once cancelled (D19).
    pub(crate) fn commit_accepted(&self, task: &TaskSpec, tx: &broadcast::Sender<RunEvent>) -> bool {
        if self.is_cancelled() {
            return false;
        }
        let role = task.role;
        let branch = format!("crew/{}", role_dir_name(role));
        let emit = |kind: GitFlowKindDto, sha: Option<String>, detail: String| {
            let _ = tx.send(event(kind, Some(role), Some(branch.clone()), sha, Some(task.id.clone()), detail));
        };
        match self.commit_steps(task, &branch) {
            Ok(CommitOutcome::Committed { sha, detail }) => {
                emit(GitFlowKindDto::Commit, Some(sha), detail);
                false
            }
            Ok(CommitOutcome::Skipped(detail)) => {
                emit(GitFlowKindDto::Skip, None, detail);
                false
            }
            Err(detail) => {
                emit(GitFlowKindDto::Error, None, detail);
                true
            }
        }
    }

    /// D14 steps 1-7; every read's exit code is checked, and nothing is
    /// written before the worktree identity is verified.
    fn commit_steps(&self, task: &TaskSpec, branch: &str) -> Result<CommitOutcome, String> {
        let wt = self
            .role_worktrees
            .iter()
            .find(|(r, _)| *r == task.role)
            .map(|(_, p)| p.clone())
            .ok_or_else(|| format!("no worktree for role {}", role_dir_name(task.role)))?;
        // Review r1 F2: no git command before the `.git` pointer is verified.
        if let Err(detail) = self.trusted_worktree(task.role, &wt) {
            self.mark_reported(task.role);
            return Err(detail);
        }
        let read = |cwd: &Path, args: &[&str]| -> Result<String, String> {
            let out = self.git(cwd, args, GIT_LOCAL_TIMEOUT)?;
            if !out.ok {
                return Err(format!("git {} failed: {}", args.join(" "), out.stderr));
            }
            Ok(out.stdout_str())
        };
        let canonical = |p: &Path| std::fs::canonicalize(p).map_err(|e| format!("cannot resolve {}: {e}", p.display()));
        let common_dir = |cwd: &Path| -> Result<PathBuf, String> {
            let dir = PathBuf::from(read(cwd, &["rev-parse", "--git-common-dir"])?);
            canonical(&if dir.is_absolute() { dir } else { cwd.join(dir) })
        };

        // (1) identity: this is the role's own worktree, of this repo, on its branch.
        let toplevel = read(&wt, &["rev-parse", "--show-toplevel"])?;
        if canonical(Path::new(&toplevel))? != canonical(&wt)? {
            return Err(format!("worktree check failed: {} is not a worktree toplevel (found {toplevel})", wt.display()));
        }
        if common_dir(&wt)? != common_dir(&self.root)? {
            return Err(format!("worktree check failed: {} does not belong to {}", wt.display(), self.root.display()));
        }
        let head = read(&wt, &["symbolic-ref", "--quiet", "HEAD"])?;
        if head != format!("refs/heads/{branch}") {
            return Err(format!("worktree check failed: {} is on {head}, not {branch}", wt.display()));
        }

        // (2) what changed.
        let status = self.git(&wt, &["status", "--porcelain=v1", "-z", "--untracked-files=all"], GIT_LOCAL_TIMEOUT)?;
        if !status.ok {
            return Err(format!("git status failed: {}", status.stderr));
        }
        if status.stdout.is_empty() {
            return Ok(CommitOutcome::Skipped("clean tree: nothing to commit".to_string()));
        }

        // (3) exclusions and the size guard.
        let entries = parse_status_z(&status.stdout);
        let (excluded_paths, kept): (Vec<_>, Vec<_>) = entries.iter().partition(|(_, path)| excluded(path));
        let size: u64 = kept
            .iter()
            .map(|(xy, path)| {
                if xy.contains('D') {
                    return 0;
                }
                std::fs::symlink_metadata(wt.join(path)).map(|m| m.len()).unwrap_or(0)
            })
            .sum();
        if size > GUARD_MAX_BYTES {
            return Err(format!(
                "size guard: {size} bytes of changes exceed the 50 MiB limit; nothing staged — remove the large files by hand"
            ));
        }
        let mut notes = String::new();
        if !excluded_paths.is_empty() {
            let first: Vec<&str> = excluded_paths.iter().take(5).map(|(_, p)| p.as_str()).collect();
            notes = format!("; excluded {} path(s): {}", excluded_paths.len(), first.join(", "));
        }

        // (4) stage everything but the excluded directories.
        let mut add: Vec<String> = vec!["add".into(), "-A".into(), "--".into(), ".".into()];
        add.extend(EXCLUDED_DIRS.iter().map(|d| format!(":(exclude,glob)**/{d}/**")));
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        read(&wt, &add)?;

        // (5) anything left to commit?
        let staged = self.git(&wt, &["diff", "--cached", "--quiet"], GIT_LOCAL_TIMEOUT)?;
        match staged.code {
            Some(0) => return Ok(CommitOutcome::Skipped(format!("only excluded paths changed{notes}"))),
            Some(1) => {}
            _ => return Err(format!("git diff --cached failed: {}", staged.stderr)),
        }

        // (6) commit, (7) read the sha back as evidence.
        let message = commit_message(task.role, task);
        let mut commit = self.identity_args(&wt)?;
        commit.extend(["commit", "-q", "--no-gpg-sign", "-m", &message]);
        read(&wt, &commit)?;
        let sha = read(&wt, &["rev-parse", "HEAD"])?;
        let mut detail = format!("committed {} path(s)", kept.len());
        if self.leftover_dirty.contains(&task.role) {
            detail.push_str("; worktree had uncommitted changes before this run; they are included");
        }
        detail.push_str(&notes);
        Ok(CommitOutcome::Committed { sha, detail })
    }

    /// Merges every role branch that is ahead of main into the user's main
    /// checkout (D4/D5), only when the sprint's lead finished Ok.
    pub(crate) fn merge_sprint(&self, sprint_index: u32, sprint_ok: bool, tx: &broadcast::Sender<RunEvent>) -> MergeReport {
        let mut report = MergeReport::default();
        if self.is_cancelled() {
            return report;
        }
        report.failures += self.report_excluded(tx);
        if !sprint_ok {
            return report;
        }
        let root = self.root.clone();
        let sprint_skip = |detail: String, branch: Option<String>| {
            let _ = tx.send(event(GitFlowKindDto::Skip, None, branch, None, None, detail));
        };
        let Some(main) = self.main_branch.clone() else {
            let reason = "no main branch (none of origin/HEAD, main, master exists locally)".to_string();
            sprint_skip(format!("merge skipped: {reason}"), None);
            // Without a main, "ahead" is measured against the checkout's HEAD:
            // only role work that exists there counts as not landed (D4).
            let ahead = ROLE_ORDER.iter().any(|role| {
                self.ahead_of("HEAD", &format!("crew/{}", role_dir_name(*role))).unwrap_or(true)
            });
            if ahead {
                report.not_landed = Some(reason);
            }
            return report;
        };
        let error = |role: Option<Role>, detail: String| {
            let branch = role.map(|r| format!("crew/{}", role_dir_name(r)));
            let _ = tx.send(event(GitFlowKindDto::Error, role, branch, None, None, detail));
        };

        for role in ROLE_ORDER {
            if self.is_cancelled() {
                break;
            }
            if self.is_excluded(role) {
                continue;
            }
            let branch = format!("crew/{}", role_dir_name(role));
            match self.ahead_of(&format!("refs/heads/{main}"), &branch) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(detail) => {
                    error(Some(role), detail);
                    report.failures += 1;
                    continue;
                }
            }
            if self.merges_blocked.load(Ordering::SeqCst) {
                let reason = "merges are blocked for this run: an earlier merge --abort could not be verified".to_string();
                sprint_skip(format!("merge skipped: {reason}"), Some(main.clone()));
                report.not_landed = Some(reason);
                break;
            }
            match self.merge_preconditions(&main) {
                Ok(None) => {}
                Ok(Some(reason)) => {
                    sprint_skip(format!("merge skipped: {reason}; the main checkout was not touched"), Some(main.clone()));
                    report.not_landed = Some(reason);
                    break;
                }
                Err(detail) => {
                    error(None, detail);
                    report.failures += 1;
                    break;
                }
            }
            // Review r1 F1: never let the merge overwrite a user file git does
            // not track (ignored files are invisible to the status check).
            match self.untracked_collisions(&main, &branch) {
                Ok(paths) if paths.is_empty() => {}
                Ok(paths) => {
                    let listed = if paths.len() > 10 {
                        format!("{} and {} more", paths[..10].join(", "), paths.len() - 10)
                    } else {
                        paths.join(", ")
                    };
                    let overwrite = format!("would overwrite untracked/ignored file(s) in the main checkout: {listed}");
                    if report.not_landed.is_none() {
                        report.not_landed = Some(format!("merging {branch} {overwrite}"));
                    }
                    let detail = format!("merge {overwrite}");
                    let _ = tx.send(event(GitFlowKindDto::Conflict, Some(role), Some(branch), None, None, detail));
                    continue;
                }
                Err(detail) => {
                    error(Some(role), detail);
                    report.failures += 1;
                    continue;
                }
            }
            match self.merge_one(&main, &branch, sprint_index) {
                Ok(sha) => {
                    report.merged_any = true;
                    let detail = format!("merged {branch} into {main}");
                    let _ = tx.send(event(GitFlowKindDto::Merge, Some(role), Some(branch), Some(sha), None, detail));
                }
                Err(MergeFailure::Conflict(paths)) => {
                    report.failures += 1;
                    let detail = format!(
                        "merge conflict in {paths}; the merge was aborted and {root} is unchanged. \
                         {branch} is left as is and will conflict again in later runs until it is reconciled with {main} by hand",
                        root = root.display()
                    );
                    let _ = tx.send(event(GitFlowKindDto::Conflict, Some(role), Some(branch), None, None, detail));
                }
                Err(MergeFailure::Error(detail)) => {
                    report.failures += 1;
                    error(Some(role), detail);
                }
                Err(MergeFailure::Blocked(detail)) => {
                    report.failures += 1;
                    self.merges_blocked.store(true, Ordering::SeqCst);
                    error(Some(role), detail);
                    break;
                }
            }
        }
        report
    }

    /// Whether `branch` exists and has commits `base` (a revision) does not.
    fn ahead_of(&self, base: &str, branch: &str) -> Result<bool, String> {
        let exists = self.git(&self.root, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")], GIT_LOCAL_TIMEOUT)?;
        match exists.code {
            Some(0) => {}
            Some(1) => return Ok(false),
            _ => return Err(format!("git rev-parse {branch} failed: {}", exists.stderr)),
        }
        let range = format!("{base}..refs/heads/{branch}");
        let count = self.git(&self.root, &["rev-list", "--count", &range], GIT_LOCAL_TIMEOUT)?;
        if !count.ok {
            return Err(format!("git rev-list {range} failed: {}", count.stderr));
        }
        count
            .stdout_str()
            .parse::<u64>()
            .map(|n| n > 0)
            .map_err(|e| format!("git rev-list {range} printed no count: {e}"))
    }

    /// D4: Ok(None) when root may be merged into, Ok(Some(reason)) for a
    /// skip, Err when a read itself failed.
    fn merge_preconditions(&self, main: &str) -> Result<Option<String>, String> {
        let head = self.git(&self.root, &["symbolic-ref", "--quiet", "HEAD"], GIT_LOCAL_TIMEOUT)?;
        match head.code {
            Some(0) if head.stdout_str() == format!("refs/heads/{main}") => {}
            Some(0) | Some(1) => return Ok(Some(format!("the main checkout is not on {main}"))),
            _ => return Err(format!("git symbolic-ref HEAD failed: {}", head.stderr)),
        }
        let status = self.git(&self.root, &["status", "--porcelain", "--untracked-files=normal"], GIT_LOCAL_TIMEOUT)?;
        if !status.ok {
            return Err(format!("git status failed in the main checkout: {}", status.stderr));
        }
        if !status.stdout.is_empty() {
            return Ok(Some("the main checkout has uncommitted changes".to_string()));
        }
        match self.merge_head_present()? {
            false => Ok(None),
            true => Ok(Some("a merge is already in progress in the main checkout".to_string())),
        }
    }

    fn merge_head_present(&self) -> Result<bool, String> {
        let out = self.git(&self.root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"], GIT_LOCAL_TIMEOUT)?;
        match out.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(format!("git rev-parse MERGE_HEAD failed: {}", out.stderr)),
        }
    }

    fn head_sha(&self) -> Result<String, String> {
        let out = self.git(&self.root, &["rev-parse", "HEAD"], GIT_LOCAL_TIMEOUT)?;
        if !out.ok {
            return Err(format!("git rev-parse HEAD failed: {}", out.stderr));
        }
        Ok(out.stdout_str())
    }

    /// One role's merge (D4) and, on any non-Ok result, D5's
    /// MERGE_HEAD -> abort -> verify sequence.
    fn merge_one(&self, main: &str, branch: &str, sprint_index: u32) -> Result<String, MergeFailure> {
        let before = self.head_sha().map_err(MergeFailure::Error)?;
        let message = format!("crew: merge {branch} (sprint {sprint_index})");
        let full_ref = format!("refs/heads/{branch}");
        // Review r2: no directory-rename relocation (ort would otherwise move
        // a file added under a directory main renamed into the new directory,
        // past the collision check). What `untracked_collisions` examined is
        // the changed paths, their ancestors and their `<name>~…` siblings —
        // not an exhaustive list of what a merge can write; git >= 2.38's
        // `merge-tree --write-tree --name-only HEAD <branch>` would be.
        let mut args = vec!["-c", "merge.directoryRenames=false"];
        args.extend(self.identity_args(&self.root).map_err(MergeFailure::Error)?);
        args.extend(["merge", "--no-ff", "--no-gpg-sign", "--no-edit", "-m", &message, &full_ref]);
        let failure = match self.git(&self.root, &args, GIT_LOCAL_TIMEOUT) {
            Ok(out) if out.ok => return self.head_sha().map_err(MergeFailure::Error),
            Ok(out) => out.stderr,
            Err(e) => e,
        };
        let root = self.root.display();
        let blocked = |why: String| MergeFailure::Blocked(format!("merge --abort did not restore {root}; resolve by hand ({why})"));
        if !self.merge_head_present().map_err(blocked)? {
            // git refused before starting the merge.
            return match self.head_sha() {
                Ok(sha) if sha == before => Err(MergeFailure::Error(format!("merge of {branch} into {main} refused: {failure}"))),
                Ok(_) | Err(_) => Err(blocked(format!("HEAD moved after a failed merge of {branch}: {failure}"))),
            };
        }
        let conflicted = self
            .git(&self.root, &["diff", "--name-only", "--diff-filter=U"], GIT_LOCAL_TIMEOUT)
            .map(|out| out.stdout_str().lines().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let _ = self.git(&self.root, &["merge", "--abort"], GIT_LOCAL_TIMEOUT);
        #[cfg(test)]
        if self.abort_leaves_merge_head {
            let _ = std::fs::write(self.root.join(".git").join("MERGE_HEAD"), format!("{before}\n"));
        }
        let restored = !self.merge_head_present().unwrap_or(true)
            && self.head_sha().is_ok_and(|sha| sha == before)
            && self
                .git(&self.root, &["status", "--porcelain"], GIT_LOCAL_TIMEOUT)
                .is_ok_and(|out| out.ok && out.stdout.is_empty());
        if !restored {
            return Err(blocked(format!("merge of {branch} failed: {failure}")));
        }
        let paths = if conflicted.is_empty() { "(no path reported)".to_string() } else { conflicted };
        Err(MergeFailure::Conflict(paths))
    }

    /// The end-of-run push (D8/D9): exactly one push, skip or error event
    /// unless cancelled.
    pub(crate) fn push_main(&self, run_completed: bool, merged_any: bool, tx: &broadcast::Sender<RunEvent>) {
        if self.is_cancelled() {
            return;
        }
        let branch = self.main_branch.clone();
        let send = |kind: GitFlowKindDto, sha: Option<String>, detail: String| {
            let _ = tx.send(event(kind, None, branch.clone(), sha, None, detail));
        };
        match self.push_steps(run_completed, merged_any) {
            Ok(PushOutcome::Pushed(sha)) => send(GitFlowKindDto::Push, Some(sha), "pushed to origin".to_string()),
            Ok(PushOutcome::Skipped(reason)) => send(GitFlowKindDto::Skip, None, format!("push skipped: {reason}")),
            Err(detail) => send(GitFlowKindDto::Error, None, detail),
        }
    }

    fn push_steps(&self, run_completed: bool, merged_any: bool) -> Result<PushOutcome, String> {
        let Some(main) = self.main_branch.clone() else {
            return Ok(PushOutcome::Skipped("no main branch".to_string()));
        };
        if !run_completed {
            return Ok(PushOutcome::Skipped("the run did not complete".to_string()));
        }
        if !merged_any {
            return Ok(PushOutcome::Skipped("nothing merged this run".to_string()));
        }
        let root = &self.root;
        let origin = self.git(root, &["remote", "get-url", "origin"], GIT_LOCAL_TIMEOUT)?;
        if !origin.ok {
            return Ok(PushOutcome::Skipped("no origin remote".to_string()));
        }
        let main_ref = format!("refs/heads/{main}");

        // git-lfs (D8): D6 disables its pre-push hook, so never push such a repo.
        let attrs = self.git(root, &["ls-tree", "--name-only", &main_ref, "--", ".gitattributes"], GIT_LOCAL_TIMEOUT)?;
        if !attrs.ok {
            return Err(format!("git ls-tree failed: {}", attrs.stderr));
        }
        if !attrs.stdout.is_empty() {
            let shown = self.git(root, &["show", &format!("{main_ref}:.gitattributes")], GIT_LOCAL_TIMEOUT)?;
            if !shown.ok {
                return Err(format!("git show .gitattributes failed: {}", shown.stderr));
            }
            if String::from_utf8_lossy(&shown.stdout).contains("filter=lfs") {
                return Ok(PushOutcome::Skipped("repo uses git-lfs; push by hand".to_string()));
            }
        }

        // D9: scan every commit the push would publish.
        self.scan_outgoing(&main)?;

        // D7: never wait on an ssh prompt; a user-configured ssh command wins.
        let mut env = Vec::new();
        if std::env::var_os("GIT_SSH_COMMAND").is_none() {
            let ssh = self.git(root, &["config", "--get", "core.sshCommand"], GIT_LOCAL_TIMEOUT)?;
            match ssh.code {
                Some(0) => {}
                Some(1) => env.push(("GIT_SSH_COMMAND".to_string(), "ssh -o BatchMode=yes".to_string())),
                _ => return Err(format!("git config --get core.sshCommand failed: {}", ssh.stderr)),
            }
        }
        let refspec = format!("{main_ref}:{main_ref}");
        let pushed = self.git_with(root, &["push", "--porcelain", "origin", &refspec], PUSH_TIMEOUT, &env, None)?;
        if !pushed.ok {
            let stderr: String = pushed.stderr.chars().take(500).collect();
            return Err(format!("push to origin failed: {stderr}"));
        }
        let sha = self.git(root, &["rev-parse", &main_ref], GIT_LOCAL_TIMEOUT)?;
        if !sha.ok {
            return Err(format!("git rev-parse {main_ref} failed: {}", sha.stderr));
        }
        Ok(PushOutcome::Pushed(sha.stdout_str()))
    }

    /// D9: Err (push withheld) when an added line in `<base>..main` looks like
    /// a secret, or when the scan itself could not complete.
    fn scan_outgoing(&self, main: &str) -> Result<(), String> {
        let root = &self.root;
        let main_ref = format!("refs/heads/{main}");
        let tracking = format!("refs/remotes/origin/{main}");
        let has_tracking = self.git(root, &["rev-parse", "--verify", "-q", &tracking], GIT_LOCAL_TIMEOUT)?;
        let base = match has_tracking.code {
            Some(0) => {
                let mb = self.git(root, &["merge-base", &tracking, &main_ref], GIT_LOCAL_TIMEOUT)?;
                match mb.code {
                    Some(0) => Some(mb.stdout_str()),
                    Some(1) => None,
                    _ => return Err(format!("push withheld: git merge-base failed: {}", mb.stderr)),
                }
            }
            Some(1) => None,
            _ => return Err(format!("push withheld: git rev-parse {tracking} failed: {}", has_tracking.stderr)),
        };
        let range = match base {
            Some(base) => format!("{base}..{main_ref}"),
            None => main_ref.clone(),
        };
        let args = [
            "-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false", "log", "-p", "--root", "--text",
            "--no-merges", "--no-color", "--no-ext-diff", "--no-textconv", "--src-prefix=a/", "--dst-prefix=b/",
            "-U0", "--format=", &range,
        ];
        let patch = match self.git_with(root, &args, PUSH_TIMEOUT, &[], Some(SCAN_MAX_BYTES)) {
            Ok(out) if out.ok => out.stdout,
            Ok(out) => return Err(format!("push withheld: history scan failed: {}", out.stderr)),
            Err(e) if e.contains("output exceeded") => {
                return Err("push withheld: outgoing history too large to scan".to_string())
            }
            Err(e) => return Err(format!("push withheld: history scan failed: {e}")),
        };
        if let Some((pattern, path)) = scan_patch(&patch) {
            return Err(format!(
                "push withheld: secret-like content ({pattern}) added in {path}; inspect {main} history before pushing"
            ));
        }
        Ok(())
    }
}

enum CommitOutcome {
    Committed { sha: String, detail: String },
    Skipped(String),
}

enum MergeFailure {
    /// Aborted and verified; carries the conflicted paths.
    Conflict(String),
    /// Refused before starting; root unchanged.
    Error(String),
    /// The abort could not be verified: stop merging for this run.
    Blocked(String),
}

enum PushOutcome {
    Pushed(String),
    Skipped(String),
}

/// Review r1 F2: Ok only when `<wt>/.git` is a regular file (not a
/// directory, not a symlink) holding exactly one `gitdir: <p>` line whose
/// canonical `<p>` lies under `<root>/.git/worktrees/`. Filesystem only — it
/// runs before any git command, so a redirected pointer never hands its
/// config (fsmonitor, filters, ...) to git.
pub(crate) fn role_worktree_is_ours(root_canonical: &Path, wt: &Path) -> Result<(), String> {
    let pointer = wt.join(".git");
    let meta = std::fs::symlink_metadata(&pointer).map_err(|e| format!("cannot stat {}: {e}", pointer.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", pointer.display()));
    }
    let content = std::fs::read_to_string(&pointer).map_err(|e| format!("cannot read {}: {e}", pointer.display()))?;
    let mut lines = content.lines();
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return Err(format!("{} must hold exactly one line", pointer.display()));
    };
    let target = line
        .strip_prefix("gitdir: ")
        .ok_or_else(|| format!("{} does not start with \"gitdir: \"", pointer.display()))?;
    let target = Path::new(target);
    let target = if target.is_absolute() { target.to_path_buf() } else { wt.join(target) };
    let target = std::fs::canonicalize(&target).map_err(|e| format!("cannot resolve {}: {e}", target.display()))?;
    let worktrees = std::fs::canonicalize(root_canonical.join(".git").join("worktrees"))
        .map_err(|e| format!("cannot resolve {}/.git/worktrees: {e}", root_canonical.display()))?;
    if target == worktrees || !target.starts_with(&worktrees) {
        return Err(format!("it points at {}, outside {}", target.display(), worktrees.display()));
    }
    Ok(())
}

/// Review r3: entries next to `name` (a root-relative path) whose file name
/// starts with `<basename of name>~` — the names ort writes a side to on a
/// file/directory or file-type conflict. A missing parent yields nothing.
fn relocation_siblings(root: &Path, name: &str) -> Vec<String> {
    let (parent, base) = match name.rfind('/') {
        Some(at) => (&name[..at], &name[at + 1..]),
        None => ("", name),
    };
    let prefix = format!("{base}~");
    let Ok(entries) = std::fs::read_dir(root.join(parent)) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|entry| entry.starts_with(&prefix))
        .map(|entry| if parent.is_empty() { entry } else { format!("{parent}/{entry}") })
        .collect()
}

/// NUL-separated git output (`-z`) as lossy strings, empty fields dropped.
fn split_z(raw: &[u8]) -> impl Iterator<Item = String> + '_ {
    raw.split(|b| *b == 0).filter(|f| !f.is_empty()).map(|f| String::from_utf8_lossy(f).into_owned())
}

/// `status --porcelain=v1 -z` entries as (XY, path); a rename's or copy's
/// second (original) path is dropped.
fn parse_status_z(raw: &[u8]) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut fields = raw.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(field) = fields.next() {
        let field = String::from_utf8_lossy(field);
        if field.len() < 4 {
            continue;
        }
        let (xy, path) = (field[..2].to_string(), field[3..].to_string());
        if xy.starts_with('R') || xy.starts_with('C') {
            fields.next();
        }
        entries.push((xy, path));
    }
    entries
}

/// A GitFlow event (D3); `role` is rendered with `role_dir_name`.
pub(crate) fn event(
    kind: GitFlowKindDto,
    role: Option<Role>,
    branch: Option<String>,
    sha: Option<String>,
    task_id: Option<String>,
    detail: String,
) -> RunEvent {
    RunEvent::GitFlow {
        kind,
        role: role.map(|r| role_dir_name(r).to_string()),
        branch,
        sha,
        task_id,
        detail,
        ts: now_ts(),
    }
}

// --- pure helpers -------------------------------------------------------------

/// `crew(<role>): <task id> — <first title line> [REQ-…]` (D14).
pub(crate) fn commit_message(role: Role, task: &TaskSpec) -> String {
    let mut message = format!("crew({}): {}", role_dir_name(role), task.id);
    let title = task.title.lines().next().unwrap_or("").trim();
    if !title.is_empty() {
        message.push_str(" — ");
        message.push_str(title);
    }
    let mut req_ids: Vec<&str> = Vec::new();
    for check in &task.dod {
        if let DodCheck::ReqCover { ids } = check {
            for id in ids {
                if !req_ids.contains(&id.as_str()) {
                    req_ids.push(id.as_str());
                }
            }
        }
    }
    if !req_ids.is_empty() {
        message.push_str(&format!(" [{}]", req_ids.join(", ")));
    }
    message
}

/// The `-c` pairs to add for identity keys the user's config lacks (D15).
pub(crate) fn identity_overrides(has_name: bool, has_email: bool) -> Vec<&'static str> {
    let mut args = Vec::new();
    if !has_name {
        args.extend(["-c", FALLBACK_NAME]);
    }
    if !has_email {
        args.extend(["-c", FALLBACK_EMAIL]);
    }
    args
}

/// Whether a status-listed path has a directory component in EXCLUDED_DIRS.
pub(crate) fn excluded(path: &str) -> bool {
    let components: Vec<&str> = path.split('/').collect();
    components[..components.len() - 1].iter().any(|c| EXCLUDED_DIRS.contains(c))
}

/// D10: some Accepted task carries a Cmd check with a parseable expect, or a
/// Browser check with a parseable expect while the browser executor is wired
/// — i.e. a check that executed and passed.
pub(crate) fn completed_gate_met(dag: &TaskDag, states: &HashMap<String, TaskState>, browser_wired: bool) -> bool {
    dag.tasks.iter().any(|task| {
        states.get(&task.id) == Some(&TaskState::Accepted)
            && task.dod.iter().any(|check| match check {
                DodCheck::Cmd { expect, .. } => parse_expect(expect).is_some(),
                DodCheck::Browser { expect, .. } => browser_wired && parse_browser_expect(expect).is_some(),
                _ => false,
            })
    })
}

/// D9: the name of the key pattern `line` contains, if any.
pub(crate) fn secret_like(line: &str) -> Option<&'static str> {
    if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
        return Some("private key");
    }
    let upper_digit = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    if followed_by(line, "AKIA", 16, upper_digit) {
        return Some("aws access key id");
    }
    let alnum = |c: char| c.is_ascii_alphanumeric();
    if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"].iter().any(|p| followed_by(line, p, 36, alnum)) {
        return Some("github token");
    }
    if followed_by(line, "github_pat_", 22, |c| c.is_ascii_alphanumeric() || c == '_') {
        return Some("github fine-grained token");
    }
    None
}

/// Whether some occurrence of `prefix` in `line` is followed by at least `n`
/// characters matching `class`.
fn followed_by(line: &str, prefix: &str, n: usize, class: impl Fn(char) -> bool) -> bool {
    line.match_indices(prefix)
        .any(|(at, _)| line[at + prefix.len()..].chars().take(n).filter(|c| class(*c)).count() == n)
}

/// D9's patch parser: the first added line that looks like a secret, with the
/// file it was added to.
fn scan_patch(patch: &[u8]) -> Option<(&'static str, String)> {
    let mut in_header = false;
    let mut file = String::from("(unknown file)");
    for raw in patch.split(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(raw);
        if line.starts_with("diff --git ") {
            in_header = true;
            continue;
        }
        if in_header {
            if let Some(target) = line.strip_prefix("+++ ") {
                file = target.strip_prefix("b/").unwrap_or(target).to_string();
            } else if line.starts_with("@@") {
                in_header = false;
            }
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            if let Some(pattern) = secret_like(added) {
                return Some((pattern, file));
            }
        }
    }
    None
}

// --- the per-project-root run lock (D21) ------------------------------------

/// Held from `RunController::start` until the finisher's end (or shutdown).
pub(crate) struct RunLock {
    path: PathBuf,
    run_id: String,
}

pub(crate) type LockSlot = Arc<std::sync::Mutex<Option<RunLock>>>;

impl RunLock {
    /// D21 steps 3-4: create the lock, or report `project_root_busy`, or
    /// steal a lock whose pid is dead.
    pub(crate) fn acquire(lock_path: &Path, run_id: &str) -> Result<RunLock, String> {
        match create_lock(lock_path, run_id) {
            Ok(lock) => return Ok(lock),
            Err(CreateError::Exists) => {}
            Err(CreateError::NotFound) => {
                // The base dir is missing (first run, or removed meanwhile).
                if let Some(parent) = lock_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                }
                match create_lock(lock_path, run_id) {
                    Ok(lock) => return Ok(lock),
                    Err(CreateError::Exists) => {}
                    Err(CreateError::NotFound) => {
                        return Err(format!("cannot create run lock {}: its directory keeps disappearing", lock_path.display()))
                    }
                    Err(CreateError::Other(e)) => return Err(e),
                }
            }
            Err(CreateError::Other(e)) => return Err(e),
        }

        let content = match std::fs::read(lock_path) {
            Ok(content) => content,
            // Released between our create and our read: one more try.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return create_lock(lock_path, run_id).map_err(|e| match e {
                    CreateError::Exists | CreateError::NotFound => busy(lock_path, &[]),
                    CreateError::Other(e) => e,
                })
            }
            Err(e) => return Err(format!("cannot read run lock {}: {e}", lock_path.display())),
        };
        match lock_pid(&content) {
            Some(pid) if !pid_alive(pid) => steal_with(lock_path, &content, run_id, &|| {}),
            _ => Err(busy(lock_path, &content)),
        }
    }
}

enum CreateError {
    Exists,
    NotFound,
    Other(String),
}

/// `create_new` + content + `sync_all` (D21 step 3); a write failure removes
/// the half-written file.
fn create_lock(lock_path: &Path, run_id: &str) -> Result<RunLock, CreateError> {
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(lock_path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(CreateError::Exists),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(CreateError::NotFound),
        Err(e) => return Err(CreateError::Other(format!("cannot create run lock {}: {e}", lock_path.display()))),
    };
    let content = format!("pid={}\nrun_id={run_id}\nstarted={}\n", std::process::id(), now_ts());
    if let Err(e) = file.write_all(content.as_bytes()).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(lock_path);
        return Err(CreateError::Other(format!("cannot write run lock {}: {e}", lock_path.display())));
    }
    Ok(RunLock { path: lock_path.to_path_buf(), run_id: run_id.to_string() })
}

fn lock_field<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    content.lines().find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
}

fn lock_pid(content: &[u8]) -> Option<u32> {
    lock_field(&String::from_utf8_lossy(content), "pid")?.trim().parse().ok()
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        // Unable to tell: never steal.
        .unwrap_or(true)
}

/// The `project_root_busy` message (D21), naming this app when the holder is
/// this very process.
fn busy(lock_path: &Path, content: &[u8]) -> String {
    let text = String::from_utf8_lossy(content);
    let started = lock_field(&text, "started").unwrap_or("unknown").trim().to_string();
    match lock_pid(content) {
        Some(pid) if pid == std::process::id() => format!(
            "project_root_busy: a run in this app (started {started}) still holds {}; wait for it to finish or stop it",
            lock_path.display()
        ),
        pid => format!(
            "project_root_busy: another run (pid {}, started {started}) holds {}; if that run is gone, delete the file",
            pid.map(|p| p.to_string()).unwrap_or_else(|| "unknown".to_string()),
            lock_path.display()
        ),
    }
}

/// D21 step 4's steal of a lock whose content `observed` named a dead pid.
/// `before_restore` is a test seam for the three-racer window between the
/// rename and the restore; production passes a no-op.
fn steal_with(lock_path: &Path, observed: &[u8], run_id: &str, before_restore: &dyn Fn()) -> Result<RunLock, String> {
    let stale = lock_path.with_file_name(format!(".crew-run.lock.stale-{}", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::rename(lock_path, &stale) {
        if e.kind() == std::io::ErrorKind::NotFound {
            // Another starter already moved it: race for a fresh create.
            return create_lock(lock_path, run_id).map_err(|e| match e {
                CreateError::Other(e) => e,
                _ => busy(lock_path, &[]),
            });
        }
        return Err(format!("cannot take over the stale run lock {}: {e}", lock_path.display()));
    }
    let renamed = std::fs::read(&stale).unwrap_or_default();
    if renamed != observed {
        // A live run replaced the stale lock between our read and our rename:
        // put its lock back without ever clobbering a newer one.
        before_restore();
        if std::fs::hard_link(&stale, lock_path).is_ok() {
            let _ = std::fs::remove_file(&stale);
        }
        return Err(busy(lock_path, &renamed));
    }
    let _ = std::fs::remove_file(&stale);
    create_lock(lock_path, run_id).map_err(|e| match e {
        CreateError::Other(e) => e,
        _ => busy(lock_path, &std::fs::read(lock_path).unwrap_or_default()),
    })
}

impl Drop for RunLock {
    /// Delete-if-owner-matches: a file that no longer names this run is left.
    fn drop(&mut self) {
        let owner = format!("run_id={}", self.run_id);
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            if content.lines().any(|line| line == owner) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Takes and drops the slot's lock, tolerating a poisoned mutex.
pub(crate) fn release(slot: &LockSlot) {
    let taken = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    drop(taken);
}

/// Releases `slot` once no git phase holds `phase` (D19/D21): shutdown's
/// aborts cannot stop an in-flight blocking phase, and the next run on the
/// root must not overlap it.
pub(crate) fn release_after_phase(phase: Arc<tokio::sync::Mutex<()>>, slot: LockSlot) {
    tokio::spawn(async move {
        let _phase = phase.lock().await;
        release(&slot);
    });
}

/// The finisher's scope guard (D21): releases the lock while unwinding a
/// panic. A plain drop (the finisher aborted by shutdown) does not release —
/// shutdown releases after the in-flight git phase ends (D19).
pub(crate) struct LockRelease(pub LockSlot);

impl Drop for LockRelease {
    fn drop(&mut self) {
        if std::thread::panicking() {
            release(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_proto::ReqId;

    // --- fixtures -----------------------------------------------------------

    /// A per-test dir under `.crew-test/`, removed when the test ends (also
    /// while unwinding a failed assertion).
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let dir = std::env::current_dir()
                .unwrap()
                .join(".crew-test")
                .join(format!("gitflow-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("test setup: create test dir");
            TestDir(std::fs::canonicalize(&dir).unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_out(cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .expect("test setup: git must be on PATH")
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let out = git_out(cwd, args);
        assert!(
            out.status.success(),
            "test setup: `git {args:?}` failed in {cwd:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `git init -q -b <branch>` + local identity + README commit.
    fn init_repo(dir: &Path, branch: &str) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", branch]);
        git(dir, &["config", "user.name", "Test"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("README.md"), b"init\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "init"]);
    }

    fn add_worktree(repo: &Path, base: &Path, role: Role) -> PathBuf {
        std::fs::create_dir_all(base).unwrap();
        let path = base.join(role_dir_name(role));
        git(repo, &["worktree", "add", "-q", "-b", &format!("crew/{}", role_dir_name(role)), &path.to_string_lossy()]);
        std::fs::canonicalize(path).unwrap()
    }

    fn write(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn task(id: &str, role: Role, title: &str, dod: Vec<DodCheck>) -> TaskSpec {
        TaskSpec {
            id: id.to_string(),
            role,
            title: title.to_string(),
            brief: String::new(),
            dod,
            deps: Vec::new(),
            artifacts_expected: Vec::new(),
        }
    }

    fn dev_task() -> TaskSpec {
        task("t-dev", Role::Developer, "Build it", Vec::new())
    }

    /// (kind, role, branch, sha, task_id, detail) of one GitFlow event.
    type Ev = (GitFlowKindDto, Option<String>, Option<String>, Option<String>, Option<String>, String);

    fn drain(rx: &mut broadcast::Receiver<RunEvent>) -> Vec<Ev> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let RunEvent::GitFlow { kind, role, branch, sha, task_id, detail, .. } = ev {
                out.push((kind, role, branch, sha, task_id, detail));
            }
        }
        out
    }

    fn ctx(repo: &Path, worktrees: &[(Role, PathBuf)]) -> GitFlowCtx {
        GitFlowCtx::for_run(Some(repo), Some(worktrees), false).expect("ctx for a real repo")
    }

    /// A repo on `main` with a developer worktree and its ctx.
    fn dev_fixture(label: &str) -> (TestDir, PathBuf, PathBuf, GitFlowCtx) {
        let dir = TestDir::new(label);
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        let c = ctx(&repo, &[(Role::Developer, wt.clone())]);
        (dir, repo, wt, c)
    }

    fn channel() -> (broadcast::Sender<RunEvent>, broadcast::Receiver<RunEvent>) {
        broadcast::channel(256)
    }

    fn pid_alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn write_hook(dir: &Path, name: &str, marker: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let hook = dir.join(name);
        std::fs::write(&hook, format!("#!/bin/sh\necho ran >> '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // --- Task 04: runner, ctx, main branch, hooks, phase ----------------------

    /// Error: an overrunning command and its whole group are killed at the
    /// deadline.
    #[test]
    fn run_with_deadline_kills_an_overrunning_group() {
        let dir = TestDir::new("deadline");
        let pids = dir.path().join("pids");
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(format!(
            "sleep 30 & echo $! > '{p}'; sleep 30 & echo $! >> '{p}'; wait",
            p = pids.display()
        ));
        let started = Instant::now();

        let result = run_bounded(cmd, Duration::from_millis(300), None);

        let err = result.expect_err("an overrunning group must time out");
        assert!(err.contains("timed out"), "error must say it timed out: {err}");
        assert!(started.elapsed() < Duration::from_secs(5), "the kill must be prompt");
        let recorded = std::fs::read_to_string(&pids).expect("sh must have recorded its children");
        let children: Vec<&str> = recorded.lines().collect();
        assert!(!children.is_empty(), "test setup: at least one sleep must have started");
        for pid in children {
            let deadline = Instant::now() + Duration::from_secs(3);
            while pid_alive(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(!pid_alive(pid), "sleep {pid} in the killed group must not survive");
        }
    }

    /// Boundary: a grandchild that left the group and keeps stdout open
    /// cannot hold the runner past its bound.
    #[test]
    fn a_detached_pipe_holder_does_not_block_the_runner() {
        let mut cmd = Command::new("perl");
        cmd.arg("-e")
            .arg("use POSIX; if (fork()==0) { POSIX::setsid(); sleep 10; exit 0 } exit 0");
        let deadline = Duration::from_secs(2);
        let started = Instant::now();

        let result = run_bounded(cmd, deadline, None);

        assert!(started.elapsed() < deadline + Duration::from_secs(2), "the runner must return within its bound");
        let err = result.expect_err("an abandoned reader is an error, not a silent success");
        assert!(err.contains("output reader did not finish"), "{err}");
    }

    /// Normal: a command within its bound returns its exit status and output.
    #[test]
    fn a_finished_command_returns_code_and_output() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf out; printf err >&2; exit 3");

        let out = run_bounded(cmd, Duration::from_secs(10), None).expect("a finished command is Ok");

        assert!(!out.ok);
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, "err");
    }

    /// Error: stdout over the cap kills the group and is an error.
    #[test]
    fn output_cap_is_an_error() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("yes | head -c 200000");

        let err = run_bounded(cmd, Duration::from_secs(10), Some(1000)).expect_err("over-cap output must fail");

        assert!(err.contains("output exceeded 1000 bytes"), "{err}");
    }

    /// Boundary (D12): without project_root or worktrees there is no ctx, so
    /// no git command can be issued.
    #[test]
    fn for_run_is_none_without_project_root() {
        let nowhere = Path::new("/nonexistent/crew-run/gitflow");
        assert!(GitFlowCtx::for_run(None, Some(&[]), true).is_none());
        assert!(GitFlowCtx::for_run(Some(nowhere), None, true).is_none());
        assert!(GitFlowCtx::for_run(None, None, false).is_none());
    }

    /// Normal (D13): origin/HEAD naming origin/main with a local main.
    #[test]
    fn resolve_main_follows_origin_head_when_the_branch_exists_locally() {
        let dir = TestDir::new("main-origin-head");
        let repo = dir.path().join("repo");
        init_repo(&repo, "trunk");
        git(&repo, &["branch", "develop"]);
        git(&repo, &["update-ref", "refs/remotes/origin/develop", "HEAD"]);
        git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/develop"]);

        let c = ctx(&repo, &[]);

        assert_eq!(c.main_branch(), Some("develop"));
    }

    /// Boundary (D13): origin/HEAD names a branch with no local ref, so the
    /// local `main` is used.
    #[test]
    fn resolve_main_falls_back_to_main_when_origin_head_has_no_local_branch() {
        let dir = TestDir::new("main-fallback");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        git(&repo, &["update-ref", "refs/remotes/origin/trunk", "HEAD"]);
        git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]);

        assert_eq!(ctx(&repo, &[]).main_branch(), Some("main"));
    }

    /// Boundary (D13): a repo on master only.
    #[test]
    fn resolve_main_uses_master_when_there_is_no_main() {
        let dir = TestDir::new("main-master");
        let repo = dir.path().join("repo");
        init_repo(&repo, "master");

        assert_eq!(ctx(&repo, &[]).main_branch(), Some("master"));
    }

    /// Error (D13): no origin/HEAD, no main, no master -> None.
    #[test]
    fn resolve_main_is_none_for_an_unknown_branch_layout() {
        let dir = TestDir::new("main-none");
        let repo = dir.path().join("repo");
        init_repo(&repo, "trunk");

        assert_eq!(ctx(&repo, &[]).main_branch(), None);
    }

    /// D6: a hook the user's config points at never runs under the runner —
    /// proven against a plain git commit that does run it.
    #[test]
    fn hooks_are_disabled() {
        let dir = TestDir::new("hooks");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let marker = dir.path().join("marker");
        let hooks = dir.path().join("hooks");
        write_hook(&hooks, "pre-commit", &marker);
        write_hook(&hooks, "post-commit", &marker);
        git(&repo, &["config", "core.hooksPath", &hooks.to_string_lossy()]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "plain"]);
        assert!(marker.exists(), "test setup: the hooks must run for a plain commit");
        std::fs::remove_file(&marker).unwrap();

        let out = run_git_bounded(&repo, &["commit", "-q", "--allow-empty", "-m", "x"], GIT_LOCAL_TIMEOUT, &[], None)
            .expect("the bounded commit runs");

        assert!(out.ok, "the commit must succeed: {}", out.stderr);
        assert!(!marker.exists(), "no hook may run under the flow's runner");
    }

    /// D19: a blocking phase that holds the guard delays anyone waiting on
    /// the phase mutex until it ends.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_held_phase_delays_the_wait() {
        let phase = Arc::new(tokio::sync::Mutex::new(()));
        let guard = phase.clone().lock_owned().await;
        let started = Instant::now();
        let blocking = tokio::task::spawn_blocking(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(guard);
        });

        let _waited = phase.lock().await;

        assert!(started.elapsed() >= Duration::from_millis(250), "the wait must last until the phase ends");
        blocking.await.unwrap();
    }

    // --- Task 05: the run lock -----------------------------------------------

    mod run_lock {
        use super::*;

        fn lock_content(pid: &str, run_id: &str) -> String {
            format!("pid={pid}\nrun_id={run_id}\nstarted=2026-09-28T00:00:00Z\n")
        }

        fn stale_files(dir: &Path) -> Vec<String> {
            std::fs::read_dir(dir)
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.contains(".stale-"))
                .collect()
        }

        fn dead_pid() -> String {
            let mut child = Command::new("sh").args(["-c", "true"]).spawn().unwrap();
            let pid = child.id().to_string();
            child.wait().unwrap();
            pid
        }

        /// Error: a lock held by another live process is busy and names it.
        #[test]
        fn second_acquire_while_held_is_busy() {
            let dir = TestDir::new("lock-busy");
            let lock = dir.path().join(".crew-run.lock");
            let mut holder = Command::new("sleep").arg("30").spawn().unwrap();
            let pid = holder.id().to_string();
            std::fs::write(&lock, lock_content(&pid, "other-run")).unwrap();

            let result = RunLock::acquire(&lock, "run-b");

            let _ = holder.kill();
            let _ = holder.wait();
            let err = result.err().expect("a live holder must make the acquire busy");
            assert!(err.contains("project_root_busy"), "{err}");
            assert!(err.contains(&format!("pid {pid}")), "the message must name the holder's pid: {err}");
            assert_eq!(std::fs::read_to_string(&lock).unwrap(), lock_content(&pid, "other-run"));
        }

        /// Normal: after release the root is free again.
        #[test]
        fn release_then_acquire() {
            let dir = TestDir::new("lock-release");
            let lock = dir.path().join(".crew-run.lock");
            let slot: LockSlot = Arc::new(std::sync::Mutex::new(Some(RunLock::acquire(&lock, "run-a").unwrap())));

            release(&slot);

            assert!(slot.lock().unwrap().is_none());
            assert!(!lock.exists(), "release must remove the lock file");
            let again = RunLock::acquire(&lock, "run-b").expect("a released root is free");
            assert!(std::fs::read_to_string(&lock).unwrap().contains("run_id=run-b\n"));
            drop(again);
        }

        /// Boundary: the first run on a root finds no base dir yet.
        #[test]
        fn missing_base_dir_is_created_by_the_caller() {
            let dir = TestDir::new("lock-first");
            let base = dir.path().join("not").join("yet");
            let lock = base.join(".crew-run.lock");

            let held = RunLock::acquire(&lock, "run-a").expect("a missing base dir is created, not an error");

            let content = std::fs::read_to_string(&lock).unwrap();
            assert!(content.starts_with(&format!("pid={}\n", std::process::id())), "{content}");
            assert!(content.contains("run_id=run-a\n") && content.contains("started="), "{content}");
            drop(held);
            assert!(!lock.exists());
        }

        /// Normal: a lock left by a dead process is taken over.
        #[test]
        fn dead_pid_lock_is_stolen() {
            let dir = TestDir::new("lock-dead");
            let lock = dir.path().join(".crew-run.lock");
            std::fs::write(&lock, lock_content(&dead_pid(), "crashed-run")).unwrap();

            let held = RunLock::acquire(&lock, "run-a").expect("a dead holder's lock is stolen");

            assert!(std::fs::read_to_string(&lock).unwrap().contains("run_id=run-a\n"));
            assert!(stale_files(dir.path()).is_empty(), "no stale file may be left behind");
            drop(held);
        }

        /// Error: an unreadable owner is never stolen.
        #[test]
        fn unparseable_lock_is_busy() {
            let dir = TestDir::new("lock-garbage");
            let lock = dir.path().join(".crew-run.lock");
            std::fs::write(&lock, "garbage").unwrap();

            let err = RunLock::acquire(&lock, "run-a").err().expect("unparseable must be busy");

            assert!(err.contains("project_root_busy"), "{err}");
            assert_eq!(std::fs::read_to_string(&lock).unwrap(), "garbage");
        }

        /// Boundary: dropping a lock whose file now names another run leaves
        /// that file alone.
        #[test]
        fn drop_leaves_a_foreign_lock() {
            let dir = TestDir::new("lock-foreign");
            let lock = dir.path().join(".crew-run.lock");
            let held = RunLock::acquire(&lock, "run-a").unwrap();
            std::fs::write(&lock, lock_content("1", "run-z")).unwrap();

            drop(held);

            assert_eq!(std::fs::read_to_string(&lock).unwrap(), lock_content("1", "run-z"));
        }

        /// Error: the lock was replaced between the read and the rename, so the
        /// steal restores it and refuses.
        #[test]
        fn steal_detects_a_replaced_lock() {
            let dir = TestDir::new("lock-replaced");
            let lock = dir.path().join(".crew-run.lock");
            let stale = lock_content(&dead_pid(), "crashed-run");
            let live = lock_content(&std::process::id().to_string(), "live-run");
            std::fs::write(&lock, &live).unwrap();

            let err = steal_with(&lock, stale.as_bytes(), "run-a", &|| {}).err().expect("a replaced lock is busy");

            assert!(err.contains("project_root_busy"), "{err}");
            assert_eq!(std::fs::read_to_string(&lock).unwrap(), live);
            assert!(stale_files(dir.path()).is_empty(), "the restored lock leaves no stale file");
        }

        /// Error: when a third starter re-created the lock before the restore,
        /// the renamed file is left in place and the start refuses.
        #[test]
        fn three_racer_restore_failure_leaves_the_renamed_file() {
            let dir = TestDir::new("lock-three");
            let lock = dir.path().join(".crew-run.lock");
            let stale = lock_content(&dead_pid(), "crashed-run");
            std::fs::write(&lock, lock_content(&std::process::id().to_string(), "live-run")).unwrap();
            let third = lock.clone();

            let err = steal_with(&lock, stale.as_bytes(), "run-a", &|| {
                std::fs::write(&third, lock_content(&std::process::id().to_string(), "third-run")).unwrap();
            })
            .err()
            .expect("a failed restore refuses");

            assert!(err.contains("project_root_busy"), "{err}");
            assert_eq!(stale_files(dir.path()).len(), 1, "the renamed live lock must never be removed");
            assert!(std::fs::read_to_string(&lock).unwrap().contains("third-run"));
        }

        /// Concurrency: two starters racing over one dead lock never both hold.
        #[test]
        fn two_racers_never_both_hold() {
            let dir = TestDir::new("lock-race");
            let lock = dir.path().join(".crew-run.lock");
            for i in 0..50 {
                std::fs::write(&lock, lock_content(&dead_pid(), "crashed-run")).unwrap();
                let barrier = Arc::new(std::sync::Barrier::new(2));
                let racers: Vec<_> = (0..2)
                    .map(|n| {
                        let (lock, barrier) = (lock.clone(), barrier.clone());
                        std::thread::spawn(move || {
                            barrier.wait();
                            RunLock::acquire(&lock, &format!("run-{i}-{n}"))
                        })
                    })
                    .collect();
                let results: Vec<_> = racers.into_iter().map(|h| h.join().unwrap()).collect();

                let held = results.iter().filter(|r| r.is_ok()).count();
                assert_eq!(held, 1, "iteration {i}: exactly one racer may hold the lock: {results:?}", results = results.iter().map(|r| r.as_ref().err()).collect::<Vec<_>>());
                drop(results);
                assert!(stale_files(dir.path()).is_empty(), "iteration {i}: no stale file may be left");
            }
        }

        /// Error: a panic in the finisher still releases the lock while
        /// unwinding.
        #[test]
        fn finisher_panic_releases_the_lock() {
            let dir = TestDir::new("lock-panic");
            let lock = dir.path().join(".crew-run.lock");
            let slot: LockSlot = Arc::new(std::sync::Mutex::new(Some(RunLock::acquire(&lock, "run-a").unwrap())));
            let guarded = slot.clone();

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let _release = LockRelease(guarded);
                panic!("finisher panicked");
            }));

            assert!(result.is_err());
            assert!(!lock.exists(), "the lock must be released during unwinding");
            assert!(slot.lock().unwrap_or_else(|p| p.into_inner()).is_none());
        }

        /// Boundary: a guard dropped WITHOUT a panic (an aborted finisher)
        /// keeps the lock — shutdown releases it after the git phase ends.
        #[test]
        fn lock_release_without_a_panic_keeps_the_lock() {
            let dir = TestDir::new("lock-abort");
            let lock = dir.path().join(".crew-run.lock");
            let slot: LockSlot = Arc::new(std::sync::Mutex::new(Some(RunLock::acquire(&lock, "run-a").unwrap())));

            drop(LockRelease(slot.clone()));

            assert!(lock.exists(), "a plain drop must not release");
            release(&slot);
            assert!(!lock.exists());
        }

        /// D19/D21: shutdown's release waits for an in-flight git phase.
        #[tokio::test(flavor = "multi_thread")]
        async fn release_after_phase_waits_for_the_phase() {
            let dir = TestDir::new("lock-after-phase");
            let lock = dir.path().join(".crew-run.lock");
            let slot: LockSlot = Arc::new(std::sync::Mutex::new(Some(RunLock::acquire(&lock, "run-a").unwrap())));
            let phase = Arc::new(tokio::sync::Mutex::new(()));
            let in_flight = phase.clone().lock_owned().await;

            release_after_phase(phase.clone(), slot.clone());
            tokio::time::sleep(Duration::from_millis(150)).await;
            let held_during_phase = lock.exists();
            drop(in_flight);
            let deadline = Instant::now() + Duration::from_secs(5);
            while lock.exists() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            assert!(held_during_phase, "the lock must outlive an in-flight phase");
            assert!(!lock.exists(), "the lock is released once the phase ends");
            assert!(slot.lock().unwrap().is_none());
        }

        /// Boundary: a lock this very process holds says so instead of
        /// advising to delete the file.
        #[test]
        fn own_pid_busy_message_names_this_app() {
            let dir = TestDir::new("lock-own");
            let lock = dir.path().join(".crew-run.lock");
            let held = RunLock::acquire(&lock, "run-a").unwrap();

            let err = RunLock::acquire(&lock, "run-b").err().expect("the app's own live lock is busy");

            assert!(err.contains("project_root_busy: a run in this app"), "{err}");
            assert!(!err.contains("delete the file"), "{err}");
            drop(held);
        }
    }

    // --- Task 06: commit on accept -------------------------------------------

    /// Normal: the planted file is committed on crew/developer with the
    /// task's subject, and the event carries that commit's sha.
    #[test]
    fn commit_accepted_commits_the_planted_file() {
        let (_dir, _repo, wt, c) = dev_fixture("commit");
        write(&wt.join("hello.txt"), b"hello\n");
        let (tx, mut rx) = channel();

        let failed = c.commit_accepted(&dev_task(), &tx);

        assert!(!failed);
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        let (kind, role, branch, sha, task_id, _) = &events[0];
        assert_eq!(*kind, GitFlowKindDto::Commit);
        assert_eq!(role.as_deref(), Some("developer"));
        assert_eq!(branch.as_deref(), Some("crew/developer"));
        assert_eq!(task_id.as_deref(), Some("t-dev"));
        assert_eq!(sha.as_deref(), Some(git(&wt, &["rev-parse", "crew/developer"]).as_str()));
        assert_eq!(git(&wt, &["log", "-1", "--format=%s", "crew/developer"]), "crew(developer): t-dev — Build it");
        assert!(git(&wt, &["ls-tree", "-r", "--name-only", "crew/developer"]).lines().any(|l| l == "hello.txt"));
    }

    /// D14: changes already present when the run started are included and
    /// flagged; a role that started clean gets no such note.
    #[test]
    fn leftover_changes_are_flagged_in_the_commit_detail() {
        let dir = TestDir::new("leftover");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let base = dir.path().join("base");
        let dev = add_worktree(&repo, &base, Role::Developer);
        let qa = add_worktree(&repo, &base, Role::Qa);
        write(&dev.join("left-over.txt"), b"from an earlier run\n");
        let c = ctx(&repo, &[(Role::Developer, dev.clone()), (Role::Qa, qa.clone())]);
        write(&qa.join("report.md"), b"new work\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));
        assert!(!c.commit_accepted(&task("t-qa", Role::Qa, "Test it", Vec::new()), &tx));

        let events = drain(&mut rx);
        assert_eq!(events.len(), 2);
        assert!(events[0].5.contains("uncommitted changes before this run"), "{}", events[0].5);
        assert!(!events[1].5.contains("before this run"), "{}", events[1].5);
        assert!(tree(&dev, "crew/developer").contains(&"left-over.txt".to_string()));
        assert!(tree(&qa, "crew/qa").contains(&"report.md".to_string()), "each role commits in its own worktree");
    }

    fn tree(cwd: &Path, rev: &str) -> Vec<String> {
        git(cwd, &["ls-tree", "-r", "--name-only", rev]).lines().map(str::to_string).collect()
    }

    /// Boundary: a clean worktree is a skip, not a commit.
    #[test]
    fn commit_skips_a_clean_tree() {
        let (_dir, _repo, wt, c) = dev_fixture("clean");
        let before = git(&wt, &["rev-parse", "crew/developer"]);
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, GitFlowKindDto::Skip);
        assert!(events[0].5.contains("clean tree"), "{}", events[0].5);
        assert_eq!(git(&wt, &["rev-parse", "crew/developer"]), before);
    }

    /// Error: a worktree switched to another branch is refused before any write.
    #[test]
    fn commit_refuses_a_worktree_on_another_branch() {
        let (_dir, _repo, wt, c) = dev_fixture("other-branch");
        git(&wt, &["switch", "-q", "-c", "elsewhere"]);
        write(&wt.join("hello.txt"), b"hello\n");
        let before = git(&wt, &["rev-parse", "elsewhere"]);
        let (tx, mut rx) = channel();

        assert!(c.commit_accepted(&dev_task(), &tx), "a wrong branch is an error");

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, GitFlowKindDto::Error);
        assert_eq!(git(&wt, &["rev-parse", "elsewhere"]), before, "nothing may be committed");
        assert!(git(&wt, &["status", "--porcelain"]).contains("hello.txt"), "nothing may be staged either");
    }

    /// Error: a held index.lock surfaces as an error event with git's reason.
    #[test]
    fn commit_reports_an_index_lock_as_error() {
        let (_dir, _repo, wt, c) = dev_fixture("index-lock");
        write(&wt.join("hello.txt"), b"hello\n");
        let git_dir = PathBuf::from(git(&wt, &["rev-parse", "--absolute-git-dir"]));
        std::fs::write(git_dir.join("index.lock"), b"").unwrap();
        let (tx, mut rx) = channel();

        assert!(c.commit_accepted(&dev_task(), &tx));

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, GitFlowKindDto::Error);
        assert!(events[0].5.contains("index.lock"), "the detail must carry git's reason: {}", events[0].5);
        let _ = std::fs::remove_file(git_dir.join("index.lock"));
    }

    /// Error: a role worktree removed from disk is an error event, not a panic.
    #[test]
    fn commit_reports_a_removed_worktree_as_error() {
        let (_dir, _repo, wt, c) = dev_fixture("removed");
        std::fs::remove_dir_all(&wt).unwrap();
        let (tx, mut rx) = channel();

        assert!(c.commit_accepted(&dev_task(), &tx));

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, GitFlowKindDto::Error);
        assert!(!events[0].5.is_empty());
    }

    /// Normal: build output under excluded directories stays out of the
    /// commit and on disk.
    #[test]
    fn commit_excludes_build_dirs() {
        let (_dir, _repo, wt, c) = dev_fixture("excludes");
        write(&wt.join("src/a.rs"), b"fn main() {}\n");
        write(&wt.join("target/x"), b"bin\n");
        write(&wt.join("pkg/node_modules/y.js"), b"y\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        let events = drain(&mut rx);
        assert_eq!(events[0].0, GitFlowKindDto::Commit);
        assert!(events[0].5.contains("excluded 2 path(s)"), "{}", events[0].5);
        let tree = git(&wt, &["ls-tree", "-r", "--name-only", "crew/developer"]);
        assert_eq!(tree.lines().collect::<Vec<_>>(), vec!["README.md", "src/a.rs"]);
        assert!(wt.join("target/x").exists(), "excluded files stay in the worktree");
    }

    /// Boundary: only excluded paths changed -> skip.
    #[test]
    fn commit_with_only_excluded_changes_is_a_skip() {
        let (_dir, _repo, wt, c) = dev_fixture("only-excluded");
        write(&wt.join("target/x"), b"bin\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        let events = drain(&mut rx);
        assert_eq!(events[0].0, GitFlowKindDto::Skip);
        assert!(events[0].5.contains("only excluded paths changed"), "{}", events[0].5);
        assert!(events[0].5.contains("excluded 1 path(s): target/x"), "{}", events[0].5);
    }

    /// Normal: an agent-written .claude/ settings file never reaches a commit.
    #[test]
    fn commit_excludes_claude_settings() {
        let (_dir, _repo, wt, c) = dev_fixture("claude");
        write(&wt.join(".claude/settings.json"), b"{\"permissions\":{\"allow\":[\"Bash\"]}}");
        write(&wt.join("a.txt"), b"a\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Commit);
        let tree = git(&wt, &["ls-tree", "-r", "--name-only", "crew/developer"]);
        assert!(!tree.contains(".claude"), "{tree}");
        assert!(tree.lines().any(|l| l == "a.txt"));
    }

    /// Boundary: a plain FILE named like an excluded dir is committed.
    #[test]
    fn a_plain_file_named_dist_is_committed_and_counted() {
        let (_dir, _repo, wt, c) = dev_fixture("dist-file");
        write(&wt.join("dist"), b"not a directory\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        let events = drain(&mut rx);
        assert_eq!(events[0].0, GitFlowKindDto::Commit);
        assert!(!events[0].5.contains("excluded"), "{}", events[0].5);
        assert!(git(&wt, &["ls-tree", "-r", "--name-only", "crew/developer"]).lines().any(|l| l == "dist"));
        assert!(!excluded("dist") && !excluded("a/dist") && excluded("dist/x") && excluded("a/.claude/b"));
    }

    fn sparse(path: &Path, len: u64) {
        let f = std::fs::File::create(path).unwrap();
        f.set_len(len).unwrap();
    }

    /// Error: more than 50 MiB of changes are refused, nothing staged.
    #[test]
    fn commit_guard_refuses_over_50_mib() {
        let (_dir, _repo, wt, c) = dev_fixture("guard-over");
        sparse(&wt.join("big.bin"), GUARD_MAX_BYTES + 1024 * 1024);
        let before = git(&wt, &["rev-parse", "crew/developer"]);
        let (tx, mut rx) = channel();

        assert!(c.commit_accepted(&dev_task(), &tx));

        let events = drain(&mut rx);
        assert_eq!(events[0].0, GitFlowKindDto::Error);
        assert!(events[0].5.contains("50 MiB"), "{}", events[0].5);
        assert_eq!(git(&wt, &["rev-parse", "crew/developer"]), before);
        assert_eq!(git(&wt, &["diff", "--cached", "--name-only"]), "", "nothing may be staged");
    }

    /// Boundary: exactly 50 MiB is accepted.
    #[test]
    fn exactly_50_mib_is_accepted() {
        let (_dir, _repo, wt, c) = dev_fixture("guard-exact");
        sparse(&wt.join("big.bin"), GUARD_MAX_BYTES);
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Commit);
    }

    /// Boundary: deleting a large tracked file counts zero bytes.
    #[test]
    fn a_deleted_large_file_counts_zero() {
        let (_dir, _repo, wt, c) = dev_fixture("guard-deleted");
        sparse(&wt.join("big.bin"), 60 * 1024 * 1024);
        git(&wt, &["add", "big.bin"]);
        git(&wt, &["commit", "-q", "-m", "setup: large file"]);
        std::fs::remove_file(wt.join("big.bin")).unwrap();
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Commit);
        assert!(!git(&wt, &["ls-tree", "-r", "--name-only", "crew/developer"]).contains("big.bin"));
    }

    /// D7: commit.gpgsign=true with a broken gpg program still commits
    /// (unsigned) — proven against a plain commit that fails.
    #[test]
    fn commit_ignores_commit_gpgsign() {
        let (_dir, repo, wt, c) = dev_fixture("gpgsign");
        git(&repo, &["config", "commit.gpgsign", "true"]);
        git(&repo, &["config", "gpg.program", "/nonexistent/gpg"]);
        assert!(!git_out(&wt, &["commit", "--allow-empty", "-q", "-m", "x"]).status.success(), "test setup: a plain commit must fail to sign");
        write(&wt.join("hello.txt"), b"hello\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Commit);
    }

    /// D15: with no identity configured anywhere, the fallback identity is used.
    #[test]
    fn commit_uses_the_fallback_identity_without_any_config() {
        let dir = TestDir::new("identity");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), b"init\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["-c", "user.name=Setup", "-c", "user.email=setup@example.com", "commit", "-q", "-m", "init"]);
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        let mut c = ctx(&repo, &[(Role::Developer, wt.clone())]);
        c.extra_env = vec![
            ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ];
        write(&wt.join("hello.txt"), b"hello\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx), "{:?}", drain(&mut rx));

        assert_eq!(git(&wt, &["log", "-1", "--format=%an <%ae>", "crew/developer"]), "linkly-crew <crew@linkly-crew.invalid>");
    }

    /// D6: commit_accepted runs no pre-commit/post-commit hook.
    #[test]
    fn hooks_are_disabled_for_commit() {
        let (dir, repo, wt, c) = dev_fixture("commit-hooks");
        let marker = dir.path().join("marker");
        let hooks = dir.path().join("hooks");
        write_hook(&hooks, "pre-commit", &marker);
        write_hook(&hooks, "post-commit", &marker);
        git(&repo, &["config", "core.hooksPath", &hooks.to_string_lossy()]);
        write(&wt.join("hello.txt"), b"hello\n");
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Commit);
        assert!(!marker.exists());
    }

    /// D19: a cancelled ctx starts no commit and emits nothing.
    #[test]
    fn a_cancelled_commit_does_nothing() {
        let (_dir, _repo, wt, c) = dev_fixture("commit-cancelled");
        write(&wt.join("hello.txt"), b"hello\n");
        c.cancel();
        let calls = c.git_calls();
        let (tx, mut rx) = channel();

        assert!(!c.commit_accepted(&dev_task(), &tx));

        assert!(drain(&mut rx).is_empty());
        assert_eq!(c.git_calls(), calls);
    }

    #[test]
    fn identity_overrides_cover_each_missing_key() {
        assert!(identity_overrides(true, true).is_empty());
        assert_eq!(identity_overrides(false, false), vec!["-c", FALLBACK_NAME, "-c", FALLBACK_EMAIL]);
        assert_eq!(identity_overrides(true, false), vec!["-c", FALLBACK_EMAIL]);
    }

    #[test]
    fn commit_message_formats() {
        let req = |s: &str| ReqId::new(s).unwrap();
        let with_reqs = task(
            "t-dev",
            Role::Developer,
            "  Build the page  \nsecond line",
            vec![
                DodCheck::ReqCover { ids: vec![req("REQ-2"), req("REQ-1")] },
                DodCheck::ReqCover { ids: vec![req("REQ-2")] },
            ],
        );
        assert_eq!(commit_message(Role::Developer, &with_reqs), "crew(developer): t-dev — Build the page [REQ-2, REQ-1]");
        assert_eq!(commit_message(Role::Qa, &task("t-qa", Role::Qa, "", Vec::new())), "crew(qa): t-qa");
        assert_eq!(commit_message(Role::Pm, &task("t-pm", Role::Pm, "Spec", Vec::new())), "crew(pm): t-pm — Spec");
    }

    // --- Task 07: sprint merge --------------------------------------------------

    fn commit_in(wt: &Path, file: &str, content: &[u8]) -> String {
        write(&wt.join(file), content);
        git(wt, &["add", file]);
        git(wt, &["commit", "-q", "-m", &format!("setup: {file}")]);
        git(wt, &["rev-parse", "HEAD"])
    }

    /// Normal: an ahead role becomes a --no-ff merge commit on main.
    #[test]
    fn merge_ahead_role_creates_a_merge_commit() {
        let (_dir, repo, wt, c) = dev_fixture("merge");
        commit_in(&wt, "hello.txt", b"hello\n");
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert_eq!(report, MergeReport { failures: 0, merged_any: true, not_landed: None });
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Merge);
        assert_eq!(events[0].3.as_deref(), Some(git(&repo, &["rev-parse", "main"]).as_str()));
        let parents = git(&repo, &["rev-list", "--parents", "-n", "1", "main"]);
        assert_eq!(parents.split_whitespace().count(), 3, "a merge commit has two parents: {parents}");
        assert_eq!(git(&repo, &["log", "-1", "--format=%s", "main"]), "crew: merge crew/developer (sprint 0)");
        assert_eq!(std::fs::read(repo.join("hello.txt")).unwrap(), b"hello\n");
    }

    /// Boundary: a dirty main checkout is skipped and left byte-identical.
    #[test]
    fn merge_skips_a_dirty_main_checkout() {
        let (_dir, repo, wt, c) = dev_fixture("merge-dirty");
        commit_in(&wt, "hello.txt", b"hello\n");
        std::fs::write(repo.join("notes.txt"), b"the user's own work\n").unwrap();
        let before = git(&repo, &["rev-parse", "main"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert!(!report.merged_any);
        assert!(report.not_landed.as_deref().unwrap_or("").contains("uncommitted"), "{report:?}");
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, GitFlowKindDto::Skip);
        assert_eq!(events[0].1, None, "a sprint-level skip names no role");
        assert_eq!(git(&repo, &["rev-parse", "main"]), before);
        assert_eq!(std::fs::read(repo.join("notes.txt")).unwrap(), b"the user's own work\n");
    }

    /// Boundary: a main checkout on another branch is skipped.
    #[test]
    fn merge_skips_when_root_is_not_on_main() {
        let (_dir, repo, wt, c) = dev_fixture("merge-off-main");
        commit_in(&wt, "hello.txt", b"hello\n");
        git(&repo, &["switch", "-q", "-c", "feature"]);
        let before = git(&repo, &["rev-parse", "main"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert!(report.not_landed.is_some() && !report.merged_any);
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, GitFlowKindDto::Skip);
        assert!(events[0].5.contains("not on main"), "{}", events[0].5);
        assert_eq!(git(&repo, &["rev-parse", "main"]), before);
    }

    /// Error: a status command that fails is an error, never read as clean.
    #[test]
    fn merge_status_failure_is_an_error_not_clean() {
        let (_dir, repo, wt, c) = dev_fixture("merge-status-fail");
        commit_in(&wt, "hello.txt", b"hello\n");
        let before = git(&repo, &["rev-parse", "main"]);
        std::fs::write(repo.join(".git/index"), b"").unwrap();
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert_eq!(report.failures, 1);
        assert!(!report.merged_any);
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, GitFlowKindDto::Error);
        assert_eq!(git(&repo, &["rev-parse", "main"]), before);
    }

    /// Error: a two-role conflict is aborted; main, the checkout and the role
    /// branch are exactly as before that merge.
    #[test]
    fn merge_conflict_is_aborted_and_the_role_branch_is_intact() {
        let dir = TestDir::new("merge-conflict");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let base = dir.path().join("base");
        let dev = add_worktree(&repo, &base, Role::Developer);
        let qa = add_worktree(&repo, &base, Role::Qa);
        commit_in(&dev, "shared.txt", b"developer\n");
        let qa_sha = commit_in(&qa, "shared.txt", b"qa\n");
        let c = ctx(&repo, &[(Role::Developer, dev), (Role::Qa, qa)]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(1, true, &tx);

        assert_eq!(report.failures, 1);
        assert!(report.merged_any, "the developer merge before the conflict still landed");
        let events = drain(&mut rx);
        let kinds: Vec<_> = events.iter().map(|e| e.0).collect();
        assert_eq!(kinds, vec![GitFlowKindDto::Merge, GitFlowKindDto::Conflict], "{events:?}");
        let detail = &events[1].5;
        assert!(detail.contains("shared.txt"), "{detail}");
        assert!(detail.contains("reconciled with main by hand"), "{detail}");
        assert_eq!(events[1].3, None);
        assert_eq!(git_out(&repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).status.code(), Some(1), "MERGE_HEAD must be gone");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), events[0].3.clone().unwrap(), "HEAD stays at the developer merge");
        assert_eq!(git(&repo, &["status", "--porcelain"]), "");
        assert_eq!(git(&repo, &["rev-parse", "crew/qa"]), qa_sha, "the role branch is never modified");
        assert_eq!(std::fs::read(repo.join("shared.txt")).unwrap(), b"developer\n");
    }

    /// Boundary (D4): without a main branch the merge is one sprint-level
    /// skip, and it counts as not landed only when role work exists.
    #[test]
    fn no_main_branch_is_a_skip_that_counts_only_real_work() {
        let dir = TestDir::new("merge-no-main");
        let repo = dir.path().join("repo");
        init_repo(&repo, "trunk");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        let c = ctx(&repo, &[(Role::Developer, wt.clone())]);
        let (tx, mut rx) = channel();

        let idle = c.merge_sprint(0, true, &tx);
        commit_in(&wt, "hello.txt", b"hello\n");
        let busy = c.merge_sprint(1, true, &tx);

        assert_eq!(idle.not_landed, None, "nothing ahead: the skip is harmless");
        assert!(busy.not_landed.as_deref().unwrap_or("").contains("no main branch"), "{busy:?}");
        let events = drain(&mut rx);
        assert_eq!(events.len(), 2, "one sprint-level skip per sprint: {events:?}");
        assert!(events.iter().all(|e| e.0 == GitFlowKindDto::Skip && e.1.is_none()));
        assert_eq!(git(&repo, &["rev-parse", "trunk"]), git(&repo, &["rev-parse", "HEAD"]), "nothing merged");
    }

    /// Review r1 F1: a role branch that commits `.env` must never overwrite
    /// the user's ignored, untracked `.env` in the main checkout — the merge
    /// for that role is refused as a Conflict, and the next role still merges.
    #[test]
    fn merge_never_overwrites_an_ignored_user_file() {
        let dir = TestDir::new("merge-ignored");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let base = dir.path().join("base");
        let dev = add_worktree(&repo, &base, Role::Developer);
        let qa = add_worktree(&repo, &base, Role::Qa);
        commit_in(&dev, ".env", b"FROM_AGENT=1\n");
        commit_in(&qa, "report.md", b"qa\n");
        commit_on_main(&repo, ".gitignore", b".env\n");
        std::fs::write(repo.join(".env"), b"USER_SECRET=precious\n").unwrap();
        assert_eq!(git(&repo, &["status", "--porcelain"]), "", "test setup: status cannot see the ignored file");
        let c = ctx(&repo, &[(Role::Developer, dev), (Role::Qa, qa)]);
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert_eq!(std::fs::read(repo.join(".env")).unwrap(), b"USER_SECRET=precious\n", "the user's file is untouched");
        let events = drain(&mut rx);
        let kinds: Vec<_> = events.iter().map(|e| e.0).collect();
        assert_eq!(kinds, vec![GitFlowKindDto::Conflict, GitFlowKindDto::Merge], "{events:?}");
        assert_eq!(events[0].1.as_deref(), Some("developer"));
        assert_eq!(events[0].2.as_deref(), Some("crew/developer"));
        assert!(events[0].5.contains("would overwrite untracked/ignored file(s) in the main checkout: .env"), "{}", events[0].5);
        assert_eq!(events[1].1.as_deref(), Some("qa"), "the next role still merges");
        assert!(report.not_landed.is_some(), "{report:?}");
        assert!(!tree(&repo, "main").contains(&".env".to_string()), "the agent's .env never reaches main");
        assert_eq!(git(&repo, &["rev-parse", "HEAD^1"]), before, "only the qa merge moved main");
    }

    /// F1 boundary: an ignored FILE standing where the role branch adds a
    /// directory is a collision too.
    #[test]
    fn an_ignored_file_in_the_way_of_a_new_directory_is_a_collision() {
        let (_dir, repo, wt, c) = dev_fixture("merge-ignored-dir");
        commit_in(&wt, "docs/readme.md", b"agent docs\n");
        commit_on_main(&repo, ".gitignore", b"docs\n");
        std::fs::write(repo.join("docs"), b"the user's notes\n").unwrap();
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert_eq!(std::fs::read(repo.join("docs")).unwrap(), b"the user's notes\n");
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Conflict);
        assert!(events[0].5.contains("docs"), "{}", events[0].5);
        assert!(report.not_landed.is_some() && !report.merged_any);
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
    }

    /// F1 boundary: a path TRACKED on main is git's own business (a clean
    /// merge or a real conflict) — never a false positive.
    #[test]
    fn a_path_tracked_on_main_is_not_a_collision() {
        let dir = TestDir::new("merge-tracked");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        commit_on_main(&repo, "config.txt", b"a\n");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        commit_in(&wt, "config.txt", b"b\n");
        let c = ctx(&repo, &[(Role::Developer, wt)]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert!(report.merged_any && report.not_landed.is_none(), "{report:?}");
        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Merge);
        assert_eq!(std::fs::read(repo.join("config.txt")).unwrap(), b"b\n");
    }

    /// F1: an ignored SYMLINK (here dangling, so only an lstat sees it) in the
    /// way of the role's file is a collision, and stays a symlink.
    #[test]
    fn an_ignored_symlink_in_the_way_is_a_collision() {
        let (_dir, repo, wt, c) = dev_fixture("merge-ignored-link");
        commit_in(&wt, "local.cfg", b"from the agent\n");
        commit_on_main(&repo, ".gitignore", b"local.cfg\n");
        std::os::unix::fs::symlink("/nonexistent/user-config", repo.join("local.cfg")).unwrap();
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        let meta = std::fs::symlink_metadata(repo.join("local.cfg")).unwrap();
        assert!(meta.file_type().is_symlink(), "the user's symlink is untouched");
        assert_eq!(std::fs::read_link(repo.join("local.cfg")).unwrap(), Path::new("/nonexistent/user-config"));
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Conflict);
        assert!(events[0].5.contains("local.cfg"), "{}", events[0].5);
        assert!(report.not_landed.is_some() && !report.merged_any);
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
    }

    /// F1 boundary: a new file inside a directory main already tracks (the
    /// ordinary layout) merges — an existing directory is never a collision.
    #[test]
    fn a_new_file_in_a_tracked_directory_merges() {
        let dir = TestDir::new("merge-tracked-dir");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        commit_on_main(&repo, "src/lib/a.rs", b"a\n");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        commit_in(&wt, "src/lib/b.rs", b"b\n");
        let c = ctx(&repo, &[(Role::Developer, wt)]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert!(report.merged_any && report.not_landed.is_none(), "{report:?}");
        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Merge);
        assert_eq!(std::fs::read(repo.join("src/lib/b.rs")).unwrap(), b"b\n");
    }

    /// Review r2: main renamed `src` to `lib` after the role branched and the
    /// role adds `src/notes.txt`. Directory-rename detection must not move it
    /// onto the user's ignored `lib/notes.txt` (which the collision check,
    /// seeing only `src/notes.txt`, cannot protect).
    fn renamed_dir_fixture(label: &str, user_file: bool) -> (TestDir, PathBuf, GitFlowCtx, String) {
        let dir = TestDir::new(label);
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        commit_on_main(&repo, "src/a.rs", b"a\n");
        commit_on_main(&repo, "src/b.rs", b"b\n");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        commit_in(&wt, "src/notes.txt", b"FROM_AGENT=1\n");
        git(&repo, &["mv", "src", "lib"]);
        git(&repo, &["commit", "-q", "-m", "rename src to lib"]);
        commit_on_main(&repo, ".gitignore", b"lib/notes.txt\n");
        if user_file {
            std::fs::write(repo.join("lib/notes.txt"), b"USER_NOTES=precious\n").unwrap();
        }
        let c = ctx(&repo, &[(Role::Developer, wt)]);
        let before = git(&repo, &["rev-parse", "HEAD"]);
        (dir, repo, c, before)
    }

    #[test]
    fn a_directory_renamed_on_main_never_relocates_onto_a_user_file() {
        let (_dir, repo, c, before) = renamed_dir_fixture("merge-dir-rename", true);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert_eq!(
            std::fs::read(repo.join("lib/notes.txt")).ok().as_deref(),
            Some(&b"USER_NOTES=precious\n"[..]),
            "the user's ignored file is neither overwritten nor deleted"
        );
        let events = drain(&mut rx);
        assert_eq!(events.iter().map(|e| e.0).collect::<Vec<_>>(), vec![GitFlowKindDto::Merge], "{events:?}");
        assert!(report.merged_any && report.not_landed.is_none() && report.failures == 0, "{report:?}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD^1"]), before, "a merge commit landed on main");
        let tracked = tree(&repo, "main");
        assert!(tracked.contains(&"src/notes.txt".to_string()), "the role's file stays at its own path: {tracked:?}");
        assert!(!tracked.contains(&"lib/notes.txt".to_string()), "{tracked:?}");
    }

    /// Boundary: the same rename without the user file merges the same way.
    #[test]
    fn a_directory_renamed_on_main_merges_at_the_role_path() {
        let (_dir, repo, c, before) = renamed_dir_fixture("merge-dir-rename-clean", false);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert!(report.merged_any && report.failures == 0, "{report:?}");
        assert_eq!(drain(&mut rx).len(), 1);
        assert_eq!(git(&repo, &["rev-parse", "HEAD^1"]), before);
        assert_eq!(std::fs::read(repo.join("src/notes.txt")).unwrap(), b"FROM_AGENT=1\n");
        assert!(!repo.join("lib/notes.txt").exists());
    }

    /// Review r3: the role adds `cfg/x` while main added a FILE `cfg` since the
    /// branch point, so ort would move main's `cfg` aside to `cfg~HEAD` — onto
    /// the user's ignored `cfg~HEAD`, which `merge --abort` then deletes.
    fn tilde_fixture(label: &str) -> (TestDir, PathBuf, PathBuf, GitFlowCtx) {
        let dir = TestDir::new(label);
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        commit_in(&wt, "cfg/x", b"from the agent\n");
        commit_on_main(&repo, "cfg", b"MAINCFG\n");
        let c = ctx(&repo, &[(Role::Developer, wt.clone())]);
        (dir, repo, wt, c)
    }

    #[test]
    fn a_conflict_relocation_name_never_overwrites_a_user_file() {
        let (_dir, repo, _wt, c) = tilde_fixture("merge-tilde");
        commit_on_main(&repo, ".gitignore", b"*~*\n");
        std::fs::write(repo.join("cfg~HEAD"), b"USER_CFG=precious\n").unwrap();
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert_eq!(
            std::fs::read(repo.join("cfg~HEAD")).ok().as_deref(),
            Some(&b"USER_CFG=precious\n"[..]),
            "the user's ignored cfg~HEAD is neither overwritten nor deleted"
        );
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Conflict);
        assert!(events[0].5.contains("would overwrite untracked/ignored file(s) in the main checkout: cfg~HEAD"), "{}", events[0].5);
        assert!(report.not_landed.is_some() && !report.merged_any, "{report:?}");
    }

    /// Boundary: a TRACKED `cfg~HEAD` is not a collision — the ordinary
    /// file/directory conflict path runs (abort, Conflict, branches intact).
    #[test]
    fn a_tracked_relocation_name_takes_the_ordinary_conflict_path() {
        let (_dir, repo, wt, c) = tilde_fixture("merge-tilde-tracked");
        commit_on_main(&repo, "cfg~HEAD", b"TRACKED\n");
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let branch = git(&wt, &["rev-parse", "crew/developer"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Conflict);
        assert!(events[0].5.contains("merge conflict in"), "{}", events[0].5);
        assert!(!events[0].5.contains("would overwrite"), "no false positive: {}", events[0].5);
        assert_eq!(report.failures, 1);
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
        assert_eq!(git(&repo, &["status", "--porcelain"]), "");
        assert_eq!(git(&wt, &["rev-parse", "crew/developer"]), branch);
        assert_eq!(std::fs::read(repo.join("cfg~HEAD")).unwrap(), b"TRACKED\n");
    }

    /// Boundary: untracked siblings that do not start with `<name>~` never
    /// block a clean merge.
    #[test]
    fn unrelated_ignored_siblings_do_not_block_a_merge() {
        let (_dir, repo, wt, c) = dev_fixture("merge-siblings");
        commit_in(&wt, "cfg", b"agent cfg\n");
        commit_on_main(&repo, ".gitignore", b"cfg2\ncfg.bak\n");
        std::fs::write(repo.join("cfg2"), b"user 2\n").unwrap();
        std::fs::write(repo.join("cfg.bak"), b"user bak\n").unwrap();
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert!(report.merged_any && report.not_landed.is_none(), "{report:?}");
        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Merge);
        assert_eq!(std::fs::read(repo.join("cfg")).unwrap(), b"agent cfg\n");
        assert_eq!(std::fs::read(repo.join("cfg2")).unwrap(), b"user 2\n");
        assert_eq!(std::fs::read(repo.join("cfg.bak")).unwrap(), b"user bak\n");
    }

    /// Review r3 ancestor rule: `<ancestor>~*` siblings are examined too.
    #[test]
    fn an_ancestor_relocation_name_is_a_collision() {
        let (_dir, repo, wt, c) = dev_fixture("merge-tilde-ancestor");
        commit_in(&wt, "a/b/new.txt", b"new\n");
        commit_on_main(&repo, ".gitignore", b"a~HEAD\n");
        std::fs::write(repo.join("a~HEAD"), b"user\n").unwrap();
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Conflict);
        assert!(events[0].5.contains("a~HEAD"), "{}", events[0].5);
        assert!(report.not_landed.is_some());
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before);
        assert_eq!(std::fs::read(repo.join("a~HEAD")).unwrap(), b"user\n");
    }

    /// F1 boundary: paths absent from the checkout's disk merge as before.
    #[test]
    fn paths_absent_from_disk_merge_as_before() {
        let (_dir, repo, wt, c) = dev_fixture("merge-absent");
        commit_in(&wt, "new/dir/file.txt", b"new\n");
        let (tx, mut rx) = channel();

        let report = c.merge_sprint(0, true, &tx);

        assert!(report.merged_any && report.not_landed.is_none(), "{report:?}");
        assert_eq!(drain(&mut rx)[0].0, GitFlowKindDto::Merge);
        assert_eq!(std::fs::read(repo.join("new/dir/file.txt")).unwrap(), b"new\n");
    }

    /// A git dir whose config runs `fsmonitor.sh`, which writes `marker`.
    fn fsmonitor_repo(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let repo = dir.join(name);
        init_repo(&repo, "main");
        let marker = dir.join(format!("{name}-fsmonitor-ran"));
        let script = dir.join(format!("{name}-fsmonitor.sh"));
        std::fs::write(&script, format!("#!/bin/sh\necho ran >> '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&repo, &["config", "core.fsmonitor", &script.to_string_lossy()]);
        (repo, marker)
    }

    /// Review r1 F2: a role worktree whose `.git` pointer was redirected to a
    /// foreign git dir gets NO git command — so that dir's config never runs —
    /// and is an error excluded from commit and merge.
    #[test]
    fn a_redirected_role_worktree_runs_no_git_and_is_an_error() {
        let dir = TestDir::new("redirected");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        commit_in(&wt, "hello.txt", b"hello\n");
        let (evil, marker) = fsmonitor_repo(dir.path(), "evil");
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", evil.join(".git").display())).unwrap();
        write(&wt.join("more.txt"), b"more\n");
        let _ = git_out(&wt, &["status", "--porcelain"]);
        assert!(marker.exists(), "test setup: a plain git command in the worktree runs the foreign fsmonitor");
        std::fs::remove_file(&marker).unwrap();
        let main_before = git(&repo, &["rev-parse", "main"]);
        let baseline = ctx(&repo, &[]).git_calls();

        let c = ctx(&repo, &[(Role::Developer, wt.clone())]);
        let after_start = c.git_calls();
        let (tx, mut rx) = channel();
        let failed = c.commit_accepted(&dev_task(), &tx);
        let after_commit = c.git_calls();
        let report = c.merge_sprint(0, true, &tx);

        assert_eq!(after_start, baseline, "for_run ran no git command against the redirected worktree");
        assert!(failed);
        assert_eq!(after_commit, after_start, "the commit path ran no git command against it");
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "one error, no merge: {events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Error);
        assert!(events[0].5.contains("is not a worktree of this repository"), "{}", events[0].5);
        assert!(!report.merged_any);
        assert_eq!(git(&repo, &["rev-parse", "main"]), main_before, "an excluded role is never merged");
        assert!(!marker.exists(), "the foreign config must never run");

        // Without a commit phase first, the merge phase still reports it once.
        let fresh = ctx(&repo, &[(Role::Developer, wt)]);
        let report = fresh.merge_sprint(0, true, &tx);
        let events = drain(&mut rx);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].0, GitFlowKindDto::Error);
        assert_eq!(report.failures, 1, "it counts as a git failure for the gate");
        assert_eq!(git(&repo, &["rev-parse", "main"]), main_before);
        assert!(!marker.exists());
    }

    /// Review r1 F2 defence in depth: a repo's own core.fsmonitor never runs
    /// under the flow's runner.
    #[test]
    fn run_git_bounded_disables_a_configured_fsmonitor() {
        let dir = TestDir::new("fsmonitor");
        let (repo, marker) = fsmonitor_repo(dir.path(), "repo");
        std::fs::write(repo.join("new.txt"), b"x\n").unwrap();
        let _ = git_out(&repo, &["status", "--porcelain"]);
        assert!(marker.exists(), "test setup: plain git runs the configured fsmonitor");
        std::fs::remove_file(&marker).unwrap();

        let out = run_git_bounded(&repo, &["status", "--porcelain"], GIT_LOCAL_TIMEOUT, &[], None).unwrap();

        assert!(out.ok, "{}", out.stderr);
        assert!(String::from_utf8_lossy(&out.stdout).contains("new.txt"));
        assert!(!marker.exists(), "-c core.fsmonitor=false must win over the repo config");
    }

    /// F2 boundary: only a `.git` FILE pointing under <root>/.git/worktrees/
    /// passes.
    #[test]
    fn role_worktree_is_ours_accepts_a_real_worktree_and_rejects_others() {
        let dir = TestDir::new("is-ours");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        let wt = add_worktree(&repo, &dir.path().join("base"), Role::Developer);
        let root = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(role_worktree_is_ours(&root, &wt), Ok(()));

        let nested = dir.path().join("nested");
        init_repo(&nested, "main");
        assert!(role_worktree_is_ours(&root, &nested).unwrap_err().contains("not a regular file"), "a .git directory fails");

        let linked = dir.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::os::unix::fs::symlink(wt.join(".git"), linked.join(".git")).unwrap();
        assert!(role_worktree_is_ours(&root, &linked).is_err(), "a symlinked .git fails");

        let main_dir = dir.path().join("main-dir");
        std::fs::create_dir_all(&main_dir).unwrap();
        std::fs::write(main_dir.join(".git"), format!("gitdir: {}\n", root.join(".git").display())).unwrap();
        assert!(role_worktree_is_ours(&root, &main_dir).is_err(), "the main .git dir is not a worktree dir");

        let pointer = std::fs::read_to_string(wt.join(".git")).unwrap();
        std::fs::write(wt.join(".git"), format!("{pointer}gitdir: /elsewhere\n")).unwrap();
        assert!(role_worktree_is_ours(&root, &wt).is_err(), "more than one line fails");
        std::fs::write(wt.join(".git"), b"").unwrap();
        assert!(role_worktree_is_ours(&root, &wt).is_err(), "an empty pointer fails");
    }

    /// D5 recovery (review r4): when `merge --abort` cannot be verified (here
    /// MERGE_HEAD is left behind while HEAD and the tree look restored), the
    /// event is an Error saying the root was NOT restored — never a Conflict
    /// claiming it is unchanged — and no later role is merged, in this sprint
    /// or the next.
    #[test]
    fn a_failed_abort_blocks_every_later_merge() {
        let dir = TestDir::new("merge-abort-fails");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        commit_on_main(&repo, "shared.txt", b"base\n");
        let base = dir.path().join("base");
        let dev = add_worktree(&repo, &base, Role::Developer);
        let qa = add_worktree(&repo, &base, Role::Qa);
        commit_in(&dev, "shared.txt", b"developer\n");
        commit_in(&qa, "report.md", b"qa\n");
        commit_on_main(&repo, "shared.txt", b"main\n");
        let mut c = ctx(&repo, &[(Role::Developer, dev), (Role::Qa, qa)]);
        c.abort_leaves_merge_head = true;
        let before = git(&repo, &["rev-parse", "HEAD"]);
        let (tx, mut rx) = channel();

        let first = c.merge_sprint(0, true, &tx);
        let first_events = drain(&mut rx);
        let second = c.merge_sprint(1, true, &tx);
        let second_events = drain(&mut rx);

        assert_eq!(first_events.len(), 1, "the sprint stops at the unverified abort: {first_events:?}");
        assert_eq!(first_events[0].0, GitFlowKindDto::Error, "never a Conflict claiming the root is unchanged");
        assert_eq!(first_events[0].1.as_deref(), Some("developer"));
        assert!(first_events[0].5.contains("did not restore"), "{}", first_events[0].5);
        assert!(first.failures >= 1 && !first.merged_any, "{first:?}");
        assert_eq!(second_events.len(), 1, "{second_events:?}");
        assert_eq!(second_events[0].0, GitFlowKindDto::Skip);
        assert!(second_events[0].5.contains("merges are blocked"), "{}", second_events[0].5);
        assert!(second.not_landed.is_some() && !second.merged_any, "{second:?}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), before, "no merge commit landed");
        assert_eq!(
            git_out(&repo, &["merge-base", "--is-ancestor", "crew/qa", "HEAD"]).status.code(),
            Some(1),
            "the second role's branch was not merged"
        );
    }

    /// Boundary: no role ahead of main -> no event at all.
    #[test]
    fn no_role_ahead_emits_nothing() {
        let (_dir, _repo, _wt, c) = dev_fixture("merge-nothing");
        let (tx, mut rx) = channel();

        assert_eq!(c.merge_sprint(0, true, &tx), MergeReport::default());
        assert!(drain(&mut rx).is_empty());
    }

    /// Boundary: a sprint whose lead did not finish Ok merges nothing and
    /// runs no git command.
    #[test]
    fn sprint_not_ok_merges_nothing() {
        let (_dir, repo, wt, c) = dev_fixture("merge-not-ok");
        commit_in(&wt, "hello.txt", b"hello\n");
        let before = git(&repo, &["rev-parse", "main"]);
        let calls = c.git_calls();
        let (tx, mut rx) = channel();

        assert_eq!(c.merge_sprint(0, false, &tx), MergeReport::default());

        assert!(drain(&mut rx).is_empty());
        assert_eq!(c.git_calls(), calls);
        assert_eq!(git(&repo, &["rev-parse", "main"]), before);
    }

    /// D19: once cancelled, no new command group starts.
    #[test]
    fn cancelled_phase_starts_no_new_group() {
        let (_dir, repo, wt, c) = dev_fixture("merge-cancelled");
        commit_in(&wt, "hello.txt", b"hello\n");
        let before = git(&repo, &["rev-parse", "main"]);
        c.cancel();
        let calls = c.git_calls();
        let (tx, mut rx) = channel();

        assert_eq!(c.merge_sprint(0, true, &tx), MergeReport::default());
        c.push_main(true, true, &tx);

        assert!(drain(&mut rx).is_empty());
        assert_eq!(c.git_calls(), calls, "no git command after cancel");
        assert_eq!(git(&repo, &["rev-parse", "main"]), before);
    }

    // --- Task 08: gate, scan, push ------------------------------------------------

    fn gate_dag(dod: Vec<DodCheck>) -> TaskDag {
        TaskDag {
            tasks: vec![task("t-dev", Role::Developer, "dev", dod), task("t-pm", Role::Pm, "pm", Vec::new())],
        }
    }

    fn states(dev: TaskState) -> HashMap<String, TaskState> {
        HashMap::from([("t-dev".to_string(), dev), ("t-pm".to_string(), TaskState::Accepted)])
    }

    fn cmd(expect: &str) -> DodCheck {
        DodCheck::Cmd { run: "true".to_string(), expect: expect.to_string() }
    }

    fn browser(expect: &str) -> DodCheck {
        DodCheck::Browser { flow: "http://localhost:3000".to_string(), expect: expect.to_string() }
    }

    #[test]
    fn completed_gate_met_cases() {
        let accepted = states(TaskState::Accepted);
        assert!(completed_gate_met(&gate_dag(vec![cmd("exit 0")]), &accepted, false), "an executed cmd check counts");
        assert!(!completed_gate_met(&gate_dag(vec![cmd("exit zero")]), &accepted, false), "an unparseable cmd check never ran");
        assert!(completed_gate_met(&gate_dag(vec![browser("text \"x\"")]), &accepted, true), "a wired browser check counts");
        assert!(!completed_gate_met(&gate_dag(vec![browser("text \"x\"")]), &accepted, false), "an unwired browser check was skipped");
        assert!(!completed_gate_met(&gate_dag(vec![browser("nonsense")]), &accepted, true), "an unparseable browser check never ran");
        assert!(!completed_gate_met(&gate_dag(vec![cmd("exit 0")]), &states(TaskState::Blocked), false), "a blocked task does not count");
        let req_only = vec![DodCheck::ReqCover { ids: vec![ReqId::new("REQ-1").unwrap()] }];
        assert!(!completed_gate_met(&gate_dag(req_only), &accepted, true), "req coverage is not an executed check");
        assert!(!completed_gate_met(&TaskDag { tasks: Vec::new() }, &HashMap::new(), true), "an empty dag has nothing");
    }

    #[test]
    fn secret_like_cases() {
        assert_eq!(secret_like("-----BEGIN OPENSSH PRIVATE KEY-----"), Some("private key"));
        assert!(secret_like(&format!("key = AKIA{}", "ABCDEFGHIJ012345")).is_some(), "AKIA + 16");
        assert_eq!(secret_like(&format!("key = AKIA{}", "ABCDEFGHIJ01234")), None, "AKIA + 15 is not a key id");
        assert!(secret_like(&format!("token ghp_{}", "a".repeat(36))).is_some());
        assert!(secret_like(&format!("github_pat_{}", "A_1".repeat(8))).is_some());
        assert_eq!(secret_like("an ordinary line of text"), None);
        assert_eq!(secret_like(""), None);
    }

    /// A repo on main with a crew/developer branch and a bare origin that has
    /// main already pushed (so origin/main is tracked).
    fn push_fixture(label: &str) -> (TestDir, PathBuf, PathBuf, GitFlowCtx) {
        let (dir, repo, _wt, c) = dev_fixture(label);
        let origin = dir.path().join("origin.git");
        git(dir.path(), &["init", "-q", "--bare", "-b", "main", &origin.to_string_lossy()]);
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]);
        git(&repo, &["push", "-q", "-u", "origin", "main"]);
        (dir, repo, origin, c)
    }

    fn commit_on_main(repo: &Path, file: &str, content: &[u8]) {
        write(&repo.join(file), content);
        git(repo, &["add", file]);
        git(repo, &["commit", "-q", "-m", &format!("add {file}")]);
    }

    fn one_event(rx: &mut broadcast::Receiver<RunEvent>) -> Ev {
        let events = drain(rx);
        assert_eq!(events.len(), 1, "exactly one push-phase event: {events:?}");
        events.into_iter().next().unwrap()
    }

    const FAKE_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nFAKEFAKEFAKE\n-----END OPENSSH PRIVATE KEY-----\n";

    /// Normal: main fast-forwards origin/main; role branches stay local.
    #[test]
    fn push_fast_forwards_origin_main() {
        let (_dir, repo, origin, c) = push_fixture("push");
        commit_on_main(&repo, "hello.txt", b"hello\n");
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        let ev = one_event(&mut rx);
        assert_eq!(ev.0, GitFlowKindDto::Push, "{ev:?}");
        let main = git(&repo, &["rev-parse", "main"]);
        assert_eq!(ev.3.as_deref(), Some(main.as_str()));
        assert_eq!(ev.2.as_deref(), Some("main"));
        assert_eq!(git(&origin, &["rev-parse", "main"]), main);
        assert_eq!(git(&origin, &["for-each-ref", "refs/heads/crew"]), "", "no role branch is pushed");
    }

    /// Error: a diverged origin is never overwritten.
    #[test]
    fn push_rejects_a_non_fast_forward() {
        let (dir, repo, origin, c) = push_fixture("push-nff");
        let other = dir.path().join("other");
        git(dir.path(), &["clone", "-q", &origin.to_string_lossy(), &other.to_string_lossy()]);
        git(&other, &["-c", "user.name=O", "-c", "user.email=o@example.com", "commit", "-q", "--allow-empty", "-m", "remote work"]);
        git(&other, &["push", "-q", "origin", "main"]);
        let remote_tip = git(&origin, &["rev-parse", "main"]);
        commit_on_main(&repo, "hello.txt", b"hello\n");
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        let ev = one_event(&mut rx);
        assert_eq!(ev.0, GitFlowKindDto::Error, "{ev:?}");
        assert!(ev.5.len() <= 600, "stderr is trimmed");
        assert_eq!(git(&origin, &["rev-parse", "main"]), remote_tip);
    }

    /// Boundary: no origin remote -> skip.
    #[test]
    fn push_skips_without_origin() {
        let (_dir, _repo, _wt, c) = dev_fixture("push-no-origin");
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        let ev = one_event(&mut rx);
        assert_eq!(ev.0, GitFlowKindDto::Skip);
        assert!(ev.5.contains("no origin"), "{}", ev.5);
    }

    /// Boundary: a run that did not complete never pushes.
    #[test]
    fn push_skips_when_not_completed() {
        let (_dir, repo, origin, c) = push_fixture("push-failed-run");
        commit_on_main(&repo, "hello.txt", b"hello\n");
        let before = git(&origin, &["rev-parse", "main"]);
        let calls = c.git_calls();
        let (tx, mut rx) = channel();

        c.push_main(false, true, &tx);

        let ev = one_event(&mut rx);
        assert_eq!(ev.0, GitFlowKindDto::Skip);
        assert!(ev.5.contains("did not complete"), "{}", ev.5);
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
        assert_eq!(c.git_calls(), calls, "a failed run issues no push-phase command");
    }

    /// Boundary: nothing merged this run -> skip.
    #[test]
    fn push_skips_when_nothing_merged() {
        let (_dir, repo, origin, c) = push_fixture("push-nothing");
        commit_on_main(&repo, "hello.txt", b"hello\n");
        let before = git(&origin, &["rev-parse", "main"]);
        let (tx, mut rx) = channel();

        c.push_main(true, false, &tx);

        let ev = one_event(&mut rx);
        assert_eq!(ev.0, GitFlowKindDto::Skip);
        assert!(ev.5.contains("nothing merged"), "{}", ev.5);
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    /// Boundary: an lfs repo is never pushed by the flow.
    #[test]
    fn push_skips_an_lfs_repo() {
        let (_dir, repo, origin, c) = push_fixture("push-lfs");
        commit_on_main(&repo, ".gitattributes", b"*.bin filter=lfs diff=lfs merge=lfs -text\n");
        let before = git(&origin, &["rev-parse", "main"]);
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        let ev = one_event(&mut rx);
        assert_eq!(ev.0, GitFlowKindDto::Skip);
        assert!(ev.5.contains("git-lfs"), "{}", ev.5);
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    /// Normal: a repo without .gitattributes is simply "no lfs".
    #[test]
    fn push_without_gitattributes_is_not_an_error() {
        let (_dir, repo, _origin, c) = push_fixture("push-no-attrs");
        commit_on_main(&repo, "hello.txt", b"hello\n");
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_eq!(one_event(&mut rx).0, GitFlowKindDto::Push);
    }

    fn assert_withheld(ev: &Ev, path: &str) {
        assert_eq!(ev.0, GitFlowKindDto::Error, "{ev:?}");
        assert!(ev.5.contains("push withheld: secret-like content"), "{}", ev.5);
        assert!(ev.5.contains(&format!("added in {path}")), "{}", ev.5);
        assert!(!ev.5.contains("BEGIN") && !ev.5.contains("FAKE"), "the matched text is never echoed: {}", ev.5);
    }

    /// Error (D9): a key header added on main withholds the push.
    #[test]
    fn push_is_withheld_when_main_adds_a_private_key_header() {
        let (_dir, repo, origin, c) = push_fixture("push-secret");
        commit_on_main(&repo, "key.pem", FAKE_KEY.as_bytes());
        let before = git(&origin, &["rev-parse", "main"]);
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "key.pem");
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    /// Error (D9): a held-back secret keeps blocking later runs.
    #[test]
    fn a_held_back_secret_blocks_the_next_run_too() {
        let (_dir, repo, origin, c) = push_fixture("push-secret-again");
        commit_on_main(&repo, "key.pem", FAKE_KEY.as_bytes());
        let (tx, mut rx) = channel();
        c.push_main(true, true, &tx);
        assert_withheld(&one_event(&mut rx), "key.pem");
        commit_on_main(&repo, "later.txt", b"clean work\n");
        let before = git(&origin, &["rev-parse", "main"]);

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "key.pem");
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    /// Error (D9): a secret added then removed inside the pushed range is
    /// still in the published history.
    #[test]
    fn a_secret_added_then_removed_in_the_pushed_range_is_withheld() {
        let (_dir, repo, origin, c) = push_fixture("push-secret-removed");
        commit_on_main(&repo, "key.pem", FAKE_KEY.as_bytes());
        git(&repo, &["rm", "-q", "key.pem"]);
        git(&repo, &["commit", "-q", "-m", "remove key"]);
        let before = git(&origin, &["rev-parse", "main"]);
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "key.pem");
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    /// Error (D9): `-diff` in .gitattributes cannot hide an added key.
    #[test]
    fn a_minus_diff_attribute_does_not_hide_the_secret() {
        let (_dir, repo, _origin, c) = push_fixture("push-minus-diff");
        commit_on_main(&repo, ".gitattributes", b"*.pem -diff\n");
        commit_on_main(&repo, "key.pem", FAKE_KEY.as_bytes());
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "key.pem");
    }

    /// Boundary (D9): an added line whose content starts with "++" is still
    /// an added line.
    #[test]
    fn an_added_line_starting_with_plus_plus_is_scanned() {
        let (_dir, repo, _origin, c) = push_fixture("push-plusplus");
        commit_on_main(&repo, "notes.txt", b"++-----BEGIN RSA PRIVATE KEY-----\n");
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "notes.txt");
    }

    /// Boundary (D9): diff.noprefix in the user's config does not break the
    /// file path the finding names.
    #[test]
    fn diff_noprefix_config_does_not_break_path_tracking() {
        let (_dir, repo, _origin, c) = push_fixture("push-noprefix");
        git(&repo, &["config", "diff.noprefix", "true"]);
        commit_on_main(&repo, "dir/key.txt", FAKE_KEY.as_bytes());
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "dir/key.txt");
    }

    /// Error (D9): with an unrelated origin history there is no merge base, so
    /// all of main is scanned — including a secret from before this run.
    #[test]
    fn unrelated_origin_history_scans_all_of_main() {
        let dir = TestDir::new("push-unrelated");
        let repo = dir.path().join("repo");
        init_repo(&repo, "main");
        commit_on_main(&repo, "key.pem", FAKE_KEY.as_bytes());
        commit_on_main(&repo, "hello.txt", b"hello\n");
        let elsewhere = dir.path().join("elsewhere");
        init_repo(&elsewhere, "main");
        let origin = dir.path().join("origin.git");
        git(dir.path(), &["init", "-q", "--bare", "-b", "main", &origin.to_string_lossy()]);
        git(&elsewhere, &["push", "-q", &origin.to_string_lossy(), "main"]);
        git(&repo, &["remote", "add", "origin", &origin.to_string_lossy()]);
        git(&repo, &["fetch", "-q", "origin"]);
        let before = git(&origin, &["rev-parse", "main"]);
        let c = ctx(&repo, &[]);
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "key.pem");
        assert_eq!(git(&origin, &["rev-parse", "main"]), before);
    }

    /// Boundary (D9): non-UTF-8 bytes before a key line do not abort the scan.
    #[test]
    fn non_utf8_content_does_not_abort_the_scan() {
        let (_dir, repo, _origin, c) = push_fixture("push-latin1");
        let mut content = b"caf\xe9 au lait\n".to_vec();
        content.extend_from_slice(b"-----BEGIN EC PRIVATE KEY-----\n");
        commit_on_main(&repo, "notes.txt", &content);
        let (tx, mut rx) = channel();

        c.push_main(true, true, &tx);

        assert_withheld(&one_event(&mut rx), "notes.txt");
    }

    /// The parser alone: header lines are not content, a deletion adds
    /// nothing, and a later hunk's added line is found.
    #[test]
    fn scan_patch_tracks_files_and_hunks() {
        let patch = b"diff --git a/x b/x\n--- a/x\n+++ /dev/null\n@@ -1 +0,0 @@\n------BEGIN RSA PRIVATE KEY-----\n\
diff --git a/y.txt b/y.txt\n--- /dev/null\n+++ b/y.txt\n@@ -0,0 +1 @@\n+fine\n@@ -5,0 +7 @@\n+AKIAABCDEFGHIJ012345\n";
        assert_eq!(scan_patch(patch), Some(("aws access key id", "y.txt".to_string())));
        assert_eq!(scan_patch(b""), None);
    }
}
