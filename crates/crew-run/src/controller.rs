//! `RunController`/`RunHandle` — the m3_sprint.rs assembly pattern (bus →
//! ledger → single subscription loop → 5 workers → `ObservingLead`)
//! promoted to a reusable crate, contract §C3, extended by
//! contracts-m5.md §C5a to a sequential multi-sprint loop: one run's bus/
//! ledger/subscription-loop/broadcast/relay are created once (plan D1) and
//! reused across every sprint slice, while each sprint gets a fresh set of
//! workers and a fresh `LeadBehavior` seeded with the accumulated terminal
//! states of every earlier sprint (`with_prior_states`, contract C3a).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crew_agent::{AgentControl, AgentRunner, BusConn, RoleHarnessBehavior, RunnerError, ScriptedCrewMember};
use crew_bus::{BusConfig, BusEvent as BusLifecycleEvent, BusHandle, BusServer};
use crew_harness::{AgentCfg as HarnessAgentCfg, HandoffSnapshot, Harness, HarnessPool, HarnessRegistry};
use crew_lead::compress::summarize_sprint;
use crew_lead::dispatch::{LeadBehavior, TaskState};
use crew_lead::plan::{LeadPlanner, PlanError, PlanOptions, SprintSlicer};
use crew_lead::plan_llm::LlmLeadPlanner;
use crew_ledger::{EventLedger, StoredMessage};
use crew_proto::{
    handoff_body, presence_read_from_body, presence_typing_from_body, Envelope, HandoffPack, MessageKind, Role,
    Roster, RosterAgent, SpecDoc, TaskDag,
};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};

use crate::config::{GateDecision, RunConfig, RunError, RunMode};
use crate::events::{
    now_ts, PresenceKindDto, RosterAgentDto, RunEvent, RunOutcomeDto, RunSnapshot, StoredMessageDto, TaskStateDto,
};
use crate::observe::{ObservingLead, TaskStateChange};
use crate::worktree;

const CHANNEL_CAPACITY: usize = 256;

/// `RunHandle::swap_harness` step 3's ack wait (contracts-m6.md §D2b,
/// 2026-08-29 보정: the control channel is only serviced *between* turns —
/// t-ctrl's `run_with_control` select loop — so a mid-turn swap's ack waits
/// for the current turn to finish. Real CLI turns measured 17-193s (m5a
/// E2E), so a short timeout would misreport a normal in-flight turn as
/// `SwapIncomplete`. Deterministic tests ack immediately regardless of this
/// value.
pub(crate) const SWAP_ACK_TIMEOUT_MS: u64 = 120_000;

/// L1 assembly budget for `summarize_sprint` (contracts-m5.md §C5a plan
/// D6) — ≈4k tokens, per backend/common/llm/context-window-budget.md's
/// "derive the cap from the serving model" guidance applied to a fixed L1
/// slice rather than the whole context window.
const L1_BUDGET_CHARS: usize = 16_000;

/// The five fixed M3 roles and their agent ids (m3_sprint.rs `roles_all`).
/// `spawn_sprint` no longer calls this (contracts-m7.md §E4: `crew_agents`
/// replaces it on the spawn path) — kept, unused, for the default path per
/// §E4's explicit "함수 자체는 default 경로·테스트용으로 존치 가능".
#[allow(dead_code)]
fn roles_all() -> [(&'static str, Role); 5] {
    [
        ("agent:pm", Role::Pm),
        ("agent:designer", Role::Designer),
        ("agent:publisher", Role::Publisher),
        ("agent:developer", Role::Developer),
        ("agent:qa", Role::Qa),
    ]
}

#[allow(dead_code)]
fn roles_routing() -> Vec<(Role, String)> {
    roles_all()
        .into_iter()
        .map(|(id, role)| (role, id.to_string()))
        .collect()
}

pub(crate) fn role_dir_name(role: Role) -> &'static str {
    match role {
        Role::Pm => "pm",
        Role::Designer => "designer",
        Role::Publisher => "publisher",
        Role::Developer => "developer",
        Role::Qa => "qa",
    }
}

/// The agent CLI cwd and the Cmd DoD exec cwd — the same seam feeds both
/// call sites in `spawn_sprint` (contracts-m11.md §I1/§I3).
///
/// `Some(role_worktrees)` — `project_root` is set (M11 + this task's plan
/// D1-D3, HANDOFF pitfall 30): a pure lookup into the role-\>worktree cache
/// `RunController::start` precomputes once via `resolve_role_worktrees`, so
/// every role resolves to its own out-of-repo `git worktree` rather than
/// `project_root` verbatim (the pitfall 30 bug this replaces). Kept
/// infallible — and its 16 call sites/tests untouched by `Result` — by
/// resolving `ensure_role_worktree`'s fallibility once up front instead of
/// on every lookup; a missing entry means `start` failed to populate every
/// `ROLE_ORDER` role, which is a `resolve_role_worktrees` bug, not a
/// reachable runtime state.
///
/// `None` — pre-M11 behavior, unchanged (t-swap follow-up fix,
/// coordinator-run real-CLI spot check): `data_dir/cli-cwd/<role>`, created
/// best-effort — mirroring `RunController::start`'s own
/// `create_dir_all(&cfg.data_dir)` — because `Command::current_dir` on a
/// missing directory fails the CLI spawn with ENOENT ("failed to spawn
/// claude process: No such file or directory"), which every worker then
/// reports as blocked, and with the default `escalation_timeout_ms=0` the
/// run hangs forever waiting on a human gate that never fires.
fn role_cli_cwd(role_worktrees: Option<&[(Role, PathBuf)]>, data_dir: &Path, role: Role) -> PathBuf {
    if let Some(worktrees) = role_worktrees {
        return worktrees
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, path)| path.clone())
            .unwrap_or_else(|| {
                panic!("role_cli_cwd: no precomputed worktree for role {role:?} — resolve_role_worktrees must populate every ROLE_ORDER entry")
            });
    }
    let cwd = data_dir.join("cli-cwd").join(role_dir_name(role));
    let _ = std::fs::create_dir_all(&cwd);
    cwd
}

/// Precomputes every canonical role's (`ROLE_ORDER`) worktree once for a
/// `project_root` (plan D1-D3): factored out of `RunController::start` so
/// it's unit-testable with an injected `worktrees_base` — the production
/// call site always derives `worktrees_base` from
/// `worktree::project_worktrees_base`, tests inject a tempdir instead
/// (tools_guidance: never touch `$HOME` from a test). Fails on the first
/// role `ensure_role_worktree` can't set up (design D5) — no partial cache
/// is returned.
fn resolve_role_worktrees(project_root: &Path, worktrees_base: &Path) -> Result<Vec<(Role, PathBuf)>, String> {
    let mut worktrees = Vec::with_capacity(ROLE_ORDER.len());
    for role in ROLE_ORDER {
        let path = worktree::ensure_role_worktree(project_root, worktrees_base, role)?;
        worktrees.push((role, path));
    }
    Ok(worktrees)
}

/// The RealCli spawn spec's system hint (`spawn_sprint`'s
/// `RoleHarnessBehavior::new` argument) — factored out so the artifacts
/// convention injection (design D4) is unit-testable without a live
/// bus/harness (RealCli `spawn_sprint` itself stays assembly-only, never
/// exercised by a worker test). `project_root` is the original repo root
/// (not any role's worktree, contract deliberately shared across roles),
/// present only when `RunConfig.project_root` is `Some`.
fn role_system_hint(role: Role, goal: &str, project_root: Option<&Path>) -> String {
    let mut hint = format!("You are the {role:?} of a crew building: {goal}");
    if let Some(root) = project_root {
        hint.push_str(&format!(
            "\n\n공유 산출물·문서·이미지·디자인 토큰은 {}/.crew/artifacts 에서 주고받는다(하위 docs/ images/ design-tokens/ deliverables/). \
             같은 파일을 동시에 편집하지 말고 역할별 파일로 분리하라. \
             프로젝트에 메인 체크아웃 쓰기를 차단하는 훅을 추가하지 말라.",
            root.display()
        ));
    }
    hint
}

/// A roster slot's `role` string (`RosterAgent::role`) to `crew_proto::Role`
/// — `None` for `"lead"` (and anything unrecognized), since that enum has
/// no `Lead` variant (t-swap plan D3): `swap_harness` skips the handoff
/// envelope for the lead slot.
fn role_from_str(role: &str) -> Option<Role> {
    match role {
        "pm" => Some(Role::Pm),
        "designer" => Some(Role::Designer),
        "publisher" => Some(Role::Publisher),
        "developer" => Some(Role::Developer),
        "qa" => Some(Role::Qa),
        _ => None,
    }
}

/// Canonical role order (contracts-m7.md §E1's `ROLE_ORDER`, mirrored here
/// since `crew-run` has no access to `crew-lead::plan`'s private constant)
/// — `crew_agents`' sort key.
const ROLE_ORDER: [Role; 5] = [Role::Pm, Role::Designer, Role::Publisher, Role::Developer, Role::Qa];

/// Non-lead roster slots with a recognized role, sorted into canonical role
/// order (contracts-m7.md §E4 verbatim) — the vary-roster replacement for
/// `roles_all()` in `spawn_sprint`'s worker loop and its `LeadBehavior`
/// routing table. `role_from_str` already returns `None` for `"lead"`, so a
/// single `filter_map` excludes both the lead slot and any unrecognized
/// role in one pass.
fn crew_agents(roster: &Roster) -> Vec<(String, Role)> {
    let mut agents: Vec<(String, Role)> = roster
        .agents
        .iter()
        .filter_map(|a| role_from_str(&a.role).map(|role| (a.id.clone(), role)))
        .collect();
    agents.sort_by_key(|(_, role)| ROLE_ORDER.iter().position(|r| r == role).unwrap_or(usize::MAX));
    agents
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

/// Validates a roster before any spawn (contracts-m7.md §E4): a `"lead"`
/// slot must exist, at least one non-lead slot must exist (crew ≥1), every
/// non-lead slot's role string must be recognized (`role_from_str`), and no
/// recognized role may repeat. Checked in that order so each violation
/// class (crew-zero vs. unknown-role vs. duplicate) surfaces its own
/// distinct message naming the offending value.
pub fn validate_roster(roster: &Roster) -> Result<(), String> {
    if !roster.agents.iter().any(|a| a.role == "lead") {
        return Err("roster has no \"lead\" slot".to_string());
    }
    let crew: Vec<&RosterAgent> = roster.agents.iter().filter(|a| a.role != "lead").collect();
    if crew.is_empty() {
        return Err("roster has no crew slots (lead only)".to_string());
    }
    for agent in &crew {
        if role_from_str(&agent.role).is_none() {
            return Err(format!("unknown role \"{}\" for agent \"{}\"", agent.role, agent.id));
        }
    }
    let mut seen: Vec<Role> = Vec::new();
    for agent in &crew {
        let role = role_from_str(&agent.role).expect("checked unknown-role above");
        if seen.contains(&role) {
            return Err(format!("duplicate role \"{}\" (agent \"{}\")", agent.role, agent.id));
        }
        seen.push(role);
    }
    Ok(())
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

/// One sprint's just-spawned workers/lead (plan D1/D5/D6), plus this
/// sprint's worker control-channel registry (t-swap plan D1/D2:
/// `agent_id` -> the `mpsc::Sender` `AgentRunner::run_with_control` reads
/// from — lead is never a key here, contracts-m6.md §D2a).
struct SpawnedSprint {
    worker_aborts: Vec<AbortHandle>,
    lead_task: JoinHandle<Result<(), RunnerError>>,
    controls: HashMap<String, mpsc::Sender<AgentControl>>,
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
    project_root: Option<&Path>,
    role_worktrees: Option<&[(Role, PathBuf)]>,
    roster: &Roster,
    pool: &Arc<HarnessPool>,
    dag: &TaskDag,
    sprint_tasks: &[String],
    l1: Option<&str>,
    max_rework: u32,
    escalation_timeout_ms: u64,
    turn_timeout: Duration,
    cumulative: Arc<Mutex<HashMap<String, TaskState>>>,
    ts_tx: mpsc::UnboundedSender<TaskStateChange>,
) -> Result<SpawnedSprint, RunError> {
    let mut worker_aborts = Vec::new();
    let mut controls: HashMap<String, mpsc::Sender<AgentControl>> = HashMap::new();
    // Vary-roster spawn/routing (contracts-m7.md §E4): `crew` replaces the
    // fixed `roles_all()` here and feeds `LeadBehavior`'s routing table
    // below, so both reflect exactly this roster's non-lead, recognized-role
    // slots.
    let crew = crew_agents(roster);
    for (agent_id, role) in &crew {
        let agent_id: &str = agent_id.as_str();
        let role = *role;
        let conn = BusConn::connect(url, token, agent_id).await?;
        // Every worker (Scripted included, plan D3) gets a control channel
        // so `swap_harness` step 3 has a real send target to test against;
        // the default `RoleBehavior::on_control` no-op-acks for behaviors
        // with no live session.
        let (ctrl_tx, ctrl_rx) = mpsc::channel::<AgentControl>(4);
        match mode {
            RunMode::Scripted { planted_violations } => {
                let planted = planted_violations
                    .iter()
                    .find(|(r, _)| *r == role)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                let member = ScriptedCrewMember::new(agent_id, role, planted);
                let task = tokio::spawn(AgentRunner::run_with_control(conn, member, ctrl_rx));
                worker_aborts.push(task.abort_handle());
                controls.insert(agent_id.to_string(), ctrl_tx);
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
                    cwd: role_cli_cwd(role_worktrees, data_dir, role),
                    model: None,
                };
                let system_hint = role_system_hint(role, goal, project_root);
                // Permit is scoped to each CLI interaction (turn), not the
                // runner's lifetime (contracts-m6.md §D2d) — a runner-
                // lifetime `pool.acquire` plus `RoleHarnessBehavior`'s
                // never-`is_done()` loop meant only `claude-code`'s default
                // limit-2 workers could ever start their bus loop, so the
                // 3rd `task.assign` onward hung forever (real-CLI spot
                // check). `with_pool` acquires/drops the permit around each
                // turn's spawn+send instead.
                let mut behavior = RoleHarnessBehavior::new(harness, harness_cfg, role, system_hint)
                    .with_pool(pool.clone(), harness_id.clone())
                    .with_turn_timeout(turn_timeout);
                if let Some(text) = l1 {
                    behavior = behavior.with_injected_context(text.to_string());
                }
                let task = tokio::spawn(AgentRunner::run_with_control(conn, behavior, ctrl_rx));
                worker_aborts.push(task.abort_handle());
                controls.insert(agent_id.to_string(), ctrl_tx);
            }
        }
    }

    let lead_conn = BusConn::connect(url, token, "agent:lead").await?;
    let routing: Vec<(Role, String)> = crew.iter().map(|(id, role)| (*role, id.clone())).collect();
    let prior_states = cumulative.lock().expect("cumulative state mutex poisoned").clone();
    let cmd_cwd_base = data_dir.to_path_buf();
    let cmd_role_worktrees = role_worktrees.map(<[(Role, PathBuf)]>::to_vec);
    let lead_behavior = LeadBehavior::new(
        "agent:lead",
        dag.clone(),
        sprint_tasks.to_vec(),
        routing,
        max_rework,
    )
    .with_prior_states(prior_states)
    .escalation_timeout_ms(escalation_timeout_ms)
    .with_cmd_exec(Arc::new(move |role| {
        role_cli_cwd(cmd_role_worktrees.as_deref(), &cmd_cwd_base, role)
    }));
    let observing = ObservingLead::new(lead_behavior, sprint_tasks.to_vec(), ts_tx, cumulative);
    // Lead never gets a control channel (contracts-m6.md §D2a, plan D2/plan
    // D6, pitfall 14) — lead swaps stay M5 boundary-only via `AgentRunner::run`.
    let lead_task = tokio::spawn(AgentRunner::run(lead_conn, observing));

    Ok(SpawnedSprint { worker_aborts, lead_task, controls })
}

/// The abort handles for whichever sprint's workers/lead are currently
/// live — read by [`RunHandle::shutdown`], written by the orchestrator at
/// every sprint boundary (plan D1: dynamic per-sprint respawn means a
/// fixed field on `RunHandle` can no longer describe "the currently
/// running tasks").
struct LiveHandles {
    worker_aborts: Vec<AbortHandle>,
    lead_abort: AbortHandle,
    /// This sprint's worker control-channel registry (t-swap plan D1) —
    /// `swap_harness` step 3 reads this to reach a live worker immediately.
    /// Cleared (not just left stale) the moment this sprint's workers are
    /// aborted, before the next sprint's `spawn_sprint` has produced a
    /// fresh map: a sender whose receiving task was just aborted would
    /// otherwise still be present here, and sending to it races the
    /// runtime's cancellation instead of cleanly falling back to the
    /// sender-absent path (contracts-m6.md §D2b step 3's "sender 부재" case).
    controls: HashMap<String, mpsc::Sender<AgentControl>>,
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

/// Capacity for the `RunHandle::resolve_gate` -> human proxy command
/// channel (plan D2) — gate decisions are rare, human-paced events, not a
/// throughput path.
const GATE_CHANNEL_CAPACITY: usize = 8;

/// One `RunHandle::resolve_gate` request routed to the human proxy task
/// (contracts-m7.md §E5, plan D2). `reply` carries the bus-publish outcome
/// back so `resolve_gate` can distinguish "published" from a bus-level
/// failure; a dropped `reply` (proxy aborted mid-flight) is treated by
/// `resolve_gate` the same as a closed `gate_tx`.
struct GateCmd {
    task_id: String,
    decision: GateDecision,
    reason: String,
    reply: oneshot::Sender<Result<(), String>>,
}

/// Run-resident `agent:human` proxy (contracts-m7.md §E5, plan D1/D3): its
/// own `recv()` is drained and discarded — `agent:human` is never itself a
/// message *source* except through `resolve_gate` — which is enough for the
/// bus to treat `agent:human` as a live recipient, turning what used to be
/// an `unknown_recipient` rejection for `human.gate` into normal delivery.
/// `GateCmd`s arriving on `gate_rx` are published as `human.response`
/// envelopes (§E2 verbatim) over this same connection.
///
/// `shutdown_rx` (rather than an `AbortHandle`) is the *natural*-end signal
/// (`RunController::start`'s finisher, plan D1): the bus never sends a
/// requires_ack=false envelope's own publish `Receipt` back to its sender on
/// first attempt (only a later retry's dedup path does, contracts-m7.md
/// §E2's fixed `requires_ack: false` runs straight into that), so a
/// resolve_gate call whose decision itself completes the run can still be
/// mid-flight in `conn.send` when the lead task finishes. `biased` ensures a
/// pending `GateCmd` is always drained ahead of a same-tick shutdown signal,
/// and — because a `select!` arm always runs its body to completion once
/// chosen — an *in-flight* `conn.send` is never interrupted by it either.
/// `RunHandle::shutdown` (an already-running, not-yet-finished run) instead
/// aborts this task outright via its `AbortHandle`, matching every other
/// resource's forceful teardown there.
async fn human_proxy_loop(
    mut conn: BusConn,
    mut gate_rx: mpsc::Receiver<GateCmd>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            biased;
            cmd = gate_rx.recv() => {
                let Some(cmd) = cmd else { break };
                let decision_str = match cmd.decision {
                    GateDecision::Approve => "approve",
                    GateDecision::Reject => "reject",
                };
                let envelope = Envelope::new(
                    // Literal duplicate of crew-lead::dispatch's private
                    // `SPRINT_LABEL`/`DEADLINE_MS` (mirrors `swap_harness`'s
                    // same tradeoff above — that crate is out of scope here).
                    "sp-m3".to_string(),
                    format!("th-gate-{}", cmd.task_id),
                    "agent:human".to_string(),
                    vec!["agent:lead".to_string()],
                    MessageKind::HumanResponse,
                    None,
                    format!("gate-{}", uuid::Uuid::new_v4()),
                    serde_json::json!({
                        "task_id": cmd.task_id,
                        "decision": decision_str,
                        "reason": cmd.reason,
                    }),
                    Vec::new(),
                    false,
                    900_000,
                );
                let result = conn.send(envelope).await.map_err(|e| e.to_string());
                let _ = cmd.reply.send(result);
            }
            recv = conn.recv() => {
                if recv.is_none() {
                    break;
                }
                // Drain-only: `human.gate`/other inbound envelopes are
                // observed by the controller's own `subscription_loop` off
                // the bus's broadcast, not through this connection.
            }
            _ = &mut shutdown_rx => {
                break;
            }
        }
    }
}

pub struct RunController;

impl RunController {
    pub async fn start(cfg: RunConfig) -> Result<RunHandle, RunError> {
        // Validated before anything else (M13 turn-recovery fix D4) — `0`
        // would make every turn instantly time out; a misconfiguration must
        // not silently become that.
        if cfg.turn_timeout_secs == 0 {
            return Err(RunError::ConfigInvalid(
                "turn_timeout_secs must be > 0".to_string(),
            ));
        }
        let turn_timeout = Duration::from_secs(cfg.turn_timeout_secs);

        // Validated before anything else, including the roster check right
        // below (contracts-m11.md §I2) — same rationale: a rejected config
        // leaves zero partial run state. No fallback to the scratch cwd on
        // failure — that would reproduce M10's exit-101 confusion
        // (docs/SPIKE-M10.md §3).
        let project_root: Option<std::path::PathBuf> = match &cfg.project_root {
            None => None,
            Some(p) => {
                if !p.is_absolute() {
                    return Err(RunError::ProjectRootInvalid(format!(
                        "must be an absolute path, got: {p:?}"
                    )));
                }
                if !p.is_dir() {
                    return Err(RunError::ProjectRootInvalid(format!(
                        "must be an existing directory, got: {p:?}"
                    )));
                }
                // D5: resolve once so both call sites (agent CLI cwd, Cmd DoD
                // exec cwd) agree on one path.
                Some(std::fs::canonicalize(p).map_err(|e| {
                    RunError::ProjectRootInvalid(format!("could not resolve {p:?}: {e}"))
                })?)
            }
        };

        // Plan D1-D3 (HANDOFF pitfall 30): pre-create every canonical
        // role's out-of-repo worktree once, up front — role_cli_cwd (both
        // the agent CLI cwd and the Cmd DoD exec cwd call sites) then does
        // a pure, infallible lookup into this cache rather than threading a
        // `Result` through spawn_sprint's per-role loop or crew-lead's
        // infallible `with_cmd_exec` closure signature. No fallback on
        // failure (design D5) — same rationale as the project_root
        // validation right above.
        let role_worktrees: Option<Vec<(Role, std::path::PathBuf)>> = match &project_root {
            None => None,
            Some(root) => {
                let worktrees_base = worktree::project_worktrees_base(root);
                Some(resolve_role_worktrees(root, &worktrees_base).map_err(RunError::ProjectRootInvalid)?)
            }
        };

        // Validated before any spec/dag/ledger/spawn work (contracts-m7.md
        // §E4 plan D5) — a rejected roster leaves zero partial run state.
        let roster = cfg.roster.clone().unwrap_or_else(default_roster);
        validate_roster(&roster).map_err(RunError::RosterInvalid)?;
        let present_roles: Vec<Role> = crew_agents(&roster).into_iter().map(|(_, role)| role).collect();

        let spec = match &cfg.mode {
            RunMode::Scripted { .. } => LeadPlanner::specify(&cfg.goal)?,
            RunMode::RealCli => {
                // Lead slot's harness id, falling back to claude-code if the
                // roster names an id with no adapter (contracts-m5.md §C3c
                // controller wiring — mirrors spawn_sprint's RealCli branch).
                let harness_id = harness_id_for(&roster, "agent:lead");
                let harness = HarnessRegistry::make(&harness_id)
                    .or_else(|| HarnessRegistry::make("claude-code"))
                    .ok_or_else(|| {
                        PlanError::LlmSpecify(format!(
                            "no harness adapter for lead harness id \"{harness_id}\" or fallback \"claude-code\""
                        ))
                    })?;
                LlmLeadPlanner::specify_with_timeout(harness, &cfg.goal, turn_timeout).await?
            }
        };
        // present = crew_agents' roles (contracts-m7.md §E4) — the LLM path
        // varies only the SpecDoc, DAG shaping is the same function either way.
        let dag = LeadPlanner::plan_dag_with(
            &spec,
            &present_roles,
            &PlanOptions {
                dev_cmd_checks: cfg.dev_cmd_checks.clone(),
            },
        )?;
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
        // D4/contract C5a) — before any sprint's workers spawn. Reuses the
        // already-validated `roster` resolved at the top of `start`.
        let roster = Arc::new(Mutex::new(roster));
        let _ = run_tx.send(RunEvent::RosterChanged {
            agents: roster_to_dto(&roster.lock().expect("roster mutex poisoned")),
            ts: now_ts(),
        });
        // Cloned before the finisher's `async move` block below moves
        // `roster` in (t-swap plan D1) — `RunHandle::swap_harness` needs its
        // own handle on the same `Arc<Mutex<Roster>>` the sprint loop reads.
        let roster_for_handle = roster.clone();
        // Every sprint's summary text, in order (t-swap plan D7) —
        // `swap_harness` step 3 assembles the same L1 injected-context text
        // sprint boundaries inject (`l1_context`), which needs this list;
        // it isn't otherwise reachable from `RunHandle`.
        let shared_summaries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let summaries_for_handle = shared_summaries.clone();

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

        // Run-resident `agent:human` proxy (contracts-m7.md §E5, plan D1):
        // connected once here, after `bus_events_rx` subscribes (so its own
        // `Registered` bus lifecycle event isn't missed) and before any
        // sprint's workers/lead spawn.
        let human_conn = BusConn::connect(&url, &token, "agent:human").await?;
        let (gate_tx, gate_rx) = mpsc::channel::<GateCmd>(GATE_CHANNEL_CAPACITY);
        let (human_shutdown_tx, human_shutdown_rx) = oneshot::channel::<()>();
        let human_task = tokio::spawn(human_proxy_loop(human_conn, gate_rx, human_shutdown_rx));
        let human_abort = human_task.abort_handle();

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
            project_root.as_deref(),
            role_worktrees.as_deref(),
            &roster_snapshot0,
            &pool,
            &dag,
            &sprint0,
            None,
            cfg.max_rework,
            cfg.escalation_timeout_ms,
            turn_timeout,
            cumulative.clone(),
            ts_tx.clone(),
        )
        .await?;
        let live = Arc::new(Mutex::new(LiveHandles {
            worker_aborts: spawned0.worker_aborts.clone(),
            lead_abort: spawned0.lead_task.abort_handle(),
            controls: spawned0.controls.clone(),
        }));

        // Controller is the single `RunEvent` emission point (plan D4):
        // `ObservingLead` only sends to the relay's mpsc channel, never
        // broadcasts directly.
        let finisher_run_tx = run_tx.clone();
        let finisher_ledger = ledger.clone();
        let mode = cfg.mode.clone();
        let goal = cfg.goal.clone();
        let data_dir = cfg.data_dir.clone();
        let project_root = project_root.clone();
        let role_worktrees = role_worktrees.clone();
        let max_rework = cfg.max_rework;
        let escalation_timeout_ms = cfg.escalation_timeout_ms;
        let finisher_live = live.clone();
        let finisher_shared_summaries = shared_summaries.clone();
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
                        project_root.as_deref(),
                        role_worktrees.as_deref(),
                        &roster_snapshot,
                        &pool,
                        &dag,
                        sprint_tasks,
                        Some(&l1),
                        max_rework,
                        escalation_timeout_ms,
                        turn_timeout,
                        cumulative.clone(),
                        ts_tx.clone(),
                    )
                    .await
                    {
                        Ok(spawned) => {
                            *finisher_live.lock().expect("live handles mutex poisoned") = LiveHandles {
                                worker_aborts: spawned.worker_aborts.clone(),
                                lead_abort: spawned.lead_task.abort_handle(),
                                controls: spawned.controls,
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
                // These workers are gone the instant they're aborted (t-swap
                // plan D1/D6) — clear their control senders now rather than
                // leaving them until the next sprint's spawn overwrites
                // `finisher_live` wholesale, so a `swap_harness` call landing
                // in this boundary window sees "sender absent" (§D2b step 3
                // Ok fallback) instead of racing the aborted task's teardown.
                finisher_live
                    .lock()
                    .expect("live handles mutex poisoned")
                    .controls
                    .clear();

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
                finisher_shared_summaries
                    .lock()
                    .expect("summaries mutex poisoned")
                    .push(summary.text.clone());
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

            // Run has ended (plan D1: "런 종료(join/shutdown) 경로에서
            // abort") — signal the human proxy to stop *gracefully* (never
            // interrupting an in-flight `resolve_gate` publish, see
            // `human_proxy_loop`'s doc comment) and wait for it to actually
            // stop before `join()` can return, so a `resolve_gate` call
            // arriving right after `join()` deterministically observes a
            // closed `gate_tx` receiver rather than racing a live proxy.
            let _ = human_shutdown_tx.send(());
            let _ = human_task.await;
            // The human proxy's own disconnect is detected by the bus
            // server on a separate task (not synchronized with the
            // `human_task.await` above) — one more drain, mirroring every
            // sprint boundary's own drain after aborting that sprint's
            // workers, gives `subscription_loop` a chance to observe and
            // broadcast its `Unregistered` before `RunFinished` so the
            // latter stays the true last event.
            drain_subscription_backlog(&drain_tx).await;

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
            roster: roster_for_handle,
            summaries: summaries_for_handle,
            human_abort,
            gate_tx,
        })
    }
}

/// The single subscription loop (plan D3): consumes the bus's own
/// `BusEvent` broadcast, appends every ledger-recorded event to the ledger,
/// then — only after the append commits — broadcasts the corresponding
/// `RunEvent`. `EnvelopeAccepted` yields `Message` (seq = `messages` table
/// seq, read back via `messages_since`); every other `BusEvent` yields
/// `BusLifecycle` (seq = `append`'s return value, the `events` table seq —
/// contract §C3 seq-space rule, the two are never compared). Presence kind
/// envelopes (`presence.read`/`presence.typing`) are the one exception:
/// `handle_bus_event`'s guard intercepts them before either path, so they
/// never reach the ledger at all (t2-be-presence D3).
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
/// §C3), scoped to ledger-recorded envelopes only: a presence kind
/// (`presence.read`/`presence.typing`) envelope is intercepted by the guard
/// below and skips `ledger.append` entirely, broadcasting
/// `RunEvent::Presence` directly instead (t2-be-presence D3) — it is a
/// volatile signal, never a `messages`/`events` row. `EnvelopeAccepted`
/// yields `Message` (seq = `messages` table seq, read back via
/// `messages_since`); every other `BusEvent` yields `BusLifecycle` (seq =
/// `append`'s return value, the `events` table seq — a different space,
/// never compared to `Message.seq`).
fn handle_bus_event(
    ledger: &EventLedger,
    run_tx: &broadcast::Sender<RunEvent>,
    snapshot: &Mutex<SnapshotState>,
    event: BusLifecycleEvent,
) {
    // Presence guard (t2-be-presence D3): keyed on `envelope.kind` alone,
    // before `ledger.append` ever runs — even a presence envelope whose
    // body fails to parse (defensive; every presence envelope is
    // self-generated by crew-agent's own runner, so this shouldn't happen
    // in practice) still skips the ledger, it just broadcasts nothing.
    if let BusLifecycleEvent::EnvelopeAccepted { envelope } = &event {
        if matches!(envelope.kind, MessageKind::PresenceRead | MessageKind::PresenceTyping) {
            if let Some(presence) = presence_run_event(envelope) {
                let _ = run_tx.send(presence);
            }
            return;
        }
    }

    let events_seq = match ledger.append(&event) {
        Ok(seq) => seq,
        Err(err) => {
            tracing::error!(error = %err, "failed to append bus event to ledger");
            return;
        }
    };

    if let BusLifecycleEvent::EnvelopeAccepted { .. } = &event {
        // Locked for the whole read-query-update section, not just the
        // initial read (t-swap plan D4): `RunHandle::swap_harness` is a
        // second caller of this function alongside `subscription_loop`, so
        // two concurrent calls reading the same `prev_last_seq` before
        // either updates it could otherwise both see (and double-broadcast)
        // the same freshly-committed row.
        let mut snap = snapshot.lock().unwrap();
        let prev_last_seq = snap.last_seq;
        match ledger.messages_since(prev_last_seq) {
            Ok(messages) => {
                for message in messages {
                    snap.last_seq = message.seq;
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

/// Converts a presence-kind envelope into its `RunEvent::Presence`
/// (t2-be-presence D1/D3/D4) — `None` if the body doesn't parse as the
/// expected schema.
fn presence_run_event(envelope: &Envelope) -> Option<RunEvent> {
    match envelope.kind {
        MessageKind::PresenceRead => {
            let body = presence_read_from_body(&envelope.body)?;
            Some(RunEvent::Presence {
                agent_id: body.agent_id,
                kind: PresenceKindDto::Read,
                target_msg_id: Some(body.target_msg_id),
                active: None,
            })
        }
        MessageKind::PresenceTyping => {
            let body = presence_typing_from_body(&envelope.body)?;
            Some(RunEvent::Presence {
                agent_id: body.agent_id,
                kind: PresenceKindDto::Typing,
                target_msg_id: None,
                active: Some(body.active),
            })
        }
        _ => None,
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
    /// The same `Arc<Mutex<Roster>>` the sprint loop reads a snapshot of at
    /// every sprint boundary (t-swap plan D1) — `swap_harness` mutates it
    /// directly; the next `spawn_sprint` call picks up the change.
    roster: Arc<Mutex<Roster>>,
    /// The same `Arc<Mutex<Vec<String>>>` the finisher appends each sprint's
    /// summary text to (t-swap plan D7) — `swap_harness` step 3 reads a
    /// snapshot to assemble the swapped-in worker's injected context.
    summaries: Arc<Mutex<Vec<String>>>,
    /// Aborts the human proxy task (contracts-m7.md §E5) — the finisher
    /// already aborts+awaits it on natural run end (plan D1); this is
    /// `shutdown()`'s defensive counterpart for an in-progress run
    /// (aborting an already-finished task is a no-op).
    human_abort: AbortHandle,
    /// Sends `resolve_gate` requests to the human proxy task (plan D2) — a
    /// closed receiver (proxy ended) surfaces as `RunError::GateUnavailable`.
    gate_tx: mpsc::Sender<GateCmd>,
}

/// One control-channel swap attempt against a specific worker's sender —
/// factored out of `swap_harness` (t-swap plan D9 scenario ⑤) so the
/// "channel closed" failure path is unit-testable without racing a real
/// sprint boundary's task teardown. `harness`/`harness_id` mirror
/// `AgentControl::Swap`'s fields (contracts-m6.md §D1, verbatim, D8: the
/// already-built `Arc<dyn Harness>` from `swap_harness`'s validation step,
/// never re-`make`'d).
async fn send_swap_control(
    tx: &mpsc::Sender<AgentControl>,
    harness: Arc<dyn Harness>,
    harness_id: String,
    injected_context: String,
) -> Result<HandoffSnapshot, String> {
    let (ack_tx, ack_rx) = oneshot::channel();
    let ctrl = AgentControl::Swap {
        harness,
        harness_id,
        injected_context,
        ack: ack_tx,
    };
    if tx.send(ctrl).await.is_err() {
        return Err("worker control channel closed".to_string());
    }
    match tokio::time::timeout(Duration::from_millis(SWAP_ACK_TIMEOUT_MS), ack_rx).await {
        Ok(Ok(Ok(snapshot))) => Ok(snapshot),
        Ok(Ok(Err(harness_err))) => Err(harness_err.to_string()),
        Ok(Err(_recv_err)) => Err("worker dropped ack".to_string()),
        Err(_elapsed) => Err("ack timeout".to_string()),
    }
}

/// `RunHandle::snapshot()`'s `(last_seq, messages)` computation, factored
/// out for unit testing (t1-msg-race plan D2/D4-②) — exercised directly
/// against a bare `EventLedger` without needing a live `RunHandle`.
///
/// `last_seq` is the max seq among the `messages` actually returned (0 if
/// none) — never a value read from `SnapshotState` independently — so the
/// pair is atomic by construction: no interleaving with `handle_bus_event`
/// (which commits to the ledger *before* acquiring the snapshot lock to
/// update `last_seq`) can produce a `last_seq` that disagrees with
/// `messages`; that mismatch was the messages-0 race's root cause. A
/// `messages_since` error is logged, not silently swallowed, and yields the
/// safe empty pair (frontend D3: `lastSeq` 0 means "replay nothing, trust
/// the live buffer").
fn snapshot_messages_and_last_seq(ledger: &EventLedger) -> (i64, Vec<StoredMessage>) {
    let messages = match ledger.messages_since(0) {
        Ok(messages) => messages,
        Err(err) => {
            tracing::error!(error = %err, "snapshot: messages_since failed, returning empty");
            Vec::new()
        }
    };
    let last_seq = messages.iter().map(|m| m.seq).max().unwrap_or(0);
    (last_seq, messages)
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
    /// `messages`/`last_seq` from `snapshot_messages_and_last_seq` (t1-msg-race
    /// plan D2 — factored out so the pairing is unit-testable without a live
    /// `RunHandle`).
    pub fn snapshot(&self) -> RunSnapshot {
        let snap = self.snapshot.lock().unwrap();
        let (last_seq, messages) = snapshot_messages_and_last_seq(&self.ledger);
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
            last_seq,
            ts: now_ts(),
        }
    }

    /// Full-text search over this run's ledger (contracts-m7.md §E7
    /// verbatim, t-bridge3 plan D1) — delegates to `EventLedger::
    /// search_messages`; `LedgerError` converts via `RunError::Ledger`
    /// (`#[from]`).
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<StoredMessage>, RunError> {
        Ok(self.ledger.search_messages(query, limit)?)
    }

    /// Swaps the harness assigned to `agent_id`'s roster slot — contract
    /// §D2b (2026-08-29 보정판, t-swap plan), extending contracts-m5.md
    /// §C5c with immediate mid-sprint effectuation. Validates both
    /// arguments before touching anything: an unknown `agent_id` or a
    /// `harness` id `HarnessRegistry::make` can't build (`opencode`'s Stub
    /// adapter is accepted) returns `RunError::SwapRejected` with the
    /// roster untouched and zero events emitted.
    ///
    /// On success: steps 1-2 always run and their effects always stick —
    /// the roster slot's harness is updated immediately, a `handoff`
    /// envelope is appended straight to the ledger and broadcast as
    /// `Message` (skipped for the `lead` slot — plan D3, `crew_proto::Role`
    /// has no `Lead` variant), then `RosterChanged` broadcasts the updated
    /// roster. Step 3 then tries to reach the live worker directly: if
    /// `agent_id` has no live control-channel sender (not in this sprint,
    /// mid-boundary, or the lead slot, which is never registered — §D2a),
    /// that's `Ok(())` too, since the swap will still take effect at the
    /// next sprint boundary (M5's existing fallback path, plan D5/D6). Only
    /// a genuine step-3 failure (send failure, ack timeout, worker-dropped
    /// ack, or a harness error) returns `Err(RunError::SwapIncomplete)` —
    /// steps 1-2's effects are **not** rolled back when that happens; the
    /// same boundary fallback still applies.
    pub async fn swap_harness(&self, agent_id: &str, harness: &str) -> Result<(), RunError> {
        {
            let roster = self.roster.lock().expect("roster mutex poisoned");
            if !roster.agents.iter().any(|a| a.id == agent_id) {
                return Err(RunError::SwapRejected(format!("unknown agent id: {agent_id}")));
            }
        }
        // D8: keep the built `Arc<dyn Harness>` for step 3 — `make` is only
        // ever called once per swap.
        let harness_arc = match HarnessRegistry::make(harness) {
            Some(h) => h,
            None => return Err(RunError::SwapRejected(format!("unknown harness id: {harness}"))),
        };

        let (old_harness, role_str) = {
            let mut roster = self.roster.lock().expect("roster mutex poisoned");
            let slot = roster
                .agents
                .iter_mut()
                .find(|a| a.id == agent_id)
                .expect("agent_id presence was just checked above");
            let old_harness = std::mem::replace(&mut slot.harness, harness.to_string());
            (old_harness, slot.role.clone())
        };

        // Carried past this block for step 3 (below, after RosterChanged —
        // contracts-m6.md §D2b orders roster+envelope+RosterChanged strictly
        // before the control-channel attempt): `None` for the lead slot,
        // which has no `HandoffPack` role and is never in `controls` anyway
        // (§D2a), so step 3 is a no-op for it either way.
        let mut pack_for_control: Option<(HandoffPack, String)> = None;
        if let Some(role) = role_from_str(&role_str) {
            let (goal, done, in_flight) = {
                let snap = self.snapshot.lock().unwrap();
                let done = snap
                    .task_states
                    .iter()
                    .filter(|(_, state)| *state == TaskStateDto::Accepted)
                    .map(|(id, _)| id.clone())
                    .collect();
                let in_flight = snap
                    .task_states
                    .iter()
                    .filter(|(_, state)| *state == TaskStateDto::Assigned)
                    .map(|(id, _)| id.clone())
                    .collect();
                (snap.goal.clone(), done, in_flight)
            };
            let pack = HandoffPack {
                role,
                spec_ref: goal.clone(),
                done,
                in_flight,
                decisions: Vec::new(),
                open_questions: Vec::new(),
                notes: format!("harness swap: {old_harness} -> {harness}"),
            };
            let envelope = Envelope::new(
                // Literal duplicate of crew-lead::dispatch's private
                // `SPRINT_LABEL` (t-swap plan D4) — that crate is out of
                // scope for this task so the constant can't be imported;
                // unifying the two is a separate, scoped-out refactor.
                "sp-m3".to_string(),
                format!("th-swap-{agent_id}"),
                "agent:lead".to_string(),
                vec![agent_id.to_string()],
                MessageKind::Handoff,
                None,
                format!("swap-{}", uuid::Uuid::new_v4()),
                handoff_body(&pack),
                Vec::new(),
                false,
                900_000,
            );
            handle_bus_event(
                &self.ledger,
                &self.run_tx,
                &self.snapshot,
                BusLifecycleEvent::EnvelopeAccepted { envelope },
            );
            pack_for_control = Some((pack, goal));
        }

        let agents = roster_to_dto(&self.roster.lock().expect("roster mutex poisoned"));
        let _ = self.run_tx.send(RunEvent::RosterChanged { agents, ts: now_ts() });

        // Step 3 (§D2b, new): reach the live worker directly, if there is
        // one. `pack_for_control` is `None` for the lead slot only, which
        // is never in `controls` either, so this whole block is exercised
        // for every non-lead slot regardless of whether a sender is found.
        if let Some((pack, goal)) = pack_for_control {
            let sender = {
                let live = self.live.lock().expect("live handles mutex poisoned");
                live.controls.get(agent_id).cloned()
            };
            if let Some(tx) = sender {
                let l1 = {
                    let summaries_so_far = self.summaries.lock().expect("summaries mutex poisoned").clone();
                    l1_context(&goal, &summaries_so_far)
                };
                let pack_json = serde_json::to_string_pretty(&pack)
                    .unwrap_or_else(|e| format!("{{\"error\":\"pack serialize failed: {e}\"}}"));
                let injected_context = format!("{l1}\n\n[handoff]\n{pack_json}");
                if let Err(reason) =
                    send_swap_control(&tx, harness_arc, harness.to_string(), injected_context).await
                {
                    return Err(RunError::SwapIncomplete(reason));
                }
            }
        }

        Ok(())
    }

    /// Publishes a `human.response` envelope for `task_id` (contracts-m7.md
    /// §E5, §E2 verbatim) via the run-resident `agent:human` proxy.
    /// `Err(RunError::GateUnavailable)` once the run has ended (the proxy's
    /// receiver is closed) or the bus itself rejects the publish. Never
    /// errors on an unknown `task_id` — the envelope still publishes; it's
    /// `LeadBehavior::handle_human_response` (contract §E3) that silently
    /// ignores anything not currently `Escalated`.
    pub async fn resolve_gate(&self, task_id: &str, decision: GateDecision, reason: &str) -> Result<(), RunError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cmd = GateCmd {
            task_id: task_id.to_string(),
            decision,
            reason: reason.to_string(),
            reply: reply_tx,
        };
        self.gate_tx
            .send(cmd)
            .await
            .map_err(|_| RunError::GateUnavailable("run ended".to_string()))?;
        match reply_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(RunError::GateUnavailable(reason)),
            Err(_) => Err(RunError::GateUnavailable("run ended".to_string())),
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
        self.human_abort.abort();
        self.bus.shutdown().await;
    }
}

#[cfg(test)]
mod send_swap_control_tests {
    //! `send_swap_control` unit-level coverage (t-swap plan D9 scenario ⑤):
    //! the "control channel closed" failure path is exercised directly
    //! against a real (receiver-dropped) `mpsc::Sender` instead of racing a
    //! live sprint boundary's task teardown, per the plan's stated fallback
    //! when the integration-level race isn't reproducible deterministically
    //! — which the `LiveHandles.controls.clear()` fix (this task) made true
    //! by design, closing exactly that race.

    use super::*;

    fn fake_harness() -> Arc<dyn Harness> {
        Arc::new(crew_harness::claude::ClaudeCodeHarness::with_binary("/bin/false"))
    }

    /// Normal: a live receiver acks Ok and `send_swap_control` returns the
    /// snapshot untouched.
    #[tokio::test]
    async fn acks_ok_returns_the_snapshot() {
        let (tx, mut rx) = mpsc::channel::<AgentControl>(4);
        tokio::spawn(async move {
            if let Some(AgentControl::Swap { ack, harness_id, .. }) = rx.recv().await {
                let _ = ack.send(Ok(HandoffSnapshot {
                    harness: harness_id,
                    session_id: "s-1".to_string(),
                    notes: "ok".to_string(),
                }));
            }
        });

        let result = send_swap_control(&tx, fake_harness(), "claude-code".to_string(), "ctx".to_string()).await;

        let snapshot = result.expect("a live receiver's Ok ack must come back as Ok");
        assert_eq!(snapshot.session_id, "s-1");
    }

    /// Error: the worker's harness-level snapshot/shutdown itself failed —
    /// the `HarnessError`'s `Display` text must surface as the reason.
    #[tokio::test]
    async fn acks_harness_err_surfaces_its_display_text() {
        let (tx, mut rx) = mpsc::channel::<AgentControl>(4);
        tokio::spawn(async move {
            if let Some(AgentControl::Swap { ack, .. }) = rx.recv().await {
                let _ = ack.send(Err(crew_harness::HarnessError::ProcessExited));
            }
        });

        let result = send_swap_control(&tx, fake_harness(), "claude-code".to_string(), "ctx".to_string()).await;

        let reason = result.expect_err("a harness-level ack error must come back as Err");
        assert_eq!(reason, crew_harness::HarnessError::ProcessExited.to_string());
    }

    /// Boundary (D9 ⑤): the receiver is already gone (dropped, as it is the
    /// instant a sprint's worker is aborted) — `send` itself fails, with no
    /// wait for the `SWAP_ACK_TIMEOUT_MS` budget.
    #[tokio::test]
    async fn closed_channel_fails_fast_without_waiting_for_the_ack_timeout() {
        let (tx, rx) = mpsc::channel::<AgentControl>(4);
        drop(rx);

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            send_swap_control(&tx, fake_harness(), "claude-code".to_string(), "ctx".to_string()),
        )
        .await
        .expect("a closed channel must fail immediately, not wait out the ack timeout");

        assert_eq!(result, Err("worker control channel closed".to_string()));
    }
}

#[cfg(test)]
mod live_controls_wiring_tests {
    //! White-box coverage (test-quality-auditor finding on this task): the
    //! integration-level "mid-sprint swap reaches the live worker" test in
    //! tests/m5_swap.rs cannot, on its own, tell "found a live sender in
    //! `LiveHandles.controls` and completed a real channel round-trip" apart
    //! from "found no sender and took the §D2b/D6 sender-absent Ok
    //! fallback" — `ScriptedCrewMember` never overrides `on_control`, so its
    //! default no-op ack (role.rs) produces zero externally observable
    //! difference between the two paths. These tests reach into
    //! `RunHandle`'s private `live` field (same module tree — a `crew-run`-
    //! internal white-box test, still within this task's scope) to pin the
    //! two structural facts an event-stream-only test can't: (a) every
    //! non-lead role is actually registered, and (b) `swap_harness`
    //! genuinely *uses* whatever sender is registered rather than always
    //! taking the Ok path regardless of what's in the map.

    use super::*;
    use crew_lead::accept::AcceptanceLoop;

    fn scripted_config(data_dir: std::path::PathBuf) -> RunConfig {
        RunConfig {
            goal: "간단한 랜딩 페이지".to_string(),
            mode: RunMode::Scripted { planted_violations: vec![] },
            data_dir,
            max_rework: AcceptanceLoop::default_budget(),
            max_per_sprint: 0,
            escalation_timeout_ms: 0,
            roster: None,
            dev_cmd_checks: Vec::new(),
            project_root: None,
            turn_timeout_secs: 900,
        }
    }

    fn test_data_dir(label: &str) -> std::path::PathBuf {
        std::env::current_dir()
            .unwrap()
            .join(".crew-test")
            .join(format!("{label}-{}", uuid::Uuid::new_v4()))
    }

    /// Default (D5, contracts-m10.md §H1h.3): the existing helper's
    /// `RunConfig` carries no cmd DoD checks unless a test opts in.
    #[test]
    fn scripted_config_default_carries_no_dev_cmd_checks() {
        let cfg = scripted_config(test_data_dir("scripted-config-default"));
        assert!(cfg.dev_cmd_checks.is_empty());
    }

    /// (a) Wiring: every non-lead role is registered the instant the first
    /// sprint spawns, lead is never registered (§D2a) — guards exactly the
    /// regression class the auditor named: `spawn_sprint` reverting to
    /// `AgentRunner::run` for a role, or an `agent_id` key mismatch between
    /// `roles_all()` and the roster.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_non_lead_role_is_registered_in_live_controls_lead_is_not() {
        let data_dir = test_data_dir("live-controls-wiring");
        let handle = RunController::start(scripted_config(data_dir.clone()))
            .await
            .expect("start must succeed");

        let registered: std::collections::HashSet<String> = {
            let live = handle.live.lock().expect("live handles mutex poisoned");
            live.controls.keys().cloned().collect()
        };

        for id in ["agent:pm", "agent:designer", "agent:publisher", "agent:developer", "agent:qa"] {
            assert!(registered.contains(id), "{id} must be registered in live controls: {registered:?}");
        }
        assert!(
            !registered.contains("agent:lead"),
            "the lead slot must never be registered (§D2a): {registered:?}"
        );

        handle.shutdown().await;
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// (b) Genuine use, not a coincidental fallback: with a sender
    /// *present* in `controls` but its receiver already dropped (simulating
    /// a stale registration — distinct from the D6 "absent" case, where the
    /// key itself is missing), `swap_harness` must still attempt the send
    /// and surface the resulting failure as `SwapIncomplete`. If
    /// `swap_harness` ever stopped reading `controls` at all (e.g. a future
    /// edit accidentally always took the D6 fallback), this test would
    /// wrongly see `Ok(())` and fail.
    #[tokio::test(flavor = "multi_thread")]
    async fn swap_harness_uses_a_present_sender_and_reports_incomplete_when_it_is_dead() {
        let data_dir = test_data_dir("live-controls-dead-sender");
        let handle = RunController::start(scripted_config(data_dir.clone()))
            .await
            .expect("start must succeed");

        {
            let (dead_tx, dead_rx) = mpsc::channel::<AgentControl>(4);
            drop(dead_rx);
            let mut live = handle.live.lock().expect("live handles mutex poisoned");
            live.controls.insert("agent:designer".to_string(), dead_tx);
        }

        let result = handle.swap_harness("agent:designer", "opencode").await;
        assert!(
            matches!(result, Err(RunError::SwapIncomplete(_))),
            "a present-but-dead sender must produce SwapIncomplete, not the sender-absent Ok fallback: {result:?}"
        );

        handle.shutdown().await;
        let _ = std::fs::remove_dir_all(&data_dir);
    }
}

#[cfg(test)]
mod role_cli_cwd_tests {
    //! Deterministic regression coverage for the ENOENT bug the coordinator's
    //! real-CLI spot check found (t-swap follow-up fix): `spawn_sprint`'s
    //! `RealCli` arm builds `HarnessAgentCfg { cwd: ... }` from a directory
    //! nothing ever created, so `Command::current_dir` failed every worker's
    //! CLI spawn with ENOENT. `role_cli_cwd` is a pure filesystem seam — no
    //! bus/tokio-task/real-CLI-binary needed — so this is a real, fast,
    //! deterministic test rather than relying solely on the `#[ignore]`
    //! real-CLI spot check (which also now exercises this fix, since it
    //! needs the directory to exist to get past spawn at all).

    use super::*;

    fn test_data_dir(label: &str) -> std::path::PathBuf {
        std::env::current_dir()
            .unwrap()
            .join(".crew-test")
            .join(format!("{label}-{}", uuid::Uuid::new_v4()))
    }

    /// Normal: the directory doesn't exist beforehand (mirrors a fresh run's
    /// `data_dir`) — `role_cli_cwd` must create it and return the same path
    /// `Command::current_dir` would need.
    #[test]
    fn creates_the_role_directory_when_it_does_not_exist() {
        let data_dir = test_data_dir("cli-cwd-fresh");
        assert!(!data_dir.exists(), "test setup: data_dir must not pre-exist");

        let cwd = role_cli_cwd(None, &data_dir, Role::Designer);

        assert_eq!(cwd, data_dir.join("cli-cwd").join("designer"));
        assert!(cwd.is_dir(), "the role cwd must exist as a directory after role_cli_cwd: {cwd:?}");

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// Boundary: calling it again for the same role (e.g. a second sprint's
    /// `spawn_sprint`) against an already-existing directory must not error
    /// or disturb its contents — `create_dir_all` is idempotent, but this
    /// pins that `role_cli_cwd` doesn't wrap it in anything that isn't.
    #[test]
    fn is_idempotent_and_preserves_existing_contents() {
        let data_dir = test_data_dir("cli-cwd-idempotent");
        let cwd = role_cli_cwd(None, &data_dir, Role::Qa);
        std::fs::write(cwd.join("marker.txt"), b"sprint-0").expect("must be able to write into the created cwd");

        let cwd_again = role_cli_cwd(None, &data_dir, Role::Qa);

        assert_eq!(cwd, cwd_again);
        assert!(cwd_again.is_dir(), "the directory must still exist: {cwd_again:?}");
        let marker = std::fs::read_to_string(cwd_again.join("marker.txt")).expect("earlier sprint's file must survive a repeat call");
        assert_eq!(marker, "sprint-0");

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// Error path: `create_dir_all` cannot create a directory *through* a
    /// path component that is itself a regular file — `role_cli_cwd`'s
    /// best-effort `let _ =` must not panic, and must return the intended
    /// path regardless (the caller then hits the same pre-fix ENOENT from
    /// the CLI spawn, not a panic from this helper — the whole point of
    /// "best-effort" is that this failure surfaces at the harness spawn,
    /// which already has real error handling, rather than here).
    #[test]
    fn does_not_panic_when_a_path_component_is_a_regular_file() {
        let data_dir = test_data_dir("cli-cwd-blocked");
        std::fs::create_dir_all(&data_dir).expect("test setup");
        std::fs::write(data_dir.join("cli-cwd"), b"not a directory").expect("test setup: block the cli-cwd path segment");

        let cwd = role_cli_cwd(None, &data_dir, Role::Publisher);

        assert_eq!(cwd, data_dir.join("cli-cwd").join("publisher"));
        assert!(!cwd.is_dir(), "creation must have failed silently (best-effort), not been magically satisfied: {cwd:?}");

        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// Normal (post-pitfall-30-fix): with a precomputed `role_worktrees`
    /// cache, `role_cli_cwd` is a pure lookup — it returns that role's
    /// worktree path verbatim and never touches `data_dir` at all (unlike
    /// the `None` branch above).
    #[test]
    fn some_role_worktrees_looks_up_the_precomputed_path_for_the_role() {
        let developer_worktree = test_data_dir("cli-cwd-worktree-developer");
        let role_worktrees = vec![(Role::Developer, developer_worktree.clone())];
        let data_dir = test_data_dir("cli-cwd-worktrees-data-dir");

        let cwd = role_cli_cwd(Some(&role_worktrees), &data_dir, Role::Developer);

        assert_eq!(cwd, developer_worktree, "must return the precomputed worktree path verbatim");
        assert!(!data_dir.exists(), "data_dir must be untouched when role_worktrees is Some");
    }

    /// M11 §I1 core contract, post-pitfall-30-fix: when `project_root` is
    /// `Some`, the agent CLI cwd (`spawn_sprint`'s `HarnessAgentCfg.cwd`
    /// call site) and the Cmd DoD exec cwd (`spawn_sprint`'s `with_cmd_exec`
    /// closure call site) resolve to the **same** path per role — asserted
    /// here against two independent (deliberately different) `data_dir`
    /// bases, proving the equality holds because of `role_worktrees`, not
    /// because the two call sites happen to share a `data_dir` — and
    /// different roles must resolve to **different** paths (the actual bug
    /// pitfall 30 was: `project_root` returned verbatim regardless of role).
    #[test]
    fn some_role_worktrees_makes_both_call_sites_agree_per_role_and_differ_across_roles() {
        let role_worktrees: Vec<(Role, std::path::PathBuf)> =
            [Role::Pm, Role::Designer, Role::Publisher, Role::Developer, Role::Qa]
                .into_iter()
                .map(|role| (role, test_data_dir(&format!("cli-cwd-worktree-{role:?}"))))
                .collect();
        let agent_cli_data_dir = test_data_dir("cli-cwd-combined-agent-base");
        let cmd_dod_data_dir = test_data_dir("cli-cwd-combined-dod-base");
        assert_ne!(
            agent_cli_data_dir, cmd_dod_data_dir,
            "test setup: the two data_dir bases must differ so equality below proves the cache, not a shared base"
        );

        let mut seen_cwds: Vec<std::path::PathBuf> = Vec::new();
        for role in [Role::Pm, Role::Designer, Role::Publisher, Role::Developer, Role::Qa] {
            let agent_cli_cwd = role_cli_cwd(Some(&role_worktrees), &agent_cli_data_dir, role);
            let cmd_dod_cwd = role_cli_cwd(Some(&role_worktrees), &cmd_dod_data_dir, role);

            assert_eq!(
                agent_cli_cwd, cmd_dod_cwd,
                "agent CLI cwd and Cmd DoD exec cwd must be the same path for role {role:?}"
            );
            assert!(
                !seen_cwds.contains(&agent_cli_cwd),
                "role {role:?} must not share a cwd with an earlier role (pitfall 30): {seen_cwds:?}"
            );
            seen_cwds.push(agent_cli_cwd);
        }
    }

    /// Boundary (invariant guard): `role_cli_cwd` panics rather than
    /// silently falling back if asked for a role missing from the
    /// precomputed cache — this should never happen in practice
    /// (`resolve_role_worktrees` always populates every `ROLE_ORDER` entry),
    /// so a panic surfaces the bug immediately instead of masking it as a
    /// wrong-but-plausible cwd.
    #[test]
    #[should_panic(expected = "no precomputed worktree for role")]
    fn some_role_worktrees_panics_on_a_missing_role_entry() {
        let role_worktrees = vec![(Role::Pm, test_data_dir("cli-cwd-only-pm"))];
        let data_dir = test_data_dir("cli-cwd-missing-role-data-dir");

        let _ = role_cli_cwd(Some(&role_worktrees), &data_dir, Role::Qa);
    }
}

#[cfg(test)]
mod role_system_hint_tests {
    //! Design D4: the artifacts-sharing convention paragraph is injected
    //! into the RealCli spawn spec only when `project_root` is `Some`, and
    //! carries the exact path + no-concurrent-edit + no-hook-addition
    //! content the plan specifies.

    use super::*;

    /// Normal: `project_root: None` — the hint is just the role/goal line,
    /// no artifacts paragraph.
    #[test]
    fn none_project_root_has_no_artifacts_paragraph() {
        let hint = role_system_hint(Role::Developer, "goal", None);
        assert_eq!(hint, "You are the Developer of a crew building: goal");
    }

    /// Normal/D4: `project_root: Some` injects the absolute path plus the
    /// no-concurrent-edit and no-hook-addition clauses.
    #[test]
    fn some_project_root_injects_the_artifacts_convention() {
        let root = std::path::Path::new("/tmp-like/does/not/need/to/exist/for/this/pure/test");
        let hint = role_system_hint(Role::Qa, "goal", Some(root));

        assert!(hint.starts_with("You are the Qa of a crew building: goal"));
        assert!(
            hint.contains(&format!("{}/.crew/artifacts", root.display())),
            "must include the absolute artifacts path: {hint}"
        );
        assert!(hint.contains("동시에 편집하지"), "must include the no-concurrent-edit clause: {hint}");
        assert!(hint.contains("훅을 추가하지"), "must include the no-hook-addition clause: {hint}");
    }
}

#[cfg(test)]
mod resolve_role_worktrees_tests {
    //! `resolve_role_worktrees` (plan D1-D3): the `RunController::start`
    //! precompute step, tested directly with an injected tempdir
    //! `worktrees_base` — production always derives this from
    //! `worktree::project_worktrees_base` (real `$HOME`), but
    //! tools_guidance forbids touching `$HOME` from a test.

    use super::*;
    use std::process::Command;

    fn test_dir(label: &str) -> std::path::PathBuf {
        std::env::current_dir()
            .unwrap()
            .join(".crew-test")
            .join(format!("{label}-{}", uuid::Uuid::new_v4()))
    }

    fn run_git_ok(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git").arg("-C").arg(cwd).args(args).status().expect("git must be on PATH");
        assert!(status.success(), "test setup: `git {args:?}` failed in {cwd:?}");
    }

    fn init_test_repo(label: &str) -> std::path::PathBuf {
        let root = test_dir(label);
        std::fs::create_dir_all(&root).expect("test setup: repo root");
        run_git_ok(&root, &["init", "-q"]);
        run_git_ok(&root, &["config", "user.email", "test@example.com"]);
        run_git_ok(&root, &["config", "user.name", "Test"]);
        std::fs::write(root.join("README.md"), b"init").expect("test setup: seed file");
        run_git_ok(&root, &["add", "."]);
        run_git_ok(&root, &["commit", "-q", "-m", "init"]);
        root
    }

    /// Normal: every `ROLE_ORDER` role gets a distinct worktree path.
    #[test]
    fn populates_every_canonical_role_with_a_distinct_path() {
        let repo = init_test_repo("resolve-worktrees-repo");
        let base = test_dir("resolve-worktrees-base");

        let worktrees = resolve_role_worktrees(&repo, &base).expect("resolution must succeed for a real git repo");

        assert_eq!(worktrees.len(), ROLE_ORDER.len(), "every canonical role must be populated");
        for role in ROLE_ORDER {
            assert!(worktrees.iter().any(|(r, _)| *r == role), "role {role:?} must be present: {worktrees:?}");
        }
        let mut paths: Vec<_> = worktrees.iter().map(|(_, p)| p.clone()).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), worktrees.len(), "every role's path must be distinct: {worktrees:?}");

        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Error: a non-git project_root fails the whole resolution (design
    /// D5) — no partial cache.
    #[test]
    fn errors_and_returns_nothing_for_a_non_git_project_root() {
        let non_repo = test_dir("resolve-worktrees-non-git");
        std::fs::create_dir_all(&non_repo).expect("test setup: plain (non-git) directory");
        let base = test_dir("resolve-worktrees-non-git-base");

        let result = resolve_role_worktrees(&non_repo, &base);

        assert!(result.is_err(), "a non-git project_root must fail resolution");

        let _ = std::fs::remove_dir_all(&non_repo);
    }
}

#[cfg(test)]
mod snapshot_atomicity_tests {
    //! t1-msg-race plan D4-②: `snapshot_messages_and_last_seq`'s
    //! `last_seq == max(returned messages' seq)` invariant, including under
    //! the exact write interleave (`ledger.append` durable before the
    //! snapshot lock's `last_seq` update, `handle_bus_event` at
    //! `controller.rs:899-919`) that produces the messages-0 race.

    use super::*;
    use std::sync::Barrier;
    use std::thread;

    fn test_envelope(n: u32) -> Envelope {
        Envelope::new(
            "sp-1".to_string(),
            "t-pm".to_string(),
            "agent:pm".to_string(),
            vec!["lead".to_string()],
            MessageKind::TaskResult,
            None,
            format!("corr-t-pm-{n}"),
            serde_json::json!({}),
            Vec::new(),
            false,
            60_000,
        )
    }

    fn empty_snapshot_state() -> SnapshotState {
        SnapshotState {
            goal: "goal".to_string(),
            spec: None,
            dag: None,
            sprint: Vec::new(),
            task_states: Vec::new(),
            last_seq: 0,
        }
    }

    /// A fresh path under `.crew-test/` (repo convention, never `/tmp`) for a
    /// file-backed ledger — needed only by the error-path test below, which
    /// requires a second connection onto the *same* on-disk database
    /// (`EventLedger::open_in_memory()` databases are private per-connection
    /// and can't be reached from a second connection).
    fn test_ledger_path(label: &str) -> std::path::PathBuf {
        let dir = std::env::current_dir().unwrap().join(".crew-test");
        std::fs::create_dir_all(&dir).expect("test setup: .crew-test dir");
        dir.join(format!("{label}-{}.sqlite3", uuid::Uuid::new_v4()))
    }

    /// Boundary: nothing ever committed -> last_seq 0, messages empty.
    #[test]
    fn empty_ledger_yields_last_seq_zero_and_no_messages() {
        let ledger = EventLedger::open_in_memory().expect("in-memory ledger");
        let (last_seq, messages) = snapshot_messages_and_last_seq(&ledger);
        assert_eq!(last_seq, 0);
        assert!(messages.is_empty());
    }

    /// Error (t1-msg-race r2 review F1): `messages_since` returning `Err`
    /// must be logged and yield the safe empty pair `(0, [])` — never a
    /// fallback that resurrects the old bug (an independent, possibly-stale
    /// `last_seq`). Forced deterministically, no sleep/flakiness: a second
    /// raw connection onto the same file-backed ledger drops the `messages`
    /// table out from under the ledger's own connection, so its next
    /// `messages_since` call is guaranteed to fail with "no such table".
    #[test]
    fn messages_since_error_yields_safe_empty_pair_not_a_stale_last_seq_fallback() {
        let path = test_ledger_path("snapshot-error-path");
        let ledger = EventLedger::open(&path).expect("file-backed ledger");
        ledger
            .append(&BusLifecycleEvent::EnvelopeAccepted { envelope: test_envelope(1) })
            .expect("append");

        {
            let raw = rusqlite::Connection::open(&path).expect("second raw connection to the same file");
            raw.execute("DROP TABLE messages", []).expect("drop messages table");
        }

        let (last_seq, messages) = snapshot_messages_and_last_seq(&ledger);
        assert_eq!(
            last_seq, 0,
            "an Err from messages_since must yield last_seq 0, never a value independent of messages"
        );
        assert!(messages.is_empty());

        let _ = std::fs::remove_file(&path);
    }

    /// Normal: one committed message -> the pair matches it.
    #[test]
    fn single_committed_message_yields_matching_last_seq() {
        let ledger = EventLedger::open_in_memory().expect("in-memory ledger");
        ledger
            .append(&BusLifecycleEvent::EnvelopeAccepted { envelope: test_envelope(1) })
            .expect("append");
        let (last_seq, messages) = snapshot_messages_and_last_seq(&ledger);
        assert_eq!(last_seq, 1);
        assert_eq!(messages.iter().map(|m| m.seq).max(), Some(1));
    }

    /// The invariant test (D4-②): two barriers (no sleep) force the exact
    /// interleave where a ledger commit lands before `SnapshotState.
    /// last_seq`'s update — mirroring `handle_bus_event`. A snapshot read
    /// landing in that window must still return a self-consistent pair.
    #[test]
    fn last_seq_matches_returned_messages_max_seq_even_when_read_between_ledger_commit_and_snap_state_update() {
        let ledger = Arc::new(EventLedger::open_in_memory().expect("in-memory ledger"));
        let snap = Arc::new(Mutex::new(empty_snapshot_state()));
        let after_append = Arc::new(Barrier::new(2));
        let after_read = Arc::new(Barrier::new(2));

        let writer = {
            let ledger = Arc::clone(&ledger);
            let snap = Arc::clone(&snap);
            let after_append = Arc::clone(&after_append);
            let after_read = Arc::clone(&after_read);
            thread::spawn(move || {
                // Mirrors handle_bus_event (controller.rs:899): the ledger
                // commit happens before the snapshot lock is ever touched.
                ledger
                    .append(&BusLifecycleEvent::EnvelopeAccepted { envelope: test_envelope(1) })
                    .expect("append");
                after_append.wait();
                // Hold off updating snap.last_seq until the reader has taken
                // its snapshot in the exact stale window.
                after_read.wait();
                snap.lock().unwrap().last_seq = 1;
            })
        };

        after_append.wait();
        // Confirms the interleave actually landed in the stale window (the
        // writer hasn't updated snap.last_seq yet) before trusting the
        // invariant check below — otherwise a broken barrier setup could
        // pass vacuously.
        let stale_last_seq = snap.lock().unwrap().last_seq;
        assert_eq!(stale_last_seq, 0, "test setup: must read snap.last_seq before the writer updates it");
        let (last_seq, messages) = snapshot_messages_and_last_seq(&ledger);
        after_read.wait();
        writer.join().expect("writer thread must not panic");

        let max_seq = messages.iter().map(|m| m.seq).max().unwrap_or(0);
        assert_eq!(
            last_seq, max_seq,
            "snapshot's last_seq must equal the max seq among the messages it actually returned, \
             regardless of how SnapshotState.last_seq's own update happened to be timed"
        );
    }
}

#[cfg(test)]
mod presence_guard_tests {
    //! t2-be-presence D3: `handle_bus_event`'s presence guard — a presence
    //! kind envelope is intercepted before `ledger.append` and only ever
    //! reaches `run_tx` as `RunEvent::Presence`, never the ledger.

    use super::*;
    use crew_proto::{presence_read_body, presence_typing_body, PresenceReadBody, PresenceTypingBody};

    fn presence_read_envelope() -> Envelope {
        Envelope::new(
            "sp-1".to_string(),
            "t-pm".to_string(),
            "agent:developer".to_string(),
            vec!["agent:lead".to_string()],
            MessageKind::PresenceRead,
            None,
            "corr-presence-1".to_string(),
            presence_read_body(&PresenceReadBody {
                agent_id: "agent:developer".to_string(),
                target_msg_id: "msg_1".to_string(),
            }),
            Vec::new(),
            false,
            900_000,
        )
    }

    fn presence_typing_envelope(active: bool) -> Envelope {
        Envelope::new(
            "sp-1".to_string(),
            "t-pm".to_string(),
            "agent:developer".to_string(),
            vec!["agent:lead".to_string()],
            MessageKind::PresenceTyping,
            None,
            "corr-presence-2".to_string(),
            presence_typing_body(&PresenceTypingBody {
                agent_id: "agent:developer".to_string(),
                active,
            }),
            Vec::new(),
            false,
            900_000,
        )
    }

    fn empty_snapshot_state() -> SnapshotState {
        SnapshotState {
            goal: "goal".to_string(),
            spec: None,
            dag: None,
            sprint: Vec::new(),
            task_states: Vec::new(),
            last_seq: 0,
        }
    }

    /// Normal (R1): a presence.read envelope broadcasts `RunEvent::Presence`
    /// and never touches the ledger (no `events`/`messages` row).
    #[test]
    fn presence_read_broadcasts_and_skips_ledger() {
        let ledger = EventLedger::open_in_memory().expect("in-memory ledger");
        let (run_tx, mut run_rx) = broadcast::channel(8);
        let snapshot = Mutex::new(empty_snapshot_state());

        handle_bus_event(
            &ledger,
            &run_tx,
            &snapshot,
            BusLifecycleEvent::EnvelopeAccepted { envelope: presence_read_envelope() },
        );

        let event = run_rx.try_recv().expect("RunEvent::Presence must be broadcast");
        match event {
            RunEvent::Presence { agent_id, kind, target_msg_id, active } => {
                assert_eq!(agent_id, "agent:developer");
                assert_eq!(kind, PresenceKindDto::Read);
                assert_eq!(target_msg_id, Some("msg_1".to_string()));
                assert_eq!(active, None);
            }
            other => panic!("expected Presence, got {other:?}"),
        }
        assert!(run_rx.try_recv().is_err(), "exactly one RunEvent must be broadcast");

        let (last_seq, messages) = snapshot_messages_and_last_seq(&ledger);
        assert_eq!(last_seq, 0, "presence must not be ledgered");
        assert!(messages.is_empty(), "presence must not appear in ledger messages (R3)");
    }

    /// Normal (R2): typing(true) then typing(false), each broadcast with
    /// the right `active` value, still unledgered.
    #[test]
    fn presence_typing_true_then_false_broadcast_in_order() {
        let ledger = EventLedger::open_in_memory().expect("in-memory ledger");
        let (run_tx, mut run_rx) = broadcast::channel(8);
        let snapshot = Mutex::new(empty_snapshot_state());

        handle_bus_event(
            &ledger,
            &run_tx,
            &snapshot,
            BusLifecycleEvent::EnvelopeAccepted { envelope: presence_typing_envelope(true) },
        );
        handle_bus_event(
            &ledger,
            &run_tx,
            &snapshot,
            BusLifecycleEvent::EnvelopeAccepted { envelope: presence_typing_envelope(false) },
        );

        let first = run_rx.try_recv().expect("typing(true) must be broadcast");
        let second = run_rx.try_recv().expect("typing(false) must be broadcast");
        match (first, second) {
            (RunEvent::Presence { active: Some(true), kind: PresenceKindDto::Typing, .. },
             RunEvent::Presence { active: Some(false), kind: PresenceKindDto::Typing, .. }) => {}
            other => panic!("expected typing(true) then typing(false), got {other:?}"),
        }

        let (last_seq, _) = snapshot_messages_and_last_seq(&ledger);
        assert_eq!(last_seq, 0, "presence must not be ledgered");
    }

    /// Boundary: a presence-kind envelope whose body fails to parse still
    /// skips the ledger (the guard keys on `kind` alone, not on
    /// `presence_run_event` succeeding) — it just broadcasts nothing.
    #[test]
    fn malformed_presence_body_skips_ledger_without_broadcasting() {
        let ledger = EventLedger::open_in_memory().expect("in-memory ledger");
        let (run_tx, mut run_rx) = broadcast::channel(8);
        let snapshot = Mutex::new(empty_snapshot_state());

        let mut envelope = presence_read_envelope();
        envelope.body = serde_json::json!({"not": "a valid presence body"});

        handle_bus_event(&ledger, &run_tx, &snapshot, BusLifecycleEvent::EnvelopeAccepted { envelope });

        assert!(run_rx.try_recv().is_err(), "malformed body must not broadcast a RunEvent");
        let (last_seq, messages) = snapshot_messages_and_last_seq(&ledger);
        assert_eq!(last_seq, 0);
        assert!(messages.is_empty());
    }

    /// Error/refusal (boundary counterpart): a non-presence kind is
    /// unaffected by the guard — still ledgered and still broadcasts
    /// `RunEvent::Message`, exactly as before t2-be-presence.
    #[test]
    fn non_presence_kind_is_still_ledgered_and_broadcasts_message() {
        let ledger = EventLedger::open_in_memory().expect("in-memory ledger");
        let (run_tx, mut run_rx) = broadcast::channel(8);
        let snapshot = Mutex::new(empty_snapshot_state());

        let envelope = Envelope::new(
            "sp-1".to_string(),
            "t-pm".to_string(),
            "agent:pm".to_string(),
            vec!["agent:lead".to_string()],
            MessageKind::TaskResult,
            None,
            "corr-normal-1".to_string(),
            serde_json::json!({}),
            Vec::new(),
            false,
            60_000,
        );

        handle_bus_event(
            &ledger,
            &run_tx,
            &snapshot,
            BusLifecycleEvent::EnvelopeAccepted { envelope: envelope.clone() },
        );

        let event = run_rx.try_recv().expect("Message must be broadcast");
        match event {
            RunEvent::Message { envelope: got, .. } => assert_eq!(got.id, envelope.id),
            other => panic!("expected Message, got {other:?}"),
        }
        let (last_seq, messages) = snapshot_messages_and_last_seq(&ledger);
        assert_eq!(last_seq, 1, "non-presence kinds must still be ledgered");
        assert_eq!(messages.len(), 1);
    }
}
