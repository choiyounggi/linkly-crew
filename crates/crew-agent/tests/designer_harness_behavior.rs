//! `DesignerHarnessBehavior` (plan A8) exercised against the real
//! `ClaudeCodeHarness` pointed at a fake CLI fixture — a Harness-trait test
//! double, not the real `claude` binary (that's t-m2loop's --ignored E2E).
//! `Session` has no public constructor outside crew-harness, so
//! `ClaudeCodeHarness::with_binary` + `with_env` (the same technique
//! crew-harness's own tests/adapter.rs uses) is the only way to drive this
//! behavior without a live CLI.

use std::path::PathBuf;
use std::sync::Arc;

use crew_agent::{DesignerHarnessBehavior, RoleBehavior};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::AgentCfg;
use crew_proto::{Envelope, MessageKind};
use serde_json::json;

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-designer-claude.sh")
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
/// `filler_count + 1` `HarnessEvent`s in one burst.
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

fn task_assign(req_ids: &[&str]) -> Envelope {
    Envelope::new(
        "sp-3".to_string(),
        "th-1".to_string(),
        "agent:pm".to_string(),
        vec!["agent:designer".to_string()],
        MessageKind::TaskAssign,
        None,
        "req_01".to_string(),
        json!({"req_ids": req_ids, "brief": "build the login screen"}),
        vec![],
        true,
        900_000,
    )
}

fn harness(mode: &str, assistant_1: &str, assistant_2: &str) -> Arc<dyn crew_harness::Harness> {
    Arc::new(
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", mode)
            .with_env("FAKE_ASSISTANT_1", assistant_1)
            .with_env("FAKE_ASSISTANT_2", assistant_2),
    )
}

#[tokio::test]
async fn success_reply_produces_ack_and_result() {
    let text = r#"{"covered_req_ids": ["REQ-1"], "artifact": "design.md summary"}"#;
    let h = harness("json", &assistant_line(text), "");
    let mut behavior =
        DesignerHarnessBehavior::new(h, agent_cfg(), "You are the Designer.".to_string());

    let assign = task_assign(&["REQ-1"]);
    let replies = behavior.on_envelope(assign).await;

    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].kind, MessageKind::TaskAck);
    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!(["REQ-1"]));
    assert_eq!(replies[1].body["artifact"], json!("design.md summary"));
    assert_eq!(replies[1].from, "agent:designer");
    assert_eq!(replies[1].to, vec!["agent:pm".to_string()]);
}

#[tokio::test]
async fn code_fenced_reply_is_extracted() {
    let text = "```json\n{\"covered_req_ids\": [\"REQ-1\"], \"artifact\": \"ok\"}\n```";
    let h = harness("json", &assistant_line(text), "");
    let mut behavior = DesignerHarnessBehavior::new(h, agent_cfg(), "hint".to_string());

    let replies = behavior.on_envelope(task_assign(&["REQ-1"])).await;

    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!(["REQ-1"]));
}

#[tokio::test]
async fn text_deltas_are_accumulated_across_events_before_parsing() {
    // Neither chunk alone is valid JSON; only the concatenation is — this
    // fails unless take_events()'s stream is actually drained and joined.
    let chunk1 = "```json\n{\"covered_req_ids\": [\"REQ-1\"";
    let chunk2 = "], \"artifact\": \"ok\"}\n```";
    let h = harness(
        "json_two_chunks",
        &assistant_line(chunk1),
        &assistant_line(chunk2),
    );
    let mut behavior = DesignerHarnessBehavior::new(h, agent_cfg(), "hint".to_string());

    let replies = behavior.on_envelope(task_assign(&["REQ-1"])).await;

    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!(["REQ-1"]));
}

#[tokio::test]
async fn drain_does_not_deadlock_when_events_exceed_channel_capacity() {
    // crew-harness's events channel capacity is 64
    // (crates/crew-harness/src/claude.rs EVENTS_CHANNEL_CAPACITY). The
    // reader task pushes every event via a bounded `events_tx.send().await`
    // and only reaches the turn's result line (which unblocks
    // `Harness::send`) after every earlier event is sent — so a turn
    // producing more than 64 events before its result line deadlocks the
    // reader, and therefore `send()`, unless something drains the events
    // stream concurrently with send() rather than after it. This single
    // assistant line carries 100 filler blocks + 1 real text block (101
    // events) to force that condition.
    let text = r#"{"covered_req_ids": ["REQ-1"], "artifact": "ok"}"#;
    let assistant = assistant_line_with_filler(100, text);
    let h = harness("json", &assistant, "");
    let mut behavior = DesignerHarnessBehavior::new(h, agent_cfg(), "hint".to_string());

    let replies = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        behavior.on_envelope(task_assign(&["REQ-1"])),
    )
    .await
    .expect("on_envelope must not deadlock when a turn emits more events than the channel capacity");

    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!(["REQ-1"]));
}

#[tokio::test]
async fn unparseable_reply_returns_blocked() {
    let h = harness("unparseable", &assistant_line("not json at all"), "");
    let mut behavior = DesignerHarnessBehavior::new(h, agent_cfg(), "hint".to_string());

    let replies = behavior.on_envelope(task_assign(&["REQ-1"])).await;

    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].kind, MessageKind::Blocked);
}

#[tokio::test]
async fn failed_turn_returns_blocked() {
    let h = harness("turn_error", "", "");
    let mut behavior = DesignerHarnessBehavior::new(h, agent_cfg(), "hint".to_string());

    let replies = behavior.on_envelope(task_assign(&["REQ-1"])).await;

    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].kind, MessageKind::Blocked);
    assert!(replies[0].body["reason"]
        .as_str()
        .unwrap()
        .contains("forced failure"));
}

#[tokio::test]
async fn unrelated_kind_never_starts_a_harness_turn() {
    let h = harness("json", &assistant_line("{}"), "");
    let mut behavior = DesignerHarnessBehavior::new(h, agent_cfg(), "hint".to_string());

    let question = Envelope::new(
        "sp-3".to_string(),
        "th-1".to_string(),
        "agent:pm".to_string(),
        vec!["agent:designer".to_string()],
        MessageKind::Question,
        None,
        "req_01".to_string(),
        json!({}),
        vec![],
        false,
        900_000,
    );

    let replies = behavior.on_envelope(question).await;

    assert!(replies.is_empty());
}

#[tokio::test]
async fn is_done_is_always_false() {
    let h = harness("json", &assistant_line("{}"), "");
    let behavior = DesignerHarnessBehavior::new(h, agent_cfg(), "hint".to_string());

    assert!(!behavior.is_done());
}
