/// Observer events emitted for every state-changing action the bus takes —
/// plan B9. Broadcast to any subscriber via `BusHandle::subscribe()`.
// Note: `Eq` (previously derived) is dropped here because `crew_proto::Envelope`
// (out of scope for this task) only implements `PartialEq`, via its `Value`
// body field. `PartialEq` still holds and is all existing tests rely on.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum BusEvent {
    Registered { agent_id: String },
    Unregistered { agent_id: String },
    Delivered { id: String, to: String },
    /// Emitted exactly once, right after a submitted envelope passes
    /// spoof-rejection and seen-dedup — before delivery is attempted.
    /// Redelivery does not re-emit this. Acceptance is independent of
    /// delivery outcome: an envelope with an unknown recipient is still
    /// accepted (contract §C1).
    EnvelopeAccepted { envelope: crew_proto::Envelope },
    ReceiptCleared { id: String },
    Redelivered { id: String, attempt: u32 },
    DeliveryFailed { id: String },
    LoopBlocked { corr: String },
    RejectedUnknownRecipient { id: String, to: String },
    SpoofRejected {
        id: String,
        claimed_from: String,
        agent_id: String,
    },
}
