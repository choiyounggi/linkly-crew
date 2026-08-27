# M1 Spike Results — claude-code adapter

Real CLI: `claude` 2.1.231, run 2026-08-27, macOS (this machine). All results below
are from actual command output, not inference from docs.

## Success criterion (DESIGN.md §11 M1)

> 한 프로세스에 3연속 턴을 보내고 각 응답을 구조화 이벤트로 수신, 강제 실패 케이스에서
> `Failed` 로 정확히 분류.

**Result: PASS.** `cargo test -p crew-harness --test real_claude -- --ignored --nocapture`
ran `three_consecutive_turns_then_resume` once: one `claude -p` process handled 3
consecutive turns and a resumed 4th turn, each returning `TurnOutcome::Success` and
a full structured event sequence (`Started` → `Usage`/`Thinking`/`Text` → `Finished`).
Total wall time 15.66s for 4 turns. Forced-failure classification (`Failed` without
reading `subtype`) is validated by the fake-CLI test
`forced_error_turn_is_failed_even_with_subtype_success` plus the real captured
auth-failure sample below — see "is_error/terminal_reason bug found and fixed".

## Spawn command (D3) — as implemented, confirmed working

```
claude -p --input-format stream-json --output-format stream-json --verbose --session-id <uuid4>
```

`--verbose` was accepted with no error and did not need to be dropped. DESIGN.md
§2.1's captured command additionally had `--include-partial-messages`, which we did
not include (D3 doesn't ask for token-level streaming deltas for M1) — no issue
observed from omitting it; each assistant message arrived as one `Text` event with
the full turn's text, which is what D5's event model expects without partial
messages.

Resume command (D8), confirmed working — same `session_id` persisted across resume,
and the resumed turn correctly recalled context from the original session ("What was
the first word I asked you to reply with, verbatim?" → "one"):

```
claude -p -r <session_id> --input-format stream-json --output-format stream-json --verbose
```

## stdin turn format (D11) — confirmed accepted as specified

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}
```

One line + `\n`, flushed. No rejection observed across 4 turns (3 + 1 resumed). No
correction needed.

## is_error/terminal_reason bug found and fixed

The plan's D4 assumed `terminal_reason` uses an "ok"/"success" style allow-list for
the success case. **Real output disagrees**, and the spike caught this before it
shipped:

- Real successful turn (captured this run): `is_error:false`, **`terminal_reason:
  "completed"`** (not `"ok"`).
- Real auth-failure sample (from the `claude-cli-headless-success-signal` memory,
  captured 2026-08-25, same CLI version, still trustworthy):
  `is_error:true, subtype:"success", terminal_reason:"api_error",
  result:"Not logged in · Please run /login"` — reconfirms DESIGN.md §2.2's warning:
  `subtype` says `"success"` even on a real, total failure.

`judge_result` (`src/event.rs`) was originally written with an allow-list
(`terminal_reason != "ok" && != "success"` → Failed), which would have
**misclassified every real successful turn as Failed** the first time
`terminal_reason` was ever consulted, because it never matched anything on the
allow-list. Fixed to: `is_error` alone decides when present (real output always
includes it); `terminal_reason == "completed"` is only consulted as a fallback when
`is_error` is absent. `subtype` is never read anywhere in the judgment path — 7
unit tests now cover this, including the two real captured samples above as
regression tests (`success_via_terminal_reason_completed_when_is_error_absent`,
`real_auth_failure_sample_from_spike`).

The fake-CLI fixture (`tests/fixtures/fake-claude.sh`) was updated to emit the
real field shape (`terminal_reason:"completed"` / `terminal_reason:"api_error"`)
so the deterministic test suite stays aligned with reality.

## Event normalization (D2/D5) — real field paths observed

Real `claude -p --verbose` in this environment also emits a large volume of
`system`/`hook_started`/`hook_response` events (this session's own
SessionStart hooks re-firing inside the spawned child, since the child inherits
this environment) and a `rate_limit_event` type. Neither is in D5's event set;
both were preserved via `HarnessEvent::Raw` rather than dropped, per D2 — no
adapter changes needed, the unknown-type fallback handled them correctly as
observed.

Confirmed real event shapes used by `normalize()`:
- `{"type":"system","subtype":"init","session_id":"...",...}` → `Started`
- `{"type":"assistant","message":{"content":[{"type":"text","text":"..."}],
  "usage":{"input_tokens":N,"output_tokens":N}}}` → `Usage` + `Text`
- `{"type":"assistant","message":{"content":[{"type":"thinking",...}]}}` → `Thinking`
- `{"type":"result","is_error":bool,"terminal_reason":"...",...}` → `Finished`/`Failed`
  (via `judge_result`)

## Summary

| Hypothesis (plan D-number) | Verified? |
|---|---|
| D3 spawn command works as written | Yes, `--verbose` accepted |
| D8 resume command works, preserves context | Yes |
| D11 stdin turn JSON format accepted | Yes |
| D4 terminal_reason allow-list ("ok"/"success") | **No — fixed to `"completed"`, `is_error` primary** |
| D2 unknown event types preserved via `Raw` | Yes, exercised by real hook/rate-limit events |
| DESIGN §2.2: `subtype` lies, `is_error` doesn't | Confirmed via real auth-failure sample |
