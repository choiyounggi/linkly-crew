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
/// whatever `on_envelope` returns, until `is_done()`.
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

        loop {
            match conn.recv().await {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crew_proto::{Envelope, MessageKind, ServerFrame};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
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
}
