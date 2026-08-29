//! Sprint-boundary compression (t-compress brief, contract C3b): a pure,
//! deterministic template that turns a sprint's spec/state/message inputs
//! into an L1-budget-bounded summary. Called by t-multisprint at each
//! sprint boundary — this module never touches `LeadBehavior` state, only
//! reads the inputs it is given (DESIGN.md §5 three-layer context, L1
//! material).
//!
//! Section order is fixed (plan D1): 목표 -> 수락된 태스크 ->
//! 차단/에스컬레이션 -> 변경 요청 요지 -> 다음 스프린트 주의. Task listing
//! always follows the `sprint` slice's input order, never `states`'
//! `HashMap` iteration order, so two calls with the same inputs produce
//! byte-identical output (contract C3b determinism requirement).

use std::collections::HashMap;

use crew_proto::{Envelope, MessageKind, SpecDoc};

use crate::dispatch::TaskState;

/// One sprint's compressed summary (contract C3b, verbatim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SprintSummary {
    pub index: u32,
    pub text: String,
}

/// Builds `sprint`'s deterministic summary within `budget_chars`
/// (contract C3b). Priority under budget pressure (plan D2): header +
/// 목표 + 수락 + 차단/에스컬레이션 + 다음 스프린트 주의 are always kept in
/// full; 변경 요청 요지 bullets are dropped oldest-first to make room; if
/// the result still exceeds `budget_chars` (e.g. a budget smaller than the
/// fixed sections), the whole text is hard-truncated on a char boundary as
/// a last resort. The `text.chars().count() <= budget_chars` invariant is
/// enforced at exactly one place, at the end of this function.
pub fn summarize_sprint(
    index: u32,
    spec: &SpecDoc,
    sprint: &[String],
    states: &HashMap<String, TaskState>,
    messages: &[Envelope],
    budget_chars: usize,
) -> SprintSummary {
    let header = format!("## 스프린트 {index} 요약\n\n");
    let goal_section = format!("### 목표\n{}\n\n", spec.goal);

    let accepted_ids: Vec<&String> = sprint
        .iter()
        .filter(|id| matches!(states.get(*id), Some(TaskState::Accepted)))
        .collect();
    let accepted_section = format!(
        "### 수락된 태스크\n{}\n\n",
        list_or_none(accepted_ids.iter().map(|id| id.as_str()))
    );

    // Escalated/Blocked ids, sprint-slice order — reused verbatim for both
    // the 차단/에스컬레이션 and 다음 스프린트 주의 sections (plan D1).
    let flagged: Vec<(&String, &'static str)> = sprint
        .iter()
        .filter_map(|id| match states.get(id) {
            Some(TaskState::Escalated) => Some((id, "escal")),
            Some(TaskState::Blocked) => Some((id, "blocked")),
            _ => None,
        })
        .collect();
    let blocked_section = format!(
        "### 차단/에스컬레이션\n{}\n\n",
        list_or_none(flagged.iter().map(|(id, label)| format!("{id}: {label}")))
    );
    let next_section = format!(
        "### 다음 스프린트 주의\n{}\n",
        list_or_none(flagged.iter().map(|(id, _)| id.as_str()))
    );

    let core = format!("{header}{goal_section}{accepted_section}{blocked_section}");

    let cr_lines: Vec<String> = messages
        .iter()
        .filter(|m| m.kind == MessageKind::ChangeRequest)
        .map(change_request_line)
        .collect();

    // Drop change-request bullets oldest-first (front of the slice) until
    // core + remaining CR section + next_section fits, or nothing is left.
    let mut dropped = 0;
    let mut assembled = assemble(&core, &cr_lines[dropped..], &next_section);
    while assembled.chars().count() > budget_chars && dropped < cr_lines.len() {
        dropped += 1;
        assembled = assemble(&core, &cr_lines[dropped..], &next_section);
    }

    let text = if assembled.chars().count() > budget_chars {
        assembled.chars().take(budget_chars).collect()
    } else {
        assembled
    };

    SprintSummary { index, text }
}

fn assemble(core: &str, cr_lines: &[String], next_section: &str) -> String {
    let cr_section = format!(
        "### 변경 요청 요지\n{}\n\n",
        list_or_none(cr_lines.iter().map(|s| s.as_str()))
    );
    format!("{core}{cr_section}{next_section}")
}

/// Renders an iterator of already-formatted lines as `- {line}` bullets,
/// or `없음` when empty — every list-shaped section in the template goes
/// through this so an empty section is never a blank gap in the text.
fn list_or_none<S: AsRef<str>>(items: impl Iterator<Item = S>) -> String {
    let mut out = String::new();
    for item in items {
        out.push_str("- ");
        out.push_str(item.as_ref());
        out.push('\n');
    }
    if out.is_empty() {
        "없음".to_string()
    } else {
        out.pop(); // drop the trailing newline; callers add their own section spacing
        out
    }
}

/// One `change_request` message's one-line summary (plan D3): joins
/// `body["violations"]` (array of strings) with `body["reason"]` (a
/// string). Either missing or a different shape marks the line
/// `(형식 외 body)` rather than skipping it, so a malformed message stays
/// auditable instead of silently vanishing.
fn change_request_line(msg: &Envelope) -> String {
    let violations = msg.body.get("violations").and_then(|v| v.as_array());
    let reason = msg.body.get("reason").and_then(|v| v.as_str());
    match (violations, reason) {
        (Some(violations), Some(reason)) => {
            let joined = violations
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{joined} — {reason}")
        }
        _ => "(형식 외 body)".to_string(),
    }
}
