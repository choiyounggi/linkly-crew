//! Agent control channel (contracts-m6.md §D1, verbatim): lets a caller
//! outside `AgentRunner`'s bus loop (t-swapnow's `crew-run`) direct a live
//! agent to perform a mid-sprint harness swap.

use crew_harness::{HandoffSnapshot, Harness, HarnessError};

/// 러너 외부에서 실행 중인 에이전트로 보내는 제어 명령.
pub enum AgentControl {
    /// mid-sprint 하네스 스왑: snapshot→shutdown→(다음 턴 lazy respawn).
    Swap {
        harness: std::sync::Arc<dyn Harness>,
        harness_id: String,
        injected_context: String, // 새 세션 첫 턴 앞에 주입할 핸드오프 텍스트
        ack: tokio::sync::oneshot::Sender<Result<HandoffSnapshot, HarnessError>>,
    },
}
