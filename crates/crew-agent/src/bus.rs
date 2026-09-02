use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crew_proto::{ClientFrame, Envelope, ServerFrame};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(2);
const SEND_ATTEMPTS: u32 = 3;
const BACKOFF_BASE_MS: u64 = 100;
const BACKOFF_CAP_MS: u64 = 1_000;
const EVENTS_CHANNEL_CAPACITY: usize = 64;

/// What `BusConn::recv` delivers: a freshly-received (deduped) `Envelope`,
/// or a bus-level `Error` frame the caller (`AgentRunner`, plan A5) decides
/// how to react to.
#[derive(Debug, Clone)]
pub enum BusEvent {
    Envelope(Envelope),
    Error { code: String, message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("invalid connect request: {0}")]
    InvalidRequest(String),
    #[error("websocket error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("timed out waiting for the bus's Welcome frame")]
    HandshakeTimeout,
    #[error("connection closed before the bus sent Welcome")]
    HandshakeClosed,
    #[error("expected a Welcome frame, got something else")]
    UnexpectedFrame,
    #[error("bus rejected the connection: {code}: {message}")]
    HandshakeRejected { code: String, message: String },
    #[error("failed to encode/decode a wire frame: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("send of envelope {id} failed after {SEND_ATTEMPTS} attempts")]
    SendFailed { id: String },
}

/// WS client connection to the crew-bus (plan A1-A3). Owns a background
/// reader task that auto-acks every received `Envelope` with a `Receipt`
/// (A2) and resolves pending `send()` calls when their `Receipt` arrives
/// (A3).
pub struct BusConn {
    outbound: Arc<Mutex<SplitSink<WsStream, Message>>>,
    pending_receipts: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    events_rx: mpsc::Receiver<BusEvent>,
    reader_task: JoinHandle<()>,
}

impl BusConn {
    /// Connects, authenticates via the HTTP-upgrade headers (t-wire:
    /// `Authorization: Bearer <token>` + `X-Crew-Agent: <agent_id>`), and
    /// waits up to 5s for the bus's `Welcome` frame before returning.
    pub async fn connect(url: &str, token: &str, agent_id: &str) -> Result<Self, BusError> {
        let mut request = url
            .into_client_request()
            .map_err(|e| BusError::InvalidRequest(e.to_string()))?;
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                    BusError::InvalidRequest(e.to_string())
                })?,
        );
        headers.insert(
            "X-Crew-Agent",
            agent_id.parse().map_err(
                |e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                    BusError::InvalidRequest(e.to_string())
                },
            )?,
        );

        let (mut ws_stream, _response) = connect_async(request).await?;

        let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, ws_stream.next())
            .await
            .map_err(|_| BusError::HandshakeTimeout)?
            .ok_or(BusError::HandshakeClosed)??;

        let text = match first {
            Message::Text(t) => t,
            _ => return Err(BusError::UnexpectedFrame),
        };
        match serde_json::from_str::<ServerFrame>(text.as_str())? {
            ServerFrame::Welcome { .. } => {}
            ServerFrame::Error { code, message } => {
                return Err(BusError::HandshakeRejected { code, message })
            }
            _ => return Err(BusError::UnexpectedFrame),
        }

        let (write, read) = ws_stream.split();
        let outbound = Arc::new(Mutex::new(write));
        let pending_receipts = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, events_rx) = mpsc::channel(EVENTS_CHANNEL_CAPACITY);

        let reader_task = tokio::spawn(reader_loop(
            read,
            outbound.clone(),
            pending_receipts.clone(),
            events_tx,
        ));

        Ok(Self {
            outbound,
            pending_receipts,
            events_rx,
            reader_task,
        })
    }

    /// Next deduped `Envelope` or `Error` frame from the bus. `None` once
    /// the connection has closed and the reader task has drained.
    pub async fn recv(&mut self) -> Option<BusEvent> {
        self.events_rx.recv().await
    }

    /// Sends `env`, waiting up to 2s per attempt for the server's `Receipt`;
    /// retries with capped exponential backoff + jitter, 3 attempts total
    /// (plan A3 / [backend-common-reliability-timeouts-and-retries]).
    pub async fn send(&self, env: Envelope) -> Result<(), BusError> {
        let id = env.id.clone();
        let text = serde_json::to_string(&ClientFrame::Envelope(env))?;

        for attempt in 1..=SEND_ATTEMPTS {
            let (tx, rx) = oneshot::channel();
            self.pending_receipts.lock().await.insert(id.clone(), tx);

            {
                let mut out = self.outbound.lock().await;
                out.send(Message::Text(text.clone().into())).await?;
            }

            match tokio::time::timeout(RECEIPT_TIMEOUT, rx).await {
                Ok(Ok(())) => return Ok(()),
                _ => {
                    self.pending_receipts.lock().await.remove(&id);
                    if attempt < SEND_ATTEMPTS {
                        tokio::time::sleep(backoff_with_jitter(attempt)).await;
                    }
                }
            }
        }

        Err(BusError::SendFailed { id })
    }

    /// Writes `env` to the wire once and returns — no `Receipt` wait, no
    /// retry, no `pending_receipts` bookkeeping (t2-be-presence D2: a
    /// volatile signal whose failure is meant to be ignored, not retried).
    /// `requires_ack=false` on the envelope only marks it as needing no
    /// *application*-level ack; the bus's own transport-level Receipt-relay
    /// to this sender is a separate mechanism gated on that same field
    /// (`crew-bus/src/routing.rs::route_one` only tracks a pending receipt
    /// when `envelope.requires_ack`), so a presence envelope's Receipt
    /// never arrives back here — [`Self::send`]'s normal wait-then-retry
    /// path would always exhaust `SEND_ATTEMPTS * RECEIPT_TIMEOUT` (~6s)
    /// before giving up, which is both wasted latency and, chained across a
    /// real conversation's every envelope hop, enough to stall the whole
    /// exchange. This is what `emit_presence_start`/`emit_presence_end`
    /// (crew-agent/src/runner.rs) use instead.
    pub async fn send_best_effort(&self, env: Envelope) -> Result<(), BusError> {
        let text = serde_json::to_string(&ClientFrame::Envelope(env))?;
        let mut out = self.outbound.lock().await;
        out.send(Message::Text(text.into())).await?;
        Ok(())
    }
}

impl Drop for BusConn {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

/// `sleep = random(0, min(cap, base * 2^attempt))` — AWS full-jitter formula
/// ([backend-common-reliability-timeouts-and-retries]). Seeded from the
/// system clock's sub-second nanos so no extra RNG dependency is needed.
fn backoff_with_jitter(attempt: u32) -> Duration {
    let exp = BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(10));
    let capped = exp.min(BACKOFF_CAP_MS);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    Duration::from_millis(nanos % (capped + 1))
}

async fn reader_loop(
    mut read: SplitStream<WsStream>,
    outbound: Arc<Mutex<SplitSink<WsStream, Message>>>,
    pending_receipts: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    events_tx: mpsc::Sender<BusEvent>,
) {
    let mut seen: HashSet<String> = HashSet::new();

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(_) => break,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let frame: ServerFrame = match serde_json::from_str(text.as_str()) {
            Ok(f) => f,
            Err(_) => continue,
        };

        match frame {
            ServerFrame::Welcome { .. } => {}
            ServerFrame::Receipt { id } => {
                if let Some(tx) = pending_receipts.lock().await.remove(&id) {
                    let _ = tx.send(());
                }
            }
            ServerFrame::Envelope(env) => {
                let is_new = seen.insert(env.id.clone());

                let receipt = ClientFrame::Receipt {
                    id: env.id.clone(),
                };
                if let Ok(receipt_text) = serde_json::to_string(&receipt) {
                    let mut out = outbound.lock().await;
                    let _ = out.send(Message::Text(receipt_text.into())).await;
                }

                if is_new && events_tx.send(BusEvent::Envelope(env)).await.is_err() {
                    break;
                }
            }
            ServerFrame::Error { code, message } => {
                if events_tx.send(BusEvent::Error { code, message }).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_proto::MessageKind;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    async fn fake_bus(
        on_welcome: impl FnOnce() -> ServerFrame + Send + 'static,
    ) -> (String, mpsc::UnboundedReceiver<ClientFrame>, mpsc::UnboundedReceiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");

        let (client_frames_tx, client_frames_rx) = mpsc::unbounded_channel::<ClientFrame>();
        let (headers_tx, headers_rx) = mpsc::unbounded_channel::<(String, String)>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.expect("accept");
            let headers_tx2 = headers_tx.clone();
            let callback = move |req: &Request, res: Response| {
                let auth = req
                    .headers()
                    .get("Authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let agent = req
                    .headers()
                    .get("X-Crew-Agent")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let _ = headers_tx2.send(("Authorization".to_string(), auth));
                let _ = headers_tx2.send(("X-Crew-Agent".to_string(), agent));
                Ok(res)
            };
            let mut ws = tokio_tungstenite::accept_hdr_async(stream, callback)
                .await
                .expect("accept_hdr_async");

            let welcome = on_welcome();
            let text = serde_json::to_string(&welcome).unwrap();
            ws.send(Message::Text(text.into())).await.unwrap();

            while let Some(Ok(Message::Text(t))) = ws.next().await {
                if let Ok(frame) = serde_json::from_str::<ClientFrame>(t.as_str()) {
                    if client_frames_tx.send(frame).is_err() {
                        break;
                    }
                }
            }
        });

        (url, client_frames_rx, headers_rx)
    }

    fn sample_envelope(id_suffix: &str) -> Envelope {
        let mut env = Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:designer".to_string(),
            vec!["agent:pm".to_string()],
            MessageKind::Question,
            None,
            format!("req_{id_suffix}"),
            json!({}),
            vec![],
            true,
            900_000,
        );
        env.id = format!("msg_{id_suffix}");
        env
    }

    #[tokio::test]
    async fn connect_succeeds_on_welcome_and_sends_auth_headers() {
        let (url, _frames, mut headers) = fake_bus(|| ServerFrame::Welcome {
            agent_id: "agent:designer".to_string(),
        })
        .await;

        let conn = BusConn::connect(&url, "tok123", "agent:designer").await;
        assert!(conn.is_ok());

        let mut seen = HashMap::new();
        for _ in 0..2 {
            if let Some((k, v)) = headers.recv().await {
                seen.insert(k, v);
            }
        }
        assert_eq!(seen.get("Authorization").map(String::as_str), Some("Bearer tok123"));
        assert_eq!(seen.get("X-Crew-Agent").map(String::as_str), Some("agent:designer"));
    }

    #[tokio::test]
    async fn connect_fails_when_first_frame_is_not_welcome() {
        let (url, _frames, _headers) = fake_bus(|| ServerFrame::Error {
            code: "unauthorized".to_string(),
            message: "bad token".to_string(),
        })
        .await;

        let result = BusConn::connect(&url, "tok123", "agent:designer").await;

        assert!(matches!(result, Err(BusError::HandshakeRejected { .. })));
    }

    #[tokio::test]
    async fn recv_auto_acks_with_receipt_and_dedups_duplicate_delivery() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");
        let (receipts_tx, mut receipts_rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let welcome = ServerFrame::Welcome {
                agent_id: "agent:designer".to_string(),
            };
            ws.send(Message::Text(serde_json::to_string(&welcome).unwrap().into()))
                .await
                .unwrap();

            let env = sample_envelope("dup1");
            let frame = ServerFrame::Envelope(env);
            let text = serde_json::to_string(&frame).unwrap();
            // Send the same envelope twice (at-least-once redelivery, A2).
            ws.send(Message::Text(text.clone().into())).await.unwrap();
            ws.send(Message::Text(text.into())).await.unwrap();

            while let Some(Ok(Message::Text(t))) = ws.next().await {
                if let Ok(ClientFrame::Receipt { id }) = serde_json::from_str(t.as_str()) {
                    let _ = receipts_tx.send(id);
                }
            }
        });

        let mut conn = BusConn::connect(&url, "tok", "agent:designer").await.unwrap();

        let first = conn.recv().await.expect("first delivery");
        match first {
            BusEvent::Envelope(env) => assert_eq!(env.id, "msg_dup1"),
            other => panic!("expected Envelope, got {other:?}"),
        }

        // Only one delivery reaches the consumer even though the server sent it twice.
        let second = tokio::time::timeout(Duration::from_millis(300), conn.recv()).await;
        assert!(second.is_err(), "duplicate must not be redelivered to the consumer");

        // But both copies get a Receipt.
        let r1 = tokio::time::timeout(Duration::from_secs(1), receipts_rx.recv())
            .await
            .unwrap()
            .unwrap();
        let r2 = tokio::time::timeout(Duration::from_secs(1), receipts_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(r1, "msg_dup1");
        assert_eq!(r2, "msg_dup1");
    }

    #[tokio::test]
    async fn send_retries_until_receipt_arrives() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");
        let (copies_tx, mut copies_rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let welcome = ServerFrame::Welcome {
                agent_id: "agent:pm".to_string(),
            };
            ws.send(Message::Text(serde_json::to_string(&welcome).unwrap().into()))
                .await
                .unwrap();

            let mut copies_seen = 0u32;
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                if let Ok(ClientFrame::Envelope(env)) = serde_json::from_str(t.as_str()) {
                    copies_seen += 1;
                    let _ = copies_tx.send(env.id.clone());
                    // Drop the first copy on the floor; only ack from the
                    // second copy onward, forcing BusConn::send to retry.
                    if copies_seen >= 2 {
                        let receipt = ServerFrame::Receipt { id: env.id };
                        let text = serde_json::to_string(&receipt).unwrap();
                        ws.send(Message::Text(text.into())).await.unwrap();
                    }
                }
            }
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let env = sample_envelope("retry1");

        let result = conn.send(env).await;
        assert!(result.is_ok(), "send should succeed once the retried copy is acked");

        let c1 = copies_rx.recv().await.unwrap();
        let c2 = copies_rx.recv().await.unwrap();
        assert_eq!(c1, "msg_retry1");
        assert_eq!(c2, "msg_retry1");
    }

    #[tokio::test]
    async fn send_fails_after_exhausting_retries() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("ws://{addr}");

        tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let welcome = ServerFrame::Welcome {
                agent_id: "agent:pm".to_string(),
            };
            ws.send(Message::Text(serde_json::to_string(&welcome).unwrap().into()))
                .await
                .unwrap();
            // Never ack anything — drain frames silently.
            while ws.next().await.is_some() {}
        });

        let conn = BusConn::connect(&url, "tok", "agent:pm").await.unwrap();
        let env = sample_envelope("never-acked");

        let result = conn.send(env).await;
        assert!(matches!(result, Err(BusError::SendFailed { .. })));
    }
}
