use crate::bus::{BusConn, BusError, BusEvent};
use crate::role::RoleBehavior;

/// Bus-level error codes that abort the runner instead of being logged and
/// skipped (plan A5) — in the M2 scenario these mean the conversation
/// itself cannot continue, so surfacing them as a test/caller failure is
/// correct rather than silently retrying.
const FATAL_ERROR_CODES: &[&str] = &["loop_blocked", "delivery_failed"];

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
pub struct AgentRunner;

impl AgentRunner {
    pub async fn run<B>(mut conn: BusConn, mut behavior: B) -> Result<(), RunnerError>
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

        loop {
            tokio::select! {
                ev = conn.recv() => {
                    match ev {
                        Some(BusEvent::Envelope(env)) => {
                            for reply in behavior.on_envelope(env).await {
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
    use crew_proto::{ClientFrame, Envelope, MessageKind, ServerFrame};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::TcpListener;
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
}
