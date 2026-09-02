use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Message kind — DESIGN.md §3.2. Wire strings are the exact table values.
///
/// `PresenceRead`/`PresenceTyping` (t2-be-presence D1) extend the closed
/// vocabulary with a volatile, never-ledgered signal — `requires_ack` is
/// always `false` for these two and `crew-run`'s controller intercepts them
/// before `EventLedger::append` (see `crew-run/src/controller.rs
/// handle_bus_event`'s presence guard). Every existing `match`/`if let` on
/// `MessageKind` across the workspace was swept for this addition
/// (`grep -rn MessageKind crates/ apps/`): every site is either the
/// `matches!` macro or carries an explicit `_ =>` / inequality-check
/// fallback, so all of them already swallow the two new variants safely
/// without needing a per-site edit — e.g. `crew-bus/src/routing.rs
/// guarded_kind` (`matches!`, presence is untouched by CorrGuard, correct:
/// presence carries no review round), `crew-lead/src/dispatch.rs:504`
/// (`_ => vec![]`, correct: presence is not Lead business), `crew-agent`'s
/// `crew_member.rs`/`designer.rs`/`harness_behavior.rs` build_prompt sites
/// (`_ => String::new()`/`vec![]`) and `pm.rs` (`if kind != TaskResult`).
/// Presence emission itself is centralized in `crew-agent/src/runner.rs`
/// (not any individual `RoleBehavior` impl) — see its module doc.
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
    #[serde(rename = "human.response")]
    HumanResponse,
    #[serde(rename = "presence.read")]
    PresenceRead,
    #[serde(rename = "presence.typing")]
    PresenceTyping,
}

/// `presence.read` envelope body (t2-be-presence D1) — serializes directly
/// as the envelope's `body` (no wrapper key). `target_msg_id` is the id of
/// the envelope being marked read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresenceReadBody {
    pub agent_id: String,
    pub target_msg_id: String,
}

/// `presence.typing` envelope body (t2-be-presence D1) — `active` is `true`
/// at CLI-turn start, `false` at turn end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresenceTypingBody {
    pub agent_id: String,
    pub active: bool,
}

/// `presence.read` envelope body constructor — mirrors `handoff::handoff_body`'s
/// build/parse pairing.
pub fn presence_read_body(body: &PresenceReadBody) -> Value {
    serde_json::to_value(body).expect("PresenceReadBody always serializes to JSON")
}

/// Inverse of [`presence_read_body`] — `None` if `body` doesn't deserialize
/// as a `PresenceReadBody`.
pub fn presence_read_from_body(body: &Value) -> Option<PresenceReadBody> {
    serde_json::from_value(body.clone()).ok()
}

/// `presence.typing` envelope body constructor.
pub fn presence_typing_body(body: &PresenceTypingBody) -> Value {
    serde_json::to_value(body).expect("PresenceTypingBody always serializes to JSON")
}

/// Inverse of [`presence_typing_body`] — `None` if `body` doesn't
/// deserialize as a `PresenceTypingBody`.
pub fn presence_typing_from_body(body: &Value) -> Option<PresenceTypingBody> {
    serde_json::from_value(body.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_kinds_and_wire_strings() -> [(MessageKind, &'static str); 14] {
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
            (MessageKind::HumanResponse, "human.response"),
            (MessageKind::PresenceRead, "presence.read"),
            (MessageKind::PresenceTyping, "presence.typing"),
        ]
    }

    #[test]
    fn all_14_kinds_round_trip_their_wire_string() {
        let cases = all_kinds_and_wire_strings();
        assert_eq!(cases.len(), 14);
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

    fn sample_read_body() -> PresenceReadBody {
        PresenceReadBody {
            agent_id: "agent:developer".to_string(),
            target_msg_id: "msg_01H".to_string(),
        }
    }

    fn sample_typing_body(active: bool) -> PresenceTypingBody {
        PresenceTypingBody {
            agent_id: "agent:developer".to_string(),
            active,
        }
    }

    /// Normal: presence.read body round-trips through the wire with no
    /// wrapper key (unlike `handoff_body`'s `{"pack": ...}`).
    #[test]
    fn presence_read_body_round_trips_with_flat_keys() {
        let body = sample_read_body();
        let value = presence_read_body(&body);
        assert_eq!(value["agent_id"], "agent:developer");
        assert_eq!(value["target_msg_id"], "msg_01H");
        assert_eq!(value.as_object().unwrap().len(), 2);

        let back = presence_read_from_body(&value).expect("must round-trip");
        assert_eq!(back, body);
    }

    /// Normal + boundary (active=false): presence.typing body round-trips
    /// for both boolean values.
    #[test]
    fn presence_typing_body_round_trips_true_and_false() {
        for active in [true, false] {
            let body = sample_typing_body(active);
            let value = presence_typing_body(&body);
            assert_eq!(value["active"], active);
            let back = presence_typing_from_body(&value).expect("must round-trip");
            assert_eq!(back, body);
        }
    }

    /// Error: a body missing a required field fails to parse (returns
    /// `None`, matching `handoff_pack_from_body`'s Option contract).
    #[test]
    fn presence_read_from_body_returns_none_when_target_msg_id_missing() {
        let malformed = serde_json::json!({ "agent_id": "agent:developer" });
        assert_eq!(presence_read_from_body(&malformed), None);
    }

    /// Error/boundary: an empty body object parses as neither presence
    /// body shape.
    #[test]
    fn empty_body_parses_as_neither_presence_shape() {
        let empty = serde_json::json!({});
        assert_eq!(presence_read_from_body(&empty), None);
        assert_eq!(presence_typing_from_body(&empty), None);
    }
}
