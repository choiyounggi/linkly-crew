//! M2 deterministic round-trip: PM<->Designer `change_request` loop over a
//! real `BusServer` (DESIGN.md §11 M2 success criterion / t-m2loop DoD).
//!
//! `AgentRunner::run` consumes its `RoleBehavior` by value and never returns
//! it (crew-agent/src/runner.rs), so `ScriptedPm::state()` can't be read
//! back after the run completes. `RecordingPm` below is a test-local
//! wrapper (no src changes) that mirrors `state()` into a shared
//! `Arc<Mutex<PmState>>` after every transition, mirroring the same
//! technique `RecordingDesigner` uses to observe the Designer's inbound
//! envelopes — so the terminal `PmState::Accepted{rounds_used:1}` can be
//! asserted directly, per plan L2.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crew_agent::{
    AgentRunner, BusConn, DesignerHarnessBehavior, PmState, RoleBehavior, ScriptedDesigner,
    ScriptedPm,
};
use crew_bus::{BusConfig, BusEvent, BusServer};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::AgentCfg;
use crew_proto::{Envelope, MessageKind};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

struct RecordingPm {
    inner: ScriptedPm,
    state_slot: Arc<Mutex<PmState>>,
}

#[async_trait]
impl RoleBehavior for RecordingPm {
    async fn on_start(&mut self) -> Vec<Envelope> {
        let envs = self.inner.on_start().await;
        *self.state_slot.lock().unwrap() = self.inner.state().clone();
        envs
    }

    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        println!("[m2] pm received {:?}", env.kind);
        let envs = self.inner.on_envelope(env).await;
        *self.state_slot.lock().unwrap() = self.inner.state().clone();
        println!("[m2] pm state now {:?}", self.inner.state());
        envs
    }

    fn is_done(&self) -> bool {
        self.inner.is_done()
    }
}

/// Mirrors every envelope the Designer receives out to `seen` before
/// delegating, so the change_request the PM sends can be inspected
/// independently of the Designer's own reply.
struct RecordingDesigner {
    inner: ScriptedDesigner,
    seen: mpsc::UnboundedSender<Envelope>,
}

#[async_trait]
impl RoleBehavior for RecordingDesigner {
    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        let _ = self.seen.send(env.clone());
        self.inner.on_envelope(env).await
    }

    fn is_done(&self) -> bool {
        self.inner.is_done()
    }
}

/// Logs every envelope in/out of the wrapped Designer behavior — this is
/// the "관찰된 봉투 흐름" (observed envelope flow) plan L4 asks docs/SPIKE-M2.md
/// to record for the real-CLI run.
struct LoggingBehavior<B> {
    inner: B,
}

#[async_trait]
impl<B: RoleBehavior + Send> RoleBehavior for LoggingBehavior<B> {
    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        println!("[m2] designer received {:?} body={}", env.kind, env.body);
        let replies = self.inner.on_envelope(env).await;
        for r in &replies {
            println!("[m2] designer replying {:?} body={}", r.kind, r.body);
        }
        replies
    }

    fn is_done(&self) -> bool {
        self.inner.is_done()
    }
}

/// A cwd for the spawned real-CLI session that is NOT inside this git
/// worktree. `loop-gate.sh` (dev-loop's Stop hook, globally installed on
/// this machine) walks up from the *spawned session's own cwd* looking for
/// `.dev-loop/gates/*.md`; since that ledger lives at this worktree's root
/// (crates/crew-agent is a descendant of it), pointing the child at a cwd
/// inside the worktree makes every one of its turns get a synthetic "gates
/// ledger unmet" feedback message appended, which corrupted an earlier
/// attempt's output into repeated duplicate JSON blobs (confirmed by reading
/// loop-gate.sh and reproducing with a raw single-turn probe — see
/// docs/SPIKE-M2.md). A dir under `$HOME` has no `.dev-loop` or
/// `.orchestration` in its ancestry, so both of the hook's gates are a
/// no-op for it — and per review r1/F1, this deliberately avoids the OS temp
/// dir (`/tmp`/`$TMPDIR`), which this machine's global policy and the
/// brief's <constraints> both forbid.
fn isolated_cli_cwd() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .expect("HOME must be set to derive a non-/tmp isolated cwd for the spawned CLI session");
    let dir = std::path::PathBuf::from(home)
        .join(".linkly-crew")
        .join("e2e-cli-cwd");
    std::fs::create_dir_all(&dir).expect("create isolated cwd");
    dir
}

async fn start_bus(retry_base: Duration) -> (crew_bus::BusHandle, String) {
    let mut cfg = BusConfig::new("m2-token");
    cfg.retry_base = retry_base;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = BusServer::new(cfg).serve(listener).await;
    let url = format!("ws://{}/ws", handle.local_addr());
    (handle, url)
}

#[tokio::test]
async fn pm_designer_roundtrip_accepts_after_one_rework_round() {
    let (bus, url) = start_bus(Duration::from_millis(50)).await;
    let mut events = bus.subscribe();

    let pm_conn = BusConn::connect(&url, "m2-token", "agent:pm")
        .await
        .expect("pm connect");
    let designer_conn = BusConn::connect(&url, "m2-token", "agent:designer")
        .await
        .expect("designer connect");

    let pm_state = Arc::new(Mutex::new(PmState::Assigned));
    let recording_pm = RecordingPm {
        inner: ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec![
                "REQ-1".to_string(),
                "REQ-2".to_string(),
                "REQ-3".to_string(),
            ],
            3,
        ),
        state_slot: pm_state.clone(),
    };

    let (seen_tx, mut seen_rx) = mpsc::unbounded_channel();
    let recording_designer = RecordingDesigner {
        inner: ScriptedDesigner::new("agent:designer", vec!["REQ-2".to_string()]),
        seen: seen_tx,
    };

    // Designer's `is_done()` is always false (plan A7/brief dependencies) —
    // its runner never returns on its own, so it's spawned and cleaned up
    // (aborted) once the PM side has finished the conversation.
    let designer_task = tokio::spawn(AgentRunner::run(designer_conn, recording_designer));

    let pm_result = tokio::time::timeout(
        Duration::from_secs(30),
        AgentRunner::run(pm_conn, recording_pm),
    )
    .await
    .expect("PM runner must finish within the 30s deterministic budget");

    assert!(
        pm_result.is_ok(),
        "PM runner should end cleanly: {pm_result:?}"
    );
    assert_eq!(
        *pm_state.lock().unwrap(),
        PmState::Accepted { rounds_used: 1 },
        "one planted violation must resolve after exactly one rework round"
    );

    // The Designer must have received exactly the change_request the PM's
    // violation detection produces, naming only the planted REQ-2.
    let mut change_requests_seen = 0;
    while let Ok(env) = seen_rx.try_recv() {
        if env.kind == MessageKind::ChangeRequest {
            assert_eq!(env.body["violations"], json!(["REQ-2"]));
            change_requests_seen += 1;
        }
    }
    assert_eq!(
        change_requests_seen, 1,
        "expected exactly one change_request round-trip"
    );

    // No DeliveryFailed/LoopBlocked anywhere in the run, and at least one
    // Delivered event actually happened.
    let mut saw_delivered = false;
    loop {
        match tokio::time::timeout(Duration::from_millis(300), events.recv()).await {
            Ok(Ok(BusEvent::Delivered { .. })) => saw_delivered = true,
            Ok(Ok(BusEvent::DeliveryFailed { id })) => panic!("unexpected DeliveryFailed: {id}"),
            Ok(Ok(BusEvent::LoopBlocked { corr })) => panic!("unexpected LoopBlocked: {corr}"),
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert!(saw_delivered, "expected at least one Delivered bus event");

    designer_task.abort();
    bus.shutdown().await;
}

/// Real `claude` CLI E2E (plan L3) — #[ignore] by default; run explicitly:
///   cargo test -p crew-agent --test m2_roundtrip -- --ignored --nocapture
///
/// Same M2 scenario as the deterministic test above, but the Designer side
/// is the real `ClaudeCodeHarness` + `DesignerHarnessBehavior` driving an
/// actual `claude` process, exercising the full spawn/stream/drain/parse/
/// rework pipeline end to end. The `system_hint` deliberately induces the
/// spec violation via the prompt (M2 verifies loop dynamics, not the LLM's
/// unprompted judgment) rather than a planted-violation script, since a real
/// model has no `planted_violations` field to script.
#[tokio::test]
#[ignore]
async fn m2_roundtrip_real_claude() {
    let (bus, url) = start_bus(Duration::from_millis(250)).await;

    let pm_conn = BusConn::connect(&url, "m2-token", "agent:pm")
        .await
        .expect("pm connect");
    let designer_conn = BusConn::connect(&url, "m2-token", "agent:designer")
        .await
        .expect("designer connect");

    let pm_state = Arc::new(Mutex::new(PmState::Assigned));
    let recording_pm = RecordingPm {
        inner: ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec![
                "REQ-1".to_string(),
                "REQ-2".to_string(),
                "REQ-3".to_string(),
            ],
            3,
        ),
        state_slot: pm_state.clone(),
    };

    let harness: Arc<dyn crew_harness::Harness> = Arc::new(ClaudeCodeHarness::new());
    let cfg = AgentCfg {
        cwd: isolated_cli_cwd(),
    };
    let system_hint = "도구를 절대 사용하지 말고, 파일을 읽거나 쓰거나 명령을 실행하지 말 것. \
        아래 프롬프트 내용만 보고 즉시 답하라. \
        첫 task assignment에는 req_ids 중 REQ-2를 제외한 coverage로 응답하고, \
        change_request를 받으면 전체 coverage로 응답하라. 반드시 JSON만."
        .to_string();
    let designer_behavior = LoggingBehavior {
        inner: DesignerHarnessBehavior::new(harness, cfg, system_hint),
    };

    let designer_task = tokio::spawn(AgentRunner::run(designer_conn, designer_behavior));

    let started = Instant::now();
    let pm_result = tokio::time::timeout(
        Duration::from_secs(300),
        AgentRunner::run(pm_conn, recording_pm),
    )
    .await
    .expect("PM runner must finish within the 300s real-CLI budget");
    let elapsed = started.elapsed();
    println!("m2_roundtrip_real_claude: elapsed={elapsed:?} pm_result={pm_result:?}");

    assert!(
        pm_result.is_ok(),
        "PM runner should end cleanly: {pm_result:?}"
    );
    assert_eq!(
        *pm_state.lock().unwrap(),
        PmState::Accepted { rounds_used: 1 },
        "the induced REQ-2 violation must resolve after exactly one rework round"
    );

    designer_task.abort();
    bus.shutdown().await;
}
