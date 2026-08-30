//! contracts-m8.md §F2 (t-pool-wire): a `TurnOutcome::Failed` whose error
//! text matches `looks_like_rate_limit` must report to the pool the turn
//! actually ran under (`HarnessPool::report_rate_limit`), observed here
//! through the pool's own public effect — a reduced effective concurrency
//! limit gating the next `acquire`. Reuses `tests/control.rs`'s technique
//! (`ClaudeCodeHarness::with_binary` + `with_env`, `Session` has no public
//! constructor outside crew-harness) and its existing `fake-role-claude.sh`
//! fixture's `turn_error` mode unmodified, wrapping it in a thin delegating
//! `Harness` test double (mirrors `control.rs`'s `FailingHarness`) that
//! rewrites the returned error text to carry (or not carry) a rate-limit
//! signal, so no new fixture file is needed.
//!
//! Real time (not `tokio::time::pause`) is used deliberately, mirroring
//! `control.rs`'s own `with_pool_serializes_turns_under_a_limit_1_bucket_
//! without_deadlock` test: a real fake-CLI subprocess is spawned per turn,
//! and this codebase has no existing precedent mixing that with a paused
//! virtual clock.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use crew_agent::{RoleBehavior, RoleHarnessBehavior};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::{
    AgentCfg, HandoffSnapshot, Harness, HarnessError, HarnessEvent, HarnessPool, Session, TurnOutcome,
    UserTurn,
};
use crew_proto::{DodCheck, Envelope, MessageKind, ReqId, Role, TaskSpec};
use serde_json::json;
use tokio::sync::mpsc;

fn fake_cli_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-role-claude.sh")
}

fn agent_cfg() -> AgentCfg {
    AgentCfg {
        cwd: std::env::current_dir().expect("current dir"),
        model: None,
    }
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
        artifacts_expected: vec![],
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

/// A `ClaudeCodeHarness` pointed at the existing `turn_error` fixture mode
/// (`is_error:true, error:"forced failure"`), with its `send()` error text
/// optionally relabeled before it reaches `RoleHarnessBehavior` — so one
/// unmodified fixture produces both a rate-limit-shaped and a plain failure.
struct RelabeledFailureHarness {
    inner: ClaudeCodeHarness,
    relabel: Option<&'static str>,
}

#[async_trait]
impl Harness for RelabeledFailureHarness {
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
        let outcome = self.inner.send(session, turn, timeout).await?;
        Ok(match (outcome, self.relabel) {
            (TurnOutcome::Failed { error }, Some(label)) => TurnOutcome::Failed {
                error: format!("{label}: {error}"),
            },
            (outcome, _) => outcome,
        })
    }

    fn take_events(&self, session: &mut Session) -> mpsc::Receiver<HarnessEvent> {
        self.inner.take_events(session)
    }

    async fn snapshot(&self, session: &Session) -> Result<HandoffSnapshot, HarnessError> {
        self.inner.snapshot(session).await
    }

    async fn shutdown(&self, session: Session) -> Result<(), HarnessError> {
        self.inner.shutdown(session).await
    }
}

fn failing_harness(session_id: &str, relabel: Option<&'static str>) -> Arc<dyn Harness> {
    Arc::new(RelabeledFailureHarness {
        inner: ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
            .with_env("FAKE_MODE", "turn_error")
            .with_env("FAKE_SESSION_ID", session_id),
        relabel,
    })
}

// ---------------------------------------------------------------------
// A 429-labeled `Failed` must reduce the pool's effective limit: after the
// turn, a limit-2 bucket must gate a third-would-be-second acquire the same
// way `pool.rs`'s own `report_rate_limit_reduces_effective_limit_and_gates_
// second_acquire` unit test proves for a direct call — this proves the
// wiring actually reaches `report_rate_limit` from a real turn failure.
// ---------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_shaped_failure_reduces_the_pools_effective_limit() {
    let pool = HarnessPool::new(std::collections::HashMap::from([("claude-code".to_string(), 2)]));

    let mut behavior = RoleHarnessBehavior::new(
        failing_harness("11111111-1111-4111-8111-111111111111", Some("HTTP 429")),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    )
    .with_pool(pool.clone(), "claude-code".to_string());

    let task = make_task_spec();
    let replies = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].kind, MessageKind::Blocked);
    assert!(replies[0].body["reason"].as_str().unwrap().contains("HTTP 429"));

    // Clear report_rate_limit's own 500ms backoff deadline before probing
    // the (separate) effective-limit reduction.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let first = pool.acquire("claude-code").await;
    let pool2 = pool.clone();
    let second_task = tokio::spawn(async move { pool2.acquire("claude-code").await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !second_task.is_finished(),
        "one rate-limit-shaped Failed on a limit-2 harness should reduce the effective limit to 1, \
         gating a second concurrent acquire"
    );

    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(5), second_task)
        .await
        .expect("second acquire must complete once the first permit drops")
        .expect("task did not panic");
    drop(second);
}

// ---------------------------------------------------------------------
// A `Failed` whose text carries no recognized signal must leave the pool's
// limit untouched — two concurrent acquires on a limit-2 bucket must both
// still complete immediately after the turn.
// ---------------------------------------------------------------------

#[tokio::test]
async fn unsignaled_failure_leaves_the_pools_limit_unchanged() {
    let pool = HarnessPool::new(std::collections::HashMap::from([("claude-code".to_string(), 2)]));

    let mut behavior = RoleHarnessBehavior::new(
        failing_harness("22222222-2222-4222-8222-222222222222", None),
        agent_cfg(),
        Role::Developer,
        "You are the Developer.".to_string(),
    )
    .with_pool(pool.clone(), "claude-code".to_string());

    let task = make_task_spec();
    let replies = behavior.on_envelope(task_assign(&task)).await;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].kind, MessageKind::Blocked);
    assert!(replies[0].body["reason"].as_str().unwrap().contains("forced failure"));

    let pool2 = pool.clone();
    let first_task = tokio::spawn(async move { pool.acquire("claude-code").await });
    let second_task = tokio::spawn(async move { pool2.acquire("claude-code").await });

    let first = tokio::time::timeout(Duration::from_millis(500), first_task)
        .await
        .expect(
            "an unsignaled Failed must not reduce the limit, so two concurrent acquires on a \
             limit-2 bucket must both complete promptly",
        )
        .expect("task did not panic");
    let second = tokio::time::timeout(Duration::from_millis(500), second_task)
        .await
        .expect(
            "an unsignaled Failed must not reduce the limit, so two concurrent acquires on a \
             limit-2 bucket must both complete promptly",
        )
        .expect("task did not panic");

    drop(first);
    drop(second);
}
