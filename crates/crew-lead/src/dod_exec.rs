//! Lead's DoD judgment (plan D2, contracts-m3.md 커버리지 규칙): a pure
//! function over an accepted `TaskSpec` and a `task.result` body — Lead
//! executes DoD itself rather than trusting the worker's self-report
//! (DESIGN.md §4.2).

use crew_proto::{DodCheck, TaskSpec};
use serde_json::Value;

/// Judgment result — contracts-m3.md 커버리지 규칙: `passed` iff `uncovered`
/// and `missing_artifacts` are both empty; `Cmd`/`Browser` checks never
/// affect `passed` in M3, they only land in `skipped`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DodVerdict {
    pub passed: bool,
    pub uncovered: Vec<String>,
    pub missing_artifacts: Vec<String>,
    pub skipped: Vec<String>,
}

fn artifact_present(artifacts: &[Value], name: &str) -> bool {
    artifacts.iter().any(|a| {
        a.get("name").and_then(Value::as_str) == Some(name)
            && a.get("content")
                .and_then(Value::as_str)
                .is_some_and(|c| !c.is_empty())
    })
}

/// Executes a task's DoD against a `task.result` body (contracts-m3.md
/// verbatim): `ReqCover` checks `body["covered_req_ids"]` (defensive parse —
/// a missing/non-array field counts as no coverage) against the union of
/// every `ReqCover.ids` in `task.dod`; artifact presence (`name` match +
/// non-empty `content` in `body["artifacts"]`) is checked for every
/// `task.artifacts_expected` entry and every `DodCheck::Artifact{name}` in
/// `task.dod` (plan D2 — the two sources are unioned, deduped by name, since
/// the M3 planner (`LeadPlanner::plan_dag`) always populates
/// `artifacts_expected` but never emits a `DodCheck::Artifact`); `Cmd`/
/// `Browser` are not executed in M3 and are recorded in `skipped` only.
pub fn judge(task: &TaskSpec, result_body: &Value) -> DodVerdict {
    let covered: Vec<&str> = match result_body.get("covered_req_ids") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    let artifacts: &[Value] = match result_body.get("artifacts") {
        Some(Value::Array(items)) => items,
        _ => &[],
    };

    let mut uncovered = Vec::new();
    let mut missing_artifacts = Vec::new();
    let mut skipped = Vec::new();

    for check in &task.dod {
        match check {
            DodCheck::ReqCover { ids } => {
                for id in ids {
                    let id = id.as_str();
                    if !covered.contains(&id) && !uncovered.iter().any(|u| u == id) {
                        uncovered.push(id.to_string());
                    }
                }
            }
            DodCheck::Artifact { name } => {
                if !artifact_present(artifacts, name) && !missing_artifacts.iter().any(|m| m == name)
                {
                    missing_artifacts.push(name.clone());
                }
            }
            DodCheck::Cmd { run, .. } => skipped.push(format!("cmd:{run}")),
            DodCheck::Browser { flow, .. } => skipped.push(format!("browser:{flow}")),
        }
    }

    for contract in &task.artifacts_expected {
        if !artifact_present(artifacts, &contract.name)
            && !missing_artifacts.iter().any(|m| m == &contract.name)
        {
            missing_artifacts.push(contract.name.clone());
        }
    }

    let passed = uncovered.is_empty() && missing_artifacts.is_empty();
    DodVerdict {
        passed,
        uncovered,
        missing_artifacts,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use crew_proto::{ArtifactContract, DodCheck, ReqId, Role, TaskSpec};
    use serde_json::json;

    fn task(dod: Vec<DodCheck>, artifacts_expected: Vec<ArtifactContract>) -> TaskSpec {
        TaskSpec {
            id: "t1".to_string(),
            role: Role::Pm,
            title: "title".to_string(),
            brief: "brief".to_string(),
            dod,
            deps: vec![],
            artifacts_expected,
        }
    }

    fn req(id: &str) -> ReqId {
        ReqId::new(id).unwrap()
    }

    #[test]
    fn full_coverage_and_artifact_passes() {
        let t = task(
            vec![DodCheck::ReqCover {
                ids: vec![req("REQ-1"), req("REQ-2")],
            }],
            vec![ArtifactContract {
                name: "spec.md".to_string(),
                kind: "doc".to_string(),
                req_ids: vec![req("REQ-1"), req("REQ-2")],
            }],
        );
        let body = json!({
            "covered_req_ids": ["REQ-1", "REQ-2"],
            "artifacts": [{"name": "spec.md", "kind": "doc", "req_ids": ["REQ-1", "REQ-2"], "content": "hello"}]
        });

        let verdict = super::judge(&t, &body);

        assert!(verdict.passed);
        assert!(verdict.uncovered.is_empty());
        assert!(verdict.missing_artifacts.is_empty());
    }

    #[test]
    fn detects_uncovered_req_ids() {
        let t = task(
            vec![DodCheck::ReqCover {
                ids: vec![req("REQ-1"), req("REQ-2")],
            }],
            vec![],
        );
        let body = json!({"covered_req_ids": ["REQ-1"], "artifacts": []});

        let verdict = super::judge(&t, &body);

        assert!(!verdict.passed);
        assert_eq!(verdict.uncovered, vec!["REQ-2".to_string()]);
    }

    #[test]
    fn detects_missing_and_empty_content_artifacts() {
        let t = task(
            vec![],
            vec![
                ArtifactContract {
                    name: "spec.md".to_string(),
                    kind: "doc".to_string(),
                    req_ids: vec![],
                },
                ArtifactContract {
                    name: "design.md".to_string(),
                    kind: "doc".to_string(),
                    req_ids: vec![],
                },
            ],
        );
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [{"name": "design.md", "kind": "doc", "req_ids": [], "content": ""}]
        });

        let verdict = super::judge(&t, &body);

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["spec.md".to_string(), "design.md".to_string()]
        );
    }

    #[test]
    fn cmd_and_browser_are_skipped_and_do_not_affect_passed() {
        let t = task(
            vec![
                DodCheck::Cmd {
                    run: "npm test".to_string(),
                    expect: "exit 0".to_string(),
                },
                DodCheck::Browser {
                    flow: "signup".to_string(),
                    expect: "success".to_string(),
                },
            ],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});

        let verdict = super::judge(&t, &body);

        assert!(verdict.passed);
        assert_eq!(
            verdict.skipped,
            vec!["cmd:npm test".to_string(), "browser:signup".to_string()]
        );
    }

    #[test]
    fn non_array_covered_req_ids_is_treated_as_no_coverage() {
        let t = task(
            vec![DodCheck::ReqCover {
                ids: vec![req("REQ-1")],
            }],
            vec![],
        );
        let body = json!({"covered_req_ids": "REQ-1", "artifacts": []});

        let verdict = super::judge(&t, &body);

        assert!(!verdict.passed);
        assert_eq!(verdict.uncovered, vec!["REQ-1".to_string()]);
    }
}
