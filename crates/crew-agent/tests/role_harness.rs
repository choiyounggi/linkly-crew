//! `RoleHarnessBehavior` (M3, generalizing `DesignerHarnessBehavior` — plan
//! t-harness) exercised against the real `ClaudeCodeHarness` pointed at a
//! fake CLI fixture — a Harness-trait test double, not the real `claude`
//! binary (that's t-m3e2e's --ignored E2E). Mirrors
//! `crates/crew-agent/tests/designer_harness_behavior.rs`'s technique:
//! `ClaudeCodeHarness::with_binary` + `with_env`, since `Session` has no
//! public constructor outside crew-harness.

use std::path::PathBuf;
use std::sync::Arc;

use crew_agent::{RoleBehavior, RoleHarnessBehavior};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::AgentCfg;
use crew_proto::{ArtifactContract, DodCheck, Envelope, MessageKind, ReqId, Role, TaskSpec};
use serde_json::json;

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-role-claude.sh")
}

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
        model: None,
    }
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

/// One assistant line whose content array carries `filler_count` non-text
/// blocks ahead of the real text block, so a single stdout line produces
/// `filler_count + 1` `HarnessEvent`s in one burst — used to prove the
/// events channel (capacity 64) does not deadlock (D2).
fn assistant_line_with_filler(filler_count: usize, text: &str) -> String {
    let mut content: Vec<serde_json::Value> =
        (0..filler_count).map(|_| json!({"type": "thinking"})).collect();
    content.push(json!({"type": "text", "text": text}));
    json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": content,
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }
    })
    .to_string()
}

fn make_task_spec(cover_ids: &[&str], artifacts: &[(&str, &str, &[&str])]) -> TaskSpec {
    let dod = if cover_ids.is_empty() {
        vec![]
    } else {
        vec![DodCheck::ReqCover {
            ids: cover_ids.iter().map(|id| ReqId::new(*id).unwrap()).collect(),
        }]
    };
    TaskSpec {
        id: "t1".to_string(),
        role: Role::Developer,
        title: "Build the landing page".to_string(),
        brief: "Build a simple landing page with signup".to_string(),
        dod,
        deps: vec![],
        artifacts_expected: artifacts
            .iter()
            .map(|(name, kind, req_ids)| ArtifactContract {
                name: name.to_string(),
                kind: kind.to_string(),
                req_ids: req_ids.iter().map(|id| ReqId::new(*id).unwrap()).collect(),
            })
            .collect(),
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

fn harness(mode: &str, assistant_1: &str) -> Arc<dyn crew_harness::Harness> {
    Arc::new(
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", mode)
            .with_env("FAKE_ASSISTANT_1", assistant_1),
    )
}

#[tokio::test]
async fn normal_assign_produces_ack_and_result_with_m3_body() {
    let text = r#"{"covered_req_ids": ["REQ-1"], "artifacts": [{"name": "index.html", "kind": "code", "req_ids": ["REQ-1"], "content": "<html></html>"}]}"#;
    let h = harness("json", &assistant_line(text));
    let mut behavior = RoleHarnessBehavior::new(
        h,
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );

    let task = make_task_spec(&["REQ-1"], &[("index.html", "code", &["REQ-1"])]);
    let replies = behavior.on_envelope(task_assign(&task)).await;

    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].kind, MessageKind::TaskAck);
    assert_eq!(replies[0].body, json!({}));
    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!(["REQ-1"]));
    assert_eq!(
        replies[1].body["artifacts"][0]["name"],
        json!("index.html")
    );
    assert_eq!(replies[1].body["artifacts"][0]["content"], json!("<html></html>"));
    assert_eq!(replies[1].from, "agent:developer");
    assert_eq!(replies[1].to, vec!["agent:lead".to_string()]);
}

#[tokio::test]
async fn missing_result_keys_are_defensively_coerced_to_empty_arrays() {
    // The model replied with a bare JSON object missing both contract keys —
    // D4's defensive coercion must fill them with empty arrays rather than
    // treating this as a parse failure (Blocked).
    let text = r#"{"note": "done, nothing to cover"}"#;
    let h = harness("json", &assistant_line(text));
    let mut behavior = RoleHarnessBehavior::new(
        h,
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );

    let task = make_task_spec(&["REQ-1"], &[]);
    let replies = behavior.on_envelope(task_assign(&task)).await;

    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!([]));
    assert_eq!(replies[1].body["artifacts"], json!([]));
}

#[tokio::test]
async fn nonjson_reply_returns_blocked() {
    let h = harness("unparseable", &assistant_line("not json at all"));
    let mut behavior = RoleHarnessBehavior::new(
        h,
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );

    let task = make_task_spec(&["REQ-1"], &[]);
    let replies = behavior.on_envelope(task_assign(&task)).await;

    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].kind, MessageKind::Blocked);
}

#[tokio::test]
async fn turn_failure_returns_blocked() {
    let h = harness("turn_error", "");
    let mut behavior = RoleHarnessBehavior::new(
        h,
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );

    let task = make_task_spec(&["REQ-1"], &[]);
    let replies = behavior.on_envelope(task_assign(&task)).await;

    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].kind, MessageKind::Blocked);
    assert!(replies[0].body["reason"]
        .as_str()
        .unwrap()
        .contains("forced failure"));
}

#[tokio::test]
async fn empty_req_cover_boundary_task_still_produces_reply() {
    // Boundary: a TaskSpec whose dod carries no ReqCover check at all (so the
    // covered-goal union build_prompt lists is empty) — the prompt must
    // still build and the turn must still map to a reply, not Blocked on the
    // request side.
    let text = r#"{"covered_req_ids": [], "artifacts": []}"#;
    let h = harness("json", &assistant_line(text));
    let mut behavior = RoleHarnessBehavior::new(
        h,
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );

    let task = make_task_spec(&[], &[]);
    let replies = behavior.on_envelope(task_assign(&task)).await;

    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].kind, MessageKind::TaskAck);
    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!([]));
    assert_eq!(replies[1].body["artifacts"], json!([]));
}

// build_prompt is private — its no-tool-use/JSON-schema hardcoding is
// asserted by a #[cfg(test)] unit test inside src/harness_behavior.rs
// instead (see build_prompt_hardcodes_no_tool_use_and_json_schema_instructions
// there), not here.

#[tokio::test]
async fn drain_does_not_deadlock_when_events_exceed_channel_capacity() {
    // crew-harness's events channel capacity is 64
    // (crates/crew-harness/src/claude.rs EVENTS_CHANNEL_CAPACITY). D2 fixes
    // tokio::spawn(drain_until_terminal(...)) *before* harness.send() so a
    // turn producing more than 64 events before its result line cannot
    // deadlock the reader task. This assistant line carries 100 filler
    // blocks + 1 real text block (101 events) to force that condition.
    let text = r#"{"covered_req_ids": ["REQ-1"], "artifacts": []}"#;
    let assistant = assistant_line_with_filler(100, text);
    let h = harness("json", &assistant);
    let mut behavior = RoleHarnessBehavior::new(
        h,
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );

    let task = make_task_spec(&["REQ-1"], &[]);
    let replies = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        behavior.on_envelope(task_assign(&task)),
    )
    .await
    .expect("on_envelope must not deadlock when a turn emits more events than the channel capacity");

    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!(["REQ-1"]));
}
