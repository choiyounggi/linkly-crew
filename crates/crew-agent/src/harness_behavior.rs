use std::sync::Arc;

use async_trait::async_trait;
use crew_harness::{AgentCfg, Harness, HarnessEvent, Session, TurnOutcome, UserTurn, DEFAULT_TURN_TIMEOUT};
use crew_proto::{Envelope, MessageKind};
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
