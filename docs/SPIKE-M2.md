# M2 Spike Results — WS bus + PM↔Designer change_request loop

Real CLI: `claude` 2.1.231, run 2026-08-27, macOS (this machine). Results below are
from actual test output (deterministic suite + one real-CLI `--ignored` run), not
inference from docs.

## Success criterion (DESIGN.md §11 M2)

> WS 버스, ack/타임아웃/재시도, PM↔Designer `change_request` 루프 1회 완주.
> ✅ 스펙 위반을 심은 산출물에 대해 change_request가 발생하고 재작업 후 수락될 것.

**Result: PASS.** Both a deterministic integration test and a real-`claude`-CLI E2E
test complete the same scenario: PM assigns `req_ids=[REQ-1,REQ-2,REQ-3]`, Designer's
first `task.result` omits `REQ-2`, PM issues one `change_request` naming exactly
`["REQ-2"]`, Designer reworks to full coverage, PM accepts with `rounds_used: 1`.

## Deterministic test

`cargo test -p crew-agent --test m2_roundtrip` (excludes the `--ignored` E2E):

```
running 1 test
test pm_designer_roundtrip_accepts_after_one_rework_round ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s
```

`crates/crew-agent/tests/m2_roundtrip.rs::pm_designer_roundtrip_accepts_after_one_rework_round`
spins up a real `BusServer` (`BusConfig::new` with `retry_base=50ms`) and two real
`BusConn` clients (`agent:pm`, `agent:designer`), running `ScriptedPm` (real_ids
`[REQ-1,REQ-2,REQ-3]`, `max_rounds=3`) against `ScriptedDesigner` (`planted_violations
=[REQ-2]`). `AgentRunner::run` consumes its `RoleBehavior` by value and never returns
it, so `ScriptedPm::state()` can't be read back after the run completes — the test
wraps both sides in local `RecordingPm`/`RecordingDesigner` shims (no src changes)
that mirror `state()`/inbound envelopes into an `Arc<Mutex<PmState>>` and an
`mpsc` channel respectively, so the terminal state can be asserted directly:

- `PmState::Accepted { rounds_used: 1 }` — asserted directly via `RecordingPm`.
- Exactly one `change_request` observed, `violations == ["REQ-2"]` — asserted via
  `RecordingDesigner`.
- No `DeliveryFailed`/`LoopBlocked` anywhere on `BusHandle::subscribe()`'s event
  stream, and at least one `Delivered` event actually happened.

Workspace suite (`cargo test --workspace`) is green: 35+ tests across
`crew-proto`/`crew-bus`/`crew-agent`/`crew-harness`, 0 failed.

## Real E2E (`--ignored`)

`cargo test -p crew-agent --test m2_roundtrip -- --ignored --nocapture` ran
`m2_roundtrip_real_claude` once, pairing the same `ScriptedPm` against a real
`ClaudeCodeHarness` + `DesignerHarnessBehavior` (an actual spawned `claude -p`
process, not a fake-CLI fixture). The violation is induced via the prompt rather
than a scripted field, since a real model has no `planted_violations` to set — this
is legitimate for M2, whose job is to verify the loop's *wire dynamics*
(spawn/stream/drain/parse/rework), not the model's unprompted judgment.

**Measured**: 2 real `claude` turns, **elapsed = 9.836s**, `pm_result = Ok(())`,
terminal `PmState::Accepted { rounds_used: 1 }`. Re-run after the r1/F1 fix (isolated
cwd moved from the OS temp dir to `~/.linkly-crew/e2e-cli-cwd/`, see Finding 2):
same outcome, **elapsed = 8.371s**, `pm_result = Ok(())`,
`PmState::Accepted { rounds_used: 1 }` — confirms the cwd change didn't affect
behavior.

Observed envelope flow (from the run's `--nocapture` log):

```
designer received TaskAssign  req_ids=[REQ-1,REQ-2,REQ-3]
designer replying  TaskAck
designer replying  TaskResult  covered_req_ids=[REQ-1,REQ-3]        <- REQ-2 omitted
pm received        TaskAck    (ignored, not TaskResult)
pm received         TaskResult -> AwaitingRework { round: 1 }
designer received  ChangeRequest  violations=[REQ-2]
designer replying  TaskAck
designer replying  TaskResult  covered_req_ids=[REQ-1,REQ-2,REQ-3]  <- reworked
pm received         TaskAck    (ignored)
pm received         TaskResult -> Accepted { rounds_used: 1 }
designer received  TaskAck (final acceptance, ignored by Designer)
```

Full raw log: `.dev-loop/e2e_final.log` (workspace-local, not committed).

## Findings

### 1. Real agentic CLI latency requires an explicit no-tool-use prompt

A first attempt (no such instruction in the prompt) hit `crew-harness`'s fixed
`DEFAULT_TURN_TIMEOUT` (120s, `crates/crew-harness/src/lib.rs`) on the very first
turn: `claude -p`, given a task-assignment-shaped prompt inside a real repo
directory, is an agentic session with full tool access by default, and nothing in
the prompt told it to skip exploring. `DesignerHarnessBehavior::build_prompt`
doesn't disable tool use, and its hard-coded per-turn timeout isn't parameterized
(`crates/crew-agent/src/harness_behavior.rs` — out of scope to change for this
task). Fix: add an explicit "don't use tools, don't read/write files, don't run
commands, answer immediately" instruction to the `system_hint`. After this the
turn is observed to complete comfortably (both turns of the final run: **9.836s
total**, not 120s+).

### 2. This session's own dev-loop Stop hook polluted the spawned CLI's output

A second attempt (with the prompt fix above) failed differently: the Designer's
turn returned `Blocked { reason: "designer turn produced no parseable JSON reply" }`.
A raw single-turn probe (bypassing `DesignerHarnessBehavior`'s JSON extraction to
see the unparsed text) showed the real CLI's *first* answer was valid, complete
JSON — but it then appeared **7 times, concatenated with no separator**, breaking
`serde_json::from_str` (which requires the whole string to be one JSON value).

Root cause, confirmed by reading `dev-loop`'s bundled `hooks/loop-gate.sh`: this
machine has `dev-loop` installed as a global Claude Code plugin. Its Stop hook
walks up from *any* claude session's cwd looking for `.dev-loop/gates/*.md`
(loop-implement's own gates ledgers) and, if any gate is unmet, injects a synthetic
"gates ledger has unmet items" user-turn back into that session — which is *always*
true for a task's own ledger while the task is still in progress. Because the
spawned Designer session's cwd was inside this same git worktree (a descendant of
`.dev-loop/gates/`), every one of its turns triggered this feedback, and the model
kept re-answering the nudge (bounded at `MAX_GATE_BLOCKS=6` retries in the hook)
before the turn was finally allowed to terminate — corrupting the accumulated text
into repeated duplicate JSON blobs and, in an earlier run, pushing total turn time
past the 120s per-turn timeout entirely.

This is **not a crew-harness/crew-agent/crew-bus defect** — it's an artifact of
running a real-CLI E2E test from inside an active dev-loop-orchestrated session on
a machine with dev-loop installed globally. Fix (test-file only, `m2_roundtrip.rs`):
give the spawned session's `AgentCfg.cwd` an isolated path outside this git
worktree (`isolated_cli_cwd()`, `~/.linkly-crew/e2e-cli-cwd/`) so the hook's
directory walk-up finds no `.dev-loop/gates` and no-ops. This deliberately avoids
the OS temp dir (`/tmp`/`$TMPDIR`), which this machine's global policy and the
task brief's `<constraints>` both forbid (review r1/F1) — a `$HOME`-relative
hidden directory is equally outside the git worktree without that tradeoff. Any
future real-CLI E2E test authored inside a dev-loop-managed repo should do the
same.

### 3. (Non-blocking, carried from review) `crew-harness`'s events channel needs concurrent draining

Not re-discovered here, but confirmed still relevant: `DesignerHarnessBehavior`
already implements this correctly (`drain_until_terminal` is `tokio::spawn`ed
*before* `harness.send()`, not after) — see HANDOFF.md §5 pitfall 7.

## Summary

| Hypothesis | Verified? |
|---|---|
| WS bus + 2 real `BusConn` clients complete a `change_request` round-trip | Yes — deterministic test, 0.32s |
| `PmState::Accepted{rounds_used:1}` reachable after exactly 1 planted violation | Yes — asserted directly (`RecordingPm` shim) |
| No `DeliveryFailed`/`LoopBlocked` bus events during a 1-violation scenario | Yes — `CorrGuard` only guards `ChangeRequest`/`TaskResult`/`ReviewRequest` (not `TaskAck`/`TaskAssign`), so round budget reaches only 2 of `max_rounds=3` |
| Same scenario holds with a real `claude` CLI Designer (not a script) | Yes — 9.836s, 2 real turns, `rounds_used:1` |
| Real-CLI turn latency needs a no-tool-use prompt hint | Yes — confirmed by reproducing the 120s-timeout failure without it |
| Nested `claude -p` sessions inherit this machine's global dev-loop Stop hook | Yes — confirmed by reading `loop-gate.sh` and reproducing/fixing via cwd isolation |
