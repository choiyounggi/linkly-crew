//! Lead's DoD judgment (plan D2, contracts-m3.md 커버리지 규칙): a pure
//! function over an accepted `TaskSpec` and a `task.result` body — Lead
//! executes DoD itself rather than trusting the worker's self-report
//! (DESIGN.md §4.2).

use crate::cmd_exec::{parse_expect, CmdOutcome};
use crew_proto::{DodCheck, TaskSpec};
use serde_json::Value;

/// Judgment result — contracts-m3.md 커버리지 규칙 as extended by M9 §G2d:
/// `passed` iff `uncovered`, `missing_artifacts`, and `failed_cmds` are all
/// empty. `Browser` checks never affect `passed`, they only land in
/// `skipped` (M9 scope-out, G0). A `Cmd` check lands in `skipped` when no
/// matching `CmdOutcome` was supplied (executor not wired, or `expect`
/// unparseable) — same as M3 — and in `failed_cmds` when the matching
/// outcome disagrees with `expect`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DodVerdict {
    pub passed: bool,
    pub uncovered: Vec<String>,
    pub missing_artifacts: Vec<String>,
    pub failed_cmds: Vec<String>,
    pub skipped: Vec<String>,
}

/// The `run` field shared by every `CmdOutcome` variant (M9 D12 matching key).
fn outcome_run(outcome: &CmdOutcome) -> &str {
    match outcome {
        CmdOutcome::Ran { run, .. }
        | CmdOutcome::Refused { run, .. }
        | CmdOutcome::TimedOut { run }
        | CmdOutcome::SpawnFailed { run, .. } => run,
    }
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
/// `artifacts_expected` but never emits a `DodCheck::Artifact`); `Cmd` checks
/// are matched against `cmd_outcomes` by `run` (first unused match wins, M9
/// D12) — a match whose `Ran{exit_code}` equals `parse_expect(expect)` passes
/// silently, a mismatched exit code or a `Refused`/`TimedOut`/`SpawnFailed`
/// outcome lands in `failed_cmds` (`cmd:<run> — <reason>`) and fails
/// `passed`, and no match (executor not wired, or `expect` unparseable)
/// lands in `skipped` only — unchanged from M3 (M9 §G2d, D11 regression
/// invariant: an empty `cmd_outcomes` reproduces M3 exactly). `Browser` is
/// never executed (M9 scope-out, G0) and is always recorded in `skipped`
/// only, same as M3.
pub fn judge(task: &TaskSpec, result_body: &Value, cmd_outcomes: &[CmdOutcome]) -> DodVerdict {
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
    let mut failed_cmds = Vec::new();
    let mut skipped = Vec::new();
    let mut used_outcome = vec![false; cmd_outcomes.len()];

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
            DodCheck::Cmd { run, expect } => {
                let Some(want) = parse_expect(expect) else {
                    skipped.push(format!("cmd:{run}"));
                    continue;
                };
                let matched = cmd_outcomes.iter().enumerate().find(|(i, outcome)| {
                    !used_outcome[*i] && outcome_run(outcome) == run.as_str()
                });
                let Some((i, outcome)) = matched else {
                    skipped.push(format!("cmd:{run}"));
                    continue;
                };
                used_outcome[i] = true;
                match outcome {
                    CmdOutcome::Ran { exit_code, .. } if *exit_code == want => {}
                    CmdOutcome::Ran { exit_code, .. } => {
                        failed_cmds.push(format!("cmd:{run} — exit {exit_code} (expected {want})"));
                    }
                    CmdOutcome::Refused { reason, .. } => {
                        failed_cmds.push(format!("cmd:{run} — refused: {reason}"));
                    }
                    CmdOutcome::TimedOut { .. } => {
                        failed_cmds.push(format!("cmd:{run} — timed out"));
                    }
                    CmdOutcome::SpawnFailed { error, .. } => {
                        failed_cmds.push(format!("cmd:{run} — spawn failed: {error}"));
                    }
                }
            }
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

    let passed = uncovered.is_empty() && missing_artifacts.is_empty() && failed_cmds.is_empty();
    DodVerdict {
        passed,
        uncovered,
        missing_artifacts,
        failed_cmds,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use crate::cmd_exec::CmdOutcome;
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

        let verdict = super::judge(&t, &body, &[]);

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

        let verdict = super::judge(&t, &body, &[]);

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

        let verdict = super::judge(&t, &body, &[]);

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

        let verdict = super::judge(&t, &body, &[]);

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

        let verdict = super::judge(&t, &body, &[]);

        assert!(!verdict.passed);
        assert_eq!(verdict.uncovered, vec!["REQ-1".to_string()]);
    }

    fn cmd_check(run: &str, expect: &str) -> DodCheck {
        DodCheck::Cmd {
            run: run.to_string(),
            expect: expect.to_string(),
        }
    }

    #[test]
    fn matching_exit_code_passes_and_is_not_skipped_or_failed() {
        let t = task(vec![cmd_check("npm test", "exit 0")], vec![]);
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![CmdOutcome::Ran {
            run: "npm test".to_string(),
            exit_code: 0,
        }];

        let verdict = super::judge(&t, &body, &outcomes);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert!(!verdict.skipped.contains(&"cmd:npm test".to_string()));
    }

    #[test]
    fn mismatched_exit_code_fails() {
        let t = task(vec![cmd_check("npm test", "exit 0")], vec![]);
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![CmdOutcome::Ran {
            run: "npm test".to_string(),
            exit_code: 1,
        }];

        let verdict = super::judge(&t, &body, &outcomes);

        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
    }

    #[test]
    fn refused_outcome_fails() {
        let t = task(vec![cmd_check("rm -rf /", "exit 0")], vec![]);
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![CmdOutcome::Refused {
            run: "rm -rf /".to_string(),
            reason: "matches no allowed prefix".to_string(),
        }];

        let verdict = super::judge(&t, &body, &outcomes);

        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
    }

    #[test]
    fn timed_out_outcome_fails() {
        let t = task(vec![cmd_check("npm test", "exit 0")], vec![]);
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![CmdOutcome::TimedOut {
            run: "npm test".to_string(),
        }];

        let verdict = super::judge(&t, &body, &outcomes);

        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
    }

    #[test]
    fn unparseable_expect_stays_skipped_and_does_not_affect_passed() {
        let t = task(
            vec![cmd_check("npm run e2e", "성공 토스트 노출")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![CmdOutcome::Ran {
            run: "npm run e2e".to_string(),
            exit_code: 0,
        }];

        let verdict = super::judge(&t, &body, &outcomes);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert_eq!(verdict.skipped, vec!["cmd:npm run e2e".to_string()]);
    }

    #[test]
    fn browser_check_stays_skipped_and_never_fails() {
        let t = task(
            vec![DodCheck::Browser {
                flow: "signup".to_string(),
                expect: "success".to_string(),
            }],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});

        let verdict = super::judge(&t, &body, &[]);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert_eq!(verdict.skipped, vec!["browser:signup".to_string()]);
    }

    #[test]
    fn duplicate_run_consumes_outcomes_in_order() {
        let t = task(
            vec![
                cmd_check("npm test", "exit 0"),
                cmd_check("npm test", "exit 0"),
            ],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![
            CmdOutcome::Ran {
                run: "npm test".to_string(),
                exit_code: 0,
            },
            CmdOutcome::Ran {
                run: "npm test".to_string(),
                exit_code: 1,
            },
        ];

        let verdict = super::judge(&t, &body, &outcomes);

        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
    }
}
