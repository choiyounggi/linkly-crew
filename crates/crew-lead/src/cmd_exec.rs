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
use std::time::Duration;

use crew_proto::TaskSpec;

/// Which `DodCheck::Cmd` commands may run, and for how long.
pub struct CmdPolicy {
    #[allow(dead_code)]
    allowed_prefixes: Vec<Vec<String>>,
    #[allow(dead_code)]
    timeout: Duration,
}

impl CmdPolicy {
    /// M9 default: verification commands of the web-app / Rust domain only.
    pub fn default_allowlist() -> Self {
        todo!("t-cmd")
    }

    pub fn with_timeout(self, _timeout: Duration) -> Self {
        todo!("t-cmd")
    }

    pub fn allow(self, _prefix: &[&str]) -> Self {
        todo!("t-cmd")
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
pub fn parse_expect(_expect: &str) -> Option<i32> {
    todo!("t-cmd")
}

/// Runs every `DodCheck::Cmd` in `task.dod` that the policy permits and whose
/// `expect` parses. Checks with an unparseable `expect` produce no outcome, so
/// `judge` records them as `skipped`.
pub async fn execute_cmd_checks(
    _task: &TaskSpec,
    _cwd: &Path,
    _policy: &CmdPolicy,
) -> Vec<CmdOutcome> {
    todo!("t-cmd")
}
