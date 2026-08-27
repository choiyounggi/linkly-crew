//! Integration tests for crew-ledger — DoD: normal round trip, real-bus
//! integration, empty boundary, closed-channel shutdown, and an error path.
//! All DBs are `open_in_memory()`; per security policy, no system temp
//! directory and no temp-file crate is used anywhere here.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crew_bus::{BusConfig, BusEvent, BusServer};
use crew_ledger::{spawn_subscriber, EventLedger};
use crew_proto::{ClientFrame, Envelope, MessageKind, ServerFrame};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const TOKEN: &str = "test-token";

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start_bus() -> (crew_bus::BusHandle, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = BusServer::new(BusConfig::new(TOKEN)).serve(listener).await;
    let url = format!("ws://{}/ws", handle.local_addr());
    (handle, url)
}

async fn connect_agent(url: &str, agent_id: &str) -> WsStream {
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {TOKEN}").parse().unwrap());
    request
        .headers_mut()
        .insert("X-Crew-Agent", agent_id.parse().unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .unwrap_or_else(|e| panic!("{agent_id} failed to connect: {e}"));
    let welcome = recv_frame(&mut ws, Duration::from_secs(3))
        .await
        .unwrap_or_else(|| panic!("{agent_id} got no Welcome"));
    assert_eq!(
        welcome,
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

/// Short retry loop for asserting on ledger state that lands asynchronously
/// (the subscriber task appends after the bus already emitted the event).
async fn poll_until(mut check: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Normal case: append then count/events_of_kind round-trip.
#[test]
fn append_then_count_and_events_of_kind_round_trip() {
    let ledger = EventLedger::open_in_memory().expect("open in-memory ledger");

    ledger
        .append(&BusEvent::Registered {
            agent_id: "agent:a".to_string(),
        })
        .expect("append Registered");
    ledger
        .append(&BusEvent::Delivered {
            id: "id-1".to_string(),
            to: "agent:b".to_string(),
        })
        .expect("append Delivered");

    assert_eq!(ledger.count().unwrap(), 2);
    let registered = ledger.events_of_kind("Registered").unwrap();
    assert_eq!(registered.len(), 1);
    assert!(registered[0].contains("agent:a"));
    let delivered = ledger.events_of_kind("Delivered").unwrap();
    assert_eq!(delivered.len(), 1);
    assert!(delivered[0].contains("agent:b"));
}

/// Boundary case: an empty ledger reports count 0 and no rows of any kind.
#[test]
fn empty_ledger_has_zero_count_and_no_rows() {
    let ledger = EventLedger::open_in_memory().expect("open in-memory ledger");
    assert_eq!(ledger.count().unwrap(), 0);
    assert!(ledger.events_of_kind("Registered").unwrap().is_empty());
}

/// Error case: opening a path whose parent directory does not exist fails.
#[test]
fn open_with_unwritable_path_fails() {
    let bad_path = Path::new("/definitely/not/a/real/crew-ledger-dir/ledger.db");
    let result = EventLedger::open(bad_path);
    assert!(result.is_err(), "expected open() to fail for a bad path");
}

/// Lifecycle case: when the broadcast sender is dropped, the subscriber
/// task observes `Closed` and exits promptly instead of hanging forever.
#[tokio::test]
async fn subscriber_exits_when_channel_closes() {
    let ledger = Arc::new(EventLedger::open_in_memory().expect("open in-memory ledger"));
    let (tx, rx) = tokio::sync::broadcast::channel::<BusEvent>(4);
    let handle = spawn_subscriber(ledger.clone(), rx);

    drop(tx);

    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("subscriber task should exit promptly once the channel closes")
        .expect("subscriber task should not panic");
    assert_eq!(ledger.count().unwrap(), 0);
}

/// Lifecycle case: `Lagged` is a log-and-continue branch, not a shutdown —
/// the subscriber keeps consuming events sent after it falls behind.
#[tokio::test]
async fn subscriber_logs_and_continues_after_lagging() {
    let ledger = Arc::new(EventLedger::open_in_memory().expect("open in-memory ledger"));
    let (tx, rx) = tokio::sync::broadcast::channel::<BusEvent>(2);
    let handle = spawn_subscriber(ledger.clone(), rx);

    // Send more events than the channel's capacity before the subscriber
    // task (spawned but not yet polled on this current-thread runtime) gets
    // to run, forcing its first `recv()` to observe `Lagged` instead of
    // every event.
    for i in 0..5 {
        tx.send(BusEvent::Registered {
            agent_id: format!("agent:{i}"),
        })
        .unwrap();
    }

    let drained = poll_until(
        || ledger.count().unwrap_or(0) >= 1,
        Duration::from_secs(2),
    )
    .await;
    assert!(
        drained,
        "subscriber should still append the events left in the buffer after Lagged"
    );

    // Prove it did not exit on `Lagged`: an event sent afterward still lands.
    tx.send(BusEvent::Registered {
        agent_id: "agent:after-lag".to_string(),
    })
    .unwrap();
    let saw_after_lag = poll_until(
        || {
            ledger
                .events_of_kind("Registered")
                .unwrap_or_default()
                .iter()
                .any(|json| json.contains("agent:after-lag"))
        },
        Duration::from_secs(2),
    )
    .await;
    assert!(
        saw_after_lag,
        "subscriber should still be running after observing Lagged"
    );

    drop(tx);
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("subscriber task should exit once the channel closes")
        .expect("subscriber task should not panic");
}

/// Integration case: a real BusServer's Registered/Delivered events land as
/// ledger rows via `spawn_subscriber`.
#[tokio::test]
async fn integration_bus_events_land_in_ledger() {
    let (bus, url) = start_bus().await;
    let ledger = Arc::new(EventLedger::open_in_memory().expect("open in-memory ledger"));
    let handle = spawn_subscriber(ledger.clone(), bus.subscribe());

    let mut sender = connect_agent(&url, "agent:sender").await;
    let mut receiver = connect_agent(&url, "agent:receiver").await;

    let env = envelope("agent:sender", "agent:receiver", "req_1");
    send_client_frame(&mut sender, &ClientFrame::Envelope(env.clone())).await;
    recv_frame(&mut receiver, Duration::from_secs(2))
        .await
        .expect("receiver should get the envelope");

    let saw_registrations = poll_until(
        || ledger.events_of_kind("Registered").unwrap_or_default().len() >= 2,
        Duration::from_secs(2),
    )
    .await;
    assert!(saw_registrations, "expected two Registered ledger rows");

    let saw_delivery = poll_until(
        || !ledger.events_of_kind("Delivered").unwrap_or_default().is_empty(),
        Duration::from_secs(2),
    )
    .await;
    assert!(saw_delivery, "expected a Delivered ledger row");
    let delivered = ledger.events_of_kind("Delivered").unwrap();
    assert!(delivered[0].contains(&env.id));

    bus.shutdown().await;
    handle.abort();
}
