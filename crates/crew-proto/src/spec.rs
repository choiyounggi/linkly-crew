use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors constructing spec.rs types — DESIGN.md §4.2 "요구사항에 `REQ-1` 식 ID 부여".
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SpecError {
    #[error("requirement id must be non-empty and start with \"REQ-\", got {0:?}")]
    InvalidReqId(String),
}

/// Requirement identifier — DESIGN.md §4.2 "요구사항에 `REQ-1` 식 ID 부여".
/// Always non-empty and `REQ-`-prefixed; construct via `ReqId::new`, which is
/// also what deserialization runs under the hood (`try_from = "String"`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReqId(String);

impl ReqId {
    pub fn new(id: impl Into<String>) -> Result<Self, SpecError> {
        let id = id.into();
        if id.is_empty() || !id.starts_with("REQ-") {
            return Err(SpecError::InvalidReqId(id));
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ReqId {
    type Error = SpecError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ReqId> for String {
    fn from(value: ReqId) -> Self {
        value.0
    }
}

/// One requirement — DESIGN.md §4.1① "목표/비목표/제약/수락기준".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    pub id: ReqId,
    pub text: String,
}

/// PM's spec output — DESIGN.md §4.1① "① Lead: 스펙화(Spec) — 목표/비목표/제약/수락기준".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecDoc {
    pub goal: String,
    pub non_goals: Vec<String>,
    pub constraints: Vec<String>,
    pub requirements: Vec<Requirement>,
    pub acceptance: Vec<String>,
}

/// One artifact's coverage contract — DESIGN.md §4.2 "핵심 장치: 모든 산출물이
/// `REQ-id`를 참조한다".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactContract {
    pub name: String,
    pub kind: String,
    pub req_ids: Vec<ReqId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_id_accepts_a_well_formed_id() {
        let id = ReqId::new("REQ-1").unwrap();
        assert_eq!(id.as_str(), "REQ-1");
    }

    #[test]
    fn req_id_rejects_empty_string() {
        assert_eq!(
            ReqId::new(""),
            Err(SpecError::InvalidReqId(String::new()))
        );
    }

    #[test]
    fn req_id_rejects_missing_prefix() {
        assert_eq!(
            ReqId::new("1"),
            Err(SpecError::InvalidReqId("1".to_string()))
        );
    }

    #[test]
    fn req_id_round_trips_through_serde() {
        let id = ReqId::new("REQ-3").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"REQ-3\"");
        let back: ReqId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn req_id_deserialize_rejects_invalid_wire_value() {
        let result: Result<ReqId, _> = serde_json::from_str("\"not-a-req-id\"");
        assert!(result.is_err());
    }

    #[test]
    fn spec_doc_round_trips_with_requirements() {
        let doc = SpecDoc {
            goal: "회원가입/로그인 되는 랜딩 페이지".to_string(),
            non_goals: vec!["결제".to_string()],
            constraints: vec!["모바일 반응형".to_string()],
            requirements: vec![Requirement {
                id: ReqId::new("REQ-1").unwrap(),
                text: "회원가입".to_string(),
            }],
            acceptance: vec!["QA 통과".to_string()],
        };
        let json = serde_json::to_string(&doc).unwrap();
        let back: SpecDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn spec_doc_accepts_all_empty_lists_as_boundary() {
        let doc = SpecDoc {
            goal: "빈 스펙".to_string(),
            non_goals: vec![],
            constraints: vec![],
            requirements: vec![],
            acceptance: vec![],
        };
        let json = serde_json::to_string(&doc).unwrap();
        let back: SpecDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(back, doc);
        assert!(back.requirements.is_empty());
    }

    #[test]
    fn artifact_contract_round_trips_and_references_req_ids() {
        let contract = ArtifactContract {
            name: "design.md".to_string(),
            kind: "doc".to_string(),
            req_ids: vec![ReqId::new("REQ-1").unwrap(), ReqId::new("REQ-3").unwrap()],
        };
        let json = serde_json::to_value(&contract).unwrap();
        assert_eq!(json["req_ids"], serde_json::json!(["REQ-1", "REQ-3"]));
        let back: ArtifactContract = serde_json::from_value(json).unwrap();
        assert_eq!(back, contract);
    }

    #[test]
    fn artifact_contract_accepts_no_req_ids_as_boundary() {
        let contract = ArtifactContract {
            name: "empty.md".to_string(),
            kind: "doc".to_string(),
            req_ids: vec![],
        };
        let json = serde_json::to_string(&contract).unwrap();
        let back: ArtifactContract = serde_json::from_str(&json).unwrap();
        assert_eq!(back.req_ids, Vec::new());
    }
}
