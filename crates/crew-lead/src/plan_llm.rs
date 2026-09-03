//! DESIGN.md §4.3 LLM-driven specification (contracts-m5.md §C3c): the
//! `RealCli` counterpart to `plan.rs`'s `LeadPlanner::specify` — same
//! [`SpecDoc`] output contract, driven by a real CLI harness turn instead
//! of a deterministic template. Reuses crew-agent's `extract_json`
//! balanced-brace fallback (re-exported `pub`, logic unchanged) to survive
//! prose contamination around the model's JSON reply (review r1 Finding
//! 1). A parse/validation failure is reported as `PlanError` — no silent
//! fallback to the deterministic template.

use std::sync::Arc;
use std::time::Duration;

use crew_agent::extract_json;
use crew_harness::{AgentCfg, Harness, HarnessEvent, TurnOutcome, UserTurn, DEFAULT_TURN_TIMEOUT};
use crew_proto::SpecDoc;
use tokio::sync::mpsc;

use crate::plan::PlanError;

/// LLM-driven counterpart to `LeadPlanner` (deterministic template, kept
/// unchanged in `plan.rs` for the `Scripted` path).
pub struct LlmLeadPlanner;

impl LlmLeadPlanner {
    /// Same signature/output contract as `LeadPlanner::specify`
    /// (contracts-m5.md §C3c verbatim) with `DEFAULT_TURN_TIMEOUT`. See
    /// `specify_with_timeout` for the body and the run-configurable knob
    /// (M13 turn-recovery fix D3).
    pub async fn specify(harness: Arc<dyn Harness>, request: &str) -> Result<SpecDoc, PlanError> {
        Self::specify_with_timeout(harness, request, DEFAULT_TURN_TIMEOUT).await
    }

    /// Spawns a fresh session on `harness`, sends one turn asking for
    /// structured JSON (bounded by `timeout`), and parses the reply into a
    /// [`SpecDoc`]. Real-CLI only — never called from worker/CI tests (those
    /// use fake-CLI fixtures via `ClaudeCodeHarness::with_binary`, see
    /// `crew-lead/tests/m5_plan_llm.rs`).
    pub async fn specify_with_timeout(harness: Arc<dyn Harness>, request: &str, timeout: Duration) -> Result<SpecDoc, PlanError> {
        let goal = request.trim();
        if goal.is_empty() {
            return Err(PlanError::EmptyRequest);
        }

        let cfg = AgentCfg {
            cwd: std::env::current_dir().map_err(|e| PlanError::LlmSpecify(format!("cwd: {e}")))?,
            model: None,
        };
        let mut session = harness
            .spawn(&cfg)
            .await
            .map_err(|e| PlanError::LlmSpecify(format!("spawn failed: {e}")))?;
        let events_rx = harness.take_events(&mut session);

        // Drain concurrently with send() — same ordering crew-agent's
        // harness_behavior.rs::drain_until_terminal documents (함정 7): the
        // events channel has capacity 64, and a turn producing more events
        // than that before its result line deadlocks the reader (and so
        // `send()`) unless something drains concurrently rather than after.
        let drain = tokio::spawn(drain_until_terminal(events_rx));

        let outcome = harness
            .send(
                &mut session,
                UserTurn {
                    text: build_prompt(goal),
                },
                timeout,
            )
            .await;

        let (_events_rx, text) = drain
            .await
            .map_err(|e| PlanError::LlmSpecify(format!("drain task panicked: {e}")))?;

        // Best-effort: `specify` is a one-shot call (no session reuse
        // across turns, unlike `RoleHarnessBehavior`), so the session is
        // always torn down here regardless of outcome.
        let _ = harness.shutdown(session).await;

        match outcome {
            Ok(TurnOutcome::Success) => parse_spec_doc(&text),
            Ok(TurnOutcome::Failed { error }) => Err(PlanError::LlmSpecify(format!("turn failed: {error}"))),
            Err(e) => Err(PlanError::LlmSpecify(format!("harness send error: {e}"))),
        }
    }
}

/// Drains `events` until the turn's terminal event (or channel close),
/// accumulating `Text` deltas — see `specify`'s doc comment for why this
/// must run concurrently with `send()`.
async fn drain_until_terminal(mut events: mpsc::Receiver<HarnessEvent>) -> (mpsc::Receiver<HarnessEvent>, String) {
    let mut text = String::new();
    loop {
        match events.recv().await {
            Some(HarnessEvent::Text { delta }) => text.push_str(&delta),
            Some(HarnessEvent::Finished { .. }) | Some(HarnessEvent::Failed { .. }) => break,
            Some(_) => continue,
            None => break,
        }
    }
    (events, text)
}

/// Structured-JSON prompt (D4): forces the model to reply with exactly the
/// `SpecDoc` shape, `REQ-`-numbered sequential requirement ids, and no
/// tool use (SPIKE-M2 Finding 1 — a system-hint-only instruction is not
/// reliable enough).
fn build_prompt(goal: &str) -> String {
    format!(
        "You are the Lead planner for a small crew. Turn the following one-line request into a structured specification.\n\n\
        Request: {goal}\n\n\
        Do not use tools, do not read or write files, do not run commands, answer immediately.\n\n\
        reply ONLY with JSON matching exactly this shape (no other text before or after):\n\
        {{\"goal\": \"...\", \"non_goals\": [\"...\"], \"constraints\": [\"...\"], \"requirements\": [{{\"id\": \"REQ-1\", \"text\": \"...\"}}, {{\"id\": \"REQ-2\", \"text\": \"...\"}}], \"acceptance\": [\"...\"]}}\n\n\
        requirements must not be empty, and every requirement id must be \"REQ-1\", \"REQ-2\", ... in order with no gaps."
    )
}

/// Extracts and validates a `SpecDoc` from the model's raw reply text.
/// Failure at any step (no JSON object found, missing/malformed fields —
/// `ReqId`'s `TryFrom<String>` enforces the `REQ-` prefix during
/// deserialization — or an empty `requirements` list) is reported as
/// `PlanError::LlmSpecify`, never a silent fallback to the deterministic
/// template.
fn parse_spec_doc(text: &str) -> Result<SpecDoc, PlanError> {
    let value = extract_json(text).ok_or_else(|| PlanError::LlmSpecify("no parseable JSON object in reply".to_string()))?;
    let spec: SpecDoc = serde_json::from_value(value)
        .map_err(|e| PlanError::LlmSpecify(format!("SpecDoc deserialize failed: {e}")))?;
    if spec.requirements.is_empty() {
        return Err(PlanError::LlmSpecify("requirements must not be empty".to_string()));
    }
    Ok(spec)
}
