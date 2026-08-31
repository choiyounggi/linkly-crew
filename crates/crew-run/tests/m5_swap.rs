//! Deterministic `RunHandle::swap_harness` tests (brief DoD, contracts-m5.md
//! §C5c 2026-08-29 보정판, t-swap plan D6): roster update + handoff envelope
//! ledger-write + `RosterChanged` event order, validation-before-mutation
//! error paths, and cross-sprint-boundary effectuation.

use std::path::PathBuf;
use std::time::Duration;

use crew_lead::accept::AcceptanceLoop;
use crew_proto::{handoff_pack_from_body, MessageKind};
use crew_run::{RunConfig, RunController, RunError, RunEvent, RunMode, RunOutcomeDto, TaskStateDto};

const GOAL: &str = "간단한 랜딩 페이지";
const START_TIMEOUT: Duration = Duration::from_secs(30);
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);
const EVENT_TIMEOUT: Duration = Duration::from_secs(10);

fn test_data_dir(label: &str) -> PathBuf {
    std::env::current_dir()
        .unwrap()
        .join(".crew-test")
        .join(format!("{label}-{}", uuid::Uuid::new_v4()))
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

fn config(data_dir: PathBuf, max_per_sprint: usize) -> RunConfig {
    RunConfig {
        goal: GOAL.to_string(),
        mode: RunMode::Scripted { planted_violations: vec![] },
        data_dir,
        max_rework: AcceptanceLoop::default_budget(),
        max_per_sprint,
        escalation_timeout_ms: 0,
        roster: None,
        dev_cmd_checks: Vec::new(),
    }
}

async fn start(cfg: RunConfig) -> crew_run::RunHandle {
    tokio::time::timeout(START_TIMEOUT, RunController::start(cfg))
        .await
        .expect("start must finish within the deterministic budget")
        .expect("start must succeed for a well-formed config")
}

async fn join_ok(handle: &mut crew_run::RunHandle) {
    tokio::time::timeout(JOIN_TIMEOUT, handle.join())
        .await
        .expect("join must finish within the deterministic budget")
        .expect("lead runner must complete cleanly");
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<RunEvent>) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    events
}

/// Waits (bounded) for the first event matching `pred`, buffering everything
/// observed along the way (including the match) so no event is dropped for
/// later inspection.
async fn wait_for(
    rx: &mut tokio::sync::broadcast::Receiver<RunEvent>,
    buf: &mut Vec<RunEvent>,
    pred: impl Fn(&RunEvent) -> bool,
) {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let ev = rx.recv().await.expect("broadcast receiver must not lag/close before the match");
            let matched = pred(&ev);
            buf.push(ev);
            if matched {
                return;
            }
        }
    })
    .await
    .expect("expected event did not arrive within the deterministic budget")
}

fn roster_entry<'a>(agents: &'a [crew_run::RosterAgentDto], id: &str) -> &'a crew_run::RosterAgentDto {
    agents.iter().find(|a| a.id == id).unwrap_or_else(|| panic!("roster must contain {id}"))
}

/// Normal path (DoD ①): sprint0 finishes, then a mid-run swap on
/// `agent:designer` — event order (`Message(kind=handoff)` then
/// `RosterChanged`), the run completes normally afterward (continuity), and
/// the final `RosterChanged` reflects the new harness.
#[tokio::test(flavor = "multi_thread")]
async fn swap_between_sprints_emits_handoff_then_roster_changed_and_run_completes() {
    let data_dir = test_data_dir("swap-happy");
    // 5 tasks / max_per_sprint=3 -> sprint0=[t-pm,t-design,t-publish], sprint1=[t-dev,t-qa].
    let mut handle = start(config(data_dir.clone(), 3)).await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    wait_for(&mut rx, &mut seen, |ev| matches!(ev, RunEvent::SprintFinished { index: 0, .. })).await;

    handle
        .swap_harness("agent:designer", "opencode")
        .await
        .expect("swapping to a known, adapter-backed harness id must succeed");

    join_ok(&mut handle).await;
    seen.extend(drain(&mut rx));

    let handoff_pos = seen.iter().position(|ev| {
        matches!(ev, RunEvent::Message { envelope, .. }
            if envelope.kind == MessageKind::Handoff && envelope.to == vec!["agent:designer".to_string()])
    });
    let roster_changed_positions: Vec<usize> = seen
        .iter()
        .enumerate()
        .filter(|(_, ev)| matches!(ev, RunEvent::RosterChanged { .. }))
        .map(|(i, _)| i)
        .collect();

    let handoff_pos = handoff_pos.expect("a handoff Message addressed to agent:designer must be observed");
    match &seen[handoff_pos] {
        RunEvent::Message { envelope, .. } => {
            assert_eq!(envelope.from, "agent:lead");
            let pack = handoff_pack_from_body(&envelope.body).expect("handoff envelope body must decode as a HandoffPack");
            assert_eq!(pack.role, crew_proto::Role::Designer);
            assert_eq!(pack.spec_ref, GOAL);
            assert!(
                pack.done.contains(&"t-pm".to_string()) && pack.done.contains(&"t-design".to_string()) && pack.done.contains(&"t-publish".to_string()),
                "done must list sprint 0's already-accepted tasks: {:?}",
                pack.done
            );
            assert_eq!(pack.notes, "harness swap: claude-code -> opencode");
        }
        other => panic!("expected Message, got {other:?}"),
    }
    // RunStarted's own initial RosterChanged plus this swap's RosterChanged.
    assert_eq!(roster_changed_positions.len(), 2, "exactly one RosterChanged from start() plus one from the swap");
    let swap_roster_pos = *roster_changed_positions.last().unwrap();
    assert!(
        handoff_pos < swap_roster_pos,
        "the swap's handoff Message must precede its RosterChanged: handoff@{handoff_pos}, roster_changed@{swap_roster_pos}"
    );

    assert!(
        matches!(seen.last(), Some(RunEvent::RunFinished { outcome: RunOutcomeDto::Completed, .. })),
        "the run must complete normally after a mid-run swap: {:?}",
        seen.last()
    );

    let final_agents = match &seen[swap_roster_pos] {
        RunEvent::RosterChanged { agents, .. } => agents,
        other => panic!("expected RosterChanged, got {other:?}"),
    };
    assert_eq!(roster_entry(final_agents, "agent:designer").harness, "opencode");

    let snap = handle.snapshot();
    assert!(
        snap.task_states.iter().all(|(_, state)| *state == TaskStateDto::Accepted),
        "every task across both sprints must still end accepted after the swap: {:?}",
        snap.task_states
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// True if `ev` is one of the two event kinds `swap_harness` (and nothing
/// else in the controller) ever emits — used to isolate swap-attributable
/// events from the ambient `BusLifecycle`/`Message`/`TaskStateChanged`
/// traffic a live Scripted sprint keeps producing concurrently.
fn is_swap_signature_event(ev: &RunEvent) -> bool {
    matches!(ev, RunEvent::RosterChanged { .. })
        || matches!(ev, RunEvent::Message { envelope, .. } if envelope.kind == MessageKind::Handoff)
}

/// Error path (DoD ②): an unknown `agent_id` is rejected before any
/// mutation — no `RosterChanged`/handoff `Message` is emitted. The sprint
/// keeps running underneath (ordinary `BusLifecycle`/`Message`/
/// `TaskStateChanged` traffic is expected and not asserted against here).
#[tokio::test(flavor = "multi_thread")]
async fn swap_with_unknown_agent_id_is_rejected_and_changes_nothing() {
    let data_dir = test_data_dir("swap-unknown-agent");
    let mut handle = start(config(data_dir.clone(), 0)).await;
    let mut rx = handle.subscribe();
    drain(&mut rx); // start()'s own RunStarted/SpecReady/RosterChanged/SprintStarted/Registered noise.

    let result = handle.swap_harness("agent:ghost", "opencode").await;

    assert!(matches!(result, Err(RunError::SwapRejected(_))), "unknown agent_id must return RunError::SwapRejected, got {result:?}");
    let after = drain(&mut rx);
    assert!(
        !after.iter().any(is_swap_signature_event),
        "a rejected swap (unknown agent_id) must not emit RosterChanged or a handoff Message: {after:?}"
    );

    join_ok(&mut handle).await;
    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Error path (DoD ②): an unknown harness id is rejected before any
/// mutation. `opencode` (Stub adapter) is deliberately not used here since
/// it must be *accepted*. Strengthened beyond "no event": a follow-up valid
/// swap's handoff `notes` must still show the roster's original
/// `claude-code` default as the "old" harness, proving the rejected attempt
/// never wrote `"not-a-real-harness"` into the slot.
#[tokio::test(flavor = "multi_thread")]
async fn swap_with_unknown_harness_id_is_rejected_and_changes_nothing() {
    let data_dir = test_data_dir("swap-unknown-harness");
    let mut handle = start(config(data_dir.clone(), 0)).await;
    let mut rx = handle.subscribe();
    drain(&mut rx); // start()'s own RunStarted/SpecReady/RosterChanged/SprintStarted/Registered noise.

    let result = handle.swap_harness("agent:designer", "not-a-real-harness").await;

    assert!(matches!(result, Err(RunError::SwapRejected(_))), "unknown harness id must return RunError::SwapRejected, got {result:?}");
    let after_reject = drain(&mut rx);
    assert!(
        !after_reject.iter().any(is_swap_signature_event),
        "a rejected swap (unknown harness id) must not emit RosterChanged or a handoff Message: {after_reject:?}"
    );

    handle
        .swap_harness("agent:designer", "opencode")
        .await
        .expect("a follow-up valid swap on the same slot must still succeed");
    let after_valid = drain(&mut rx);
    let notes = after_valid
        .iter()
        .find_map(|ev| match ev {
            RunEvent::Message { envelope, .. } if envelope.kind == MessageKind::Handoff => {
                handoff_pack_from_body(&envelope.body).map(|pack| pack.notes)
            }
            _ => None,
        })
        .expect("the follow-up valid swap must emit a handoff Message with a HandoffPack body");
    assert_eq!(
        notes, "harness swap: claude-code -> opencode",
        "the rejected attempt must not have mutated the slot's harness away from the claude-code default"
    );

    join_ok(&mut handle).await;
    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Boundary (DoD ③): swapping the `lead` slot succeeds, but — since
/// `crew_proto::Role` has no `Lead` variant to build a `HandoffPack` from —
/// no handoff envelope/Message is emitted, only `RosterChanged`.
#[tokio::test(flavor = "multi_thread")]
async fn swap_on_lead_slot_emits_roster_changed_only_no_handoff_message() {
    let data_dir = test_data_dir("swap-lead");
    let mut handle = start(config(data_dir.clone(), 0)).await;
    let mut rx = handle.subscribe();
    drain(&mut rx); // start()'s own RunStarted/SpecReady/RosterChanged/SprintStarted/Registered noise.

    handle
        .swap_harness("agent:lead", "opencode")
        .await
        .expect("swapping the lead slot to a known harness must succeed");

    let after_swap = drain(&mut rx);
    let roster_changed: Vec<&RunEvent> = after_swap
        .iter()
        .filter(|ev| matches!(ev, RunEvent::RosterChanged { .. }))
        .collect();
    assert_eq!(
        roster_changed.len(), 1,
        "the lead swap must emit exactly one RosterChanged: {after_swap:?}"
    );
    match roster_changed[0] {
        RunEvent::RosterChanged { agents, .. } => {
            assert_eq!(roster_entry(agents, "agent:lead").harness, "opencode");
        }
        other => panic!("expected RosterChanged, got {other:?}"),
    }
    let handoff_count = after_swap
        .iter()
        .filter(|ev| matches!(ev, RunEvent::Message { envelope, .. } if envelope.kind == MessageKind::Handoff))
        .count();
    assert_eq!(handoff_count, 0, "the lead slot has no crew_proto::Role, so no handoff Message must be emitted: {after_swap:?}");

    join_ok(&mut handle).await;
    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Boundary (DoD ④): the handoff `Message.seq` is monotonically continuous
/// with every prior message in the `messages` space.
#[tokio::test(flavor = "multi_thread")]
async fn swap_handoff_message_seq_is_monotonic_with_prior_messages() {
    let data_dir = test_data_dir("swap-seq");
    let mut handle = start(config(data_dir.clone(), 0)).await;
    let mut rx = handle.subscribe();
    let mut seen = Vec::new();

    wait_for(&mut rx, &mut seen, |ev| matches!(ev, RunEvent::SprintFinished { index: 0, .. })).await;
    let max_seq_before_swap = seen
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Message { seq, .. } => Some(*seq),
            _ => None,
        })
        .max()
        .expect("sprint 0 must have produced at least one Message before the swap");

    handle.swap_harness("agent:qa", "claude-code").await.expect("swap must succeed");

    join_ok(&mut handle).await;
    seen.extend(drain(&mut rx));

    let message_seqs: Vec<i64> = seen
        .iter()
        .filter_map(|ev| match ev {
            RunEvent::Message { seq, .. } => Some(*seq),
            _ => None,
        })
        .collect();
    assert!(
        message_seqs.windows(2).all(|w| w[0] < w[1]),
        "Message.seq must stay strictly increasing across the swap: {message_seqs:?}"
    );
    let handoff_seq = seen
        .iter()
        .find_map(|ev| match ev {
            RunEvent::Message { seq, envelope } if envelope.kind == MessageKind::Handoff => Some(*seq),
            _ => None,
        })
        .expect("a handoff Message must be observed");
    assert!(
        handoff_seq > max_seq_before_swap,
        "the handoff Message.seq ({handoff_seq}) must be greater than every pre-swap seq ({max_seq_before_swap})"
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// New (t-swap plan D9 scenario ①): a swap called *while* a sprint's
/// workers are still live — not after `SprintFinished` like the M5 test
/// above — exercises the new mid-sprint immediate-effect path
/// (contracts-m6.md §D2b step 3): `swap_harness` sends `AgentControl::Swap`
/// straight to the live worker and waits for its ack, rather than only
/// falling back to the sprint-boundary path. Single sprint
/// (`max_per_sprint=0`) keeps the worker set alive for the whole run — no
/// boundary to race, isolating this from `swap_between_sprints_...`'s
/// boundary-fallback path.
#[tokio::test(flavor = "multi_thread")]
async fn swap_mid_sprint_reaches_the_live_worker_ack_ok_and_run_completes() {
    let data_dir = test_data_dir("swap-mid-sprint");
    let mut handle = start(config(data_dir.clone(), 0)).await;
    let mut rx = handle.subscribe();
    drain(&mut rx); // start()'s own RunStarted/SpecReady/RosterChanged/SprintStarted/Registered noise.

    handle
        .swap_harness("agent:designer", "opencode")
        .await
        .expect("a mid-sprint swap against a live worker must ack Ok");

    join_ok(&mut handle).await;
    let after = drain(&mut rx);

    let handoff = after.iter().any(|ev| {
        matches!(ev, RunEvent::Message { envelope, .. }
            if envelope.kind == MessageKind::Handoff && envelope.to == vec!["agent:designer".to_string()])
    });
    assert!(handoff, "a handoff Message addressed to agent:designer must be observed: {after:?}");
    assert!(
        after.iter().any(|ev| matches!(ev, RunEvent::RosterChanged { .. })),
        "a RosterChanged must be observed: {after:?}"
    );
    assert!(
        matches!(after.last(), Some(RunEvent::RunFinished { outcome: RunOutcomeDto::Completed, .. })),
        "the run must complete normally after a mid-sprint swap (continuity): {:?}",
        after.last()
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// Boundary (t-swap plan D9 scenario ④): a swap called after the run has
/// already finished still succeeds. `swap_harness` never checked run state
/// even before this task (M5) — steps 1-2 (roster mutation, handoff
/// envelope, `RosterChanged`) never depended on it — and after every
/// sprint's workers are aborted `controls` is cleared (this task's
/// `LiveHandles.controls.clear()` fix), so step 3 takes the sender-absent
/// `Ok` fallback (§D2b step 3 / D6) exactly like M5's boundary-only path.
#[tokio::test(flavor = "multi_thread")]
async fn swap_after_run_finished_still_succeeds_via_the_sender_absent_fallback() {
    let data_dir = test_data_dir("swap-after-finish");
    let mut handle = start(config(data_dir.clone(), 0)).await;
    let mut rx = handle.subscribe();
    drain(&mut rx);

    join_ok(&mut handle).await;
    drain(&mut rx); // drain whatever ran to completion before the swap.

    let result = handle.swap_harness("agent:qa", "opencode").await;
    assert!(
        result.is_ok(),
        "a swap after the run finished must still succeed (M5 behavior preserved): {result:?}"
    );

    let after = drain(&mut rx);
    assert!(
        after.iter().any(|ev| matches!(ev, RunEvent::RosterChanged { .. })),
        "steps 1-2 must still apply after the run has finished: {after:?}"
    );

    handle.shutdown().await;
    cleanup(&data_dir);
}

/// DoD's explicit "opencode(Stub)는 허용" case: `HarnessRegistry::make`
/// returns `Some` for `opencode` (a Stub adapter, not `None`/real), so it
/// must be accepted as a swap target.
#[tokio::test(flavor = "multi_thread")]
async fn swap_to_opencode_stub_adapter_is_accepted() {
    let data_dir = test_data_dir("swap-opencode");
    let mut handle = start(config(data_dir.clone(), 0)).await;

    let result = handle.swap_harness("agent:publisher", "opencode").await;
    assert!(result.is_ok(), "opencode (Stub adapter) must be an accepted swap target: {result:?}");

    join_ok(&mut handle).await;
    handle.shutdown().await;
    cleanup(&data_dir);
}

// --- real CLI spot check (contracts-m6.md §D2b step 5 / §D6 — add only,
// coordinator runs manually, never here) -------------------------------

/// t-swap plan D10: a two-sprint `RealCli` run with `agent:designer` swapped
/// claude-code -> claude-code mid-sprint-1 — exercising the real
/// snapshot -> shutdown -> lazy-respawn path end to end (`RoleHarnessBehavior
/// ::on_control`) against the real CLI, not a fixture. Asserts the swap acks
/// Ok and the run completes with zero interventions. Same "add only" pattern
/// as crew-lead's `m5_plan_llm.rs::real_cli_specify_via_lead_harness_slot`.
/// `max_per_sprint=2` is a best-effort split for a typical landing-page
/// spec's requirement count — this test is `#[ignore]`d and never runs in
/// CI, so it doesn't need a deterministic sprint count. This is also the
/// only integration-level coverage of the `role_cli_cwd` ENOENT fix
/// (controller.rs — a worker's `Command::current_dir` needs its `cli-cwd`
/// directory to actually exist): every worker here would fail to spawn and
/// report blocked without it, which is exactly how the coordinator's run of
/// this test first surfaced the bug. `role_cli_cwd`'s own unit tests
/// (controller.rs `role_cli_cwd_tests`) cover the directory-creation logic
/// deterministically; a full deterministic `RunController::start` test
/// under `RunMode::RealCli` isn't feasible without a fake-CLI-script fixture
/// (crew-run has none — unlike crew-lead's `m5_plan_llm.rs`), since
/// `LlmLeadPlanner::specify` runs a real CLI turn before any `spawn_sprint`
/// call even happens; adding that fixture is out of scope for this fix.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn real_cli_mid_sprint_designer_swap_completes_two_sprints() {
    let data_dir = test_data_dir("swap-real-cli");
    let cfg = RunConfig {
        goal: "간단한 랜딩 페이지".to_string(),
        mode: RunMode::RealCli,
        data_dir: data_dir.clone(),
        max_rework: AcceptanceLoop::default_budget(),
        max_per_sprint: 2,
        escalation_timeout_ms: 0,
        roster: None,
        dev_cmd_checks: Vec::new(),
    };

    // Real CLI turns measured 17-193s (m5a E2E) — far past the deterministic
    // helpers' 30s budgets above, so this test uses its own generous ones.
    let real_start_timeout = Duration::from_secs(300);
    let real_join_timeout = Duration::from_secs(1800);

    let mut handle = tokio::time::timeout(real_start_timeout, RunController::start(cfg))
        .await
        .expect("real-CLI start must finish within the budget")
        .expect("real-CLI start must succeed");

    handle
        .swap_harness("agent:designer", "claude-code")
        .await
        .expect("a mid-sprint-1 real-CLI swap must ack Ok");

    tokio::time::timeout(real_join_timeout, handle.join())
        .await
        .expect("real-CLI run must finish within the budget")
        .expect("the run must complete with zero interventions after the mid-sprint swap");

    handle.shutdown().await;
    cleanup(&data_dir);
}
