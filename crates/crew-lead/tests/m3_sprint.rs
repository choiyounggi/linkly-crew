//! M3 sprint E2E: proves DESIGN.md §11's M3 success criterion — a single
//! "간단한 랜딩 페이지" request completes specify -> plan_dag -> dispatch ->
//! DoD judgment -> accept with **zero human intervention** required for the
//! happy/rework paths, and that an unrecoverable gap (an unregistered role)
//! escalates via `human.gate` without blocking the rest of the sprint
//! (t-m3e2e brief, plan D4-D6).
//!
//! Three deterministic scenarios below use the M2-verified pattern
//! (`crates/crew-agent/tests/m2_roundtrip.rs`): a real in-process
//! `BusServer` on an ephemeral port, `ScriptedCrewMember` workers for all
//! five roles, and a `RecordingLead` wrapper (mirrors that file's
//! `RecordingPm`) that mirrors every envelope `LeadBehavior` emits into a
//! shared `Vec` so `change_request`/`human.gate` counts can be asserted
//! after `AgentRunner::run` consumes the behavior by value. An
//! `EventLedger::open_in_memory()` subscribes to the bus's broadcast feed
//! before any worker connects (plan D2), so no `Delivered` event can be
//! missed.
//!
//! Run the deterministic suite:
//!   cargo test -p crew-lead --test m3_sprint
//!
//! The fourth test drives the same sprint through the real `claude` CLI via
//! `RoleHarnessBehavior` for all five roles. It is `#[ignore]`d by default —
//! manual/coordinator-only, never run by a worker session or CI (shared
//! rate limit) — run explicitly with:
//!   cargo test -p crew-lead --test m3_sprint -- --ignored --nocapture

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crew_agent::{AgentRunner, BusConn, RoleBehavior, RoleHarnessBehavior, ScriptedCrewMember};
use crew_bus::{BusConfig, BusHandle, BusServer};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::AgentCfg;
use crew_ledger::{spawn_subscriber, EventLedger};
use crew_lead::accept::AcceptanceLoop;
use crew_lead::dispatch::LeadBehavior;
use crew_lead::plan::{LeadPlanner, SprintSlicer};
use crew_proto::{Envelope, MessageKind, Role, TaskDag};
use tokio::net::TcpListener;

const BUS_TOKEN: &str = "m3-token";

/// Test-local wrapper (no src changes) mirroring `m2_roundtrip.rs`'s
/// `RecordingPm`: `AgentRunner::run` consumes `LeadBehavior` by value and
/// never returns it, so every envelope it emits is mirrored into `sent`
/// as it is produced, letting `change_request`/`human.gate` counts and
/// recipients be asserted after the run completes (plan D3).
struct RecordingLead {
    inner: LeadBehavior,
    sent: Arc<Mutex<Vec<(MessageKind, String)>>>,
}

impl RecordingLead {
    fn record(&self, envs: &[Envelope]) {
        let mut sent = self.sent.lock().unwrap();
        for env in envs {
            sent.push((env.kind, env.to.first().cloned().unwrap_or_default()));
        }
    }
}

#[async_trait]
impl RoleBehavior for RecordingLead {
    async fn on_start(&mut self) -> Vec<Envelope> {
        let envs = self.inner.on_start().await;
        self.record(&envs);
        envs
    }

    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        let envs = self.inner.on_envelope(env).await;
        self.record(&envs);
        envs
    }

    fn is_done(&self) -> bool {
        self.inner.is_done()
    }
}

async fn start_bus(retry_base: Duration) -> (BusHandle, String) {
    let mut cfg = BusConfig::new(BUS_TOKEN);
    cfg.retry_base = retry_base;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let handle = BusServer::new(cfg).serve(listener).await;
    let url = format!("ws://{}/ws", handle.local_addr());
    (handle, url)
}

/// M3's fixed 5-role linear DAG for one sprint (plan D2): specify -> plan_dag
/// -> slice into a single sprint (`max_per_sprint=7` comfortably fits the 5
/// tasks in one chunk).
fn build_sprint() -> (TaskDag, Vec<String>) {
    let spec = LeadPlanner::specify("간단한 랜딩 페이지").unwrap();
    let dag = LeadPlanner::plan_dag(&spec).unwrap();
    let sprint = SprintSlicer::slice(&dag, 7).unwrap().remove(0);
    (dag, sprint)
}

fn roles_all() -> Vec<(Role, String)> {
    vec![
        (Role::Pm, "agent:pm".to_string()),
        (Role::Designer, "agent:designer".to_string()),
        (Role::Publisher, "agent:publisher".to_string()),
        (Role::Developer, "agent:developer".to_string()),
        (Role::Qa, "agent:qa".to_string()),
    ]
}

/// A cwd for the spawned real-CLI role sessions that is NOT inside this git
/// worktree and NOT the OS temp dir (brief <constraints>: `/tmp`/`$TMPDIR`
/// forbidden; dev-loop's Stop hook also treats any cwd under this worktree
/// as carrying this task's gates ledger — see `m2_roundtrip.rs`'s
/// `isolated_cli_cwd` for the full rationale). One subdirectory per role so
/// the five concurrent sessions never share a cwd.
fn isolated_cli_cwd(role: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .expect("HOME must be set to derive a non-/tmp isolated cwd for the spawned CLI session");
    let dir = PathBuf::from(home)
        .join(".linkly-crew")
        .join("e2e-m3-cwd")
        .join(role);
    std::fs::create_dir_all(&dir).expect("create isolated cwd");
    dir
}

#[tokio::test]
async fn scenario1_happy_path_all_roles_accepted_and_ledger_records_delivery() {
    let (bus, url) = start_bus(Duration::from_millis(50)).await;
    let ledger = Arc::new(EventLedger::open_in_memory().unwrap());
    // Subscribe before any worker connects so no Delivered event is missed.
    spawn_subscriber(ledger.clone(), bus.subscribe());

    let lead_conn = BusConn::connect(&url, BUS_TOKEN, "agent:lead")
        .await
        .expect("lead connect");

    let worker_agents = [
        ("agent:pm", Role::Pm),
        ("agent:designer", Role::Designer),
        ("agent:publisher", Role::Publisher),
        ("agent:developer", Role::Developer),
        ("agent:qa", Role::Qa),
    ];
    let mut worker_tasks = Vec::new();
    for (agent_id, role) in worker_agents {
        let conn = BusConn::connect(&url, BUS_TOKEN, agent_id)
            .await
            .expect("worker connect");
        let member = ScriptedCrewMember::new(agent_id, role, vec![]);
        worker_tasks.push(tokio::spawn(AgentRunner::run(conn, member)));
    }

    let (dag, sprint) = build_sprint();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let lead = RecordingLead {
        inner: LeadBehavior::new(
            "agent:lead",
            dag,
            sprint,
            roles_all(),
            AcceptanceLoop::default_budget(),
        ),
        sent: sent.clone(),
    };

    let lead_result = tokio::time::timeout(Duration::from_secs(30), AgentRunner::run(lead_conn, lead))
        .await
        .expect("lead runner must finish within the 30s deterministic budget");
    assert!(
        lead_result.is_ok(),
        "lead runner should end cleanly: {lead_result:?}"
    );

    {
        let sent = sent.lock().unwrap();
        let change_requests = sent
            .iter()
            .filter(|(k, _)| *k == MessageKind::ChangeRequest)
            .count();
        let human_gates = sent.iter().filter(|(k, _)| *k == MessageKind::HumanGate).count();
        assert_eq!(change_requests, 0, "happy path must not need any rework");
        assert_eq!(human_gates, 0, "happy path must not escalate to a human");
    }

    assert!(
        ledger.count().unwrap() > 0,
        "ledger must record at least one bus event"
    );
    assert!(
        !ledger.events_of_kind("Delivered").unwrap().is_empty(),
        "ledger must record at least one Delivered event"
    );

    for task in worker_tasks {
        task.abort();
    }
    bus.shutdown().await;
}

#[tokio::test]
async fn scenario2_single_rework_round_then_accepted() {
    let (bus, url) = start_bus(Duration::from_millis(50)).await;
    let ledger = Arc::new(EventLedger::open_in_memory().unwrap());
    spawn_subscriber(ledger.clone(), bus.subscribe());

    let lead_conn = BusConn::connect(&url, BUS_TOKEN, "agent:lead")
        .await
        .expect("lead connect");

    // Only the designer plants a violation (REQ-2 omitted on the first
    // task.result); every other role covers fully on the first attempt.
    let worker_specs: [(&str, Role, &[&str]); 5] = [
        ("agent:pm", Role::Pm, &[]),
        ("agent:designer", Role::Designer, &["REQ-2"]),
        ("agent:publisher", Role::Publisher, &[]),
        ("agent:developer", Role::Developer, &[]),
        ("agent:qa", Role::Qa, &[]),
    ];
    let mut worker_tasks = Vec::new();
    for (agent_id, role, planted) in worker_specs {
        let conn = BusConn::connect(&url, BUS_TOKEN, agent_id)
            .await
            .expect("worker connect");
        let planted: Vec<String> = planted.iter().map(|s| s.to_string()).collect();
        let member = ScriptedCrewMember::new(agent_id, role, planted);
        worker_tasks.push(tokio::spawn(AgentRunner::run(conn, member)));
    }

    let (dag, sprint) = build_sprint();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let lead = RecordingLead {
        inner: LeadBehavior::new(
            "agent:lead",
            dag,
            sprint,
            roles_all(),
            AcceptanceLoop::default_budget(),
        ),
        sent: sent.clone(),
    };

    let lead_result = tokio::time::timeout(Duration::from_secs(30), AgentRunner::run(lead_conn, lead))
        .await
        .expect("lead runner must finish within the 30s deterministic budget");
    assert!(
        lead_result.is_ok(),
        "lead runner should end cleanly: {lead_result:?}"
    );

    {
        let sent = sent.lock().unwrap();
        let change_requests: Vec<_> = sent
            .iter()
            .filter(|(k, _)| *k == MessageKind::ChangeRequest)
            .collect();
        let human_gates = sent.iter().filter(|(k, _)| *k == MessageKind::HumanGate).count();
        assert_eq!(
            change_requests.len(),
            1,
            "exactly one planted violation must trigger exactly one rework round"
        );
        assert_eq!(change_requests[0].1, "agent:designer");
        assert_eq!(human_gates, 0, "one rework round within budget must not escalate");
    }

    assert!(ledger.count().unwrap() > 0, "ledger must record at least one bus event");

    for task in worker_tasks {
        task.abort();
    }
    bus.shutdown().await;
}

#[tokio::test]
async fn scenario3_unregistered_role_escalates_via_human_gate() {
    let (bus, url) = start_bus(Duration::from_millis(50)).await;
    let ledger = Arc::new(EventLedger::open_in_memory().unwrap());
    spawn_subscriber(ledger.clone(), bus.subscribe());

    let lead_conn = BusConn::connect(&url, BUS_TOKEN, "agent:lead")
        .await
        .expect("lead connect");

    // Qa intentionally has no worker connection: it is also missing from
    // the roles routing table below, so its task is never assigned.
    let worker_agents = [
        ("agent:pm", Role::Pm),
        ("agent:designer", Role::Designer),
        ("agent:publisher", Role::Publisher),
        ("agent:developer", Role::Developer),
    ];
    let mut worker_tasks = Vec::new();
    for (agent_id, role) in worker_agents {
        let conn = BusConn::connect(&url, BUS_TOKEN, agent_id)
            .await
            .expect("worker connect");
        let member = ScriptedCrewMember::new(agent_id, role, vec![]);
        worker_tasks.push(tokio::spawn(AgentRunner::run(conn, member)));
    }

    let (dag, sprint) = build_sprint();
    let roles = vec![
        (Role::Pm, "agent:pm".to_string()),
        (Role::Designer, "agent:designer".to_string()),
        (Role::Publisher, "agent:publisher".to_string()),
        (Role::Developer, "agent:developer".to_string()),
        // Qa intentionally unregistered.
    ];
    let sent = Arc::new(Mutex::new(Vec::new()));
    let lead = RecordingLead {
        inner: LeadBehavior::new(
            "agent:lead",
            dag,
            sprint,
            roles,
            AcceptanceLoop::default_budget(),
        ),
        sent: sent.clone(),
    };

    let lead_result = tokio::time::timeout(Duration::from_secs(30), AgentRunner::run(lead_conn, lead))
        .await
        .expect("lead runner must finish within the 30s deterministic budget");
    assert!(
        lead_result.is_ok(),
        "lead runner should end cleanly even when one task escalates: {lead_result:?}"
    );

    {
        let sent = sent.lock().unwrap();
        let change_requests = sent
            .iter()
            .filter(|(k, _)| *k == MessageKind::ChangeRequest)
            .count();
        let human_gates: Vec<_> = sent.iter().filter(|(k, _)| *k == MessageKind::HumanGate).collect();
        let task_assigns = sent
            .iter()
            .filter(|(k, _)| *k == MessageKind::TaskAssign)
            .count();
        assert_eq!(change_requests, 0, "no scripted violations were planted in this scenario");
        assert_eq!(human_gates.len(), 1, "the unregistered qa role must escalate exactly once");
        assert_eq!(human_gates[0].1, "agent:human");
        assert_eq!(
            task_assigns, 4,
            "the four registered roles must still be dispatched and accepted"
        );
    }

    for task in worker_tasks {
        task.abort();
    }
    bus.shutdown().await;
}

/// Real `claude` CLI E2E (plan D7) — #[ignore] by default, coordinator/manual
/// only (shared rate limit — never run from a worker session or CI). Same
/// M3 sprint as scenario 1, but every role is a real `ClaudeCodeHarness` +
/// `RoleHarnessBehavior` process instead of a scripted stand-in. Assertions
/// mirror scenario 1 (completion + ledger activity) — the model's actual
/// output content is not asserted: `RoleHarnessBehavior` coerces the reply
/// shape and the Lead's own DoD execution is what judges coverage.
#[tokio::test]
#[ignore]
async fn real_cli_five_role_sprint_completes() {
    let (bus, url) = start_bus(Duration::from_millis(250)).await;
    let ledger = Arc::new(EventLedger::open_in_memory().unwrap());
    spawn_subscriber(ledger.clone(), bus.subscribe());

    let lead_conn = BusConn::connect(&url, BUS_TOKEN, "agent:lead")
        .await
        .expect("lead connect");

    let worker_agents = [
        ("agent:pm", Role::Pm, "pm"),
        ("agent:designer", Role::Designer, "designer"),
        ("agent:publisher", Role::Publisher, "publisher"),
        ("agent:developer", Role::Developer, "developer"),
        ("agent:qa", Role::Qa, "qa"),
    ];
    let mut worker_tasks = Vec::new();
    for (agent_id, role, dir_name) in worker_agents {
        let conn = BusConn::connect(&url, BUS_TOKEN, agent_id)
            .await
            .expect("worker connect");
        let harness: Arc<dyn crew_harness::Harness> = Arc::new(ClaudeCodeHarness::new());
        let cfg = AgentCfg {
            cwd: isolated_cli_cwd(dir_name),
            model: None,
        };
        let system_hint = format!("You are the {role:?} of a web team building a simple landing page.");
        let behavior = RoleHarnessBehavior::new(harness, cfg, role, system_hint);
        worker_tasks.push(tokio::spawn(AgentRunner::run(conn, behavior)));
    }

    let (dag, sprint) = build_sprint();
    let lead = LeadBehavior::new(
        "agent:lead",
        dag,
        sprint,
        roles_all(),
        AcceptanceLoop::default_budget(),
    );

    let lead_result = tokio::time::timeout(Duration::from_secs(300), AgentRunner::run(lead_conn, lead))
        .await
        .expect("lead runner must finish within the 300s real-CLI budget");
    assert!(
        lead_result.is_ok(),
        "lead runner should end cleanly: {lead_result:?}"
    );
    assert!(
        ledger.count().unwrap() > 0,
        "ledger must record at least one bus event"
    );

    for task in worker_tasks {
        task.abort();
    }
    bus.shutdown().await;
}
