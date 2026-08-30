//! `RoleHarnessBehavior::with_injected_context` (contracts-m5.md C4a, plan
//! t-handoff D4) — asserts on the *actual prompt text* the fake CLI
//! received, not just the envelope replies, since injection is a prompt-
//! assembly detail invisible to the M3 body contract. Reuses the
//! `fake-role-claude.sh` fixture from `tests/role_harness.rs` (D6: no new
//! fixture), extended with an opt-in `FAKE_CAPTURE_FILE` so this suite can
//! read back each turn's raw stdin line.

use std::path::PathBuf;
use std::sync::Arc;

use crew_agent::{RoleBehavior, RoleHarnessBehavior};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::AgentCfg;
use crew_proto::{ArtifactContract, DodCheck, Envelope, MessageKind, ReqId, Role, TaskSpec};
use serde_json::{json, Value};

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-role-claude.sh")
}

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
        model: None,
    }
}

/// A worktree-local capture path under `~/.linkly-crew/` (never `/tmp` —
/// forbidden by this repo's constraints), truncated before each test run.
fn capture_file(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME must be set");
    let dir = PathBuf::from(home).join(".linkly-crew").join("t-handoff-tests");
    std::fs::create_dir_all(&dir).expect("create capture dir");
    let path = dir.join(format!("{name}.ndjson"));
    let _ = std::fs::remove_file(&path);
    path
}

/// Extracts the `message.content[0].text` prompt text the fake CLI
/// received for each captured turn, in order. The file may not exist yet
/// if no turn has reached the harness — that's an empty capture, not an
/// error.
fn captured_prompts(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let value: Value = serde_json::from_str(line).expect("captured line must be JSON");
            value["message"]["content"][0]["text"]
                .as_str()
                .expect("captured turn must carry message.content[0].text")
                .to_string()
        })
        .collect()
}

fn assistant_line(text: &str) -> String {
    json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }
    })
    .to_string()
}

fn make_task_spec() -> TaskSpec {
    TaskSpec {
        id: "t1".to_string(),
        role: Role::Developer,
        title: "Build the landing page".to_string(),
        brief: "Build a simple landing page".to_string(),
        dod: vec![DodCheck::ReqCover {
            ids: vec![ReqId::new("REQ-1").unwrap()],
        }],
        deps: vec![],
        artifacts_expected: vec![ArtifactContract {
            name: "index.html".to_string(),
            kind: "code".to_string(),
            req_ids: vec![ReqId::new("REQ-1").unwrap()],
        }],
    }
}

fn task_assign(task: &TaskSpec) -> Envelope {
    Envelope::new(
        "sp-3".to_string(),
        "th-1".to_string(),
        "agent:lead".to_string(),
        vec!["agent:developer".to_string()],
        MessageKind::TaskAssign,
        None,
        "req_01".to_string(),
        json!({ "task": task }),
        vec![],
        true,
        900_000,
    )
}

fn harness(capture: &PathBuf) -> Arc<dyn crew_harness::Harness> {
    let text = r#"{"covered_req_ids": ["REQ-1"], "artifacts": []}"#;
    Arc::new(
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", "json")
            .with_env("FAKE_ASSISTANT_1", assistant_line(text))
            .with_env("FAKE_CAPTURE_FILE", capture.to_str().unwrap()),
    )
}

#[tokio::test]
async fn injected_context_prepends_once_on_the_first_turn_only() {
    let capture = capture_file("inject_first_turn_only");
    let mut behavior = RoleHarnessBehavior::new(
        harness(&capture),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    )
    .with_injected_context("HANDOFF_PACK_MARKER".to_string());

    let task = make_task_spec();

    let first = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(first[1].kind, MessageKind::TaskResult, "first turn must still succeed");

    let second = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(second[1].kind, MessageKind::TaskResult, "second turn must still succeed");

    let prompts = captured_prompts(&capture);
    assert_eq!(prompts.len(), 2, "expected exactly 2 captured turns");

    assert!(
        prompts[0].starts_with("HANDOFF_PACK_MARKER\n\n"),
        "injected text must prefix the first turn's prompt exactly once: {:?}",
        prompts[0]
    );
    assert!(
        !prompts[1].contains("HANDOFF_PACK_MARKER"),
        "injected text must not leak into a later turn: {:?}",
        prompts[1]
    );
}

#[tokio::test]
async fn injected_context_survives_a_build_prompt_error_and_still_lands_on_the_next_turn() {
    // Error case: a malformed TaskAssign (missing body["task"]) makes
    // build_prompt return Err before it ever reaches the
    // `injected_context.take()` line — the injection must NOT be silently
    // consumed by that failed attempt. A wrong implementation that takes
    // the injected text too early (e.g. at the top of build_prompt) would
    // lose it here and fail this test.
    let capture = capture_file("inject_survives_build_prompt_error");
    let mut behavior = RoleHarnessBehavior::new(
        harness(&capture),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    )
    .with_injected_context("HANDOFF_PACK_MARKER".to_string());

    let malformed = Envelope::new(
        "sp-3".to_string(),
        "th-1".to_string(),
        "agent:lead".to_string(),
        vec!["agent:developer".to_string()],
        MessageKind::TaskAssign,
        None,
        "req_01".to_string(),
        json!({}), // no "task" key
        vec![],
        true,
        900_000,
    );
    let error_replies = behavior.on_envelope(malformed).await;
    assert_eq!(
        error_replies[0].kind,
        MessageKind::Blocked,
        "malformed task.assign must be blocked before any harness turn runs"
    );
    assert!(
        captured_prompts(&capture).is_empty(),
        "a build_prompt error must never reach the harness at all"
    );

    let task = make_task_spec();
    let ok_replies = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(ok_replies[1].kind, MessageKind::TaskResult);

    let prompts = captured_prompts(&capture);
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].starts_with("HANDOFF_PACK_MARKER\n\n"),
        "injected text must still land on the first turn that actually reaches the harness: {:?}",
        prompts[0]
    );
}

#[tokio::test]
async fn no_injected_context_leaves_the_prompt_unchanged() {
    // Regression: without with_injected_context, build_prompt's existing
    // behavior must be byte-for-byte the same as before D4.
    let capture = capture_file("inject_regression_no_injection");
    let mut with_injection = RoleHarnessBehavior::new(
        harness(&capture),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );
    // Deliberately not calling with_injected_context.

    let task = make_task_spec();
    let replies = with_injection.on_envelope(task_assign(&task)).await;
    assert_eq!(replies[1].kind, MessageKind::TaskResult);

    let prompts = captured_prompts(&capture);
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].starts_with("You are the Developer.\n\n"),
        "prompt must start with system_hint, not an injected prefix: {:?}",
        prompts[0]
    );
    assert!(!prompts[0].contains("HANDOFF_PACK_MARKER"));
}
