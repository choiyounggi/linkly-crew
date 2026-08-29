//! `LlmLeadPlanner::specify` (contracts-m5.md §C3c) exercised against the
//! real `ClaudeCodeHarness` pointed at a fake CLI fixture — a Harness-trait
//! test double, not the real `claude` binary (that's this file's
//! `#[ignore]` real-CLI test). Mirrors crew-agent's
//! tests/designer_harness_behavior.rs technique: `Session` has no public
//! constructor outside crew-harness, so `ClaudeCodeHarness::with_binary` +
//! `with_env` is the only way to drive `specify` without a live CLI.

use std::path::PathBuf;
use std::sync::Arc;

use crew_harness::claude::ClaudeCodeHarness;
use crew_lead::plan::PlanError;
use crew_lead::plan_llm::LlmLeadPlanner;
use crew_proto::{ReqId, Requirement, SpecDoc};
use serde_json::json;

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-lead-claude.sh")
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

fn harness(mode: &str, assistant_1: &str) -> Arc<dyn crew_harness::Harness> {
    Arc::new(
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", mode)
            .with_env("FAKE_ASSISTANT_1", assistant_1),
    )
}

fn req(id: &str, text: &str) -> Requirement {
    Requirement {
        id: ReqId::new(id).unwrap(),
        text: text.to_string(),
    }
}

// --- normal path -----------------------------------------------------

#[tokio::test]
async fn specify_parses_valid_spec_doc_json_despite_surrounding_prose() {
    let body = json!({
        "goal": "shopping cart",
        "non_goals": ["payments"],
        "constraints": ["mobile responsive"],
        "requirements": [
            {"id": "REQ-1", "text": "cart page"},
            {"id": "REQ-2", "text": "checkout button"}
        ],
        "acceptance": ["all reqs covered"]
    })
    .to_string();
    // Leading/trailing prose (review r1 Finding 1 — e.g. a Stop hook tail)
    // must not stop extract_json's balanced-brace fallback from finding it.
    let text = format!("Sure, here you go:\n{body}\n\nLearning review: nothing to persist.");
    let h = harness("json", &assistant_line(&text));

    let spec = LlmLeadPlanner::specify(h, "shopping cart request")
        .await
        .expect("valid JSON reply must produce a SpecDoc");

    let expected = SpecDoc {
        goal: "shopping cart".to_string(),
        non_goals: vec!["payments".to_string()],
        constraints: vec!["mobile responsive".to_string()],
        requirements: vec![req("REQ-1", "cart page"), req("REQ-2", "checkout button")],
        acceptance: vec!["all reqs covered".to_string()],
    };
    assert_eq!(spec, expected);
}

#[tokio::test]
async fn specify_parses_code_fenced_json_reply() {
    let body = json!({
        "goal": "landing page",
        "non_goals": [],
        "constraints": [],
        "requirements": [{"id": "REQ-1", "text": "hero section"}],
        "acceptance": []
    })
    .to_string();
    let text = format!("```json\n{body}\n```");
    let h = harness("json", &assistant_line(&text));

    let spec = LlmLeadPlanner::specify(h, "landing page request")
        .await
        .expect("fenced JSON reply must produce a SpecDoc");

    assert_eq!(spec.goal, "landing page");
    assert_eq!(spec.requirements.len(), 1);
    assert_eq!(spec.requirements[0].id.as_str(), "REQ-1");
}

// --- error paths -------------------------------------------------------

#[tokio::test]
async fn specify_rejects_reply_with_no_json_object() {
    let h = harness("json", &assistant_line("I cannot comply with that request right now."));

    let err = LlmLeadPlanner::specify(h, "some request")
        .await
        .expect_err("a reply with no JSON object must be a PlanError");

    assert!(matches!(err, PlanError::LlmSpecify(_)));
}

#[tokio::test]
async fn specify_rejects_json_missing_the_requirements_field() {
    let body = json!({
        "goal": "landing page",
        "non_goals": [],
        "constraints": [],
        "acceptance": []
    })
    .to_string();
    let h = harness("json", &assistant_line(&body));

    let err = LlmLeadPlanner::specify(h, "some request")
        .await
        .expect_err("a reply missing the required \"requirements\" field must be a PlanError");

    assert!(matches!(err, PlanError::LlmSpecify(_)));
}

#[tokio::test]
async fn specify_reports_turn_failure_as_plan_error() {
    let h = harness("turn_error", "");

    let err = LlmLeadPlanner::specify(h, "some request")
        .await
        .expect_err("a failed harness turn must be a PlanError");

    match err {
        PlanError::LlmSpecify(msg) => assert!(msg.contains("forced failure")),
        other => panic!("expected PlanError::LlmSpecify, got {other:?}"),
    }
}

// --- boundary paths ------------------------------------------------------

#[tokio::test]
async fn specify_rejects_empty_requirements_array() {
    let body = json!({
        "goal": "landing page",
        "non_goals": [],
        "constraints": [],
        "requirements": [],
        "acceptance": []
    })
    .to_string();
    let h = harness("json", &assistant_line(&body));

    let err = LlmLeadPlanner::specify(h, "some request")
        .await
        .expect_err("an empty requirements array must be a PlanError, not an empty SpecDoc");

    assert!(matches!(err, PlanError::LlmSpecify(_)));
}

#[tokio::test]
async fn specify_rejects_a_requirement_id_violating_the_req_prefix_rule() {
    let body = json!({
        "goal": "landing page",
        "non_goals": [],
        "constraints": [],
        "requirements": [{"id": "NOT-A-REQ", "text": "hero section"}],
        "acceptance": []
    })
    .to_string();
    let h = harness("json", &assistant_line(&body));

    let err = LlmLeadPlanner::specify(h, "some request")
        .await
        .expect_err("a REQ- prefix violation must be a PlanError (ReqId enforces the rule)");

    assert!(matches!(err, PlanError::LlmSpecify(_)));
}

#[tokio::test]
async fn specify_rejects_whitespace_only_request_without_spawning_a_session() {
    // The fake CLI would error on any turn it actually receives (unknown
    // FAKE_MODE), so a passing test here proves the empty-request check
    // short-circuits before any harness call.
    let h = harness("unknown-mode-would-error", "");

    let err = LlmLeadPlanner::specify(h, "   ")
        .await
        .expect_err("a whitespace-only request must be rejected");

    assert_eq!(err, PlanError::EmptyRequest);
}

// --- real CLI (contract §C8 — add only, never run here) -----------------

#[tokio::test]
#[ignore]
async fn real_cli_specify_via_lead_harness_slot() {
    let h: Arc<dyn crew_harness::Harness> = Arc::new(ClaudeCodeHarness::new());
    let spec = LlmLeadPlanner::specify(h, "간단한 랜딩 페이지")
        .await
        .expect("real CLI specify should succeed");
    assert!(!spec.requirements.is_empty());
}
