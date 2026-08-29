//! crew-run — the m3_sprint.rs assembly pattern (ephemeral-port bus +
//! ledger + single subscription loop + 5 workers + Lead) promoted to a
//! reusable run controller (contract §C3, DESIGN.md HANDOFF §4).

mod config;
mod controller;
mod events;
mod observe;

pub use config::{RunConfig, RunError, RunMode};
pub use controller::{RunController, RunHandle};
pub use events::{RosterAgentDto, RunEvent, RunOutcomeDto, RunSnapshot, StoredMessageDto, TaskStateDto};
