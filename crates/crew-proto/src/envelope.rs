use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use ulid::Ulid;

use crate::error::ProtoError;
use crate::kind::MessageKind;

/// Agent-to-agent message envelope — DESIGN.md §3.1. Field names and shape
/// follow the §3.1 JSON example 1:1; this crate carries no I/O.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub ts: String,
    pub sprint: String,
    pub thread: String,
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    pub kind: MessageKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    pub corr: String,
    pub body: Value,
    #[serde(default)]
    pub artifacts: Vec<String>,
    pub requires_ack: bool,
    pub deadline_ms: u64,
}

impl Envelope {
    /// Builds a new envelope, generating `id` (ULID, `msg_`-prefixed) and
    /// `ts` (RFC3339, UTC now) per DESIGN.md §3.1. Every other field is
    /// caller-supplied.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sprint: String,
        thread: String,
        from: String,
        to: Vec<String>,
        kind: MessageKind,
        in_reply_to: Option<String>,
        corr: String,
        body: Value,
        artifacts: Vec<String>,
        requires_ack: bool,
        deadline_ms: u64,
    ) -> Self {
        Self {
            id: format!("msg_{}", Ulid::new()),
            ts: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .expect("RFC3339 formatting of a valid OffsetDateTime cannot fail"),
            sprint,
            thread,
            from,
            to,
            kind,
            in_reply_to,
            corr,
            body,
            artifacts,
            requires_ack,
            deadline_ms,
        }
    }

    /// DESIGN.md §3.2 recipient-obligation table.
    pub fn validate(&self) -> Result<(), ProtoError> {
        if self.id.is_empty() {
            return Err(ProtoError::EmptyId);
        }
        if self.from.is_empty() {
            return Err(ProtoError::EmptyFrom);
        }
        if self.corr.is_empty() {
            return Err(ProtoError::EmptyCorr);
        }
        if self.to.is_empty() {
            return Err(ProtoError::EmptyTo);
        }
        if self.deadline_ms == 0 {
            return Err(ProtoError::InvalidDeadline);
        }
        if self.kind == MessageKind::TaskAssign && !self.requires_ack {
            return Err(ProtoError::TaskAssignRequiresAck);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DESIGN.md §3.1 example, reproduced as valid JSON (the doc's JSONC
    /// inline comments stripped; every value kept verbatim).
    const DOC_EXAMPLE_JSON: &str = r#"{
        "id": "msg_01H...",
        "ts": "2026-08-27T10:00:00Z",
        "sprint": "sp-3",
        "thread": "th-login-ui",
        "from": "agent:designer",
        "to":   ["agent:publisher"],
        "kind": "change_request",
        "in_reply_to": "msg_01H...",
        "corr": "req_01H...",
        "body": {},
        "artifacts": ["art:design/login-v2.png"],
        "requires_ack": true,
        "deadline_ms": 900000
    }"#;

    fn valid_envelope() -> Envelope {
        serde_json::from_str(DOC_EXAMPLE_JSON).unwrap()
    }

    #[test]
    fn deserializes_doc_example_json_verbatim() {
        let env: Envelope = serde_json::from_str(DOC_EXAMPLE_JSON).unwrap();
        assert_eq!(env.id, "msg_01H...");
        assert_eq!(env.ts, "2026-08-27T10:00:00Z");
        assert_eq!(env.sprint, "sp-3");
        assert_eq!(env.thread, "th-login-ui");
        assert_eq!(env.from, "agent:designer");
        assert_eq!(env.to, vec!["agent:publisher".to_string()]);
        assert_eq!(env.kind, MessageKind::ChangeRequest);
        assert_eq!(env.in_reply_to, Some("msg_01H...".to_string()));
        assert_eq!(env.corr, "req_01H...");
        assert_eq!(env.body, serde_json::json!({}));
        assert_eq!(env.artifacts, vec!["art:design/login-v2.png".to_string()]);
        assert!(env.requires_ack);
        assert_eq!(env.deadline_ms, 900000);
    }

    #[test]
    fn to_and_artifacts_default_to_empty_when_absent() {
        let json = r#"{
            "id": "msg_01",
            "ts": "2026-08-27T10:00:00Z",
            "sprint": "sp-3",
            "thread": "th-1",
            "from": "agent:a",
            "kind": "question",
            "corr": "req_01",
            "body": {},
            "requires_ack": false,
            "deadline_ms": 1000
        }"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.to, Vec::<String>::new());
        assert_eq!(env.artifacts, Vec::<String>::new());
        assert_eq!(env.in_reply_to, None);
    }

    #[test]
    fn in_reply_to_none_is_omitted_on_serialize() {
        let mut env = valid_envelope();
        env.in_reply_to = None;
        let serialized = serde_json::to_value(&env).unwrap();
        assert!(!serialized.as_object().unwrap().contains_key("in_reply_to"));
    }

    #[test]
    fn validate_accepts_a_well_formed_envelope() {
        assert_eq!(valid_envelope().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_empty_id() {
        let mut env = valid_envelope();
        env.id = String::new();
        assert_eq!(env.validate(), Err(ProtoError::EmptyId));
    }

    #[test]
    fn validate_rejects_empty_from() {
        let mut env = valid_envelope();
        env.from = String::new();
        assert_eq!(env.validate(), Err(ProtoError::EmptyFrom));
    }

    #[test]
    fn validate_rejects_empty_corr() {
        let mut env = valid_envelope();
        env.corr = String::new();
        assert_eq!(env.validate(), Err(ProtoError::EmptyCorr));
    }

    #[test]
    fn validate_rejects_empty_to() {
        let mut env = valid_envelope();
        env.to = Vec::new();
        assert_eq!(env.validate(), Err(ProtoError::EmptyTo));
    }

    #[test]
    fn validate_rejects_zero_deadline() {
        let mut env = valid_envelope();
        env.deadline_ms = 0;
        assert_eq!(env.validate(), Err(ProtoError::InvalidDeadline));
    }

    #[test]
    fn validate_rejects_task_assign_without_requires_ack() {
        let mut env = valid_envelope();
        env.kind = MessageKind::TaskAssign;
        env.requires_ack = false;
        assert_eq!(env.validate(), Err(ProtoError::TaskAssignRequiresAck));
    }

    #[test]
    fn validate_accepts_task_assign_with_requires_ack() {
        let mut env = valid_envelope();
        env.kind = MessageKind::TaskAssign;
        env.requires_ack = true;
        assert_eq!(env.validate(), Ok(()));
    }

    #[test]
    fn new_generates_id_and_ts() {
        let env = Envelope::new(
            "sp-3".to_string(),
            "th-login-ui".to_string(),
            "agent:designer".to_string(),
            vec!["agent:publisher".to_string()],
            MessageKind::ChangeRequest,
            None,
            "req_01H".to_string(),
            serde_json::json!({}),
            vec![],
            false,
            900000,
        );

        assert!(env.id.starts_with("msg_"));
        assert!(Ulid::from_string(env.id.trim_start_matches("msg_")).is_ok());
        assert!(
            time::OffsetDateTime::parse(&env.ts, &time::format_description::well_known::Rfc3339)
                .is_ok()
        );
        assert_eq!(env.sprint, "sp-3");
        assert_eq!(env.corr, "req_01H");
        assert!(env.validate().is_ok());
    }

    #[test]
    fn new_generates_distinct_ids_across_calls() {
        let env_a = Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:a".to_string(),
            vec!["agent:b".to_string()],
            MessageKind::Question,
            None,
            "req_1".to_string(),
            serde_json::json!({}),
            vec![],
            false,
            1000,
        );
        let env_b = Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:a".to_string(),
            vec!["agent:b".to_string()],
            MessageKind::Question,
            None,
            "req_1".to_string(),
            serde_json::json!({}),
            vec![],
            false,
            1000,
        );
        assert_ne!(env_a.id, env_b.id);
    }
}
