//! Run configuration and error types — contract §C3 verbatim, extended by
//! contracts-m5.md §C5a (multi-sprint fields).

use std::path::PathBuf;

use crew_agent::BusError;
use crew_ledger::LedgerError;
use crew_lead::plan::PlanError;

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
}
