//! Per-rule precision from the owner's feedback marks, phase-2 eligibility, base rates, and the mark
//! API. The endpoint that calls it is SP4's; until then only tests do. The SQL stays in
//! `devtime_store`: this module judges the counts it returns.
//!
//! Stub: the signatures are fixed and the bodies answer "nothing" until the logic is written.

use sqlx::SqlitePool;

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{Level, Rule};
use crate::devtime_store::RuleCaseCounts;

/// One rule's precision over the cases the owner (or an exact detector) has settled.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub struct RulePrecision {
    pub rule_id: String,
    pub level: Level,
    /// `exact + inferred_marked`: spec §7's "casos exact ou validados na app".
    pub n_cases: i64,
    pub n_not_rework: i64,
    /// `(n_cases - n_not_rework) / n_cases`, or `None` without cases.
    pub precision: Option<f64>,
    /// What is reported while there are fewer than `min_cases` cases.
    pub prior: f64,
    /// A base rule with enough cases and a precision at or above the floor. A rule that is not
    /// eligible is flagged out of phase 2.
    pub eligible: bool,
}

/// Why a mark was refused.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub enum MarkError {
    UnknownFinding,
    BadVerdict,
    BadCause,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for MarkError {
    fn from(error: sqlx::Error) -> Self {
        MarkError::Db(error)
    }
}

/// How often a rule fires, over the sessions that have at least one attempt.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub struct BaseRate {
    pub rule_id: String,
    pub sessions_fired: i64,
    pub sessions_with_tools: i64,
    pub rate: f64,
}

/// PURE: the precision of one rule from its case counts. Stub: no cases, the prior, not eligible.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub fn precision_of(
    _counts: &RuleCaseCounts,
    rule: &Rule,
    cfg: &DevtimeRulesConfig,
) -> RulePrecision {
    RulePrecision {
        rule_id: rule.id.to_string(),
        level: rule.level,
        n_cases: 0,
        n_not_rework: 0,
        precision: None,
        prior: cfg
            .precision
            .priors
            .get(rule.id)
            .copied()
            .unwrap_or(cfg.precision.prior_default),
        eligible: false,
    }
}

/// Precision of every registered rule. Stub: empty.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn rule_report(
    _pool: &SqlitePool,
    _cfg: &DevtimeRulesConfig,
    _since: Option<&str>,
) -> sqlx::Result<Vec<RulePrecision>> {
    Ok(Vec::new())
}

/// Records the owner's verdict on one finding, validated against `FEEDBACK_VERDICTS` and
/// `FEEDBACK_CAUSES` plus `LEVERS`. Stub: records nothing.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn mark_case(
    _pool: &SqlitePool,
    _finding_key: &str,
    _verdict: &str,
    _cause: Option<&str>,
) -> Result<(), MarkError> {
    Ok(())
}

/// Per-rule firing frequency. Stub: empty.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn base_rates(
    _pool: &SqlitePool,
    _project_id: Option<&str>,
    _since: Option<&str>,
) -> sqlx::Result<Vec<BaseRate>> {
    Ok(Vec::new())
}
