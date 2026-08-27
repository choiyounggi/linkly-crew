//! crew-bus — axum WS bus server: registration, direct routing,
//! Receipt-based at-least-once redelivery, dedup, CorrGuard enforcement,
//! and observer events. DESIGN.md §3/§11 M2.

mod connection;
mod routing;
mod state;

pub mod config;
pub mod event;

pub use config::BusConfig;
pub use event::BusEvent;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::routing::get;
use axum::Router;
use crew_proto::CorrGuard;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, oneshot};

use connection::ws_handler;
use state::{SeenSet, Shared};

/// Builds a bus server from a fixed configuration — plan B1.
pub struct BusServer {
    cfg: BusConfig,
}

impl BusServer {
    pub fn new(cfg: BusConfig) -> Self {
        Self { cfg }
    }

    /// Binds the axum WS app to an already-bound listener (tests use
    /// `127.0.0.1:0` for an ephemeral port) and serves it on a background
    /// task until `BusHandle::shutdown` is called.
    pub async fn serve(self, listener: TcpListener) -> BusHandle {
        let local_addr = listener
            .local_addr()
            .expect("bound listener always has a local addr");

        let (events_tx, _) = broadcast::channel(256);
        let max_rounds = self.cfg.max_rounds;
        let seen_capacity = self.cfg.seen_capacity;
        let shared = Arc::new(Shared {
            cfg: self.cfg,
            registry: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            seen: Mutex::new(SeenSet::new(seen_capacity)),
            guard: Mutex::new(CorrGuard::new(max_rounds)),
            events: events_tx.clone(),
        });

        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(shared);

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        BusHandle {
            local_addr,
            events_tx,
            shutdown_tx: Some(shutdown_tx),
            join: Some(join),
        }
    }
}

/// Handle to a running bus — plan B1. `subscribe()` gives tests and
/// operators a live feed of `BusEvent`s; `shutdown()` stops the server and
/// waits for it to finish.
pub struct BusHandle {
    local_addr: SocketAddr,
    events_tx: broadcast::Sender<BusEvent>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl BusHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BusEvent> {
        self.events_tx.subscribe()
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}
