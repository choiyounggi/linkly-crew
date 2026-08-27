use serde_json::Value;

/// Common event shape every harness normalizes its CLI-specific stream into.
/// See DESIGN.md §2.3.
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessEvent {
    Started { session_id: String },
    Thinking,
    ToolUse { name: String, input: Value },
    Text { delta: String },
    Usage { input_tokens: u64, output_tokens: u64 },
    Finished { ok: bool, reason: Option<String> },
    Failed { error: String },
    Raw(Value),
}

/// Result of a single turn, produced by [`judge_result`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Success,
    Failed { error: String },
}

/// Judge a `type:"result"` event's outcome.
///
/// `claude -p` reports `subtype:"success"` even on failed turns, so this
/// function judges `is_error` first and `terminal_reason` second, and MUST
/// NEVER read `subtype` — DESIGN.md §2.2 rule 1.
///
/// `is_error` is real-CLI-verified to always be present, so it alone
/// decides when present. `terminal_reason` is a fallback for the case it
/// is somehow absent; `"completed"` is the only value real output has been
/// observed to carry on a successful turn (SPIKE.md 2026-08-27; also
/// `claude-cli-headless-success-signal` memory, 2026-08-25: a real auth
/// failure carried `is_error:true, subtype:"success", terminal_reason:
/// "api_error"` — confirming `terminal_reason` is not itself a fixed
/// "ok" token and must never be read as a success signal on its own).
pub fn judge_result(result: &Value) -> TurnOutcome {
    match result.get("is_error").and_then(Value::as_bool) {
        Some(true) => TurnOutcome::Failed {
            error: extract_error_message(result),
        },
        Some(false) => TurnOutcome::Success,
        None => match result.get("terminal_reason").and_then(Value::as_str) {
            Some("completed") => TurnOutcome::Success,
            Some(reason) => TurnOutcome::Failed {
                error: reason.to_string(),
            },
            None => TurnOutcome::Failed {
                error: "result event carried neither is_error nor terminal_reason".to_string(),
            },
        },
    }
}

fn extract_error_message(result: &Value) -> String {
    result
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| result.get("result").and_then(Value::as_str))
        .unwrap_or("claude-code reported is_error=true with no error detail")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_when_is_error_false_and_no_terminal_reason() {
        let result = json!({"type": "result", "subtype": "success", "is_error": false});
        assert_eq!(judge_result(&result), TurnOutcome::Success);
    }

    #[test]
    fn failed_when_is_error_true_even_with_subtype_success() {
        // The real CLI's documented failure mode: subtype says "success" but
        // is_error is true. judge_result must not be fooled by subtype.
        let result = json!({
            "type": "result",
            "subtype": "success",
            "is_error": true,
            "error": "tool denied by policy"
        });
        assert_eq!(
            judge_result(&result),
            TurnOutcome::Failed {
                error: "tool denied by policy".to_string()
            }
        );
    }

    #[test]
    fn success_when_subtype_says_error_but_is_error_false() {
        // Mirror case: subtype claims "error" but is_error is false and
        // terminal_reason is clean. A subtype-reading implementation would
        // wrongly return Failed here.
        let result = json!({
            "type": "result",
            "subtype": "error",
            "is_error": false,
            "terminal_reason": "completed"
        });
        assert_eq!(judge_result(&result), TurnOutcome::Success);
    }

    #[test]
    fn failed_via_terminal_reason_when_is_error_absent() {
        let result = json!({"type": "result", "terminal_reason": "timeout"});
        assert_eq!(
            judge_result(&result),
            TurnOutcome::Failed {
                error: "timeout".to_string()
            }
        );
    }

    #[test]
    fn success_via_terminal_reason_completed_when_is_error_absent() {
        let result = json!({"type": "result", "terminal_reason": "completed"});
        assert_eq!(judge_result(&result), TurnOutcome::Success);
    }

    #[test]
    fn real_auth_failure_sample_from_spike() {
        // Captured 2026-08-25 (claude-cli-headless-success-signal memory) and
        // reconfirmed 2026-08-27 (SPIKE.md): a real auth failure carries
        // subtype:"success" and terminal_reason:"api_error" alongside
        // is_error:true.
        let result = json!({
            "is_error": true,
            "subtype": "success",
            "terminal_reason": "api_error",
            "total_cost_usd": 0,
            "result": "Not logged in · Please run /login"
        });
        assert_eq!(
            judge_result(&result),
            TurnOutcome::Failed {
                error: "Not logged in · Please run /login".to_string()
            }
        );
    }

    #[test]
    fn failed_error_message_falls_back_to_result_field() {
        let result = json!({
            "type": "result",
            "is_error": true,
            "result": "permission denied"
        });
        assert_eq!(
            judge_result(&result),
            TurnOutcome::Failed {
                error: "permission denied".to_string()
            }
        );
    }
}
