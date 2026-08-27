use serde::{Deserialize, Serialize};

use crate::envelope::Envelope;

/// Frames a connected agent sends to the bus over the WS wire —
/// DESIGN.md §3/§11 M2. Connection auth happens at the HTTP upgrade
/// (`Authorization: Bearer <token>` + `X-Crew-Agent: <agent_id>`), so there
/// is no `Hello` frame here. One WS Text frame carries exactly one JSON
/// object of this shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Envelope(Envelope),
    Receipt { id: String },
}

/// Frames the bus sends to a connected agent over the WS wire —
/// DESIGN.md §3/§11 M2. `Welcome` is the first frame the bus sends once a
/// connection's HTTP-upgrade auth has been accepted. `Error.code` is a
/// plain `String`, not an enum, so the bus can add codes without a
/// crew-proto release; documented values: `unauthorized`, `bad_frame`,
/// `unknown_recipient`, `loop_blocked`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Welcome { agent_id: String },
    Envelope(Envelope),
    Receipt { id: String },
    Error { code: String, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Envelope;
    use crate::kind::MessageKind;

    fn sample_envelope() -> Envelope {
        Envelope::new(
            "sp-3".to_string(),
            "th-1".to_string(),
            "agent:designer".to_string(),
            vec!["agent:publisher".to_string()],
            MessageKind::ChangeRequest,
            None,
            "req_01".to_string(),
            serde_json::json!({}),
            vec!["art:design/login-v2.png".to_string()],
            true,
            900000,
        )
    }

    #[test]
    fn client_frame_envelope_round_trips() {
        let frame = ClientFrame::Envelope(sample_envelope());
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "envelope");
        assert_eq!(json["kind"], "change_request");

        let back: ClientFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn client_frame_receipt_round_trips() {
        let frame = ClientFrame::Receipt {
            id: "msg_01".to_string(),
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "receipt");
        assert_eq!(json["id"], "msg_01");

        let back: ClientFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn server_frame_welcome_round_trips() {
        let frame = ServerFrame::Welcome {
            agent_id: "agent:designer".to_string(),
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "welcome");
        assert_eq!(json["agent_id"], "agent:designer");

        let back: ServerFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn server_frame_envelope_round_trips() {
        let frame = ServerFrame::Envelope(sample_envelope());
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "envelope");

        let back: ServerFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn server_frame_receipt_round_trips() {
        let frame = ServerFrame::Receipt {
            id: "msg_02".to_string(),
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "receipt");

        let back: ServerFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn server_frame_error_round_trips() {
        let frame = ServerFrame::Error {
            code: "unauthorized".to_string(),
            message: "missing bearer token".to_string(),
        };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["code"], "unauthorized");
        assert_eq!(json["message"], "missing bearer token");

        let back: ServerFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn unknown_client_frame_type_fails_to_deserialize() {
        let json = serde_json::json!({"type": "bogus", "id": "x"});
        let result: Result<ClientFrame, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_server_frame_type_fails_to_deserialize() {
        let json = serde_json::json!({"type": "bogus"});
        let result: Result<ServerFrame, _> = serde_json::from_value(json);
        assert!(result.is_err());
    }

    #[test]
    fn client_frame_envelope_with_empty_to_and_artifacts_round_trips() {
        // Boundary case: Envelope::to / Envelope::artifacts are `#[serde(default)]`
        // empty Vec<String> — confirm the frame wrapper preserves them as empty,
        // not absent, through a full serialize/deserialize round trip.
        let mut envelope = sample_envelope();
        envelope.to = Vec::new();
        envelope.artifacts = Vec::new();
        let frame = ClientFrame::Envelope(envelope);

        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["to"], serde_json::json!([]));
        assert_eq!(json["artifacts"], serde_json::json!([]));

        let back: ClientFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
        match back {
            ClientFrame::Envelope(env) => {
                assert!(env.to.is_empty());
                assert!(env.artifacts.is_empty());
            }
            _ => panic!("expected ClientFrame::Envelope"),
        }
    }

    #[test]
    fn receipt_with_empty_id_round_trips() {
        // Boundary case: empty string id is a well-formed (if useless) value —
        // the wire type itself does not validate content, only shape.
        let frame = ClientFrame::Receipt { id: String::new() };
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["id"], "");

        let back: ClientFrame = serde_json::from_value(json).unwrap();
        assert_eq!(back, frame);
    }
}
