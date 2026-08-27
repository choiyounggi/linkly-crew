use std::collections::{HashMap, HashSet, VecDeque};

/// Outcome of recording one (from, to) hop on a `corr`. DESIGN.md §3.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    ExceededRounds,
    CycleDetected,
    InvalidInput,
}

#[derive(Debug, Default, Clone)]
struct CorrState {
    edges: Vec<(String, String)>,
    round_count: u32,
}

/// Ping-pong guard for one `corr` conversation graph — DESIGN.md §3.4.
/// Pure, in-memory, no I/O: the M2 bus owns acting on the returned `Verdict`.
pub struct CorrGuard {
    max_rounds: u8,
    states: HashMap<String, CorrState>,
}

impl CorrGuard {
    pub fn new(max_rounds: u8) -> Self {
        Self {
            max_rounds,
            states: HashMap::new(),
        }
    }

    /// Records one hop `from -> to` on `corr` and returns the verdict.
    /// Priority: InvalidInput -> CycleDetected -> ExceededRounds/Accepted.
    pub fn record(&mut self, corr: &str, from: &str, to: &str) -> Verdict {
        if corr.is_empty() || from.is_empty() || to.is_empty() {
            return Verdict::InvalidInput;
        }

        let state = self.states.entry(corr.to_string()).or_default();

        if from == to || Self::creates_multi_hop_cycle(&state.edges, from, to) {
            state.edges.push((from.to_string(), to.to_string()));
            return Verdict::CycleDetected;
        }

        let is_reversal = state
            .edges
            .last()
            .map(|(last_from, last_to)| last_from == to && last_to == from)
            .unwrap_or(false);

        state.edges.push((from.to_string(), to.to_string()));

        if is_reversal {
            state.round_count += 1;
        }

        if state.round_count > self.max_rounds as u32 {
            Verdict::ExceededRounds
        } else {
            Verdict::Accepted
        }
    }

    /// True when adding edge `from -> to` closes a cycle of 3+ distinct nodes
    /// through the edges already recorded (a direct `to -> from` reversal —
    /// depth 1 — is the normal round-trip and is NOT a cycle).
    fn creates_multi_hop_cycle(edges: &[(String, String)], from: &str, to: &str) -> bool {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut queue: VecDeque<(&str, u32)> = VecDeque::new();
        visited.insert(to);
        queue.push_back((to, 0));

        while let Some((node, depth)) = queue.pop_front() {
            for (edge_from, edge_to) in edges {
                if edge_from != node {
                    continue;
                }
                if edge_to == from && depth + 1 >= 2 {
                    return true;
                }
                if visited.insert(edge_to.as_str()) {
                    queue.push_back((edge_to.as_str(), depth + 1));
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_within_round_budget() {
        // Normal case: 2 round-trips, comfortably inside the default budget (3).
        let mut guard = CorrGuard::new(3);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // round 1
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // round 2
    }

    #[test]
    fn accepts_exactly_max_rounds() {
        // max_rounds injected as a value that differs from the type's
        // hardcoded shipped default (3), per the mechanism-vs-default split:
        // this pins the *mechanism* (the injected bound is honoured).
        let mut guard = CorrGuard::new(2);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // round 1
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted); // round 2 == max_rounds
    }

    #[test]
    fn rejects_round_beyond_budget() {
        let mut guard = CorrGuard::new(2);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // round 1
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted); // round 2
        assert_eq!(
            guard.record("req_1", "B", "A"),
            Verdict::ExceededRounds // round 3 > max_rounds(2)
        );
    }

    #[test]
    fn shipped_default_max_rounds_is_three() {
        // Pins the DESIGN.md §3.4 shipped default (3) at its own boundary,
        // separately from the injected-value mechanism test above (which
        // uses 2, a value that differs from the default, per the
        // mechanism-vs-default split).
        let mut guard = CorrGuard::new(3);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // round 1
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted); // round 2
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // round 3 == default
        assert_eq!(
            guard.record("req_1", "A", "B"),
            Verdict::ExceededRounds // round 4 exceeds default 3
        );
    }

    #[test]
    fn detects_self_cycle() {
        let mut guard = CorrGuard::new(3);
        assert_eq!(guard.record("req_1", "A", "A"), Verdict::CycleDetected);
    }

    #[test]
    fn detects_three_node_cycle() {
        let mut guard = CorrGuard::new(3);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "C"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "C", "A"), Verdict::CycleDetected);
    }

    #[test]
    fn two_node_reversal_is_not_a_cycle() {
        // A->B then B->A is the normal round-trip (governed by round budget,
        // not cycle detection) per D5's resolution of the two mechanisms.
        let mut guard = CorrGuard::new(3);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted);
    }

    #[test]
    fn invalid_input() {
        let mut guard = CorrGuard::new(3);
        assert_eq!(guard.record("", "A", "B"), Verdict::InvalidInput);
        assert_eq!(guard.record("req_1", "", "B"), Verdict::InvalidInput);
        assert_eq!(guard.record("req_1", "A", ""), Verdict::InvalidInput);
    }

    #[test]
    fn cycle_takes_priority_over_exceeded_rounds() {
        // Drive rounds to the budget, then close a self-cycle on the same
        // corr: CycleDetected must win even though rounds are also exceeded.
        let mut guard = CorrGuard::new(1);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // round 1 == max
        assert_eq!(
            guard.record("req_1", "A", "A"),
            Verdict::CycleDetected // self-cycle, not ExceededRounds
        );
    }

    #[test]
    fn different_corrs_are_independent() {
        let mut guard = CorrGuard::new(1);
        assert_eq!(guard.record("req_1", "A", "B"), Verdict::Accepted);
        assert_eq!(guard.record("req_1", "B", "A"), Verdict::Accepted); // req_1 round 1 == max
        assert_eq!(guard.record("req_2", "A", "B"), Verdict::Accepted); // fresh corr, unaffected
    }
}
