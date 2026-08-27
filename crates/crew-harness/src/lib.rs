pub mod claude;
pub mod error;
pub mod event;

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub use error::HarnessError;
pub use event::{judge_result, HarnessEvent, TurnOutcome};

/// Default turn timeout for [`Harness::send`] callers that don't need a
/// shorter one (tests inject a short duration explicitly — see D6).
pub const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(120);

/// Identifies which harness implementation produced a [`Session`]
/// (`claude-code`, `codex`, ... — DESIGN.md §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessId(pub &'static str);

impl fmt::Display for HarnessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Configuration needed to spawn an agent's process. Only the fields the
/// M1 spawn command (D3) actually requires — no speculative fields.
#[derive(Debug, Clone)]
pub struct AgentCfg {
    /// Working directory the CLI process runs in.
    pub cwd: PathBuf,
}

/// A single user turn sent to a live session.
#[derive(Debug, Clone)]
pub struct UserTurn {
    pub text: String,
}

/// A live, long-lived CLI process plus the plumbing needed to drive it.
///
/// Owned opaquely by callers of [`Harness`]; constructed only by a
/// `Harness` implementation (`claude.rs`).
pub struct Session {
    pub(crate) session_id: uuid::Uuid,
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    /// Every parsed event, for [`Harness::take_events`] consumers.
    pub(crate) events_rx: Option<mpsc::Receiver<HarnessEvent>>,
    /// The reader task's judged outcome for the in-flight turn, one slot at
    /// a time — `send` takes `&mut Session` so turns never overlap.
    pub(crate) turn_rx: mpsc::Receiver<TurnOutcome>,
    pub(crate) reader_task: JoinHandle<()>,
    pub(crate) stderr_task: JoinHandle<()>,
}

impl Session {
    pub fn session_id(&self) -> uuid::Uuid {
        self.session_id
    }

    /// Wait for the child process to exit. Exposed so callers (and tests)
    /// can confirm a killed child actually terminated.
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

/// Adapter contract every CLI-backed agent harness implements.
/// Reduced from DESIGN.md §2.3 for M1 — see plan D9 (no `snapshot`/
/// `HandoffPack`; that's M5 scope).
#[async_trait]
pub trait Harness: Send + Sync {
    fn id(&self) -> HarnessId;

    async fn spawn(&self, cfg: &AgentCfg) -> Result<Session, HarnessError>;

    async fn send(
        &self,
        session: &mut Session,
        turn: UserTurn,
        timeout: Duration,
    ) -> Result<TurnOutcome, HarnessError>;

    fn take_events(&self, session: &mut Session) -> mpsc::Receiver<HarnessEvent>;

    async fn shutdown(&self, session: Session) -> Result<(), HarnessError>;
}
