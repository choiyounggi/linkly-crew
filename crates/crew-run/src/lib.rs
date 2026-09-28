//! crew-run — the m3_sprint.rs assembly pattern (ephemeral-port bus +
//! ledger + single subscription loop + 5 workers + Lead) promoted to a
//! reusable run controller (contract §C3, DESIGN.md HANDOFF §4).

mod config;
mod controller;
mod events;
mod gitflow;
mod observe;
mod worktree;

pub use config::{
    default_dev_cmd_checks_node, default_dev_cmd_checks_rust, BrowserCheck, GateDecision,
    RunConfig, RunError, RunMode,
};
pub use crew_lead::plan::CmdCheck;
pub use controller::{validate_roster, RunController, RunHandle};
pub use events::{GitFlowKindDto, RosterAgentDto, RunEvent, RunOutcomeDto, RunSnapshot, StoredMessageDto, TaskStateDto};
