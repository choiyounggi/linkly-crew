//! Lead's DoD judgment (plan D2, contracts-m3.md 커버리지 규칙) over an
//! accepted `TaskSpec` and a `task.result` body — Lead executes DoD itself
//! rather than trusting the worker's self-report (DESIGN.md §4.2). No longer a
//! pure function: an artifact reported by `path` is verified against the role
//! worktree on disk (issue #31b), so `judge` reads the filesystem.

use crate::browser_exec::{parse_browser_expect, BrowserOutcome};
use crate::cmd_exec::{parse_expect, CmdOutcome};
use crew_proto::{task_result_artifacts_from_body, DodCheck, TaskResultArtifact, TaskSpec};
use serde_json::Value;
use std::path::{Path, PathBuf};

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
///
/// A `missing_artifacts` entry is the bare artifact name when the entry is
/// absent, was dropped by the wire parser, or carried empty `content`; an
/// entry reported by `path` that failed verification reads
/// `<name> (<reason>)` — e.g. `report (file not found)` — and `accept.rs`'s
/// `dod_violation_strings` prefixes it with `artifact:` unchanged (issue #31b).
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

/// The fixed reasons a reported artifact `path` can fail (plan D5). They are
/// appended to the artifact name as `<name> (<reason>)` in
/// `DodVerdict::missing_artifacts`, and deliberately never echo the
/// agent-supplied path string back into the verdict.
const REASON_INVALID: &str = "invalid path";
const REASON_NO_CWD: &str = "path not verifiable: no role worktree";
const REASON_CWD_UNAVAILABLE: &str = "role worktree unavailable";
const REASON_NOT_FOUND: &str = "file not found";
const REASON_ESCAPES: &str = "path escapes worktree";
const REASON_NOT_FILE: &str = "not a regular file";
const REASON_EMPTY: &str = "empty file";

/// Judges ONE reported artifact entry (issue #31b, plan D3/D4).
///
/// `Ok(())` = present. `Err(None)` = missing with no reason to report (a
/// content entry that is absent or empty — the M3 wording). `Err(Some(r))` =
/// missing because the reported `path` failed check `r`.
///
/// `canon_cwd` is the canonicalized role worktree, computed once per `judge`
/// call: `None` = no resolver wired at all, `Some(Err)` = the resolver named a
/// directory that cannot be canonicalized.
fn entry_status(
    a: &TaskResultArtifact,
    canon_cwd: Option<&std::io::Result<PathBuf>>,
) -> Result<(), Option<&'static str>> {
    // D4: an entry with no `path` keeps the M3 rule — non-empty `content`.
    // An entry that HAS a `path` is judged by the path alone; non-empty
    // `content` must never rescue it, which is the issue #31 defect itself.
    if a.path.is_none() {
        return match a.content.as_deref() {
            Some(c) if !c.is_empty() => Ok(()),
            _ => Err(None),
        };
    }
    // D3, first failure wins. `validated_path` is a LEXICAL check that returns
    // the ORIGINAL string (t5-proto), so canonicalize + the prefix check below
    // are what actually confine the path to the worktree: `..` is rejected
    // here, but a symlink pointing outside is invisible until it is resolved.
    let Ok(rel) = a.validated_path() else {
        return Err(Some(REASON_INVALID));
    };
    // A trailing separator asserts "this is a directory", and only a regular
    // file can satisfy an artifact. Rejected explicitly rather than left to
    // `canonicalize`: glibc's realpath reports ENOTDIR here, but macOS strips
    // the slash and RESOLVES to the file (measured), so relying on the OS
    // would make the verdict platform-dependent.
    if rel.ends_with(std::path::is_separator) {
        return Err(Some(REASON_NOT_FOUND));
    }
    let Some(canon_cwd) = canon_cwd else {
        return Err(Some(REASON_NO_CWD));
    };
    let Ok(base) = canon_cwd else {
        return Err(Some(REASON_CWD_UNAVAILABLE));
    };
    let Ok(resolved) = std::fs::canonicalize(base.join(rel)) else {
        return Err(Some(REASON_NOT_FOUND));
    };
    if !resolved.starts_with(base) {
        return Err(Some(REASON_ESCAPES));
    }
    // `symlink_metadata`, not `metadata`: a symlink swapped in after the
    // canonicalize above is then seen as a link rather than followed.
    let Ok(meta) = std::fs::symlink_metadata(&resolved) else {
        return Err(Some(REASON_NOT_FOUND));
    };
    if !meta.file_type().is_file() {
        return Err(Some(REASON_NOT_FILE));
    }
    if meta.len() == 0 {
        return Err(Some(REASON_EMPTY));
    }
    Ok(())
}

/// The `missing_artifacts` entry for one expected artifact name, or `None`
/// when it is present (plan D6): ANY entry with that name being present wins;
/// otherwise the reported reason is the first `Some(reason)` among that name's
/// entries in input order, and a bare name when there is none (no entry at
/// all, an entry the t5-proto parser dropped, or an empty-content entry).
fn missing_entry(
    artifacts: &[TaskResultArtifact],
    name: &str,
    canon_cwd: Option<&std::io::Result<PathBuf>>,
) -> Option<String> {
    let mut reason: Option<&'static str> = None;
    for a in artifacts.iter().filter(|a| a.name == name) {
        match entry_status(a, canon_cwd) {
            Ok(()) => return None,
            Err(r) => reason = reason.or(r),
        }
    }
    match reason {
        Some(r) => Some(format!("{name} ({r})")),
        None => Some(name.to_string()),
    }
}

/// Executes a task's DoD against a `task.result` body (contracts-m3.md
/// verbatim): `ReqCover` checks `body["covered_req_ids"]` (defensive parse —
/// a missing/non-array field counts as no coverage) against the union of
/// every `ReqCover.ids` in `task.dod`; artifact presence is checked for every
/// `task.artifacts_expected` entry and every `DodCheck::Artifact{name}` in
/// `task.dod` against `body["artifacts"]` as parsed by the wire contract
/// (`crew_proto::task_result_artifacts_from_body`) — an entry reporting a
/// `path` is present only when `artifact_cwd` is wired and that path resolves,
/// inside the canonical role worktree, to a regular non-empty file (issue
/// #31b, `entry_status`), while an entry without a `path` keeps the M3 rule of
/// non-empty `content` and is never written to disk
/// (plan D2 — the two sources are unioned, deduped by name, since
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
    artifact_cwd: Option<&Path>,
) -> DodVerdict {
    let covered: Vec<&str> = match result_body.get("covered_req_ids") {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    // The t5-proto contract owns the wire shape; a malformed entry is DROPPED
    // there, so its name is simply absent and reported missing (fail-closed).
    let artifacts = task_result_artifacts_from_body(result_body);
    let canon_cwd = artifact_cwd.map(std::fs::canonicalize);

    let mut uncovered = Vec::new();
    let mut missing_artifacts = Vec::new();
    let mut failed_cmds = Vec::new();
    let mut skipped = Vec::new();
    // Names already judged, so an artifact named by BOTH a `DodCheck::Artifact`
    // and an `artifacts_expected` contract is reported once (D6). Keyed on the
    // NAME: comparing `missing_artifacts` strings no longer works now that an
    // entry can carry a ` (<reason>)` suffix.
    let mut judged: Vec<&str> = Vec::new();
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
                if !judged.contains(&name.as_str()) {
                    judged.push(name.as_str());
                    if let Some(m) = missing_entry(&artifacts, name, canon_cwd.as_ref()) {
                        missing_artifacts.push(m);
                    }
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
        let name = contract.name.as_str();
        if !judged.contains(&name) {
            judged.push(name);
            if let Some(m) = missing_entry(&artifacts, name, canon_cwd.as_ref()) {
                missing_artifacts.push(m);
            }
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

/// On-disk fixtures for the artifact tests (plan D10/D11).
///
/// Creates a uniquely named directory under the WORKTREE's `.claude/tmp`
/// (git-excluded) — never `/tmp` or `$TMPDIR` — and removes it in `Drop`, so
/// cleanup also runs when the test that created it panics. `pub(crate)` so
/// `dispatch.rs`'s tests reuse it without a new module in `lib.rs`.
#[cfg(test)]
pub(crate) mod test_scratch {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    pub(crate) struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        /// A fresh directory named by pid + a process-wide counter, so parallel
        /// cargo test threads — and two `cargo test` processes in the same
        /// worktree — never collide (D11).
        pub(crate) fn new(label: &str) -> Self {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.claude/tmp")
                .join(format!("t6-fixture-{label}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path)
                .expect("create scratch dir under the worktree .claude/tmp");
            Self { path }
        }

        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        /// Writes `bytes` at `rel` inside the scratch dir, creating parents.
        pub(crate) fn write(&self, rel: &str, bytes: &[u8]) {
            let target = self.path.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("create fixture parent dir");
            }
            std::fs::write(&target, bytes).expect("write fixture file");
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::browser_exec::BrowserOutcome;
    use crate::cmd_exec::CmdOutcome;
    use crate::dod_exec::test_scratch::ScratchDir;
    use crew_proto::{ArtifactContract, DodCheck, ReqId, Role, TaskSpec};
    use serde_json::json;
    use std::path::Path;

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

        let verdict = super::judge(&t, &body, &[], &[], None);

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

        let verdict = super::judge(&t, &body, &[], &[], None);

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

        let verdict = super::judge(&t, &body, &[], &[], None);

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

        let verdict = super::judge(&t, &body, &[], &[], None);

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

        let verdict = super::judge(&t, &body, &[], &[], None);

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

        let verdict = super::judge(&t, &body, &outcomes, &[], None);

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

        let verdict = super::judge(&t, &body, &outcomes, &[], None);

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

        let verdict = super::judge(&t, &body, &outcomes, &[], None);

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

        let verdict = super::judge(&t, &body, &outcomes, &[], None);

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

        let verdict = super::judge(&t, &body, &outcomes, &[], None);

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

        let verdict = super::judge(&t, &body, &[], &[], None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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
        let verdict = super::judge(&t, &body, &[], &[], None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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

        let verdict = super::judge(&t, &body, &[], &outcomes, None);

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

        let verdict = super::judge(&t, &body, &outcomes, &[], None);

        assert!(!verdict.passed);
        assert_eq!(verdict.failed_cmds.len(), 1);
    }

    // ---- issue #31b: artifacts judged against the disk (plan D3/D4/D5) ----

    fn contract(name: &str) -> ArtifactContract {
        ArtifactContract {
            name: name.to_string(),
            kind: "doc".to_string(),
            req_ids: vec![],
        }
    }

    /// A result body reporting one artifact BY PATH (no `content`).
    fn path_body(name: &str, path: &str) -> serde_json::Value {
        json!({
            "covered_req_ids": [],
            "artifacts": [{"name": name, "kind": "doc", "req_ids": [], "path": path}],
        })
    }

    // N1
    #[test]
    fn path_entry_with_nonempty_file_in_cwd_passes() {
        let cwd = ScratchDir::new("n1");
        cwd.write("out/report.md", b"x");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // The path must be resolved against the ROLE cwd, never the process
        // cwd: no such file exists relative to crates/crew-lead (plan D11).
        assert!(!Path::new("out/report.md").exists());
        assert!(verdict.passed);
        assert_eq!(verdict.missing_artifacts, Vec::<String>::new());
    }

    // N2
    #[test]
    #[cfg(unix)]
    fn path_entry_through_symlink_to_file_inside_cwd_passes() {
        let cwd = ScratchDir::new("n2");
        cwd.write("out/real.md", b"x");
        std::os::unix::fs::symlink(cwd.path().join("out/real.md"), cwd.path().join("out/report.md"))
            .expect("symlink inside the cwd");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(verdict.passed);
        assert_eq!(verdict.missing_artifacts, Vec::<String>::new());
    }

    // E1
    #[test]
    fn path_entry_with_absent_file_is_missing_file_not_found() {
        let cwd = ScratchDir::new("e1");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (file not found)".to_string()]
        );
    }

    // E2
    #[test]
    fn path_entry_with_empty_file_is_missing_empty_file() {
        let cwd = ScratchDir::new("e2");
        cwd.write("out/report.md", b"");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (empty file)".to_string()]
        );
    }

    // E3
    #[test]
    fn path_entry_with_parent_dir_component_is_missing_invalid_path() {
        let cwd = ScratchDir::new("e3-cwd");
        let outside = ScratchDir::new("e3-outside");
        outside.write("secret.md", b"x");
        let escape = format!(
            "../{}/secret.md",
            outside.path().file_name().unwrap().to_str().unwrap()
        );
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", &escape);

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // The target really exists on disk — it is rejected for being outside
        // the worktree, lexically, before any filesystem lookup.
        assert!(outside.path().join("secret.md").exists());
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (invalid path)".to_string()]
        );
    }

    // E4
    #[test]
    #[cfg(unix)]
    fn path_entry_symlink_escaping_cwd_is_missing_path_escapes_worktree() {
        let cwd = ScratchDir::new("e4-cwd");
        let outside = ScratchDir::new("e4-outside");
        outside.write("secret.md", b"x");
        std::fs::create_dir_all(cwd.path().join("out")).expect("create out/");
        std::os::unix::fs::symlink(outside.path().join("secret.md"), cwd.path().join("out/report.md"))
            .expect("symlink escaping the cwd");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // Lexically clean, and the link target is a real non-empty file: only
        // canonicalize + prefix check can catch this (t5-proto contract note).
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (path escapes worktree)".to_string()]
        );
    }

    // E5
    #[test]
    fn invalid_path_strings_are_missing_invalid_path() {
        let cwd = ScratchDir::new("e5");
        let t = task(
            vec![],
            vec![contract("a"), contract("b"), contract("c")],
        );
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [
                {"name": "a", "kind": "doc", "req_ids": [], "path": ""},
                {"name": "b", "kind": "doc", "req_ids": [], "path": "/etc/hosts"},
                {"name": "c", "kind": "doc", "req_ids": [], "path": "."},
            ],
        });

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec![
                "a (invalid path)".to_string(),
                "b (invalid path)".to_string(),
                "c (invalid path)".to_string(),
            ]
        );
    }

    // E6
    #[test]
    fn path_entry_naming_a_directory_is_missing_not_a_regular_file() {
        let cwd = ScratchDir::new("e6");
        std::fs::create_dir_all(cwd.path().join("out/report.md")).expect("create the dir");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (not a regular file)".to_string()]
        );
    }

    // E7
    #[test]
    fn path_entry_under_absent_parent_is_missing_file_not_found_without_panic() {
        let cwd = ScratchDir::new("e7");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "nope/deeper/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (file not found)".to_string()]
        );
    }

    // E8
    #[test]
    fn nonexistent_cwd_makes_path_entries_missing_role_worktree_unavailable() {
        let scratch = ScratchDir::new("e8");
        let gone = scratch.path().join("gone");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(gone.as_path()));

        assert!(!gone.exists());
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (role worktree unavailable)".to_string()]
        );
    }

    // E9
    #[test]
    #[cfg(unix)]
    fn path_entry_naming_a_fifo_is_missing_not_a_regular_file() {
        use std::os::unix::ffi::OsStrExt;

        let cwd = ScratchDir::new("e9");
        std::fs::create_dir_all(cwd.path().join("out")).expect("create out/");
        let fifo = cwd.path().join("out/report.md");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path in a directory this test owns.
        let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo must succeed");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (not a regular file)".to_string()]
        );
    }

    // E10
    #[test]
    fn path_entry_with_trailing_slash_is_missing_file_not_found() {
        let cwd = ScratchDir::new("e10");
        cwd.write("out/report.md", b"x");
        let t = task(vec![], vec![contract("report")]);
        let body = path_body("report", "out/report.md/");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // The file exists, but a trailing slash asserts "directory" (ENOTDIR).
        assert!(cwd.path().join("out/report.md").is_file());
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (file not found)".to_string()]
        );
    }

    // B1
    #[test]
    fn content_only_entry_still_passes_with_a_cwd() {
        let cwd = ScratchDir::new("b1");
        let t = task(vec![], vec![contract("report")]);
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [{"name": "report", "kind": "doc", "req_ids": [], "content": "hello"}],
        });

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(verdict.passed);
        assert_eq!(verdict.missing_artifacts, Vec::<String>::new());
    }

    // B2
    #[test]
    fn path_wins_over_content_when_both_present() {
        let cwd = ScratchDir::new("b2");
        let t = task(vec![], vec![contract("report")]);
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [{
                "name": "report", "kind": "doc", "req_ids": [],
                "path": "absent.md", "content": "hello",
            }],
        });

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // Non-empty content must NOT rescue a path that is not on disk — that
        // is exactly the issue #31 defect.
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (file not found)".to_string()]
        );
    }

    // B3
    #[test]
    fn empty_artifacts_array_with_cwd_reports_bare_name() {
        let cwd = ScratchDir::new("b3");
        let t = task(vec![], vec![contract("report")]);
        let body = json!({"covered_req_ids": [], "artifacts": []});

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(verdict.missing_artifacts, vec!["report".to_string()]);
    }

    // B4
    #[test]
    fn artifact_check_and_expected_contract_same_name_reported_once() {
        let cwd = ScratchDir::new("b4");
        let t = task(
            vec![DodCheck::Artifact {
                name: "report".to_string(),
            }],
            vec![contract("report")],
        );
        let body = path_body("report", "absent.md");

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (file not found)".to_string()],
            "the dod check and the expected contract name one artifact, reported once"
        );
    }

    // B5
    #[test]
    fn no_cwd_path_entry_is_missing_not_verifiable_and_content_entry_unchanged() {
        let cwd = ScratchDir::new("b5");
        cwd.write("out/report.md", b"x");
        let t = task(vec![], vec![contract("report"), contract("spec.md")]);
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [
                {"name": "report", "kind": "doc", "req_ids": [], "path": "out/report.md"},
                {"name": "spec.md", "kind": "doc", "req_ids": [], "content": "x"},
            ],
        });

        // No resolver: the file exists, but nothing can be verified without a
        // role worktree, so the claim is not evidence (fail-closed, plan D7).
        let verdict = super::judge(&t, &body, &[], &[], None);

        assert!(cwd.path().join("out/report.md").is_file());
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (path not verifiable: no role worktree)".to_string()]
        );
    }

    // B6
    #[test]
    fn dropped_entry_counts_as_absent() {
        let cwd = ScratchDir::new("b6");
        cwd.write("out/report.md", b"x");
        let t = task(vec![], vec![contract("report")]);
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [{
                "name": "report", "kind": "doc", "req_ids": null,
                "path": "out/report.md",
            }],
        });

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // The t5-proto parser DROPS a malformed entry, so the name is simply
        // absent — fail-closed, never "nothing to check".
        assert!(!verdict.passed);
        assert_eq!(verdict.missing_artifacts, vec!["report".to_string()]);
    }

    // B8
    #[test]
    fn first_failing_reason_wins_when_same_name_entries_fail_differently() {
        let cwd = ScratchDir::new("b8");
        cwd.write("out/empty.md", b"");
        let t = task(vec![], vec![contract("report")]);
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [
                {"name": "report", "kind": "doc", "req_ids": [], "path": "out/empty.md"},
                {"name": "report", "kind": "doc", "req_ids": [], "path": "absent.md"},
            ],
        });

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // Both entries fail, with DIFFERENT reasons: the reported one is the
        // FIRST in input order (plan D6's tie-break), so a last-wins
        // implementation would say "file not found" here.
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (empty file)".to_string()]
        );
    }

    // B9
    #[test]
    fn a_reasonless_entry_does_not_mask_a_later_path_reason() {
        let cwd = ScratchDir::new("b9");
        let t = task(vec![], vec![contract("report")]);
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [
                {"name": "report", "kind": "doc", "req_ids": [], "content": ""},
                {"name": "report", "kind": "doc", "req_ids": [], "path": "absent.md"},
            ],
        });

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        // The empty-content entry carries no reason (bare name); it must not
        // suppress the reason the later path entry does carry.
        assert!(!verdict.passed);
        assert_eq!(
            verdict.missing_artifacts,
            vec!["report (file not found)".to_string()]
        );
    }

    // B7
    #[test]
    fn any_passing_entry_with_same_name_wins() {
        let cwd = ScratchDir::new("b7");
        cwd.write("out/report.md", b"x");
        let t = task(vec![], vec![contract("report")]);
        let body = json!({
            "covered_req_ids": [],
            "artifacts": [
                {"name": "report", "kind": "doc", "req_ids": [], "path": "absent.md"},
                {"name": "report", "kind": "doc", "req_ids": [], "path": "out/report.md"},
            ],
        });

        let verdict = super::judge(&t, &body, &[], &[], Some(cwd.path()));

        assert!(verdict.passed);
        assert_eq!(verdict.missing_artifacts, Vec::<String>::new());
    }
}
