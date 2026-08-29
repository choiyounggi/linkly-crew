//! `RoleHarnessBehavior::on_control` (contracts-m6.md §D1, plan t-ctrl): mid-
//! sprint harness swap — snapshot -> shutdown -> harness replace -> lazy
//! respawn on the next turn. Reuses `fake-role-claude.sh` (see
//! `tests/role_harness.rs`), distinguishing the "old" and "new" harness by
//! `FAKE_SESSION_ID` and by the JSON the fake CLI replies with, so a swap's
//! effect is observable entirely through the public API (no private-field
//! access needed).
//!
//! `ClaudeCodeHarness::snapshot`/`shutdown` never fail on their own (see
//! crew-harness's own `handoff_snapshot_tests` comment), and `Session` has
//! no public constructor outside crew-harness, so the snapshot/shutdown
//! error-path tests wrap a real `ClaudeCodeHarness` in a local `Harness`
//! test double that delegates `spawn`/`send`/`take_events` to it (a real
//! session, real cleanup) but injects a forced `Err` from `snapshot` and/or
//! `shutdown`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use crew_agent::{AgentControl, RoleBehavior, RoleHarnessBehavior};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::{
    AgentCfg, HandoffSnapshot, Harness, HarnessError, HarnessEvent, HarnessPool, Session, TurnOutcome,
    UserTurn,
};
use crew_proto::{ArtifactContract, DodCheck, Envelope, MessageKind, ReqId, Role, TaskSpec};
use serde_json::json;
use tokio::sync::mpsc;

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-role-claude.sh")
}

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
    }
}

fn assistant_line(text: &str) -> String {
    json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }
    })
    .to_string()
}

/// A worktree-local capture path under `~/.linkly-crew/` (never `/tmp`),
/// truncated before each test run — mirrors `tests/injected_context.rs`.
fn capture_file(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME must be set");
    let dir = PathBuf::from(home).join(".linkly-crew").join("t-ctrl-tests");
    std::fs::create_dir_all(&dir).expect("create capture dir");
    let path = dir.join(format!("{name}.ndjson"));
    let _ = std::fs::remove_file(&path);
    path
}

fn captured_prompts(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("captured line must be JSON");
            value["message"]["content"][0]["text"]
                .as_str()
                .expect("captured turn must carry message.content[0].text")
                .to_string()
        })
        .collect()
}

fn make_task_spec() -> TaskSpec {
    TaskSpec {
        id: "t1".to_string(),
        role: Role::Developer,
        title: "Build the landing page".to_string(),
        brief: "Build a simple landing page".to_string(),
        dod: vec![DodCheck::ReqCover {
            ids: vec![ReqId::new("REQ-1").unwrap()],
        }],
        deps: vec![],
        artifacts_expected: vec![ArtifactContract {
            name: "index.html".to_string(),
            kind: "code".to_string(),
            req_ids: vec![ReqId::new("REQ-1").unwrap()],
        }],
    }
}

fn task_assign(task: &TaskSpec) -> Envelope {
    Envelope::new(
        "sp-3".to_string(),
        "th-1".to_string(),
        "agent:lead".to_string(),
        vec!["agent:developer".to_string()],
        MessageKind::TaskAssign,
        None,
        "req_01".to_string(),
        json!({ "task": task }),
        vec![],
        true,
        900_000,
    )
}

/// One `ClaudeCodeHarness` + fake-role-claude.sh combo, distinguished by
/// `session_id` (echoed on the fake CLI's init line) and by the JSON body it
/// replies with, so "which harness actually ran this turn" is observable
/// through `TaskResult.body` and `HandoffSnapshot.session_id` alone.
fn harness_with(session_id: &str, covered_req_id: &str) -> Arc<dyn Harness> {
    let text = format!(r#"{{"covered_req_ids": ["{covered_req_id}"], "artifacts": []}}"#);
    Arc::new(
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", "json")
            .with_env("FAKE_SESSION_ID", session_id)
            .with_env("FAKE_ASSISTANT_1", assistant_line(&text)),
    )
}

fn harness_with_capture(session_id: &str, covered_req_id: &str, capture: &PathBuf) -> Arc<dyn Harness> {
    let text = format!(r#"{{"covered_req_ids": ["{covered_req_id}"], "artifacts": []}}"#);
    Arc::new(
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", "json")
            .with_env("FAKE_SESSION_ID", session_id)
            .with_env("FAKE_ASSISTANT_1", assistant_line(&text))
            .with_env("FAKE_CAPTURE_FILE", capture.to_str().unwrap()),
    )
}

// ---------------------------------------------------------------------
// Case (1) normal swap: live session -> snapshot/shutdown on the old
// harness, harness replaced, injected_context lands on the very next turn,
// which is served by the *new* harness (D9 case ①).
// ---------------------------------------------------------------------

#[tokio::test]
async fn swap_with_live_session_snapshots_old_harness_then_next_turn_uses_new_harness() {
    let old_session_id = "11111111-1111-4111-8111-111111111111";
    let new_capture = capture_file("swap_normal_new_harness_prompts");

    let mut behavior = RoleHarnessBehavior::new(
        harness_with(old_session_id, "OLD"),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );

    let task = make_task_spec();

    // Establish a live session under the old harness.
    let first = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(first[1].kind, MessageKind::TaskResult);
    assert_eq!(first[1].body["covered_req_ids"], json!(["OLD"]));

    // Swap to a distinguishable new harness.
    let new_harness = harness_with_capture("22222222-2222-4222-8222-222222222222", "NEW", &new_capture);
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let replies = behavior
        .on_control(AgentControl::Swap {
            harness: new_harness,
            harness_id: "harness-new".to_string(),
            injected_context: "SWAP_MARKER".to_string(),
            ack: ack_tx,
        })
        .await;
    assert!(replies.is_empty(), "on_control must reply with no envelopes");

    let snapshot = ack_rx
        .await
        .expect("ack sender must not be dropped without sending")
        .expect("snapshot/shutdown must both succeed against the fake CLI");
    assert_eq!(
        snapshot.session_id, old_session_id,
        "ack must carry the OLD harness's snapshot, taken before the harness field was replaced"
    );

    // The next turn must lazily respawn under the NEW harness.
    let second = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(second[1].kind, MessageKind::TaskResult);
    assert_eq!(
        second[1].body["covered_req_ids"],
        json!(["NEW"]),
        "the turn after a swap must be served by the new harness, not the old one"
    );

    let prompts = captured_prompts(&new_capture);
    assert_eq!(prompts.len(), 1);
    assert!(
        prompts[0].starts_with("SWAP_MARKER\n\n"),
        "injected_context from the swap must prefix the very next turn's prompt: {:?}",
        prompts[0]
    );
}

// ---------------------------------------------------------------------
// Case (2) error paths: snapshot/shutdown errors still drop the session and
// propagate Err via ack (D9 case ②). `ClaudeCodeHarness` itself never fails
// these two calls, so a thin delegating wrapper injects the forced error
// while still performing the real spawn/send/cleanup via the inner harness.
// ---------------------------------------------------------------------

struct FailingHarness {
    inner: ClaudeCodeHarness,
    fail_snapshot: bool,
    fail_shutdown: bool,
}

#[async_trait]
impl Harness for FailingHarness {
    fn id(&self) -> crew_harness::HarnessId {
        self.inner.id()
    }

    async fn spawn(&self, cfg: &AgentCfg) -> Result<Session, HarnessError> {
        self.inner.spawn(cfg).await
    }

    async fn send(
        &self,
        session: &mut Session,
        turn: UserTurn,
        timeout: Duration,
    ) -> Result<TurnOutcome, HarnessError> {
        self.inner.send(session, turn, timeout).await
    }

    fn take_events(&self, session: &mut Session) -> mpsc::Receiver<HarnessEvent> {
        self.inner.take_events(session)
    }

    async fn snapshot(&self, session: &Session) -> Result<HandoffSnapshot, HarnessError> {
        if self.fail_snapshot {
            return Err(HarnessError::Unavailable("forced snapshot failure".to_string()));
        }
        self.inner.snapshot(session).await
    }

    async fn shutdown(&self, session: Session) -> Result<(), HarnessError> {
        // Always actually clean up the real child process, regardless of
        // whether this call reports success — a forced-failure test double
        // must not leak a real subprocess.
        let real_result = self.inner.shutdown(session).await;
        if self.fail_shutdown {
            return Err(HarnessError::Unavailable("forced shutdown failure".to_string()));
        }
        real_result
    }
}

fn failing_harness(session_id: &str, covered_req_id: &str, fail_snapshot: bool, fail_shutdown: bool) -> Arc<dyn Harness> {
    let text = format!(r#"{{"covered_req_ids": ["{covered_req_id}"], "artifacts": []}}"#);
    let inner = ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
        .with_env("FAKE_MODE", "json")
        .with_env("FAKE_SESSION_ID", session_id)
        .with_env("FAKE_ASSISTANT_1", assistant_line(&text));
    Arc::new(FailingHarness {
        inner,
        fail_snapshot,
        fail_shutdown,
    })
}

#[tokio::test]
async fn swap_shutdown_failure_propagates_err_ack_and_still_drops_the_session() {
    let mut behavior = RoleHarnessBehavior::new(
        failing_harness("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "OLD", false, true),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );
    let task = make_task_spec();

    let first = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(first[1].kind, MessageKind::TaskResult);

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    behavior
        .on_control(AgentControl::Swap {
            harness: harness_with("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "NEW"),
            harness_id: "harness-new".to_string(),
            injected_context: String::new(),
            ack: ack_tx,
        })
        .await;

    let ack = ack_rx.await.expect("ack sender must not be dropped without sending");
    assert!(
        ack.is_err(),
        "a shutdown failure must propagate as an Err ack even though snapshot succeeded"
    );

    // The session must still have been dropped (no zombie) and the harness
    // replaced — the next turn must respawn cleanly under the new harness.
    let second = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(second[1].kind, MessageKind::TaskResult);
    assert_eq!(second[1].body["covered_req_ids"], json!(["NEW"]));
}

#[tokio::test]
async fn swap_snapshot_failure_still_shuts_down_and_propagates_err_ack() {
    let mut behavior = RoleHarnessBehavior::new(
        failing_harness("cccccccc-cccc-4ccc-8ccc-cccccccccccc", "OLD", true, false),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );
    let task = make_task_spec();

    let first = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(first[1].kind, MessageKind::TaskResult);

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    behavior
        .on_control(AgentControl::Swap {
            harness: harness_with("dddddddd-dddd-4ddd-8ddd-dddddddddddd", "NEW"),
            harness_id: "harness-new".to_string(),
            injected_context: String::new(),
            ack: ack_tx,
        })
        .await;

    let ack = ack_rx.await.expect("ack sender must not be dropped without sending");
    assert!(ack.is_err(), "a snapshot failure must propagate as an Err ack");

    // Shutdown must still have run (session dropped) despite the snapshot
    // error — the next turn respawns under the new harness, proving no
    // zombie session/process was left behind.
    let second = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(second[1].kind, MessageKind::TaskResult);
    assert_eq!(second[1].body["covered_req_ids"], json!(["NEW"]));
}

// ---------------------------------------------------------------------
// Case (3) boundary: swap with no live session yet (D9 case ③).
// ---------------------------------------------------------------------

#[tokio::test]
async fn swap_with_no_session_yet_acks_ok_without_touching_any_harness() {
    let mut behavior = RoleHarnessBehavior::new(
        harness_with("00000000-0000-4000-8000-000000000000", "UNUSED"),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    );
    // Deliberately no prior on_envelope call — no session has ever spawned.

    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    let replies = behavior
        .on_control(AgentControl::Swap {
            harness: harness_with("11111111-2222-4333-8444-555555555555", "NEW"),
            harness_id: "harness-new".to_string(),
            injected_context: "ctx".to_string(),
            ack: ack_tx,
        })
        .await;
    assert!(replies.is_empty());

    let snapshot = ack_rx
        .await
        .expect("ack sender must not be dropped without sending")
        .expect("no-session swap must ack Ok");
    assert_eq!(snapshot.harness, "harness-new");
    assert_eq!(snapshot.session_id, "");
    assert_eq!(snapshot.notes, "no session yet");

    // The new harness is still installed and used on the very next turn.
    let task = make_task_spec();
    let replies = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(replies[1].kind, MessageKind::TaskResult);
    assert_eq!(replies[1].body["covered_req_ids"], json!(["NEW"]));
}

// ---------------------------------------------------------------------
// Case (4) with_pool (contracts-m6.md §D2d): the actual CLI-interaction
// span of each turn is gated on a `HarnessPool` permit, held only for that
// span — not the whole behavior's lifetime (the M5 deadlock this fixes) —
// so a second behavior sharing the same limit-1 bucket genuinely blocks
// while the first's turn is in flight, and unblocks once it ends.
// ---------------------------------------------------------------------

fn slow_harness(session_id: &str, sleep_seconds: &str) -> Arc<dyn Harness> {
    let text = r#"{"covered_req_ids": ["SLOW"], "artifacts": []}"#;
    Arc::new(
        ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", "slow")
            .with_env("FAKE_SESSION_ID", session_id)
            .with_env("FAKE_SLEEP_SECONDS", sleep_seconds)
            .with_env("FAKE_ASSISTANT_1", assistant_line(text)),
    )
}

#[tokio::test]
async fn with_pool_serializes_turns_under_a_limit_1_bucket_without_deadlock() {
    let pool = HarnessPool::new(std::collections::HashMap::from([("claude-code".to_string(), 1)]));

    let mut behavior_slow = RoleHarnessBehavior::new(
        slow_harness("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee", "0.3"),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    )
    .with_pool(pool.clone(), "claude-code".to_string());

    let mut behavior_fast = RoleHarnessBehavior::new(
        harness_with("ffffffff-ffff-4fff-8fff-ffffffffffff", "FAST"),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    )
    .with_pool(pool.clone(), "claude-code".to_string());

    let task = make_task_spec();
    let assign_slow = task_assign(&task);
    let assign_fast = task_assign(&task);

    let slow_task = tokio::spawn(async move { behavior_slow.on_envelope(assign_slow).await });
    // Give the slow turn a chance to actually acquire the pool's only
    // permit (its ensure_session spawn) before the fast turn is spawned.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let fast_task = tokio::spawn(async move { behavior_fast.on_envelope(assign_fast).await });
    // The fast turn must still be blocked on `pool.acquire` while the slow
    // turn holds the only permit — under the old M5 bug (permit held for
    // the whole runner lifetime), this would never unblock at all.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !fast_task.is_finished(),
        "fast turn must be blocked on the pool while the slow turn holds the only permit"
    );

    let slow_replies = tokio::time::timeout(Duration::from_secs(5), slow_task)
        .await
        .expect("slow turn must not deadlock")
        .expect("slow turn task must not panic");
    assert_eq!(slow_replies[1].kind, MessageKind::TaskResult);
    assert_eq!(slow_replies[1].body["covered_req_ids"], json!(["SLOW"]));

    // Once the slow turn's permit is released (turn ended), the fast turn
    // must complete promptly — proving the permit is returned between
    // turns, not held indefinitely.
    let fast_replies = tokio::time::timeout(Duration::from_secs(5), fast_task)
        .await
        .expect("fast turn must complete once the slow turn releases its permit")
        .expect("fast turn task must not panic");
    assert_eq!(fast_replies[1].kind, MessageKind::TaskResult);
    assert_eq!(fast_replies[1].body["covered_req_ids"], json!(["FAST"]));
}
