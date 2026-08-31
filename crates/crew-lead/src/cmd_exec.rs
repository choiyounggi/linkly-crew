//! contract: t-cmd owns the implementation (run m9a, contracts-m9.md §G2b).
//!
//! `DodCheck::Cmd` executor. `dod_exec::judge` stays a pure function; execution
//! happens in this module, ahead of `judge`, and its outcomes are injected into
//! `judge` as its third argument. The seam exists because `judge`'s only caller
//! — `Lead::handle_task_result` (`dispatch.rs:295`) — is a synchronous pure
//! state machine that must not become async.
//!
//! Safety boundary (contracts-m9.md §G2c): no shell, argv exec only, and the
//! command text is vetted against a positive character allowlist plus a command
//! prefix allowlist — never a metacharacter blocklist.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crew_proto::{DodCheck, TaskSpec};
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
    for tok in &tokens {
        if !tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/@:=+-".contains(c))
        {
            return Err(format!("token {tok:?} outside the allowed character set"));
        }
    }
    let matches_prefix = policy.allowed_prefixes.iter().any(|prefix| {
        tokens.len() >= prefix.len() && tokens[..prefix.len()] == prefix[..]
    });
    if !matches_prefix {
        return Err(format!("{tokens:?} matches no allowed prefix"));
    }
    Ok(tokens)
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

        match timeout(policy.timeout, child.wait()).await {
            Ok(Ok(status)) => match status.code() {
                Some(code) => outcomes.push(CmdOutcome::Ran {
                    run: run.clone(),
                    exit_code: code,
                }),
                None => outcomes.push(CmdOutcome::SpawnFailed {
                    run: run.clone(),
                    error: "terminated by signal".to_string(),
                }),
            },
            Ok(Err(e)) => outcomes.push(CmdOutcome::SpawnFailed {
                run: run.clone(),
                error: e.to_string(),
            }),
            Err(_) => {
                let _ = child.kill().await;
                outcomes.push(CmdOutcome::TimedOut { run: run.clone() });
            }
        }
    }

    outcomes
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
}
