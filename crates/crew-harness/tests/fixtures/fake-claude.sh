#!/bin/sh
# fake-claude.sh — deterministic stand-in for the real `claude` CLI's
# stream-json protocol, for crew-harness's fake-CLI test suite (D7).
#
# FAKE_MODE=normal|error|timeout selects the canned response. Reads one
# line of stdin per turn (the input format is not inspected — the fixture
# only needs to prove the harness's parsing/timeout/judging logic, not
# real prompt handling).
set -eu

if [ -n "${FAKE_ARGV_LOG:-}" ]; then
  for a in "$@"; do printf '%s\n' "$a"; done > "$FAKE_ARGV_LOG"
fi

if [ -n "${FAKE_ENV_LOG:-}" ]; then
  env > "$FAKE_ENV_LOG"
fi

mode="${FAKE_MODE:-normal}"

# Mirrors the real CLI: with no override, the init line reports back the
# `--session-id <uuid>` the caller spawned with (crew-harness's normal
# spawn path always passes this), so a snapshot's "no report differs"
# case is exercised by default. FAKE_SESSION_ID still wins when a test
# wants the init line to report something else (M5 t-handoff D3).
cli_session_id=""
while [ $# -gt 0 ]; do
  case "$1" in
    --session-id)
      cli_session_id="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
session_id="${FAKE_SESSION_ID:-${cli_session_id:-00000000-0000-4000-8000-000000000000}}"

printf '{"type":"system","subtype":"init","session_id":"%s"}\n' "$session_id"

while IFS= read -r _turn_line; do
  case "$mode" in
    normal)
      printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":5}}}\n'
      # Field shape matches the real CLI (SPIKE.md 2026-08-27): terminal_reason
      # is "completed" on success, not "ok".
      printf '{"type":"result","subtype":"success","is_error":false,"terminal_reason":"completed","session_id":"%s","result":"ok"}\n' "$session_id"
      ;;
    error)
      printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"trying..."}],"usage":{"input_tokens":10,"output_tokens":5}}}\n'
      # The real CLI's documented failure mode (DESIGN.md §2.2, SPIKE.md
      # 2026-08-27): subtype still says "success" even though is_error is
      # true, and terminal_reason carries the real failure code (e.g.
      # "api_error"), not a generic "error" string.
      printf '{"type":"result","subtype":"success","is_error":true,"terminal_reason":"api_error","session_id":"%s","error":"forced error"}\n' "$session_id"
      ;;
    timeout)
      sleep 5
      printf '{"type":"result","subtype":"success","is_error":false,"session_id":"%s","result":"too late"}\n' "$session_id"
      ;;
    *)
      echo "fake-claude.sh: unknown FAKE_MODE '$mode'" >&2
      exit 1
      ;;
  esac
done
