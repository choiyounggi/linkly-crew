use serde::{Deserialize, Serialize};

/// One agent slot in a roster (contracts-m5.md C1, verbatim). `role` is a
/// plain string ("lead" | "pm" | "designer" | "publisher" | "developer" |
/// "qa") — `crate::dag::Role` has no `Lead` variant, so it isn't reused here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RosterAgent {
    pub id: String,
    pub role: String,
    pub harness: String,
    pub model: String,
    pub instructions: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Roster {
    pub agents: Vec<RosterAgent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_agent(id: &str, role: &str) -> RosterAgent {
        RosterAgent {
            id: id.to_string(),
            role: role.to_string(),
            harness: "claude-code".to_string(),
            model: "sonnet".to_string(),
            instructions: "be helpful".to_string(),
        }
    }

    #[test]
    fn roster_round_trips_through_serde() {
        let roster = Roster {
            agents: vec![sample_agent("a1", "lead"), sample_agent("a2", "developer")],
        };
        let json = serde_json::to_string(&roster).unwrap();
        let back: Roster = serde_json::from_str(&json).unwrap();
        assert_eq!(back, roster);
    }

    #[test]
    fn empty_agents_vector_round_trips_as_boundary() {
        let roster = Roster { agents: vec![] };
        let json = serde_json::to_string(&roster).unwrap();
        let back: Roster = serde_json::from_str(&json).unwrap();
        assert_eq!(back, roster);
        assert_eq!(json, r#"{"agents":[]}"#);
    }

    #[test]
    fn missing_required_field_fails_to_deserialize() {
        let json = r#"{"id":"a1","role":"lead","harness":"claude-code","model":"sonnet"}"#;
        let err = serde_json::from_str::<RosterAgent>(json).unwrap_err();
        assert!(err.to_string().contains("instructions"));
    }
}
