use async_trait::async_trait;
use crew_proto::Envelope;

/// Pure role-logic contract (plan A4): internal state transitions only, no
/// I/O. `AgentRunner` (A5) is the only caller — it sends whatever envelopes
/// a call returns and feeds every received envelope back through
/// `on_envelope`.
#[async_trait]
pub trait RoleBehavior {
    async fn on_start(&mut self) -> Vec<Envelope> {
        vec![]
    }

    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope>;

    fn is_done(&self) -> bool;

    /// `None` (default) disables the tick branch entirely — `AgentRunner`
    /// falls back to its original recv-only loop (contract C2).
    fn tick_interval(&self) -> Option<std::time::Duration> {
        None
    }

    /// `now_ms` is passed in (not read from the system clock) so callers
    /// can drive this deterministically in tests (contract C2).
    async fn on_tick(&mut self, _now_ms: u64) -> Vec<Envelope> {
        Vec::new()
    }
}
