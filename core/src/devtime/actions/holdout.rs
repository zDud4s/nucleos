//! The residual control group of a live pair (actions design spec, section 6.1). A deterministic
//! hash of the decision's identity picks `holdout_fraction` of the live `fire`s; those are recorded
//! as `mode = 'shadow'`, `decision = 'fire'`, `suppress_reason = 'holdout'` and never delivered.
//! Rollback reads them back as the control. Pure: no clock, no randomness, no SQL.

use super::Decision;
use sha2::{Digest, Sha256};

/// PURE: true when this decision falls in the holdout. The same inputs always give the same answer,
/// so a recompute of a session never flips a decision in or out of the control group.
pub fn is_held_out(d: &Decision, fraction: f64) -> bool {
    if fraction.is_nan() || fraction <= 0.0 {
        return false;
    }
    if fraction >= 1.0 {
        return true;
    }
    let mut h = Sha256::new();
    for part in [
        d.action.as_str(),
        d.rule_id.as_str(),
        d.project_id.as_str(),
        d.subject_key.as_str(),
        d.session_id.as_deref().unwrap_or(""),
    ] {
        h.update(part.as_bytes());
        h.update([0_u8]);
    }
    let digest = h.finalize();
    let mut first = [0_u8; 8];
    first.copy_from_slice(&digest[..8]);
    // Top 53 bits to a uniform value in [0, 1).
    let unit = (u64::from_be_bytes(first) >> 11) as f64 / (1_u64 << 53) as f64;
    unit < fraction
}

/// PURE: turns a live `fire` that the hash selects into a shadow `fire` marked `holdout`. Shadow
/// decisions and suppressed ones pass untouched: the control group only costs delivered fires.
/// `decide_inject` calls it itself; `run_for_session` calls it on each `decide_propose` result.
pub fn apply_holdout(mut d: Decision, fraction: f64) -> Decision {
    if d.mode == "live" && d.decision == "fire" && is_held_out(&d, fraction) {
        d.mode = "shadow".to_string();
        d.suppress_reason = Some("holdout".to_string());
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DevtimeActionsConfig;
    use crate::devtime::actions::propose::{CrossCandidate, ProposeState, decide_propose};
    use crate::devtime::actions::{ProjectState, RecentFinding, decide_inject};
    use crate::devtime::precision::RulePrecision;
    use crate::devtime::rules::Level;

    fn dec(mode: &str, decision: &str, n: usize) -> Decision {
        Decision {
            action: "inject".into(),
            rule_id: "D3".into(),
            project_id: "p".into(),
            session_id: Some(format!("s{n}")),
            subject_key: "D3|p".into(),
            mode: mode.into(),
            decision: decision.into(),
            suppress_reason: (decision == "suppress").then(|| "cap".to_string()),
            basis_precision: None,
            basis_cases: None,
            basis_recent_count: None,
            basis_recent_cost_ms: None,
            expected_saving_ms: None,
            rule_version: 1,
            action_version: 1,
        }
    }

    #[test]
    fn zero_never_and_one_always() {
        let d = dec("live", "fire", 1);
        assert!(!is_held_out(&d, 0.0));
        assert!(is_held_out(&d, 1.0));
    }

    #[test]
    fn same_decision_same_answer() {
        let d = dec("live", "fire", 7);
        assert_eq!(is_held_out(&d, 0.5), is_held_out(&d.clone(), 0.5));
    }

    #[test]
    fn about_ten_percent_are_held_out() {
        let n = (0..4000)
            .filter(|&i| is_held_out(&dec("live", "fire", i), 0.10))
            .count();
        assert!((300..=500).contains(&n), "held out {n} of 4000");
    }

    #[test]
    fn a_held_out_live_fire_becomes_a_shadow_holdout_fire() {
        let d = apply_holdout(dec("live", "fire", 1), 1.0);
        assert_eq!(d.mode, "shadow");
        assert_eq!(d.decision, "fire");
        assert_eq!(d.suppress_reason.as_deref(), Some("holdout"));
    }

    #[test]
    fn shadow_and_suppressed_decisions_are_untouched() {
        for d in [dec("shadow", "fire", 1), dec("live", "suppress", 1)] {
            assert_eq!(apply_holdout(d.clone(), 1.0), d);
        }
    }

    #[test]
    fn decide_inject_applies_the_holdout_to_a_live_fire() {
        let cfg = DevtimeActionsConfig {
            holdout_fraction: 1.0,
            ..Default::default()
        };
        let state = ProjectState {
            project_id: "p".into(),
            session_id: Some("s".into()),
            mode: "live".into(),
            recent: (0..5)
                .map(|_| RecentFinding {
                    rule_id: "D3".into(),
                    cost_ms: 600_000,
                })
                .collect(),
            precisions: vec![precision("D3")],
        };
        let d = decide_inject(&state, "D3", &cfg, 1);
        assert_eq!(d.decision, "fire");
        assert_eq!(d.mode, "shadow");
        assert_eq!(d.suppress_reason.as_deref(), Some("holdout"));
    }

    fn precision(rule_id: &str) -> RulePrecision {
        RulePrecision {
            rule_id: rule_id.into(),
            level: Level::Base,
            n_cases: 25,
            n_not_rework: 0,
            precision: Some(1.0),
            prior: 0.5,
            eligible: true,
        }
    }

    #[test]
    fn decide_propose_applies_the_holdout_to_a_live_fire() {
        let cfg = DevtimeActionsConfig {
            holdout_fraction: 1.0,
            ..Default::default()
        };
        let state = ProposeState {
            project_id: "p".into(),
            mode: "live".into(),
            candidates: vec![],
            open_proposals: 0,
            already_proposed: vec![],
            precisions: vec![precision("F1")],
        };
        let c = CrossCandidate {
            finding_key: "F1|k".into(),
            rule_id: "F1".into(),
            sessions: 5,
            cost_ms: 9_000_000,
            count: 7,
        };
        let d = apply_holdout(decide_propose(&state, &c, &cfg, 1), cfg.holdout_fraction);
        assert_eq!(d.decision, "fire");
        assert_eq!(d.mode, "shadow");
        assert_eq!(d.suppress_reason.as_deref(), Some("holdout"));
    }
}
