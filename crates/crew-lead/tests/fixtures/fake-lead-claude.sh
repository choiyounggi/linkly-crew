#!/bin/sh
# fake-lead-claude.sh — deterministic stand-in for the real `claude` CLI's
# stream-json protocol, used only by LlmLeadPlanner::specify's Harness-test-
# double tests (crew-lead/tests/m5_plan_llm.rs). Not the real claude CLI —
# that's the #[ignore] real-CLI test in the same file. Trimmed copy of
# crew-agent/tests/fixtures/fake-role-claude.sh's shape (itself mirroring
# crew-harness's tests/fixtures/fake-claude.sh) kept as its own file per
# crate-boundary convention. Lets the caller control the assistant text
# verbatim via FAKE_ASSISTANT_1 (a pre-built full JSON line) so tests can
# exercise JSON extraction and SpecDoc validation.
set -eu

mode="${FAKE_MODE:-json}"
session_id="${FAKE_SESSION_ID:-00000000-0000-4000-8000-000000000003}"

printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$session_id"

while IFS= read -r _turn_line; do
  case "$mode" in
    json)
      printf '%s\n' "$FAKE_ASSISTANT_1"
      printf '{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"%s","result":"ok"}\n' "$session_id"
      ;;
    turn_error)
      printf '{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","session_id":"%s","error":"forced failure"}\n' "$session_id"
      ;;
    *)
      echo "fake-lead-claude.sh: unknown FAKE_MODE '$mode'" >&2
      exit 1
      ;;
  esac
done
