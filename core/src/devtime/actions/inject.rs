//! `inject` fixtures for the pure decision (D3 search bursts and D6 long foreground commands).
//! The function itself, [`super::decide_inject`], lives in the parent; this module holds its tests.

#[cfg(test)]
mod tests {
    use crate::config::DevtimeActionsConfig;
    use crate::devtime::actions::{Decision, ProjectState, RecentFinding, decide_inject};
    use crate::devtime::precision::RulePrecision;
    use crate::devtime::rules::Level;

    const VERSION: i64 = 1;

    fn precision(rule_id: &str, eligible: bool) -> RulePrecision {
        // 25 cases, all right: Wilson lower bound ~0.87, above the 0.6 default floor.
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

    fn state(rule_id: &str, costs: &[i64], eligible: bool) -> ProjectState {
        ProjectState {
            project_id: "p1".to_string(),
            session_id: Some("s-new".to_string()),
            mode: "shadow".to_string(),
            recent: costs
                .iter()
                .map(|cost_ms| RecentFinding {
                    rule_id: rule_id.to_string(),
                    cost_ms: *cost_ms,
                })
                .collect(),
            precisions: vec![precision(rule_id, eligible)],
        }
    }

    fn decide(rule_id: &str, costs: &[i64], eligible: bool) -> Decision {
        decide_inject(
            &state(rule_id, costs, eligible),
            rule_id,
            &DevtimeActionsConfig::default(),
            VERSION,
        )
    }

    // Defaults: inject_min_count 3, inject_min_cost_ms 120_000.

    fn check_fires(rule_id: &str) {
        let d = decide(rule_id, &[60_000, 120_000, 180_000], true);
        assert_eq!(d.decision, "fire");
        assert_eq!(d.suppress_reason, None);
        assert_eq!(d.action, "inject");
        assert_eq!(d.rule_id, rule_id);
        assert_eq!(d.project_id, "p1");
        assert_eq!(d.session_id.as_deref(), Some("s-new"));
        assert_eq!(d.mode, "shadow");
        assert_eq!(d.action_version, VERSION);
        assert_eq!(d.basis_recent_count, Some(3));
        assert_eq!(d.basis_recent_cost_ms, Some(360_000));
        assert_eq!(d.basis_cases, Some(25));
    }

    fn check_no_findings(rule_id: &str) {
        let d = decide(rule_id, &[], true);
        assert_eq!(d.decision, "suppress");
        assert_eq!(d.suppress_reason.as_deref(), Some("below_threshold"));
    }

    fn check_below_count(rule_id: &str) {
        // Two findings, each far above the cost floor: only the count is short.
        let d = decide(rule_id, &[200_000, 200_000], true);
        assert_eq!(d.decision, "suppress");
        assert_eq!(d.suppress_reason.as_deref(), Some("below_threshold"));
        assert_eq!(d.basis_recent_count, Some(2));
    }

    fn check_below_cost(rule_id: &str) {
        // Three findings but only 30 s lost in total: the count is met, the cost is not.
        let d = decide(rule_id, &[10_000, 10_000, 10_000], true);
        assert_eq!(d.decision, "suppress");
        assert_eq!(d.suppress_reason.as_deref(), Some("below_threshold"));
        assert_eq!(d.basis_recent_cost_ms, Some(30_000));
    }

    fn check_not_eligible(rule_id: &str) {
        // Counts and cost would fire; the rule's precision is not eligible.
        let d = decide(rule_id, &[60_000, 120_000, 180_000], false);
        assert_eq!(d.decision, "suppress");
        assert_eq!(d.suppress_reason.as_deref(), Some("not_eligible"));
    }

    #[test]
    fn d3_fires_when_count_cost_and_eligibility_hold() {
        check_fires("D3");
    }

    #[test]
    fn d3_without_findings_is_suppressed_below_threshold() {
        check_no_findings("D3");
    }

    #[test]
    fn d3_below_inject_min_count_is_suppressed_below_threshold() {
        check_below_count("D3");
    }

    #[test]
    fn d3_below_inject_min_cost_ms_is_suppressed_below_threshold() {
        check_below_cost("D3");
    }

    #[test]
    fn d3_ineligible_rule_is_suppressed_not_eligible() {
        check_not_eligible("D3");
    }

    #[test]
    fn d6_fires_when_count_cost_and_eligibility_hold() {
        check_fires("D6");
    }

    #[test]
    fn d6_without_findings_is_suppressed_below_threshold() {
        check_no_findings("D6");
    }

    #[test]
    fn d6_below_inject_min_count_is_suppressed_below_threshold() {
        check_below_count("D6");
    }

    #[test]
    fn d6_below_inject_min_cost_ms_is_suppressed_below_threshold() {
        check_below_cost("D6");
    }

    #[test]
    fn d6_ineligible_rule_is_suppressed_not_eligible() {
        check_not_eligible("D6");
    }

    #[test]
    fn subject_key_is_rule_id_pipe_project_id() {
        assert_eq!(
            decide("D3", &[60_000, 120_000, 180_000], true).subject_key,
            "D3|p1"
        );
        assert_eq!(
            decide("D6", &[60_000, 120_000, 180_000], true).subject_key,
            "D6|p1"
        );
    }

    #[test]
    fn expected_saving_is_half_the_median_cost_odd_count() {
        // Median of [60k, 120k, 180k] is 120k, so 60k.
        let d = decide("D3", &[180_000, 60_000, 120_000], true);
        assert_eq!(d.expected_saving_ms, Some(60_000));
    }

    #[test]
    fn expected_saving_is_half_the_median_cost_even_count() {
        // Median of [100k, 200k, 300k, 400k] is 250k, so 125k.
        let d = decide("D6", &[400_000, 100_000, 300_000, 200_000], true);
        assert_eq!(d.expected_saving_ms, Some(125_000));
    }

    #[test]
    fn findings_of_other_rules_do_not_count() {
        // Two D3 findings plus three D6 ones: D3 stays below the count.
        let mut s = state("D3", &[200_000, 200_000], true);
        s.recent.extend((0..3).map(|_| RecentFinding {
            rule_id: "D6".to_string(),
            cost_ms: 200_000,
        }));
        let d = decide_inject(&s, "D3", &DevtimeActionsConfig::default(), VERSION);
        assert_eq!(d.decision, "suppress");
        assert_eq!(d.suppress_reason.as_deref(), Some("below_threshold"));
        assert_eq!(d.basis_recent_count, Some(2));
    }

    #[test]
    fn decision_vocabulary_stays_closed() {
        // `store::shadow` is private to the store, so the closed lists are restated here; the GREEN
        // step may re-export the consts and swap these for them.
        const ACTIONS: [&str; 2] = ["inject", "propose"];
        const MODES: [&str; 2] = ["shadow", "live"];
        const DECISIONS: [&str; 2] = ["fire", "suppress"];
        const SUPPRESS_REASONS: [&str; 5] = [
            "cap",
            "cooldown",
            "below_threshold",
            "not_eligible",
            "holdout",
        ];
        for d in [
            decide("D3", &[60_000, 120_000, 180_000], true),
            decide("D3", &[], true),
            decide("D6", &[60_000, 120_000, 180_000], false),
        ] {
            assert!(ACTIONS.contains(&d.action.as_str()));
            assert!(MODES.contains(&d.mode.as_str()));
            assert!(DECISIONS.contains(&d.decision.as_str()));
            assert_eq!(d.suppress_reason.is_some(), d.decision == "suppress");
            if let Some(reason) = &d.suppress_reason {
                assert!(SUPPRESS_REASONS.contains(&reason.as_str()));
            }
        }
    }
}
