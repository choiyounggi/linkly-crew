//! M5 sprint compression (t-compress brief, contract C3b): proves
//! `summarize_sprint` is a deterministic, budget-bounded template —
//! accepted/blocked/escalated tasks land in the right section, the
//! `text.chars().count() <= budget_chars` invariant holds even under a
//! budget smaller than the fixed header, and empty/unknown-id inputs never
//! panic. `crew-lead/tests/{m3_sprint.rs,m5_escalation.rs}` are untouched;
//! this is a new file per the brief's scope boundary.

use std::collections::HashMap;

use crew_lead::compress::summarize_sprint;
use crew_lead::dispatch::TaskState;
use crew_proto::{Envelope, MessageKind, SpecDoc};
use serde_json::json;

fn spec_with_goal(goal: &str) -> SpecDoc {
    SpecDoc {
        goal: goal.to_string(),
        non_goals: vec![],
        constraints: vec![],
        requirements: vec![],
        acceptance: vec![],
    }
}

fn change_request(violations: Vec<&str>, reason: &str) -> Envelope {
    Envelope::new(
        "sp-1".to_string(),
        "th-1".to_string(),
        "agent:designer".to_string(),
        vec!["agent:lead".to_string()],
        MessageKind::ChangeRequest,
        None,
        "req_1".to_string(),
        json!({ "violations": violations, "reason": reason }),
        vec![],
        false,
        900_000,
    )
}

fn non_change_request() -> Envelope {
    Envelope::new(
        "sp-1".to_string(),
        "th-1".to_string(),
        "agent:developer".to_string(),
        vec!["agent:lead".to_string()],
        MessageKind::TaskResult,
        None,
        "req_2".to_string(),
        json!({ "artifact": "x" }),
        vec![],
        false,
        900_000,
    )
}

/// Normal case: 3 accepted + 1 escalated + 1 blocked task, 2 change
/// requests (one non-CR message that must be filtered out) — every task
/// lands in exactly its section, and two calls with the same inputs
/// produce byte-identical output (determinism is not incidental on
/// `HashMap` iteration order, since the `states` map is looked up by key,
/// never iterated for ordering).
#[test]
fn classifies_accepted_blocked_and_change_requests_and_is_deterministic() {
    let spec = spec_with_goal("로그인 되는 랜딩 페이지");
    let sprint = vec![
        "t1".to_string(),
        "t2".to_string(),
        "t3".to_string(),
        "t4".to_string(),
        "t5".to_string(),
    ];
    let mut states = HashMap::new();
    states.insert("t1".to_string(), TaskState::Accepted);
    states.insert("t2".to_string(), TaskState::Accepted);
    states.insert("t3".to_string(), TaskState::Accepted);
    states.insert("t4".to_string(), TaskState::Escalated);
    states.insert("t5".to_string(), TaskState::Blocked);
    let messages = vec![
        change_request(vec!["REQ-1"], "누락된 필드"),
        non_change_request(),
        change_request(vec!["REQ-2", "REQ-3"], "레이아웃 깨짐"),
    ];

    let first = summarize_sprint(3, &spec, &sprint, &states, &messages, 4_000);
    let second = summarize_sprint(3, &spec, &sprint, &states, &messages, 4_000);
    assert_eq!(first, second, "same inputs must produce byte-identical output");

    let text = &first.text;
    assert_eq!(first.index, 3);
    assert!(text.contains("## 스프린트 3 요약"));
    assert!(text.contains("로그인 되는 랜딩 페이지"));

    let accepted_section = section(text, "### 수락된 태스크", "### 차단/에스컬레이션");
    assert!(accepted_section.contains("- t1"));
    assert!(accepted_section.contains("- t2"));
    assert!(accepted_section.contains("- t3"));
    assert!(!accepted_section.contains("t4"));
    assert!(!accepted_section.contains("t5"));

    let blocked_section = section(text, "### 차단/에스컬레이션", "### 변경 요청 요지");
    assert!(blocked_section.contains("t4: escal"));
    assert!(blocked_section.contains("t5: blocked"));
    assert!(!blocked_section.contains("t1"));

    let cr_section = section(text, "### 변경 요청 요지", "### 다음 스프린트 주의");
    assert!(cr_section.contains("REQ-1 — 누락된 필드"));
    assert!(cr_section.contains("REQ-2, REQ-3 — 레이아웃 깨짐"));

    let next_section = &text[text.find("### 다음 스프린트 주의").unwrap()..];
    assert!(next_section.contains("t4"));
    assert!(next_section.contains("t5"));
}

/// Extreme: a `change_request` body missing `violations`/`reason` is
/// marked `(형식 외 body)` rather than skipped (plan D3 — audit
/// visibility), and a budget smaller than even the fixed header still
/// upholds the `chars().count() <= budget_chars` invariant without
/// panicking, including with Korean (multi-byte) content in the goal so
/// truncation cannot land mid-character.
#[test]
fn malformed_change_request_body_is_marked_and_tiny_budget_never_panics_or_exceeds() {
    let spec = spec_with_goal("한글 목표 문자열로 예산 절단 경계를 검증한다 매우 길게 길게");
    let sprint = vec!["t1".to_string()];
    let mut states = HashMap::new();
    states.insert("t1".to_string(), TaskState::Accepted);
    let malformed = Envelope::new(
        "sp-1".to_string(),
        "th-1".to_string(),
        "agent:designer".to_string(),
        vec!["agent:lead".to_string()],
        MessageKind::ChangeRequest,
        None,
        "req_1".to_string(),
        json!({ "unexpected": "shape" }),
        vec![],
        false,
        900_000,
    );
    let messages = vec![malformed];

    // Generous budget: malformed body still shows up, marked.
    let generous = summarize_sprint(1, &spec, &sprint, &states, &messages, 4_000);
    assert!(generous.text.contains("(형식 외 body)"));

    for budget in [0usize, 1, 5, 10, 50] {
        let summary = summarize_sprint(1, &spec, &sprint, &states, &messages, budget);
        assert!(
            summary.text.chars().count() <= budget,
            "budget={budget} exceeded: {} chars",
            summary.text.chars().count()
        );
    }
}

/// Order-sensitive: when a change-request bullet must be dropped to fit
/// the budget, the OLDEST message (first in `messages`) is dropped, not
/// the newest (plan D2). A plain char-count sweep can't distinguish "drop
/// oldest" from "drop newest" — both satisfy the raw invariant — so this
/// pins the exact degraded text: the budgeted two-message call must equal
/// the natural output of the newest message alone.
#[test]
fn drops_oldest_change_request_first_under_tight_budget() {
    let spec = spec_with_goal("드랍 순서 테스트");
    let sprint = vec!["t1".to_string()];
    let mut states = HashMap::new();
    states.insert("t1".to_string(), TaskState::Accepted);
    let old_cr = change_request(vec!["OLDMARKER"], "오래된 변경 요청");
    let new_cr = change_request(vec!["NEWMARKER"], "최신 변경 요청");

    let newest_only = summarize_sprint(
        1,
        &spec,
        &sprint,
        &states,
        std::slice::from_ref(&new_cr),
        10_000,
    );
    let target_budget = newest_only.text.chars().count();

    let both = summarize_sprint(1, &spec, &sprint, &states, &[old_cr, new_cr], target_budget);

    assert_eq!(
        both.text, newest_only.text,
        "must degrade to exactly the newest-only summary once the oldest CR is dropped"
    );
    assert!(both.text.contains("NEWMARKER"));
    assert!(!both.text.contains("OLDMARKER"));
}

/// Boundary: empty `messages` and empty `sprint` never panic and still
/// produce a valid (non-empty) summary; a `sprint` id absent from `states`
/// is treated as unrecorded and appears in neither the accepted nor the
/// blocked/escalation section.
#[test]
fn empty_sprint_and_messages_and_unknown_task_id_do_not_panic() {
    let spec = spec_with_goal("빈 스프린트");
    let empty: HashMap<String, TaskState> = HashMap::new();

    let empty_sprint = summarize_sprint(1, &spec, &[], &empty, &[], 4_000);
    assert!(!empty_sprint.text.is_empty());
    assert!(empty_sprint.text.contains("### 수락된 태스크\n없음"));
    assert!(empty_sprint.text.contains("### 차단/에스컬레이션\n없음"));
    assert!(empty_sprint.text.contains("### 변경 요청 요지\n없음"));
    assert!(empty_sprint.text.contains("### 다음 스프린트 주의\n없음"));

    let unknown_sprint = vec!["ghost-task".to_string()];
    let unknown = summarize_sprint(1, &spec, &unknown_sprint, &empty, &[], 4_000);
    assert!(!unknown.text.contains("ghost-task"));
    assert!(unknown.text.chars().count() <= 4_000);
}

/// Slices out the text between two section headers (exclusive of the
/// closing header) for section-scoped assertions above.
fn section<'a>(text: &'a str, start_header: &str, end_header: &str) -> &'a str {
    let start = text.find(start_header).unwrap();
    let end = text[start..].find(end_header).map(|i| start + i).unwrap();
    &text[start..end]
}
