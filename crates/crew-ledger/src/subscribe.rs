use std::sync::Arc;

use tokio::sync::broadcast;

use crew_bus::BusEvent;

use crate::store::EventLedger;

/// Drains a `BusEvent` broadcast stream into the ledger until the channel
/// closes. `Lagged` means the broadcast (capacity 256) dropped events before
/// this subscriber read them — a known property of `tokio::sync::broadcast`;
/// M3 only observes and logs the gap, it does not attempt to recover it.
pub fn spawn_subscriber(
    ledger: Arc<EventLedger>,
    mut rx: broadcast::Receiver<BusEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(err) = ledger.append(&event) {
                        tracing::error!(error = %err, "failed to append bus event to ledger");
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ledger subscriber lagged; events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}
