/// Observer events emitted for every state-changing action the bus takes —
/// plan B9. Broadcast to any subscriber via `BusHandle::subscribe()`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BusEvent {
    Registered { agent_id: String },
    Unregistered { agent_id: String },
    Delivered { id: String, to: String },
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
