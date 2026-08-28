//! RunEvent JSON contract — Rust/TS shared shape, contract §C3
//! ("RunEvent — Rust·TS 공유 JSON 계약", serde `tag = "type"`, snake_case).

use crew_proto::{Envelope, SpecDoc, TaskDag};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    RunStarted {
        run_id: String,
        goal: String,
        ts: String,
    },
    SpecReady {
        spec: SpecDoc,
        dag: TaskDag,
        sprint: Vec<String>,
        ts: String,
    },
    /// `seq` is the `messages` table seq (contract §C3 seq-space rule) —
    /// obtained via `EventLedger::messages_since`, never `append`'s
    /// return value.
    Message {
        seq: i64,
        envelope: Envelope,
    },
    TaskStateChanged {
        task_id: String,
        state: TaskStateDto,
        ts: String,
    },
    /// `seq` is the `events` table seq (contract §C3 seq-space rule) —
    /// `EventLedger::append`'s return value. A different space than
    /// `Message.seq`; the two must never be compared.
    BusLifecycle {
        seq: i64,
        kind: String,
        payload: serde_json::Value,
    },
    RunFinished {
        outcome: RunOutcomeDto,
        ts: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStateDto {
    Pending,
    Assigned,
    Accepted,
    Escalated,
}

impl From<crew_lead::dispatch::TaskState> for TaskStateDto {
    fn from(state: crew_lead::dispatch::TaskState) -> Self {
        match state {
            crew_lead::dispatch::TaskState::Pending => TaskStateDto::Pending,
            crew_lead::dispatch::TaskState::Assigned => TaskStateDto::Assigned,
            crew_lead::dispatch::TaskState::Accepted => TaskStateDto::Accepted,
            crew_lead::dispatch::TaskState::Escalated => TaskStateDto::Escalated,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcomeDto {
    Completed,
    Failed,
}

/// A `messages` row for `RunSnapshot` restoration — contract §C3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessageDto {
    pub seq: i64,
    pub envelope: Envelope,
}

/// Current run state for reconnect/app-restart, contract §C3. `last_seq` is
/// the `messages` table seq (same space as `RunEvent::Message.seq`) — a
/// reconnecting subscriber can resume via `messages_since(last_seq)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSnapshot {
    pub run_id: String,
    pub goal: String,
    pub spec: Option<SpecDoc>,
    pub dag: Option<TaskDag>,
    pub sprint: Vec<String>,
    pub task_states: Vec<(String, TaskStateDto)>,
    pub messages: Vec<StoredMessageDto>,
    pub last_seq: i64,
    pub ts: String,
}

/// RFC3339 UTC "now" used for every `RunEvent`'s `ts` field — same format
/// `crew_proto::Envelope::new` uses for its own `ts`.
pub fn now_ts() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC3339 formatting of a valid OffsetDateTime cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_envelope() -> Envelope {
        Envelope::new(
            "sp-m3".to_string(),
            "th-1".to_string(),
            "agent:lead".to_string(),
            vec!["agent:pm".to_string()],
            crew_proto::MessageKind::TaskAssign,
            None,
            "corr-t-pm".to_string(),
            json!({}),
            vec![],
            true,
            900_000,
        )
    }

    #[test]
    fn run_started_serializes_with_snake_case_type_tag() {
        let ev = RunEvent::RunStarted {
            run_id: "run-1".to_string(),
            goal: "goal".to_string(),
            ts: "2026-08-28T00:00:00Z".to_string(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "run_started");
        assert_eq!(json["run_id"], "run-1");
    }

    #[test]
    fn message_seq_and_envelope_round_trip() {
        let original = sample_envelope();
        let ev = RunEvent::Message {
            seq: 3,
            envelope: original.clone(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "message");
        assert_eq!(json["seq"], 3);
        assert_eq!(json["envelope"]["kind"], "task.assign");
        let back: RunEvent = serde_json::from_value(json).unwrap();
        match back {
            RunEvent::Message { seq, envelope } => {
                assert_eq!(seq, 3);
                assert_eq!(envelope.id, original.id);
            }
            other => panic!("expected Message, got {other:?}"),
        }
    }

    #[test]
    fn task_state_changed_uses_snake_case_state_values() {
        let ev = RunEvent::TaskStateChanged {
            task_id: "t-pm".to_string(),
            state: TaskStateDto::Accepted,
            ts: "2026-08-28T00:00:00Z".to_string(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "task_state_changed");
        assert_eq!(json["state"], "accepted");
    }

    #[test]
    fn bus_lifecycle_carries_kind_and_arbitrary_payload() {
        let ev = RunEvent::BusLifecycle {
            seq: 7,
            kind: "Registered".to_string(),
            payload: json!({"agent_id": "agent:pm"}),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "bus_lifecycle");
        assert_eq!(json["seq"], 7);
        assert_eq!(json["kind"], "Registered");
        assert_eq!(json["payload"]["agent_id"], "agent:pm");
    }

    #[test]
    fn run_finished_outcome_serializes_snake_case() {
        let ev = RunEvent::RunFinished {
            outcome: RunOutcomeDto::Completed,
            ts: "2026-08-28T00:00:00Z".to_string(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "run_finished");
        assert_eq!(json["outcome"], "completed");

        let failed = serde_json::to_value(&RunEvent::RunFinished {
            outcome: RunOutcomeDto::Failed,
            ts: "2026-08-28T00:00:00Z".to_string(),
        })
        .unwrap();
        assert_eq!(failed["outcome"], "failed");
    }

    #[test]
    fn task_state_dto_from_lead_task_state_maps_every_variant() {
        assert_eq!(
            TaskStateDto::from(crew_lead::dispatch::TaskState::Pending),
            TaskStateDto::Pending
        );
        assert_eq!(
            TaskStateDto::from(crew_lead::dispatch::TaskState::Assigned),
            TaskStateDto::Assigned
        );
        assert_eq!(
            TaskStateDto::from(crew_lead::dispatch::TaskState::Accepted),
            TaskStateDto::Accepted
        );
        assert_eq!(
            TaskStateDto::from(crew_lead::dispatch::TaskState::Escalated),
            TaskStateDto::Escalated
        );
    }

    #[test]
    fn run_snapshot_serializes_all_c3_fields() {
        let snap = RunSnapshot {
            run_id: "run-1".to_string(),
            goal: "goal".to_string(),
            spec: None,
            dag: None,
            sprint: vec!["t-pm".to_string()],
            task_states: vec![("t-pm".to_string(), TaskStateDto::Pending)],
            messages: vec![StoredMessageDto {
                seq: 1,
                envelope: sample_envelope(),
            }],
            last_seq: 1,
            ts: "2026-08-28T00:00:00Z".to_string(),
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["run_id"], "run-1");
        assert_eq!(json["last_seq"], 1);
        assert_eq!(json["messages"][0]["seq"], 1);
        assert!(json["spec"].is_null());
    }
}
