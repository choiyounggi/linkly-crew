//! Lead's DoD judgment (plan D2, contracts-m3.md 커버리지 규칙): a pure
//! function over an accepted `TaskSpec` and a `task.result` body — Lead
//! executes DoD itself rather than trusting the worker's self-report
//! (DESIGN.md §4.2).

use crate::browser_exec::{parse_browser_expect, BrowserOutcome};
use crate::cmd_exec::{parse_expect, CmdOutcome};
use crew_proto::{DodCheck, TaskSpec};
use serde_json::Value;

/// Judgment result — contracts-m3.md 커버리지 규칙 as extended by M9 §G2d:
/// `passed` iff `uncovered`, `missing_artifacts`, and `failed_cmds` are all
/// empty. A `Browser` check is matched against `browser_outcomes` by
/// `(flow, expect)` and now *does* affect `passed` (issue #3): a failing
/// browser outcome lands in `failed_cmds`, while an unrecognized `expect`,
/// an unavailable binary, or no matching outcome land in `skipped` only.
/// A `Cmd` check lands in `skipped` when no
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

/// The `(flow, expect)` pair shared by every `BrowserOutcome` variant
/// (mirrors `outcome_run`'s single-field matching key for `CmdOutcome`).
fn outcome_flow_expect(outcome: &BrowserOutcome) -> (&str, &str) {
    match outcome {
        BrowserOutcome::Ran { flow, expect, .. }
        | BrowserOutcome::Refused { flow, expect, .. }
        | BrowserOutcome::TimedOut { flow, expect }
        | BrowserOutcome::SpawnFailed { flow, expect, .. }
        | BrowserOutcome::BinaryUnavailable { flow, expect, .. } => {
            (flow.as_str(), expect.as_str())
        }
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
/// invariant: an empty `cmd_outcomes` reproduces M3 exactly). `Browser`
/// checks are matched against `browser_outcomes` by `(flow, expect)` identity
/// (first-unused-match wins, mirroring `Cmd`): a `Ran{exit_code:0}` passes
/// silently; a `Ran{exit_code!=0}`, `Refused`, `TimedOut`, or `SpawnFailed`
/// outcome lands in `failed_cmds` (`browser:<flow> — <reason>`) and fails
/// `passed`; and a `BinaryUnavailable` outcome, an unrecognized `expect`, or
/// no matching outcome at all land in `skipped` only and never fail `passed`
/// — "could not execute" must never be indistinguishable from "passed"
/// (issue #3).
pub fn judge(
    task: &TaskSpec,
    result_body: &Value,
    cmd_outcomes: &[CmdOutcome],
    browser_outcomes: &[BrowserOutcome],
) -> DodVerdict {
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
    let mut used_browser_outcome = vec![false; browser_outcomes.len()];

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
            DodCheck::Browser { flow, expect } => {
                if parse_browser_expect(expect).is_none() {
                    skipped.push(format!("browser:{flow}"));
                    continue;
                }
                let matched = browser_outcomes.iter().enumerate().find(|(i, outcome)| {
                    !used_browser_outcome[*i]
                        && outcome_flow_expect(outcome) == (flow.as_str(), expect.as_str())
                });
                let Some((i, outcome)) = matched else {
                    skipped.push(format!("browser:{flow}"));
                    continue;
                };
                used_browser_outcome[i] = true;
                match outcome {
                    BrowserOutcome::Ran { exit_code, .. } if *exit_code == 0 => {}
                    BrowserOutcome::Ran { exit_code, .. } => {
                        failed_cmds.push(format!("browser:{flow} — exit {exit_code}"));
                    }
                    BrowserOutcome::Refused { reason, .. } => {
                        failed_cmds.push(format!("browser:{flow} — refused: {reason}"));
                    }
                    BrowserOutcome::TimedOut { .. } => {
                        failed_cmds.push(format!("browser:{flow} — timed out"));
                    }
                    BrowserOutcome::SpawnFailed { error, .. } => {
                        failed_cmds.push(format!("browser:{flow} — spawn failed: {error}"));
                    }
                    // NOT `failed_cmds`: the binary was never there to run, so
                    // this is "could not execute", not "failed". Folding it
                    // into `failed_cmds` — the blanket rule the sibling `Cmd`
                    // arm uses for Refused/TimedOut/SpawnFailed — would
                    // reverse settled decision D1. Pinned by
                    // `binary_unavailable_browser_check_is_skipped_not_failed`.
                    BrowserOutcome::BinaryUnavailable { binary, .. } => {
                        skipped.push(format!("browser:{flow} — binary unavailable: {binary}"));
                    }
                }
            }
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
    use crate::browser_exec::BrowserOutcome;
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

    fn browser_check(flow: &str, expect: &str) -> DodCheck {
        DodCheck::Browser {
            flow: flow.to_string(),
            expect: expect.to_string(),
        }
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

        let verdict = super::judge(&t, &body, &[], &[]);

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

        let verdict = super::judge(&t, &body, &[], &[]);

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

        let verdict = super::judge(&t, &body, &[], &[]);

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["spec.md".to_string(), "design.md".to_string()]
        );
    }

    // Still holds under the new 4-arg `judge`, but for a narrower reason
    // than before: this check's `expect` ("success") is unparseable under the
    // new grammar, so it exits at the `parse_browser_expect` guard and never
    // reaches outcome matching at all. It would stay skipped no matter what
    // `browser_outcomes` were supplied. The parseable-but-unmatched case is
    // pinned separately by
    // `parseable_browser_check_with_no_outcome_is_skipped_not_passed`.
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

        let verdict = super::judge(&t, &body, &[], &[]);

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

        let verdict = super::judge(&t, &body, &[], &[]);

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

        let verdict = super::judge(&t, &body, &outcomes, &[]);

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

        let verdict = super::judge(&t, &body, &outcomes, &[]);

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

        let verdict = super::judge(&t, &body, &outcomes, &[]);

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

        let verdict = super::judge(&t, &body, &outcomes, &[]);

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

        let verdict = super::judge(&t, &body, &outcomes, &[]);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert_eq!(verdict.skipped, vec!["cmd:npm run e2e".to_string()]);
    }

    // Same as above: unparseable `expect` ("success"), so this pins the
    // *unrecognized grammar* path, not the *no matching outcome* path. Both
    // land in `skipped`, but only the grammar guard is exercised here.
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

        let verdict = super::judge(&t, &body, &[], &[]);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert_eq!(verdict.skipped, vec!["browser:signup".to_string()]);
    }

    // --- browser outcomes: normal ---

    #[test]
    fn passing_browser_outcome_passes_and_is_not_skipped() {
        let t = task(
            vec![browser_check("http://localhost:3000", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![BrowserOutcome::Ran {
            flow: "http://localhost:3000".to_string(),
            expect: "text \"hi\"".to_string(),
            exit_code: 0,
        }];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert!(!verdict
            .skipped
            .contains(&"browser:http://localhost:3000".to_string()));
    }

    // --- browser outcomes: error ---

    #[test]
    fn failing_browser_outcome_makes_passed_false() {
        let t = task(
            vec![browser_check("http://localhost:3000", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![BrowserOutcome::Ran {
            flow: "http://localhost:3000".to_string(),
            expect: "text \"hi\"".to_string(),
            exit_code: 1,
        }];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        // The whole point of issue #3: a failing browser check now actually
        // blocks acceptance instead of being recorded and ignored.
        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
    }

    // --- browser outcomes: boundary ---

    #[test]
    fn binary_unavailable_browser_check_is_skipped_not_failed() {
        let t = task(
            vec![browser_check("http://localhost:3000", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![BrowserOutcome::BinaryUnavailable {
            flow: "http://localhost:3000".to_string(),
            expect: "text \"hi\"".to_string(),
            binary: "no-such-browser-cli".to_string(),
        }];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        // "Could not execute" is not "failed" — but it is also not a silent
        // pass of the *check*: it stays visible in `skipped`. Mirroring the
        // `Cmd` arm's blanket Refused/TimedOut/SpawnFailed fold here would
        // reverse settled decision D1, which is what this test catches.
        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert!(
            verdict
                .skipped
                .iter()
                .any(|s| s.starts_with("browser:http://localhost:3000")),
            "expected a skipped entry for the unavailable binary, got {:?}",
            verdict.skipped
        );
    }

    #[test]
    fn refused_browser_outcome_fails_the_verdict() {
        let t = task(
            vec![browser_check("https://example.com", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![BrowserOutcome::Refused {
            flow: "https://example.com".to_string(),
            expect: "text \"hi\"".to_string(),
            reason: "non-localhost host refused: example.com".to_string(),
        }];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        // A refused check is a check that did NOT verify anything. Letting it
        // fall into `skipped` would make a DoD naming an off-localhost flow
        // pass silently — the security-relevant half of issue #3.
        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
        assert!(
            verdict.failed_cmds[0].contains("refused"),
            "failure should name the refusal, got {:?}",
            verdict.failed_cmds
        );
        assert!(verdict.skipped.is_empty(), "got {:?}", verdict.skipped);
    }

    #[test]
    fn timed_out_browser_outcome_fails_the_verdict() {
        let t = task(
            vec![browser_check("http://localhost:3000", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![BrowserOutcome::TimedOut {
            flow: "http://localhost:3000".to_string(),
            expect: "text \"hi\"".to_string(),
        }];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        // A hung browser check must not become a silent pass.
        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
        assert!(
            verdict.failed_cmds[0].contains("timed out"),
            "failure should name the timeout, got {:?}",
            verdict.failed_cmds
        );
        assert!(verdict.skipped.is_empty(), "got {:?}", verdict.skipped);
    }

    #[test]
    fn spawn_failed_browser_outcome_fails_the_verdict() {
        let t = task(
            vec![browser_check("http://localhost:3000", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![BrowserOutcome::SpawnFailed {
            flow: "http://localhost:3000".to_string(),
            expect: "text \"hi\"".to_string(),
            error: "No such file or directory (os error 2)".to_string(),
        }];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        // A binary that was found but could not be executed is a broken
        // check, not an absent one — unlike `BinaryUnavailable`, it fails.
        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
        assert!(
            verdict.failed_cmds[0].contains("spawn failed"),
            "failure should name the spawn failure, got {:?}",
            verdict.failed_cmds
        );
        assert!(verdict.skipped.is_empty(), "got {:?}", verdict.skipped);
    }

    #[test]
    fn parseable_browser_check_with_no_outcome_is_skipped_not_passed() {
        let t = task(
            vec![browser_check("http://localhost:3000", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});

        // The live production path today: `dispatch.rs` passes `&[]` because
        // the executor is not wired yet (t3 owns that). A parseable check
        // with no outcome must stay VISIBLE in `skipped`, never be dropped.
        let verdict = super::judge(&t, &body, &[], &[]);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert_eq!(
            verdict.skipped,
            vec!["browser:http://localhost:3000".to_string()]
        );
    }

    #[test]
    fn browser_outcome_for_a_different_expect_is_not_matched() {
        let t = task(
            vec![browser_check("http://localhost:3000", "text \"hi\"")],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        // Same `flow`, different `expect`: this outcome belongs to another
        // check. A flow-only matching key would consume it here and report a
        // failure the task never actually incurred.
        let outcomes = vec![BrowserOutcome::Ran {
            flow: "http://localhost:3000".to_string(),
            expect: "visible \"#done\"".to_string(),
            exit_code: 1,
        }];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        assert!(verdict.passed);
        assert!(verdict.failed_cmds.is_empty());
        assert_eq!(
            verdict.skipped,
            vec!["browser:http://localhost:3000".to_string()]
        );
    }

    #[test]
    fn duplicate_browser_checks_consume_outcomes_in_order() {
        let t = task(
            vec![
                browser_check("http://localhost:3000", "text \"hi\""),
                browser_check("http://localhost:3000", "text \"hi\""),
            ],
            vec![],
        );
        let body = json!({"covered_req_ids": [], "artifacts": []});
        let outcomes = vec![
            BrowserOutcome::Ran {
                flow: "http://localhost:3000".to_string(),
                expect: "text \"hi\"".to_string(),
                exit_code: 0,
            },
            BrowserOutcome::Ran {
                flow: "http://localhost:3000".to_string(),
                expect: "text \"hi\"".to_string(),
                exit_code: 1,
            },
        ];

        let verdict = super::judge(&t, &body, &[], &outcomes);

        // Without the `used_browser_outcome` bookkeeping both checks would
        // match the first (passing) outcome and the failure would vanish.
        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
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

        let verdict = super::judge(&t, &body, &outcomes, &[]);

        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
    }
}
