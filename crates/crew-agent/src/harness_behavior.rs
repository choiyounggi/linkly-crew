use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use crew_harness::{AgentCfg, Harness, HarnessEvent, Session, TurnOutcome, UserTurn, DEFAULT_TURN_TIMEOUT};
use crew_proto::{Envelope, MessageKind};
use crew_proto::{DodCheck, TaskSpec};
use serde_json::{json, Value};
use tokio::sync::mpsc;

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

fn extract_json(text: &str) -> Option<Value> {
    serde_json::from_str::<Value>(strip_code_fence(text))
        .ok()
        .filter(Value::is_object)
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
                self.blocked(&env, format!("designer turn failed: {error}"))
            }
            Err(e) => self.blocked(&env, format!("harness send error: {e}")),
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

    /// Builds this turn's prompt, or an `Err(reason)` if `env` is a
    /// `TaskAssign` whose `body["task"]` does not parse as a `TaskSpec`
    /// (M3 contract, `.orchestration/contracts-m3.md`) — the caller maps
    /// that to `Blocked` (D3).
    fn build_prompt(&self, env: &Envelope) -> Result<String, String> {
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
        Ok(format!(
            "{}\n\n{}\n\nDo not use tools, do not read or write files, do not run commands, answer immediately.\n\nreply ONLY with JSON {{\"covered_req_ids\": [...], \"artifacts\": [{{\"name\": \"...\", \"kind\": \"...\", \"req_ids\": [...], \"content\": \"...\"}}]}}",
            self.system_hint, task_desc
        ))
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
            .send(session, UserTurn { text: prompt }, DEFAULT_TURN_TIMEOUT)
            .await;

        let (events_rx, text) = drain
            .await
            .expect("drain_until_terminal must not panic");
        self.events = Some(events_rx);

        match outcome {
            Ok(TurnOutcome::Success) => match extract_json(&text) {
                Some(result_body) => self.ack_and_result(&env, coerce_result_body(result_body)),
                None => self.blocked(&env, "role turn produced no parseable JSON reply".to_string()),
            },
            Ok(TurnOutcome::Failed { error }) => {
                self.blocked(&env, format!("role turn failed: {error}"))
            }
            Err(e) => self.blocked(&env, format!("harness send error: {e}")),
        }
    }

    fn is_done(&self) -> bool {
        false
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
