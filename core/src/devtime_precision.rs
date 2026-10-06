//! Per-rule precision from the owner's feedback marks, phase-2 eligibility, base rates, and the mark
//! API. The endpoint that calls it is SP4's; until then only tests do. The SQL stays in
//! `devtime_store`: this module judges the counts it returns.
//!
//! Privacy: a mark carries a verdict and a cause from closed vocabularies. No message text is ever
//! stored here.

use sqlx::SqlitePool;

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{Level, RULES, Rule};
use crate::devtime_store::{
    self, FEEDBACK_CAUSES, FEEDBACK_VERDICTS, FeedbackRow, LEVERS, RuleCaseCounts,
};

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
#[allow(dead_code)] // SP4's endpoint is the reader
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

/// PURE: the precision of one rule from its case counts.
///
/// A case is a finding that is `exact` or that the owner marked; `n_not_rework` is how many of the
/// cases the owner marked `not_rework`. The rule is eligible for phase 2 only when it is a base rule
/// with at least `min_cases` cases and a precision at or above the floor.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub fn precision_of(
    counts: &RuleCaseCounts,
    rule: &Rule,
    cfg: &DevtimeRulesConfig,
) -> RulePrecision {
    let n_cases = counts.exact + counts.inferred_marked;
    let n_not_rework = counts.not_rework;
    let precision = if n_cases > 0 {
        let right = (n_cases - n_not_rework).max(0);
        Some(right as f64 / n_cases as f64)
    } else {
        None
    };
    let prior = cfg
        .precision
        .priors
        .get(rule.id)
        .copied()
        .unwrap_or(cfg.precision.prior_default);
    let enough = n_cases >= i64::from(cfg.precision.min_cases);
    let eligible = rule.level == Level::Base
        && enough
        && precision.is_some_and(|value| value >= cfg.precision.floor);
    RulePrecision {
        rule_id: rule.id.to_string(),
        level: rule.level,
        n_cases,
        n_not_rework,
        precision,
        prior,
        eligible,
    }
}

/// Precision of every registered rule, in registry order. A rule with no findings reports no cases.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn rule_report(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    since: Option<&str>,
) -> sqlx::Result<Vec<RulePrecision>> {
    let counts = devtime_store::rule_case_counts(pool, since).await?;
    let empty = RuleCaseCounts::default();
    let mut report = Vec::with_capacity(RULES.len());
    for rule in RULES {
        let found = counts
            .iter()
            .find(|row| row.rule_id == rule.id)
            .unwrap_or(&empty);
        report.push(precision_of(found, rule, cfg));
    }
    Ok(report)
}

/// Records the owner's verdict on one finding, validated against `FEEDBACK_VERDICTS` and
/// `FEEDBACK_CAUSES` plus `LEVERS`. The rule, session and rule version are copied from the finding.
/// Marking again replaces the earlier mark. A cause given with `confirmed` overrides the inferred
/// lever on display and stays only in feedback.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn mark_case(
    pool: &SqlitePool,
    finding_key: &str,
    verdict: &str,
    cause: Option<&str>,
) -> Result<(), MarkError> {
    if !FEEDBACK_VERDICTS.contains(&verdict) {
        return Err(MarkError::BadVerdict);
    }
    let known_cause = |name: &str| FEEDBACK_CAUSES.contains(&name) || LEVERS.contains(&name);
    if cause.is_some_and(|name| !known_cause(name)) {
        return Err(MarkError::BadCause);
    }
    let finding = devtime_store::finding_by_key(pool, finding_key)
        .await?
        .ok_or(MarkError::UnknownFinding)?;
    devtime_store::upsert_feedback(
        pool,
        &FeedbackRow {
            finding_key: finding.finding_key.clone(),
            rule_id: finding.rule_id.clone(),
            session_id: Some(finding.session_id.clone()),
            verdict: verdict.to_string(),
            cause: cause.map(str::to_string),
            rule_version: Some(finding.rule_version),
            marked_at: String::new(),
        },
    )
    .await?;
    Ok(())
}

/// Per-rule firing frequency: how many of the sessions with at least one attempt fired the rule.
/// Only rules that fired in some session are listed.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn base_rates(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: Option<&str>,
) -> sqlx::Result<Vec<BaseRate>> {
    let (with_tools, fired) = devtime_store::rule_base_rates(pool, project_id, since).await?;
    Ok(fired
        .into_iter()
        .map(|(rule_id, sessions_fired)| BaseRate {
            rule_id,
            sessions_fired,
            sessions_with_tools: with_tools,
            rate: if with_tools > 0 {
                sessions_fired as f64 / with_tools as f64
            } else {
                0.0
            },
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules::rule;
    use crate::devtime_rules_fixture::{script, test_pool};
    use crate::devtime_store::{FindingRow, SessionRow};

    const WHEN: &str = "2026-10-04T10:00:00.000Z";

    fn finding(key: &str, rule_id: &str, confidence: &str) -> FindingRow {
        FindingRow {
            finding_key: key.to_string(),
            project_id: "p1".to_string(),
            session_id: "s1".to_string(),
            scope: "session".to_string(),
            rule_id: rule_id.to_string(),
            rule_version: 1,
            level: "base".to_string(),
            waste: "rework".to_string(),
            lever: "model".to_string(),
            confidence: confidence.to_string(),
            lane: "main".to_string(),
            started_at: WHEN.to_string(),
            ended_at: WHEN.to_string(),
            cost_ms: 1000,
            count: 1,
            attempt_ids: "[]".to_string(),
            sessions: "[]".to_string(),
            rules_version: "fp".to_string(),
            parser_version: 2,
        }
    }

    /// `n` findings of `rule_id` with keys `<rule_id>-<i>`.
    fn findings(rule_id: &str, n: usize, confidence: &str) -> Vec<FindingRow> {
        (0..n)
            .map(|i| finding(&format!("{rule_id}-{i}"), rule_id, confidence))
            .collect()
    }

    async fn store(pool: &SqlitePool, rows: &[FindingRow]) {
        devtime_store::replace_cross_findings(pool, "p1", &[], rows)
            .await
            .unwrap();
    }

    fn report_of<'a>(report: &'a [RulePrecision], rule_id: &str) -> &'a RulePrecision {
        report
            .iter()
            .find(|row| row.rule_id == rule_id)
            .expect("the rule is in the report")
    }

    #[tokio::test]
    async fn precision_harness_matches_marks() {
        let pool = test_pool().await;
        let mut rows = findings("A1", 22, "exact");
        rows.extend(findings("A3", 25, "exact"));
        store(&pool, &rows).await;
        for i in 0..3 {
            mark_case(&pool, &format!("A1-{i}"), "not_rework", None)
                .await
                .unwrap();
        }
        mark_case(&pool, "A3-0", "not_rework", Some("taste"))
            .await
            .unwrap();

        // The ship default: 19/22 is above the 0.8 floor.
        let cfg = DevtimeRulesConfig::default();
        let report = rule_report(&pool, &cfg, None).await.unwrap();
        let first = report_of(&report, "A1");
        assert_eq!(first.n_cases, 22);
        assert_eq!(first.n_not_rework, 3);
        assert!((first.precision.unwrap() - 19.0 / 22.0).abs() < 1e-9);
        assert!(first.eligible);
        let second = report_of(&report, "A3");
        assert_eq!(second.n_cases, 25);
        assert_eq!(second.n_not_rework, 1);
        assert!((second.precision.unwrap() - 24.0 / 25.0).abs() < 1e-9);
        assert!(second.eligible);

        // A stricter floor from the config flags the first out and keeps the second.
        let mut strict = DevtimeRulesConfig::default();
        strict.precision.floor = 0.9;
        let report = rule_report(&pool, &strict, None).await.unwrap();
        assert!(!report_of(&report, "A1").eligible);
        assert!(report_of(&report, "A3").eligible);

        // Every registered rule is reported, and one with no findings has no cases.
        assert_eq!(report.len(), RULES.len());
        let silent = report_of(&report, "B1");
        assert_eq!(silent.n_cases, 0);
        assert_eq!(silent.precision, None);
        assert!(!silent.eligible);
    }

    #[tokio::test]
    async fn inferred_rule_counts_only_marked_cases() {
        let pool = test_pool().await;
        store(&pool, &findings("A5", 25, "inferred")).await;
        let cfg = DevtimeRulesConfig::default();

        let report = rule_report(&pool, &cfg, None).await.unwrap();
        let before = report_of(&report, "A5");
        assert_eq!(before.n_cases, 0);
        assert_eq!(before.precision, None);
        assert!(!before.eligible);

        for i in 0..5 {
            mark_case(&pool, &format!("A5-{i}"), "confirmed", None)
                .await
                .unwrap();
        }
        let report = rule_report(&pool, &cfg, None).await.unwrap();
        let few = report_of(&report, "A5");
        assert_eq!(few.n_cases, 5);
        assert_eq!(few.precision, Some(1.0));
        assert!(!few.eligible, "five cases are fewer than min_cases");

        for i in 5..20 {
            mark_case(&pool, &format!("A5-{i}"), "confirmed", None)
                .await
                .unwrap();
        }
        mark_case(&pool, "A5-20", "not_rework", None).await.unwrap();
        let report = rule_report(&pool, &cfg, None).await.unwrap();
        let enough = report_of(&report, "A5");
        assert_eq!(enough.n_cases, 21);
        assert_eq!(enough.n_not_rework, 1);
        assert!(enough.eligible);
    }

    #[test]
    fn c2_prior_is_low_and_flagged_out() {
        let cfg = DevtimeRulesConfig::default();
        let c2 = rule("C2").expect("C2 is registered");
        let none = precision_of(&RuleCaseCounts::default(), c2, &cfg);
        assert_eq!(none.prior, 0.2);
        assert_eq!(none.precision, None);
        assert!(!none.eligible);

        // A rule without a configured prior reports the default one.
        let a1 = rule("A1").expect("A1 is registered");
        let plain = precision_of(&RuleCaseCounts::default(), a1, &cfg);
        assert_eq!(plain.prior, cfg.precision.prior_default);

        // A deferred rule is never eligible, however clean its cases look.
        let d11 = rule("D11").expect("D11 is registered");
        assert_eq!(d11.level, Level::Deferred);
        let clean = RuleCaseCounts {
            rule_id: "D11".to_string(),
            exact: 100,
            inferred_marked: 0,
            not_rework: 0,
            total: 100,
        };
        let judged = precision_of(&clean, d11, &cfg);
        assert_eq!(judged.precision, Some(1.0));
        assert!(!judged.eligible);
    }

    #[tokio::test]
    async fn mark_case_rejects_unknown_key_verdict_and_cause() {
        let pool = test_pool().await;
        store(&pool, &findings("A1", 1, "exact")).await;

        let unknown = mark_case(&pool, "nope", "confirmed", None).await;
        assert!(matches!(unknown, Err(MarkError::UnknownFinding)));
        let verdict = mark_case(&pool, "A1-0", "maybe", None).await;
        assert!(matches!(verdict, Err(MarkError::BadVerdict)));
        let cause = mark_case(&pool, "A1-0", "confirmed", Some("vibes")).await;
        assert!(matches!(cause, Err(MarkError::BadCause)));

        // A refused mark leaves nothing behind.
        let counts = devtime_store::rule_case_counts(&pool, None).await.unwrap();
        assert_eq!(counts[0].not_rework, 0);

        // A named cause and a lever are both valid causes.
        mark_case(&pool, "A1-0", "confirmed", Some("spec_gap"))
            .await
            .unwrap();
        mark_case(&pool, "A1-0", "confirmed", Some("model"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn remark_replaces_previous() {
        let pool = test_pool().await;
        store(&pool, &findings("A1", 1, "exact")).await;

        mark_case(&pool, "A1-0", "not_rework", None).await.unwrap();
        let counts = devtime_store::rule_case_counts(&pool, None).await.unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].not_rework, 1);

        mark_case(&pool, "A1-0", "confirmed", Some("infra"))
            .await
            .unwrap();
        let counts = devtime_store::rule_case_counts(&pool, None).await.unwrap();
        assert_eq!(counts.len(), 1, "one mark per finding, never a second row");
        assert_eq!(counts[0].not_rework, 0);
        assert_eq!(counts[0].exact, 1);
        assert_eq!(counts[0].total, 1);
    }

    #[tokio::test]
    async fn base_rates_report_frequency_over_sessions_with_tools() {
        let pool = test_pool().await;
        for id in ["s1", "s2", "s3"] {
            let text = format!(
                r#"
                turn main 0
                bash main 2+1 prog="cargo test" aid=a-{id}
                "#
            );
            script(&text)
                .session(id)
                .persist(&pool, "p1")
                .await
                .unwrap();
        }
        // A session with no attempt at all: it cannot have fired anything, so it is not counted.
        let mut tx = devtime_store::begin_chunk(&pool).await.unwrap();
        devtime_store::upsert_session(
            &mut *tx,
            &SessionRow {
                session_id: "s4".to_string(),
                project_id: "p1".to_string(),
                started_at: Some(WHEN.to_string()),
                ended_at: Some(WHEN.to_string()),
                dirty: 1,
                parser_version: 2,
                updated_at: WHEN.to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut rows = Vec::new();
        for (key, rule_id, session) in [
            ("k1", "A1", "s1"),
            ("k2", "A1", "s1"),
            ("k3", "A1", "s2"),
            ("k4", "A2", "s3"),
            ("k5", "B1", "s4"),
        ] {
            let mut row = finding(key, rule_id, "exact");
            row.session_id = session.to_string();
            rows.push(row);
        }
        store(&pool, &rows).await;

        let rates = base_rates(&pool, Some("p1"), None).await.unwrap();
        let ids: Vec<&str> = rates.iter().map(|row| row.rule_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["A1", "A2"],
            "B1 fired only in a session without attempts"
        );
        assert_eq!(rates[0].sessions_with_tools, 3);
        assert_eq!(
            rates[0].sessions_fired, 2,
            "two findings in one session count once"
        );
        assert!((rates[0].rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(rates[1].sessions_fired, 1);
        assert!((rates[1].rate - 1.0 / 3.0).abs() < 1e-9);

        // Another project has no sessions: no rates, and no division by zero.
        assert!(
            base_rates(&pool, Some("p2"), None)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
