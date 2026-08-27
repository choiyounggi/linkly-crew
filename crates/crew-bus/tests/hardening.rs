//! Hardening tests for crew-bus (M2 review carry-over):
//! - envelope.from spoofing rejected before routing (DESIGN §9 impersonation).
//! - seen-set dedup window is capacity-bounded, not unbounded.

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

async fn start_bus(cfg: BusConfig) -> (crew_bus::BusHandle, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = BusServer::new(cfg).serve(listener).await;
    let url = format!("ws://{}/ws", handle.local_addr());
    (handle, url)
}

fn default_cfg() -> BusConfig {
    let mut cfg = BusConfig::new(TOKEN);
    cfg.retry_base = Duration::from_millis(200);
    cfg
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

/// `Envelope::new` generates its own `id` (ULID) — every field here is
/// otherwise caller-supplied, per crew-proto's signature.
fn envelope(from: &str, to: &str, corr: &str) -> Envelope {
    Envelope::new(
        "sp-1".to_string(),
        "th-1".to_string(),
        from.to_string(),
        vec![to.to_string()],
        MessageKind::Question,
        None,
        corr.to_string(),
        serde_json::json!({}),
        vec![],
        false,
        60_000,
    )
}

/// Normal case: an envelope whose `from` matches the authenticated
/// connection id is still delivered exactly as before hardening.
#[tokio::test]
async fn test_matching_from_is_delivered() {
    let (bus, url) = start_bus(default_cfg()).await;
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope("agent:sender", "agent:receiver", "req_1");
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;

    let delivered = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the envelope");
    assert_eq!(delivered, ServerFrame::Envelope(env));

    bus.shutdown().await;
}

/// Error case: a connection authenticated as `agent:mallory` claiming
/// `from: "agent:victim"` is rejected before routing — the sender gets
/// `from_mismatch`, the intended recipient never sees it, a
/// `SpoofRejected` event fires, and the connection stays open (proven by
/// a subsequent legitimate envelope still working on the same socket).
#[tokio::test]
async fn test_spoofed_from_is_rejected_not_routed() {
    let (bus, url) = start_bus(default_cfg()).await;
    let mut events = bus.subscribe();
    let mut mallory = connect_agent(&url, "agent:mallory").await;
    let mut victim = connect_agent(&url, "agent:victim").await;

    let spoofed = envelope("agent:victim", "agent:victim", "req_spoof");
    let spoofed_id = spoofed.id.clone();
    send_client_frame(&mut mallory, &ClientFrame::Envelope(spoofed.clone())).await;

    let rejection = recv_frame(&mut mallory, Duration::from_secs(2))
        .await
        .expect("mallory should get a from_mismatch Error");
    assert_eq!(
        rejection,
        ServerFrame::Error {
            code: "from_mismatch".to_string(),
            message: "agent:victim".to_string(),
        }
    );

    // Victim never sees the spoofed envelope — it was never routed.
    assert_eq!(recv_frame(&mut victim, Duration::from_millis(300)).await, None);

    let saw_spoof_rejected = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match events.recv().await {
                Ok(crew_bus::BusEvent::SpoofRejected {
                    id,
                    claimed_from,
                    agent_id,
                }) if id == spoofed_id => {
                    assert_eq!(claimed_from, "agent:victim");
                    assert_eq!(agent_id, "agent:mallory");
                    return true;
                }
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(saw_spoof_rejected, "expected a SpoofRejected BusEvent");

    // Connection stays open: mallory can still send a truthful envelope.
    let honest = envelope("agent:mallory", "agent:victim", "req_honest");
    send_client_frame(&mut mallory, &ClientFrame::Envelope(honest.clone())).await;
    let delivered = recv_frame(&mut victim, Duration::from_secs(2))
        .await
        .expect("victim should get the honest envelope on the still-open connection");
    assert_eq!(delivered, ServerFrame::Envelope(honest));

    bus.shutdown().await;
}

/// Boundary case: with `seen_capacity = 2`, submitting 3 distinct ids
/// evicts the oldest (`id-1`). Resubmitting `id-1` afterward is outside
/// the dedup window and gets re-routed (documented eviction behavior),
/// while resubmitting `id-3` (still inside the window) only gets a
/// Receipt and is not re-routed.
#[tokio::test]
async fn test_seen_capacity_evicts_oldest_id() {
    let mut cfg = default_cfg();
    cfg.seen_capacity = 2;
    let (bus, url) = start_bus(cfg).await;
    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let mut sent = Vec::new();
    for label in ["id-1", "id-2", "id-3"] {
        let env = envelope("agent:sender", "agent:receiver", "req_1");
        send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;
        let delivered = recv_frame(&mut receiver, Duration::from_secs(2))
            .await
            .unwrap_or_else(|| panic!("receiver should get {label}"));
        assert_eq!(delivered, ServerFrame::Envelope(env.clone()));
        sent.push(env);
    }
    let id_1 = sent[0].clone();
    let id_3 = sent[2].clone();

    // Within-window duplicate (id-3, still tracked): Receipt only, no re-route.
    send_client_frame(&mut sender, &ClientFrame::Envelope(id_3.clone())).await;
    let dup_reply = recv_frame(&mut sender, Duration::from_secs(2))
        .await
        .expect("sender should get a Receipt for the within-window duplicate");
    assert_eq!(dup_reply, ServerFrame::Receipt { id: id_3.id.clone() });

    // Outside-window resubmit (id-1, evicted by capacity=2): re-routed as new.
    send_client_frame(&mut sender, &ClientFrame::Envelope(id_1.clone())).await;
    let redelivered = recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("id-1 fell outside the capacity-2 dedup window and should be re-routed");
    assert_eq!(redelivered, ServerFrame::Envelope(id_1));

    bus.shutdown().await;
}
