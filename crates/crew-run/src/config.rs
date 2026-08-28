//! Run configuration and error types — contract §C3 verbatim.

use std::path::PathBuf;

use crew_agent::BusError;
use crew_ledger::LedgerError;
use crew_lead::plan::PlanError;

/// One run's configuration (contract §C3).
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
}

/// How the run's five crew-member workers behave (contract §C3).
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
}
