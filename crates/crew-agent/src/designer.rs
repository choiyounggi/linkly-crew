use async_trait::async_trait;
use crew_proto::{Envelope, MessageKind};
use serde_json::{json, Value};

use crate::role::RoleBehavior;

const DEADLINE_MS: u64 = 900_000;

/// Scripted Designer role (plan A7): on the first `task.assign` it omits
/// `planted_violations` from its coverage (simulating a spec violation); on
/// every later `change_request` it reports full coverage (rework fixes the
/// violation, which is planted only once).
pub struct ScriptedDesigner {
    agent_id: String,
    planted_violations: Vec<String>,
    req_ids: Vec<String>,
    violation_planted: bool,
}

impl ScriptedDesigner {
    pub fn new(agent_id: impl Into<String>, planted_violations: Vec<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            planted_violations,
            req_ids: vec![],
            violation_planted: false,
        }
    }

    fn reply(&self, in_reply_to: &Envelope, kind: MessageKind, body: Value) -> Envelope {
        Envelope::new(
            in_reply_to.sprint.clone(),
            in_reply_to.thread.clone(),
            self.agent_id.clone(),
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

    fn ack_and_result(&self, in_reply_to: &Envelope, covered: Vec<String>) -> Vec<Envelope> {
        let ack = self.reply(in_reply_to, MessageKind::TaskAck, json!({}));
        let result = self.reply(
            in_reply_to,
            MessageKind::TaskResult,
            json!({"covered_req_ids": covered, "artifact": "design.md 요약 텍스트"}),
        );
        vec![ack, result]
    }
}

fn string_array(body: &Value, field: &str) -> Vec<String> {
    match body.get(field) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect(),
        _ => vec![],
    }
}

#[async_trait]
impl RoleBehavior for ScriptedDesigner {
    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        match env.kind {
            MessageKind::TaskAssign => {
                self.req_ids = string_array(&env.body, "req_ids");
                let covered = if self.violation_planted {
                    self.req_ids.clone()
                } else {
                    self.violation_planted = true;
                    self.req_ids
                        .iter()
                        .filter(|id| !self.planted_violations.contains(id))
                        .cloned()
                        .collect()
                };
                self.ack_and_result(&env, covered)
            }
            MessageKind::ChangeRequest => {
                let covered = self.req_ids.clone();
                self.ack_and_result(&env, covered)
            }
            _ => vec![],
        }
    }

    fn is_done(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_assign(req_ids: &[&str]) -> Envelope {
        Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:pm".to_string(),
            vec!["agent:designer".to_string()],
            MessageKind::TaskAssign,
            None,
            "req_01".to_string(),
            json!({"req_ids": req_ids, "brief": "build the login screen"}),
            vec![],
            true,
            900_000,
        )
    }

    fn change_request(in_reply_to: &Envelope, violations: &[&str]) -> Envelope {
        Envelope::new(
            in_reply_to.sprint.clone(),
            in_reply_to.thread.clone(),
            "agent:pm".to_string(),
            vec!["agent:designer".to_string()],
            MessageKind::ChangeRequest,
            Some(in_reply_to.id.clone()),
            in_reply_to.corr.clone(),
            json!({"violations": violations, "reason": "missing coverage"}),
            vec![],
            true,
            900_000,
        )
    }

    #[tokio::test]
    async fn first_assign_omits_planted_violation_from_coverage() {
        let mut designer = ScriptedDesigner::new("agent:designer", vec!["REQ-2".to_string()]);
        let assign = task_assign(&["REQ-1", "REQ-2", "REQ-3"]);
        let assign_id = assign.id.clone();

        let replies = designer.on_envelope(assign).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].kind, MessageKind::TaskAck);
        assert_eq!(replies[0].in_reply_to, Some(assign_id.clone()));
        assert_eq!(replies[1].kind, MessageKind::TaskResult);
        assert_eq!(replies[1].in_reply_to, Some(assign_id));
        assert_eq!(
            replies[1].body["covered_req_ids"],
            json!(["REQ-1", "REQ-3"])
        );
        assert_eq!(replies[1].to, vec!["agent:pm".to_string()]);
        assert!(replies[1].requires_ack);
    }

    #[tokio::test]
    async fn change_request_after_planted_violation_covers_everything() {
        let mut designer = ScriptedDesigner::new("agent:designer", vec!["REQ-2".to_string()]);
        let assign = task_assign(&["REQ-1", "REQ-2", "REQ-3"]);
        let _first = designer.on_envelope(assign.clone()).await;

        let cr = change_request(&assign, &["REQ-2"]);
        let replies = designer.on_envelope(cr).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(replies[1].kind, MessageKind::TaskResult);
        assert_eq!(
            replies[1].body["covered_req_ids"],
            json!(["REQ-1", "REQ-2", "REQ-3"])
        );
    }

    #[tokio::test]
    async fn empty_req_ids_produces_empty_coverage_with_no_violation() {
        let mut designer = ScriptedDesigner::new("agent:designer", vec!["REQ-2".to_string()]);
        let assign = task_assign(&[]);

        let replies = designer.on_envelope(assign).await;

        assert_eq!(replies[1].body["covered_req_ids"], json!([]));
    }

    #[tokio::test]
    async fn unrelated_kind_produces_no_reply() {
        let mut designer = ScriptedDesigner::new("agent:designer", vec![]);
        let question = Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:pm".to_string(),
            vec!["agent:designer".to_string()],
            MessageKind::Question,
            None,
            "req_01".to_string(),
            json!({}),
            vec![],
            false,
            900_000,
        );

        let replies = designer.on_envelope(question).await;

        assert!(replies.is_empty());
    }

    #[test]
    fn is_done_is_always_false() {
        let designer = ScriptedDesigner::new("agent:designer", vec![]);
        assert!(!designer.is_done());
    }
}
