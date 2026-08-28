use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::dag::Role;

/// DESIGN §5 핸드오프 팩 — 하네스 교체/세션 재시작 시 주입되는 컨텍스트
/// (contracts-m5.md C1, verbatim).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HandoffPack {
    pub role: Role,
    /// 스펙 goal 요약 또는 참조.
    pub spec_ref: String,
    /// 수락된 task id.
    pub done: Vec<String>,
    /// 진행 중 task id.
    pub in_flight: Vec<String>,
    pub decisions: Vec<String>,
    pub open_questions: Vec<String>,
    /// 하네스 스냅샷 notes 등 자유 텍스트.
    pub notes: String,
}

/// `handoff` 봉투 body 계약: `{"pack": <HandoffPack serde_json>}`
/// (contracts-m5.md C1).
pub fn handoff_body(pack: &HandoffPack) -> Value {
    json!({ "pack": pack })
}

/// Inverse of [`handoff_body`] — `None` if `body` carries no `"pack"` key
/// or its value doesn't deserialize as a `HandoffPack`.
pub fn handoff_pack_from_body(body: &Value) -> Option<HandoffPack> {
    body.get("pack")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pack() -> HandoffPack {
        HandoffPack {
            role: Role::Developer,
            spec_ref: "spec-1".to_string(),
            done: vec!["t1".to_string(), "t2".to_string()],
            in_flight: vec!["t3".to_string()],
            decisions: vec!["use postgres".to_string()],
            open_questions: vec!["cache ttl?".to_string()],
            notes: "handoff notes".to_string(),
        }
    }

    #[test]
    fn round_trips_through_serde_with_snake_case_keys() {
        let pack = sample_pack();
        let json = serde_json::to_value(&pack).unwrap();
        assert_eq!(json["spec_ref"], serde_json::json!("spec-1"));
        assert_eq!(json["in_flight"], serde_json::json!(["t3"]));
        let back: HandoffPack = serde_json::from_value(json).unwrap();
        assert_eq!(back, pack);
    }

    #[test]
    fn empty_vectors_and_strings_round_trip_as_boundary() {
        let pack = HandoffPack {
            role: Role::Qa,
            spec_ref: String::new(),
            done: vec![],
            in_flight: vec![],
            decisions: vec![],
            open_questions: vec![],
            notes: String::new(),
        };
        let json = serde_json::to_string(&pack).unwrap();
        let back: HandoffPack = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pack);
    }

    #[test]
    fn missing_required_field_fails_to_deserialize() {
        let json = r#"{"role":"developer","spec_ref":"s","done":[],"in_flight":[],"decisions":[],"open_questions":[]}"#;
        let err = serde_json::from_str::<HandoffPack>(json).unwrap_err();
        assert!(err.to_string().contains("notes"));
    }

    #[test]
    fn handoff_body_wraps_pack_under_pack_key() {
        let pack = sample_pack();
        let body = handoff_body(&pack);
        assert_eq!(body.as_object().unwrap().len(), 1);
        let back = handoff_pack_from_body(&body).expect("must round-trip");
        assert_eq!(back, pack);
    }

    #[test]
    fn handoff_pack_from_body_returns_none_when_pack_key_absent() {
        assert_eq!(handoff_pack_from_body(&serde_json::json!({})), None);
    }
}
