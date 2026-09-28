//! Issue #31c end to end (t7 design D17/D21): a scripted run over a real
//! throwaway repo commits accepted work on `crew/<role>`, merges an accepted
//! sprint into `main`, pushes `main` to a bare `origin` when the run
//! completed, and gates `Completed` on an executed-and-passed DoD check.
//!
//! Every assertion reads git state (log, ls-tree, rev-parse, for-each-ref) or
//! the run's events — never a command string. Everything lives under
//! `crates/crew-run/.crew-test/m13-<label>-<uuid>/` and the role worktrees
//! are injected through `start_with_worktrees_base`, so nothing lands under
//! `~/.linkly-crew`. The scripted crew member cannot write files, so each
//! case plants its files in the role worktrees before the run starts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crew_lead::accept::AcceptanceLoop;
use crew_proto::Role;
use crew_run::{
    BrowserCheck, GitFlowKindDto, RunConfig, RunController, RunError, RunEvent, RunHandle, RunMode, RunOutcomeDto,
};
use tokio::sync::broadcast::error::RecvError;

const COLLECT_TIMEOUT: Duration = Duration::from_secs(60);
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
const ROLES: [(Role, &str); 5] = [
    (Role::Pm, "pm"),
    (Role::Designer, "designer"),
    (Role::Publisher, "publisher"),
    (Role::Developer, "developer"),
    (Role::Qa, "qa"),
];

// --- fixture ------------------------------------------------------------------

/// One case's scratch tree: repo/ (the user's main checkout), base/ (the
/// injected worktrees base), data/, bin/ and optionally origin.git/.
struct Fixture {
    dir: PathBuf,
    repo: PathBuf,
    base: PathBuf,
    data: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".crew-test")
            .join(format!("m13-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("test setup: fixture dir");
        let dir = std::fs::canonicalize(&dir).unwrap();
        let fixture = Fixture {
            repo: dir.join("repo"),
            base: dir.join("base"),
            data: dir.join("data"),
            bin: dir.join("bin"),
            dir,
        };
        std::fs::create_dir_all(&fixture.repo).unwrap();
        git(&fixture.repo, &["init", "-q", "-b", "main"]);
        git(&fixture.repo, &["config", "user.name", "Test"]);
        git(&fixture.repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(fixture.repo.join("README.md"), b"init\n").unwrap();
        git(&fixture.repo, &["add", "."]);
        git(&fixture.repo, &["commit", "-q", "-m", "init"]);
        fixture
    }

    /// Pre-creates all five role worktrees (the run reuses them) so a case
    /// can plant files before the run starts.
    fn worktrees(&self) -> &Self {
        std::fs::create_dir_all(&self.base).unwrap();
        for (_, name) in ROLES {
            let path = self.base.join(name);
            git(&self.repo, &["worktree", "add", "-q", "-b", &format!("crew/{name}"), &path.to_string_lossy()]);
        }
        self
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }

    /// A bare `origin` that already has `main` (so origin/main is tracked).
    fn origin(&self) -> PathBuf {
        let origin = self.dir.join("origin.git");
        git(&self.dir, &["init", "-q", "--bare", "-b", "main", &origin.to_string_lossy()]);
        git(&self.repo, &["remote", "add", "origin", &origin.to_string_lossy()]);
        git(&self.repo, &["push", "-q", "-u", "origin", "main"]);
        origin
    }

    /// With `browser_pass`, the Developer task carries a Browser check the
    /// committed fixture passes — the executed check the Completed gate needs.
    fn config(&self, project_root: Option<PathBuf>, browser_pass: bool) -> RunConfig {
        let (browser_binary, dev_browser_checks) = if browser_pass {
            std::fs::create_dir_all(&self.bin).unwrap();
            let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-browser-cli.sh");
            let dst = self.bin.join("fake-browser-cli.sh");
            std::fs::copy(&src, &dst).expect("test setup: copy the browser fixture");
            (
                Some(dst.to_string_lossy().into_owned()),
                vec![BrowserCheck {
                    flow: "http://localhost:3000/signup".to_string(),
                    expect: "text \"crew-browser-dod-pass\"".to_string(),
                }],
            )
        } else {
            (None, Vec::new())
        };
        RunConfig {
            goal: "간단한 랜딩 페이지".to_string(),
            mode: RunMode::Scripted { planted_violations: vec![] },
            data_dir: self.data.clone(),
            max_rework: AcceptanceLoop::default_budget(),
            max_per_sprint: 0,
            escalation_timeout_ms: 0,
            roster: None,
            dev_cmd_checks: Vec::new(),
            project_root,
            browser_binary,
            dev_browser_checks,
            turn_timeout_secs: 900,
        }
    }
}

impl Drop for Fixture {
    /// Bounded remove-until-gone; never panics.
    fn drop(&mut self) {
        let deadline = Instant::now() + JOIN_TIMEOUT;
        loop {
            match std::fs::remove_dir_all(&self.dir) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if Instant::now() < deadline => std::thread::yield_now(),
                Err(_) => return,
            }
        }
    }
}

fn git_output(cwd: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("git must be on PATH")
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = git_output(cwd, args);
    assert!(
        out.status.success(),
        "`git {args:?}` failed in {cwd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn tree(cwd: &Path, rev: &str) -> Vec<String> {
    git(cwd, &["ls-tree", "-r", "--name-only", rev]).lines().map(str::to_string).collect()
}

fn plant(dir: &Path, file: &str, content: &[u8]) {
    std::fs::write(dir.join(file), content).expect("test setup: plant a file");
}

// --- running ------------------------------------------------------------------

struct Finished {
    joined: Result<(), String>,
    events: Vec<RunEvent>,
    /// Whether `lock` (when given to `finish_sampling`) still existed at the
    /// moment the collector received RunFinished — before any join.
    lock_at_run_finished: Option<bool>,
}

/// Collects every event until RunFinished (the timeout is on the collector,
/// never on join), then joins only if RunFinished arrived.
async fn finish(handle: &mut RunHandle) -> Finished {
    finish_sampling(handle, None).await
}

/// `finish`, also sampling `lock`'s existence inside the collector the
/// instant RunFinished is received.
async fn finish_sampling(handle: &mut RunHandle, lock: Option<PathBuf>) -> Finished {
    let mut rx = handle.subscribe();
    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        let mut lock_at_run_finished = None;
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let last = matches!(ev, RunEvent::RunFinished { .. });
                    if last {
                        lock_at_run_finished = lock.as_ref().map(|l| l.exists());
                    }
                    events.push(ev);
                    if last {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
        (events, lock_at_run_finished)
    });
    match tokio::time::timeout(COLLECT_TIMEOUT, collector).await {
        Ok(Ok((events, lock_at_run_finished))) if events.iter().any(|e| matches!(e, RunEvent::RunFinished { .. })) => {
            Finished { joined: handle.join().await.map_err(|e| e.to_string()), events, lock_at_run_finished }
        }
        Ok(Ok((events, lock_at_run_finished))) => Finished {
            joined: Err("event stream closed before RunFinished".to_string()),
            events,
            lock_at_run_finished,
        },
        Ok(Err(e)) => Finished { joined: Err(format!("collector failed: {e}")), events: Vec::new(), lock_at_run_finished: None },
        Err(_) => Finished { joined: Err("timed out".to_string()), events: Vec::new(), lock_at_run_finished: None },
    }
}

struct Outcome {
    joined: Result<(), String>,
    events: Vec<RunEvent>,
    git_calls: usize,
}

/// start -> finish -> read the git counter -> shutdown_and_wait, on every
/// path; assertions happen only after this returns.
async fn run(cfg: RunConfig, base: &Path) -> Outcome {
    let mut handle = RunController::start_with_worktrees_base(cfg, base.to_path_buf())
        .await
        .unwrap_or_else(|e| panic!("start must succeed: {e}"));
    let Finished { joined, events, .. } = finish(&mut handle).await;
    let git_calls = handle.git_commands_issued();
    handle.shutdown_and_wait().await;
    Outcome { joined, events, git_calls }
}

// --- event views -----------------------------------------------------------------

#[derive(Debug, Clone)]
struct Flow {
    kind: GitFlowKindDto,
    role: Option<String>,
    sha: Option<String>,
    task_id: Option<String>,
    detail: String,
}

fn flows(events: &[RunEvent]) -> Vec<Flow> {
    events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::GitFlow { kind, role, sha, task_id, detail, .. } => Some(Flow {
                kind: *kind,
                role: role.clone(),
                sha: sha.clone(),
                task_id: task_id.clone(),
                detail: detail.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn of_kind(flows: &[Flow], kind: GitFlowKindDto) -> Vec<Flow> {
    flows.iter().filter(|f| f.kind == kind).cloned().collect()
}

/// Commit-phase events carry the task id; merge, push and gate events do not.
fn commit_phase(flows: &[Flow]) -> Vec<Flow> {
    flows.iter().filter(|f| f.task_id.is_some()).cloned().collect()
}

/// RunFinished is the last event, exactly once — so every git_flow event
/// precedes it — and returns its outcome.
fn finished_last(events: &[RunEvent]) -> RunOutcomeDto {
    let finished: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, RunEvent::RunFinished { .. }))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(finished, vec![events.len() - 1], "RunFinished must be the one last event");
    match &events[events.len() - 1] {
        RunEvent::RunFinished { outcome, .. } => *outcome,
        _ => unreachable!(),
    }
}

// --- cases --------------------------------------------------------------------

/// Normal: the planted file is committed on crew/developer, merged into main,
/// and main is pushed to origin.
#[tokio::test(flavor = "multi_thread")]
async fn happy_path_commits_merges_and_pushes() {
    let f = Fixture::new("happy");
    f.worktrees();
    let origin = f.origin();
    plant(&f.wt("developer"), "hello.txt", b"hello\n");

    let out = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert!(out.joined.is_ok(), "{:?}", out.joined);
    assert_eq!(finished_last(&out.events), RunOutcomeDto::Completed);
    let flows = flows(&out.events);
    let commits = commit_phase(&flows);
    assert_eq!(commits.len(), 5, "one commit-phase event per accepted task: {commits:?}");
    let committed = of_kind(&commits, GitFlowKindDto::Commit);
    assert_eq!(committed.len(), 1, "{commits:?}");
    assert_eq!(of_kind(&commits, GitFlowKindDto::Skip).len(), 4, "{commits:?}");
    assert_eq!(committed[0].task_id.as_deref(), Some("t-dev"));
    assert_eq!(committed[0].sha.as_deref(), Some(git(&f.repo, &["rev-parse", "crew/developer"]).as_str()));
    assert!(git(&f.repo, &["log", "-1", "--format=%s", "crew/developer"]).starts_with("crew(developer): t-dev"));
    assert!(tree(&f.repo, "crew/developer").contains(&"hello.txt".to_string()));
    let merges = of_kind(&flows, GitFlowKindDto::Merge);
    assert_eq!(merges.len(), 1, "{flows:?}");
    assert_eq!(merges[0].role.as_deref(), Some("developer"));
    assert!(tree(&f.repo, "main").contains(&"hello.txt".to_string()));
    let pushes = of_kind(&flows, GitFlowKindDto::Push);
    assert_eq!(pushes.len(), 1, "{flows:?}");
    let main = git(&f.repo, &["rev-parse", "main"]);
    assert_eq!(pushes[0].sha.as_deref(), Some(main.as_str()));
    assert_eq!(git(&origin, &["rev-parse", "main"]), main, "origin received main");
    assert!(tree(&origin, "main").contains(&"hello.txt".to_string()));
    assert_eq!(git(&origin, &["for-each-ref", "refs/heads/crew"]), "", "role branches stay local");
    assert!(out.git_calls > 0, "the counter sees the flow's commands");
}

/// Boundary: without an origin remote the push is a skip; the work is on main.
#[tokio::test(flavor = "multi_thread")]
async fn no_origin_is_a_push_skip() {
    let f = Fixture::new("no-origin");
    f.worktrees();
    plant(&f.wt("developer"), "hello.txt", b"hello\n");

    let out = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert_eq!(finished_last(&out.events), RunOutcomeDto::Completed);
    let flows = flows(&out.events);
    assert!(of_kind(&flows, GitFlowKindDto::Push).is_empty());
    let push_phase: Vec<_> = flows.iter().filter(|f| f.detail.starts_with("push skipped")).collect();
    assert_eq!(push_phase.len(), 1, "{flows:?}");
    assert!(push_phase[0].detail.contains("no origin"), "{}", push_phase[0].detail);
    assert!(tree(&f.repo, "main").contains(&"hello.txt".to_string()));
}

/// Boundary: nothing planted -> every accepted task is a skip, nothing merged,
/// nothing pushed.
#[tokio::test(flavor = "multi_thread")]
async fn clean_roles_skip() {
    let f = Fixture::new("clean");
    f.worktrees();
    let origin = f.origin();
    let main_before = git(&f.repo, &["rev-parse", "main"]);

    let out = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert_eq!(finished_last(&out.events), RunOutcomeDto::Completed);
    let flows = flows(&out.events);
    let commits = commit_phase(&flows);
    assert_eq!(commits.len(), 5);
    assert!(commits.iter().all(|f| f.kind == GitFlowKindDto::Skip), "{commits:?}");
    assert!(of_kind(&flows, GitFlowKindDto::Merge).is_empty());
    let skips = of_kind(&flows, GitFlowKindDto::Skip);
    assert!(skips.iter().any(|f| f.detail.contains("nothing merged")), "{flows:?}");
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main_before);
    assert_eq!(git(&origin, &["rev-parse", "main"]), main_before);
}

/// Error: two roles writing the same file conflict; the merge is aborted, the
/// checkout and the role branch are intact, and the run fails.
#[tokio::test(flavor = "multi_thread")]
async fn conflict_between_two_roles_aborts_and_fails() {
    let f = Fixture::new("conflict");
    f.worktrees();
    let origin = f.origin();
    let origin_before = git(&origin, &["rev-parse", "main"]);
    plant(&f.wt("developer"), "shared.txt", b"developer\n");
    plant(&f.wt("qa"), "shared.txt", b"qa\n");

    let out = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert!(out.joined.is_err(), "a failed run's join is an error");
    assert_eq!(finished_last(&out.events), RunOutcomeDto::Failed);
    let flows = flows(&out.events);
    let merges = of_kind(&flows, GitFlowKindDto::Merge);
    assert_eq!(merges.len(), 1, "{flows:?}");
    assert_eq!(merges[0].role.as_deref(), Some("developer"));
    let conflicts = of_kind(&flows, GitFlowKindDto::Conflict);
    assert_eq!(conflicts.len(), 1, "{flows:?}");
    assert_eq!(conflicts[0].role.as_deref(), Some("qa"));
    assert!(conflicts[0].detail.contains("shared.txt"), "{}", conflicts[0].detail);
    let gates = of_kind(&flows, GitFlowKindDto::Gate);
    assert_eq!(gates.len(), 1, "{flows:?}");
    assert!(gates[0].detail.contains("conflict/error"), "{}", gates[0].detail);
    assert_eq!(git_output(&f.repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).status.code(), Some(1));
    assert_eq!(git(&f.repo, &["rev-parse", "HEAD"]), merges[0].sha.clone().unwrap());
    assert_eq!(git(&f.repo, &["status", "--porcelain"]), "");
    let qa_commit = commit_phase(&flows).into_iter().find(|f| f.task_id.as_deref() == Some("t-qa")).unwrap();
    assert_eq!(qa_commit.kind, GitFlowKindDto::Commit);
    assert_eq!(git(&f.repo, &["rev-parse", "crew/qa"]), qa_commit.sha.unwrap(), "crew/qa is intact");
    assert_eq!(std::fs::read(f.repo.join("shared.txt")).unwrap(), b"developer\n");
    assert!(of_kind(&flows, GitFlowKindDto::Push).is_empty());
    assert_eq!(git(&origin, &["rev-parse", "main"]), origin_before, "a failed run is never pushed");
}

/// Boundary: a dirty main checkout is skipped and left untouched; the work
/// did not land, so the run fails.
#[tokio::test(flavor = "multi_thread")]
async fn dirty_main_checkout_is_untouched_and_fails() {
    let f = Fixture::new("dirty-main");
    f.worktrees();
    plant(&f.wt("developer"), "hello.txt", b"hello\n");
    plant(&f.repo, "notes.txt", b"the user's own work\n");
    let main_before = git(&f.repo, &["rev-parse", "main"]);

    let out = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert_eq!(finished_last(&out.events), RunOutcomeDto::Failed);
    let flows = flows(&out.events);
    let sprint_skips: Vec<_> = of_kind(&flows, GitFlowKindDto::Skip)
        .into_iter()
        .filter(|f| f.task_id.is_none() && f.detail.contains("uncommitted"))
        .collect();
    assert_eq!(sprint_skips.len(), 1, "{flows:?}");
    assert_eq!(sprint_skips[0].role, None);
    let gates = of_kind(&flows, GitFlowKindDto::Gate);
    assert_eq!(gates.len(), 1, "{flows:?}");
    assert!(gates[0].detail.contains("did not land on main"), "{}", gates[0].detail);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main_before);
    assert_eq!(std::fs::read(f.repo.join("notes.txt")).unwrap(), b"the user's own work\n");
    assert!(tree(&f.repo, "crew/developer").contains(&"hello.txt".to_string()), "the work stays on its branch");
}

/// Review r1 F1: a role's accepted artifact colliding with an IGNORED file in
/// the project root never overwrites it; the work does not land and the run
/// fails.
#[tokio::test(flavor = "multi_thread")]
async fn an_ignored_user_file_is_never_overwritten_by_a_merge() {
    let f = Fixture::new("ignored");
    f.worktrees();
    std::fs::write(f.repo.join(".gitignore"), b".env\n").unwrap();
    git(&f.repo, &["add", ".gitignore"]);
    git(&f.repo, &["commit", "-q", "-m", "ignore .env"]);
    plant(&f.repo, ".env", b"USER_SECRET=precious\n");
    plant(&f.wt("developer"), ".env", b"FROM_AGENT=1\n");
    let main_before = git(&f.repo, &["rev-parse", "main"]);

    let out = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert_eq!(std::fs::read(f.repo.join(".env")).unwrap(), b"USER_SECRET=precious\n");
    assert_eq!(finished_last(&out.events), RunOutcomeDto::Failed);
    let flows = flows(&out.events);
    let conflicts = of_kind(&flows, GitFlowKindDto::Conflict);
    assert_eq!(conflicts.len(), 1, "{flows:?}");
    assert!(conflicts[0].detail.contains(".env"), "{}", conflicts[0].detail);
    let gates = of_kind(&flows, GitFlowKindDto::Gate);
    assert_eq!(gates.len(), 1, "{flows:?}");
    assert!(gates[0].detail.contains("did not land on main"), "{}", gates[0].detail);
    assert_eq!(git(&f.repo, &["rev-parse", "main"]), main_before);
    assert!(tree(&f.repo, "crew/developer").contains(&".env".to_string()), "the work stays on its branch");
}

/// Error (ruling C3): accepted work without an executed check is not
/// Completed — it merges but is never pushed.
#[tokio::test(flavor = "multi_thread")]
async fn gate_without_executed_checks_fails_and_does_not_push() {
    let f = Fixture::new("gate");
    f.worktrees();
    let origin = f.origin();
    let origin_before = git(&origin, &["rev-parse", "main"]);
    plant(&f.wt("developer"), "hello.txt", b"hello\n");

    let out = run(f.config(Some(f.repo.clone()), false), &f.base).await;

    assert!(out.joined.is_err());
    assert_eq!(finished_last(&out.events), RunOutcomeDto::Failed);
    let flows = flows(&out.events);
    assert_eq!(of_kind(&flows, GitFlowKindDto::Merge).len(), 1, "{flows:?}");
    let gates = of_kind(&flows, GitFlowKindDto::Gate);
    assert_eq!(gates.len(), 1, "{flows:?}");
    assert!(gates[0].detail.contains("no accepted task carried an executed cmd or browser DoD check"), "{}", gates[0].detail);
    assert!(of_kind(&flows, GitFlowKindDto::Push).is_empty());
    assert!(flows.iter().any(|f| f.detail.contains("did not complete")), "{flows:?}");
    assert_eq!(git(&origin, &["rev-parse", "main"]), origin_before, "origin is untouched");
}

/// Error: a git failure on one task is an error event with detail, and the
/// run continues to its end.
#[tokio::test(flavor = "multi_thread")]
async fn commit_error_is_reported_and_the_run_continues() {
    let f = Fixture::new("commit-error");
    f.worktrees();
    let dev = f.wt("developer");
    plant(&dev, "hello.txt", b"hello\n");
    let git_dir = PathBuf::from(git(&dev, &["rev-parse", "--absolute-git-dir"]));
    std::fs::write(git_dir.join("index.lock"), b"").unwrap();

    let out = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert_eq!(finished_last(&out.events), RunOutcomeDto::Failed);
    let flows = flows(&out.events);
    let commits = commit_phase(&flows);
    assert_eq!(commits.len(), 5, "every other task still gets its event: {commits:?}");
    let errors = of_kind(&commits, GitFlowKindDto::Error);
    assert_eq!(errors.len(), 1, "{commits:?}");
    assert_eq!(errors[0].task_id.as_deref(), Some("t-dev"));
    assert!(errors[0].detail.contains("index.lock"), "{}", errors[0].detail);
    let gates = of_kind(&flows, GitFlowKindDto::Gate);
    assert!(gates.len() == 1 && gates[0].detail.contains("git flow reported 1"), "{flows:?}");
}

/// Boundary (D12): without project_root the run issues no git command at all.
#[tokio::test(flavor = "multi_thread")]
async fn project_root_none_issues_no_git_command() {
    let f = Fixture::new("none");

    let out = run(f.config(None, false), &f.base).await;

    assert!(out.joined.is_ok(), "{:?}", out.joined);
    assert_eq!(finished_last(&out.events), RunOutcomeDto::Completed);
    assert_eq!(out.git_calls, 0);
    assert!(flows(&out.events).is_empty());
    assert!(!f.base.exists(), "no worktrees base is created without project_root");
}

/// Error (D21): a root whose lock is held is refused before any role
/// worktree is created.
#[tokio::test(flavor = "multi_thread")]
async fn busy_project_root_is_refused() {
    let f = Fixture::new("busy");
    std::fs::create_dir_all(&f.base).unwrap();
    let lock = f.base.join(".crew-run.lock");
    let held = format!("pid={}\nrun_id=another-run\nstarted=2026-09-28T00:00:00Z\n", std::process::id());
    std::fs::write(&lock, &held).unwrap();

    let result = RunController::start_with_worktrees_base(f.config(Some(f.repo.clone()), true), f.base.clone()).await;

    match result {
        Err(RunError::ProjectRootInvalid(msg)) => assert!(msg.contains("project_root_busy"), "{msg}"),
        Err(other) => panic!("expected project_root_busy, got {other}"),
        Ok(handle) => {
            handle.shutdown_and_wait().await;
            panic!("a busy root must be refused");
        }
    }
    for (_, name) in ROLES {
        assert!(!f.base.join(name).exists(), "no role worktree may be created for a busy root");
    }
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), held, "the holder's lock is untouched");
}

/// Normal (D21): a run that finished on its own frees the root even while
/// its handle is kept (as the app keeps finished handles).
#[tokio::test(flavor = "multi_thread")]
async fn a_finished_run_frees_the_root() {
    let f = Fixture::new("frees");
    f.worktrees();
    let lock = f.base.join(".crew-run.lock");

    let mut first = RunController::start_with_worktrees_base(f.config(Some(f.repo.clone()), true), f.base.clone())
        .await
        .unwrap_or_else(|e| panic!("first start: {e}"));
    let first_done = finish_sampling(&mut first, Some(lock.clone())).await;
    let lock_after_first = lock.exists();
    // Removing the lock is the last write to base/ in this run (the role
    // worktrees were pre-created), so base/'s mtime is the release time —
    // an ordering witness that does not race the collector.
    let released_at = std::fs::metadata(&f.base).and_then(|m| m.modified()).expect("base/ mtime");
    let second = RunController::start_with_worktrees_base(f.config(Some(f.repo.clone()), true), f.base.clone()).await;
    let second_done = match second {
        Ok(mut handle) => {
            let done = finish(&mut handle).await;
            handle.shutdown_and_wait().await;
            Ok(done)
        }
        Err(e) => Err(e.to_string()),
    };
    first.shutdown_and_wait().await;

    assert!(first_done.joined.is_ok(), "{:?}", first_done.joined);
    assert_eq!(
        first_done.lock_at_run_finished,
        Some(false),
        "the lock is already released when RunFinished is received, before any join"
    );
    assert!(!lock_after_first, "and still released after the join");
    let finished_at = first_done
        .events
        .iter()
        .find_map(|e| match e {
            RunEvent::RunFinished { ts, .. } => Some(ts.clone()),
            _ => None,
        })
        .expect("RunFinished");
    let finished_at: std::time::SystemTime = time::OffsetDateTime::parse(&finished_at, &time::format_description::well_known::Rfc3339)
        .expect("RunFinished ts is RFC3339")
        .into();
    assert!(
        released_at <= finished_at,
        "the lock must be released before RunFinished is sent: released {released_at:?}, RunFinished {finished_at:?}"
    );
    let second_done = second_done.expect("a second run on a freed root must start");
    assert!(second_done.joined.is_ok(), "{:?}", second_done.joined);
    assert_eq!(finished_last(&second_done.events), RunOutcomeDto::Completed);
    assert!(!lock.exists());
}

/// D19/D21: `shutdown` on a live run frees the root once any in-flight git
/// phase has ended, so a later run on the same root can start.
#[tokio::test(flavor = "multi_thread")]
async fn a_shut_down_run_frees_the_root() {
    let f = Fixture::new("shutdown");
    f.worktrees();
    let lock = f.base.join(".crew-run.lock");
    let first = RunController::start_with_worktrees_base(f.config(Some(f.repo.clone()), true), f.base.clone())
        .await
        .unwrap_or_else(|e| panic!("first start: {e}"));
    let held_while_live = lock.exists();

    first.shutdown().await;
    let deadline = Instant::now() + JOIN_TIMEOUT;
    while lock.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let freed = !lock.exists();
    let second = run(f.config(Some(f.repo.clone()), true), &f.base).await;

    assert!(held_while_live, "a live run holds the lock");
    assert!(freed, "shutdown must release the lock");
    assert!(second.joined.is_ok(), "{:?}", second.joined);
    assert!(!lock.exists());
}

/// Error (D21 order): a rejected (non-git) root creates no worktrees base.
#[tokio::test(flavor = "multi_thread")]
async fn rejected_root_creates_no_base_dir() {
    let f = Fixture::new("rejected");
    let plain = f.dir.join("plain");
    std::fs::create_dir_all(&plain).unwrap();

    let result = RunController::start_with_worktrees_base(f.config(Some(plain), true), f.base.clone()).await;

    match result {
        Err(RunError::ProjectRootInvalid(msg)) => assert!(msg.contains("not_a_git_repo"), "{msg}"),
        Err(other) => panic!("expected ProjectRootInvalid, got {other}"),
        Ok(handle) => {
            handle.shutdown_and_wait().await;
            panic!("a non-git root must be refused");
        }
    }
    assert!(!f.base.exists(), "a rejected root must create no base dir");
}
