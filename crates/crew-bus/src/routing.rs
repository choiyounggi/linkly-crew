use std::sync::Arc;
use std::time::Duration;

use crew_proto::{ClientFrame, Envelope, MessageKind, ServerFrame, Verdict};
use rand::Rng;

use crate::event::BusEvent;
use crate::state::{send_to, PendingEntry, Shared};

/// Parses and dispatches one incoming WS text frame from `from_agent`. A
/// frame that fails to parse as `ClientFrame` JSON is rejected with
/// `bad_frame` but the connection stays open — plan B10.
pub(crate) fn handle_client_text(shared: &Arc<Shared>, from_agent: &str, text: &str) {
    let frame: ClientFrame = match serde_json::from_str(text) {
        Ok(frame) => frame,
        Err(err) => {
            send_to(
                shared,
                from_agent,
                ServerFrame::Error {
                    code: "bad_frame".to_string(),
                    message: err.to_string(),
                },
            );
            return;
        }
    };

    match frame {
        ClientFrame::Receipt { id } => clear_pending(shared, &id, from_agent),
        ClientFrame::Envelope(envelope) => {
            if let Err(err) = envelope.validate() {
                send_to(
                    shared,
                    from_agent,
                    ServerFrame::Error {
                        code: "bad_frame".to_string(),
                        message: err.to_string(),
                    },
                );
                return;
            }
            handle_envelope(shared, envelope);
        }
    }
}

/// Submission dedup (plan B5): an envelope id already seen by the bus is not
/// re-routed — the submitter just gets its `Receipt{id}` back, confirming
/// the bus already has it.
fn handle_envelope(shared: &Arc<Shared>, envelope: Envelope) {
    let is_duplicate = !shared.seen.lock().unwrap().insert(envelope.id.clone());
    if is_duplicate {
        send_to(
            shared,
            &envelope.from,
            ServerFrame::Receipt {
                id: envelope.id.clone(),
            },
        );
        return;
    }

    for recipient in envelope.to.clone() {
        route_one(shared, &envelope, &recipient);
    }
}

/// Routes `envelope` to a single `recipient` — plan B6/B7. Unknown/`channel:`
/// recipients and CorrGuard-blocked hops are rejected back to the sender;
/// otherwise the envelope is delivered and, if `requires_ack`, tracked for
/// at-least-once redelivery (plan B4).
fn route_one(shared: &Arc<Shared>, envelope: &Envelope, recipient: &str) {
    let is_registered = shared
        .registry
        .lock()
        .unwrap()
        .contains_key(recipient);
    if recipient.starts_with("channel:") || !is_registered {
        reject_unknown_recipient(shared, envelope, recipient);
        return;
    }

    if guarded_kind(envelope.kind) {
        let verdict = shared
            .guard
            .lock()
            .unwrap()
            .record(&envelope.corr, &envelope.from, recipient);
        match verdict {
            Verdict::Accepted => {}
            Verdict::ExceededRounds | Verdict::CycleDetected => {
                send_to(
                    shared,
                    &envelope.from,
                    ServerFrame::Error {
                        code: "loop_blocked".to_string(),
                        message: envelope.corr.clone(),
                    },
                );
                let _ = shared.events.send(BusEvent::LoopBlocked {
                    corr: envelope.corr.clone(),
                });
                return;
            }
            Verdict::InvalidInput => {
                send_to(
                    shared,
                    &envelope.from,
                    ServerFrame::Error {
                        code: "bad_frame".to_string(),
                        message: "corr/from/to must be non-empty for a guarded kind".to_string(),
                    },
                );
                return;
            }
        }
    }

    let delivered = send_to(shared, recipient, ServerFrame::Envelope(envelope.clone()));
    if !delivered {
        // Registered-check above raced with a disconnect; same outcome as
        // never having been registered.
        reject_unknown_recipient(shared, envelope, recipient);
        return;
    }
    let _ = shared.events.send(BusEvent::Delivered {
        id: envelope.id.clone(),
        to: recipient.to_string(),
    });

    if envelope.requires_ack {
        spawn_retry(shared.clone(), envelope.clone(), recipient.to_string());
    }
}

fn reject_unknown_recipient(shared: &Shared, envelope: &Envelope, recipient: &str) {
    send_to(
        shared,
        &envelope.from,
        ServerFrame::Error {
            code: "unknown_recipient".to_string(),
            message: recipient.to_string(),
        },
    );
    let _ = shared.events.send(BusEvent::RejectedUnknownRecipient {
        id: envelope.id.clone(),
        to: recipient.to_string(),
    });
}

/// CorrGuard applies only to the kinds that carry a review round — plan B6.
fn guarded_kind(kind: MessageKind) -> bool {
    matches!(
        kind,
        MessageKind::ChangeRequest | MessageKind::TaskResult | MessageKind::ReviewRequest
    )
}

/// A recipient's `Receipt{id}` clears that (id, recipient) pending entry and
/// forwards a `Receipt{id}` on to the original sender, closing the
/// at-least-once loop — plan B4. No matching entry (already cleared,
/// already failed, or `requires_ack == false`) is a silent no-op.
fn clear_pending(shared: &Arc<Shared>, id: &str, recipient: &str) {
    let key = (id.to_string(), recipient.to_string());
    let entry = shared.pending.lock().unwrap().remove(&key);
    if let Some(entry) = entry {
        send_to(
            shared,
            &entry.envelope.from,
            ServerFrame::Receipt { id: id.to_string() },
        );
        let _ = shared
            .events
            .send(BusEvent::ReceiptCleared { id: id.to_string() });
    }
}

/// Registers the initial delivery as attempt 1 and spawns the background
/// retry loop — plan B4: capped exponential backoff with full jitter,
/// `max_delivery_attempts` total sends, then `delivery_failed` back to the
/// sender.
fn spawn_retry(shared: Arc<Shared>, envelope: Envelope, recipient: String) {
    let key = (envelope.id.clone(), recipient.clone());
    shared.pending.lock().unwrap().insert(
        key.clone(),
        PendingEntry {
            envelope: envelope.clone(),
            attempts: 1,
        },
    );

    tokio::spawn(async move {
        let mut attempt = 1u32;
        while attempt < shared.cfg.max_delivery_attempts {
            let delay = backoff_delay(shared.cfg.retry_base, attempt, shared.cfg.max_delivery_attempts);
            tokio::time::sleep(delay).await;

            let mut pending = shared.pending.lock().unwrap();
            let Some(entry) = pending.get_mut(&key) else {
                return; // acked while we were sleeping
            };
            attempt += 1;
            entry.attempts = attempt;
            drop(pending);

            send_to(&shared, &recipient, ServerFrame::Envelope(envelope.clone()));
            let _ = shared.events.send(BusEvent::Redelivered {
                id: envelope.id.clone(),
                attempt,
            });
        }

        let exhausted = shared.pending.lock().unwrap().remove(&key).is_some();
        if exhausted {
            send_to(
                &shared,
                &envelope.from,
                ServerFrame::Error {
                    code: "delivery_failed".to_string(),
                    message: envelope.id.clone(),
                },
            );
            let _ = shared
                .events
                .send(BusEvent::DeliveryFailed { id: envelope.id.clone() });
        }
    });
}

/// `random(0, min(cap, base * 2^attempt))` full jitter — plan B4. `cap` is
/// not itself a config field; since attempts are bounded by
/// `max_delivery_attempts`, capping at the backoff of the final attempt
/// (`base * 2^max_attempts`) keeps the formula's intent — bound runaway
/// growth — without adding an unspecified knob.
fn backoff_delay(base: Duration, attempt: u32, max_attempts: u32) -> Duration {
    let cap = base.saturating_mul(1u32.checked_shl(max_attempts).unwrap_or(u32::MAX));
    let exp = base.saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX));
    let bound = exp.min(cap);
    if bound.is_zero() {
        return Duration::ZERO;
    }
    let jitter_ms = rand::thread_rng().gen_range(0..=bound.as_millis() as u64);
    Duration::from_millis(jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delay_never_exceeds_cap() {
        let base = Duration::from_millis(10);
        for attempt in 0..6 {
            let delay = backoff_delay(base, attempt, 3);
            assert!(delay <= base.saturating_mul(1 << 3));
        }
    }

    #[test]
    fn guarded_kind_matches_only_review_round_kinds() {
        assert!(guarded_kind(MessageKind::ChangeRequest));
        assert!(guarded_kind(MessageKind::TaskResult));
        assert!(guarded_kind(MessageKind::ReviewRequest));
        assert!(!guarded_kind(MessageKind::TaskAssign));
        assert!(!guarded_kind(MessageKind::TaskAck));
        assert!(!guarded_kind(MessageKind::Question));
    }
}
