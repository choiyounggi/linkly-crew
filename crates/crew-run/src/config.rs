//! Run configuration and error types — contract §C3 verbatim, extended by
//! contracts-m5.md §C5a (multi-sprint fields).

use std::path::PathBuf;

use crew_agent::BusError;
use crew_ledger::LedgerError;
use crew_lead::plan::{CmdCheck, PlanError};

/// One run's configuration (contract §C3, extended by contracts-m5.md
/// §C5a).
pub struct RunConfig {
    pub goal: String,
    pub mode: RunMode,
    /// Caller-supplied directory the run's ledger lives under. `/tmp` and
    /// `$TMPDIR` are forbidden by the workspace constraints — the app's
    /// default (t-bridge) is `~/.linkly-crew/app-runs/<run_id>/`.
    pub data_dir: PathBuf,
    /// Lead's per-task rework budget. Default `AcceptanceLoop::default_budget()`
    /// (contracts-m3.md layering rule: Lead's budget must exhaust before
    /// CorrGuard's separate max_rounds would).
    pub max_rework: u32,
    /// Sprint size cap (contracts-m5.md §C5a). `0` = every task in one
    /// sprint (current single-sprint behavior, unchanged).
    pub max_per_sprint: usize,
    /// `Lead`'s escalation-cascade timeout, forwarded verbatim to
    /// `LeadBehavior::escalation_timeout_ms` (contracts-m5.md §C5a). `0` =
    /// disabled (default).
    pub escalation_timeout_ms: u64,
    /// Agent roster (contracts-m5.md §C5a). `None` = the default 6-slot
    /// roster (lead + 5 roles, all `claude-code`/`"default"`).
    pub roster: Option<crew_proto::Roster>,
    /// `cmd` DoD checks to attach to the Developer task (contracts-m10.md
    /// §H1g). Empty vector (default) = no emission = the M9-and-earlier
    /// behavior unchanged.
    pub dev_cmd_checks: Vec<CmdCheck>,
    /// The real project tree the crew works in and the Lead's `cmd` DoD checks
    /// execute in. `None` (the shipped default) = the M10-and-earlier behavior
    /// exactly: every role's CLI cwd and the Cmd DoD exec cwd are the per-role
    /// scratch `<data_dir>/cli-cwd/<role>`. `Some(root)` = `root` must be a git
    /// repository; every role's CLI cwd and the Cmd DoD exec cwd are instead
    /// that role's own out-of-repo `git worktree` (`crew/<role>` branch) under
    /// it — distinct per role, never `root` itself (HANDOFF trap 30, this
    /// crate's `worktree` module) — with a shared `<root>/.crew/artifacts`
    /// convention injected into each role's spawn spec.
    ///
    /// Caller-designated only — never derived from cwd, git, or a guess
    /// (HANDOFF §5 trap 29 / contracts-m11.md §I6). With `Some`, the Lead
    /// executes agent-authored code by design; the containment is this
    /// human-designated tree, not the argv allowlist.
    pub project_root: Option<PathBuf>,
    /// Per-turn timeout (seconds) for every role worker's CLI turn and the
    /// Lead's LLM `specify` call (M13 turn-recovery fix D3). Default 900 —
    /// matches the task deadline (`deadline_ms` 900_000). `0` is rejected by
    /// `start_run` as `RunError::ConfigInvalid` before any spawn.
    pub turn_timeout_secs: u64,
}

/// Default Rust-domain dev cmd checks that satisfy `CmdPolicy`'s positional
/// allowlist (contracts-m9.md §G2c / contracts-m10.md §H1g): the prefix
/// tokens equal an allowed prefix literally, and every trailing token is a
/// bare identifier. `cargo test --workspace` is not used here — its trailing
/// `--workspace` flag fails the bare-identifier rule and would be `Refused`.
/// Not a default — `RunConfig::dev_cmd_checks`'s shipped default stays empty.
pub fn default_dev_cmd_checks_rust() -> Vec<CmdCheck> {
    vec![CmdCheck {
        run: "cargo test".to_string(),
        expect: "exit 0".to_string(),
    }]
}

/// Default Node-domain dev cmd checks — see [`default_dev_cmd_checks_rust`]
/// for the allowlist rule they satisfy. Not a default — `RunConfig`'s
/// shipped default stays empty.
pub fn default_dev_cmd_checks_node() -> Vec<CmdCheck> {
    vec![
        CmdCheck {
            run: "npm test".to_string(),
            expect: "exit 0".to_string(),
        },
        CmdCheck {
            run: "npm run build".to_string(),
            expect: "exit 0".to_string(),
        },
    ]
}

/// How the run's five crew-member workers behave (contract §C3). `Clone`
/// so the multi-sprint loop (contracts-m5.md §C5a) can hold one `RunConfig`
/// value while re-spawning fresh workers from the same `mode` at every
/// sprint boundary.
#[derive(Clone)]
pub enum RunMode {
    /// Deterministic demo/test: five `ScriptedCrewMember`s. `planted_violations`
    /// maps a role to the REQ ids it omits from coverage on its first attempt.
    Scripted {
        planted_violations: Vec<(crew_proto::Role, Vec<String>)>,
    },
    /// Five real CLI role sessions (`RoleHarnessBehavior` + `ClaudeCodeHarness`).
    /// Type/assembly only — never exercised by a worker/CI test.
    RealCli,
}

/// A human reviewer's decision on an `Escalated` task (contracts-m7.md §E5),
/// carried through `RunHandle::resolve_gate` into a `human.response`
/// envelope (§E2 verbatim: wire values `"approve"`/`"reject"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Approve,
    Reject,
}

/// Errors from starting or running a `crew-run` (plan D7).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("failed to build the sprint spec/dag: {0}")]
    SpecFailed(#[from] PlanError),
    #[error("bus connection failed: {0}")]
    Bus(#[from] BusError),
    #[error("ledger error: {0}")]
    Ledger(#[from] LedgerError),
    #[error("lead runner failed: {0}")]
    Join(String),
    /// `RunHandle::swap_harness` validation failure (contracts-m5.md §C5c,
    /// t-swap plan D2) — unknown `agent_id` or unknown `harness` id, message
    /// text distinguishes the two. The roster is left unchanged and no
    /// event is emitted when this is returned.
    #[error("swap rejected: {0}")]
    SwapRejected(String),
    /// `RunHandle::swap_harness` step 3 (mid-sprint immediate effectuation,
    /// contracts-m6.md §D2b, t-swap plan D4) failed to reach the live
    /// worker: send failure, ack timeout, worker-dropped ack, or a harness
    /// error from the swap itself. The roster mutation, handoff envelope,
    /// and `RosterChanged` from steps 1-2 are unaffected — this only means
    /// the swap falls back to taking effect at the next sprint boundary
    /// (M5 semantics), not that it failed outright.
    #[error("swap incomplete, falls back to next sprint boundary: {0}")]
    SwapIncomplete(String),
    /// `validate_roster` rejected `RunConfig::roster` (contracts-m7.md §E4)
    /// — `RunController::start` returns this before any spawn or ledger
    /// creation, so a rejected roster leaves no partial run state.
    #[error("invalid roster: {0}")]
    RosterInvalid(String),
    /// `RunHandle::resolve_gate` (contracts-m7.md §E5) could not reach the
    /// run-resident `agent:human` proxy — the run has already ended (the
    /// proxy is torn down alongside it, plan D1) or the bus rejected the
    /// `human.response` publish.
    #[error("gate unavailable: {0}")]
    GateUnavailable(String),
    /// `RunConfig.project_root` failed startup validation (contracts-m11.md
    /// §I2): not absolute, or not an existing directory. The message carries
    /// the received value verbatim. No fallback to the scratch cwd — a silent
    /// fallback reproduces M10's exit-101 confusion (docs/SPIKE-M10.md §3).
    #[error("project_root invalid: {0}")]
    ProjectRootInvalid(String),
    /// `RunConfig` failed startup validation before any spawn (M13
    /// turn-recovery fix D4) — e.g. `turn_timeout_secs == 0`, which would
    /// otherwise make every turn instantly time out.
    #[error("invalid run config: {0}")]
    ConfigInvalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_lead::cmd_exec::CmdPolicy;

    #[test]
    fn default_dev_cmd_checks_rust_satisfies_the_positional_allowlist() {
        // Asserts each `run` against the production judge
        // (`CmdPolicy::default_allowlist().vet_run`, contracts-m9.md §G2c r2
        // F3) rather than a locally-reproduced copy of its rule (M11 D1/D3 —
        // the guard's assertion target moves from the string's *shape* to
        // the production judge's *consequence*, guard-shape-vs-consequence).
        let policy = CmdPolicy::default_allowlist();
        let checks = default_dev_cmd_checks_rust();
        assert!(!checks.is_empty());
        for c in &checks {
            assert_eq!(policy.vet_run(&c.run), Ok(()));
            assert_eq!(c.expect, "exit 0");
        }
    }

    #[test]
    fn default_dev_cmd_checks_node_satisfies_the_positional_allowlist() {
        let policy = CmdPolicy::default_allowlist();
        let checks = default_dev_cmd_checks_node();
        assert!(!checks.is_empty());
        for c in &checks {
            assert_eq!(policy.vet_run(&c.run), Ok(()));
            assert_eq!(c.expect, "exit 0");
        }
    }

    #[test]
    fn cmd_policy_refuses_a_trailing_flag_as_a_negative_control() {
        // Negative control (testing-quality-tests-that-cannot-fail): proves
        // the production judge can actually refuse, guarding against a
        // helper/policy that accepts everything (M11 D2 — the negative
        // control itself is kept, only its assertion mechanism changes).
        let policy = CmdPolicy::default_allowlist();
        assert!(
            policy.vet_run("cargo test --workspace").is_err(),
            "a trailing flag must be refused"
        );
    }
}
