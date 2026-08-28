//! Integration tests for crew-bus, driven as raw `tokio-tungstenite`
//! clients (never `crew-agent`, which is owned by a separate task) —
//! brief DoD: normal routing+Receipt, redelivery-then-success, exhausted
//! delivery_failed, dedup, loop_blocked at round 4, unknown_recipient,
//! auth failure 401, duplicate agent_id rejected.

use std::time::Duration;

use crew_bus::{BusConfig, BusServer};
use crew_proto::{ClientFrame, Envelope, MessageKind, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const TOKEN: &str = "test-token";

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start_bus(retry_base: Duration) -> (crew_bus::BusHandle, String) {
    let mut cfg = BusConfig::new(TOKEN);
    cfg.retry_base = retry_base;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = BusServer::new(cfg).serve(listener).await;
    let url = format!("ws://{}/ws", handle.local_addr());
    (handle, url)
}

async fn connect_raw(
    url: &str,
    token: Option<&str>,
    agent_id: Option<&str>,
) -> Result<(WsStream, tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>), tokio_tungstenite::tungstenite::Error>
{
    let mut request = url.into_client_request().unwrap();
    if let Some(token) = token {
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    }
    if let Some(agent_id) = agent_id {
        request
            .headers_mut()
            .insert("X-Crew-Agent", agent_id.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request).await
}

/// Connects as `agent_id`, asserts the first frame is `Welcome`, and
/// returns the open stream positioned right after it.
async fn connect_agent(url: &str, agent_id: &str) -> WsStream {
    let (mut ws, _resp) = connect_raw(url, Some(TOKEN), Some(agent_id))
        .await
        .unwrap_or_else(|e| panic!("{agent_id} failed to connect: {e}"));
    let first = recv_frame(&mut ws, Duration::from_secs(3))
        .await
        .unwrap_or_else(|| panic!("{agent_id} got no Welcome"));
    assert_eq!(
        first,
        ServerFrame::Welcome {
            agent_id: agent_id.to_string()
        }
    );
    ws
}

async fn send_client_frame(ws: &mut WsStream, frame: &ClientFrame) {
    let json = serde_json::to_string(frame).unwrap();
    ws.send(WsMessage::Text(json.into())).await.unwrap();
}

/// Reads WS messages until a `Text` frame decodes as a `ServerFrame`, a
/// non-text control frame is skipped, the stream ends, or `timeout` elapses.
async fn recv_frame(ws: &mut WsStream, timeout: Duration) -> Option<ServerFrame> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(WsMessage::Text(text)))) => {
                return Some(serde_json::from_str(&text).expect("valid ServerFrame JSON"))
            }
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(_))) | Ok(None) => return None,
            Err(_) => return None,
        }
    }
}

fn envelope(from: &str, to: &str, kind: MessageKind, corr: &str, requires_ack: bool) -> Envelope {
    Envelope::new(
        "sp-1".to_string(),
        "th-1".to_string(),
        from.to_string(),
        vec![to.to_string()],
        kind,
        None,
        corr.to_string(),
        serde_json::json!({}),
        vec![],
        requires_ack,
        60_000,
    )
}

#[tokio::test]
async fn test_normal_routing_delivers_and_receipts() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        true,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;

    let delivered = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the envelope");
    assert_eq!(delivered, ServerFrame::Envelope(env.clone()));

    send_client_frame(&mut receiver, &ClientFrame::Receipt { id: env.id.clone() }).await;

    let acked = recv_frame(&mut sender, Duration::from_secs(2))
        .await
        .expect("sender should get the forwarded Receipt");
    assert_eq!(acked, ServerFrame::Receipt { id: env.id.clone() });

    bus.shutdown().await;
}

#[tokio::test]
async fn test_redelivery_after_no_receipt_succeeds() {
    let (bus, url) = start_bus(Duration::from_millis(20)).await;
    let mut events = bus.subscribe();
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        true,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;

    let first = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the first delivery");
    assert_eq!(first, ServerFrame::Envelope(env.clone()));

    // Deliberately don't ack yet — wait for the bus to redeliver.
    let redelivered = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get a redelivered copy");
    assert_eq!(redelivered, ServerFrame::Envelope(env.clone()));

    send_client_frame(&mut receiver, &ClientFrame::Receipt { id: env.id.clone() }).await;

    let acked = recv_frame(&mut sender, Duration::from_secs(2))
        .await
        .expect("sender should get the forwarded Receipt");
    assert_eq!(acked, ServerFrame::Receipt { id: env.id.clone() });

    let saw_redelivered_event = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match events.recv().await {
                Ok(crew_bus::BusEvent::Redelivered { id, .. }) if id == env.id => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(saw_redelivered_event, "expected a Redelivered BusEvent");

    bus.shutdown().await;
}

#[tokio::test]
async fn test_delivery_exhausted_after_max_attempts() {
    let (bus, url) = start_bus(Duration::from_millis(15)).await;
    let mut events = bus.subscribe();
    let mut sender = connect_agent(&url, "agent:sender").await;
    let _receiver = connect_agent(&url, "agent:receiver").await; // never acks

    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        true,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;

    let failure = recv_frame(&mut sender, Duration::from_secs(3))
        .await
        .expect("sender should get a delivery_failed Error");
    assert_eq!(
        failure,
        ServerFrame::Error {
            code: "delivery_failed".to_string(),
            message: env.id.clone(),
        }
    );

    // Default max_delivery_attempts = 3: exactly 2 redeliveries (attempts 2
    // and 3) beyond the initial send, then exhaustion — no more, no less.
    let mut redelivered_attempts = Vec::new();
    let collected = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            match events.recv().await {
                Ok(crew_bus::BusEvent::Redelivered { id, attempt }) if id == env.id => {
                    redelivered_attempts.push(attempt)
                }
                Ok(crew_bus::BusEvent::DeliveryFailed { id }) if id == env.id => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await;
    assert!(collected.is_ok(), "expected DeliveryFailed event within timeout");
    assert_eq!(
        redelivered_attempts,
        vec![2, 3],
        "expected exactly attempts 2 and 3 as redeliveries before exhaustion"
    );

    bus.shutdown().await;
}

#[tokio::test]
async fn test_duplicate_submission_is_deduped() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        false,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;
    let first = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the first copy");
    assert_eq!(first, ServerFrame::Envelope(env.clone()));

    // Resubmit the identical envelope (same id) — a client-side retry.
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;
    let dedup_reply = recv_frame(&mut sender, Duration::from_secs(2))
        .await
        .expect("sender should get a Receipt for the duplicate submission");
    assert_eq!(dedup_reply, ServerFrame::Receipt { id: env.id.clone() });

    // A distinct, subsequent envelope proves the duplicate above was never
    // re-routed: it must be the very next thing the receiver sees.
    let canary = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_2",
        false,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(canary.clone())).await;
    let next = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the canary next");
    assert_eq!(next, ServerFrame::Envelope(canary));

    bus.shutdown().await;
}

#[tokio::test]
async fn test_loop_blocked_after_max_rounds() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let mut events = bus.subscribe();
    let mut pm = connect_agent(&url, "agent:pm").await;
    let mut designer = connect_agent(&url, "agent:designer").await;

    let corr = "req_loop";
    // Default max_rounds = 3: hops 1-4 accepted, hop 5 exceeds the budget.
    let hop1 = envelope("agent:pm", "agent:designer", MessageKind::ChangeRequest, corr, false);
    send_client_frame(&mut pm, &ClientFrame::Envelope(hop1.clone())).await;
    assert_eq!(
        recv_frame(&mut designer, Duration::from_secs(2)).await,
        Some(ServerFrame::Envelope(hop1))
    );

    let hop2 = envelope("agent:designer", "agent:pm", MessageKind::TaskResult, corr, false);
    send_client_frame(&mut designer, &ClientFrame::Envelope(hop2.clone())).await;
    assert_eq!(
        recv_frame(&mut pm, Duration::from_secs(2)).await,
        Some(ServerFrame::Envelope(hop2))
    );

    let hop3 = envelope("agent:pm", "agent:designer", MessageKind::ChangeRequest, corr, false);
    send_client_frame(&mut pm, &ClientFrame::Envelope(hop3.clone())).await;
    assert_eq!(
        recv_frame(&mut designer, Duration::from_secs(2)).await,
        Some(ServerFrame::Envelope(hop3))
    );

    let hop4 = envelope("agent:designer", "agent:pm", MessageKind::TaskResult, corr, false);
    send_client_frame(&mut designer, &ClientFrame::Envelope(hop4.clone())).await;
    assert_eq!(
        recv_frame(&mut pm, Duration::from_secs(2)).await,
        Some(ServerFrame::Envelope(hop4))
    );

    let hop5 = envelope("agent:pm", "agent:designer", MessageKind::ChangeRequest, corr, false);
    send_client_frame(&mut pm, &ClientFrame::Envelope(hop5.clone())).await;
    let blocked = recv_frame(&mut pm, Duration::from_secs(2))
        .await
        .expect("pm should get a loop_blocked Error, not a routed frame");
    assert_eq!(
        blocked,
        ServerFrame::Error {
            code: "loop_blocked".to_string(),
            message: corr.to_string(),
        }
    );
    // Designer must not receive hop5 — it was blocked, not delivered.
    assert_eq!(recv_frame(&mut designer, Duration::from_millis(300)).await, None);

    let saw_loop_blocked_event = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match events.recv().await {
                Ok(crew_bus::BusEvent::LoopBlocked { corr: c }) if c == corr => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(saw_loop_blocked_event, "expected a LoopBlocked BusEvent");

    bus.shutdown().await;
}

#[tokio::test]
async fn test_unknown_recipient_rejected() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let mut sender = connect_agent(&url, "agent:sender").await;

    let env = envelope(
        "agent:sender",
        "agent:ghost",
        MessageKind::Question,
        "req_1",
        false,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;

    let rejected = recv_frame(&mut sender, Duration::from_secs(2))
        .await
        .expect("sender should get an unknown_recipient Error");
    assert_eq!(
        rejected,
        ServerFrame::Error {
            code: "unknown_recipient".to_string(),
            message: "agent:ghost".to_string(),
        }
    );

    bus.shutdown().await;
}

#[tokio::test]
async fn test_auth_failure_returns_401() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;

    let err = connect_raw(&url, Some("wrong-token"), Some("agent:x"))
        .await
        .expect_err("wrong bearer token must be rejected before upgrade");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("expected an HTTP 401 handshake error, got: {other}"),
    }

    let err_missing = connect_raw(&url, None, Some("agent:y"))
        .await
        .expect_err("missing bearer token must be rejected before upgrade");
    match err_missing {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), 401);
        }
        other => panic!("expected an HTTP 401 handshake error, got: {other}"),
    }

    bus.shutdown().await;
}

/// Waits up to `timeout` for an `EnvelopeAccepted` BusEvent matching `id`.
async fn wait_for_envelope_accepted(
    events: &mut tokio::sync::broadcast::Receiver<crew_bus::BusEvent>,
    id: &str,
    timeout: Duration,
) -> bool {
    tokio::time::timeout(timeout, async {
        loop {
            match events.recv().await {
                Ok(crew_bus::BusEvent::EnvelopeAccepted { envelope }) if envelope.id == id => {
                    return true
                }
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Normal case (contract §C1): a submitted envelope is accepted exactly
/// once, and the broadcast `EnvelopeAccepted` carries the envelope verbatim.
#[tokio::test]
async fn test_envelope_accepted_emitted_once_with_verbatim_fields() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let mut events = bus.subscribe();
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        false,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;
    let _ = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the envelope");

    let accepted = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match events.recv().await {
                Ok(crew_bus::BusEvent::EnvelopeAccepted { envelope }) if envelope.id == env.id => {
                    return Some(envelope)
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
    .expect("expected an EnvelopeAccepted BusEvent");
    // Verbatim field assertions, not just presence.
    assert_eq!(accepted.id, env.id);
    assert_eq!(accepted.from, env.from);
    assert_eq!(accepted.to, env.to);
    assert_eq!(accepted.kind, env.kind);
    assert_eq!(accepted.corr, env.corr);
    assert_eq!(accepted.body, env.body);
    assert_eq!(accepted, env);

    let reemitted = wait_for_envelope_accepted(&mut events, &env.id, Duration::from_millis(300)).await;
    assert!(!reemitted, "EnvelopeAccepted should fire exactly once for a single submission");

    bus.shutdown().await;
}

/// Duplicate case: resubmitting the same envelope id (client-side retry) is
/// deduped by the seen-set and does not re-fire `EnvelopeAccepted`.
#[tokio::test]
async fn test_envelope_accepted_not_reemitted_on_duplicate_submission() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let mut events = bus.subscribe();
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        false,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;
    let _ = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the first copy");
    assert!(
        wait_for_envelope_accepted(&mut events, &env.id, Duration::from_secs(1)).await,
        "expected EnvelopeAccepted for the first submission"
    );

    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;
    let dedup_reply = recv_frame(&mut sender, Duration::from_secs(2))
        .await
        .expect("sender should get a Receipt for the duplicate submission");
    assert_eq!(dedup_reply, ServerFrame::Receipt { id: env.id.clone() });

    let reemitted = wait_for_envelope_accepted(&mut events, &env.id, Duration::from_millis(300)).await;
    assert!(!reemitted, "EnvelopeAccepted must not re-fire on duplicate submission");

    bus.shutdown().await;
}

/// Redelivery case: the background retry loop resending an unacked
/// `requires_ack` envelope must not produce a second `EnvelopeAccepted` —
/// acceptance is bound to first-seen submission, not delivery attempts.
#[tokio::test]
async fn test_envelope_accepted_not_reemitted_on_automatic_redelivery() {
    let (bus, url) = start_bus(Duration::from_millis(20)).await;
    let mut events = bus.subscribe();
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        true,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;

    let _first = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the first delivery");
    // Deliberately don't ack — force at least one automatic redelivery.
    let _redelivered = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get a redelivered copy");

    let mut accepted_count = 0;
    let _ = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            match events.recv().await {
                Ok(crew_bus::BusEvent::EnvelopeAccepted { envelope }) if envelope.id == env.id => {
                    accepted_count += 1;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    })
    .await;
    assert_eq!(
        accepted_count, 1,
        "EnvelopeAccepted must fire exactly once despite an automatic redelivery"
    );

    send_client_frame(&mut receiver, &ClientFrame::Receipt { id: env.id.clone() }).await;
    bus.shutdown().await;
}

/// Boundary case (contract §C1): an envelope accepted but routed to an
/// unregistered recipient still fires `EnvelopeAccepted` — acceptance is
/// distinct from successful delivery.
#[tokio::test]
async fn test_envelope_accepted_emitted_for_unknown_recipient() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let mut events = bus.subscribe();
    let mut sender = connect_agent(&url, "agent:sender").await;

    let env = envelope(
        "agent:sender",
        "agent:ghost",
        MessageKind::Question,
        "req_1",
        false,
    );
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;

    let rejected = recv_frame(&mut sender, Duration::from_secs(2))
        .await
        .expect("sender should get an unknown_recipient Error");
    assert_eq!(
        rejected,
        ServerFrame::Error {
            code: "unknown_recipient".to_string(),
            message: "agent:ghost".to_string(),
        }
    );

    assert!(
        wait_for_envelope_accepted(&mut events, &env.id, Duration::from_secs(1)).await,
        "EnvelopeAccepted should fire even though the recipient is unknown"
    );

    bus.shutdown().await;
}

/// Serialization (contract §C1 / crew-ledger compatibility): `EnvelopeAccepted`
/// uses the same externally-tagged shape as every other `BusEvent`, so the
/// ledger's single-top-level-key `kind` extraction still works unmodified.
#[test]
fn test_envelope_accepted_serializes_externally_tagged() {
    let env = envelope(
        "agent:sender",
        "agent:receiver",
        MessageKind::Question,
        "req_1",
        false,
    );
    let event = crew_bus::BusEvent::EnvelopeAccepted { envelope: env };
    let value = serde_json::to_value(&event).unwrap();
    let obj = value
        .as_object()
        .expect("externally-tagged enum serializes as a single-key object");
    assert_eq!(obj.len(), 1, "expected exactly one top-level key");
    assert!(obj.contains_key("EnvelopeAccepted"));
}

#[tokio::test]
async fn test_duplicate_agent_id_rejected() {
    let (bus, url) = start_bus(Duration::from_millis(200)).await;
    let _first = connect_agent(&url, "agent:dup").await;

    let (mut second, _resp) = connect_raw(&url, Some(TOKEN), Some("agent:dup"))
        .await
        .expect("the WS handshake itself still succeeds for the duplicate");
    let reply = recv_frame(&mut second, Duration::from_secs(2))
        .await
        .expect("duplicate connection should get a duplicate_agent Error");
    assert_eq!(
        reply,
        ServerFrame::Error {
            code: "duplicate_agent".to_string(),
            message: "agent:dup".to_string(),
        }
    );

    // The connection is closed right after; no further frames arrive.
    assert_eq!(recv_frame(&mut second, Duration::from_millis(500)).await, None);

    bus.shutdown().await;
}
