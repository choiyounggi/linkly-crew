use serde::{Deserialize, Serialize};

/// Message kind — DESIGN.md §3.2. Wire strings are the exact table values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    #[serde(rename = "task.assign")]
    TaskAssign,
    #[serde(rename = "task.ack")]
    TaskAck,
    #[serde(rename = "task.progress")]
    TaskProgress,
    #[serde(rename = "task.result")]
    TaskResult,
    #[serde(rename = "review.request")]
    ReviewRequest,
    #[serde(rename = "change_request")]
    ChangeRequest,
    #[serde(rename = "question")]
    Question,
    #[serde(rename = "answer")]
    Answer,
    #[serde(rename = "blocked")]
    Blocked,
    #[serde(rename = "handoff")]
    Handoff,
    #[serde(rename = "human.gate")]
    HumanGate,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_kinds_and_wire_strings() -> [(MessageKind, &'static str); 11] {
        [
            (MessageKind::TaskAssign, "task.assign"),
            (MessageKind::TaskAck, "task.ack"),
            (MessageKind::TaskProgress, "task.progress"),
            (MessageKind::TaskResult, "task.result"),
            (MessageKind::ReviewRequest, "review.request"),
            (MessageKind::ChangeRequest, "change_request"),
            (MessageKind::Question, "question"),
            (MessageKind::Answer, "answer"),
            (MessageKind::Blocked, "blocked"),
            (MessageKind::Handoff, "handoff"),
            (MessageKind::HumanGate, "human.gate"),
        ]
    }

    #[test]
    fn all_11_kinds_round_trip_their_wire_string() {
        let cases = all_kinds_and_wire_strings();
        assert_eq!(cases.len(), 11);
        for (kind, wire) in cases {
            let serialized = serde_json::to_string(&kind).unwrap();
            assert_eq!(serialized, format!("\"{wire}\""));

            let deserialized: MessageKind = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, kind);
        }
    }

    #[test]
    fn unknown_wire_string_fails_to_deserialize() {
        let result: Result<MessageKind, _> = serde_json::from_str("\"not.a.kind\"");
        assert!(result.is_err());
    }
}
