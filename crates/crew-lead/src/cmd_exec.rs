//! contract: t-cmd owns the implementation (run m9a, contracts-m9.md §G2b).
//!
//! `DodCheck::Cmd` executor. `dod_exec::judge` stays a pure function; execution
//! happens in this module, ahead of `judge`, and its outcomes are injected into
//! `judge` as its third argument. The seam exists because `judge`'s only caller
//! — `Lead::handle_task_result` (`dispatch.rs:295`) — is a synchronous pure
//! state machine that must not become async.
//!
//! Safety boundary (contracts-m9.md §G2c, hardened by review r2 F3): no
//! shell, argv exec only, and the command text is vetted positionally — the
//! prefix tokens must equal a configured command literally, and every token
//! after the prefix must be a bare identifier (no flags, no paths) — never a
//! metacharacter blocklist. A prefix-only check (no trailing-token rule) lets
//! a flag like `--manifest-path=<outside the repo>` escape the cwd sandbox
//! while still exiting 0, which is exactly the RCE r2 F3 reproduced.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crew_proto::{DodCheck, TaskSpec};
#[cfg(unix)]
use tokio::process::Child;
use tokio::process::Command;
use tokio::time::timeout;

/// Which `DodCheck::Cmd` commands may run, and for how long.
pub struct CmdPolicy {
    allowed_prefixes: Vec<Vec<String>>,
    timeout: Duration,
}

impl CmdPolicy {
    /// M9 default: verification commands of the web-app / Rust domain only.
    pub fn default_allowlist() -> Self {
        const PREFIXES: &[&[&str]] = &[
            &["cargo", "test"],
            &["cargo", "build"],
            &["cargo", "clippy"],
            &["npm", "test"],
            &["npm", "run"],
            &["pnpm", "test"],
            &["pnpm", "run"],
            &["yarn", "test"],
            &["yarn", "run"],
        ];
        CmdPolicy {
            allowed_prefixes: PREFIXES
                .iter()
                .map(|p| p.iter().map(|s| s.to_string()).collect())
                .collect(),
            timeout: Duration::from_secs(120),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn allow(mut self, prefix: &[&str]) -> Self {
        self.allowed_prefixes
            .push(prefix.iter().map(|s| s.to_string()).collect());
        self
    }

    /// The single production judge for whether a `DodCheck::Cmd` `run` string
    /// is permitted, exposed so callers/tests assert against the real policy
    /// instead of a copy (HANDOFF §4 잔여 5).
    pub fn vet_run(&self, run: &str) -> Result<(), String> {
        vet(run, self).map(|_| ())
    }
}

/// Outcome of one attempted `DodCheck::Cmd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdOutcome {
    /// Ran to completion; carries the observed exit code.
    Ran { run: String, exit_code: i32 },
    /// Refused by policy (character set, prefix allowlist, or empty command).
    Refused { run: String, reason: String },
    /// Exceeded the policy timeout; the child was killed.
    TimedOut { run: String },
    /// Spawn itself failed (ENOENT, signal termination, ...).
    SpawnFailed { run: String, error: String },
}

/// Parses `expect`. Only the `exit <N>` form is recognized; anything else is
/// `None`, and a `None` here means the check is not executed at all.
pub fn parse_expect(expect: &str) -> Option<i32> {
    let trimmed = expect.trim();
    let rest = trimmed.strip_prefix("exit ")?;
    rest.parse::<i32>().ok()
}

/// Validates `run` against the policy's character allowlist and prefix
/// allowlist. Returns the tokenized argv on success, or the refusal reason.
fn vet(run: &str, policy: &CmdPolicy) -> Result<Vec<String>, String> {
    let tokens: Vec<String> = run
        .split_ascii_whitespace()
        .map(|s| s.to_string())
        .collect();
    if tokens.is_empty() {
        return Err("empty command".to_string());
    }

    // Positional allowlist (contracts-m9.md §G2c + coordinator decision, r2
    // F3): the prefix position is a literal-equality match against a known
    // command, so it needs no separate character check. Everything after the
    // prefix is a bare identifier only — no flags, no paths — closing the
    // flag-injection sandbox escape a prefix-only check left open
    // (`--manifest-path=...`, `--prefix=...`, etc).
    let matched_prefix = policy
        .allowed_prefixes
        .iter()
        .find(|prefix| tokens.len() >= prefix.len() && tokens[..prefix.len()] == prefix[..]);

    let Some(prefix) = matched_prefix else {
        return Err(format!("{tokens:?} matches no allowed prefix"));
    };

    for tok in &tokens[prefix.len()..] {
        if !is_bare_trailing_token(tok) {
            return Err(format!(
                "trailing argument {tok:?} is not a bare identifier"
            ));
        }
    }

    Ok(tokens)
}

/// Trailing-position rule (r2 F3): `^[A-Za-z0-9][A-Za-z0-9._-]*$` and no `..`
/// substring. Positive rule, not a blocklist — it happens to exclude every
/// flag (`-`-leading), absolute path (`/`-leading), relative path
/// (`.`-leading), and `=`/`:`/`@`/`+`-bearing token, which is the point.
fn is_bare_trailing_token(tok: &str) -> bool {
    if tok.contains("..") {
        return false;
    }
    let mut chars = tok.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Runs every `DodCheck::Cmd` in `task.dod` that the policy permits and whose
/// `expect` parses. Checks with an unparseable `expect` produce no outcome, so
/// `judge` records them as `skipped`.
pub async fn execute_cmd_checks(
    task: &TaskSpec,
    cwd: &Path,
    policy: &CmdPolicy,
) -> Vec<CmdOutcome> {
    let mut outcomes = Vec::new();

    for check in &task.dod {
        let DodCheck::Cmd { run, expect } = check else {
            continue;
        };
        if parse_expect(expect).is_none() {
            continue;
        }

        let argv = match vet(run, policy) {
            Err(reason) => {
                outcomes.push(CmdOutcome::Refused {
                    run: run.clone(),
                    reason,
                });
                continue;
            }
            Ok(argv) => argv,
        };

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.current_dir(cwd);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        // Making the child the leader of its own new process group (trap
        // 26) is what lets a timeout kill its descendants too, via
        // `killpg`, rather than only the direct child.
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(not(unix))]
        cmd.kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Err(e) => {
                outcomes.push(CmdOutcome::SpawnFailed {
                    run: run.clone(),
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
                    Some(code) => outcomes.push(CmdOutcome::Ran {
                        run: run.clone(),
                        exit_code: code,
                    }),
                    None => outcomes.push(CmdOutcome::SpawnFailed {
                        run: run.clone(),
                        error: "terminated by signal".to_string(),
                    }),
                }
            }
            Ok(Err(e)) => {
                // `child.wait()` itself errored: exit status is unknown, so
                // this is not a completion — treat it like a runaway and
                // clean up rather than disarm (r1 F1: disarming here left
                // unix with no cleanup path at all, since this diff also
                // moved `kill_on_drop(true)` under `cfg(not(unix))`).
                #[cfg(unix)]
                cleanup_unknown_state(group_guard.as_mut(), &mut child).await;
                outcomes.push(CmdOutcome::SpawnFailed {
                    run: run.clone(),
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
                outcomes.push(CmdOutcome::TimedOut { run: run.clone() });
            }
        }
    }

    outcomes
}

/// Owns the group-kill responsibility for a spawned child's entire process
/// group while `armed`. `Drop` runs a synchronous `killpg(pgid, SIGKILL)` —
/// no `await` needed — so panic, early-return, and task-cancellation paths
/// all still clean up the child's descendants (trap 26), not just the
/// explicit timeout branch.
#[cfg(unix)]
struct ProcessGroupGuard {
    pgid: i32,
    armed: bool,
}

#[cfg(unix)]
impl ProcessGroupGuard {
    /// `process_group(0)` at spawn time makes the child its own group
    /// leader, so its pgid equals its pid. A live child's id is never 0,
    /// but a guard must never be built for 0 anyway: `killpg(0, _)` targets
    /// the *caller's own* process group, not the child's — that would kill
    /// this coordinator/app process instead of the runaway command.
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
/// status is unknown, so this is not a completion. Kill the group if there
/// is one, or fall back to killing just the direct child. Pulled out of
/// `execute_cmd_checks` (r1 F1) so the policy can be pinned by a test: the
/// test suite has no reliable way to force `Child::wait` itself to error.
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
    use std::path::PathBuf;

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

    fn cmd_check(run: &str, expect: &str) -> DodCheck {
        DodCheck::Cmd {
            run: run.to_string(),
            expect: expect.to_string(),
        }
    }

    // --- parse_expect: normal ---

    #[test]
    fn parse_expect_recognizes_exit_n() {
        assert_eq!(parse_expect("exit 0"), Some(0));
        assert_eq!(parse_expect("exit 137"), Some(137));
        assert_eq!(parse_expect(" exit 2 "), Some(2));
    }

    // --- parse_expect: error / boundary ---

    #[test]
    fn parse_expect_rejects_non_exit_n_forms() {
        assert_eq!(parse_expect(""), None);
        assert_eq!(parse_expect("exit"), None);
        assert_eq!(parse_expect("exit abc"), None);
        assert_eq!(parse_expect("exit 0 extra"), None);
        assert_eq!(parse_expect("성공 토스트 노출"), None);
    }

    // --- execute_cmd_checks: normal ---

    #[tokio::test]
    async fn allowed_command_completes_successfully() {
        let policy = CmdPolicy::default_allowlist().allow(&["/usr/bin/true"]);
        let t = task(vec![cmd_check("/usr/bin/true", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert_eq!(
            outcomes,
            vec![CmdOutcome::Ran {
                run: "/usr/bin/true".to_string(),
                exit_code: 0,
            }]
        );
    }

    #[tokio::test]
    async fn allowed_command_reports_nonzero_exit_without_judging() {
        let policy = CmdPolicy::default_allowlist().allow(&["/usr/bin/false"]);
        let t = task(vec![cmd_check("/usr/bin/false", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert_eq!(
            outcomes,
            vec![CmdOutcome::Ran {
                run: "/usr/bin/false".to_string(),
                exit_code: 1,
            }]
        );
    }

    // --- execute_cmd_checks: error ---

    #[tokio::test]
    async fn disallowed_prefix_is_refused() {
        let policy = CmdPolicy::default_allowlist();
        let t = task(vec![cmd_check("rm -rf /", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        match &outcomes[..] {
            [CmdOutcome::Refused { run, .. }] => assert_eq!(run, "rm -rf /"),
            other => panic!("expected a single Refused outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn shell_metacharacters_are_refused() {
        let policy = CmdPolicy::default_allowlist();
        let t = task(vec![cmd_check("cargo test && rm -rf /", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        match &outcomes[..] {
            [CmdOutcome::Refused { .. }] => {}
            other => panic!("expected a single Refused outcome, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nonexistent_binary_produces_spawn_failed() {
        let policy = CmdPolicy::default_allowlist().allow(&["/does/not/exist"]);
        let t = task(vec![cmd_check("/does/not/exist", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        match &outcomes[..] {
            [CmdOutcome::SpawnFailed { run, .. }] => assert_eq!(run, "/does/not/exist"),
            other => panic!("expected a single SpawnFailed outcome, got {other:?}"),
        }
    }

    // --- execute_cmd_checks: boundary ---

    #[tokio::test]
    async fn empty_and_whitespace_only_commands_are_refused() {
        let policy = CmdPolicy::default_allowlist();
        let t = task(vec![cmd_check("", "exit 0"), cmd_check("   ", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert_eq!(outcomes.len(), 2);
        for outcome in &outcomes {
            match outcome {
                CmdOutcome::Refused { reason, .. } => assert_eq!(reason, "empty command"),
                other => panic!("expected Refused(\"empty command\"), got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn slow_command_times_out() {
        let policy = CmdPolicy::default_allowlist()
            .allow(&["/bin/sleep"])
            .with_timeout(Duration::from_millis(50));
        let t = task(vec![cmd_check("/bin/sleep 5", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert_eq!(
            outcomes,
            vec![CmdOutcome::TimedOut {
                run: "/bin/sleep 5".to_string(),
            }]
        );
    }

    #[tokio::test]
    async fn task_with_no_cmd_checks_produces_empty_vec() {
        let policy = CmdPolicy::default_allowlist();
        let t = task(vec![DodCheck::ReqCover { ids: vec![] }]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert!(outcomes.is_empty());
    }

    // --- vet(): trailing-token positional allowlist (review r2 F3) ---
    //
    // These call `vet` directly rather than going through
    // `execute_cmd_checks`: the whole point is to prove the *rejection*
    // happens before any process would spawn, and running a real `cargo`/
    // `npm` invocation from inside this crate's own test suite would be
    // both slow and, for the accepted case, actually execute the flag the
    // rule exists to keep out.

    #[test]
    fn vet_refuses_flag_injection_via_equals_syntax() {
        let policy = CmdPolicy::default_allowlist();

        let result = vet("cargo test --manifest-path=../../evil/Cargo.toml", &policy);

        match result {
            Err(reason) => assert!(
                reason.contains("trailing argument"),
                "reason should mention the trailing argument, got: {reason}"
            ),
            Ok(argv) => panic!("expected Refused, got Ok({argv:?})"),
        }
    }

    #[test]
    fn vet_refuses_prefix_flag_injection() {
        let policy = CmdPolicy::default_allowlist();

        let result = vet("npm run build --prefix=/tmp/evil", &policy);

        assert!(result.is_err(), "expected Refused, got {result:?}");
    }

    #[test]
    fn vet_refuses_bare_flag_with_no_value() {
        let policy = CmdPolicy::default_allowlist();

        let result = vet("cargo test --workspace", &policy);

        assert!(result.is_err(), "expected Refused, got {result:?}");
    }

    #[test]
    fn vet_refuses_trailing_relative_path() {
        let policy = CmdPolicy::default_allowlist();

        let result = vet("cargo test ../../evil", &policy);

        assert!(result.is_err(), "expected Refused, got {result:?}");
    }

    #[test]
    fn vet_refuses_trailing_absolute_path() {
        let policy = CmdPolicy::default_allowlist();

        let result = vet("cargo test /abs/path", &policy);

        assert!(result.is_err(), "expected Refused, got {result:?}");
    }

    #[test]
    fn vet_accepts_bare_trailing_identifier() {
        // Non-regression guard (r2's required last row): the trailing-token
        // rule must not over-reject an ordinary bare-word argument.
        let policy = CmdPolicy::default_allowlist();

        let result = vet("npm run build", &policy);

        assert_eq!(
            result,
            Ok(vec![
                "npm".to_string(),
                "run".to_string(),
                "build".to_string(),
            ])
        );
    }

    // --- vet_run(): public façade over vet() (contracts-m11.md §I4) ---
    //
    // `vet_run` is the seam other crates/tests call instead of duplicating
    // the allowlist rules (HANDOFF §4 잔여 5). These assert the same
    // judgments as the `vet()` tests above, through the public entry point,
    // plus one explicit equivalence check that the façade does not change
    // the underlying verdict.

    #[test]
    fn vet_run_accepts_allowed_prefix() {
        let policy = CmdPolicy::default_allowlist();

        let result = policy.vet_run("cargo test");

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn vet_run_accepts_bare_trailing_identifier() {
        let policy = CmdPolicy::default_allowlist();

        let result = policy.vet_run("npm run build");

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn vet_run_refuses_trailing_flag() {
        let policy = CmdPolicy::default_allowlist();

        let result = policy.vet_run("cargo test --workspace");

        match result {
            Err(reason) => assert!(
                reason.contains("--workspace"),
                "reason should mention the rejected token, got: {reason}"
            ),
            Ok(()) => panic!("expected Refused for a trailing flag"),
        }
    }

    #[test]
    fn vet_run_refuses_unmatched_prefix() {
        let policy = CmdPolicy::default_allowlist();

        let result = policy.vet_run("rm -rf /");

        match result {
            Err(reason) => assert!(
                reason.contains("no allowed prefix"),
                "reason should convey the prefix mismatch, got: {reason}"
            ),
            Ok(()) => panic!("expected Refused for an unmatched prefix"),
        }
    }

    #[test]
    fn vet_run_refuses_empty_string() {
        let policy = CmdPolicy::default_allowlist();

        let result = policy.vet_run("");

        assert_eq!(result, Err("empty command".to_string()));
    }

    #[test]
    fn vet_run_refuses_whitespace_only() {
        let policy = CmdPolicy::default_allowlist();

        let result = policy.vet_run("   ");

        assert_eq!(result, Err("empty command".to_string()));
    }

    #[test]
    fn vet_run_agrees_with_vet_judgment() {
        let policy = CmdPolicy::default_allowlist();
        let inputs = [
            "cargo test",
            "npm run build",
            "cargo test --workspace",
            "rm -rf /",
            "",
            "   ",
        ];

        for input in inputs {
            let vet_run_ok = policy.vet_run(input).is_ok();
            let vet_ok = vet(input, &policy).is_ok();
            assert_eq!(
                vet_run_ok, vet_ok,
                "vet_run and vet disagreed on {input:?}: vet_run.is_ok()={vet_run_ok}, vet.is_ok()={vet_ok}"
            );
        }
    }

    #[tokio::test]
    async fn allowed_prefix_with_bare_trailing_argument_still_executes() {
        // End-to-end companion to vet_accepts_bare_trailing_identifier:
        // proves the positional split doesn't just parse correctly but
        // still lets a legitimate trailing argument reach the child.
        let policy = CmdPolicy::default_allowlist().allow(&["/bin/echo"]);
        let t = task(vec![cmd_check("/bin/echo hello", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert_eq!(
            outcomes,
            vec![CmdOutcome::Ran {
                run: "/bin/echo hello".to_string(),
                exit_code: 0,
            }]
        );
    }

    // --- execute_cmd_checks: process-group cleanup on timeout (trap 26) ---
    //
    // A timed-out command whose child forks its own background grandchild
    // must not leak that grandchild. `slow_command_times_out` above only
    // proves the direct child dies; it says nothing about descendants. This
    // test spawns a shell script that backgrounds a `sleep 300`, records the
    // real pid, and then observes with `libc::kill(pid, 0)` whether that
    // grandchild is still alive after the parent times out.

    /// Kills any grandchild the test recorded and removes its scratch dir,
    /// even if an assertion above panics — the test must never leak a
    /// process, RED run or not.
    struct TestCleanup {
        dir: PathBuf,
        grandchild_pid: Option<i32>,
    }

    impl Drop for TestCleanup {
        fn drop(&mut self) {
            if let Some(pid) = self.grandchild_pid {
                // Best-effort: if the fix under test worked, this is
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
    /// under load the write can still lag a few scheduler ticks behind the
    /// parent returning from `execute_cmd_checks`.
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
    /// elapses. A signal delivery is not synchronous with the `kill(2)`
    /// syscall returning — checking aliveness in the same instant a
    /// `killpg` was issued, with no intervening await, races the kernel.
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

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_command_kills_grandchild_process() {
        let dir = cwd()
            .join("target")
            .join(format!("m10-pgroup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create test scratch dir");
        let mut cleanup = TestCleanup {
            dir: dir.clone(),
            grandchild_pid: None,
        };

        let script_path = dir.join("spawner.sh");
        let pid_file = dir.join("grandchild.pid");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nsleep 300 &\nGCPID=$!\necho \"$GCPID\" > \"{}\"\nsync\nsleep 300\n",
                pid_file.display()
            ),
        )
        .expect("write spawner script");
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)
                .expect("stat spawner script")
                .permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(&script_path, perms).expect("chmod spawner script");
        }
        let script_abs = script_path
            .to_str()
            .expect("scratch path is utf8")
            .to_string();

        // A generous window: under concurrent test-thread load, forking the
        // shell and backgrounding+recording its own grandchild can take
        // longer than a tight timeout would allow, and the group-kill must
        // not fire before the script finishes writing its pid file.
        let policy = CmdPolicy::default_allowlist()
            .allow(&[script_abs.as_str()])
            .with_timeout(Duration::from_millis(1500));
        let t = task(vec![cmd_check(&script_abs, "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert_eq!(
            outcomes,
            vec![CmdOutcome::TimedOut {
                run: script_abs.clone(),
            }]
        );

        let pid_contents = wait_for_nonempty_file(&pid_file, Duration::from_secs(2))
            .await
            .expect("grandchild pid file was never written by the spawner script");
        let grandchild_pid: i32 = pid_contents
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("grandchild pid file {pid_contents:?} did not parse: {e}"));
        assert!(
            grandchild_pid > 1,
            "grandchild pid must be a real pid, got {grandchild_pid}"
        );
        cleanup.grandchild_pid = Some(grandchild_pid);

        let alive = unsafe { libc::kill(grandchild_pid, 0) };
        let probe_err = std::io::Error::last_os_error();
        assert_eq!(
            alive, -1,
            "expected grandchild pid {grandchild_pid} to be dead after the timeout, \
             but kill(pid, 0) returned {alive} (still alive)"
        );
        assert_eq!(
            probe_err.raw_os_error(),
            Some(libc::ESRCH),
            "expected ESRCH (no such process) for grandchild {grandchild_pid}, got {probe_err}"
        );
    }

    // --- cleanup_unknown_state: the wait()-errored branch (r1 F1) ---
    //
    // `child.wait()` returning `Err` is not something the test suite can
    // provoke reliably (it would require corrupting the process table out
    // from under tokio). Instead these pin the extracted cleanup policy
    // directly: with a guard, the whole group dies; without one, the
    // fallback still kills the direct child.

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_unknown_state_kills_the_whole_group_when_a_guard_exists() {
        let dir = cwd()
            .join("target")
            .join(format!("m10-cleanup-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create test scratch dir");
        let mut cleanup = TestCleanup {
            dir: dir.clone(),
            grandchild_pid: None,
        };

        let script_path = dir.join("spawner.sh");
        let pid_file = dir.join("grandchild.pid");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nsleep 300 &\nGCPID=$!\necho \"$GCPID\" > \"{}\"\nsync\nsleep 300\n",
                pid_file.display()
            ),
        )
        .expect("write spawner script");
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path)
                .expect("stat spawner script")
                .permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(&script_path, perms).expect("chmod spawner script");
        }

        let mut cmd = Command::new(&script_path);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.process_group(0);
        let mut child = cmd.spawn().expect("spawn spawner script");
        let mut guard = child
            .id()
            .and_then(ProcessGroupGuard::new)
            .expect("live child has a non-zero id, so a guard must build");

        let pid_contents = wait_for_nonempty_file(&pid_file, Duration::from_secs(2))
            .await
            .expect("grandchild pid file was never written by the spawner script");
        let grandchild_pid: i32 = pid_contents
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("grandchild pid file {pid_contents:?} did not parse: {e}"));
        cleanup.grandchild_pid = Some(grandchild_pid);

        cleanup_unknown_state(Some(&mut guard), &mut child).await;

        assert!(
            wait_until_dead(grandchild_pid, Duration::from_secs(2)).await,
            "expected grandchild pid {grandchild_pid} to be dead after cleanup_unknown_state, \
             but it was still alive after waiting"
        );

        let _ = child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_unknown_state_falls_back_to_killing_the_child_without_a_guard() {
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("300");
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        let mut child = cmd.spawn().expect("spawn /bin/sleep");
        let pid = child.id().expect("live child has a pid") as i32;

        cleanup_unknown_state(None, &mut child).await;

        let alive = unsafe { libc::kill(pid, 0) };
        let probe_err = std::io::Error::last_os_error();
        assert_eq!(
            alive, -1,
            "expected child pid {pid} to be dead after the no-guard fallback, \
             but kill(pid, 0) returned {alive} (still alive)"
        );
        assert_eq!(
            probe_err.raw_os_error(),
            Some(libc::ESRCH),
            "expected ESRCH (no such process) for pid {pid}, got {probe_err}"
        );
    }

    // --- ProcessGroupGuard: boundary ---

    #[cfg(unix)]
    #[test]
    fn process_group_guard_refuses_pgid_zero() {
        // killpg(0, SIG) targets the *caller's* process group, not a
        // child's — building a guard for child_id 0 must be impossible.
        assert!(ProcessGroupGuard::new(0).is_none());
    }

    // --- execute_cmd_checks: normal completion still runs in a new group ---

    #[tokio::test]
    async fn allowed_command_in_new_process_group_still_completes_normally() {
        // Regression companion to allowed_command_completes_successfully:
        // proves `process_group(0)` doesn't change the happy path, and that
        // the disarmed guard on the completion branch doesn't kill anything
        // — if it did, this test process (in the same original group up
        // until the child's own group is formed) would be unaffected either
        // way, so the real evidence is that `Ran` is still reported with
        // the right exit code, i.e. nothing raced the completion.
        let policy = CmdPolicy::default_allowlist().allow(&["/usr/bin/true"]);
        let t = task(vec![cmd_check("/usr/bin/true", "exit 0")]);

        let outcomes = execute_cmd_checks(&t, &cwd(), &policy).await;

        assert_eq!(
            outcomes,
            vec![CmdOutcome::Ran {
                run: "/usr/bin/true".to_string(),
                exit_code: 0,
            }]
        );
    }
}
