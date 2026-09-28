//! `task.result` 본문의 산출물 계약 — issue #31 "원하는 완료의 정의 2"
//! (산출물이 디스크에 실재하는 파일의 경로를 가리켜야 한다).
//!
//! DESIGN.md §4.2 artifact contract의 RESULT side. EXPECTED side는
//! [`crate::spec::ArtifactContract`]이고, 이 모듈은 에이전트가 실제로 돌려준
//! 쪽을 파싱한다 — 그래서 `path`/`content`가 모두 optional이다.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ProtoError, ReqId};

/// One artifact as reported in a `task.result` body's `artifacts` array.
///
/// `name`/`kind` mirror [`crate::spec::ArtifactContract`] and stay required.
/// `path` is the issue #31 addition (the on-disk location inside the role
/// worktree); `content` is the pre-#31 inline shape and still parses, so a
/// producer we cannot force-upgrade — the model — keeps working. Unknown
/// fields are ignored on purpose (no `deny_unknown_fields`): a model adding a
/// field must not cost us the whole entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResultArtifact {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub req_ids: Vec<ReqId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Read the `artifacts` array out of a `task.result` body.
///
/// A missing `artifacts` key, or one whose value is not an array, yields an
/// empty vec — not an error: the body is untyped `Value` and "reported no
/// artifact" is a legitimate result the judge handles.
///
/// Every entry is parsed on its own; one that does not deserialize (no
/// `name`/`kind`, a req id that is not `REQ-`-prefixed, a non-object) is
/// DROPPED while its valid siblings survive. Dropping is fail-closed: the
/// dropped entry's name is then absent, so the Lead's judge reports that
/// artifact missing instead of accepting an entry it could not read. Never
/// panics.
pub fn task_result_artifacts_from_body(body: &Value) -> Vec<TaskResultArtifact> {
    let Some(entries) = body.get("artifacts").and_then(Value::as_array) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
        .collect()
}

impl TaskResultArtifact {
    /// The reported `path`, if it is safe to resolve inside a role worktree.
    ///
    /// Accepts only a non-empty RELATIVE path: rejects an absent `path`
    /// ([`ProtoError::ArtifactPathMissing`]), and rejects with
    /// [`ProtoError::InvalidArtifactPath`] an empty string, a leading `/`, any
    /// `..` (or root/prefix) component, and a path with no real component at
    /// all (`.`, `./`). A `.` component elsewhere is tolerated (`./a.txt` is
    /// fine), and a name that merely starts with dots is not a parent
    /// reference (`..foo/x` is fine). Returns the ORIGINAL string, unchanged.
    ///
    /// This is a LEXICAL check only. The consumer owns the base directory, so
    /// it must still canonicalize the path against the role worktree and
    /// verify the resolved path stays under it (symlinks are not visible here).
    pub fn validated_path(&self) -> Result<&str, ProtoError> {
        let Some(p) = self.path.as_deref() else {
            return Err(ProtoError::ArtifactPathMissing);
        };
        let invalid = || ProtoError::InvalidArtifactPath(p.to_string());
        if p.is_empty() || p.starts_with('/') {
            return Err(invalid());
        }
        let mut has_normal = false;
        for component in std::path::Path::new(p).components() {
            match component {
                std::path::Component::Normal(_) => has_normal = true,
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_) => return Err(invalid()),
            }
        }
        if !has_normal {
            return Err(invalid());
        }
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(id: &str) -> ReqId {
        ReqId::new(id).expect("test req id must be well formed")
    }

    fn artifact_with_path(path: Option<&str>) -> TaskResultArtifact {
        TaskResultArtifact {
            name: "a".to_string(),
            kind: "code".to_string(),
            req_ids: vec![],
            path: path.map(str::to_string),
            content: None,
        }
    }

    #[test]
    fn parses_full_entry_with_path() {
        let body = json!({
            "artifacts": [
                { "name": "index.html", "kind": "code", "req_ids": ["REQ-1"], "path": "web/index.html" }
            ]
        });
        assert_eq!(
            task_result_artifacts_from_body(&body),
            vec![TaskResultArtifact {
                name: "index.html".to_string(),
                kind: "code".to_string(),
                req_ids: vec![req("REQ-1")],
                path: Some("web/index.html".to_string()),
                content: None,
            }]
        );
    }

    #[test]
    fn parses_content_only_entry_for_wire_compat() {
        let body = json!({ "artifacts": [{ "name": "a", "kind": "doc", "content": "x" }] });
        let got = task_result_artifacts_from_body(&body);
        assert_eq!(
            got,
            vec![TaskResultArtifact {
                name: "a".to_string(),
                kind: "doc".to_string(),
                req_ids: vec![],
                path: None,
                content: Some("x".to_string()),
            }]
        );
    }

    #[test]
    fn missing_artifacts_key_yields_empty() {
        assert_eq!(task_result_artifacts_from_body(&json!({})), vec![]);
    }

    #[test]
    fn non_array_artifacts_yields_empty() {
        assert_eq!(
            task_result_artifacts_from_body(&json!({ "artifacts": "x" })),
            vec![]
        );
        assert_eq!(
            task_result_artifacts_from_body(&json!({ "artifacts": { "name": "a" } })),
            vec![]
        );
    }

    #[test]
    fn empty_artifacts_array_yields_empty() {
        assert_eq!(
            task_result_artifacts_from_body(&json!({ "artifacts": [] })),
            vec![]
        );
    }

    #[test]
    fn malformed_entry_is_dropped_and_valid_sibling_kept() {
        let body = json!({
            "artifacts": [
                { "name": "good", "kind": "code", "path": "src/lib.rs" },
                { "kind": "x" },
                { "name": "b", "kind": "k", "req_ids": ["FOO"] },
                7,
                { "name": "also-good", "kind": "doc", "content": "y" }
            ]
        });
        assert_eq!(
            task_result_artifacts_from_body(&body),
            vec![
                TaskResultArtifact {
                    name: "good".to_string(),
                    kind: "code".to_string(),
                    req_ids: vec![],
                    path: Some("src/lib.rs".to_string()),
                    content: None,
                },
                TaskResultArtifact {
                    name: "also-good".to_string(),
                    kind: "doc".to_string(),
                    req_ids: vec![],
                    path: None,
                    content: Some("y".to_string()),
                },
            ]
        );
    }

    #[test]
    fn unknown_entry_fields_are_tolerated() {
        let body = json!({
            "artifacts": [{
                "name": "index.html",
                "kind": "code",
                "path": "web/index.html",
                "sha256": "not-a-field-we-know",
                "bytes": 12
            }]
        });
        assert_eq!(
            task_result_artifacts_from_body(&body),
            vec![TaskResultArtifact {
                name: "index.html".to_string(),
                kind: "code".to_string(),
                req_ids: vec![],
                path: Some("web/index.html".to_string()),
                content: None,
            }]
        );
    }

    #[test]
    fn content_only_entry_serializes_without_path_key() {
        let artifact = TaskResultArtifact {
            name: "a".to_string(),
            kind: "doc".to_string(),
            req_ids: vec![],
            path: None,
            content: Some("x".to_string()),
        };
        let value = serde_json::to_value(&artifact).expect("serializes");
        assert!(value.get("path").is_none(), "got {value}");
        assert_eq!(value["content"], json!("x"));
    }

    #[test]
    fn validated_path_accepts_relative_paths() {
        for p in ["a/b.rs", "./a.txt", "..foo/x", "a//b"] {
            assert_eq!(artifact_with_path(Some(p)).validated_path(), Ok(p));
        }
    }

    #[test]
    fn validated_path_rejects_parent_components() {
        for p in ["../x", "a/../b"] {
            assert_eq!(
                artifact_with_path(Some(p)).validated_path(),
                Err(ProtoError::InvalidArtifactPath(p.to_string()))
            );
        }
    }

    #[test]
    fn validated_path_rejects_absolute_path() {
        assert_eq!(
            artifact_with_path(Some("/abs")).validated_path(),
            Err(ProtoError::InvalidArtifactPath("/abs".to_string()))
        );
    }

    #[test]
    fn validated_path_rejects_empty_and_dot_only() {
        for p in ["", ".", "./"] {
            assert_eq!(
                artifact_with_path(Some(p)).validated_path(),
                Err(ProtoError::InvalidArtifactPath(p.to_string()))
            );
        }
    }

    #[test]
    fn validated_path_without_path_is_missing() {
        assert_eq!(
            artifact_with_path(None).validated_path(),
            Err(ProtoError::ArtifactPathMissing)
        );
    }
}
