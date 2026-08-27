use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use crew_proto::ServerFrame;
use futures_util::{SinkExt, StreamExt};

use crate::event::BusEvent;
use crate::routing::handle_client_text;
use crate::state::{send_to, unregister, Shared};

/// GET /ws upgrade handler — plan B2. Auth happens at the HTTP upgrade:
/// `Authorization: Bearer <token>` must match `cfg.token` and
/// `X-Crew-Agent` must be present, or the upgrade is refused with 401
/// before any WebSocket handshake happens.
pub(crate) async fn ws_handler(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(agent_id) = authenticate(&shared, &headers) else {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    };
    ws.on_upgrade(move |socket| handle_socket(socket, agent_id, shared))
}

fn authenticate(shared: &Shared, headers: &HeaderMap) -> Option<String> {
    let auth = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let token = auth.strip_prefix("Bearer ")?;
    if token != shared.cfg.token {
        return None;
    }
    let agent_id = headers.get("X-Crew-Agent")?.to_str().ok()?;
    if agent_id.is_empty() {
        return None;
    }
    Some(agent_id.to_string())
}

/// Post-upgrade connection lifecycle — plan B2/B3/B8. A duplicate
/// `agent_id` gets `Error{code:"duplicate_agent"}` then the socket closes
/// without registering; otherwise the connection registers, is sent
/// `Welcome`, and then serves both directions until it closes.
async fn handle_socket(socket: WebSocket, agent_id: String, shared: Arc<Shared>) {
    let (mut sender, mut receiver) = socket.split();

    // The registry lock must not be held across an `.await` (Send bound on
    // the connection future), so the check-and-insert decision is made and
    // the guard dropped before any awaited send below.
    let new_rx = {
        let mut registry = shared.registry.lock().unwrap();
        if registry.contains_key(&agent_id) {
            None
        } else {
            let (tx, rx) = tokio::sync::mpsc::channel(shared.cfg.outbound_buffer);
            registry.insert(agent_id.clone(), tx);
            Some(rx)
        }
    };
    let Some(mut out_rx) = new_rx else {
        let json = serde_json::to_string(&ServerFrame::Error {
            code: "duplicate_agent".to_string(),
            message: agent_id.clone(),
        })
        .expect("ServerFrame::Error always serializes");
        let _ = sender.send(Message::Text(json.into())).await;
        let _ = sender.send(Message::Close(None)).await;
        return;
    };
    let _ = shared.events.send(BusEvent::Registered {
        agent_id: agent_id.clone(),
    });
    send_to(
        &shared,
        &agent_id,
        ServerFrame::Welcome {
            agent_id: agent_id.clone(),
        },
    );

    let mut ping_ticker = tokio::time::interval(shared.cfg.ping_interval);
    ping_ticker.tick().await; // first tick fires immediately; consume it
    let mut last_pong = tokio::time::Instant::now();

    loop {
        tokio::select! {
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(frame) => {
                        if send_frame(&mut sender, &frame).await.is_err() {
                            break;
                        }
                    }
                    None => break, // registry dropped our sender (send_to backpressure close)
                }
            }
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        handle_client_text(&shared, &agent_id, text.as_str());
                    }
                    Some(Ok(Message::Pong(_))) => {
                        last_pong = tokio::time::Instant::now();
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break,
                }
            }
            _ = ping_ticker.tick() => {
                if last_pong.elapsed() > shared.cfg.ping_interval * 2 {
                    break;
                }
                if sender.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
        }
    }

    unregister(&shared, &agent_id);
}

async fn send_frame(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    frame: &ServerFrame,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(frame).expect("ServerFrame always serializes");
    sender.send(Message::Text(json.into())).await
}
