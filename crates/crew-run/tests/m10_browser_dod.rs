//! Issue #3 wiring proof: `RunConfig.browser_binary` +
//! `RunConfig.dev_browser_checks` reach the Lead that the real
//! `RunController::start` assembles, a real subprocess is really spawned
//! with the contracted argv, and a non-zero exit really makes the DoD
//! verdict fail (user decision D4).
//!
//! Why this file spawns for real while its Cmd sibling deliberately does
//! not: `m10_cmd_dod.rs:5-27` keeps subprocess count at zero because a
//! `cmd` check's `run` is a shell-ish command line that would find this
//! crate's own `Cargo.toml` and recurse into a nested `cargo test`. A
//! `browser` check has no such hazard — its argv is a URL plus an assertion
//! triple, and the binary is a committed fixture (`tests/fixtures/
//! fake-browser-cli.sh`) copied per test. No network, no real browser, no
//! screen capture.
//!
//! `$HOME` NOTE (coordinator rulings C4/C6, plan D15): cases 1, 2 and 4 pass
//! `project_root: Some(..)`, and `RunController::start` creates five real
//! out-of-repo `git worktree` trees under `~/.linkly-crew/projects/` for any
//! such run — keyed on `project_root` ALONE (`controller.rs`'s
//! `role_worktrees` match), never on whether browser wiring is on. Each of
//! those three cases therefore runs `cleanup_home_entry`, which deletes the
//! ONE exact path that run created and nothing else. Cases 3 and 5 pass
//! `project_root: None` and create no such entry, so they run no cleanup.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::{DodCheck, MessageKind, Role};
use crew_run::{
    BrowserCheck, GateDecision, RunConfig, RunController, RunEvent, RunMode, RunOutcomeDto,
    TaskStateDto,
};

const GOAL: &str = "간단한 랜딩 페이지";
const FLOW: &str = "http://localhost:3000/signup";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const JOIN_TIMEOUT: Duration = Duration::from_secs(60);
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

/// The sentinel payloads the fixture keys its exit code off. They travel
/// through the `expect` grammar's quoted `<v>` slot, so they are also what
/// the argv assertion reads back out of `argv.log`.
const PASS_SENTINEL: &str = "crew-browser-dod-pass";
const FAIL_SENTINEL: &str = "crew-browser-dod-fail";

// --- scratch layout (plan "Test scratch layout") -------------------------

/// The three sibling scratch trees one case owns. `repo`'s basename is
/// EXACTLY `<label>-<uuid>`, because `cleanup_home_entry` matches the
/// `$HOME` entry production derives from that basename.
struct Scratch {
    repo: PathBuf,
    data_dir: PathBuf,
    bin_dir: PathBuf,
    /// `<label>-<uuid>` — the `$HOME` entry's prefix.
    repo_name: String,
}

impl Scratch {
    fn new(label: &str) -> Self {
        let id = uuid::Uuid::new_v4();
        let base = std::env::current_dir().expect("cwd must resolve").join(".crew-test");
        let repo_name = format!("{label}-{id}");
        Self {
            repo: base.join(&repo_name),
            data_dir: base.join(format!("{label}-data-{id}")),
            bin_dir: base.join(format!("{label}-bin-{id}")),
            repo_name,
        }
    }

    /// Removes all three trees. Best-effort by design: a failure here must
    /// not mask the assertion failure that skipped an earlier cleanup.
    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.repo);
        let _ = std::fs::remove_dir_all(&self.data_dir);
        let _ = std::fs::remove_dir_all(&self.bin_dir);
    }
}

fn run_git_ok(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .expect("test setup: git must be on PATH");
    assert!(status.success(), "test setup: `git {args:?}` failed in {cwd:?}");
}

/// A real, minimal git repo — `RunController::start`'s role-worktree setup
/// needs a resolvable `HEAD` to branch each role's worktree from.
fn init_test_repo(root: &Path) {
    std::fs::create_dir_all(root).expect("test setup: repo root");
    run_git_ok(root, &["init", "-q"]);
    run_git_ok(root, &["config", "user.email", "test@example.com"]);
    run_git_ok(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("README.md"), b"init").expect("test setup: seed file");
    run_git_ok(root, &["add", "."]);
    run_git_ok(root, &["commit", "-q", "-m", "init"]);
}

/// Copies the committed fixture into this case's own bin dir and returns the
/// copy's absolute path. `std::fs::copy` is used specifically because it
/// preserves the Unix mode bits; the execute bit is then asserted, because a
/// non-executable copy would surface later as an unexplained
/// `BinaryUnavailable` that still lets the run finish `Completed`.
fn install_fixture(bin_dir: &Path) -> PathBuf {
    std::fs::create_dir_all(bin_dir).expect("test setup: bin dir");
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-browser-cli.sh");
    assert!(src.is_file(), "committed fixture must exist at {src:?}");
    let dst = bin_dir.join("fake-browser-cli.sh");
    std::fs::copy(&src, &dst).expect("test setup: copy the fixture");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dst).expect("test setup: stat the copy").permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "the fixture copy must keep an execute bit, got mode {mode:o} at {dst:?}"
        );
    }
    dst
}

fn argv_log(bin_dir: &Path) -> PathBuf {
    bin_dir.join("argv.log")
}

/// Waits (bounded) for the fixture to record an argv line, independent of
/// what the run then decides. The fixture logs BEFORE it chooses an exit
/// code, so this observes the argv contract even when the resulting check
/// fails — which is what lets the ordered-triple assertion be the FIRST
/// thing to fire when the contract breaks, instead of a downstream timeout
/// waiting for a task state that a broken argv will never reach.
async fn wait_for_argv_log(bin_dir: &Path) -> String {
    let log = argv_log(bin_dir);
    let deadline = std::time::Instant::now() + EVENT_TIMEOUT;
    loop {
        if let Ok(contents) = std::fs::read_to_string(&log) {
            if !contents.trim().is_empty() {
                return contents;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the fixture never recorded an argv line at {log:?} within the deterministic budget \
             — it was never spawned"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

// --- config ---------------------------------------------------------------

fn config(
    s: &Scratch,
    project_root: Option<PathBuf>,
    browser_binary: Option<String>,
    dev_browser_checks: Vec<BrowserCheck>,
    max_rework: u32,
) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations: vec![] },
        data_dir: s.data_dir.clone(),
        max_rework,
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

fn browser_check(sentinel: &str) -> BrowserCheck {
    BrowserCheck {
        flow: FLOW.to_string(),
        expect: format!("text \"{sentinel}\""),
    }
}

// --- run helpers (mirroring m10_cmd_dod.rs / m7_gate.rs) ------------------

async fn start_ok(cfg: RunConfig) -> crew_run::RunHandle {
    tokio::time::timeout(START_TIMEOUT, RunController::start(cfg))
        .await
        .expect("start must finish within the deterministic budget")
        .expect("start must succeed for a well-formed config")
}

async fn join_ok(handle: &mut crew_run::RunHandle) {
    tokio::time::timeout(JOIN_TIMEOUT, handle.join())
        .await
        .expect("join must finish within the deterministic budget")
        .expect("lead runner must complete cleanly");
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<RunEvent>) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

fn spec_ready_dag(events: &[RunEvent]) -> crew_proto::TaskDag {
    events
        .iter()
        .find_map(|ev| match ev {
            RunEvent::SpecReady { dag, .. } => Some(dag.clone()),
            _ => None,
        })
        .expect("spec_ready must have been observed")
}

fn developer_browser_checks(dag: &crew_proto::TaskDag) -> Vec<(String, String)> {
    dag.tasks
        .iter()
        .find(|t| t.role == Role::Developer)
        .expect("developer task must be present in the default 5-role roster")
        .dod
        .iter()
        .filter_map(|c| match c {
            DodCheck::Browser { flow, expect } => Some((flow.clone(), expect.clone())),
            _ => None,
        })
        .collect()
}

async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<RunEvent>,
    buf: &mut Vec<RunEvent>,
    pred: impl Fn(&RunEvent) -> bool,
) {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let ev = rx.recv().await.expect("broadcast receiver must not lag/close before the match");
            let matched = pred(&ev);
            buf.push(ev);
            if matched {
                return;
            }
        }
    })
    .await
    .expect("expected event did not arrive within the deterministic budget")
}

fn is_task_state(ev: &RunEvent, task_id: &str, state: TaskStateDto) -> bool {
    matches!(ev, RunEvent::TaskStateChanged { task_id: t, state: s, .. } if t == task_id && *s == state)
}

/// Every `violations` string carried by any `change.request` envelope seen
/// so far. The fail case asserts here and NEVER on `human.gate`:
/// `dispatch.rs`'s `violation_strings` omits `failed_cmds` entirely
/// (pre-existing, out of scope), so a `human.gate` assertion could never
/// fire and the test would be green for the wrong reason.
fn change_request_violations(events: &[RunEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Message { envelope, .. } if envelope.kind == MessageKind::ChangeRequest => {
                Some(envelope.body["violations"].as_array().cloned().unwrap_or_default())
            }
            _ => None,
        })
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

// --- the $HOME preflight (plan D15, coordinator rulings C4/C6) ------------

fn projects_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME must be set on this platform"))
        .join(".linkly-crew")
        .join("projects")
}

/// Read-only snapshot of `~/.linkly-crew/projects`'s entry names. A missing
/// directory is an empty set, never an error — this never creates it.
fn snapshot_projects() -> HashSet<OsString> {
    match std::fs::read_dir(projects_dir()) {
        Ok(entries) => entries.filter_map(Result::ok).map(|e| e.file_name()).collect(),
        Err(_) => HashSet::new(),
    }
}

/// Deletes the ONE `~/.linkly-crew/projects` entry this case's run created.
///
/// The target is selected by TWO independent conditions — absent from the
/// `before` snapshot AND prefixed with this case's own `<label>-<uuid>-` —
/// and exactly one match is required. Zero matches is a test FAILURE, not a
/// no-op: it means production stopped deriving the entry name from the
/// `project_root` basename, which is precisely the silent-no-op that ruling
/// C4 exists to prevent. The set difference keeps the pre-existing entries
/// structurally unreachable; the uuid prefix keeps a sibling test binary's
/// concurrent leak from being misidentified during `cargo test --workspace`.
///
/// Recovery, if a panic before the delete strands an entry: the printed
/// absolute path is a regenerable per-project role-worktree cache; remove
/// that one path by hand. No `Drop` guard deletes under `$HOME` while
/// unwinding.
fn find_created_home_entry(before: &HashSet<OsString>, repo_name: &str) -> PathBuf {
    let projects = projects_dir();
    let after = snapshot_projects();
    let prefix = format!("{repo_name}-");

    let mut created: Vec<OsString> = after
        .difference(before)
        .filter(|name| name.to_string_lossy().starts_with(&prefix))
        .cloned()
        .collect();
    created.sort();

    assert_eq!(
        created.len(),
        1,
        "expected EXACTLY ONE new {} entry prefixed {prefix:?}, found {created:?}. \
         Zero means production no longer derives the worktree base from the \
         project_root basename, so this cleanup would silently no-op while \
         still leaking (coordinator ruling C4).",
        projects.display()
    );

    let victim = projects.join(&created[0]);
    assert_eq!(
        victim.parent(),
        Some(projects.as_path()),
        "refusing to delete {victim:?}: it is not exactly one level under {}",
        projects.display()
    );
    let meta = victim
        .symlink_metadata()
        .unwrap_or_else(|e| panic!("refusing to delete {victim:?}: cannot stat it: {e}"));
    assert!(
        !meta.file_type().is_symlink(),
        "refusing to delete {victim:?}: it is a symlink, not a directory"
    );
    assert!(meta.is_dir(), "refusing to delete {victim:?}: it is not a directory");
    victim
}

/// Deletes the one path [`find_created_home_entry`] identified and verified.
fn cleanup_home_entry(victim: &Path) {
    std::fs::remove_dir_all(victim)
        .unwrap_or_else(|e| panic!("failed to remove {victim:?}: {e}; remove it by hand"));
    assert!(!victim.exists(), "{victim:?} still exists after removal");
}

/// The absolute path the spawned fixture reported as its working directory.
fn recorded_spawn_cwd(bin_dir: &Path) -> PathBuf {
    let log = bin_dir.join("cwd.log");
    let contents = std::fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("the fixture must have recorded its cwd at {log:?}: {e}"));
    let first = contents.lines().next().expect("cwd.log must have at least one line");
    PathBuf::from(first)
}

// --- case 1: normal — a passing browser check really spawns --------------

/// Normal (R6/R9): a configured fake CLI that exits 0 is really spawned
/// through the DAG the real `RunController::start` assembled, with the
/// contracted argv, and the run completes with the check not counted
/// against `passed`.
#[tokio::test(flavor = "multi_thread")]
async fn a_passing_browser_check_spawns_with_the_contracted_argv_and_completes() {
    let s = Scratch::new("browser-dod-pass");
    init_test_repo(&s.repo);
    let binary = install_fixture(&s.bin_dir);
    let home_before = snapshot_projects();

    let cfg = config(
        &s,
        Some(s.repo.clone()),
        Some(binary.to_string_lossy().into_owned()),
        vec![browser_check(PASS_SENTINEL)],
        AcceptanceLoop::default_budget(),
    );
    let mut handle = start_ok(cfg).await;
    let mut rx = handle.subscribe();
    let mut events = Vec::new();

    // (a) a real subprocess ran, with argv = [flow, kind, value] IN ORDER.
    // Asserted FIRST, from the fixture's own log, so that a broken argv
    // reports as a one-line contract diff rather than as some downstream
    // consequence — a task state that a broken argv can never reach, or a
    // join that never finishes.
    let recorded = wait_for_argv_log(&s.bin_dir).await;
    let first = recorded.lines().next().expect("argv.log must have at least one line");
    let fields: Vec<&str> = first.split('\t').collect();
    assert_eq!(
        fields,
        vec![FLOW, "text", PASS_SENTINEL],
        "argv must be flow, then kind, then value — in that order (t2 contract §8); got {first:?}"
    );

    // (b) an exit-0 check does not fail the run, and (c) the check reached
    // the DAG the real planner built.
    join_ok(&mut handle).await;
    events.extend(drain(&mut rx));
    let checks = developer_browser_checks(&spec_ready_dag(&events));
    assert_eq!(
        checks,
        vec![(FLOW.to_string(), format!("text \"{PASS_SENTINEL}\""))],
        "the configured browser check must reach the Developer task's dod"
    );
    let finished = events.iter().find_map(|ev| match ev {
        RunEvent::RunFinished { outcome, .. } => Some(*outcome),
        _ => None,
    });
    assert_eq!(finished, Some(RunOutcomeDto::Completed));
    assert!(
        change_request_violations(&events)
            .iter()
            .all(|v| !v.starts_with("browser:")),
        "a passing browser check must raise no browser violation"
    );

    // (d) USER DECISION D3, asserted as a CONSEQUENCE: the browser check must
    // have executed in the Developer's own per-role git worktree — the same
    // directory the Cmd DoD uses — never the shared `<data_dir>/cli-cwd`
    // scratch. This reads the cwd the spawned process actually reported and
    // compares it against the worktree production really created, discovered
    // by set-difference rather than by re-deriving the naming rule here.
    //
    // Without this, wiring the browser closure to the wrong cwd (e.g. passing
    // `None` where the role worktrees belong) is invisible: the fixture is
    // invoked by absolute path, so a wrong cwd changes no exit code and no
    // argv. That is a silent trust-boundary regression.
    let created = find_created_home_entry(&home_before, &s.repo_name);
    let expected_cwd = created.join("worktrees").join("developer");
    let actual_cwd = recorded_spawn_cwd(&s.bin_dir);
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    assert_eq!(
        canon(&actual_cwd),
        canon(&expected_cwd),
        "the browser check must run in the Developer's per-role worktree (user decision D3); \
         it ran in {actual_cwd:?} instead of {expected_cwd:?}"
    );

    handle.shutdown().await;
    cleanup_home_entry(&created);
    s.cleanup();
}

// --- case 2: error — a failing browser check makes passed=false ----------

/// Error (R7, user decision D4 — the load-bearing case): the fixture exits
/// 3, so `judge` buckets it into `failed_cmds`, the Lead withholds
/// acceptance and raises a `change.request` naming the flow and its exit
/// code. `max_rework: 1` is deliberate: with a budget of 0 the very first
/// failure exhausts it and the Lead escalates without ever emitting a
/// `change.request`, so the D4 violation string — the thing this case
/// exists to assert — would never appear. With 1, attempt one reworks
/// (observable) and attempt two exhausts the budget, escalating `t-dev`,
/// which a human `Reject` then blocks.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_browser_check_withholds_acceptance_and_escalates() {
    let s = Scratch::new("browser-dod-fail");
    init_test_repo(&s.repo);
    let binary = install_fixture(&s.bin_dir);
    let home_before = snapshot_projects();

    let cfg = config(
        &s,
        Some(s.repo.clone()),
        Some(binary.to_string_lossy().into_owned()),
        vec![browser_check(FAIL_SENTINEL)],
        1,
    );
    let mut handle = start_ok(cfg).await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-dev", TaskStateDto::Escalated)).await;

    // The change.request envelope — NEVER human.gate (violation_strings
    // drops failed_cmds, so a human.gate assertion could never fire).
    let violations = change_request_violations(&seen);
    assert!(
        violations.iter().any(|v| v == &format!("browser:{FLOW} — exit 3")),
        "a non-zero browser exit must appear as a change.request violation; got {violations:?}"
    );

    handle
        .resolve_gate("t-dev", GateDecision::Reject, "browser check failed")
        .await
        .expect("resolve_gate must succeed while the run is live");
    wait_for(&mut rx, &mut seen, |ev| is_task_state(ev, "t-dev", TaskStateDto::Blocked)).await;

    join_ok(&mut handle).await;
    let snap = handle.snapshot();
    let dev_state = snap
        .task_states
        .iter()
        .find(|(id, _)| id == "t-dev")
        .map(|(_, st)| *st)
        .expect("t-dev must be present in the final snapshot");
    assert_eq!(
        dev_state,
        TaskStateDto::Blocked,
        "a failing browser check must never end as Accepted"
    );

    let log = argv_log(&s.bin_dir);
    assert!(log.is_file(), "the fixture must really have been spawned at {log:?}");

    handle.shutdown().await;
    cleanup_home_entry(&find_created_home_entry(&home_before, &s.repo_name));
    s.cleanup();
}

// --- case 3: boundary — project_root None spawns nothing -----------------

/// Boundary (user decision D3): with `project_root: None` the executor is
/// never wired, even though a binary IS configured and checks ARE present.
/// Zero spawns, and the check stays `skipped` without touching `passed`.
/// No `$HOME` entry is created, so no cleanup runs.
#[tokio::test(flavor = "multi_thread")]
async fn no_project_root_never_spawns_even_with_a_binary_configured() {
    let s = Scratch::new("browser-dod-noroot");
    let binary = install_fixture(&s.bin_dir);
    let home_before = snapshot_projects();

    let cfg = config(
        &s,
        None,
        Some(binary.to_string_lossy().into_owned()),
        vec![browser_check(FAIL_SENTINEL)],
        AcceptanceLoop::default_budget(),
    );
    let mut handle = start_ok(cfg).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    assert!(
        !argv_log(&s.bin_dir).exists(),
        "project_root: None must spawn nothing, but the fixture logged an argv"
    );
    // The check still reached the DAG — proving the silence comes from the
    // wiring predicate, not from the check being dropped upstream.
    assert_eq!(developer_browser_checks(&spec_ready_dag(&events)).len(), 1);
    let finished = events.iter().find_map(|ev| match ev {
        RunEvent::RunFinished { outcome, .. } => Some(*outcome),
        _ => None,
    });
    assert_eq!(
        finished,
        Some(RunOutcomeDto::Completed),
        "an unexecuted browser check must leave the run completing normally"
    );

    handle.shutdown().await;
    assert_eq!(
        snapshot_projects().difference(&home_before).count(),
        0,
        "a project_root: None run must create no ~/.linkly-crew/projects entry"
    );
    s.cleanup();
}

// --- case 4: boundary — binary unset spawns nothing ----------------------

/// Boundary: `project_root` IS set but no binary is configured, so nothing
/// is spawned and the check stays `skipped`. This case still creates the
/// `$HOME` role worktrees, because `controller.rs` gates them on
/// `project_root` ALONE and consults no browser field — which is exactly
/// why the cleanup is keyed on `project_root: Some(..)` and not on "this
/// test turned wiring on" (reviewer MAJOR-1).
#[tokio::test(flavor = "multi_thread")]
async fn an_unconfigured_binary_never_spawns_but_still_creates_the_home_entry() {
    let s = Scratch::new("browser-dod-nobin");
    init_test_repo(&s.repo);
    // Installed but never configured — so a spawn would still be possible
    // if the wiring ignored `browser_binary`.
    install_fixture(&s.bin_dir);
    let home_before = snapshot_projects();

    let cfg = config(
        &s,
        Some(s.repo.clone()),
        None,
        vec![browser_check(FAIL_SENTINEL)],
        AcceptanceLoop::default_budget(),
    );
    let mut handle = start_ok(cfg).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    assert!(
        !argv_log(&s.bin_dir).exists(),
        "an unset browser_binary must spawn nothing"
    );
    let finished = events.iter().find_map(|ev| match ev {
        RunEvent::RunFinished { outcome, .. } => Some(*outcome),
        _ => None,
    });
    assert_eq!(finished, Some(RunOutcomeDto::Completed));

    handle.shutdown().await;
    cleanup_home_entry(&find_created_home_entry(&home_before, &s.repo_name));
    s.cleanup();
}

// --- case 5: boundary — the shipped default is unarmed -------------------

/// Boundary (함정 29 / issue #5): a config left at the shipped defaults
/// attaches ZERO browser checks to the Developer task.
#[tokio::test(flavor = "multi_thread")]
async fn the_shipped_default_attaches_no_browser_check_at_all() {
    let s = Scratch::new("browser-dod-default");
    let home_before = snapshot_projects();

    let cfg = config(&s, None, None, Vec::new(), AcceptanceLoop::default_budget());
    let mut handle = start_ok(cfg).await;
    let mut rx = handle.subscribe();

    join_ok(&mut handle).await;
    let events = drain(&mut rx);

    let checks = developer_browser_checks(&spec_ready_dag(&events));
    assert!(
        checks.is_empty(),
        "a default RunConfig must produce no browser check, got {checks:?}"
    );

    handle.shutdown().await;
    assert_eq!(
        snapshot_projects().difference(&home_before).count(),
        0,
        "a default run must create no ~/.linkly-crew/projects entry"
    );
    s.cleanup();
}
