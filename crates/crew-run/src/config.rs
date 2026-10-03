//! Run configuration and error types — contract §C3 verbatim, extended by
//! contracts-m5.md §C5a (multi-sprint fields).

use std::path::{Path, PathBuf};

use crew_agent::BusError;
use crew_ledger::LedgerError;
use crew_lead::plan::{CmdCheck, PlanError};

/// One browser DoD check to attach to the Developer task (t3 plan D3),
/// the browser-side sibling of [`CmdCheck`].
///
/// `flow` is a navigation URL and only `localhost` / `127.0.0.1` are
/// accepted — `crew_lead::browser_exec` refuses any other host before a
/// spawn is ever attempted (user decision D3).
///
/// `expect` must be exactly one of the three structured forms
/// `text "<v>"`, `visible "<v>"`, `url "<v>"` (user decision D2). There is
/// no escaping, so a `"` inside `<v>` makes the whole `expect` unparseable.
/// Anything outside those three forms is never executed and is recorded as
/// `skipped` — it is not an error and it never affects `passed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCheck {
    pub flow: String,
    pub expect: String,
}

/// One run's configuration (contract §C3, extended by contracts-m5.md
/// §C5a).
pub struct RunConfig {
    pub goal: String,
    pub mode: RunMode,
    /// Caller-supplied directory the run's ledger lives under. `/tmp` and
    /// `$TMPDIR` are forbidden by the workspace constraints — the app's
    /// default (t-bridge) is `~/.linkly-crew/app-runs/<run_id>/`.
    pub data_dir: PathBuf,
    /// Lead's per-task rework budget. Default `AcceptanceLoop::default_budget()`
    /// (contracts-m3.md layering rule: Lead's budget must exhaust before
    /// CorrGuard's separate max_rounds would).
    pub max_rework: u32,
    /// Sprint size cap (contracts-m5.md §C5a). `0` = every task in one
    /// sprint (current single-sprint behavior, unchanged).
    pub max_per_sprint: usize,
    /// `Lead`'s escalation-cascade timeout, forwarded verbatim to
    /// `LeadBehavior::escalation_timeout_ms` (contracts-m5.md §C5a). `0` =
    /// disabled (default).
    pub escalation_timeout_ms: u64,
    /// Agent roster (contracts-m5.md §C5a). `None` = the default 6-slot
    /// roster (lead + 5 roles, all `claude-code`/`"default"`).
    pub roster: Option<crew_proto::Roster>,
    /// `cmd` DoD checks to attach to the Developer task (contracts-m10.md
    /// §H1g). Empty vector (default) = no emission = the M9-and-earlier
    /// behavior unchanged.
    pub dev_cmd_checks: Vec<CmdCheck>,
    /// The real project tree the crew works in and the Lead's `cmd` DoD checks
    /// execute in. `None` (the shipped default) = the M10-and-earlier behavior
    /// exactly: every role's CLI cwd and the Cmd DoD exec cwd are the per-role
    /// scratch `<data_dir>/cli-cwd/<role>`. `Some(root)` = `root` must be a git
    /// repository; every role's CLI cwd and the Cmd DoD exec cwd are instead
    /// that role's own out-of-repo `git worktree` (`crew/<role>` branch) under
    /// it — distinct per role, never `root` itself (HANDOFF trap 30, this
    /// crate's `worktree` module) — with a shared `<root>/.crew/artifacts`
    /// convention injected into each role's spawn spec.
    ///
    /// Caller-designated only — never derived from cwd, git, or a guess
    /// (HANDOFF §5 trap 29 / contracts-m11.md §I6). With `Some`, the Lead
    /// executes agent-authored code by design; the containment is this
    /// human-designated tree, not the argv allowlist.
    pub project_root: Option<PathBuf>,
    // ---------------------------------------------------------------------
    // Construction-site inventory for the two fields below (t3 plan D2,
    // reconciled 2026-09-23). `RunConfig` has no `Default` impl, so adding a
    // field forces an explicit initializer at every literal construction, and
    // the plan made this reconciliation a stop-and-report tripwire.
    //
    // Measured at base 193f52a, before the sweep:
    //     git grep -c 'RunConfig {'   ->  26 lines
    //       = 1 struct definition + 9 fn signatures + 16 literal constructions
    //
    // Only 14 of those 16 literals needed an explicit initializer:
    //   - 13 were named by `cargo check --workspace --all-targets`;
    //   - 1 more, apps/crew-app/src-tauri/src/core.rs, is INVISIBLE to that
    //     command: apps/crew-app/src-tauri carries its own empty [workspace]
    //     table and is not a root-workspace member, so it has to be checked
    //     with its own --manifest-path;
    //   - the last 2, crates/crew-run/tests/run_controller.rs:200 and :220,
    //     use functional-update syntax (`..cfg`, `..scripted_config(..)`) and
    //     INHERIT new fields, so they compile untouched and need no line.
    //
    // So a later sweep that counts explicit initializers and finds 14, not
    // 16, has NOT missed two sites. Re-measure with this file excluded —
    // otherwise this comment's own text inflates the count it quotes:
    //     git grep -c 'RunConfig {' -- ':!crates/crew-run/src/config.rs'
    // -> 27 today = the 25 above (26 less this file's struct definition),
    //    plus the one signature and one literal added by the new
    //    tests/m10_browser_dod.rs.
    // ---------------------------------------------------------------------
    /// External browser CLI the Lead shells out to for `browser` DoD checks
    /// (user decision D1 — no new crate dependency, no tool hardcoded).
    /// `None` (the shipped default) = the browser DoD is never wired, so
    /// every browser check stays `skipped`, exactly as before t3.
    ///
    /// A bare name is resolved against the run process's `PATH`; an
    /// absolute path is used as-is. Note the `PATH` that matters is the one
    /// the run process inherited — for the Tauri app that is the
    /// GUI-launched environment, not the shell you tested in.
    ///
    /// Being absent from `PATH` is a NORMAL path, not an error: nothing is
    /// spawned and the check is recorded as `skipped`, never as a failure.
    /// Wiring additionally requires `project_root` to be `Some` (D3); with
    /// `project_root: None` this field alone changes nothing.
    pub browser_binary: Option<String>,
    /// `browser` DoD checks to attach to the Developer task — the browser
    /// sibling of `dev_cmd_checks`, and the only producer of
    /// `DodCheck::Browser` in production (t3 ruling C1).
    ///
    /// Empty (the shipped default) = no browser check is ever attached, so
    /// a run started from a default config produces zero browser checks.
    /// It must stay unarmed by default for the same reason `dev_cmd_checks`
    /// does (함정 29 / issue #5): a knob that arms itself executes
    /// agent-adjacent tooling nobody asked for.
    pub dev_browser_checks: Vec<BrowserCheck>,
    /// Per-turn timeout (seconds) for every role worker's CLI turn and the
    /// Lead's LLM `specify` call (M13 turn-recovery fix D3). Default 900 —
    /// matches the task deadline (`deadline_ms` 900_000). `0` is rejected by
    /// `start_run` as `RunError::ConfigInvalid` before any spawn.
    pub turn_timeout_secs: u64,
}

/// Default Rust-domain dev cmd checks that satisfy `CmdPolicy`'s positional
/// allowlist (contracts-m9.md §G2c / contracts-m10.md §H1g): the prefix
/// tokens equal an allowed prefix literally, and every trailing token is a
/// bare identifier. `cargo test --workspace` is not used here — its trailing
/// `--workspace` flag fails the bare-identifier rule and would be `Refused`.
/// Not a default — `RunConfig::dev_cmd_checks`'s shipped default stays empty.
pub fn default_dev_cmd_checks_rust() -> Vec<CmdCheck> {
    vec![CmdCheck {
        run: "cargo test".to_string(),
        expect: "exit 0".to_string(),
    }]
}

/// Default Node-domain dev cmd checks — see [`default_dev_cmd_checks_rust`]
/// for the allowlist rule they satisfy. Not a default — `RunConfig`'s
/// shipped default stays empty.
pub fn default_dev_cmd_checks_node() -> Vec<CmdCheck> {
    vec![
        CmdCheck {
            run: "npm test".to_string(),
            expect: "exit 0".to_string(),
        },
        CmdCheck {
            run: "npm run build".to_string(),
            expect: "exit 0".to_string(),
        },
    ]
}

/// Auto-detects the dev cmd DoD checks for a project tree (issue #33): a
/// `Cargo.toml` regular file at `project_root` selects
/// [`default_dev_cmd_checks_rust`]. A `package.json` regular file contributes
/// `npm test` only if its `scripts.test` is a non-empty string and
/// `npm run build` only if its `scripts.build` is (in that order) — a missing
/// script would make npm exit non-zero on every round, so it is never armed.
/// A malformed or non-object `package.json` contributes nothing. Both
/// markers select rust then node. Neither, a missing root, or a marker that
/// is a directory (not a regular file) yields an empty vector.
///
/// A pure filesystem read of the root's two direct entries — nothing is
/// spawned and subdirectories are never searched. The caller decides whether
/// to arm the result (the app arms it only for a human-designated
/// `project_root`, 함정 29 / issue #5).
pub fn detect_dev_cmd_checks(project_root: &Path) -> Vec<CmdCheck> {
    let is_file = |name: &str| project_root.join(name).is_file();
    let mut checks = Vec::new();
    if is_file("Cargo.toml") {
        checks.extend(default_dev_cmd_checks_rust());
    }
    if is_file("package.json") {
        checks.extend(node_checks_from_package_json(
            &project_root.join("package.json"),
        ));
    }
    checks
}

/// The node half of [`detect_dev_cmd_checks`]: one [`CmdCheck`] per declared,
/// non-empty `scripts.test` / `scripts.build`. Unreadable or malformed JSON,
/// a non-object root, or a non-object `scripts` all yield nothing.
fn node_checks_from_package_json(path: &Path) -> Vec<CmdCheck> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let has_script = |name: &str| {
        json.get("scripts")
            .and_then(|scripts| scripts.get(name))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|cmd| !cmd.trim().is_empty())
    };
    [("test", "npm test"), ("build", "npm run build")]
        .into_iter()
        .filter(|(script, _)| has_script(script))
        .map(|(_, run)| CmdCheck {
            run: run.to_string(),
            expect: "exit 0".to_string(),
        })
        .collect()
}

/// How the run's five crew-member workers behave (contract §C3). `Clone`
/// so the multi-sprint loop (contracts-m5.md §C5a) can hold one `RunConfig`
/// value while re-spawning fresh workers from the same `mode` at every
/// sprint boundary.
#[derive(Clone)]
pub enum RunMode {
    /// Deterministic demo/test: five `ScriptedCrewMember`s. `planted_violations`
    /// maps a role to the REQ ids it omits from coverage on its first attempt.
    Scripted {
        planted_violations: Vec<(crew_proto::Role, Vec<String>)>,
    },
    /// Five real CLI role sessions (`RoleHarnessBehavior` + `ClaudeCodeHarness`).
    /// Type/assembly only — never exercised by a worker/CI test.
    RealCli,
}

/// A human reviewer's decision on an `Escalated` task (contracts-m7.md §E5),
/// carried through `RunHandle::resolve_gate` into a `human.response`
/// envelope (§E2 verbatim: wire values `"approve"`/`"reject"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Approve,
    Reject,
}

/// Errors from starting or running a `crew-run` (plan D7).
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("failed to build the sprint spec/dag: {0}")]
    SpecFailed(#[from] PlanError),
    #[error("bus connection failed: {0}")]
    Bus(#[from] BusError),
    #[error("ledger error: {0}")]
    Ledger(#[from] LedgerError),
    #[error("lead runner failed: {0}")]
    Join(String),
    /// `RunHandle::swap_harness` validation failure (contracts-m5.md §C5c,
    /// t-swap plan D2) — unknown `agent_id` or unknown `harness` id, message
    /// text distinguishes the two. The roster is left unchanged and no
    /// event is emitted when this is returned.
    #[error("swap rejected: {0}")]
    SwapRejected(String),
    /// `RunHandle::swap_harness` step 3 (mid-sprint immediate effectuation,
    /// contracts-m6.md §D2b, t-swap plan D4) failed to reach the live
    /// worker: send failure, ack timeout, worker-dropped ack, or a harness
    /// error from the swap itself. The roster mutation, handoff envelope,
    /// and `RosterChanged` from steps 1-2 are unaffected — this only means
    /// the swap falls back to taking effect at the next sprint boundary
    /// (M5 semantics), not that it failed outright.
    #[error("swap incomplete, falls back to next sprint boundary: {0}")]
    SwapIncomplete(String),
    /// `validate_roster` rejected `RunConfig::roster` (contracts-m7.md §E4)
    /// — `RunController::start` returns this before any spawn or ledger
    /// creation, so a rejected roster leaves no partial run state.
    #[error("invalid roster: {0}")]
    RosterInvalid(String),
    /// `RunHandle::resolve_gate` (contracts-m7.md §E5) could not reach the
    /// run-resident `agent:human` proxy — the run has already ended (the
    /// proxy is torn down alongside it, plan D1) or the bus rejected the
    /// `human.response` publish.
    #[error("gate unavailable: {0}")]
    GateUnavailable(String),
    /// `RunConfig.project_root` failed startup validation (contracts-m11.md
    /// §I2): not absolute, or not an existing directory. The message carries
    /// the received value verbatim. No fallback to the scratch cwd — a silent
    /// fallback reproduces M10's exit-101 confusion (docs/SPIKE-M10.md §3).
    #[error("project_root invalid: {0}")]
    ProjectRootInvalid(String),
    /// `RunConfig` failed startup validation before any spawn (M13
    /// turn-recovery fix D4) — e.g. `turn_timeout_secs == 0`, which would
    /// otherwise make every turn instantly time out.
    #[error("invalid run config: {0}")]
    ConfigInvalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crew_lead::cmd_exec::CmdPolicy;

    #[test]
    fn default_dev_cmd_checks_rust_satisfies_the_positional_allowlist() {
        // Asserts each `run` against the production judge
        // (`CmdPolicy::default_allowlist().vet_run`, contracts-m9.md §G2c r2
        // F3) rather than a locally-reproduced copy of its rule (M11 D1/D3 —
        // the guard's assertion target moves from the string's *shape* to
        // the production judge's *consequence*, guard-shape-vs-consequence).
        let policy = CmdPolicy::default_allowlist();
        let checks = default_dev_cmd_checks_rust();
        assert!(!checks.is_empty());
        for c in &checks {
            assert_eq!(policy.vet_run(&c.run), Ok(()));
            assert_eq!(c.expect, "exit 0");
        }
    }

    #[test]
    fn default_dev_cmd_checks_node_satisfies_the_positional_allowlist() {
        let policy = CmdPolicy::default_allowlist();
        let checks = default_dev_cmd_checks_node();
        assert!(!checks.is_empty());
        for c in &checks {
            assert_eq!(policy.vet_run(&c.run), Ok(()));
            assert_eq!(c.expect, "exit 0");
        }
    }

    /// A fresh project dir under `.crew-test/` (repo convention, never
    /// `/tmp`), removed on drop — pass or panic. `Drop` removes exactly the
    /// one path it owns, never its parent.
    struct DetectRoot(PathBuf);

    impl DetectRoot {
        fn new(label: &str) -> Self {
            let dir = std::env::current_dir()
                .unwrap()
                .join(".crew-test")
                .join(format!("detect-{label}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("test setup: detect root");
            Self(dir)
        }

        fn touch(&self, name: &str) {
            self.write(name, "");
        }

        fn write(&self, name: &str, contents: &str) {
            std::fs::write(self.0.join(name), contents).expect("test setup: marker file");
        }
    }

    impl Drop for DetectRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pairs(checks: &[CmdCheck]) -> Vec<(&str, &str)> {
        checks
            .iter()
            .map(|c| (c.run.as_str(), c.expect.as_str()))
            .collect()
    }

    #[test]
    fn detect_dev_cmd_checks_selects_the_rust_preset_for_a_cargo_toml() {
        let root = DetectRoot::new("rust");
        root.touch("Cargo.toml");
        assert_eq!(
            pairs(&detect_dev_cmd_checks(&root.0)),
            vec![("cargo test", "exit 0")]
        );
    }

    const BOTH_SCRIPTS: &str = r#"{"scripts":{"build":"vite build","test":"vitest run"}}"#;

    #[test]
    fn detect_dev_cmd_checks_selects_npm_test_then_build_when_both_scripts_exist() {
        let root = DetectRoot::new("node");
        root.write("package.json", BOTH_SCRIPTS);
        assert_eq!(
            pairs(&detect_dev_cmd_checks(&root.0)),
            vec![("npm test", "exit 0"), ("npm run build", "exit 0")]
        );
        assert_eq!(
            detect_dev_cmd_checks(&root.0),
            default_dev_cmd_checks_node(),
            "both scripts must yield exactly the node preset"
        );
    }

    #[test]
    fn detect_dev_cmd_checks_arms_nothing_for_a_package_json_without_scripts() {
        let root = DetectRoot::new("node-no-scripts");
        root.write("package.json", r#"{"name":"x"}"#);
        assert!(detect_dev_cmd_checks(&root.0).is_empty());
    }

    #[test]
    fn detect_dev_cmd_checks_arms_only_the_declared_non_empty_script() {
        let root = DetectRoot::new("node-build-only");
        root.write(
            "package.json",
            r#"{"scripts":{"build":"tsc","test":"","lint":"eslint ."}}"#,
        );
        assert_eq!(
            pairs(&detect_dev_cmd_checks(&root.0)),
            vec![("npm run build", "exit 0")]
        );
    }

    #[test]
    fn detect_dev_cmd_checks_ignores_a_non_string_or_non_object_scripts_value() {
        let root = DetectRoot::new("node-odd");
        root.write("package.json", r#"{"scripts":{"test":1,"build":["tsc"]}}"#);
        assert!(detect_dev_cmd_checks(&root.0).is_empty());
        root.write("package.json", r#"["not","an","object"]"#);
        assert!(detect_dev_cmd_checks(&root.0).is_empty());
    }

    #[test]
    fn detect_dev_cmd_checks_skips_malformed_package_json_but_keeps_rust() {
        let root = DetectRoot::new("node-malformed");
        root.write("package.json", r#"{"scripts":{"test":"vitest""#);
        assert!(
            detect_dev_cmd_checks(&root.0).is_empty(),
            "malformed JSON -> no node checks"
        );
        root.touch("Cargo.toml");
        assert_eq!(
            pairs(&detect_dev_cmd_checks(&root.0)),
            vec![("cargo test", "exit 0")]
        );
    }

    #[test]
    fn detect_dev_cmd_checks_orders_rust_before_node_when_both_markers_exist() {
        let root = DetectRoot::new("both");
        root.write("package.json", BOTH_SCRIPTS);
        root.touch("Cargo.toml");
        assert_eq!(
            pairs(&detect_dev_cmd_checks(&root.0)),
            vec![
                ("cargo test", "exit 0"),
                ("npm test", "exit 0"),
                ("npm run build", "exit 0")
            ]
        );
    }

    #[test]
    fn detect_dev_cmd_checks_is_empty_without_markers_even_if_a_subdirectory_has_one() {
        let root = DetectRoot::new("neither");
        root.touch("README.md");
        std::fs::create_dir_all(root.0.join("sub")).unwrap();
        std::fs::write(root.0.join("sub").join("Cargo.toml"), "").unwrap();
        assert!(
            detect_dev_cmd_checks(&root.0).is_empty(),
            "no root marker, no recursion"
        );
    }

    #[test]
    fn detect_dev_cmd_checks_is_empty_for_a_missing_root() {
        let root = DetectRoot::new("missing");
        let missing = root.0.join("does-not-exist");
        assert!(detect_dev_cmd_checks(&missing).is_empty());
    }

    #[test]
    fn detect_dev_cmd_checks_ignores_a_cargo_toml_that_is_a_directory() {
        let root = DetectRoot::new("marker-dir");
        std::fs::create_dir_all(root.0.join("Cargo.toml")).unwrap();
        assert!(
            detect_dev_cmd_checks(&root.0).is_empty(),
            "a directory named Cargo.toml is not a marker"
        );
    }

    #[test]
    fn cmd_policy_refuses_a_trailing_flag_as_a_negative_control() {
        // Negative control (testing-quality-tests-that-cannot-fail): proves
        // the production judge can actually refuse, guarding against a
        // helper/policy that accepts everything (M11 D2 — the negative
        // control itself is kept, only its assertion mechanism changes).
        let policy = CmdPolicy::default_allowlist();
        assert!(
            policy.vet_run("cargo test --workspace").is_err(),
            "a trailing flag must be refused"
        );
    }
}
