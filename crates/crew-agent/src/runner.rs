use crew_proto::{
    presence_read_body, presence_typing_body, Envelope, MessageKind, PresenceReadBody, PresenceTypingBody,
};
use serde_json::Value;

use crew_harness::{HandoffSnapshot, HarnessError};

use crate::bus::{BusConn, BusError, BusEvent};
use crate::control::AgentControl;
use crate::role::RoleBehavior;

/// Bus-level error codes that abort the runner instead of being logged and
/// skipped (plan A5) — in the M2 scenario these mean the conversation
/// itself cannot continue, so surfacing them as a test/caller failure is
/// correct rather than silently retrying.
const FATAL_ERROR_CODES: &[&str] = &["loop_blocked", "delivery_failed"];

/// Notes on the `Ok` ack a queued `AgentControl::Swap` gets when its worker
/// ended legitimately (aborted at a boundary/shutdown, or returned) before
/// serving it — the same outcome crew-run reports for an absent sender.
pub(crate) const UNSERVED_ON_EXIT_NOTES: &str =
    "worker ended before serving this control; no live session";

/// How the runner's task ended — decides the answer an unserved control gets.
enum ExitKind {
    /// Initial value; still set in `Drop` means the future was dropped
    /// without returning (abort at a sprint boundary or `RunHandle::shutdown`).
    Aborted,
    Returned,
    /// The runner returned `Err`; holds `format!("{err:?}")`.
    Failed(String),
    Panicked,
}

/// Owns the worker's control receiver for the life of `run_with_control`.
/// On drop (every return, error, abort or panic) it hands the receiver to a
/// task that answers every control still queued or sent later, until the
/// last Sender is gone — so crew-run never sees a dropped ack or a closed
/// channel from a worker that merely ended. No `close()`: a Sender cloned
/// before the exit must still be able to deliver and get an answer.
struct CtrlInbox {
    rx: Option<tokio::sync::mpsc::Receiver<AgentControl>>,
    exit: ExitKind,
}

fn answer_unserved(ctrl: AgentControl, exit: &ExitKind) {
    match ctrl {
        AgentControl::Swap {
            harness_id, ack, ..
        } => {
            let answer = match exit {
                ExitKind::Aborted | ExitKind::Returned => Ok(HandoffSnapshot {
                    harness: harness_id,
                    session_id: String::new(),
                    notes: UNSERVED_ON_EXIT_NOTES.to_string(),
                }),
                ExitKind::Failed(e) => Err(HarnessError::Unavailable(format!(
                    "worker ended with an error before serving this control: {e}"
                ))),
                ExitKind::Panicked => Err(HarnessError::Unavailable(
                    "worker panicked before serving this control".to_string(),
                )),
            };
            let _ = ack.send(answer);
        }
    }
}

impl Drop for CtrlInbox {
    fn drop(&mut self) {
        // tokio drops a panicked task's future while the panic unwinds
        // (runtime/task/harness.rs poll_future Guard); an abort drops it
        // outside any unwind. Relies on the default panic=unwind.
        if matches!(self.exit, ExitKind::Aborted) && std::thread::panicking() {
            self.exit = ExitKind::Panicked;
        }
        let Some(mut rx) = self.rx.take() else { return };
        let exit = std::mem::replace(&mut self.exit, ExitKind::Aborted);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    let mut answered = 0usize;
                    while let Some(ctrl) = rx.recv().await {
                        answer_unserved(ctrl, &exit);
                        answered += 1;
                    }
                    if answered > 0 {
                        tracing::debug!(
                            answered,
                            "answered control messages left unserved when the runner ended"
                        );
                    }
                });
            }
            Err(_) => {
                while let Ok(ctrl) = rx.try_recv() {
                    answer_unserved(ctrl, &exit);
                }
            }
        }
    }
}

/// t2-be-presence D2: `presence.read`/`presence.typing` deadline — reused
/// from the same 900s convention every other envelope in this crate uses
/// (`DEADLINE_MS` in `crew_member.rs`/`designer.rs`/`harness_behavior.rs`/
/// `pm.rs`); `deadline_ms` is validated (`> 0`) but not otherwise consumed
/// at runtime, so there is no reason to invent a different value here.
const PRESENCE_DEADLINE_MS: u64 = 900_000;

/// Builds a presence envelope reporting on `processing` — addressed back to
/// `processing.from` (the sender being told "I'm reading/typing"),
/// `requires_ack = false` (D1: presence is a volatile signal, intentionally
/// never ledgered — see `crew-run/src/controller.rs`'s presence guard).
/// `agent_id` is `processing.to`'s first recipient (the agent now handling
/// `processing`), the same derivation every `reply()` helper in this crate
/// already uses for "who am I" (e.g. `harness_behavior.rs::reply`).
fn presence_envelope(processing: &Envelope, kind: MessageKind, body: Value) -> Envelope {
    Envelope::new(
        processing.sprint.clone(),
        processing.thread.clone(),
        processing.to.first().cloned().unwrap_or_default(),
        vec![processing.from.clone()],
        kind,
        Some(processing.id.clone()),
        processing.corr.clone(),
        body,
        vec![],
        false,
        PRESENCE_DEADLINE_MS,
    )
}

/// Emits `presence.read` then `presence.typing{active:true}` for `env`,
/// in that order (D2/R1/R2) via [`BusConn::send_best_effort`] — a single
/// write each, no receipt wait/retry, failure ignored (D2 "실패 무시").
/// `BusConn::send`'s normal receipt-wait doesn't apply here: it never
/// completes for a `requires_ack=false` envelope on a real bus (see
/// `send_best_effort`'s doc comment) — using it here would block every
/// envelope this crate processes for several seconds per presence signal.
async fn emit_presence_start(conn: &BusConn, env: &Envelope) {
    let agent_id = env.to.first().cloned().unwrap_or_default();

    let read = presence_envelope(
        env,
        MessageKind::PresenceRead,
        presence_read_body(&PresenceReadBody {
            agent_id: agent_id.clone(),
            target_msg_id: env.id.clone(),
        }),
    );
    let _ = conn.send_best_effort(read).await;

    let typing_on = presence_envelope(
        env,
        MessageKind::PresenceTyping,
        presence_typing_body(&PresenceTypingBody { agent_id, active: true }),
    );
    let _ = conn.send_best_effort(typing_on).await;
}

/// Emits `presence.typing{active:false}` for `env` (D2/R2) — same
/// best-effort semantics as [`emit_presence_start`].
async fn emit_presence_end(conn: &BusConn, env: &Envelope) {
    let agent_id = env.to.first().cloned().unwrap_or_default();
    let typing_off = presence_envelope(
        env,
        MessageKind::PresenceTyping,
        presence_typing_body(&PresenceTypingBody { agent_id, active: false }),
    );
    let _ = conn.send_best_effort(typing_off).await;
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("failed to send an envelope: {0}")]
    Send(#[from] BusError),
    #[error("bus reported a fatal error: {code}: {message}")]
    Fatal { code: String, message: String },
    #[error("bus connection closed unexpectedly")]
    ConnectionClosed,
}

/// Drives one `RoleBehavior` against one `BusConn` (plan A5): send
/// `on_start`'s envelopes, then loop receiving envelopes and sending back
/// whatever `on_envelope` returns, until `is_done()`. When
/// `behavior.tick_interval()` is `Some(iv)` (contract C2), an interval fires
/// alongside `recv` via `tokio::select!` and `on_tick` is called on every
/// tick; when it is `None` the tick branch's `select!` guard is always
/// false, so the loop is exactly the original recv-only loop.
///
/// t2-be-presence D2: this is the crate's single presence emission point —
/// every non-presence received envelope is bracketed with
/// `presence.read` + `presence.typing{active:true}` before `on_envelope`
/// runs and `presence.typing{active:false}` after, for every `RoleBehavior`
/// impl uniformly (`ScriptedCrewMember`/`ScriptedDesigner`/`ScriptedPm`'s
/// synchronous processing and `RoleHarnessBehavior`/`DesignerHarnessBehavior`'s
/// real CLI turn alike) — see `emit_presence_start`/`emit_presence_end`
/// below and the `select!` arm that calls them. No individual
/// `RoleBehavior` impl emits presence itself.
pub struct AgentRunner;

impl AgentRunner {
    /// Wrapper (contracts-m6.md §D1 D10): delegates to `run_with_control`
    /// with a control channel whose sender is dropped immediately, so the
    /// ctrl branch is disabled from the first `select!` iteration onward
    /// (D3) — behavior and signature identical to before the control
    /// channel existed.
    pub async fn run<B>(conn: BusConn, behavior: B) -> Result<(), RunnerError>
    where
        B: RoleBehavior + Send,
    {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Self::run_with_control(conn, behavior, rx).await
    }

    /// Same loop as [`AgentRunner::run`], with an external control channel
    /// (contracts-m6.md §D1). Every control message accepted into `ctrl_rx`
    /// is answered even when this runner ends first (see [`CtrlInbox`]).
    pub async fn run_with_control<B>(
        conn: BusConn,
        behavior: B,
        ctrl_rx: tokio::sync::mpsc::Receiver<AgentControl>,
    ) -> Result<(), RunnerError>
    where
        B: RoleBehavior + Send,
    {
        let mut inbox = CtrlInbox {
            rx: Some(ctrl_rx),
            exit: ExitKind::Aborted,
        };
        let result = Self::run_with_control_inner(
            conn,
            behavior,
            inbox
                .rx
                .as_mut()
                .expect("CtrlInbox holds its receiver until drop"),
        )
        .await;
        inbox.exit = match &result {
            Ok(()) => ExitKind::Returned,
            Err(e) => ExitKind::Failed(format!("{e:?}")),
        };
        result
    }

    async fn run_with_control_inner<B>(
        mut conn: BusConn,
        mut behavior: B,
        ctrl_rx: &mut tokio::sync::mpsc::Receiver<AgentControl>,
    ) -> Result<(), RunnerError>
    where
        B: RoleBehavior + Send,
    {
        for env in behavior.on_start().await {
            conn.send(env).await?;
        }
        if behavior.is_done() {
            return Ok(());
        }

        let mut ticker = behavior.tick_interval().map(tokio::time::interval);
        // `ctrl_rx.recv()` on a closed channel returns `None` immediately,
        // which would busy-loop this branch forever once the sender is
        // dropped — this flag disables it for good the first time that
        // happens (contracts-m6.md §D1: "종료 사유 아님").
        let mut ctrl_open = true;

        loop {
            tokio::select! {
                ev = conn.recv() => {
                    match ev {
                        Some(BusEvent::Envelope(env)) => {
                            // D2's "재귀 방지 가드": a *received* presence
                            // envelope never triggers its own presence
                            // signal — this is the only place this crate
                            // emits presence, and it only fires here, on
                            // the receive path, so an agent's own outgoing
                            // sends (including these presence envelopes
                            // themselves) never loop back through this
                            // check on the sending side.
                            let is_presence =
                                matches!(env.kind, MessageKind::PresenceRead | MessageKind::PresenceTyping);
                            let replies = if is_presence {
                                behavior.on_envelope(env).await
                            } else {
                                emit_presence_start(&conn, &env).await;
                                let replies = behavior.on_envelope(env.clone()).await;
                                emit_presence_end(&conn, &env).await;
                                replies
                            };
                            for reply in replies {
                                conn.send(reply).await?;
                            }
                            if behavior.is_done() {
                                return Ok(());
                            }
                        }
                        Some(BusEvent::Error { code, message }) => {
                            if FATAL_ERROR_CODES.contains(&code.as_str()) {
                                return Err(RunnerError::Fatal { code, message });
                            }
                            tracing::warn!(code = %code, message = %message, "bus reported a non-fatal error frame");
                        }
                        None => return Err(RunnerError::ConnectionClosed),
                    }
                }
                // `select!`'s `if` guard only skips *polling* this branch —
                // the async expression is still constructed even when
                // disabled (tokio::select! docs), so the `None` arm below
                // must be a real (never-polled-to-completion) future rather
                // than a `.unwrap()` that would panic on construction.
                _ = async {
                    match ticker.as_mut() {
                        Some(interval) => interval.tick().await,
                        None => std::future::pending().await,
                    }
                }, if ticker.is_some() => {
                    let now_ms = now_unix_ms();
                    for reply in behavior.on_tick(now_ms).await {
                        conn.send(reply).await?;
                    }
                    if behavior.is_done() {
                        return Ok(());
                    }
                }
                ctrl = ctrl_rx.recv(), if ctrl_open => {
                    match ctrl {
                        Some(c) => {
                            for reply in behavior.on_control(c).await {
                                conn.send(reply).await?;
                            }
                            if behavior.is_done() {
                                return Ok(());
                            }
                        }
                        None => {
                            ctrl_open = false;
                        }
                    }
                }
            }
        }
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crew_harness::{HandoffSnapshot, HarnessError};
    use crew_proto::{ClientFrame, Envelope, MessageKind, ServerFrame};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::tungstenite::Message;

    struct CountingBehavior {
        remaining: u32,
    }

    #[async_trait]
    impl RoleBehavior for CountingBehavior {
        async fn on_envelope(&mut self, _env: Envelope) -> Vec<Envelope> {
            self.remaining = self.remaining.saturating_sub(1);
            vec![]
        }

        fn is_done(&self) -> bool {
            self.remaining == 0
        }
    }

    fn wire_envelope(id: &str) -> Envelope {
        let mut env = Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:pm".to_string(),
            vec!["agent:designer".to_string()],
            MessageKind::Question,
            None,
            "req_1".to_string(),
            json!({}),
            vec![],
            true,
            900_000,
        );
        env.id = id.to_string();
        env
    }

    async fn start_fake_bus() -> (String, TcpListener) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (format!("ws://{addr}"), listener)
    }

    async fn send_welcome(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) {
        let welcome = ServerFrame::Welcome {
            agent_id: "agent:designer".to_string(),
        };
        ws.send(Message::Text(serde_json::to_string(&welcome).unwrap().into()))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stops_cleanly_once_is_done_after_two_envelopes() {
        let (url, listener) = start_fake_bus().await;

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            for id in ["msg_1", "msg_2"] {
                let frame = ServerFrame::Envelope(wire_envelope(id));
                let text = serde_json::to_string(&frame).unwrap();
                ws.send(Message::Text(text.into())).await.unwrap();
                // Drain the auto-Receipt the runner's BusConn sends back.
                let _ = ws.next().await;
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let result = AgentRunner::run(conn, CountingBehavior { remaining: 2 }).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn nonfatal_error_frame_is_logged_and_loop_continues() {
        let (url, listener) = start_fake_bus().await;

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let error = ServerFrame::Error {
                code: "unknown_recipient".to_string(),
                message: "no such agent".to_string(),
            };
            ws.send(Message::Text(serde_json::to_string(&error).unwrap().into()))
                .await
                .unwrap();

            for id in ["msg_1", "msg_2"] {
                let frame = ServerFrame::Envelope(wire_envelope(id));
                let text = serde_json::to_string(&frame).unwrap();
                ws.send(Message::Text(text.into())).await.unwrap();
                let _ = ws.next().await;
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let result = AgentRunner::run(conn, CountingBehavior { remaining: 2 }).await;

        assert!(result.is_ok(), "non-fatal error frame must not abort the runner");
    }

    #[tokio::test]
    async fn fatal_error_code_aborts_the_runner() {
        let (url, listener) = start_fake_bus().await;

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let error = ServerFrame::Error {
                code: "loop_blocked".to_string(),
                message: "cycle detected".to_string(),
            };
            ws.send(Message::Text(serde_json::to_string(&error).unwrap().into()))
                .await
                .unwrap();
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let result = AgentRunner::run(conn, CountingBehavior { remaining: 2 }).await;

        match result {
            Err(RunnerError::Fatal { code, .. }) => assert_eq!(code, "loop_blocked"),
            other => panic!("expected Fatal error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connection_closed_before_done_is_an_error() {
        let (url, listener) = start_fake_bus().await;

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;
            // Close immediately with no further frames.
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let result = AgentRunner::run(conn, CountingBehavior { remaining: 2 }).await;

        assert!(matches!(result, Err(RunnerError::ConnectionClosed)));
    }

    /// `tick_interval = Some(10ms)`: first `on_tick` returns one envelope
    /// (normal — asserts the runner actually sends what `on_tick` returns),
    /// second `on_tick` returns an empty vec and flips `is_done` (boundary —
    /// an empty tick reply must not produce a send). `tick_interval = None`
    /// behaviors (every other test in this file) are the regression case:
    /// their `select!` tick branch is permanently disabled by its `if`
    /// guard, so this loop is byte-identical to the original recv-only one
    /// for them.
    struct TickingBehavior {
        ticks: Arc<AtomicU32>,
    }

    #[async_trait]
    impl RoleBehavior for TickingBehavior {
        async fn on_envelope(&mut self, _env: Envelope) -> Vec<Envelope> {
            vec![]
        }

        fn is_done(&self) -> bool {
            self.ticks.load(Ordering::SeqCst) >= 2
        }

        fn tick_interval(&self) -> Option<Duration> {
            Some(Duration::from_millis(10))
        }

        async fn on_tick(&mut self, _now_ms: u64) -> Vec<Envelope> {
            let n = self.ticks.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                vec![wire_envelope("tick-msg")]
            } else {
                vec![]
            }
        }
    }

    // `tokio::time::pause`/`advance` proved flaky here under the full
    // workspace suite's parallel load: `BusConn::connect`'s own internal
    // handshake timeout (`bus.rs::HANDSHAKE_TIMEOUT`) shares the paused
    // virtual clock, and real accept()/handshake IO racing against that
    // clock's auto-advance intermittently timed out the connect itself —
    // reproduced by a full `cargo test --workspace` run, not by running
    // this test alone. A short *real* interval plus a generously bounded
    // real-time `timeout` avoids that interaction entirely; the assertions
    // below are on states reached, not on elapsed durations, so this loses
    // no determinism that matters for this test.
    #[tokio::test]
    async fn tick_interval_some_drives_on_tick_and_sends_its_envelopes() {
        let (url, listener) = start_fake_bus().await;
        let (recv_tx, mut recv_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            while let Some(Ok(Message::Text(text))) = ws.next().await {
                if let Ok(ClientFrame::Envelope(env)) = serde_json::from_str::<ClientFrame>(&text) {
                    let receipt = ServerFrame::Receipt { id: env.id.clone() };
                    ws.send(Message::Text(serde_json::to_string(&receipt).unwrap().into()))
                        .await
                        .unwrap();
                    let _ = recv_tx.send(env);
                }
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let ticks = Arc::new(AtomicU32::new(0));
        let handle = tokio::spawn(AgentRunner::run(conn, TickingBehavior { ticks: ticks.clone() }));

        let budget = Duration::from_secs(5);

        // Normal: the first on_tick's one envelope must actually be sent.
        let first = tokio::time::timeout(budget, recv_rx.recv())
            .await
            .expect("first on_tick must fire within the budget")
            .expect("first on_tick's envelope must be sent");
        assert_eq!(first.id, "tick-msg");
        assert_eq!(ticks.load(Ordering::SeqCst), 1);

        // Boundary: the second on_tick returns an empty vec, so no further
        // envelope arrives, and is_done() flipping true ends the runner.
        let result = tokio::time::timeout(budget, handle)
            .await
            .expect("runner must finish once is_done() flips true")
            .expect("runner task must not panic");
        assert!(result.is_ok(), "runner should end cleanly: {result:?}");
        assert_eq!(ticks.load(Ordering::SeqCst), 2);
        assert!(
            recv_rx.try_recv().is_err(),
            "an empty on_tick reply must not produce a second send"
        );
    }

    /// contracts-m6.md §D1: `run_with_control`'s ctrl branch must actually
    /// invoke `on_control` and deliver its reply envelopes, while the
    /// envelope loop keeps running to completion around it (D9 case ①,
    /// runner level — `CountingBehavior` doesn't override `on_control`, so
    /// this exercises the default no-op-ack path end to end through the
    /// select loop, not just the trait method in isolation).
    #[tokio::test]
    async fn run_with_control_delivers_ctrl_swap_ack_and_finishes_normally() {
        let (url, listener) = start_fake_bus().await;

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            for id in ["msg_1", "msg_2"] {
                let frame = ServerFrame::Envelope(wire_envelope(id));
                let text = serde_json::to_string(&frame).unwrap();
                ws.send(Message::Text(text.into())).await.unwrap();
                let _ = ws.next().await;
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel(4);
        let handle = tokio::spawn(AgentRunner::run_with_control(
            conn,
            CountingBehavior { remaining: 2 },
            ctrl_rx,
        ));

        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        let ctrl = AgentControl::Swap {
            harness: Arc::new(crew_harness::claude::ClaudeCodeHarness::with_binary(
                "/bin/false",
            )),
            harness_id: "harness-new".to_string(),
            injected_context: "ctx".to_string(),
            ack: ack_tx,
        };
        ctrl_tx.send(ctrl).await.expect("ctrl_rx must still be open");

        let budget = Duration::from_secs(5);
        let snapshot = tokio::time::timeout(budget, ack_rx)
            .await
            .expect("ack must arrive within the budget")
            .expect("ack sender must not be dropped without sending")
            .expect("default on_control must ack Ok");
        assert_eq!(snapshot.notes, "no live session");

        let result = tokio::time::timeout(budget, handle)
            .await
            .expect("runner must finish once is_done() flips true")
            .expect("runner task must not panic");
        assert!(result.is_ok(), "runner should end cleanly around the ctrl message: {result:?}");
    }

    /// contracts-m6.md §D1 D3: a closed `ctrl_rx` (sender dropped) is not a
    /// termination reason — the runner must keep servicing the envelope
    /// loop to completion (D9 case ③, runner level; `AgentRunner::run`'s
    /// own regression tests above cover this indirectly via its internal
    /// channel, but this exercises `run_with_control` directly with an
    /// externally supplied, externally closed channel).
    #[tokio::test]
    async fn run_with_control_survives_a_closed_ctrl_channel() {
        let (url, listener) = start_fake_bus().await;

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            for id in ["msg_1", "msg_2"] {
                let frame = ServerFrame::Envelope(wire_envelope(id));
                let text = serde_json::to_string(&frame).unwrap();
                ws.send(Message::Text(text.into())).await.unwrap();
                let _ = ws.next().await;
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<AgentControl>(4);
        drop(ctrl_tx);

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            AgentRunner::run_with_control(conn, CountingBehavior { remaining: 2 }, ctrl_rx),
        )
        .await
        .expect("runner must not hang or busy-loop on a closed ctrl channel");

        assert!(result.is_ok(), "runner should finish normally: {result:?}");
    }

    /// t2-be-presence R1/R2 (normal, deterministic): receiving a
    /// non-presence envelope emits presence.read, then
    /// presence.typing(active:true), then — once `on_envelope` returns —
    /// presence.typing(active:false), strictly in that order on the wire,
    /// before any of `CountingBehavior`'s own replies (it has none here).
    #[tokio::test]
    async fn presence_read_then_typing_true_then_typing_false_in_order() {
        let (url, listener) = start_fake_bus().await;
        let (sent_tx, mut sent_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let frame = ServerFrame::Envelope(wire_envelope("msg_1"));
            ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                .await
                .unwrap();
            let _ = ws.next().await; // drain the auto-Receipt for msg_1

            // Forward every envelope the agent sends back for inspection —
            // the agent should send exactly 3 (no real reply, CountingBehavior
            // returns none).
            //
            // #18 (confirmed 2026-09-22): this loop used to write a Receipt
            // for every envelope BEFORE forwarding it. The 3 presence
            // envelopes go out via fire-and-forget `send_best_effort`, and
            // `AgentRunner::run` returns on `is_done()` right after the last
            // one, dropping `BusConn` (`Drop` = abrupt `reader_task.abort()`,
            // no Close handshake). When that teardown won the race, the
            // Receipt write hit EPIPE, its `.unwrap()` panicked this task
            // (`runner.rs:615:26` BrokenPipe) before `sent_tx.send`, and the
            // test's `recv()` unwrapped a closed channel (`:627:82`/`:628:81`
            // None). Fix: forward first, and — like real crew-bus
            // (`crew-bus/src/routing.rs`, Receipts only for `requires_ack`)
            // — reply only to `requires_ack` envelopes; presence is
            // `requires_ack=false`, so no write races the teardown at all.
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                if let Ok(ClientFrame::Envelope(env)) = serde_json::from_str::<ClientFrame>(&text) {
                    let requires_ack = env.requires_ack;
                    let id = env.id.clone();
                    let _ = sent_tx.send(env);
                    if requires_ack {
                        let receipt = ServerFrame::Receipt { id };
                        ws.send(Message::Text(serde_json::to_string(&receipt).unwrap().into()))
                            .await
                            .unwrap();
                    }
                }
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let result = AgentRunner::run(conn, CountingBehavior { remaining: 1 }).await;
        assert!(result.is_ok(), "runner should end cleanly: {result:?}");

        let budget = Duration::from_secs(5);
        let first = tokio::time::timeout(budget, sent_rx.recv()).await.unwrap().unwrap();
        let second = tokio::time::timeout(budget, sent_rx.recv()).await.unwrap().unwrap();
        let third = tokio::time::timeout(budget, sent_rx.recv()).await.unwrap().unwrap();

        assert_eq!(first.kind, MessageKind::PresenceRead);
        assert_eq!(second.kind, MessageKind::PresenceTyping);
        assert_eq!(third.kind, MessageKind::PresenceTyping);

        let read_body = crew_proto::presence_read_from_body(&first.body).expect("read body must parse");
        assert_eq!(read_body.target_msg_id, "msg_1");
        assert_eq!(read_body.agent_id, "agent:designer");

        let typing_on = crew_proto::presence_typing_from_body(&second.body).expect("typing body must parse");
        assert!(typing_on.active, "typing must be true first");
        let typing_off = crew_proto::presence_typing_from_body(&third.body).expect("typing body must parse");
        assert!(!typing_off.active, "typing must be false last");

        // Nothing more must arrive — either the channel times out waiting
        // (still open) or `sent_tx` was already dropped because `is_done()`
        // ended the runner and closed the connection right after the 3rd
        // send; both mean "no 4th envelope", so only an actual `Some` fails.
        if let Ok(Some(extra)) = tokio::time::timeout(Duration::from_millis(200), sent_rx.recv()).await {
            panic!("exactly 3 presence envelopes expected, got a 4th: {:?}", extra.kind);
        }
    }

    /// #18 regression (worst-case ordering, forced): the fake bus handles
    /// the 3rd presence envelope only after the client has already dropped
    /// its `BusConn` (`gone` fires after `drop(conn)`), which is the
    /// interleaving that used to lose the envelope to a panicking Receipt
    /// write. All 3 must still be forwarded, in order.
    #[tokio::test]
    async fn presence_third_envelope_is_forwarded_even_when_the_receipt_reply_write_races_client_teardown() {
        let (url, listener) = start_fake_bus().await;
        let (sent_tx, mut sent_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();
        let (gone_tx, gone_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let mut gone_rx = Some(gone_rx);
            let mut received = 0u32;
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                if let Ok(ClientFrame::Envelope(env)) = serde_json::from_str::<ClientFrame>(&text) {
                    received += 1;
                    let requires_ack = env.requires_ack;
                    let id = env.id.clone();
                    let _ = sent_tx.send(env);
                    if received == 3 {
                        if let Some(gone_rx) = gone_rx.take() {
                            let _ = gone_rx.await;
                        }
                    }
                    if requires_ack {
                        let receipt = ServerFrame::Receipt { id };
                        ws.send(Message::Text(serde_json::to_string(&receipt).unwrap().into()))
                            .await
                            .unwrap();
                    }
                }
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let env = wire_envelope("msg_1");
        emit_presence_start(&conn, &env).await;
        emit_presence_end(&conn, &env).await;
        drop(conn);
        let _ = gone_tx.send(());

        let budget = Duration::from_secs(5);
        let first = tokio::time::timeout(budget, sent_rx.recv()).await.unwrap().unwrap();
        let second = tokio::time::timeout(budget, sent_rx.recv()).await.unwrap().unwrap();
        let third = tokio::time::timeout(budget, sent_rx.recv()).await.unwrap().unwrap();

        assert_eq!(first.kind, MessageKind::PresenceRead);
        assert_eq!(second.kind, MessageKind::PresenceTyping);
        assert_eq!(third.kind, MessageKind::PresenceTyping);

        let read_body = crew_proto::presence_read_from_body(&first.body).expect("read body must parse");
        assert_eq!(read_body.target_msg_id, "msg_1");
        assert_eq!(read_body.agent_id, "agent:designer");

        let typing_on = crew_proto::presence_typing_from_body(&second.body).expect("typing body must parse");
        assert!(typing_on.active, "typing must be true first");
        let typing_off = crew_proto::presence_typing_from_body(&third.body).expect("typing body must parse");
        assert!(!typing_off.active, "typing must be false last");

        if let Ok(Some(extra)) = tokio::time::timeout(Duration::from_millis(200), sent_rx.recv()).await {
            panic!("exactly 3 presence envelopes expected, got a 4th: {:?}", extra.kind);
        }
    }

    /// t2-be-presence D2 recursion guard (boundary): receiving a
    /// presence-kind envelope must not itself trigger nested presence —
    /// zero envelopes are sent back for it.
    #[tokio::test]
    async fn receiving_a_presence_kind_envelope_emits_no_nested_presence() {
        let (url, listener) = start_fake_bus().await;
        let (sent_tx, mut sent_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let mut env = wire_envelope("msg_presence_1");
            env.kind = MessageKind::PresenceRead;
            env.body = crew_proto::presence_read_body(&crew_proto::PresenceReadBody {
                agent_id: "agent:designer".to_string(),
                target_msg_id: "msg_0".to_string(),
            });
            let frame = ServerFrame::Envelope(env);
            ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                .await
                .unwrap();
            let _ = ws.next().await; // drain the auto-Receipt

            while let Some(Ok(Message::Text(text))) = ws.next().await {
                if let Ok(ClientFrame::Envelope(env)) = serde_json::from_str::<ClientFrame>(&text) {
                    let _ = sent_tx.send(env);
                }
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            AgentRunner::run(conn, CountingBehavior { remaining: 1 }),
        )
        .await
        .expect("runner must not hang");
        assert!(result.is_ok(), "runner should end cleanly: {result:?}");

        // Same reasoning as the order test above: a closed channel (runner
        // ended, connection dropped) and a timeout both mean "nothing was
        // sent" — only an actual `Some` is a failure.
        if let Ok(Some(unexpected)) = tokio::time::timeout(Duration::from_millis(200), sent_rx.recv()).await {
            panic!(
                "no envelope must be sent back for a received presence-kind envelope, got: {:?}",
                unexpected.kind
            );
        }
    }

    /// t2-be-presence D2 step 3 (normal, richer): the same bracket applies
    /// uniformly to a real `RoleBehavior` impl (`ScriptedCrewMember`), not
    /// just a synthetic test double — its own TaskAck+TaskResult replies
    /// are sent after typing(false), all 5 in one deterministic order.
    #[tokio::test]
    async fn scripted_crew_member_gets_presence_around_its_real_replies() {
        use crate::crew_member::ScriptedCrewMember;
        use crew_proto::{ArtifactContract, DodCheck, ReqId, Role, TaskSpec};

        let (url, listener) = start_fake_bus().await;
        let (sent_tx, mut sent_rx) = tokio::sync::mpsc::unbounded_channel::<Envelope>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let task = TaskSpec {
                id: "t1".to_string(),
                role: Role::Developer,
                title: "title".to_string(),
                brief: "brief".to_string(),
                dod: vec![DodCheck::ReqCover {
                    ids: vec![ReqId::new("REQ-1").unwrap()],
                }],
                deps: vec![],
                artifacts_expected: vec![ArtifactContract {
                    name: "index.html".to_string(),
                    kind: "code".to_string(),
                    req_ids: vec![ReqId::new("REQ-1").unwrap()],
                }],
            };
            let mut assign = wire_envelope("msg_assign");
            assign.kind = MessageKind::TaskAssign;
            assign.to = vec!["agent:developer".to_string()];
            assign.body = json!({ "task": task });
            let frame = ServerFrame::Envelope(assign);
            ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                .await
                .unwrap();
            let _ = ws.next().await; // drain the auto-Receipt

            // Ack+forward exactly the 5 expected sends, then stop reading —
            // the connection drop that follows ends the runner (its
            // `is_done()` is always false) deterministically.
            for _ in 0..5u32 {
                let Some(Ok(Message::Text(text))) = ws.next().await else {
                    break;
                };
                if let Ok(ClientFrame::Envelope(env)) = serde_json::from_str::<ClientFrame>(&text) {
                    let receipt = ServerFrame::Receipt { id: env.id.clone() };
                    ws.send(Message::Text(serde_json::to_string(&receipt).unwrap().into()))
                        .await
                        .unwrap();
                    let _ = sent_tx.send(env);
                }
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:developer").await.unwrap();
        let behavior = ScriptedCrewMember::new("agent:developer", Role::Developer, vec![]);
        // `ScriptedCrewMember::is_done()` is always false, so the runner
        // only stops once the fake bus task above finishes its 5-send loop
        // and drops the socket — that closes the connection and ends the
        // runner with `ConnectionClosed`, well inside this budget.
        let outcome = tokio::time::timeout(Duration::from_secs(5), AgentRunner::run(conn, behavior))
            .await
            .expect("runner must not hang");
        assert!(matches!(outcome, Err(RunnerError::ConnectionClosed)));

        let budget = Duration::from_secs(5);
        let mut got = Vec::new();
        for _ in 0..5u32 {
            got.push(tokio::time::timeout(budget, sent_rx.recv()).await.unwrap().unwrap());
        }

        assert_eq!(
            got.iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![
                MessageKind::PresenceRead,
                MessageKind::PresenceTyping,
                MessageKind::PresenceTyping,
                MessageKind::TaskAck,
                MessageKind::TaskResult,
            ]
        );
        let typing_on = crew_proto::presence_typing_from_body(&got[1].body).unwrap();
        assert!(typing_on.active);
        let typing_off = crew_proto::presence_typing_from_body(&got[2].body).unwrap();
        assert!(!typing_off.active);
    }
    // ---- #23 piece B: what happens to a control message its worker never
    // serves. The five tests below are one per exit mechanism class of
    // `run_with_control` (abort, failed send, runner error, panic, and a send
    // arriving after the runner already returned); every `return Ok(())` site
    // inside the loop is a single class with the error returns, because they
    // all leave through the same drop of the control receiver.

    type AckRx = oneshot::Receiver<Result<HandoffSnapshot, HarnessError>>;

    fn swap_ctrl() -> (AgentControl, AckRx) {
        let (ack, ack_rx) = oneshot::channel();
        let ctrl = AgentControl::Swap {
            harness: Arc::new(crew_harness::claude::ClaudeCodeHarness::with_binary(
                "/bin/false",
            )),
            harness_id: "harness-new".to_string(),
            injected_context: "ctx".to_string(),
            ack,
        };
        (ctrl, ack_rx)
    }

    /// Welcome, then hold the socket open and never send anything — in
    /// particular no `Receipt`, so a `BusConn::send` exhausts its retries.
    async fn silent_bus(listener: TcpListener) {
        let (stream, _peer) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        send_welcome(&mut ws).await;
        while let Some(Ok(_)) = ws.next().await {}
    }

    /// Welcome, deliver `msg_1`, then hold the socket open.
    async fn one_envelope_bus(listener: TcpListener) {
        let (stream, _peer) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        send_welcome(&mut ws).await;
        let frame = ServerFrame::Envelope(wire_envelope("msg_1"));
        ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
            .await
            .unwrap();
        while let Some(Ok(_)) = ws.next().await {}
    }

    /// Signals that it reached `on_start`, then parks there forever: the
    /// runner never reaches its `select!`, so an abort can only hit it while
    /// a control message sits unread.
    struct ParkedOnStart {
        entered: Option<oneshot::Sender<()>>,
    }

    #[async_trait]
    impl RoleBehavior for ParkedOnStart {
        async fn on_start(&mut self) -> Vec<Envelope> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            std::future::pending::<Vec<Envelope>>().await
        }

        async fn on_envelope(&mut self, _env: Envelope) -> Vec<Envelope> {
            vec![]
        }

        fn is_done(&self) -> bool {
            false
        }
    }

    /// Parks in `on_start` until released, then returns one envelope — so the
    /// test decides the exact moment the runner starts its first `conn.send`.
    struct GatedStart {
        entered: Option<oneshot::Sender<()>>,
        release: Option<oneshot::Receiver<()>>,
    }

    #[async_trait]
    impl RoleBehavior for GatedStart {
        async fn on_start(&mut self) -> Vec<Envelope> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
            }
            if let Some(release) = self.release.take() {
                let _ = release.await;
            }
            vec![wire_envelope("start_1")]
        }

        async fn on_envelope(&mut self, _env: Envelope) -> Vec<Envelope> {
            vec![]
        }

        fn is_done(&self) -> bool {
            false
        }
    }

    /// Parks inside its first `on_envelope` until released — the window in
    /// which the bus can queue more events and the test can queue controls.
    /// With `panic_after_release` it then panics, which is the runner task's
    /// panic exit.
    struct GatedEnvelope {
        entered: Option<oneshot::Sender<()>>,
        release: Option<oneshot::Receiver<()>>,
        panic_after_release: bool,
    }

    #[async_trait]
    impl RoleBehavior for GatedEnvelope {
        async fn on_envelope(&mut self, _env: Envelope) -> Vec<Envelope> {
            if self.entered.is_some() {
                if let Some(entered) = self.entered.take() {
                    let _ = entered.send(());
                }
                if let Some(release) = self.release.take() {
                    let _ = release.await;
                }
                if self.panic_after_release {
                    panic!("t-panic test behaviour");
                }
            }
            vec![]
        }

        fn is_done(&self) -> bool {
            false
        }
    }

    /// Error/boundary case, and the arm the #23 piece B probe confirmed
    /// (`outcome=aborted-future-dropped`, `pending_at_drop=1`): crew-run
    /// aborts a worker at the sprint/run boundary while `swap_harness` is
    /// sending on a control Sender it cloned just before. `abort()` only
    /// *requests* cancellation, so the send still lands in the receiver's
    /// buffer — and the queued swap must be answered rather than dropped.
    #[tokio::test]
    async fn an_aborted_runner_answers_a_swap_queued_after_the_abort_request() {
        let (url, listener) = start_fake_bus().await;
        tokio::spawn(silent_bus(listener));

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel(4);
        let (entered_tx, entered_rx) = oneshot::channel();
        let handle = tokio::spawn(AgentRunner::run_with_control(
            conn,
            ParkedOnStart {
                entered: Some(entered_tx),
            },
            ctrl_rx,
        ));

        entered_rx.await.expect("the behaviour must reach on_start");
        handle.abort();
        let (ctrl, ack_rx) = swap_ctrl();
        ctrl_tx
            .try_send(ctrl)
            .expect("the receiver is alive: abort was only requested");

        assert!(handle.await.unwrap_err().is_cancelled());

        let snapshot = tokio::time::timeout(Duration::from_secs(5), ack_rx)
            .await
            .expect("ack within budget")
            .expect("a queued swap must be answered, not dropped")
            .expect("an aborted worker answers Ok");
        assert_eq!(snapshot.notes, UNSERVED_ON_EXIT_NOTES);
        assert_eq!(snapshot.harness, "harness-new");
        assert!(
            snapshot.session_id.is_empty(),
            "an unserved swap has no session: {snapshot:?}"
        );
    }

    /// Error case: the runner leaves through `conn.send(..)?` (the bus never
    /// receipts, so `BusConn::send` exhausts its 3 attempts) while a swap is
    /// already queued. The worker DIED, so the answer must be the error
    /// contract — `HarnessError::Unavailable` naming the failure — not an
    /// `Ok` snapshot that would hide the death from crew-run.
    #[tokio::test]
    async fn a_runner_ending_on_a_failed_send_answers_a_swap_queued_while_it_was_sending() {
        let (url, listener) = start_fake_bus().await;
        tokio::spawn(silent_bus(listener));

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel(4);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let handle = tokio::spawn(AgentRunner::run_with_control(
            conn,
            GatedStart {
                entered: Some(entered_tx),
                release: Some(release_rx),
            },
            ctrl_rx,
        ));

        entered_rx.await.expect("the behaviour must reach on_start");
        let (ctrl, ack_rx) = swap_ctrl();
        ctrl_tx
            .try_send(ctrl)
            .expect("the receiver is alive while on_start is parked");
        release_tx
            .send(())
            .expect("the behaviour awaits the release");

        // ~6-7s of real time: 3 send attempts x 2s receipt timeout + backoff.
        let result = tokio::time::timeout(Duration::from_secs(30), handle)
            .await
            .expect("runner ends within budget")
            .expect("no panic");
        assert!(
            matches!(result, Err(RunnerError::Send(BusError::SendFailed { .. }))),
            "{result:?}"
        );

        match tokio::time::timeout(Duration::from_secs(5), ack_rx)
            .await
            .expect("ack within budget")
            .expect("a queued swap must be answered, not dropped")
        {
            Err(HarnessError::Unavailable(msg)) => {
                assert!(msg.contains("SendFailed"), "{msg}")
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// Error + boundary case at the channel's capacity: the runner leaves
    /// through `RunnerError::Fatal` with a full control buffer, so both
    /// answers appear in one run — the swaps the loop served before the fatal
    /// frame won the `select!` (`Ok`, "no live session") and every one left
    /// behind must still be answered (`Unavailable`, naming the fatal code).
    ///
    /// Probabilistic by construction: tokio::select! starts at a random
    /// branch; with the tick branch disabled ctrl wins 2 of 3 starts, so
    /// unpatched code leaves >=1 swap unanswered with probability
    /// 1-(2/3)^32 ~ 1-2.3e-6, and the patched `unserved >= 1` assertion has
    /// the same 2.3e-6 flake.
    #[tokio::test]
    async fn a_runner_ending_on_a_fatal_frame_answers_every_swap_it_left_queued() {
        let (url, listener) = start_fake_bus().await;
        let (go_tx, go_rx) = oneshot::channel::<()>();
        let (buffered_tx, buffered_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let frame = ServerFrame::Envelope(wire_envelope("msg_1"));
            ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                .await
                .unwrap();

            go_rx.await.expect("the test releases the fatal frame");

            let error = ServerFrame::Error {
                code: "loop_blocked".to_string(),
                message: "cycle detected".to_string(),
            };
            ws.send(Message::Text(serde_json::to_string(&error).unwrap().into()))
                .await
                .unwrap();
            let frame = ServerFrame::Envelope(wire_envelope("msg_2"));
            ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
                .await
                .unwrap();

            // The reader task receipts every envelope it buffers, so msg_2's
            // Receipt is the signal that both frames are in the events queue.
            let mut buffered_tx = Some(buffered_tx);
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(text) = msg {
                    if let Ok(ClientFrame::Receipt { id }) =
                        serde_json::from_str::<ClientFrame>(text.as_str())
                    {
                        if id == "msg_2" {
                            if let Some(tx) = buffered_tx.take() {
                                let _ = tx.send(());
                            }
                        }
                    }
                }
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel(32);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let handle = tokio::spawn(AgentRunner::run_with_control(
            conn,
            GatedEnvelope {
                entered: Some(entered_tx),
                release: Some(release_rx),
                panic_after_release: false,
            },
            ctrl_rx,
        ));

        entered_rx
            .await
            .expect("the behaviour must reach on_envelope");
        go_tx.send(()).expect("the bus task awaits the go signal");
        buffered_rx
            .await
            .expect("the fatal frame and msg_2 must be buffered before the controls");

        let mut acks = Vec::new();
        for _ in 0..32 {
            let (ctrl, ack_rx) = swap_ctrl();
            ctrl_tx
                .try_send(ctrl)
                .expect("the buffer holds 32 controls");
            acks.push(ack_rx);
        }
        release_tx
            .send(())
            .expect("the behaviour awaits the release");

        let result = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("runner ends within budget")
            .expect("no panic");
        assert!(
            matches!(&result, Err(RunnerError::Fatal { code, .. }) if code == "loop_blocked"),
            "{result:?}"
        );

        let mut served = 0usize;
        let mut unserved = 0usize;
        for ack_rx in acks {
            let answer = tokio::time::timeout(Duration::from_secs(5), ack_rx)
                .await
                .expect("ack within budget")
                .expect("every queued swap must be answered");
            match answer {
                Ok(snapshot) if snapshot.notes == "no live session" => served += 1,
                Err(HarnessError::Unavailable(msg)) if msg.contains("loop_blocked") => {
                    unserved += 1
                }
                other => panic!("unexpected answer: {other:?}"),
            }
        }
        assert_eq!(
            served + unserved,
            32,
            "served {served}, unserved {unserved}"
        );
        assert!(
            unserved >= 1,
            "the fatal frame must have cut the drain short: served {served}"
        );
    }

    /// Error case: the worker task panics inside `on_envelope`. The control
    /// receiver is dropped while the panic unwinds, so the queued swap must
    /// still be answered — with the error contract, since the worker died.
    #[tokio::test]
    async fn a_panicking_runner_answers_a_queued_swap_with_an_error() {
        let (url, listener) = start_fake_bus().await;
        tokio::spawn(one_envelope_bus(listener));

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel(4);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let handle = tokio::spawn(AgentRunner::run_with_control(
            conn,
            GatedEnvelope {
                entered: Some(entered_tx),
                release: Some(release_rx),
                panic_after_release: true,
            },
            ctrl_rx,
        ));

        entered_rx
            .await
            .expect("the behaviour must reach on_envelope");
        let (ctrl, ack_rx) = swap_ctrl();
        ctrl_tx
            .try_send(ctrl)
            .expect("the receiver is alive while on_envelope is parked");
        release_tx
            .send(())
            .expect("the behaviour awaits the release");

        assert!(handle.await.unwrap_err().is_panic());

        match tokio::time::timeout(Duration::from_secs(5), ack_rx)
            .await
            .expect("ack within budget")
            .expect("a queued swap must be answered, not dropped")
        {
            Err(HarnessError::Unavailable(msg)) => {
                assert!(msg.contains("panicked"), "{msg}")
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// Boundary case on the other side of the exit: the control arrives
    /// *after* the runner has already returned. crew-run's `swap_harness`
    /// holds a cloned Sender, so the send must still be accepted and
    /// answered — a closed channel there is the second error arm
    /// ("worker control channel closed") of issue #23.
    ///
    /// Determinism relies on current_thread FIFO scheduling: the drain task
    /// spawned in the runner's Drop is queued before this task's JoinHandle
    /// wake, and yield_now defers this task's waker behind it, so the drain
    /// has been polled once before the send (theoretical exception only if
    /// event_interval or more other ready tasks were queued ahead).
    #[tokio::test]
    async fn a_swap_sent_after_the_runner_returned_is_still_answered() {
        let (url, listener) = start_fake_bus().await;

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_welcome(&mut ws).await;

            let error = ServerFrame::Error {
                code: "loop_blocked".to_string(),
                message: "cycle detected".to_string(),
            };
            ws.send(Message::Text(serde_json::to_string(&error).unwrap().into()))
                .await
                .unwrap();
            while let Some(Ok(_)) = ws.next().await {}
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel(4);
        let handle = tokio::spawn(AgentRunner::run_with_control(
            conn,
            CountingBehavior { remaining: 2 },
            ctrl_rx,
        ));

        let result = handle.await.expect("no panic");
        assert!(
            matches!(&result, Err(RunnerError::Fatal { code, .. }) if code == "loop_blocked"),
            "{result:?}"
        );
        tokio::task::yield_now().await;

        tokio::time::timeout(Duration::from_secs(5), async {
            let (ctrl, ack_rx) = swap_ctrl();
            ctrl_tx
                .send(ctrl)
                .await
                .expect("a Sender still exists, so the ended worker's channel must still accept");
            match ack_rx.await.expect("the late swap must be answered") {
                Err(HarnessError::Unavailable(msg)) => {
                    assert!(msg.contains("loop_blocked"), "{msg}")
                }
                other => panic!("expected Unavailable, got {other:?}"),
            }
        })
        .await
        .expect("the late swap must be answered within the budget");
    }
}
