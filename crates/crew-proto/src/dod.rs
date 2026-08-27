use serde::{Deserialize, Serialize};

use crate::spec::ReqId;

/// Definition-of-done check — DESIGN.md §4.2 "DoD가 기계 검증 가능해진다" JSON
/// example. Field names and `kind` wire values match that example 1:1;
/// `Artifact` is an M3 addition (verifies an expected artifact exists at
/// acceptance time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DodCheck {
    Cmd { run: String, expect: String },
    ReqCover { ids: Vec<ReqId> },
    Browser { flow: String, expect: String },
    Artifact { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DESIGN.md §4.2 `task.dod` example, reproduced as a valid JSON array
    /// (the doc's `task.dod = ...` JS assignment and trailing `//` comments
    /// stripped; every value kept verbatim).
    const DOC_EXAMPLE_JSON: &str = r#"[
        { "kind":"cmd",      "run":"npm test",  "expect":"exit 0" },
        { "kind":"cmd",      "run":"npm run build", "expect":"exit 0" },
        { "kind":"req_cover","ids":["REQ-1","REQ-3"] },
        { "kind":"browser",  "flow":"signup", "expect":"성공 토스트 노출" }
    ]"#;

    #[test]
    fn deserializes_doc_example_json_verbatim() {
        let checks: Vec<DodCheck> = serde_json::from_str(DOC_EXAMPLE_JSON).unwrap();
        assert_eq!(
            checks,
            vec![
                DodCheck::Cmd {
                    run: "npm test".to_string(),
                    expect: "exit 0".to_string(),
                },
                DodCheck::Cmd {
                    run: "npm run build".to_string(),
                    expect: "exit 0".to_string(),
                },
                DodCheck::ReqCover {
                    ids: vec![ReqId::new("REQ-1").unwrap(), ReqId::new("REQ-3").unwrap()],
                },
                DodCheck::Browser {
                    flow: "signup".to_string(),
                    expect: "성공 토스트 노출".to_string(),
                },
            ]
        );
    }

    #[test]
    fn cmd_round_trips_with_kind_tag() {
        let check = DodCheck::Cmd {
            run: "npm test".to_string(),
            expect: "exit 0".to_string(),
        };
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "cmd", "run": "npm test", "expect": "exit 0"})
        );
        let back: DodCheck = serde_json::from_value(json).unwrap();
        assert_eq!(back, check);
    }

    #[test]
    fn artifact_variant_round_trips() {
        let check = DodCheck::Artifact {
            name: "design.md".to_string(),
        };
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "artifact", "name": "design.md"}));
        let back: DodCheck = serde_json::from_value(json).unwrap();
        assert_eq!(back, check);
    }

    #[test]
    fn unknown_kind_fails_to_deserialize() {
        let result: Result<DodCheck, _> =
            serde_json::from_str(r#"{"kind":"not_a_kind","run":"x","expect":"y"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn req_cover_with_invalid_req_id_fails_to_deserialize() {
        let result: Result<DodCheck, _> =
            serde_json::from_str(r#"{"kind":"req_cover","ids":["not-a-req-id"]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn req_cover_accepts_empty_ids_as_boundary() {
        let check = DodCheck::ReqCover { ids: vec![] };
        let json = serde_json::to_string(&check).unwrap();
        let back: DodCheck = serde_json::from_str(&json).unwrap();
        assert_eq!(back, DodCheck::ReqCover { ids: vec![] });
    }
}
