//! `RunController`/`RunHandle` — the m3_sprint.rs assembly pattern (bus →
//! ledger → single subscription loop → 5 workers → `ObservingLead`)
//! promoted to a reusable crate, contract §C3.

use std::sync::{Arc, Mutex};

use crew_agent::{AgentRunner, BusConn, RoleHarnessBehavior, RunnerError, ScriptedCrewMember};
use crew_bus::{BusConfig, BusEvent as BusLifecycleEvent, BusHandle, BusServer};
use crew_harness::claude::ClaudeCodeHarness;
use crew_harness::{AgentCfg as HarnessAgentCfg, Harness};
use crew_lead::dispatch::LeadBehavior;
use crew_lead::plan::{LeadPlanner, SprintSlicer};
use crew_ledger::EventLedger;
use crew_proto::{Role, SpecDoc, TaskDag};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};

use crate::config::{RunConfig, RunError, RunMode};
use crate::events::{now_ts, RunEvent, RunOutcomeDto, RunSnapshot, StoredMessageDto, TaskStateDto};
use crate::observe::{ObservingLead, TaskStateChange};

const CHANNEL_CAPACITY: usize = 256;

/// The five fixed M3 roles and their agent ids (m3_sprint.rs `roles_all`).
fn roles_all() -> [(&'static str, Role); 5] {
    [
        ("agent:pm", Role::Pm),
        ("agent:designer", Role::Designer),
        ("agent:publisher", Role::Publisher),
        ("agent:developer", Role::Developer),
        ("agent:qa", Role::Qa),
    ]
}

fn roles_routing() -> Vec<(Role, String)> {
    roles_all()
        .into_iter()
        .map(|(id, role)| (role, id.to_string()))
        .collect()
}

fn role_dir_name(role: Role) -> &'static str {
    match role {
        Role::Pm => "pm",
        Role::Designer => "designer",
        Role::Publisher => "publisher",
        Role::Developer => "developer",
        Role::Qa => "qa",
    }
}

/// Shared, mutex-guarded run state — `snapshot()`'s source of truth
/// besides the ledger (plan D5).
struct SnapshotState {
    goal: String,
    spec: Option<SpecDoc>,
    dag: Option<TaskDag>,
    sprint: Vec<String>,
    task_states: Vec<(String, TaskStateDto)>,
    /// `messages` table seq — same space as `RunEvent::Message.seq`
    /// (contract §C3 seq-space rule).
    last_seq: i64,
}

pub struct RunController;

impl RunController {
    pub async fn start(cfg: RunConfig) -> Result<RunHandle, RunError> {
        let spec = LeadPlanner::specify(&cfg.goal)?;
        let dag = LeadPlanner::plan_dag(&spec)?;
        // Fixed 5-task M3 template: one chunk always holds every task.
        let sprint = SprintSlicer::slice(&dag, dag.tasks.len().max(1))?
            .into_iter()
            .next()
            .unwrap_or_default();

        let run_id = uuid::Uuid::new_v4().to_string();
        let token = uuid::Uuid::new_v4().to_string();

        // Created together with its "genesis" receiver, which never misses
        // a send (unlike a receiver obtained from `subscribe()` later) —
        // handed to the first external `subscribe()` caller so it sees the
        // full event history from `RunStarted` onward (see `RunHandle`).
        let (run_tx, genesis_rx) = broadcast::channel::<RunEvent>(CHANNEL_CAPACITY);

        let snapshot = Arc::new(Mutex::new(SnapshotState {
            goal: cfg.goal.clone(),
            spec: Some(spec.clone()),
            dag: Some(dag.clone()),
            sprint: sprint.clone(),
            task_states: sprint
                .iter()
                .map(|id| (id.clone(), TaskStateDto::Pending))
                .collect(),
            last_seq: 0,
        }));

        let _ = run_tx.send(RunEvent::RunStarted {
            run_id: run_id.clone(),
            goal: cfg.goal.clone(),
            ts: now_ts(),
        });
        let _ = run_tx.send(RunEvent::SpecReady {
            spec: spec.clone(),
            dag: dag.clone(),
            sprint: sprint.clone(),
            ts: now_ts(),
        });

        // Best-effort: if this fails, `EventLedger::open` below fails for
        // the same reason and surfaces as `RunError::Ledger`.
        let _ = std::fs::create_dir_all(&cfg.data_dir);
        let ledger = Arc::new(EventLedger::open(&cfg.data_dir.join("ledger.sqlite"))?);

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding an ephemeral loopback port cannot fail in practice");
        let bus_cfg = BusConfig::new(token.clone());
        let bus = BusServer::new(bus_cfg).serve(listener).await;
        let url = format!("ws://{}/ws", bus.local_addr());

        // Subscribe before any worker connects (plan D2/D3) so no
        // `Registered`/`EnvelopeAccepted` event can be missed.
        let bus_events_rx = bus.subscribe();
        let (drain_tx, drain_rx) = mpsc::unbounded_channel::<oneshot::Sender<()>>();
        let sub_task = tokio::spawn(subscription_loop(
            ledger.clone(),
            bus_events_rx,
            run_tx.clone(),
            snapshot.clone(),
            drain_rx,
        ));

        let mut worker_tasks: Vec<JoinHandle<Result<(), RunnerError>>> = Vec::new();
        for (agent_id, role) in roles_all() {
            let conn = BusConn::connect(&url, &token, agent_id).await?;
            match &cfg.mode {
                RunMode::Scripted { planted_violations } => {
                    let planted = planted_violations
                        .iter()
                        .find(|(r, _)| *r == role)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    let member = ScriptedCrewMember::new(agent_id, role, planted);
                    worker_tasks.push(tokio::spawn(AgentRunner::run(conn, member)));
                }
                RunMode::RealCli => {
                    let harness: Arc<dyn Harness> = Arc::new(ClaudeCodeHarness::new());
                    let harness_cfg = HarnessAgentCfg {
                        cwd: cfg.data_dir.join("cli-cwd").join(role_dir_name(role)),
                    };
                    let system_hint =
                        format!("You are the {role:?} of a crew building: {}", cfg.goal);
                    let behavior = RoleHarnessBehavior::new(harness, harness_cfg, role, system_hint);
                    worker_tasks.push(tokio::spawn(AgentRunner::run(conn, behavior)));
                }
            }
        }

        let lead_conn = BusConn::connect(&url, &token, "agent:lead").await?;
        let (ts_tx, ts_rx) = mpsc::unbounded_channel::<TaskStateChange>();
        let lead_behavior = LeadBehavior::new(
            "agent:lead",
            dag.clone(),
            sprint.clone(),
            roles_routing(),
            cfg.max_rework,
        );
        let observing = ObservingLead::new(lead_behavior, sprint.clone(), ts_tx);
        let lead_task: JoinHandle<Result<(), RunnerError>> =
            tokio::spawn(AgentRunner::run(lead_conn, observing));
        let lead_abort = lead_task.abort_handle();

        let relay_task = tokio::spawn(relay_loop(ts_rx, run_tx.clone(), snapshot.clone()));
        let relay_abort = relay_task.abort_handle();

        // Controller is the single `RunEvent` emission point (plan D4):
        // `ObservingLead` only sends to the relay's mpsc channel, never
        // broadcasts directly.
        let finisher_run_tx = run_tx.clone();
        let finisher: JoinHandle<RunOutcomeDto> = tokio::spawn(async move {
            let lead_result = lead_task.await;
            // Every `EnvelopeAccepted`/lifecycle `BusEvent` this run will
            // ever produce was broadcast by the bus strictly before the
            // corresponding envelope was delivered to the Lead (contract
            // §C1: "Emitted exactly once ... before delivery is
            // attempted") — so by the time `lead_task` resolves, the
            // subscription loop's already-buffered backlog is everything
            // relevant. Ask it to drain that backlog before announcing
            // `RunFinished`, so a subscriber never observes `run_finished`
            // ahead of a `message`/`bus_lifecycle` event it should have
            // preceded.
            let (ack_tx, ack_rx) = oneshot::channel();
            if drain_tx.send(ack_tx).is_ok() {
                let _ = ack_rx.await;
            }
            // Wait for the relay to flush every already-queued
            // `TaskStateChanged` before announcing `RunFinished`.
            let _ = relay_task.await;
            let outcome = match lead_result {
                Ok(Ok(())) => RunOutcomeDto::Completed,
                _ => RunOutcomeDto::Failed,
            };
            let _ = finisher_run_tx.send(RunEvent::RunFinished {
                outcome,
                ts: now_ts(),
            });
            outcome
        });

        Ok(RunHandle {
            run_id,
            run_tx,
            first_rx: Mutex::new(Some(genesis_rx)),
            snapshot,
            ledger,
            bus,
            worker_tasks,
            sub_task,
            lead_abort,
            relay_abort,
            finisher: Some(finisher),
            joined_outcome: None,
        })
    }
}

/// The single subscription loop (plan D3): consumes the bus's own
/// `BusEvent` broadcast, appends every event to the ledger, then — only
/// after the append commits — broadcasts the corresponding `RunEvent`.
/// `EnvelopeAccepted` yields `Message` (seq = `messages` table seq, read
/// back via `messages_since`); every other `BusEvent` yields
/// `BusLifecycle` (seq = `append`'s return value, the `events` table seq —
/// contract §C3 seq-space rule, the two are never compared).
async fn subscription_loop(
    ledger: Arc<EventLedger>,
    mut bus_rx: broadcast::Receiver<BusLifecycleEvent>,
    run_tx: broadcast::Sender<RunEvent>,
    snapshot: Arc<Mutex<SnapshotState>>,
    mut drain_rx: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
) {
    loop {
        tokio::select! {
            biased;
            Some(ack) = drain_rx.recv() => {
                // Drain everything already buffered (see the finisher's
                // comment for why this is guaranteed to be everything
                // relevant once the Lead's own run has finished) before
                // acknowledging.
                while let Ok(event) = bus_rx.try_recv() {
                    handle_bus_event(&ledger, &run_tx, &snapshot, event);
                }
                let _ = ack.send(());
            }
            recv = bus_rx.recv() => {
                match recv {
                    Ok(event) => handle_bus_event(&ledger, &run_tx, &snapshot, event),
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "run controller's bus subscriber lagged; events dropped");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Appends one `BusEvent` to the ledger, then broadcasts the corresponding
/// `RunEvent` — ledger write always precedes the visible event (contract
/// §C3). `EnvelopeAccepted` yields `Message` (seq = `messages` table seq,
/// read back via `messages_since`); every other `BusEvent` yields
/// `BusLifecycle` (seq = `append`'s return value, the `events` table seq —
/// a different space, never compared to `Message.seq`).
fn handle_bus_event(
    ledger: &EventLedger,
    run_tx: &broadcast::Sender<RunEvent>,
    snapshot: &Mutex<SnapshotState>,
    event: BusLifecycleEvent,
) {
    let events_seq = match ledger.append(&event) {
        Ok(seq) => seq,
        Err(err) => {
            tracing::error!(error = %err, "failed to append bus event to ledger");
            return;
        }
    };

    if let BusLifecycleEvent::EnvelopeAccepted { .. } = &event {
        let prev_last_seq = snapshot.lock().unwrap().last_seq;
        match ledger.messages_since(prev_last_seq) {
            Ok(messages) => {
                for message in messages {
                    snapshot.lock().unwrap().last_seq = message.seq;
                    let _ = run_tx.send(RunEvent::Message {
                        seq: message.seq,
                        envelope: message.envelope,
                    });
                }
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to read messages_since");
            }
        }
    } else {
        let (kind, payload) = bus_lifecycle_kind_and_payload(&event);
        let _ = run_tx.send(RunEvent::BusLifecycle {
            seq: events_seq,
            kind,
            payload,
        });
    }
}

/// `kind` is the `BusEvent` variant name, taken from its externally-tagged
/// JSON's single top-level key — same technique `EventLedger::append` uses
/// internally, so the two never drift apart.
fn bus_lifecycle_kind_and_payload(event: &BusLifecycleEvent) -> (String, serde_json::Value) {
    let payload = serde_json::to_value(event).expect("BusEvent always serializes to JSON");
    let kind = payload
        .as_object()
        .and_then(|obj| obj.keys().next())
        .cloned()
        .expect("BusEvent's externally-tagged Serialize always yields a single-key object");
    (kind, payload)
}

/// Drains `ObservingLead`'s task-state-change relay, updating the shared
/// snapshot and broadcasting `TaskStateChanged` — the controller, not
/// `ObservingLead`, is the single `RunEvent` emission point (plan D4).
async fn relay_loop(
    mut ts_rx: mpsc::UnboundedReceiver<TaskStateChange>,
    run_tx: broadcast::Sender<RunEvent>,
    snapshot: Arc<Mutex<SnapshotState>>,
) {
    while let Some(change) = ts_rx.recv().await {
        {
            let mut snap = snapshot.lock().unwrap();
            match snap
                .task_states
                .iter_mut()
                .find(|(id, _)| *id == change.task_id)
            {
                Some(entry) => entry.1 = change.state,
                None => snap.task_states.push((change.task_id.clone(), change.state)),
            }
        }
        let _ = run_tx.send(RunEvent::TaskStateChanged {
            task_id: change.task_id,
            state: change.state,
            ts: now_ts(),
        });
    }
}

/// Handle to a running crew — contract §C3.
pub struct RunHandle {
    run_id: String,
    run_tx: broadcast::Sender<RunEvent>,
    /// The channel's own "genesis" receiver (alive since `broadcast::channel`
    /// was called, before any `RunEvent` was sent) — handed out on the
    /// first `subscribe()` call so that caller sees the full history from
    /// `RunStarted` onward; later callers get a normal live-tail receiver.
    first_rx: Mutex<Option<broadcast::Receiver<RunEvent>>>,
    snapshot: Arc<Mutex<SnapshotState>>,
    ledger: Arc<EventLedger>,
    bus: BusHandle,
    worker_tasks: Vec<JoinHandle<Result<(), RunnerError>>>,
    sub_task: JoinHandle<()>,
    lead_abort: AbortHandle,
    relay_abort: AbortHandle,
    finisher: Option<JoinHandle<RunOutcomeDto>>,
    joined_outcome: Option<RunOutcomeDto>,
}

impl RunHandle {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn subscribe(&self) -> broadcast::Receiver<RunEvent> {
        if let Some(rx) = self.first_rx.lock().unwrap().take() {
            return rx;
        }
        self.run_tx.subscribe()
    }

    /// Current state for reconnect/app-restart (contract §C3):
    /// `task_states`/`spec`/`dag`/`sprint` from the in-memory snapshot,
    /// `messages` from the ledger's `messages_since(0)`.
    pub fn snapshot(&self) -> RunSnapshot {
        let snap = self.snapshot.lock().unwrap();
        let messages = self.ledger.messages_since(0).unwrap_or_default();
        RunSnapshot {
            run_id: self.run_id.clone(),
            goal: snap.goal.clone(),
            spec: snap.spec.clone(),
            dag: snap.dag.clone(),
            sprint: snap.sprint.clone(),
            task_states: snap.task_states.clone(),
            messages: messages
                .into_iter()
                .map(|m| StoredMessageDto {
                    seq: m.seq,
                    envelope: m.envelope,
                })
                .collect(),
            last_seq: snap.last_seq,
            ts: now_ts(),
        }
    }

    /// Waits for the Lead runner to finish (contract §C3). Idempotent: a
    /// second call returns the same cached outcome instead of panicking on
    /// an already-consumed join handle.
    pub async fn join(&mut self) -> Result<(), RunError> {
        let outcome = if let Some(outcome) = self.joined_outcome {
            outcome
        } else {
            let outcome = match self.finisher.take() {
                Some(handle) => handle.await.unwrap_or(RunOutcomeDto::Failed),
                None => RunOutcomeDto::Failed,
            };
            self.joined_outcome = Some(outcome);
            outcome
        };
        match outcome {
            RunOutcomeDto::Completed => Ok(()),
            RunOutcomeDto::Failed => Err(RunError::Join("lead runner did not complete".to_string())),
        }
    }

    /// Worker/lead/relay/subscription tasks abort, then the bus shuts down
    /// (m3 pattern). Safe to call regardless of whether the run already
    /// finished on its own — aborting an already-finished task is a no-op.
    pub async fn shutdown(self) {
        for task in &self.worker_tasks {
            task.abort();
        }
        self.sub_task.abort();
        self.lead_abort.abort();
        self.relay_abort.abort();
        if let Some(finisher) = &self.finisher {
            finisher.abort();
        }
        self.bus.shutdown().await;
    }
}
