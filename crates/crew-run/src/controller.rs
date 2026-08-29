//! `RunController`/`RunHandle` — the m3_sprint.rs assembly pattern (bus →
//! ledger → single subscription loop → 5 workers → `ObservingLead`)
//! promoted to a reusable crate, contract §C3, extended by
//! contracts-m5.md §C5a to a sequential multi-sprint loop: one run's bus/
//! ledger/subscription-loop/broadcast/relay are created once (plan D1) and
//! reused across every sprint slice, while each sprint gets a fresh set of
//! workers and a fresh `LeadBehavior` seeded with the accumulated terminal
//! states of every earlier sprint (`with_prior_states`, contract C3a).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crew_agent::{AgentRunner, BusConn, RoleHarnessBehavior, RunnerError, ScriptedCrewMember};
use crew_bus::{BusConfig, BusEvent as BusLifecycleEvent, BusHandle, BusServer};
use crew_harness::{AgentCfg as HarnessAgentCfg, Harness, HarnessPool, HarnessRegistry};
use crew_lead::compress::summarize_sprint;
use crew_lead::dispatch::{LeadBehavior, TaskState};
use crew_lead::plan::{LeadPlanner, SprintSlicer};
use crew_ledger::EventLedger;
use crew_proto::{Envelope, Role, Roster, RosterAgent, SpecDoc, TaskDag};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};

use crate::config::{RunConfig, RunError, RunMode};
use crate::events::{now_ts, RosterAgentDto, RunEvent, RunOutcomeDto, RunSnapshot, StoredMessageDto, TaskStateDto};
use crate::observe::{ObservingLead, TaskStateChange};

const CHANNEL_CAPACITY: usize = 256;

/// L1 assembly budget for `summarize_sprint` (contracts-m5.md §C5a plan
/// D6) — ≈4k tokens, per backend/common/llm/context-window-budget.md's
/// "derive the cap from the serving model" guidance applied to a fixed L1
/// slice rather than the whole context window.
const L1_BUDGET_CHARS: usize = 16_000;

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

/// The default 6-slot roster (contracts-m5.md §C5a: `None` = lead + 5
/// roles, all `claude-code`/`"default"`) — team size is fixed (contract
/// §C0), only harness/model assignment varies per slot.
fn default_roster() -> Roster {
    let slot = |id: &str, role: &str| RosterAgent {
        id: id.to_string(),
        role: role.to_string(),
        harness: "claude-code".to_string(),
        model: "default".to_string(),
        instructions: String::new(),
    };
    Roster {
        agents: vec![
            slot("agent:lead", "lead"),
            slot("agent:pm", "pm"),
            slot("agent:designer", "designer"),
            slot("agent:publisher", "publisher"),
            slot("agent:developer", "developer"),
            slot("agent:qa", "qa"),
        ],
    }
}

fn roster_to_dto(roster: &Roster) -> Vec<RosterAgentDto> {
    roster.agents.iter().map(RosterAgentDto::from).collect()
}

/// The roster slot's harness id for `agent_id`, or `"claude-code"` if the
/// roster has no matching slot (plan D6 default).
fn harness_id_for(roster: &Roster, agent_id: &str) -> String {
    roster
        .agents
        .iter()
        .find(|a| a.id == agent_id)
        .map(|a| a.harness.clone())
        .unwrap_or_else(|| "claude-code".to_string())
}

/// L1 context assembled at every sprint boundary after the first (plan
/// D6): the spec goal plus every prior sprint's summary text, joined —
/// injected into `RealCli` workers' first turn via
/// `RoleHarnessBehavior::with_injected_context`.
fn l1_context(goal: &str, summaries_so_far: &[String]) -> String {
    format!("[공유 사실]\n목표: {goal}\n\n{}", summaries_so_far.join("\n\n"))
}

/// Shared, mutex-guarded run state — `snapshot()`'s source of truth
/// besides the ledger (plan D5).
struct SnapshotState {
    goal: String,
    spec: Option<SpecDoc>,
    dag: Option<TaskDag>,
    /// Every task id across every sprint, in topological order (plan D1)
    /// — the currently-active sprint's own subset is carried separately by
    /// each `SprintStarted.task_ids`.
    sprint: Vec<String>,
    task_states: Vec<(String, TaskStateDto)>,
    /// `messages` table seq — same space as `RunEvent::Message.seq`
    /// (contract §C3 seq-space rule).
    last_seq: i64,
}

/// One sprint's just-spawned workers/lead (plan D1/D5/D6).
struct SpawnedSprint {
    worker_aborts: Vec<AbortHandle>,
    lead_task: JoinHandle<Result<(), RunnerError>>,
}

/// Connects this sprint's five workers plus its `ObservingLead`-wrapped
/// Lead: `Scripted` always spawns a fresh `ScriptedCrewMember` per role per
/// sprint (session-restart semantics via a brand new instance, contract
/// C5a); `RealCli` resolves each role's harness id from `roster`, skipping
/// (logging, not failing) any role whose `HarnessRegistry::make` returns
/// `None` (plan D6 — assembly-only, this path is never exercised by a
/// worker test) and injecting `l1` on sprint index > 0.
#[allow(clippy::too_many_arguments)]
async fn spawn_sprint(
    url: &str,
    token: &str,
    mode: &RunMode,
    goal: &str,
    data_dir: &Path,
    roster: &Roster,
    pool: &Arc<HarnessPool>,
    dag: &TaskDag,
    sprint_tasks: &[String],
    l1: Option<&str>,
    max_rework: u32,
    escalation_timeout_ms: u64,
    cumulative: Arc<Mutex<HashMap<String, TaskState>>>,
    ts_tx: mpsc::UnboundedSender<TaskStateChange>,
) -> Result<SpawnedSprint, RunError> {
    let mut worker_aborts = Vec::new();
    for (agent_id, role) in roles_all() {
        let conn = BusConn::connect(url, token, agent_id).await?;
        match mode {
            RunMode::Scripted { planted_violations } => {
                let planted = planted_violations
                    .iter()
                    .find(|(r, _)| *r == role)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                let member = ScriptedCrewMember::new(agent_id, role, planted);
                let task = tokio::spawn(AgentRunner::run(conn, member));
                worker_aborts.push(task.abort_handle());
            }
            RunMode::RealCli => {
                let harness_id = harness_id_for(roster, agent_id);
                let harness: Option<Arc<dyn Harness>> = HarnessRegistry::make(&harness_id);
                let Some(harness) = harness else {
                    tracing::warn!(
                        agent_id,
                        harness_id,
                        "no adapter for this harness id; skipping worker spawn (RealCli assembly-only path)"
                    );
                    continue;
                };
                let harness_cfg = HarnessAgentCfg {
                    cwd: data_dir.join("cli-cwd").join(role_dir_name(role)),
                };
                let system_hint = format!("You are the {role:?} of a crew building: {goal}");
                let mut behavior = RoleHarnessBehavior::new(harness, harness_cfg, role, system_hint);
                if let Some(text) = l1 {
                    behavior = behavior.with_injected_context(text.to_string());
                }
                let pool = pool.clone();
                let acquire_id = harness_id.clone();
                let task = tokio::spawn(async move {
                    let _permit = pool.acquire(&acquire_id).await;
                    AgentRunner::run(conn, behavior).await
                });
                worker_aborts.push(task.abort_handle());
            }
        }
    }

    let lead_conn = BusConn::connect(url, token, "agent:lead").await?;
    let prior_states = cumulative.lock().expect("cumulative state mutex poisoned").clone();
    let lead_behavior = LeadBehavior::new(
        "agent:lead",
        dag.clone(),
        sprint_tasks.to_vec(),
        roles_routing(),
        max_rework,
    )
    .with_prior_states(prior_states)
    .escalation_timeout_ms(escalation_timeout_ms);
    let observing = ObservingLead::new(lead_behavior, sprint_tasks.to_vec(), ts_tx, cumulative);
    let lead_task = tokio::spawn(AgentRunner::run(lead_conn, observing));

    Ok(SpawnedSprint { worker_aborts, lead_task })
}

/// The abort handles for whichever sprint's workers/lead are currently
/// live — read by [`RunHandle::shutdown`], written by the orchestrator at
/// every sprint boundary (plan D1: dynamic per-sprint respawn means a
/// fixed field on `RunHandle` can no longer describe "the currently
/// running tasks").
struct LiveHandles {
    worker_aborts: Vec<AbortHandle>,
    lead_abort: AbortHandle,
}

/// Drains the subscription loop's already-buffered bus-event backlog
/// (plan D2/finisher pattern) so `ledger.messages_since` sees everything
/// this sprint (or, at the very end, this run) has produced so far before
/// the caller reads it.
async fn drain_subscription_backlog(drain_tx: &mpsc::UnboundedSender<oneshot::Sender<()>>) {
    let (ack_tx, ack_rx) = oneshot::channel();
    if drain_tx.send(ack_tx).is_ok() {
        let _ = ack_rx.await;
    }
}

pub struct RunController;

impl RunController {
    pub async fn start(cfg: RunConfig) -> Result<RunHandle, RunError> {
        let spec = LeadPlanner::specify(&cfg.goal)?;
        let dag = LeadPlanner::plan_dag(&spec)?;
        let effective_max = if cfg.max_per_sprint == 0 {
            dag.tasks.len().max(1)
        } else {
            cfg.max_per_sprint
        };
        let sprints = SprintSlicer::slice(&dag, effective_max)?;
        let full_order: Vec<String> = sprints.iter().flatten().cloned().collect();

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
            sprint: full_order.clone(),
            task_states: full_order
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
            sprint: full_order.clone(),
            ts: now_ts(),
        });

        // RosterChanged fires once right after RunStarted/SpecReady (plan
        // D4/contract C5a) — before any sprint's workers spawn.
        let roster = Arc::new(Mutex::new(cfg.roster.clone().unwrap_or_else(default_roster)));
        let _ = run_tx.send(RunEvent::RosterChanged {
            agents: roster_to_dto(&roster.lock().expect("roster mutex poisoned")),
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

        // One relay + one channel for the whole run (plan D1): every
        // sprint's `ObservingLead` gets its own clone of `ts_tx`.
        let (ts_tx, ts_rx) = mpsc::unbounded_channel::<TaskStateChange>();
        let relay_task = tokio::spawn(relay_loop(ts_rx, run_tx.clone(), snapshot.clone()));
        let relay_abort = relay_task.abort_handle();

        // One cumulative terminal-state map for the whole run (plan
        // D1/C3a): every sprint's `ObservingLead` writes its own task ids
        // into it, so `with_prior_states` for sprint N+1 already has every
        // earlier sprint's outcome.
        let cumulative: Arc<Mutex<HashMap<String, TaskState>>> = Arc::new(Mutex::new(HashMap::new()));
        let pool = HarnessPool::with_defaults();

        let sprint0 = sprints.first().cloned().unwrap_or_default();
        let _ = run_tx.send(RunEvent::SprintStarted {
            index: 0,
            task_ids: sprint0.clone(),
            ts: now_ts(),
        });
        let roster_snapshot0 = roster.lock().expect("roster mutex poisoned").clone();
        let spawned0 = spawn_sprint(
            &url,
            &token,
            &cfg.mode,
            &cfg.goal,
            &cfg.data_dir,
            &roster_snapshot0,
            &pool,
            &dag,
            &sprint0,
            None,
            cfg.max_rework,
            cfg.escalation_timeout_ms,
            cumulative.clone(),
            ts_tx.clone(),
        )
        .await?;
        let live = Arc::new(Mutex::new(LiveHandles {
            worker_aborts: spawned0.worker_aborts.clone(),
            lead_abort: spawned0.lead_task.abort_handle(),
        }));

        // Controller is the single `RunEvent` emission point (plan D4):
        // `ObservingLead` only sends to the relay's mpsc channel, never
        // broadcasts directly.
        let finisher_run_tx = run_tx.clone();
        let finisher_ledger = ledger.clone();
        let mode = cfg.mode.clone();
        let goal = cfg.goal.clone();
        let data_dir = cfg.data_dir.clone();
        let max_rework = cfg.max_rework;
        let escalation_timeout_ms = cfg.escalation_timeout_ms;
        let finisher_live = live.clone();
        let finisher: JoinHandle<RunOutcomeDto> = tokio::spawn(async move {
            let mut current_worker_aborts = spawned0.worker_aborts;
            let mut current_lead_task = spawned0.lead_task;

            let mut boundary_seq: i64 = 0;
            let mut summaries: Vec<String> = Vec::new();
            let mut outcome = RunOutcomeDto::Completed;

            for (index, sprint_tasks) in sprints.iter().enumerate() {
                if index > 0 {
                    let _ = finisher_run_tx.send(RunEvent::SprintStarted {
                        index: index as u32,
                        task_ids: sprint_tasks.clone(),
                        ts: now_ts(),
                    });
                    let l1 = l1_context(&goal, &summaries);
                    let roster_snapshot = roster.lock().expect("roster mutex poisoned").clone();
                    match spawn_sprint(
                        &url,
                        &token,
                        &mode,
                        &goal,
                        &data_dir,
                        &roster_snapshot,
                        &pool,
                        &dag,
                        sprint_tasks,
                        Some(&l1),
                        max_rework,
                        escalation_timeout_ms,
                        cumulative.clone(),
                        ts_tx.clone(),
                    )
                    .await
                    {
                        Ok(spawned) => {
                            *finisher_live.lock().expect("live handles mutex poisoned") = LiveHandles {
                                worker_aborts: spawned.worker_aborts.clone(),
                                lead_abort: spawned.lead_task.abort_handle(),
                            };
                            current_worker_aborts = spawned.worker_aborts;
                            current_lead_task = spawned.lead_task;
                        }
                        Err(err) => {
                            tracing::error!(error = %err, index, "failed to spawn sprint; ending run early");
                            outcome = RunOutcomeDto::Failed;
                            break;
                        }
                    }
                }

                let lead_result = (&mut current_lead_task).await;
                for handle in &current_worker_aborts {
                    handle.abort();
                }

                drain_subscription_backlog(&drain_tx).await;

                let stored = finisher_ledger.messages_since(boundary_seq).unwrap_or_default();
                boundary_seq = stored.iter().map(|m| m.seq).max().unwrap_or(boundary_seq);
                let sprint_messages: Vec<Envelope> = stored.into_iter().map(|m| m.envelope).collect();
                let states_snapshot = cumulative.lock().expect("cumulative state mutex poisoned").clone();
                let summary = summarize_sprint(
                    index as u32,
                    &spec,
                    sprint_tasks,
                    &states_snapshot,
                    &sprint_messages,
                    L1_BUDGET_CHARS,
                );
                summaries.push(summary.text.clone());
                let _ = finisher_run_tx.send(RunEvent::SprintFinished {
                    index: index as u32,
                    summary: summary.text,
                    ts: now_ts(),
                });

                let sprint_ok = matches!(lead_result, Ok(Ok(())));
                if !sprint_ok {
                    outcome = RunOutcomeDto::Failed;
                    break;
                }
            }

            // Every clone of `ts_tx` besides this one has already been
            // dropped (each sprint's `ObservingLead` was consumed and
            // dropped once its `lead_task` resolved) — dropping this last
            // one lets `relay_loop`'s `while let Some(..) = recv().await`
            // end so every already-queued `TaskStateChanged` is flushed
            // before `RunFinished`.
            drop(ts_tx);
            let _ = relay_task.await;

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
            live,
            sub_task,
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

/// Handle to a running crew — contract §C3, extended by contracts-m5.md
/// §C5a's multi-sprint loop.
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
    /// Whichever sprint's workers/lead are currently running (plan D1) —
    /// replaced at every sprint boundary by the orchestrator inside
    /// `RunController::start`'s spawned finisher task.
    live: Arc<Mutex<LiveHandles>>,
    sub_task: JoinHandle<()>,
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
    /// `live` is read fresh (plan D1: workers/lead are re-spawned at every
    /// sprint boundary, so a single fixed set of handles captured at
    /// `start()` time could no longer describe "what's currently running").
    pub async fn shutdown(self) {
        if let Some(finisher) = &self.finisher {
            finisher.abort();
        }
        {
            let live = self.live.lock().unwrap();
            for handle in &live.worker_aborts {
                handle.abort();
            }
            live.lead_abort.abort();
        }
        self.sub_task.abort();
        self.relay_abort.abort();
        self.bus.shutdown().await;
    }
}
