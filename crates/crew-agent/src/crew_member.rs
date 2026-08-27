use async_trait::async_trait;
use crew_proto::{ArtifactContract, DodCheck, Envelope, MessageKind, Role, TaskSpec};
use serde_json::{json, Value};

use crate::role::RoleBehavior;

const DEADLINE_MS: u64 = 900_000;

/// Generic scripted crew member usable for any of the 5 M3 roles (plan D1):
/// role differences are carried entirely by the assigned `TaskSpec`'s
/// `artifacts_expected`/`dod` contracts, so one struct configured with a
/// `Role` covers pm/designer/publisher/developer/qa (Simplicity First —
/// instructions.md). Follows the `ScriptedDesigner` planted-violation
/// pattern generalized to the M3 body contract (contracts-m3.md): on the
/// first `task.assign` it omits `planted_violations` from coverage; on the
/// following `change_request` it reports full coverage.
pub struct ScriptedCrewMember {
    agent_id: String,
    role: Role,
    planted_violations: Vec<String>,
    violation_planted: bool,
    current_task: Option<TaskSpec>,
}

impl ScriptedCrewMember {
    pub fn new(agent_id: impl Into<String>, role: Role, planted_violations: Vec<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            role,
            planted_violations,
            violation_planted: false,
            current_task: None,
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

    /// Coverage target (contracts-m3.md): the union of every `ReqCover.ids`
    /// in `task.dod`.
    fn target_req_ids(task: &TaskSpec) -> Vec<String> {
        let mut ids = Vec::new();
        for check in &task.dod {
            if let DodCheck::ReqCover { ids: check_ids } = check {
                for id in check_ids {
                    let id = id.as_str().to_string();
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        ids
    }

    /// One artifact per `task.artifacts_expected` entry (contracts-m3.md):
    /// name/kind/req_ids copied verbatim, content always non-empty.
    fn build_artifacts(&self, task: &TaskSpec) -> Vec<Value> {
        task.artifacts_expected
            .iter()
            .map(|contract: &ArtifactContract| {
                let req_ids: Vec<String> = contract
                    .req_ids
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
                let content = format!(
                    "[{:?}] {} — covers {}",
                    self.role,
                    contract.name,
                    req_ids.join(", ")
                );
                json!({
                    "name": contract.name,
                    "kind": contract.kind,
                    "req_ids": req_ids,
                    "content": content,
                })
            })
            .collect()
    }

    fn ack_and_result(
        &self,
        in_reply_to: &Envelope,
        task: &TaskSpec,
        covered: Vec<String>,
    ) -> Vec<Envelope> {
        let ack = self.reply(in_reply_to, MessageKind::TaskAck, json!({}));
        let artifacts = self.build_artifacts(task);
        let result = self.reply(
            in_reply_to,
            MessageKind::TaskResult,
            json!({"covered_req_ids": covered, "artifacts": artifacts}),
        );
        vec![ack, result]
    }
}

#[async_trait]
impl RoleBehavior for ScriptedCrewMember {
    async fn on_envelope(&mut self, env: Envelope) -> Vec<Envelope> {
        match env.kind {
            MessageKind::TaskAssign => {
                let task: TaskSpec = match env.body.get("task") {
                    Some(value) => match serde_json::from_value(value.clone()) {
                        Ok(task) => task,
                        Err(err) => {
                            return vec![self.reply(
                                &env,
                                MessageKind::Blocked,
                                json!({"reason": format!("invalid \"task\" body: {err}")}),
                            )]
                        }
                    },
                    None => {
                        return vec![self.reply(
                            &env,
                            MessageKind::Blocked,
                            json!({"reason": "missing \"task\" field in body"}),
                        )]
                    }
                };

                let target = Self::target_req_ids(&task);
                let covered = if self.violation_planted {
                    target
                } else {
                    self.violation_planted = true;
                    target
                        .into_iter()
                        .filter(|id| !self.planted_violations.contains(id))
                        .collect()
                };
                let replies = self.ack_and_result(&env, &task, covered);
                self.current_task = Some(task);
                replies
            }
            MessageKind::ChangeRequest => match &self.current_task {
                Some(task) => {
                    let covered = Self::target_req_ids(task);
                    self.ack_and_result(&env, task, covered)
                }
                None => vec![],
            },
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
    use crew_proto::ReqId;

    fn task_spec(
        id: &str,
        role: Role,
        req_id_groups: Vec<Vec<&str>>,
        artifacts: Vec<(&str, &str, Vec<&str>)>,
    ) -> TaskSpec {
        let dod = req_id_groups
            .into_iter()
            .map(|group| DodCheck::ReqCover {
                ids: group.into_iter().map(|s| ReqId::new(s).unwrap()).collect(),
            })
            .collect();
        let artifacts_expected = artifacts
            .into_iter()
            .map(|(name, kind, req_ids)| ArtifactContract {
                name: name.to_string(),
                kind: kind.to_string(),
                req_ids: req_ids.into_iter().map(|s| ReqId::new(s).unwrap()).collect(),
            })
            .collect();
        TaskSpec {
            id: id.to_string(),
            role,
            title: format!("task {id}"),
            brief: "brief".to_string(),
            dod,
            deps: vec![],
            artifacts_expected,
        }
    }

    fn task_assign(task: &TaskSpec) -> Envelope {
        Envelope::new(
            "sp-m3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:worker".to_string()],
            MessageKind::TaskAssign,
            None,
            "req_01".to_string(),
            json!({"task": task}),
            vec![],
            true,
            900_000,
        )
    }

    fn change_request(in_reply_to: &Envelope, violations: &[&str]) -> Envelope {
        Envelope::new(
            in_reply_to.sprint.clone(),
            in_reply_to.thread.clone(),
            "agent:lead".to_string(),
            vec!["agent:worker".to_string()],
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
    async fn first_assign_covers_all_reqs_with_artifacts_matching_contract() {
        let mut member = ScriptedCrewMember::new("agent:worker", Role::Pm, vec![]);
        let task = task_spec(
            "t1",
            Role::Pm,
            vec![vec!["REQ-1", "REQ-2"]],
            vec![("plan.md", "doc", vec!["REQ-1", "REQ-2"])],
        );
        let assign = task_assign(&task);
        let assign_id = assign.id.clone();

        let replies = member.on_envelope(assign).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].kind, MessageKind::TaskAck);
        assert_eq!(replies[0].in_reply_to, Some(assign_id.clone()));
        assert_eq!(replies[1].kind, MessageKind::TaskResult);
        assert_eq!(replies[1].in_reply_to, Some(assign_id));
        assert_eq!(
            replies[1].body["covered_req_ids"],
            json!(["REQ-1", "REQ-2"])
        );
        let artifacts = replies[1].body["artifacts"].as_array().unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0]["name"], json!("plan.md"));
        assert_eq!(artifacts[0]["kind"], json!("doc"));
        assert_eq!(artifacts[0]["req_ids"], json!(["REQ-1", "REQ-2"]));
        assert!(!artifacts[0]["content"].as_str().unwrap().is_empty());
        assert!(replies[1].requires_ack);
    }

    #[tokio::test]
    async fn planted_violation_is_omitted_once_then_fully_covered_after_rework() {
        let mut member =
            ScriptedCrewMember::new("agent:worker", Role::Designer, vec!["REQ-2".to_string()]);
        let task = task_spec(
            "t1",
            Role::Designer,
            vec![vec!["REQ-1", "REQ-2", "REQ-3"]],
            vec![],
        );
        let assign = task_assign(&task);

        let first = member.on_envelope(assign.clone()).await;
        assert_eq!(first[1].body["covered_req_ids"], json!(["REQ-1", "REQ-3"]));

        let cr = change_request(&assign, &["REQ-2"]);
        let second = member.on_envelope(cr).await;

        assert_eq!(second.len(), 2);
        assert_eq!(second[1].kind, MessageKind::TaskResult);
        assert_eq!(
            second[1].body["covered_req_ids"],
            json!(["REQ-1", "REQ-2", "REQ-3"])
        );
    }

    #[tokio::test]
    async fn dod_without_req_cover_produces_empty_coverage() {
        let mut member = ScriptedCrewMember::new("agent:worker", Role::Qa, vec![]);
        let task = task_spec("t1", Role::Qa, vec![], vec![]);
        let assign = task_assign(&task);

        let replies = member.on_envelope(assign).await;

        assert_eq!(replies[1].body["covered_req_ids"], json!([]));
    }

    #[tokio::test]
    async fn empty_artifacts_expected_produces_empty_artifacts() {
        let mut member = ScriptedCrewMember::new("agent:worker", Role::Publisher, vec![]);
        let task = task_spec("t1", Role::Publisher, vec![vec!["REQ-1"]], vec![]);
        let assign = task_assign(&task);

        let replies = member.on_envelope(assign).await;

        assert_eq!(replies[1].body["artifacts"], json!([]));
    }

    #[tokio::test]
    async fn missing_task_field_replies_blocked() {
        let mut member = ScriptedCrewMember::new("agent:worker", Role::Developer, vec![]);
        let assign = Envelope::new(
            "sp-m3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:worker".to_string()],
            MessageKind::TaskAssign,
            None,
            "req_01".to_string(),
            json!({}),
            vec![],
            true,
            900_000,
        );
        let assign_id = assign.id.clone();

        let replies = member.on_envelope(assign).await;

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::Blocked);
        assert_eq!(replies[0].in_reply_to, Some(assign_id));
        assert!(!replies[0].body["reason"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn malformed_task_field_replies_blocked() {
        let mut member = ScriptedCrewMember::new("agent:worker", Role::Developer, vec![]);
        let assign = Envelope::new(
            "sp-m3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:worker".to_string()],
            MessageKind::TaskAssign,
            None,
            "req_01".to_string(),
            json!({"task": "not a task spec"}),
            vec![],
            true,
            900_000,
        );

        let replies = member.on_envelope(assign).await;

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].kind, MessageKind::Blocked);
        assert!(!replies[0].body["reason"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unrelated_kind_produces_no_reply() {
        let mut member = ScriptedCrewMember::new("agent:worker", Role::Pm, vec![]);
        let question = Envelope::new(
            "sp-m3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:worker".to_string()],
            MessageKind::Question,
            None,
            "req_01".to_string(),
            json!({}),
            vec![],
            false,
            900_000,
        );

        let replies = member.on_envelope(question).await;

        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn change_request_without_prior_assign_produces_no_reply() {
        let mut member = ScriptedCrewMember::new("agent:worker", Role::Designer, vec![]);
        let cr = Envelope::new(
            "sp-m3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:worker".to_string()],
            MessageKind::ChangeRequest,
            None,
            "req_01".to_string(),
            json!({"violations": ["REQ-1"], "reason": "x"}),
            vec![],
            true,
            900_000,
        );

        let replies = member.on_envelope(cr).await;

        assert!(replies.is_empty());
    }

    #[test]
    fn instantiable_with_all_five_roles() {
        for role in [
            Role::Pm,
            Role::Designer,
            Role::Publisher,
            Role::Developer,
            Role::Qa,
        ] {
            let member = ScriptedCrewMember::new("agent:worker", role, vec![]);
            assert!(!member.is_done());
        }
    }

    #[test]
    fn is_done_is_always_false() {
        let member = ScriptedCrewMember::new("agent:worker", Role::Qa, vec![]);
        assert!(!member.is_done());
    }
}
