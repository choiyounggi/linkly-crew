#!/bin/sh
# fake-designer-claude.sh — deterministic stand-in for the real `claude` CLI's
# stream-json protocol, used only by DesignerHarnessBehavior's Harness-test-
# double tests (crew-agent). Not the real claude CLI — that's t-m2loop's
# --ignored E2E. Mirrors crew-harness's own tests/fixtures/fake-claude.sh
# shape but lets the caller control the assistant text verbatim (via
# FAKE_ASSISTANT_1/2, pre-built as full JSON lines) so tests can exercise
# JSON extraction, code-fence stripping, and delta accumulation.
set -eu

mode="${FAKE_MODE:-json}"
session_id="${FAKE_SESSION_ID:-00000000-0000-4000-8000-000000000001}"

printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$session_id"

result_line() {
  printf '{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"%s","result":"ok"}\n' "$session_id"
}

while IFS= read -r _turn_line; do
  case "$mode" in
    json)
      printf '%s\n' "$FAKE_ASSISTANT_1"
      result_line
      ;;
    json_two_chunks)
      printf '%s\n' "$FAKE_ASSISTANT_1"
      printf '%s\n' "$FAKE_ASSISTANT_2"
      result_line
      ;;
    unparseable)
      printf '%s\n' "$FAKE_ASSISTANT_1"
      result_line
      ;;
    turn_error)
      printf '{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","session_id":"%s","error":"forced failure"}\n' "$session_id"
      ;;
    *)
      echo "fake-designer-claude.sh: unknown FAKE_MODE '$mode'" >&2
      exit 1
      ;;
  esac
done
