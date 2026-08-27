use crew_proto::{Envelope, MessageKind};
use serde_json::{json, Value};

use crate::role::RoleBehavior;

use async_trait::async_trait;

const DEADLINE_MS: u64 = 900_000;

/// PM's own progress through one PM<->Designer conversation — plan A7. The
/// truth source for whether/how the conversation ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PmState {
    Assigned,
    AwaitingRework { round: u8 },
    Accepted { rounds_used: u8 },
    Exhausted,
}

/// Scripted PM role (plan A7): assigns `req_ids`, validates each
/// `task.result`'s `covered_req_ids` against them, and either accepts or
/// requests rework, bounded by `max_rounds`.
pub struct ScriptedPm {
    agent_id: String,
    designer_id: String,
    req_ids: Vec<String>,
    max_rounds: u8,
    sprint: String,
    thread: String,
    corr: String,
    round: u8,
    state: PmState,
}

impl ScriptedPm {
    pub fn new(
        agent_id: impl Into<String>,
        designer_id: impl Into<String>,
        req_ids: Vec<String>,
        max_rounds: u8,
    ) -> Self {
        let designer_id = designer_id.into();
        let corr = format!("req_{}", uuid::Uuid::new_v4());
        let thread = format!("th-{designer_id}");
        Self {
            agent_id: agent_id.into(),
            designer_id,
            req_ids,
            max_rounds,
            sprint: "sp-m2".to_string(),
            thread,
            corr,
            round: 0,
            state: PmState::Assigned,
        }
    }

    pub fn state(&self) -> &PmState {
        &self.state
    }

    /// `covered_req_ids ⊇ req_ids` check with defensive parsing (plan A7 /
    /// docs/RESEARCH.md R6): a missing or non-array field is treated as no
    /// coverage at all, so the whole `req_ids` set comes back as violations.
    fn missing_req_ids(&self, body: &Value) -> Vec<String> {
        let covered: Vec<&str> = match body.get("covered_req_ids") {
            Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        self.req_ids
            .iter()
            .filter(|id| !covered.contains(&id.as_str()))
            .cloned()
            .collect()
    }

    fn reply(
        &self,
        in_reply_to: &Envelope,
        kind: MessageKind,
        body: Value,
    ) -> Envelope {
        Envelope::new(
            in_reply_to.sprint.clone(),
            in_reply_to.thread.clone(),
            self.agent_id.clone(),
            vec![self.designer_id.clone()],
            kind,
            Some(in_reply_to.id.clone()),
            in_reply_to.corr.clone(),
            body,
            vec![],
            true,
            DEADLINE_MS,
        )
    }
}

#[async_trait]
impl RoleBehavior for ScriptedPm {
    async fn on_start(&mut self) -> Vec<Envelope> {
        let assign = Envelope::new(
            self.sprint.clone(),
            self.thread.clone(),
            self.agent_id.clone(),
            vec![self.designer_id.clone()],
            MessageKind::TaskAssign,
            None,
            self.corr.clone(),
            json!({"req_ids": self.req_ids, "brief": "M2 change_request loop"}),
            vec![],
            true,
            DEADLINE_MS,
        );
        vec![assign]
    }

    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        if env.kind != MessageKind::TaskResult {
            return vec![];
        }

        let missing = self.missing_req_ids(&env.body);
        if missing.is_empty() {
            let rounds_used = self.round;
            self.state = PmState::Accepted { rounds_used };
            vec![self.reply(
                &env,
                MessageKind::TaskAck,
                json!({"accepted": true, "rounds_used": rounds_used}),
            )]
        } else {
            self.round += 1;
            if self.round > self.max_rounds {
                self.state = PmState::Exhausted;
                vec![]
            } else {
                self.state = PmState::AwaitingRework { round: self.round };
                let reason = format!("{} not covered", missing.join(", "));
                vec![self.reply(
                    &env,
                    MessageKind::ChangeRequest,
                    json!({"violations": missing, "reason": reason}),
                )]
            }
        }
    }

    fn is_done(&self) -> bool {
        matches!(self.state, PmState::Accepted { .. } | PmState::Exhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_result(in_reply_to: &Envelope, body: Value) -> Envelope {
        Envelope::new(
            in_reply_to.sprint.clone(),
            in_reply_to.thread.clone(),
            "agent:designer".to_string(),
            vec!["agent:pm".to_string()],
            MessageKind::TaskResult,
            Some(in_reply_to.id.clone()),
            in_reply_to.corr.clone(),
            body,
            vec![],
            true,
            DEADLINE_MS,
        )
    }

    #[tokio::test]
    async fn on_start_sends_task_assign() {
        let mut pm = ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec!["REQ-1".to_string(), "REQ-2".to_string()],
            3,
        );

        let replies = pm.on_start().await;

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::TaskAssign);
        assert_eq!(replies[0].to, vec!["agent:designer".to_string()]);
        assert_eq!(
            replies[0].body["req_ids"],
            json!(["REQ-1", "REQ-2"])
        );
        assert!(replies[0].requires_ack);
        assert_eq!(replies[0].deadline_ms, DEADLINE_MS);
        assert_eq!(*pm.state(), PmState::Assigned);
    }

    #[tokio::test]
    async fn detects_violation_then_accepts_after_rework() {
        let mut pm = ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec!["REQ-1".to_string(), "REQ-2".to_string()],
            3,
        );
        let assign = pm.on_start().await.remove(0);

        let short_result = task_result(&assign, json!({"covered_req_ids": ["REQ-1"]}));
        let replies = pm.on_envelope(short_result).await;

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::ChangeRequest);
        assert_eq!(replies[0].body["violations"], json!(["REQ-2"]));
        assert_eq!(*pm.state(), PmState::AwaitingRework { round: 1 });

        let cr = replies.into_iter().next().unwrap();
        let full_result = task_result(&cr, json!({"covered_req_ids": ["REQ-1", "REQ-2"]}));
        let replies = pm.on_envelope(full_result).await;

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::TaskAck);
        assert_eq!(
            replies[0].body,
            json!({"accepted": true, "rounds_used": 1})
        );
        assert_eq!(*pm.state(), PmState::Accepted { rounds_used: 1 });
        assert!(pm.is_done());
    }

    #[tokio::test]
    async fn round_budget_exhausts_after_repeated_violations() {
        let mut pm = ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec!["REQ-1".to_string()],
            1,
        );
        let mut last = pm.on_start().await.remove(0);

        // round 1: still within budget (max_rounds = 1) -> rework requested.
        let result = task_result(&last, json!({"covered_req_ids": []}));
        let replies = pm.on_envelope(result).await;
        assert_eq!(*pm.state(), PmState::AwaitingRework { round: 1 });
        last = replies.into_iter().next().expect("change_request sent");

        // round 2: exceeds max_rounds(1) -> exhausted, no further reply.
        let result = task_result(&last, json!({"covered_req_ids": []}));
        let replies = pm.on_envelope(result).await;
        assert!(replies.is_empty());
        assert_eq!(*pm.state(), PmState::Exhausted);
        assert!(pm.is_done());
    }

    #[tokio::test]
    async fn empty_req_ids_accepts_immediately() {
        let mut pm = ScriptedPm::new("agent:pm", "agent:designer", vec![], 3);
        let assign = pm.on_start().await.remove(0);

        let result = task_result(&assign, json!({"covered_req_ids": []}));
        let replies = pm.on_envelope(result).await;

        assert_eq!(replies[0].kind, MessageKind::TaskAck);
        assert_eq!(*pm.state(), PmState::Accepted { rounds_used: 0 });
    }

    #[tokio::test]
    async fn missing_covered_req_ids_field_is_treated_as_full_violation() {
        let mut pm = ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec!["REQ-1".to_string()],
            3,
        );
        let assign = pm.on_start().await.remove(0);

        let result = task_result(&assign, json!({}));
        let replies = pm.on_envelope(result).await;

        assert_eq!(replies[0].kind, MessageKind::ChangeRequest);
        assert_eq!(replies[0].body["violations"], json!(["REQ-1"]));
    }

    #[tokio::test]
    async fn non_array_covered_req_ids_field_is_treated_as_full_violation() {
        let mut pm = ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec!["REQ-1".to_string()],
            3,
        );
        let assign = pm.on_start().await.remove(0);

        let result = task_result(&assign, json!({"covered_req_ids": "REQ-1"}));
        let replies = pm.on_envelope(result).await;

        assert_eq!(replies[0].kind, MessageKind::ChangeRequest);
        assert_eq!(replies[0].body["violations"], json!(["REQ-1"]));
    }

    #[tokio::test]
    async fn non_task_result_kind_is_ignored() {
        let mut pm = ScriptedPm::new(
            "agent:pm",
            "agent:designer",
            vec!["REQ-1".to_string()],
            3,
        );
        let assign = pm.on_start().await.remove(0);
        let ack = Envelope::new(
            assign.sprint.clone(),
            assign.thread.clone(),
            "agent:designer".to_string(),
            vec!["agent:pm".to_string()],
            MessageKind::TaskAck,
            Some(assign.id.clone()),
            assign.corr.clone(),
            json!({}),
            vec![],
            true,
            DEADLINE_MS,
        );

        let replies = pm.on_envelope(ack).await;

        assert!(replies.is_empty());
        assert_eq!(*pm.state(), PmState::Assigned);
    }
}
