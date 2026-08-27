use thiserror::Error;

/// Envelope validation failures — DESIGN.md §3.2 recipient-obligation table.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("id must not be empty")]
    EmptyId,
    #[error("from must not be empty")]
    EmptyFrom,
    #[error("corr must not be empty")]
    EmptyCorr,
    #[error("to must not be empty")]
    EmptyTo,
    #[error("deadline_ms must be greater than 0")]
    InvalidDeadline,
    #[error("task.assign requires requires_ack = true")]
    TaskAssignRequiresAck,
}
