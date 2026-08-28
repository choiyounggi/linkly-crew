pub mod claude;
pub mod error;
pub mod event;

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
    /// Session id the CLI itself reported on its `system/init` line, set at
    /// most once by the reader task (D3, M5 t-handoff plan) — shared via
    /// `Arc` because the reader task is spawned, and so writes to it,
    /// independently of this struct's own lifetime.
    pub(crate) reported_session_id: Arc<OnceLock<String>>,
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

/// A harness's account of a live session's identity, for handoff packs
/// (contracts-m5.md C4a, verbatim).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandoffSnapshot {
    pub harness: String,
    pub session_id: String,
    pub notes: String,
}

/// Adapter contract every CLI-backed agent harness implements.
/// Reduced from DESIGN.md §2.3 for M1 — see plan D9 (no `snapshot`/
/// `HandoffPack`; that's M5 scope, added below by contracts-m5.md C4a).
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

    /// Snapshots `session`'s identity for a handoff pack (contracts-m5.md
    /// C4a) — required (not default) since only the implementation knows
    /// what its own session identity actually is.
    async fn snapshot(&self, session: &Session) -> Result<HandoffSnapshot, HarnessError>;

    async fn shutdown(&self, session: Session) -> Result<(), HarnessError>;
}

#[cfg(test)]
mod handoff_snapshot_tests {
    use super::HandoffSnapshot;

    #[test]
    fn round_trips_through_serde() {
        let snap = HandoffSnapshot {
            harness: "claude-code".to_string(),
            session_id: "11111111-1111-4111-8111-111111111111".to_string(),
            notes: "some notes".to_string(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: HandoffSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn empty_notes_round_trips_as_boundary() {
        let snap = HandoffSnapshot {
            harness: "claude-code".to_string(),
            session_id: "11111111-1111-4111-8111-111111111111".to_string(),
            notes: String::new(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: HandoffSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn missing_required_field_fails_to_deserialize() {
        // No natural failure path exists for ClaudeCodeHarness::snapshot
        // itself (it always succeeds) — this substitutes the required
        // error-case test with HandoffSnapshot's own serde contract, per
        // the plan's D6 fallback.
        let json = r#"{"harness":"claude-code","session_id":"11111111-1111-4111-8111-111111111111"}"#;
        let err = serde_json::from_str::<HandoffSnapshot>(json).unwrap_err();
        assert!(err.to_string().contains("notes"));
    }
}
