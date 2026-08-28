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

/// Extracts the model's contract JSON object from `text`. Tries the whole
/// (fence-stripped) text first — the original, still-common case. If that
/// fails, a USER-GLOBAL hook (e.g. a Stop hook) can append prose after an
/// otherwise-valid reply inside the spawned CLI session; cwd isolation does
/// not shield against this (review r1, Finding 1). So on failure this scans
/// for balanced top-level `{...}` substrings — brace matching that ignores
/// braces inside JSON string values — and returns the first candidate that
/// parses as a JSON object.
fn extract_json(text: &str) -> Option<Value> {
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
    /// Text to prepend to the first turn's prompt (handoff pack JSON,
    /// sprint summary, ...) — contracts-m5.md C4a. `Option::take()` in
    /// `build_prompt` guarantees it is consumed exactly once, so later
    /// turns are unaffected without any extra bookkeeping.
    injected_context: Option<String>,
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
        }
    }

    /// Prepends `text` to the very first turn's prompt only (contracts-m5.md
    /// C4a) — e.g. a handoff pack JSON or a sprint summary assembled by the
    /// caller.
    pub fn with_injected_context(mut self, text: String) -> Self {
        self.injected_context = Some(text);
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

    /// Builds this turn's prompt, or an `Err(reason)` if `env` is a
    /// `TaskAssign` whose `body["task"]` does not parse as a `TaskSpec`
    /// (M3 contract, `.orchestration/contracts-m3.md`) — the caller maps
    /// that to `Blocked` (D3). `&mut self` so a successful build can
    /// consume `injected_context` exactly once (M5 C4a).
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

        Ok(match self.injected_context.take() {
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
