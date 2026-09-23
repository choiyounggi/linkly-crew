//! contract: t2-browser-exec owns the implementation (run m9b, issue #3).
//!
//! `DodCheck::Browser` executor. Symmetric with `cmd_exec.rs`'s `DodCheck::Cmd`
//! executor: `dod_exec::judge` stays a pure function; execution happens here,
//! ahead of `judge`, and its outcomes are injected into `judge` as a 4th
//! argument.
//!
//! No LLM judgement anywhere in this module — `parse_browser_expect` only
//! recognizes 3 structured forms (`text "<v>"` / `visible "<v>"` /
//! `url "<v>"`); anything else is `None`, and the check is skipped, never
//! executed. Safety boundary: no shell, argv exec only (mirrors
//! `cmd_exec.rs`'s boundary), and `flow` (the navigation URL — see below)
//! must resolve to `localhost`/`127.0.0.1` or the check is `Refused` before
//! any spawn.
//!
//! `flow` is the navigation URL. `DodCheck::Browser { flow, expect }`'s Rust
//! shape (`crates/crew-proto/src/dod.rs`) predates this module and is frozen
//! — not edited here — so `flow` is reinterpreted from its pre-this-module
//! symbolic-name meaning (e.g. `"signup"`) to a `http://`/`https://` URL,
//! which is what `validate_localhost_url` below checks.
//!
//! Gap, not silently dropped: no browser CLI binary is reachable on the
//! machine this module was authored on (checked: `command -v gstack browse
//! chrome-headless-shell chromium` — all not found), so every test in this
//! module uses a fixture binary, never a real browser. A real-CLI spot check
//! is a deferred, one-time obligation for whoever wires an actual binary into
//! `BrowserPolicy` for the first time — do that spot check before relying on
//! this path in an unattended run.
//!
//! Argv/exit-code contract handed to that future adapter (not yet verified
//! against any real tool): `argv = [resolved_binary_path, flow, kind, value]`
//! where `kind` is `"text"` / `"visible"` / `"url"` (the recognized
//! `BrowserExpect` variant, lowercased) and `value` is the quoted payload;
//! exit code `0` means the check passed, nonzero means it failed.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crew_proto::{DodCheck, TaskSpec};
#[cfg(unix)]
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::timeout;

/// Which browser CLI binary to run, and for how long. Symmetric with
/// `cmd_exec::CmdPolicy`, but there is no prefix allowlist — one configured
/// binary, not a set of commands.
pub struct BrowserPolicy {
    binary: String,
    timeout: Duration,
}

impl BrowserPolicy {
    /// `binary` is a bare name (PATH-scanned) or an absolute path.
    pub fn new(binary: impl Into<String>) -> Self {
        BrowserPolicy {
            binary: binary.into(),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// The three recognized structured `expect` forms — `parse_browser_expect`'s
/// return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserExpect {
    Text(String),
    Visible(String),
    Url(String),
}

impl BrowserExpect {
    /// Lowercase `kind` token used to build the CLI argv (see the module doc
    /// comment's argv contract).
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            BrowserExpect::Text(_) => "text",
            BrowserExpect::Visible(_) => "visible",
            BrowserExpect::Url(_) => "url",
        }
    }

    /// The quoted payload, unwrapped.
    pub(crate) fn value(&self) -> &str {
        match self {
            BrowserExpect::Text(v) | BrowserExpect::Visible(v) | BrowserExpect::Url(v) => v,
        }
    }
}

/// Outcome of one attempted browser check. The five variants are deliberately
/// distinct: "could not execute" (`BinaryUnavailable`) must never be
/// indistinguishable from "executed and passed" (`Ran { exit_code: 0 }`),
/// which is what issue #3 is about. `dod_exec::judge` owns the mapping from
/// these variants to `failed_cmds`/`skipped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserOutcome {
    /// Ran to completion; carries the observed exit code (0 = check passed).
    Ran {
        flow: String,
        expect: String,
        exit_code: i32,
    },
    /// Refused before any spawn (non-localhost `flow`).
    Refused {
        flow: String,
        expect: String,
        reason: String,
    },
    /// Exceeded the policy timeout; the child and its group were killed.
    TimedOut { flow: String, expect: String },
    /// Spawn itself failed (ENOENT, bad interpreter, signal termination, ...).
    SpawnFailed {
        flow: String,
        expect: String,
        error: String,
    },
    /// The configured binary was not found on PATH, so nothing was spawned.
    /// This is a normal path, not an error: `judge` records it as `skipped`
    /// and it never affects `passed`.
    BinaryUnavailable {
        flow: String,
        expect: String,
        binary: String,
    },
}

/// Parses `expect`. Only `text "<v>"` / `visible "<v>"` / `url "<v>"` are
/// recognized; anything else is `None`, and a `None` here means the check is
/// not executed at all (mirrors `cmd_exec::parse_expect`).
pub fn parse_browser_expect(expect: &str) -> Option<BrowserExpect> {
    let trimmed = expect.trim();
    let (keyword, rest) = trimmed.split_once(' ')?;
    let value = parse_quoted(rest)?;
    match keyword {
        "text" => Some(BrowserExpect::Text(value)),
        "visible" => Some(BrowserExpect::Visible(value)),
        "url" => Some(BrowserExpect::Url(value)),
        _ => None,
    }
}

/// `s` must be exactly `"<inner>"` — starts and ends with `"`, at least 2
/// bytes long, and `inner` contains no further `"` (no escaping supported; an
/// embedded quote makes the whole `expect` unparseable, so the check is
/// skipped).
fn parse_quoted(s: &str) -> Option<String> {
    if s.len() < 2 {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes[0] != b'"' || bytes[bytes.len() - 1] != b'"' {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    if inner.contains('"') {
        return None;
    }
    Some(inner.to_string())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn scan_path_dirs(dirs: &[PathBuf], bin: &str) -> Option<PathBuf> {
    dirs.iter().map(|dir| dir.join(bin)).find(|p| is_executable(p))
}

/// PATH-scans `path_env` for `binary` (bare name or absolute path — if
/// `binary` is absolute, `PathBuf::join` replaces the whole path for every
/// `dir`, so it resolves correctly with no special-case branch). Returns
/// `None` without ever attempting a spawn if nothing matches: binary absence
/// is a normal, non-failing path. `path_env` is injected rather than read
/// from `std::env::var_os` here, so tests can supply a synthetic value
/// without mutating global process state.
pub(crate) fn locate_binary(binary: &str, path_env: &OsStr) -> Option<PathBuf> {
    if binary.is_empty() {
        return None;
    }
    let dirs: Vec<PathBuf> = std::env::split_paths(path_env).collect();
    scan_path_dirs(&dirs, binary)
}

/// `flow` must be `http://` or `https://` followed by a host equal to
/// `localhost` or `127.0.0.1` (case-insensitive), with no userinfo (`@`).
/// Anything else — empty, missing scheme, other host, userinfo — is `Err`
/// before any spawn is attempted. Positive allowlist, never a blocklist: the
/// point is to create no new network exposure.
pub(crate) fn validate_localhost_url(flow: &str) -> Result<(), String> {
    let s = flow.trim();
    if s.is_empty() {
        return Err("empty flow".to_string());
    }
    let rest = s
        .strip_prefix("http://")
        .or_else(|| s.strip_prefix("https://"))
        .ok_or_else(|| format!("flow is not a URL (missing http(s):// scheme): {s:?}"))?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err(format!("flow has no host: {s:?}"));
    }
    if authority.contains('@') {
        return Err(format!("flow authority carries userinfo, refused: {s:?}"));
    }
    let host_end = authority.find(':').unwrap_or(authority.len());
    let host = &authority[..host_end];
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost" || host_lower == "127.0.0.1" {
        Ok(())
    } else {
        Err(format!("non-localhost host refused: {host}"))
    }
}

/// Runs every `DodCheck::Browser` in `task.dod` whose `expect` parses,
/// refusing non-localhost `flow`s and skipping when the configured binary is
/// not found. Argv-only, no shell, timeout + process-group cleanup mirroring
/// `cmd_exec::execute_cmd_checks`.
pub async fn execute_browser_checks(
    task: &TaskSpec,
    cwd: &Path,
    policy: &BrowserPolicy,
) -> Vec<BrowserOutcome> {
    let mut outcomes = Vec::new();

    for check in &task.dod {
        let DodCheck::Browser { flow, expect } = check else {
            continue;
        };
        let Some(parsed) = parse_browser_expect(expect) else {
            continue;
        };

        if let Err(reason) = validate_localhost_url(flow) {
            outcomes.push(BrowserOutcome::Refused {
                flow: flow.clone(),
                expect: expect.clone(),
                reason,
            });
            continue;
        }

        let path_env = std::env::var_os("PATH").unwrap_or_default();
        let Some(binary_path) = locate_binary(&policy.binary, &path_env) else {
            outcomes.push(BrowserOutcome::BinaryUnavailable {
                flow: flow.clone(),
                expect: expect.clone(),
                binary: policy.binary.clone(),
            });
            continue;
        };

        let mut cmd = Command::new(&binary_path);
        cmd.arg(flow);
        cmd.arg(parsed.kind());
        cmd.arg(parsed.value());
        cmd.current_dir(cwd);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        // Making the child the leader of its own new process group is what
        // lets a timeout kill its descendants too, via `killpg`, rather than
        // only the direct child.
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(not(unix))]
        cmd.kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Err(e) => {
                outcomes.push(BrowserOutcome::SpawnFailed {
                    flow: flow.clone(),
                    expect: expect.clone(),
                    error: e.to_string(),
                });
                continue;
            }
            Ok(child) => child,
        };
        #[cfg(unix)]
        let mut group_guard = child.id().and_then(ProcessGroupGuard::new);

        match timeout(policy.timeout, child.wait()).await {
            Ok(Ok(status)) => {
                #[cfg(unix)]
                if let Some(guard) = group_guard.as_mut() {
                    guard.disarm();
                }
                match status.code() {
                    Some(code) => outcomes.push(BrowserOutcome::Ran {
                        flow: flow.clone(),
                        expect: expect.clone(),
                        exit_code: code,
                    }),
                    None => outcomes.push(BrowserOutcome::SpawnFailed {
                        flow: flow.clone(),
                        expect: expect.clone(),
                        error: "terminated by signal".to_string(),
                    }),
                }
            }
            Ok(Err(e)) => {
                // `child.wait()` itself errored: exit status is unknown, so
                // this is not a completion — clean up rather than disarm.
                #[cfg(unix)]
                cleanup_unknown_state(group_guard.as_mut(), &mut child).await;
                outcomes.push(BrowserOutcome::SpawnFailed {
                    flow: flow.clone(),
                    expect: expect.clone(),
                    error: e.to_string(),
                });
            }
            Err(_) => {
                #[cfg(unix)]
                {
                    match group_guard.as_mut() {
                        Some(guard) => {
                            guard.kill_group();
                            guard.disarm();
                            let _ = child.wait().await;
                        }
                        None => {
                            let _ = child.kill().await;
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = child.kill().await;
                }
                outcomes.push(BrowserOutcome::TimedOut {
                    flow: flow.clone(),
                    expect: expect.clone(),
                });
            }
        }
    }

    outcomes
}

/// Owns the group-kill responsibility for a spawned child's entire process
/// group while `armed`. `Drop` runs a synchronous `killpg(pgid, SIGKILL)`, so
/// panic, early-return, and task-cancellation paths all still clean up the
/// child's descendants, not just the explicit timeout branch. A local
/// duplicate of `cmd_exec.rs`'s guard: that one is private to its module and
/// `cmd_exec.rs` is out of scope for this task.
#[cfg(unix)]
struct ProcessGroupGuard {
    pgid: i32,
    armed: bool,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    /// `process_group(0)` at spawn time makes the child its own group leader,
    /// so its pgid equals its pid. A live child's id is never 0, but a guard
    /// must never be built for 0 anyway: `killpg(0, _)` targets the *caller's
    /// own* process group, which would kill this process instead of the
    /// runaway child.
    fn new(child_id: u32) -> Option<Self> {
        if child_id == 0 {
            return None;
        }
        Some(ProcessGroupGuard {
            pgid: child_id as i32,
            armed: true,
        })
    }

    fn kill_group(&self) {
        // SAFETY: killpg is called with a valid signal constant and no
        // borrowed/untrusted memory. The return value is ignored: ESRCH
        // ("no such process group") just means the whole group already
        // exited on its own, which is the success case, not an error.
        unsafe {
            libc::killpg(self.pgid, libc::SIGKILL);
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if self.armed {
            self.kill_group();
        }
    }
}

/// Cleans up after `child.wait()` itself returns an I/O error — the exit
/// status is unknown, so this is not a completion. Kill the group if there is
/// one, or fall back to killing just the direct child.
#[cfg(unix)]
async fn cleanup_unknown_state(guard: Option<&mut ProcessGroupGuard>, child: &mut Child) {
    match guard {
        Some(guard) => {
            guard.kill_group();
            guard.disarm();
        }
        None => {
            let _ = child.kill().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_proto::{ArtifactContract, Role};
    use std::ffi::OsString;

    fn task(dod: Vec<DodCheck>) -> TaskSpec {
        TaskSpec {
            id: "t1".to_string(),
            role: Role::Pm,
            title: "title".to_string(),
            brief: "brief".to_string(),
            dod,
            deps: vec![],
            artifacts_expected: Vec::<ArtifactContract>::new(),
        }
    }

    fn cwd() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn browser_check(flow: &str, expect: &str) -> DodCheck {
        DodCheck::Browser {
            flow: flow.to_string(),
            expect: expect.to_string(),
        }
    }

    /// Kills any grandchild the test recorded and removes its scratch dir,
    /// even if an assertion above panics — a test must never leak a process
    /// or a scratch directory, RED run or not.
    struct TestCleanup {
        dir: PathBuf,
        grandchild_pid: Option<i32>,
    }

    impl Drop for TestCleanup {
        fn drop(&mut self) {
            #[cfg(unix)]
            if let Some(pid) = self.grandchild_pid {
                // Best-effort: if the cleanup under test worked, this is
                // already dead and returns ESRCH, which we ignore.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Polls `path` until it holds non-whitespace content or `budget`
    /// elapses. The spawner script `sync`s before its own long sleep, but
    /// under load the write can still lag a few scheduler ticks behind.
    async fn wait_for_nonempty_file(path: &Path, budget: Duration) -> Option<String> {
        let step = Duration::from_millis(20);
        let mut waited = Duration::ZERO;
        loop {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if !contents.trim().is_empty() {
                    return Some(contents);
                }
            }
            if waited >= budget {
                return None;
            }
            tokio::time::sleep(step).await;
            waited += step;
        }
    }

    /// Polls `libc::kill(pid, 0)` until it reports `ESRCH` or `budget`
    /// elapses. Signal delivery is not synchronous with `kill(2)` returning,
    /// so checking aliveness in the same instant races the kernel.
    #[cfg(unix)]
    async fn wait_until_dead(pid: i32, budget: Duration) -> bool {
        let step = Duration::from_millis(10);
        let mut waited = Duration::ZERO;
        loop {
            let alive = unsafe { libc::kill(pid, 0) };
            if alive == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            if waited >= budget {
                return false;
            }
            tokio::time::sleep(step).await;
            waited += step;
        }
    }

    /// Creates `target/<label>-<pid>` and returns it with its cleanup guard.
    /// Never `/tmp`: scratch state stays inside the crate's own build dir.
    fn scratch(label: &str) -> (PathBuf, TestCleanup) {
        let dir = cwd()
            .join("target")
            .join(format!("{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test scratch dir");
        (
            dir.clone(),
            TestCleanup {
                dir,
                grandchild_pid: None,
            },
        )
    }

    fn write_executable(path: &Path, contents: &str, mode: u32) {
        std::fs::write(path, contents).expect("write fixture binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .expect("stat fixture binary")
                .permissions();
            perms.set_mode(mode);
            std::fs::set_permissions(path, perms).expect("chmod fixture binary");
        }
    }

    // --- parse_browser_expect: normal ---

    #[test]
    fn parse_browser_expect_recognizes_the_three_structured_forms() {
        assert_eq!(
            parse_browser_expect("text \"hi\""),
            Some(BrowserExpect::Text("hi".to_string()))
        );
        assert_eq!(
            parse_browser_expect("visible \"#submit\""),
            Some(BrowserExpect::Visible("#submit".to_string()))
        );
        assert_eq!(
            parse_browser_expect("url \"http://localhost:3000/done\""),
            Some(BrowserExpect::Url("http://localhost:3000/done".to_string()))
        );
    }

    // --- parse_browser_expect: error ---

    #[test]
    fn parse_browser_expect_rejects_unrecognized_forms() {
        // An unrecognized keyword is not a near-miss to be guessed at: it is
        // skipped, never executed.
        assert_eq!(parse_browser_expect("click \"#btn\""), None);
        // The pre-this-module natural-language form. `cmd_exec`'s own
        // `parse_expect_rejects_non_exit_n_forms` pins the same string for
        // the sibling `cmd` grammar — no LLM judges it in either module.
        assert_eq!(parse_browser_expect("성공 토스트 노출"), None);
    }

    // --- parse_browser_expect: boundary ---

    #[test]
    fn parse_browser_expect_handles_empty_and_unterminated_input() {
        assert_eq!(parse_browser_expect(""), None);
        // An empty *value* is still a recognized form.
        assert_eq!(
            parse_browser_expect("text \"\""),
            Some(BrowserExpect::Text(String::new()))
        );
        assert_eq!(parse_browser_expect("text \"unterminated"), None);
        // No escaping is supported, so an embedded quote makes the whole
        // `expect` unparseable rather than silently truncating the value.
        assert_eq!(parse_browser_expect("text \"a\"b\""), None);
    }

    #[test]
    fn browser_expect_exposes_its_kind_token_and_value() {
        // These two feed the CLI argv directly, so an inverted mapping here
        // would run a `visible` assertion for a `text` expectation while
        // still reporting exit 0 — a check that verifies the wrong thing.
        let text = parse_browser_expect("text \"hi\"").expect("parses");
        let visible = parse_browser_expect("visible \"#submit\"").expect("parses");
        let url = parse_browser_expect("url \"http://localhost/x\"").expect("parses");

        assert_eq!((text.kind(), text.value()), ("text", "hi"));
        assert_eq!((visible.kind(), visible.value()), ("visible", "#submit"));
        assert_eq!((url.kind(), url.value()), ("url", "http://localhost/x"));
    }

    // --- locate_binary: normal ---

    #[test]
    fn locate_binary_finds_an_executable_on_the_injected_path() {
        let (dir, _cleanup) = scratch("browser-exec-locate");
        let fixture = dir.join("fake-browser");
        write_executable(&fixture, "#!/bin/sh\nexit 0\n", 0o755);
        let path_env = std::env::join_paths([&dir]).expect("join scratch dir into a PATH value");

        assert_eq!(locate_binary("fake-browser", &path_env), Some(fixture));
    }

    // --- locate_binary: error ---

    #[test]
    fn locate_binary_returns_none_for_a_missing_binary() {
        let (dir, _cleanup) = scratch("browser-exec-locate-missing");
        let path_env = std::env::join_paths([&dir]).expect("join scratch dir into a PATH value");

        assert_eq!(
            locate_binary("definitely-not-a-real-browser-cli-xyz", &path_env),
            None
        );
    }

    // --- locate_binary: boundary ---

    #[test]
    fn locate_binary_returns_none_for_an_empty_binary_name() {
        let path_env = OsString::from("/usr/bin:/bin");

        assert_eq!(locate_binary("", &path_env), None);
    }

    #[cfg(unix)]
    #[test]
    fn locate_binary_ignores_a_file_without_the_exec_bit() {
        let (dir, _cleanup) = scratch("browser-exec-nonexec");
        let fixture = dir.join("not-executable");
        write_executable(&fixture, "#!/bin/sh\nexit 0\n", 0o644);
        let path_env = std::env::join_paths([&dir]).expect("join scratch dir into a PATH value");

        // Without the exec-bit test this would resolve and then fail at
        // spawn, reporting `SpawnFailed` for what is really an unusable
        // binary — blurring two outcomes that must stay distinct.
        assert_eq!(locate_binary("not-executable", &path_env), None);
    }

    // --- validate_localhost_url: normal ---

    #[test]
    fn validate_localhost_url_accepts_localhost_and_loopback() {
        assert!(validate_localhost_url("http://localhost:3000/signup").is_ok());
        assert!(validate_localhost_url("http://127.0.0.1/x").is_ok());
        // Uppercase host, lowercase scheme: the host match is
        // case-insensitive.
        assert!(validate_localhost_url("http://LOCALHOST:8080").is_ok());
    }

    // --- validate_localhost_url: error ---

    #[test]
    fn validate_localhost_url_refuses_non_localhost_and_non_http() {
        assert!(validate_localhost_url("https://example.com").is_err());
        assert!(validate_localhost_url("ftp://localhost").is_err());
    }

    // --- validate_localhost_url: boundary ---

    #[test]
    fn validate_localhost_url_refuses_empty_flow_and_userinfo() {
        assert!(validate_localhost_url("").is_err());
        // `http://user@localhost/x` — userinfo can smuggle a different
        // authority past a naive prefix check, so it is refused outright.
        assert!(validate_localhost_url("http://user@localhost/x").is_err());
    }

    // --- execute_browser_checks: normal ---

    #[tokio::test]
    async fn browser_check_runs_and_reports_its_exit_code() {
        let policy = BrowserPolicy::new("/usr/bin/true");
        let t = task(vec![browser_check("http://localhost:3000", "text \"hi\"")]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            BrowserOutcome::Ran { exit_code, flow, .. } => {
                assert_eq!(*exit_code, 0);
                assert_eq!(flow, "http://localhost:3000");
            }
            other => panic!("expected Ran, got {other:?}"),
        }
    }

    // --- execute_browser_checks: error ---

    #[tokio::test]
    async fn browser_check_reports_nonzero_exit_without_judging() {
        let policy = BrowserPolicy::new("/usr/bin/false");
        let t = task(vec![browser_check("http://localhost:3000", "text \"hi\"")]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        // Still `Ran`: folding a nonzero exit into a *failure* is `judge`'s
        // job, not the executor's.
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            BrowserOutcome::Ran { exit_code, .. } => assert_eq!(*exit_code, 1),
            other => panic!("expected Ran, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_localhost_flow_is_refused_before_any_spawn() {
        // `/usr/bin/true` would exit 0 if it were ever spawned, so an
        // outcome of anything but `Refused` here means the URL guard did not
        // run before the spawn.
        let policy = BrowserPolicy::new("/usr/bin/true");
        let t = task(vec![browser_check("https://example.com", "text \"hi\"")]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            BrowserOutcome::Refused { reason, .. } => assert!(
                reason.contains("example.com"),
                "refusal reason should name the rejected host, got {reason:?}"
            ),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    // --- execute_browser_checks: boundary ---

    #[tokio::test]
    async fn a_task_with_no_browser_checks_produces_no_outcomes() {
        let policy = BrowserPolicy::new("/usr/bin/true");
        let t = task(vec![DodCheck::ReqCover { ids: vec![] }]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        assert!(outcomes.is_empty(), "got {outcomes:?}");
    }

    #[tokio::test]
    async fn an_absent_binary_is_reported_without_spawning() {
        let policy = BrowserPolicy::new("definitely-not-a-real-browser-cli-xyz");
        let t = task(vec![browser_check("http://localhost:3000", "text \"hi\"")]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            BrowserOutcome::BinaryUnavailable { binary, .. } => {
                assert_eq!(binary, "definitely-not-a-real-browser-cli-xyz");
            }
            other => panic!("expected BinaryUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_hanging_binary_times_out() {
        let (dir, _cleanup) = scratch("browser-exec-timeout");
        let script = dir.join("hang.sh");
        write_executable(&script, "#!/bin/sh\nsleep 300\n", 0o700);
        let policy = BrowserPolicy::new(script.to_str().expect("scratch path is utf8"))
            .with_timeout(Duration::from_millis(50));
        let t = task(vec![browser_check("http://localhost:3000", "text \"hi\"")]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            BrowserOutcome::TimedOut { flow, .. } => {
                assert_eq!(flow, "http://localhost:3000");
            }
            other => panic!("expected TimedOut, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unexecutable_file_reports_spawn_failure() {
        let (dir, _cleanup) = scratch("browser-exec-spawnfail");
        let broken = dir.join("broken-browser");
        // Executable bit set, so `locate_binary` resolves it, but the
        // interpreter named by its shebang does not exist, so the kernel
        // refuses the exec — `SpawnFailed`, never a silent pass.
        write_executable(&broken, "#!/nonexistent/interpreter\n", 0o755);
        let policy = BrowserPolicy::new(broken.to_str().expect("scratch path is utf8"));
        let t = task(vec![browser_check("http://localhost:3000", "text \"hi\"")]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            BrowserOutcome::SpawnFailed { error, flow, expect } => {
                // The missing *interpreter* is what the kernel reports, so
                // the error must be a real ENOENT and not an empty string.
                assert!(
                    error.contains("No such file or directory"),
                    "spawn failure should carry the OS reason, got {error:?}"
                );
                assert_eq!(flow, "http://localhost:3000");
                assert_eq!(expect, "text \"hi\"");
            }
            other => panic!("expected SpawnFailed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_spawned_argv_is_flow_then_kind_then_value() {
        let (dir, _cleanup) = scratch("browser-exec-argv");
        let script = dir.join("echo-argv.sh");
        let argv_file = dir.join("argv.txt");
        // The fixture records exactly what it was invoked with. Every other
        // test here ignores its arguments, so without this one the argv
        // assembly — the contract handed to any real browser CLI adapter —
        // is asserted nowhere.
        write_executable(
            &script,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"{}\"\n",
                argv_file.display()
            ),
            0o700,
        );
        let policy = BrowserPolicy::new(script.to_str().expect("scratch path is utf8"));
        let t = task(vec![browser_check(
            "http://localhost:3000/signup",
            "visible \"#submit\"",
        )]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        assert!(
            matches!(&outcomes[0], BrowserOutcome::Ran { exit_code: 0, .. }),
            "got {:?}",
            outcomes[0]
        );
        let recorded = wait_for_nonempty_file(&argv_file, Duration::from_secs(2))
            .await
            .expect("fixture never recorded its argv");
        assert_eq!(
            recorded.trim(),
            "http://localhost:3000/signup visible #submit"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_browser_check_kills_the_whole_process_group() {
        let (dir, mut cleanup) = scratch("browser-exec-pgroup");
        let script = dir.join("spawner.sh");
        let pid_file = dir.join("grandchild.pid");
        // The script backgrounds a grandchild and records its pid, then
        // hangs. Killing only the direct child would leave that grandchild
        // alive — a leaked browser process outliving the run.
        write_executable(
            &script,
            &format!(
                // No `sync` here: `wait_for_nonempty_file` already polls for
                // the write to land, and a filesystem-wide flush stalls every
                // other process-spawning test running concurrently.
                "#!/bin/sh\nsleep 300 &\nGCPID=$!\necho \"$GCPID\" > \"{}\"\nsleep 300\n",
                pid_file.display()
            ),
            0o700,
        );
        // Generous: under concurrent test-thread load, forking the shell and
        // recording the grandchild can take far longer than the act itself.
        let policy = BrowserPolicy::new(script.to_str().expect("scratch path is utf8"))
            .with_timeout(Duration::from_millis(1500));
        let t = task(vec![browser_check("http://localhost:3000", "text \"hi\"")]);

        let outcomes = execute_browser_checks(&t, &cwd(), &policy).await;

        let pid_contents = wait_for_nonempty_file(&pid_file, Duration::from_secs(2))
            .await
            .expect("grandchild pid file was never written by the spawner script");
        let grandchild_pid: i32 = pid_contents
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("grandchild pid file {pid_contents:?} did not parse: {e}"));
        cleanup.grandchild_pid = Some(grandchild_pid);

        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(&outcomes[0], BrowserOutcome::TimedOut { .. }),
            "got {:?}",
            outcomes[0]
        );
        assert!(
            wait_until_dead(grandchild_pid, Duration::from_secs(2)).await,
            "grandchild {grandchild_pid} outlived the timeout — the process \
             group was not killed"
        );
    }
}
