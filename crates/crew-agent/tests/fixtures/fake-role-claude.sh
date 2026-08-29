#!/bin/sh
# fake-role-claude.sh — deterministic stand-in for the real `claude` CLI's
# stream-json protocol, used only by RoleHarnessBehavior's Harness-test-
# double tests (crew-agent). Not the real claude CLI — that's t-m3e2e's
# --ignored E2E. Mirrors crew-agent's own tests/fixtures/fake-designer-claude.sh
# shape (itself mirroring crew-harness's tests/fixtures/fake-claude.sh) but is
# kept as its own file so M2's designer fixture stays untouched. Lets the
# caller control the assistant text verbatim (via FAKE_ASSISTANT_1/2,
# pre-built as full JSON lines) so tests can exercise JSON extraction and the
# M3 body contract's defensive-coercion path.
set -eu

mode="${FAKE_MODE:-json}"
session_id="${FAKE_SESSION_ID:-00000000-0000-4000-8000-000000000002}"

printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$session_id"

result_line() {
  printf '{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"%s","result":"ok"}\n' "$session_id"
}

while IFS= read -r _turn_line; do
  # Optionally record every raw turn line verbatim, so tests can assert on
  # what prompt text the harness actually sent per turn (M5 t-handoff D4,
  # with_injected_context) without teaching this fixture to parse JSON.
  if [ -n "${FAKE_CAPTURE_FILE:-}" ]; then
    printf '%s\n' "$_turn_line" >>"$FAKE_CAPTURE_FILE"
  fi
  case "$mode" in
    json)
      printf '%s\n' "$FAKE_ASSISTANT_1"
      result_line
      ;;
    unparseable)
      printf '%s\n' "$FAKE_ASSISTANT_1"
      result_line
      ;;
    turn_error)
      printf '{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","session_id":"%s","error":"forced failure"}\n' "$session_id"
      ;;
    slow)
      # M6 t-ctrl D2d pool test: sleeps before replying like `json`, so a
      # test can observe another turn genuinely blocked on the pool's
      # permit for this turn's whole duration (not just a race).
      sleep "${FAKE_SLEEP_SECONDS:-0.3}"
      printf '%s\n' "$FAKE_ASSISTANT_1"
      result_line
      ;;
    *)
      echo "fake-role-claude.sh: unknown FAKE_MODE '$mode'" >&2
      exit 1
      ;;
  esac
done
