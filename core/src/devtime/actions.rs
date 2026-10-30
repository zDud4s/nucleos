//! The automatic actions' decision step (actions design spec, section 3). `decide_*` are pure
//! functions: project state and config in, one [`Decision`] out, ready to be written to
//! `devtime_shadow`. No message text is ever carried here: only ids, counts and milliseconds.

#![allow(dead_code)] // the wiring step (2.4) is the caller

pub mod inject;

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
