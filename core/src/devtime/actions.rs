//! The automatic actions' decision step (actions design spec, section 3). `decide_*` are pure
//! functions: project state and config in, one [`Decision`] out, ready to be written to
//! `devtime_shadow`. No message text is ever carried here: only ids, counts and milliseconds.

#![allow(dead_code)] // the wiring step (2.4) is the caller

pub mod inject;
pub mod propose;

use crate::devtime::precision::{RulePrecision, action_eligible};

/// One decision, shaped like a `devtime_shadow` row before it is stored. Its string fields come from
/// the closed vocabularies in `devtime::store::shadow` (`ACTIONS`, `MODES`, `DECISIONS`,
/// `SUPPRESS_REASONS`).
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// `inject` or `propose`.
    pub action: String,
    pub rule_id: String,
    pub project_id: String,
    /// The session the decision is for (`inject`); `None` for a cross-session `propose`.
    pub session_id: Option<String>,
    /// `inject`: `rule_id|project_id`; `propose`: the cross finding's key.
    pub subject_key: String,
    /// `shadow` or `live`.
    pub mode: String,
    /// `fire` or `suppress`.
    pub decision: String,
    /// Set exactly when `decision` is `suppress`.
    pub suppress_reason: Option<String>,
    pub basis_precision: Option<f64>,
    pub basis_cases: Option<i64>,
    pub basis_recent_count: Option<i64>,
    pub basis_recent_cost_ms: Option<i64>,
    /// A-priori estimate: `0.5 x median(cost_ms)` over the rule's findings in the window.
    pub expected_saving_ms: Option<i64>,
    pub rule_version: i64,
    pub action_version: i64,
}

/// One finding of a rule in the project, inside the look-back window: only its cost matters here.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentFinding {
    pub rule_id: String,
    pub cost_ms: i64,
}

/// What `decide_*` reads about one project when a session starts.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectState {
    pub project_id: String,
    /// The session that is starting.
    pub session_id: Option<String>,
    /// `shadow` or `live`, from `devtime_action_state`.
    pub mode: String,
    /// The project's findings in the last `inject_window_days`, all rules.
    pub recent: Vec<RecentFinding>,
    /// Precision of every rule, as `precision::rule_report` returns it.
    pub precisions: Vec<RulePrecision>,
}

/// PURE: decides `inject` for one rule (D3 or D6) in one project.
///
/// `fire` when the rule has at least `inject_min_count` findings costing at least
/// `inject_min_cost_ms` in total and `action_eligible` holds; `suppress/not_eligible` when the rule
/// is not eligible; `suppress/below_threshold` otherwise.
pub fn decide_inject(
    state: &ProjectState,
    rule_id: &str,
    cfg: &crate::config::DevtimeActionsConfig,
    action_version: i64,
) -> Decision {
    let precision = state.precisions.iter().find(|p| p.rule_id == rule_id);
    let mut costs: Vec<i64> = state
        .recent
        .iter()
        .filter(|f| f.rule_id == rule_id)
        .map(|f| f.cost_ms)
        .collect();
    let count = i64::try_from(costs.len()).unwrap_or(i64::MAX);
    let total = costs.iter().fold(0_i64, |acc, c| acc.saturating_add(*c));
    let eligible = precision.is_some_and(|p| action_eligible(p, cfg));
    let suppress_reason = if !eligible {
        Some("not_eligible")
    } else if count < i64::from(cfg.inject_min_count)
        || total < i64::try_from(cfg.inject_min_cost_ms).unwrap_or(i64::MAX)
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
        action: "inject".to_string(),
        rule_id: rule_id.to_string(),
        project_id: state.project_id.clone(),
        session_id: state.session_id.clone(),
        subject_key: format!("{rule_id}|{}", state.project_id),
        mode: state.mode.clone(),
        decision: decision.to_string(),
        suppress_reason: suppress_reason.map(str::to_string),
        basis_precision: precision.and_then(|p| p.precision),
        basis_cases: precision.map(|p| p.n_cases),
        basis_recent_count: Some(count),
        basis_recent_cost_ms: Some(total),
        expected_saving_ms: median(&mut costs).map(|m| m / 2),
        rule_version: crate::devtime::rules::rule(rule_id).map_or(1, |r| i64::from(r.version)),
        action_version,
    }
}

/// The version of the action logic; a bump starts a fresh cooldown history.
pub const ACTION_VERSION: i64 = 1;

/// One earlier `fire` of `inject`, read back from `devtime_shadow` for the cooldown.
#[derive(Debug, Clone, PartialEq)]
pub struct PriorFire {
    pub rule_id: String,
    pub project_id: String,
    /// `shadow` or `live`: a fire only counts toward the cooldown of its own mode.
    pub mode: String,
    pub action_version: i64,
    /// When the fire was decided, in unix milliseconds.
    pub at_ms: i64,
}

/// PURE: applies the per-session cap and the per-(rule, project) cooldown to the `inject`
/// candidates of one session.
///
/// Cooldown first: a `fire` whose (rule, project, mode, action_version) has a `PriorFire` within
/// `inject_cooldown_days` of `now_ms` becomes `suppress/cooldown`. Then, among the surviving
/// `fire`s, the top `inject_max_per_session` by `basis_recent_cost_ms` desc (ties by `rule_id`
/// asc) stay; the rest become `suppress/cap`. Decisions already `suppress` pass untouched and take
/// no cap slot. The output is sorted by (`rule_id`, `project_id`), whatever the input order.
pub fn apply_cap_and_cooldown(
    decisions: Vec<Decision>,
    prior: &[PriorFire],
    now_ms: i64,
    cfg: &crate::config::DevtimeActionsConfig,
) -> Vec<Decision> {
    let window_ms = i64::from(cfg.inject_cooldown_days).saturating_mul(86_400_000);
    let mut out = decisions;
    // Cooldown first, so a cooled-down fire takes no cap slot.
    for d in out.iter_mut().filter(|d| d.decision == "fire") {
        let cooled = prior.iter().any(|p| {
            p.rule_id == d.rule_id
                && p.project_id == d.project_id
                && p.mode == d.mode
                && p.action_version == d.action_version
                && now_ms.saturating_sub(p.at_ms) < window_ms
        });
        if cooled {
            d.decision = "suppress".to_string();
            d.suppress_reason = Some("cooldown".to_string());
        }
    }
    // Cap: rank the surviving fires by cost desc, ties by rule_id asc (then project_id).
    let mut fires: Vec<usize> = (0..out.len())
        .filter(|&i| out[i].decision == "fire")
        .collect();
    fires.sort_by(|&a, &b| {
        out[b]
            .basis_recent_cost_ms
            .unwrap_or(0)
            .cmp(&out[a].basis_recent_cost_ms.unwrap_or(0))
            .then_with(|| out[a].rule_id.cmp(&out[b].rule_id))
            .then_with(|| out[a].project_id.cmp(&out[b].project_id))
    });
    let keep = usize::try_from(cfg.inject_max_per_session).unwrap_or(usize::MAX);
    for &i in fires.iter().skip(keep) {
        out[i].decision = "suppress".to_string();
        out[i].suppress_reason = Some("cap".to_string());
    }
    out.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.project_id.cmp(&b.project_id))
    });
    out
}

/// Median of `values` (sorted in place); the mean of the two middle ones for an even count.
fn median(values: &mut [i64]) -> Option<i64> {
    values.sort_unstable();
    let n = values.len();
    if n == 0 {
        return None;
    }
    let mid = n / 2;
    if n % 2 == 1 {
        Some(values[mid])
    } else {
        Some(values[mid - 1].saturating_add(values[mid]) / 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 86_400_000;
    const NOW: i64 = 100 * DAY_MS;

    fn cfg() -> crate::config::DevtimeActionsConfig {
        crate::config::DevtimeActionsConfig::default()
    }

    fn cand(rule: &str, cost: i64, mode: &str) -> Decision {
        Decision {
            action: "inject".to_string(),
            rule_id: rule.to_string(),
            project_id: "p1".to_string(),
            session_id: Some("s1".to_string()),
            subject_key: format!("{rule}|p1"),
            mode: mode.to_string(),
            decision: "fire".to_string(),
            suppress_reason: None,
            basis_precision: Some(0.9),
            basis_cases: Some(20),
            basis_recent_count: Some(5),
            basis_recent_cost_ms: Some(cost),
            expected_saving_ms: Some(cost / 10),
            rule_version: 1,
            action_version: ACTION_VERSION,
        }
    }

    fn prior(rule: &str, mode: &str, version: i64, days_ago: i64) -> PriorFire {
        PriorFire {
            rule_id: rule.to_string(),
            project_id: "p1".to_string(),
            mode: mode.to_string(),
            action_version: version,
            at_ms: NOW - days_ago * DAY_MS,
        }
    }

    fn find<'a>(out: &'a [Decision], rule: &str) -> &'a Decision {
        out.iter()
            .find(|d| d.rule_id == rule)
            .expect("rule in output")
    }

    #[test]
    fn cap_cooldown_cap_keeps_top_by_cost_and_suppresses_rest() {
        let input = vec![
            cand("D1", 100, "live"),
            cand("D2", 500, "live"),
            cand("D3", 300, "live"),
            cand("D4", 400, "live"),
            cand("D5", 200, "live"),
        ];
        let out = apply_cap_and_cooldown(input, &[], NOW, &cfg());
        assert_eq!(out.len(), 5);
        for kept in ["D2", "D4", "D3"] {
            let d = find(&out, kept);
            assert_eq!(d.decision, "fire", "{kept}");
            assert_eq!(d.suppress_reason, None);
        }
        for dropped in ["D1", "D5"] {
            let d = find(&out, dropped);
            assert_eq!(d.decision, "suppress", "{dropped}");
            assert_eq!(d.suppress_reason.as_deref(), Some("cap"));
        }
    }

    #[test]
    fn cap_cooldown_prior_fire_inside_window_suppresses() {
        let input = vec![cand("D3", 300, "live")];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "live", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "suppress");
        assert_eq!(out[0].suppress_reason.as_deref(), Some("cooldown"));
    }

    #[test]
    fn cap_cooldown_prior_fire_outside_window_does_not_suppress() {
        let input = vec![cand("D3", 300, "live")];
        let days = i64::from(cfg().inject_cooldown_days) + 1;
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "live", ACTION_VERSION, days)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "fire");
        assert_eq!(out[0].suppress_reason, None);
    }

    #[test]
    fn cap_cooldown_applies_in_shadow_like_live() {
        let input = vec![cand("D3", 300, "shadow")];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "shadow", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "suppress");
        assert_eq!(out[0].suppress_reason.as_deref(), Some("cooldown"));
    }

    #[test]
    fn cap_cooldown_fire_of_other_mode_does_not_count() {
        let input = vec![cand("D3", 300, "live")];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "shadow", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "fire");
    }

    #[test]
    fn cap_cooldown_other_rule_or_project_does_not_count() {
        let mut other_project = prior("D3", "live", ACTION_VERSION, 1);
        other_project.project_id = "p2".to_string();
        let priors = [other_project, prior("D6", "live", ACTION_VERSION, 1)];
        let out = apply_cap_and_cooldown(vec![cand("D3", 300, "live")], &priors, NOW, &cfg());
        assert_eq!(out[0].decision, "fire");
    }

    #[test]
    fn cap_cooldown_new_action_version_does_not_inherit_old_cooldown() {
        let mut c = cand("D3", 300, "live");
        c.action_version = ACTION_VERSION + 1;
        let out = apply_cap_and_cooldown(
            vec![c],
            &[prior("D3", "live", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "fire");
    }

    #[test]
    fn cap_cooldown_cooled_down_fire_takes_no_cap_slot() {
        let input = vec![
            cand("D1", 900, "live"),
            cand("D2", 500, "live"),
            cand("D3", 400, "live"),
            cand("D4", 300, "live"),
        ];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D1", "live", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(
            find(&out, "D1").suppress_reason.as_deref(),
            Some("cooldown")
        );
        for kept in ["D2", "D3", "D4"] {
            assert_eq!(find(&out, kept).decision, "fire", "{kept}");
        }
    }

    #[test]
    fn cap_cooldown_is_deterministic_with_ties_and_any_input_order() {
        let a = vec![
            cand("D1", 200, "live"),
            cand("D2", 200, "live"),
            cand("D3", 200, "live"),
            cand("D4", 200, "live"),
            cand("D5", 900, "live"),
        ];
        let mut b = a.clone();
        b.reverse();
        let mut c = a.clone();
        c.rotate_left(2);
        let out_a = apply_cap_and_cooldown(a, &[], NOW, &cfg());
        assert_eq!(out_a, apply_cap_and_cooldown(b, &[], NOW, &cfg()));
        assert_eq!(out_a, apply_cap_and_cooldown(c, &[], NOW, &cfg()));
        // Tie on cost breaks by rule_id asc: D5 (highest), then D1, D2 stay; D3, D4 are capped.
        assert_eq!(find(&out_a, "D5").decision, "fire");
        assert_eq!(find(&out_a, "D1").decision, "fire");
        assert_eq!(find(&out_a, "D2").decision, "fire");
        assert_eq!(find(&out_a, "D3").suppress_reason.as_deref(), Some("cap"));
        assert_eq!(find(&out_a, "D4").suppress_reason.as_deref(), Some("cap"));
    }

    #[test]
    fn cap_cooldown_already_suppressed_pass_intact_and_take_no_slot() {
        let mut below = cand("D0", 9_999, "live");
        below.decision = "suppress".to_string();
        below.suppress_reason = Some("below_threshold".to_string());
        let input = vec![
            below.clone(),
            cand("D1", 300, "live"),
            cand("D2", 200, "live"),
            cand("D3", 100, "live"),
        ];
        let out = apply_cap_and_cooldown(input, &[], NOW, &cfg());
        assert_eq!(find(&out, "D0"), &below);
        for kept in ["D1", "D2", "D3"] {
            assert_eq!(find(&out, kept).decision, "fire", "{kept}");
        }
    }
}
