//! `propose` decision for cross-session findings F1 (repeated command) and F2 (repeated
//! correction). The function is pure: state and config in, one [`Decision`] out.

use crate::config::DevtimeActionsConfig;
use crate::devtime::actions::Decision;
use crate::devtime::precision::{RulePrecision, action_eligible};

/// One cross-session finding, aggregated over the look-back window.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossCandidate {
    pub finding_key: String,
    pub rule_id: String,
    /// Distinct session count.
    pub sessions: i64,
    /// Cost accumulated in the window.
    pub cost_ms: i64,
    pub count: i64,
}

/// What `decide_propose` reads about one project.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposeState {
    pub project_id: String,
    /// `shadow` or `live`.
    pub mode: String,
    pub candidates: Vec<CrossCandidate>,
    /// Proposal rows already open in the project.
    pub open_proposals: i64,
    /// Finding keys that already have a `propose` row.
    pub already_proposed: Vec<String>,
    pub precisions: Vec<RulePrecision>,
}

/// PURE: decides `propose` for one cross finding.
pub fn decide_propose(
    state: &ProposeState,
    candidate: &CrossCandidate,
    cfg: &DevtimeActionsConfig,
    action_version: i64,
) -> Decision {
    let rule_id = candidate.rule_id.as_str();
    let precision = state.precisions.iter().find(|p| p.rule_id == rule_id);
    let eligible = precision.is_some_and(|p| action_eligible(p, cfg));
    // The closed reason list has no "duplicate": an already-proposed key reads as a cooldown.
    let suppress_reason = if !eligible {
        Some("not_eligible")
    } else if state.already_proposed.contains(&candidate.finding_key) {
        Some("cooldown")
    } else if state.open_proposals >= i64::from(cfg.propose_max_open) {
        Some("cap")
    } else if candidate.sessions < i64::from(cfg.propose_min_sessions)
        || candidate.cost_ms < i64::try_from(cfg.propose_min_cost_ms).unwrap_or(i64::MAX)
    {
        Some("below_threshold")
    } else {
        None
    };
    let decision = if suppress_reason.is_some() {
        "suppress"
    } else {
        "fire"
    };
    Decision {
        action: "propose".to_string(),
        rule_id: rule_id.to_string(),
        project_id: state.project_id.clone(),
        session_id: None,
        subject_key: candidate.finding_key.clone(),
        mode: state.mode.clone(),
        decision: decision.to_string(),
        suppress_reason: suppress_reason.map(str::to_string),
        basis_precision: precision.and_then(|p| p.precision),
        basis_cases: precision.map(|p| p.n_cases),
        basis_recent_count: Some(candidate.count),
        basis_recent_cost_ms: Some(candidate.cost_ms),
        expected_saving_ms: None,
        rule_version: crate::devtime::rules::rule(rule_id).map_or(1, |r| i64::from(r.version)),
        action_version,
    }
}

/// The `proposals.kind` a rule's proposal is filed under: F1 a script, F2 a gotcha.
pub fn proposal_kind(rule_id: &str) -> Option<&'static str> {
    match rule_id {
        "F1" => Some("devtime-script"),
        "F2" => Some("devtime-gotcha"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime::rules::Level;

    const VERSION: i64 = 2;

    fn precision(rule_id: &str, eligible: bool) -> RulePrecision {
        RulePrecision {
            rule_id: rule_id.to_string(),
            level: Level::Base,
            n_cases: 25,
            n_not_rework: 0,
            precision: Some(1.0),
            prior: 0.5,
            eligible,
        }
    }

    fn candidate(rule_id: &str, sessions: i64, cost_ms: i64) -> CrossCandidate {
        CrossCandidate {
            finding_key: format!("{rule_id}|k1"),
            rule_id: rule_id.to_string(),
            sessions,
            cost_ms,
            count: 7,
        }
    }

    fn state(rule_id: &str, eligible: bool, open: i64, proposed: &[&str]) -> ProposeState {
        ProposeState {
            project_id: "p1".to_string(),
            mode: "shadow".to_string(),
            candidates: vec![],
            open_proposals: open,
            already_proposed: proposed.iter().map(|s| (*s).to_string()).collect(),
            precisions: vec![precision(rule_id, eligible)],
        }
    }

    fn decide(st: &ProposeState, c: &CrossCandidate) -> Decision {
        decide_propose(st, c, &DevtimeActionsConfig::default(), VERSION)
    }

    fn assert_suppressed(d: &Decision, reason: &str) {
        assert_eq!(d.decision, "suppress");
        assert_eq!(d.suppress_reason.as_deref(), Some(reason));
    }

    // Defaults: propose_min_sessions 3, propose_min_cost_ms 600_000, propose_max_open 5.

    fn check_fires(rule_id: &str) {
        let c = candidate(rule_id, 3, 600_000);
        let d = decide(&state(rule_id, true, 0, &[]), &c);
        assert_eq!(d.decision, "fire");
        assert_eq!(d.suppress_reason, None);
        assert_eq!(d.action, "propose");
        assert_eq!(d.rule_id, rule_id);
        assert_eq!(d.project_id, "p1");
        assert_eq!(d.session_id, None);
        assert_eq!(d.subject_key, c.finding_key);
        assert_eq!(d.mode, "shadow");
        assert_eq!(d.action_version, VERSION);
        assert_eq!(d.basis_cases, Some(25));
        assert_eq!(d.basis_precision, Some(1.0));
        assert_eq!(d.basis_recent_count, Some(7));
        assert_eq!(d.basis_recent_cost_ms, Some(600_000));
        // Chosen: a proposal has no a-priori saving estimate.
        assert_eq!(d.expected_saving_ms, None);
    }

    #[test]
    fn f1_fires_when_sessions_cost_and_eligibility_hold() {
        check_fires("F1");
    }

    #[test]
    fn f2_fires_when_sessions_cost_and_eligibility_hold() {
        check_fires("F2");
    }

    #[test]
    fn below_min_sessions_is_suppressed_below_threshold() {
        let d = decide(&state("F1", true, 0, &[]), &candidate("F1", 2, 5_000_000));
        assert_suppressed(&d, "below_threshold");
    }

    #[test]
    fn below_min_cost_is_suppressed_below_threshold() {
        let d = decide(&state("F1", true, 0, &[]), &candidate("F1", 9, 599_999));
        assert_suppressed(&d, "below_threshold");
    }

    #[test]
    fn already_proposed_key_is_suppressed_as_cooldown() {
        // The closed SUPPRESS_REASONS list has no "duplicate"; "cooldown" fits best.
        let c = candidate("F2", 4, 900_000);
        let d = decide(&state("F2", true, 0, &["F2|k1"]), &c);
        assert_suppressed(&d, "cooldown");
    }

    #[test]
    fn other_proposed_key_does_not_block() {
        let c = candidate("F2", 4, 900_000);
        let d = decide(&state("F2", true, 0, &["F2|other"]), &c);
        assert_eq!(d.decision, "fire");
    }

    #[test]
    fn open_proposals_at_cap_are_suppressed_cap() {
        let d = decide(&state("F1", true, 5, &[]), &candidate("F1", 4, 900_000));
        assert_suppressed(&d, "cap");
    }

    #[test]
    fn four_open_proposals_still_fire() {
        let d = decide(&state("F1", true, 4, &[]), &candidate("F1", 4, 900_000));
        assert_eq!(d.decision, "fire");
        assert_eq!(d.suppress_reason, None);
    }

    #[test]
    fn ineligible_rule_is_suppressed_not_eligible() {
        let d = decide(&state("F1", false, 0, &[]), &candidate("F1", 4, 900_000));
        assert_suppressed(&d, "not_eligible");
    }

    #[test]
    fn unknown_rule_is_suppressed_not_eligible() {
        let mut st = state("F1", true, 0, &[]);
        st.precisions.clear();
        let d = decide(&st, &candidate("F1", 4, 900_000));
        assert_suppressed(&d, "not_eligible");
    }

    #[test]
    fn not_eligible_wins_over_below_threshold() {
        let d = decide(&state("F1", false, 0, &[]), &candidate("F1", 1, 1));
        assert_suppressed(&d, "not_eligible");
    }

    #[test]
    fn proposal_kind_maps_f1_f2_and_nothing_else() {
        assert_eq!(proposal_kind("F1"), Some("devtime-script"));
        assert_eq!(proposal_kind("F2"), Some("devtime-gotcha"));
        assert_eq!(proposal_kind("D3"), None);
        assert_eq!(proposal_kind("E2"), None);
    }

    #[test]
    fn decision_is_deterministic_and_carries_mode_and_version() {
        let mut st = state("F1", true, 0, &[]);
        st.mode = "live".to_string();
        let c = candidate("F1", 4, 900_000);
        let a = decide_propose(&st, &c, &DevtimeActionsConfig::default(), 9);
        let b = decide_propose(&st, &c, &DevtimeActionsConfig::default(), 9);
        assert_eq!(a, b);
        assert_eq!(a.mode, "live");
        assert_eq!(a.action_version, 9);
    }
}
