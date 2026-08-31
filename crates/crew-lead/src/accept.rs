//! Lead's accept/rework/escalate decision (plan D3, contracts-m3.md 층위
//! 규칙): a pure state machine over a task's [`DodVerdict`](crate::dod_exec::DodVerdict)
//! history — CorrGuard (bus layer, max_rounds=3) is a separate layer, so
//! Lead's own rework budget defaults to 2 to exhaust before CorrGuard would
//! block (M2 measured: one violation consumes 2 of CorrGuard's 3 rounds).

use crate::dod_exec::DodVerdict;

/// One round's outcome (plan D3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptDecision {
    Accept,
    Rework { violations: Vec<String> },
    Escalate { reason: String },
}

/// Per-task rework budget tracker (plan D3).
#[derive(Debug, Clone)]
pub struct AcceptanceLoop {
    rounds_used: u32,
    max_rework: u32,
}

impl AcceptanceLoop {
    /// contracts-m3.md 층위 규칙: Lead 기본 예산 ≤ 2.
    pub fn default_budget() -> u32 {
        2
    }

    pub fn new(max_rework: u32) -> Self {
        Self {
            rounds_used: 0,
            max_rework,
        }
    }

    pub fn rounds_used(&self) -> u32 {
        self.rounds_used
    }

    /// `passed` → `Accept`; otherwise consumes one rework round if the
    /// budget allows (`Rework` with `uncovered` prefixed `"REQ-"`,
    /// `missing_artifacts` prefixed `"artifact:"`, and `failed_cmds`
    /// (already `cmd:<run> — <reason>`-formatted by `dod_exec::judge`,
    /// M9 §G2d), concatenated), else `Escalate`.
    pub fn decide(&mut self, verdict: &DodVerdict) -> AcceptDecision {
        if verdict.passed {
            return AcceptDecision::Accept;
        }

        if self.rounds_used >= self.max_rework {
            return AcceptDecision::Escalate {
                reason: "rework budget exhausted".to_string(),
            };
        }

        self.rounds_used += 1;
        let violations = verdict
            .uncovered
            .iter()
            .cloned()
            .chain(
                verdict
                    .missing_artifacts
                    .iter()
                    .map(|name| format!("artifact:{name}")),
            )
            .chain(verdict.failed_cmds.iter().cloned())
            .collect();
        AcceptDecision::Rework { violations }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing_verdict() -> DodVerdict {
        DodVerdict {
            passed: true,
            uncovered: vec![],
            missing_artifacts: vec![],
            failed_cmds: vec![],
            skipped: vec![],
        }
    }

    fn failing_verdict() -> DodVerdict {
        DodVerdict {
            passed: false,
            uncovered: vec!["REQ-1".to_string()],
            missing_artifacts: vec!["spec.md".to_string()],
            failed_cmds: vec![],
            skipped: vec![],
        }
    }

    #[test]
    fn passed_verdict_accepts() {
        let mut loop_ = AcceptanceLoop::new(AcceptanceLoop::default_budget());

        let decision = loop_.decide(&passing_verdict());

        assert_eq!(decision, AcceptDecision::Accept);
        assert_eq!(loop_.rounds_used(), 0);
    }

    #[test]
    fn first_failure_requests_rework_with_prefixed_violations() {
        let mut loop_ = AcceptanceLoop::new(AcceptanceLoop::default_budget());

        let decision = loop_.decide(&failing_verdict());

        assert_eq!(
            decision,
            AcceptDecision::Rework {
                violations: vec!["REQ-1".to_string(), "artifact:spec.md".to_string()]
            }
        );
        assert_eq!(loop_.rounds_used(), 1);
    }

    #[test]
    fn budget_exhausted_after_default_two_rounds_escalates() {
        let mut loop_ = AcceptanceLoop::new(AcceptanceLoop::default_budget());

        loop_.decide(&failing_verdict());
        loop_.decide(&failing_verdict());
        let decision = loop_.decide(&failing_verdict());

        assert!(matches!(decision, AcceptDecision::Escalate { .. }));
        assert_eq!(loop_.rounds_used(), 2);
    }

    #[test]
    fn zero_max_rework_escalates_immediately() {
        let mut loop_ = AcceptanceLoop::new(0);

        let decision = loop_.decide(&failing_verdict());

        assert!(matches!(decision, AcceptDecision::Escalate { .. }));
        assert_eq!(loop_.rounds_used(), 0);
    }

    #[test]
    fn failed_cmd_alone_still_reworks_with_a_non_empty_diagnostic() {
        // Regression for review t-cmd-r1 F1: ReqCover/artifacts fully
        // satisfied, only a Cmd check failed — the Rework violations must
        // still carry the failure, not come back empty.
        let verdict = DodVerdict {
            passed: false,
            uncovered: vec![],
            missing_artifacts: vec![],
            failed_cmds: vec!["cmd:cargo test — exit 1 (expected 0)".to_string()],
            skipped: vec![],
        };
        let mut loop_ = AcceptanceLoop::new(AcceptanceLoop::default_budget());

        let decision = loop_.decide(&verdict);

        assert_eq!(
            decision,
            AcceptDecision::Rework {
                violations: vec!["cmd:cargo test — exit 1 (expected 0)".to_string()]
            }
        );
    }
}
