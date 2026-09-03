use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use crew_harness::{
    AgentCfg, HandoffSnapshot, Harness, HarnessEvent, Session, TurnOutcome, UserTurn,
    DEFAULT_TURN_TIMEOUT,
};
use crew_proto::{Envelope, MessageKind};
use crew_proto::{DodCheck, TaskSpec};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::control::AgentControl;
use crate::role::RoleBehavior;

const DEADLINE_MS: u64 = 900_000;

/// claude-backed Designer role (plan A8): turns a received envelope into a
/// harness turn and maps the model's JSON reply back onto the M2 body
/// contract (A6). `Session.events_rx` can only be taken once
/// (crew-harness/src/lib.rs `take_events` panics on a second call), so the
/// events stream is captured once per spawned session (the
/// crates/crew-harness/tests/adapter.rs:28-77 pattern this decision cites)
/// and drained fresh for every turn after that.
pub struct DesignerHarnessBehavior {
    harness: Arc<dyn Harness>,
    cfg: AgentCfg,
    system_hint: String,
    session: Option<Session>,
    events: Option<mpsc::Receiver<HarnessEvent>>,
}

impl DesignerHarnessBehavior {
    pub fn new(harness: Arc<dyn Harness>, cfg: AgentCfg, system_hint: String) -> Self {
        Self {
            harness,
            cfg,
            system_hint,
            session: None,
            events: None,
        }
    }

    async fn ensure_session(&mut self) -> Result<(), crew_harness::HarnessError> {
        if self.session.is_none() {
            let mut session = self.harness.spawn(&self.cfg).await?;
            let events = self.harness.take_events(&mut session);
            self.session = Some(session);
            self.events = Some(events);
        }
        Ok(())
    }

    /// Drops the dead session after a failed turn (shutting it down) so the
    /// next envelope's `ensure_session` respawns a fresh CLI process instead
    /// of writing to a dead pipe (M13 turn-recovery fix, D1).
    async fn discard_session(&mut self) {
        if let Some(s) = self.session.take() {
            let _ = self.harness.shutdown(s).await;
        }
        self.events = None;
    }

    fn build_prompt(&self, env: &Envelope) -> String {
        let task = match env.kind {
            MessageKind::TaskAssign => {
                let req_ids = env.body.get("req_ids").cloned().unwrap_or(json!([]));
                let brief = env.body.get("brief").and_then(Value::as_str).unwrap_or("");
                format!("Task assignment. req_ids={req_ids}. brief: {brief}")
            }
            MessageKind::ChangeRequest => {
                let violations = env.body.get("violations").cloned().unwrap_or(json!([]));
                let reason = env.body.get("reason").and_then(Value::as_str).unwrap_or("");
                format!("Change request. violations={violations}. reason: {reason}")
            }
            _ => String::new(),
        };
        format!(
            "{}\n\n{}\n\nreply ONLY with JSON {{\"covered_req_ids\": [...], \"artifact\": \"...\"}}",
            self.system_hint, task
        )
    }

    fn reply(&self, in_reply_to: &Envelope, kind: MessageKind, body: Value) -> Envelope {
        let from = in_reply_to.to.first().cloned().unwrap_or_default();
        Envelope::new(
            in_reply_to.sprint.clone(),
            in_reply_to.thread.clone(),
            from,
            vec![in_reply_to.from.clone()],
            kind,
            Some(in_reply_to.id.clone()),
            in_reply_to.corr.clone(),
            body,
            vec![],
            true,
            DEADLINE_MS,
        )
    }

    fn ack_and_result(&self, in_reply_to: &Envelope, result_body: Value) -> Vec<Envelope> {
        let ack = self.reply(in_reply_to, MessageKind::TaskAck, json!({}));
        let result = self.reply(in_reply_to, MessageKind::TaskResult, result_body);
        vec![ack, result]
    }

    fn blocked(&self, in_reply_to: &Envelope, reason: String) -> Vec<Envelope> {
        vec![self.reply(in_reply_to, MessageKind::Blocked, json!({"reason": reason}))]
    }
}

/// Drains `events` until the turn's terminal event (or channel close),
/// accumulating `Text` deltas. Must run *concurrently* with the `send()`
/// call that produces these events, not after it: crew-harness's reader
/// task pushes each event via a bounded `events_tx.send().await`
/// (capacity 64, crates/crew-harness/src/claude.rs `EVENTS_CHANNEL_CAPACITY`)
/// and only reaches the turn's result line — which unblocks `send()` — once
/// every preceding event has been sent. A turn producing more than 64
/// events before its result line deadlocks the reader (and so `send()`
/// itself) if nothing is draining this receiver in the meantime.
async fn drain_until_terminal(mut events: mpsc::Receiver<HarnessEvent>) -> (mpsc::Receiver<HarnessEvent>, String) {
    let mut text = String::new();
    loop {
        match events.recv().await {
            Some(HarnessEvent::Text { delta }) => text.push_str(&delta),
            Some(HarnessEvent::Finished { .. }) | Some(HarnessEvent::Failed { .. }) => break,
            Some(_) => continue,
            None => break,
        }
    }
    (events, text)
}

/// Recognizes rate-limit-shaped failure text (contracts-m8.md §F2, signal
/// list verbatim) so a `TurnOutcome::Failed` on a pool-holding behavior can
/// report it to `HarnessPool::report_rate_limit`. A false positive here is
/// harmless — it only costs a spurious backoff + one-step limit reduction,
/// which contracts-m8.md §F1's lazy recovery self-heals — so the list favors
/// recall over precision.
pub(crate) fn looks_like_rate_limit(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "rate limit",
        "rate_limit",
        "429",
        "usage limit",
        "session limit",
        "overloaded",
        "quota",
    ]
    .iter()
    .any(|signal| lower.contains(signal))
}

/// Strips a leading/trailing ``` fence (with an optional language tag), so
/// `{"covered_req_ids": [...]}` and ` ```json\n{...}\n``` ` both parse.
fn strip_code_fence(text: &str) -> &str {
    let t = text.trim();
    match t.strip_prefix("```") {
        Some(rest) => {
            let after_lang = match rest.find('\n') {
                Some(i) => &rest[i + 1..],
                None => rest,
            };
            after_lang.strip_suffix("```").unwrap_or(after_lang).trim()
        }
        None => t,
    }
}

/// Extracts the model's contract JSON object from `text`. Tries the whole
/// (fence-stripped) text first — the original, still-common case. If that
/// fails, a USER-GLOBAL hook (e.g. a Stop hook) can append prose after an
/// otherwise-valid reply inside the spawned CLI session; cwd isolation does
/// not shield against this (review r1, Finding 1). So on failure this scans
/// for balanced top-level `{...}` substrings — brace matching that ignores
/// braces inside JSON string values — and returns the first candidate that
/// parses as a JSON object.
pub fn extract_json(text: &str) -> Option<Value> {
    let stripped = strip_code_fence(text);

    if let Ok(value) = serde_json::from_str::<Value>(stripped) {
        if value.is_object() {
            return Some(value);
        }
    }

    balanced_brace_candidates(stripped)
        .into_iter()
        .find_map(|candidate| serde_json::from_str::<Value>(candidate).ok().filter(Value::is_object))
}

/// Finds every top-level `{...}` substring of `text` via brace-depth
/// matching, treating bytes inside a JSON string literal (honoring `\"`
/// escapes) as inert so a brace in a string value never mis-closes a
/// candidate early. Byte-index slicing is safe here: every slice boundary
/// falls on a `{`/`}` byte, and those are always ASCII, hence always UTF-8
/// char-boundary-safe.
fn balanced_brace_candidates(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut candidates = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            match matching_brace_end(bytes, i) {
                Some(end) => {
                    candidates.push(&text[i..=end]);
                    i = end + 1;
                    continue;
                }
                None => break,
            }
        }
        i += 1;
    }
    candidates
}

/// Byte index of the `}` that closes the `{` at `bytes[start]`, or `None` if
/// the text ends before the braces balance.
fn matching_brace_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[async_trait]
impl RoleBehavior for DesignerHarnessBehavior {
    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        if !matches!(env.kind, MessageKind::TaskAssign | MessageKind::ChangeRequest) {
            return vec![];
        }

        if let Err(e) = self.ensure_session().await {
            return self.blocked(&env, format!("spawn failed: {e}"));
        }

        let prompt = self.build_prompt(&env);
        let events_rx = self.events.take().expect("ensure_session set it");
        // Drain concurrently with send() — see drain_until_terminal's doc
        // comment for why sequencing this after send() deadlocks on a real
        // CLI turn that emits more events than the channel's capacity.
        let drain = tokio::spawn(drain_until_terminal(events_rx));

        let session = self.session.as_mut().expect("ensure_session set it");
        let outcome = self
            .harness
            .send(session, UserTurn { text: prompt }, DEFAULT_TURN_TIMEOUT)
            .await;

        let (events_rx, text) = drain
            .await
            .expect("drain_until_terminal must not panic");
        self.events = Some(events_rx);

        match outcome {
            Ok(TurnOutcome::Success) => match extract_json(&text) {
                Some(result_body) => self.ack_and_result(&env, result_body),
                None => self.blocked(&env, "designer turn produced no parseable JSON reply".to_string()),
            },
            Ok(TurnOutcome::Failed { error }) => {
                self.discard_session().await;
                tracing::warn!(reason = %error, "turn failed; session discarded — next turn respawns");
                self.blocked(&env, format!("designer turn failed: {error}"))
            }
            Err(e) => {
                self.discard_session().await;
                tracing::warn!(reason = %e, "turn failed; session discarded — next turn respawns");
                self.blocked(&env, format!("harness send error: {e}"))
            }
        }
    }

    fn is_done(&self) -> bool {
        false
    }
}

/// claude-backed role worker (M3 plan t-harness): a role-parameterized
/// generalization of `DesignerHarnessBehavior` that speaks the M3 envelope
/// body contract (`.orchestration/contracts-m3.md`) instead of M2's. Reuses
/// the same verified machinery (`drain_until_terminal`, `strip_code_fence`,
/// `extract_json`, the concurrent-drain-then-send ordering) — see that
/// struct's docs above for why draining must run concurrently with `send()`.
pub struct RoleHarnessBehavior {
    harness: Arc<dyn Harness>,
    cfg: AgentCfg,
    role: crew_proto::Role,
    system_hint: String,
    session: Option<Session>,
    events: Option<mpsc::Receiver<HarnessEvent>>,
    /// Text to prepend to the first *successful* turn's prompt (handoff
    /// pack JSON, sprint summary, ...) — contracts-m5.md C4a. Cleared only
    /// in `on_envelope`'s `Ok(Success)` arm (M13 turn-recovery fix, review
    /// r1 finding 1) — NOT in `build_prompt` — so a turn that fails and is
    /// retried (session discarded, respawned) still gets it prepended;
    /// only a genuinely successful turn consumes it.
    injected_context: Option<String>,
    /// contracts-m6.md §D2d: when set, the actual CLI-interaction span of
    /// each turn (`ensure_session`'s spawn + the send call) runs under a
    /// permit from this pool for `pool_harness_id`, held only for that span
    /// — never for the runner's whole lifetime (the M5 deadlock this fixes:
    /// holding a permit per-worker for the whole run starves later workers
    /// once the harness's concurrency limit is exceeded).
    pool: Option<Arc<crew_harness::HarnessPool>>,
    pool_harness_id: Option<String>,
    /// M13 turn-recovery fix (D3): the per-turn timeout passed to
    /// `Harness::send`. Defaults to `DEFAULT_TURN_TIMEOUT`; a run's actual
    /// value comes from `RunConfig.turn_timeout_secs` via `with_turn_timeout`.
    turn_timeout: Duration,
}

impl RoleHarnessBehavior {
    pub fn new(harness: Arc<dyn Harness>, cfg: AgentCfg, role: crew_proto::Role, system_hint: String) -> Self {
        Self {
            harness,
            cfg,
            role,
            system_hint,
            session: None,
            events: None,
            injected_context: None,
            pool: None,
            pool_harness_id: None,
            turn_timeout: DEFAULT_TURN_TIMEOUT,
        }
    }

    /// Prepends `text` to every attempt's prompt until one turn genuinely
    /// succeeds (contracts-m5.md C4a; M13 turn-recovery fix, review r1
    /// finding 1 — a failed-and-retried attempt must not lose it) — e.g. a
    /// handoff pack JSON or a sprint summary assembled by the caller.
    pub fn with_injected_context(mut self, text: String) -> Self {
        self.injected_context = Some(text);
        self
    }

    /// contracts-m6.md §D2d: gate each turn's actual harness interaction on
    /// `pool`'s per-harness concurrency limit for `harness_id`. Unset (the
    /// default) leaves turn behavior exactly as before this existed.
    pub fn with_pool(mut self, pool: Arc<crew_harness::HarnessPool>, harness_id: String) -> Self {
        self.pool = Some(pool);
        self.pool_harness_id = Some(harness_id);
        self
    }

    /// Sets the per-turn timeout passed to `Harness::send` (M13 turn-
    /// recovery fix D3) — the run's `RunConfig.turn_timeout_secs`.
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Self {
        tracing::info!(secs = timeout.as_secs(), "turn timeout");
        self.turn_timeout = timeout;
        self
    }

    async fn ensure_session(&mut self) -> Result<(), crew_harness::HarnessError> {
        if self.session.is_none() {
            let mut session = self.harness.spawn(&self.cfg).await?;
            let events = self.harness.take_events(&mut session);
            self.session = Some(session);
            self.events = Some(events);
        }
        Ok(())
    }

    /// Drops the dead session after a failed turn (shutting it down) so the
    /// next envelope's `ensure_session` respawns a fresh CLI process instead
    /// of writing to a dead pipe (M13 turn-recovery fix, D1).
    async fn discard_session(&mut self) {
        if let Some(s) = self.session.take() {
            let _ = self.harness.shutdown(s).await;
        }
        self.events = None;
    }

    /// Builds this turn's prompt, or an `Err(reason)` if `env` is a
    /// `TaskAssign` whose `body["task"]` does not parse as a `TaskSpec`
    /// (M3 contract, `.orchestration/contracts-m3.md`) — the caller maps
    /// that to `Blocked` (D3). Reads (does not consume) `injected_context`
    /// (M5 C4a) — a failed turn must still have it available for the
    /// retry's prompt (M13 turn-recovery fix, review r1 finding 1); the
    /// caller clears it once a turn actually succeeds, in `on_envelope`'s
    /// `Ok(Success)` arm.
    fn build_prompt(&mut self, env: &Envelope) -> Result<String, String> {
        let task_desc = match env.kind {
            MessageKind::TaskAssign => {
                let task_value = env
                    .body
                    .get("task")
                    .cloned()
                    .ok_or_else(|| "task.assign body missing \"task\"".to_string())?;
                let task: TaskSpec = serde_json::from_value(task_value)
                    .map_err(|e| format!("task.assign body[\"task\"] is not a valid TaskSpec: {e}"))?;

                // Covered goal = union of every ReqCover.ids across the task's dod (M3 contract).
                let cover_ids: BTreeSet<&str> = task
                    .dod
                    .iter()
                    .filter_map(|check| match check {
                        DodCheck::ReqCover { ids } => Some(ids.iter().map(|id| id.as_str())),
                        _ => None,
                    })
                    .flatten()
                    .collect();
                let artifacts_expected: Vec<Value> = task
                    .artifacts_expected
                    .iter()
                    .map(|a| {
                        json!({
                            "name": a.name,
                            "kind": a.kind,
                            "req_ids": a.req_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                        })
                    })
                    .collect();

                format!(
                    "Task assignment. role={role:?} title={title:?} brief: {brief}\ncover_req_ids={cover_ids:?}\nartifacts_expected={artifacts_expected:?}",
                    role = self.role,
                    title = task.title,
                    brief = task.brief,
                )
            }
            MessageKind::ChangeRequest => {
                let violations = env.body.get("violations").cloned().unwrap_or(json!([]));
                let reason = env.body.get("reason").and_then(Value::as_str).unwrap_or("");
                format!("Change request. violations={violations}. reason: {reason}")
            }
            _ => String::new(),
        };

        // Hardcoded here rather than left to `system_hint` (SPIKE-M2 Finding
        // 1): a caller that forgets to fold it into the hint reintroduces
        // the 120s tool-use timeout.
        let prompt = format!(
            "{}\n\n{}\n\nDo not use tools, do not read or write files, do not run commands, answer immediately.\n\nreply ONLY with JSON {{\"covered_req_ids\": [...], \"artifacts\": [{{\"name\": \"...\", \"kind\": \"...\", \"req_ids\": [...], \"content\": \"...\"}}]}}",
            self.system_hint, task_desc
        );

        Ok(match self.injected_context.as_deref() {
            Some(injected) => format!("{injected}\n\n{prompt}"),
            None => prompt,
        })
    }

    fn reply(&self, in_reply_to: &Envelope, kind: MessageKind, body: Value) -> Envelope {
        let from = in_reply_to.to.first().cloned().unwrap_or_default();
        Envelope::new(
            in_reply_to.sprint.clone(),
            in_reply_to.thread.clone(),
            from,
            vec![in_reply_to.from.clone()],
            kind,
            Some(in_reply_to.id.clone()),
            in_reply_to.corr.clone(),
            body,
            vec![],
            true,
            DEADLINE_MS,
        )
    }

    fn ack_and_result(&self, in_reply_to: &Envelope, result_body: Value) -> Vec<Envelope> {
        let ack = self.reply(in_reply_to, MessageKind::TaskAck, json!({}));
        let result = self.reply(in_reply_to, MessageKind::TaskResult, result_body);
        vec![ack, result]
    }

    fn blocked(&self, in_reply_to: &Envelope, reason: String) -> Vec<Envelope> {
        vec![self.reply(in_reply_to, MessageKind::Blocked, json!({"reason": reason}))]
    }
}

/// Ensures the M3 `task.result` body always carries both contract keys as
/// arrays (D4 defensive coercion) — a model reply that covers everything
/// vacuously or attaches no artifacts may reasonably omit one or both keys,
/// and that isn't a parse failure.
fn coerce_result_body(mut body: Value) -> Value {
    if !body.get("covered_req_ids").map(Value::is_array).unwrap_or(false) {
        body["covered_req_ids"] = json!([]);
    }
    if !body.get("artifacts").map(Value::is_array).unwrap_or(false) {
        body["artifacts"] = json!([]);
    }
    body
}

#[async_trait]
impl RoleBehavior for RoleHarnessBehavior {
    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        if !matches!(env.kind, MessageKind::TaskAssign | MessageKind::ChangeRequest) {
            return vec![];
        }

        let prompt = match self.build_prompt(&env) {
            Ok(prompt) => prompt,
            Err(reason) => return self.blocked(&env, reason),
        };

        // contracts-m6.md §D2d: hold the permit only across this turn's
        // actual CLI interaction (spawn + send), not the runner's whole
        // lifetime — dropped explicitly right after `send` returns, below.
        let permit = match (self.pool.as_ref(), self.pool_harness_id.as_ref()) {
            (Some(pool), Some(harness_id)) => Some(pool.acquire(harness_id).await),
            _ => None,
        };

        if let Err(e) = self.ensure_session().await {
            return self.blocked(&env, format!("spawn failed: {e}"));
        }

        let events_rx = self.events.take().expect("ensure_session set it");
        // Drain concurrently with send() — see drain_until_terminal's doc
        // comment for why sequencing this after send() deadlocks on a real
        // CLI turn that emits more events than the channel's capacity.
        let drain = tokio::spawn(drain_until_terminal(events_rx));

        let session = self.session.as_mut().expect("ensure_session set it");
        let outcome = self
            .harness
            .send(session, UserTurn { text: prompt }, self.turn_timeout)
            .await;
        drop(permit);

        let (events_rx, text) = drain
            .await
            .expect("drain_until_terminal must not panic");
        self.events = Some(events_rx);

        match outcome {
            Ok(TurnOutcome::Success) => {
                // The turn actually reached the model — whatever was
                // injected is now part of that live session's history, so
                // it must not be re-prepended to a later turn (M13
                // turn-recovery fix, review r1 finding 1: only a genuine
                // success consumes it — a Failed/Err turn below leaves it
                // in place for the retry).
                self.injected_context = None;
                match extract_json(&text) {
                    Some(result_body) => self.ack_and_result(&env, coerce_result_body(result_body)),
                    None => self.blocked(&env, "role turn produced no parseable JSON reply".to_string()),
                }
            }
            Ok(TurnOutcome::Failed { error }) => {
                // contracts-m8.md §F2: report a rate-limit-shaped failure to
                // the pool this turn actually ran under, so the next
                // `acquire` backs off and the harness's effective
                // concurrency limit shrinks. Only reachable when `with_pool`
                // was used (M6 §D2d) — the permit scope above is unchanged,
                // this only adds a report after it's already dropped.
                if let (Some(pool), Some(harness_id)) = (self.pool.as_ref(), self.pool_harness_id.as_ref()) {
                    if looks_like_rate_limit(&error) {
                        pool.report_rate_limit(harness_id);
                    }
                }
                self.discard_session().await;
                tracing::warn!(role = ?self.role, reason = %error, "turn failed; session discarded — next turn respawns");
                self.blocked(&env, format!("role turn failed: {error}"))
            }
            Err(e) => {
                self.discard_session().await;
                tracing::warn!(role = ?self.role, reason = %e, "turn failed; session discarded — next turn respawns");
                self.blocked(&env, format!("harness send error: {e}"))
            }
        }
    }

    fn is_done(&self) -> bool {
        false
    }

    /// contracts-m6.md §D1: mid-sprint harness swap. Snapshot/shutdown run
    /// against the *old* harness (before `self.harness` is replaced) —
    /// either erroring propagates to `ack`, but the session is dropped via
    /// `self.session.take()` either way (no zombie). Respawn is left to the
    /// next turn's `ensure_session` (lazy — D5). §D2d: the pool's stored
    /// harness_id is updated to the new one, so a turn after the swap
    /// consumes the new harness's own concurrency limit.
    async fn on_control(&mut self, ctrl: AgentControl) -> Vec<Envelope> {
        match ctrl {
            AgentControl::Swap {
                harness,
                harness_id,
                injected_context,
                ack,
            } => {
                if self.pool.is_some() {
                    self.pool_harness_id = Some(harness_id.clone());
                }
                let snapshot = match self.session.take() {
                    Some(session) => {
                        let snap_result = self.harness.snapshot(&session).await;
                        let shutdown_result = self.harness.shutdown(session).await;
                        match (snap_result, shutdown_result) {
                            (Ok(snap), Ok(())) => Ok(snap),
                            (Ok(_), Err(e)) => Err(e),
                            (Err(e), _) => Err(e),
                        }
                    }
                    None => Ok(HandoffSnapshot {
                        harness: harness_id,
                        session_id: String::new(),
                        notes: "no session yet".to_string(),
                    }),
                };
                self.harness = harness;
                self.injected_context = Some(injected_context);
                let _ = ack.send(snapshot);
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod role_harness_behavior_tests {
    use super::*;
    use crew_proto::{ArtifactContract, ReqId, Role};

    fn make_task_spec(cover_ids: &[&str]) -> TaskSpec {
        let dod = if cover_ids.is_empty() {
            vec![]
        } else {
            vec![DodCheck::ReqCover {
                ids: cover_ids.iter().map(|id| ReqId::new(*id).unwrap()).collect(),
            }]
        };
        TaskSpec {
            id: "t1".to_string(),
            role: Role::Developer,
            title: "Build the landing page".to_string(),
            brief: "Build a simple landing page".to_string(),
            dod,
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

    fn behavior() -> RoleHarnessBehavior {
        // No harness turn is ever sent in this test — build_prompt is
        // called directly — so a never-invoked fake binary path is fine.
        RoleHarnessBehavior::new(
            Arc::new(crew_harness::claude::ClaudeCodeHarness::with_binary("/bin/false")),
            AgentCfg {
                cwd: std::env::current_dir().expect("current dir"),
                model: None,
            },
            Role::Developer,
            // Deliberately does NOT mention tools or JSON — proves the
            // instruction is hardcoded in build_prompt, not delegated to
            // system_hint (SPIKE-M2 Finding 1).
            "You are a helpful assistant.".to_string(),
        )
    }

    #[test]
    fn build_prompt_hardcodes_no_tool_use_and_json_schema_instructions() {
        let task = make_task_spec(&["REQ-1"]);
        let prompt = behavior()
            .build_prompt(&task_assign(&task))
            .expect("valid task assign must build a prompt");

        assert!(
            prompt.to_lowercase().contains("do not use tools"),
            "prompt must hardcode the no-tool-use instruction: {prompt}"
        );
        assert!(
            prompt.contains("covered_req_ids") && prompt.contains("artifacts"),
            "prompt must hardcode the M3 JSON schema instruction: {prompt}"
        );
    }

    #[test]
    fn build_prompt_rejects_task_assign_with_missing_task_body() {
        let env = Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:developer".to_string()],
            MessageKind::TaskAssign,
            None,
            "req_01".to_string(),
            json!({}),
            vec![],
            true,
            900_000,
        );

        let err = behavior()
            .build_prompt(&env)
            .expect_err("missing body[\"task\"] must not build a prompt");
        assert!(err.contains("task"));
    }
}

/// M13 turn-recovery fix (D1/D2/D6/D9): a failed turn (`Ok(Failed{..})` or
/// any `Err(HarnessError)`) must drop the dead session so the next envelope
/// respawns, for both `RoleHarnessBehavior` and `DesignerHarnessBehavior`;
/// a `Success` outcome must keep the session alive (no spurious respawn).
/// `Session` has no public constructor outside crew-harness (see
/// `crates/crew-agent/tests/control.rs`'s `FailingHarness` doc comment), so
/// `ScriptedHarness` below wraps a real `ClaudeCodeHarness` driving the
/// `fake-role-claude.sh` fixture (a real subprocess, real events, so
/// `drain_until_terminal` never hangs) and only overrides the *value
/// returned to the caller* per a scripted queue; `spawn`/`shutdown` counts
/// are real `AtomicUsize`s, proving genuine respawn and cleanup.
#[cfg(test)]
mod turn_recovery_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crew_harness::claude::ClaudeCodeHarness;
    use crew_harness::HarnessError;
    use crew_proto::{ArtifactContract, DodCheck, ReqId, Role, TaskSpec};
    use tokio::sync::Mutex as AsyncMutex;

    fn fake_cli_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-role-claude.sh")
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

    /// M2 designer envelope shape — no `task` key, just `req_ids`/`brief`.
    fn designer_task_assign() -> Envelope {
        Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:designer".to_string()],
            MessageKind::TaskAssign,
            None,
            "req_01".to_string(),
            json!({ "req_ids": ["REQ-1"], "brief": "Design the landing page" }),
            vec![],
            true,
            900_000,
        )
    }

    /// One scripted `send` outcome — decoupled from the real transport (the
    /// fixture always completes a real, successful turn, so events keep
    /// flowing); only the value returned to the caller is overridden.
    enum ScriptedTurn {
        Real,
        Failed(&'static str),
        WriteBrokenPipe,
        ProcessExited,
    }

    struct ScriptedHarness {
        inner: ClaudeCodeHarness,
        script: AsyncMutex<VecDeque<ScriptedTurn>>,
        spawn_count: AtomicUsize,
        shutdown_count: AtomicUsize,
        /// Every turn's prompt text, in send order (review r1 finding 1:
        /// proves whether `injected_context` actually reached a given turn).
        prompts: std::sync::Mutex<Vec<String>>,
    }

    impl ScriptedHarness {
        fn new(script: Vec<ScriptedTurn>) -> Self {
            let text = r#"{"covered_req_ids": ["REQ-1"], "artifacts": []}"#;
            Self {
                inner: ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
                    .with_env("FAKE_MODE", "json")
                    .with_env("FAKE_ASSISTANT_1", assistant_line(text)),
                script: AsyncMutex::new(script.into_iter().collect()),
                spawn_count: AtomicUsize::new(0),
                shutdown_count: AtomicUsize::new(0),
                prompts: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn spawn_count(&self) -> usize {
            self.spawn_count.load(Ordering::SeqCst)
        }

        fn shutdown_count(&self) -> usize {
            self.shutdown_count.load(Ordering::SeqCst)
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Harness for ScriptedHarness {
        fn id(&self) -> crew_harness::HarnessId {
            self.inner.id()
        }

        async fn spawn(&self, cfg: &AgentCfg) -> Result<Session, HarnessError> {
            self.spawn_count.fetch_add(1, Ordering::SeqCst);
            self.inner.spawn(cfg).await
        }

        async fn send(
            &self,
            session: &mut Session,
            turn: UserTurn,
            timeout: Duration,
        ) -> Result<TurnOutcome, HarnessError> {
            self.prompts.lock().unwrap().push(turn.text.clone());
            // Always run the real turn so the fixture's events actually flow
            // (drain_until_terminal needs a terminal event or it hangs
            // forever) — only the value handed back to the caller follows
            // the script.
            let real = self.inner.send(session, turn, timeout).await;
            match self.script.lock().await.pop_front() {
                None | Some(ScriptedTurn::Real) => real,
                Some(ScriptedTurn::Failed(error)) => Ok(TurnOutcome::Failed {
                    error: error.to_string(),
                }),
                Some(ScriptedTurn::WriteBrokenPipe) => Err(HarnessError::Write(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "simulated broken pipe",
                ))),
                Some(ScriptedTurn::ProcessExited) => Err(HarnessError::ProcessExited),
            }
        }

        fn take_events(&self, session: &mut Session) -> mpsc::Receiver<HarnessEvent> {
            self.inner.take_events(session)
        }

        async fn snapshot(&self, session: &Session) -> Result<HandoffSnapshot, HarnessError> {
            self.inner.snapshot(session).await
        }

        async fn shutdown(&self, session: Session) -> Result<(), HarnessError> {
            self.shutdown_count.fetch_add(1, Ordering::SeqCst);
            self.inner.shutdown(session).await
        }
    }

    // -----------------------------------------------------------------
    // RoleHarnessBehavior
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn role_respawns_after_failed_timeout_then_succeeds() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::Failed("timeout"), ScriptedTurn::Real]));
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        );
        let task = make_task_spec();

        let first = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, MessageKind::Blocked);
        assert!(
            first[0].body["reason"].as_str().unwrap().starts_with("role turn failed: "),
            "reason string must stay exactly `role turn failed: ...`: {:?}",
            first[0].body["reason"]
        );

        let second = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(
            second[1].kind,
            MessageKind::TaskResult,
            "second turn must succeed on a fresh session"
        );

        assert_eq!(harness.spawn_count(), 2, "the dead session must be discarded and a fresh one spawned");
        assert_eq!(harness.shutdown_count(), 1);
    }

    #[tokio::test]
    async fn role_respawns_after_write_broken_pipe_err() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::WriteBrokenPipe, ScriptedTurn::Real]));
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        );
        let task = make_task_spec();

        let first = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(first[0].kind, MessageKind::Blocked);
        assert!(first[0].body["reason"]
            .as_str()
            .unwrap()
            .starts_with("harness send error: failed to write turn to child stdin: "));

        let second = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        assert_eq!(harness.spawn_count(), 2);
        assert_eq!(harness.shutdown_count(), 1);
    }

    #[tokio::test]
    async fn role_respawns_after_process_exited_err() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::ProcessExited, ScriptedTurn::Real]));
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        );
        let task = make_task_spec();

        let first = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(first[0].kind, MessageKind::Blocked);
        assert_eq!(
            first[0].body["reason"],
            json!("harness send error: child process exited before a result event arrived")
        );

        let second = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        assert_eq!(harness.spawn_count(), 2);
        assert_eq!(harness.shutdown_count(), 1);
    }

    #[tokio::test]
    async fn role_reuses_session_across_consecutive_successes() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::Real, ScriptedTurn::Real]));
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        );
        let task = make_task_spec();

        let first = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(first[1].kind, MessageKind::TaskResult);
        let second = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        assert_eq!(harness.spawn_count(), 1, "a successful turn must keep the session alive");
        assert_eq!(harness.shutdown_count(), 0);
    }

    /// Review r1 finding 1: `with_injected_context`'s L1 text must not be
    /// lost when the very first turn it was meant for fails and is retried
    /// (the exact ledger defect this task fixes) — `build_prompt` must not
    /// consume it before the turn is known to have succeeded.
    #[tokio::test]
    async fn injected_context_survives_a_failed_first_turn_and_reaches_the_retry() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::Failed("timeout"), ScriptedTurn::Real]));
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        )
        .with_injected_context("L1-MARKER".to_string());
        let task = make_task_spec();

        let first = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(first[0].kind, MessageKind::Blocked, "the first turn must fail as scripted");

        let second = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult, "the retry must succeed on a fresh session");

        let prompts = harness.prompts();
        assert_eq!(prompts.len(), 2);
        assert!(
            prompts[0].starts_with("L1-MARKER\n\n"),
            "the failed first attempt's prompt must also carry the marker: {:?}",
            prompts[0]
        );
        assert!(
            prompts[1].starts_with("L1-MARKER\n\n"),
            "the retry's prompt must still carry the L1 marker: {:?}",
            prompts[1]
        );
    }

    /// Same scenario without a failure: `injected_context` is genuinely
    /// single-shot once a turn actually succeeds — the second (unrelated)
    /// turn must not re-prepend it.
    #[tokio::test]
    async fn injected_context_is_cleared_only_after_a_turn_succeeds() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::Real, ScriptedTurn::Real]));
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        )
        .with_injected_context("L1-MARKER".to_string());
        let task = make_task_spec();

        let first = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(first[1].kind, MessageKind::TaskResult);
        let second = behavior.on_envelope(task_assign(&task)).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        let prompts = harness.prompts();
        assert_eq!(prompts.len(), 2);
        assert!(prompts[0].starts_with("L1-MARKER\n\n"), "the first prompt must carry the marker: {:?}", prompts[0]);
        assert!(
            !prompts[1].contains("L1-MARKER"),
            "a turn after a genuine success must not re-carry the marker: {:?}",
            prompts[1]
        );
    }

    // -----------------------------------------------------------------
    // DesignerHarnessBehavior (R2 — same recovery, M2 path)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn designer_respawns_after_failed_timeout_then_succeeds() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::Failed("timeout"), ScriptedTurn::Real]));
        let mut behavior = DesignerHarnessBehavior::new(harness.clone(), agent_cfg(), "You are the Designer.".to_string());

        let first = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(first[0].kind, MessageKind::Blocked);
        assert!(first[0].body["reason"]
            .as_str()
            .unwrap()
            .starts_with("designer turn failed: "));

        let second = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        assert_eq!(harness.spawn_count(), 2);
        assert_eq!(harness.shutdown_count(), 1);
    }

    #[tokio::test]
    async fn designer_respawns_after_write_broken_pipe_err() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::WriteBrokenPipe, ScriptedTurn::Real]));
        let mut behavior = DesignerHarnessBehavior::new(harness.clone(), agent_cfg(), "You are the Designer.".to_string());

        let first = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(first[0].kind, MessageKind::Blocked);
        assert!(first[0].body["reason"]
            .as_str()
            .unwrap()
            .starts_with("harness send error: failed to write turn to child stdin: "));

        let second = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        assert_eq!(harness.spawn_count(), 2, "the dead session must be discarded and a fresh one spawned");
        assert_eq!(harness.shutdown_count(), 1);
    }

    #[tokio::test]
    async fn designer_respawns_after_process_exited_err() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::ProcessExited, ScriptedTurn::Real]));
        let mut behavior = DesignerHarnessBehavior::new(harness.clone(), agent_cfg(), "You are the Designer.".to_string());

        let first = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(first[0].kind, MessageKind::Blocked);
        assert_eq!(
            first[0].body["reason"],
            json!("harness send error: child process exited before a result event arrived")
        );

        let second = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        assert_eq!(harness.spawn_count(), 2, "the dead session must be discarded and a fresh one spawned");
        assert_eq!(harness.shutdown_count(), 1);
    }

    #[tokio::test]
    async fn designer_reuses_session_across_consecutive_successes() {
        let harness = Arc::new(ScriptedHarness::new(vec![ScriptedTurn::Real, ScriptedTurn::Real]));
        let mut behavior = DesignerHarnessBehavior::new(harness.clone(), agent_cfg(), "You are the Designer.".to_string());

        let first = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(first[1].kind, MessageKind::TaskResult);
        let second = behavior.on_envelope(designer_task_assign()).await;
        assert_eq!(second[1].kind, MessageKind::TaskResult);

        assert_eq!(harness.spawn_count(), 1, "a successful turn must keep the session alive");
        assert_eq!(harness.shutdown_count(), 0);
    }
}

/// M13 turn-recovery fix (D3): `RoleHarnessBehavior::with_turn_timeout`
/// controls the `timeout` argument `send` receives; unset, it defaults to
/// `DEFAULT_TURN_TIMEOUT` (now 900s).
#[cfg(test)]
mod timeout_knob_tests {
    use super::*;
    use std::path::PathBuf;

    use crew_harness::claude::ClaudeCodeHarness;
    use crew_harness::HarnessError;
    use crew_proto::Role;

    fn fake_cli_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-role-claude.sh")
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

    fn agent_cfg() -> AgentCfg {
        AgentCfg {
            cwd: std::env::current_dir().expect("current dir"),
            model: None,
        }
    }

    fn task_assign() -> Envelope {
        let task = TaskSpec {
            id: "t1".to_string(),
            role: Role::Developer,
            title: "Build the landing page".to_string(),
            brief: "Build a simple landing page".to_string(),
            dod: vec![],
            deps: vec![],
            artifacts_expected: vec![],
        };
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

    /// Records the `timeout` argument of every `send` call; otherwise a
    /// thin real delegate to `ClaudeCodeHarness` (`Session` has no public
    /// constructor outside crew-harness — see `tests/control.rs`).
    struct TimeoutCapturingHarness {
        inner: ClaudeCodeHarness,
        recorded: std::sync::Mutex<Vec<Duration>>,
    }

    #[async_trait]
    impl Harness for TimeoutCapturingHarness {
        fn id(&self) -> crew_harness::HarnessId {
            self.inner.id()
        }

        async fn spawn(&self, cfg: &AgentCfg) -> Result<Session, HarnessError> {
            self.inner.spawn(cfg).await
        }

        async fn send(&self, session: &mut Session, turn: UserTurn, timeout: Duration) -> Result<TurnOutcome, HarnessError> {
            self.recorded.lock().unwrap().push(timeout);
            // Record the caller's `timeout` for the assertion, but bound the
            // real fixture subprocess call with a generous real deadline
            // regardless of it — a genuinely-elapsing 7ms timeout against a
            // real spawned process is a flaky test, not a pass-through proof.
            self.inner.send(session, turn, Duration::from_secs(5)).await
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

    fn timeout_capturing_harness() -> Arc<TimeoutCapturingHarness> {
        let text = r#"{"covered_req_ids": ["REQ-1"], "artifacts": []}"#;
        Arc::new(TimeoutCapturingHarness {
            inner: ClaudeCodeHarness::with_binary(fake_cli_path().to_str().unwrap())
                .with_env("FAKE_MODE", "json")
                .with_env("FAKE_ASSISTANT_1", assistant_line(text)),
            recorded: std::sync::Mutex::new(Vec::new()),
        })
    }

    #[tokio::test]
    async fn with_turn_timeout_passes_its_duration_to_send() {
        let harness = timeout_capturing_harness();
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        )
        .with_turn_timeout(Duration::from_millis(7));

        behavior.on_envelope(task_assign()).await;

        assert_eq!(*harness.recorded.lock().unwrap(), vec![Duration::from_millis(7)]);
    }

    #[tokio::test]
    async fn without_with_turn_timeout_send_receives_the_900s_default() {
        let harness = timeout_capturing_harness();
        let mut behavior = RoleHarnessBehavior::new(
            harness.clone(),
            agent_cfg(),
            Role::Developer,
            "You are the Developer.".to_string(),
        );

        behavior.on_envelope(task_assign()).await;

        assert_eq!(*harness.recorded.lock().unwrap(), vec![DEFAULT_TURN_TIMEOUT]);
        assert_eq!(DEFAULT_TURN_TIMEOUT, Duration::from_secs(900));
    }
}

/// review r1 Finding 1: a USER-GLOBAL Stop hook can append prose after an
/// otherwise-valid contract reply inside the spawned CLI session (observed:
/// the memory-loop learning-nudge hook's "Learning review: nothing to
/// persist." tail). `extract_json` must still recover the object.
#[cfg(test)]
mod extract_json_tests {
    use super::*;

    #[test]
    fn extract_json_parses_valid_json_with_trailing_hook_prose() {
        let text = "{\"covered_req_ids\": [\"REQ-1\"], \"artifacts\": []}\n\nLearning review: nothing to persist.";

        let value = extract_json(text).expect("must extract JSON despite trailing hook prose");

        assert_eq!(value["covered_req_ids"], json!(["REQ-1"]));
        assert_eq!(value["artifacts"], json!([]));
    }

    #[test]
    fn extract_json_parses_valid_json_with_leading_prose() {
        let text = "Sure, here is my answer:\n{\"covered_req_ids\": [\"REQ-1\"], \"artifacts\": []}";

        let value = extract_json(text).expect("must extract JSON despite leading prose");

        assert_eq!(value["covered_req_ids"], json!(["REQ-1"]));
    }

    #[test]
    fn extract_json_returns_none_when_text_has_no_json_object() {
        let text = "I cannot comply with that request right now.";

        assert_eq!(extract_json(text), None);
    }

    #[test]
    fn extract_json_still_handles_fenced_json_unchanged() {
        let text = "```json\n{\"covered_req_ids\": [\"REQ-1\"], \"artifacts\": []}\n```";

        let value = extract_json(text).expect("fenced JSON must still parse");

        assert_eq!(value["covered_req_ids"], json!(["REQ-1"]));
    }

    #[test]
    fn extract_json_ignores_braces_inside_string_values_when_brace_matching() {
        let text = r#"noise before {"note": "a { b } c", "covered_req_ids": [], "artifacts": []} noise after"#;

        let value = extract_json(text)
            .expect("must find the balanced object despite braces inside a string value");

        assert_eq!(value["note"], json!("a { b } c"));
        assert_eq!(value["covered_req_ids"], json!([]));
    }
}

/// contracts-m8.md §F2: `looks_like_rate_limit`'s signal list, verbatim.
#[cfg(test)]
mod looks_like_rate_limit_tests {
    use super::*;

    #[test]
    fn matches_mixed_case_rate_limit_phrase() {
        assert!(looks_like_rate_limit("Error: Rate Limit exceeded, try again later"));
    }

    #[test]
    fn matches_http_429_status_mentioned_in_prose() {
        assert!(looks_like_rate_limit("upstream call failed: HTTP 429"));
    }

    #[test]
    fn matches_quota_exceeded_message() {
        assert!(looks_like_rate_limit("quota exceeded for this billing period"));
    }

    #[test]
    fn does_not_match_unrelated_failure_text() {
        assert!(!looks_like_rate_limit("file not found"));
    }

    #[test]
    fn does_not_match_empty_string() {
        assert!(!looks_like_rate_limit(""));
    }
}
