//! crew-agent — WS agent-runner client + role behaviors (DESIGN.md §3/§11 M2).

mod bus;
mod designer;
mod harness_behavior;
mod pm;
mod role;
mod runner;

pub use bus::{BusConn, BusError, BusEvent};
pub use designer::ScriptedDesigner;
pub use harness_behavior::DesignerHarnessBehavior;
pub use pm::{PmState, ScriptedPm};
pub use role::RoleBehavior;
pub use runner::{AgentRunner, RunnerError};
