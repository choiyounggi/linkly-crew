//! `opencode` adapter stub (contracts-m5.md C4b, plan D4). Implements the
//! full `Harness` trait so `HarnessRegistry::make("opencode")` type-checks
//! and the pool/registry can treat it uniformly, but `spawn` always fails —
//! there is no real opencode CLI integration yet (M5 C0 scope). Since
//! `spawn` never succeeds, a `Session` can never exist for this harness; the
//! remaining methods are defensive rather than reachable in practice.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{
    AgentCfg, HandoffSnapshot, Harness, HarnessError, HarnessEvent, HarnessId, Session,
    TurnOutcome, UserTurn,
};

const HARNESS_ID: HarnessId = HarnessId("opencode");

#[derive(Debug, Default)]
pub struct OpencodeHarness;

impl OpencodeHarness {
    pub fn new() -> Self {
        Self
    }
}

fn unavailable() -> HarnessError {
    HarnessError::Unavailable("opencode adapter is a stub (M5 C0)".to_string())
}

#[async_trait]
impl Harness for OpencodeHarness {
    fn id(&self) -> HarnessId {
        HARNESS_ID
    }

    async fn spawn(&self, _cfg: &AgentCfg) -> Result<Session, HarnessError> {
        Err(unavailable())
    }

    async fn send(
        &self,
        _session: &mut Session,
        _turn: UserTurn,
        _timeout: Duration,
    ) -> Result<TurnOutcome, HarnessError> {
        Err(unavailable())
    }

    fn take_events(&self, _session: &mut Session) -> mpsc::Receiver<HarnessEvent> {
        // Sender is dropped immediately, so the receiver reads as closed.
        let (_tx, rx) = mpsc::channel(1);
        rx
    }

    async fn snapshot(&self, _session: &Session) -> Result<HandoffSnapshot, HarnessError> {
        Err(unavailable())
    }

    async fn shutdown(&self, _session: Session) -> Result<(), HarnessError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_is_unavailable() {
        let harness = OpencodeHarness::new();
        let cfg = AgentCfg {
            cwd: std::env::current_dir().unwrap(),
        };
        let result = harness.spawn(&cfg).await;
        let err = match result {
            Err(err) => err,
            Ok(_) => panic!("spawn should always fail on the stub adapter"),
        };
        assert!(matches!(err, HarnessError::Unavailable(_)));
        assert!(err.to_string().contains("harness unavailable"));
    }

    #[test]
    fn id_is_opencode() {
        let harness = OpencodeHarness::new();
        assert_eq!(harness.id().0, "opencode");
    }
}
