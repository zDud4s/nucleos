//! Metric 9, the autopilot (spec analytics-pipeline §6.1 "9. Classificador"): how often the
//! classifier and the judge agree with a person, what the judge costs, and which actions were
//! refused. Current window only; the spec gives this metric no previous figures.
//!
//! `shadow_decisions` and `judge_verdicts` have no `project_id`: the project comes through
//! `runs`. Both joins are LEFT, so the global figure keeps a decision whose run was deleted.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::SqlitePool;

use super::stats::{self, Dist};
use crate::analytics::Window;
use crate::governance::shadow::AGREE_CASE;
use crate::judge::READINESS_SELECT;

/// Only rows a person has judged count toward `n` and `agree`; the rest are `pending`, not
/// disagreement. `AGREE_CASE` qualifies its columns with the table name, so no alias here.
pub(crate) fn classifier_sql() -> String {
    format!(
        "SELECT shadow_decisions.action_class AS action_class,
                SUM(CASE WHEN shadow_decisions.human_verdict IS NOT NULL THEN 1 ELSE 0 END) AS n,
                SUM(CASE WHEN shadow_decisions.human_verdict IS NOT NULL
                         THEN {AGREE_CASE} ELSE 0 END) AS agreed,
                SUM(CASE WHEN shadow_decisions.human_verdict IS NULL THEN 1 ELSE 0 END) AS pending
           FROM shadow_decisions LEFT JOIN runs ON runs.id = shadow_decisions.run_id
          WHERE shadow_decisions.created_at >= ?1 AND shadow_decisions.created_at < ?2
            AND (?3 IS NULL OR runs.project_id = ?3)
          GROUP BY shadow_decisions.action_class ORDER BY shadow_decisions.action_class"
    )
}

pub(crate) const JUDGE_SQL: &str =
    "SELECT jv.latency_ms, jv.input_tokens, jv.cost_usd, jv.error IS NOT NULL AS errored,
        jv.enforced
   FROM judge_verdicts jv LEFT JOIN runs r ON r.id = jv.run_id
  WHERE jv.created_at >= ?1 AND jv.created_at < ?2 AND (?3 IS NULL OR r.project_id = ?3)";

/// The judge's agreement with a person over the window, in the unit the promotion gate counts in:
/// distinct actions `(tool_name, tool_input_digest)`, by `band`. The arithmetic is
/// `judge::review`'s `READINESS_SELECT` itself - one fragment for both callers, so this figure and
/// D11's `readiness_on` cannot drift - and only the window and the project filter are added. The
/// opposite-band check inside the fragment looks at the project's whole history, as the gate does.
/// The key does not carry the project, so with no project filter an identical action in two
/// projects is one action.
pub(crate) fn judge_agreement_sql() -> String {
    format!(
        "SELECT {READINESS_SELECT}
           FROM judge_verdicts jv LEFT JOIN runs r ON r.id = jv.run_id
          WHERE jv.human_verdict IS NOT NULL
            AND jv.created_at >= ?1 AND jv.created_at < ?2
            AND (?3 IS NULL OR r.project_id = ?3)"
    )
}

/// No fixed list of statuses: only `pending` is confirmed, so the page shows whatever comes.
pub(crate) const REFUSED_SQL: &str = "SELECT status, COUNT(*) FROM proposals
  WHERE kind = 'refused-action' AND created_at >= ?1 AND created_at < ?2
    AND (?3 IS NULL OR project_id = ?3)
  GROUP BY status";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClassAgreement {
    pub action_class: String,
    /// Decisions a person has judged.
    pub n: i64,
    pub agree: Option<f64>,
    /// Decisions nobody has judged yet.
    pub pending: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Judge {
    /// Verdicts in the window, one per consultation.
    pub n: i64,
    /// Distinct actions a person has judged (not verdict rows): the unit of `agree`.
    pub reviewed: i64,
    pub latency_ms: Dist,
    pub input_tokens: i64,
    pub cost_usd: f64,
    pub error_rate: Option<f64>,
    pub enforced_share: Option<f64>,
    /// The judge's opinion against the person's, per distinct action; a capped allow counts as an
    /// allow. See [`judge_agreement_sql`].
    pub agree: Option<f64>,
    /// What `agree` compares: the judge's `band`, not the decision that finally took effect.
    pub basis: &'static str,
    /// What `agree` and `reviewed` count: distinct actions, as the promotion gate does.
    pub unit: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Refused {
    pub total: i64,
    pub by_status: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Autopilot {
    pub classifier: Vec<ClassAgreement>,
    pub judge: Judge,
    pub refused_actions: Refused,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct JudgeRow {
    latency_ms: Option<i64>,
    input_tokens: Option<i64>,
    cost_usd: f64,
    errored: i64,
    enforced: i64,
}

/// PURE: the judge figures from the window's verdicts, plus `(reviewed, agreeing)` distinct
/// actions from [`judge_agreement_sql`]. The agreement rule is the band, not the effect
/// (`final_decision`), so a capped allow does not count as a disagreement; `final_decision` is
/// also recorded before the hook's unattended/DontAsk conversions (`judge.rs` `VerdictRow`), so it
/// is not the decision the run finally received either.
fn judge_block(rows: &[JudgeRow], reviewed: i64, agreeing: i64) -> Judge {
    let n = rows.len() as i64;
    let flagged = |keep: fn(&JudgeRow) -> bool| rows.iter().filter(|row| keep(row)).count() as i64;
    Judge {
        n,
        reviewed,
        latency_ms: stats::dist(rows.iter().filter_map(|row| row.latency_ms).collect()),
        input_tokens: rows.iter().filter_map(|row| row.input_tokens).sum(),
        cost_usd: rows.iter().map(|row| row.cost_usd).sum(),
        error_rate: stats::ratio(flagged(|row| row.errored != 0), n),
        enforced_share: stats::ratio(flagged(|row| row.enforced != 0), n),
        agree: stats::ratio(agreeing, reviewed),
        basis: "band",
        unit: "action",
    }
}

pub(crate) async fn read(
    pool: &SqlitePool,
    project: Option<&str>,
    window: Window,
) -> sqlx::Result<Autopilot> {
    // `AssertSqlSafe` because the text is built at run time. Safe by construction: the only
    // interpolated fragments are the `AGREE_CASE` and `READINESS_SELECT` consts, and the project
    // stays a bound parameter, as in `governance::shadow::scoreboard`.
    let classes: Vec<(String, i64, i64, i64)> =
        sqlx::query_as(sqlx::AssertSqlSafe(classifier_sql()))
            .bind(window.from_bound())
            .bind(window.to_bound())
            .bind(project)
            .fetch_all(pool)
            .await?;
    let judge: Vec<JudgeRow> = sqlx::query_as(JUDGE_SQL)
        .bind(window.from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await?;
    let (reviewed, agreeing): (i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(judge_agreement_sql()))
            .bind(window.from_bound())
            .bind(window.to_bound())
            .bind(project)
            .fetch_one(pool)
            .await?;
    let refused: Vec<(String, i64)> = sqlx::query_as(REFUSED_SQL)
        .bind(window.from_bound())
        .bind(window.to_bound())
        .bind(project)
        .fetch_all(pool)
        .await?;

    Ok(Autopilot {
        classifier: classes
            .into_iter()
            .map(|(action_class, n, agreed, pending)| ClassAgreement {
                action_class,
                n,
                agree: stats::ratio(agreed, n),
                pending,
            })
            .collect(),
        judge: judge_block(&judge, reviewed, agreeing),
        refused_actions: Refused {
            total: refused.iter().map(|(_, count)| count).sum(),
            by_status: refused.into_iter().collect(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::tests::{close, now};

    async fn run(pool: &SqlitePool, project: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'p', 'completed', 'shadow', '2026-10-16T09:00:00Z') RETURNING id",
        )
        .bind(project)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn decision(
        pool: &SqlitePool,
        run_id: i64,
        class: &str,
        decision: &str,
        verdict: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO shadow_decisions (run_id, tool_name, decision, action_class,
                                           classifier_version, human_verdict, created_at)
             VALUES (?, 'Write', ?, ?, 1, ?, '2026-10-16T10:00:00Z')",
        )
        .bind(run_id)
        .bind(decision)
        .bind(class)
        .bind(verdict)
        .execute(pool)
        .await
        .unwrap();
    }

    /// One judge verdict: `(run, band, human verdict, latency, tokens, cost, error, enforced)`.
    type Verdict = (
        i64,
        Option<&'static str>,
        Option<&'static str>,
        Option<i64>,
        Option<i64>,
        f64,
        Option<&'static str>,
        i64,
    );

    async fn verdict(pool: &SqlitePool, created: &str, v: Verdict) {
        verdict_of(pool, created, "d", v).await;
    }

    /// [`verdict`] of the action `('Write', digest)`.
    async fn verdict_of(pool: &SqlitePool, created: &str, digest: &str, v: Verdict) {
        let (run_id, band, human, latency, tokens, cost, error, enforced) = v;
        sqlx::query(
            "INSERT INTO judge_verdicts (run_id, tool_name, tool_input_digest, action_class,
                                         classifier_decision, judge, model, questions_version,
                                         band, final_decision, enforced, latency_ms,
                                         input_tokens, cost_usd, error, human_verdict, created_at)
             VALUES (?, 'Write', ?, 'fs.write', 'allow', 'observe', 'm', 1,
                     ?, 'allow', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(run_id)
        .bind(digest)
        .bind(band)
        .bind(enforced)
        .bind(latency)
        .bind(tokens)
        .bind(cost)
        .bind(error)
        .bind(human)
        .bind(created)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn window_read(pool: &SqlitePool, project: Option<&str>) -> Autopilot {
        let window = Window::new(7, now()).unwrap();
        read(pool, project, window).await.unwrap()
    }

    /// Spec §11: agreement per class across projects, through `runs`, with `AGREE_CASE`'s
    /// asymmetric rule; unreviewed rows count as pending, not as disagreement.
    #[tokio::test]
    async fn classifier_agreement_aggregates_across_projects_through_runs() {
        let pool = crate::testdb::fresh_pool().await;
        let a = run(&pool, "a").await;
        let b = run(&pool, "b").await;
        decision(&pool, a, "fs.write", "allow", Some("approve")).await;
        decision(&pool, b, "fs.write", "pending_approval", Some("reject")).await;
        decision(&pool, b, "fs.write", "deny", Some("approve")).await;
        decision(&pool, a, "fs.write", "allow", None).await;
        decision(&pool, a, "net", "deny", Some("reject")).await;
        // Outside the window: not counted.
        sqlx::query(
            "INSERT INTO shadow_decisions (run_id, tool_name, decision, action_class,
                                           classifier_version, human_verdict, created_at)
             VALUES (?, 'Write', 'allow', 'net', 1, 'reject', '2026-09-01T10:00:00Z')",
        )
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();

        let global = window_read(&pool, None).await.classifier;
        assert_eq!(global.len(), 2);
        let (fs, net) = (&global[0], &global[1]);
        assert_eq!(
            (fs.action_class.as_str(), fs.n, fs.pending),
            ("fs.write", 3, 1)
        );
        assert!(close(fs.agree.unwrap(), 2.0 / 3.0), "{fs:?}");
        assert_eq!(
            (net.action_class.as_str(), net.n, net.pending),
            ("net", 1, 0)
        );
        assert_eq!(net.agree, Some(1.0));

        let only_a = window_read(&pool, Some("a")).await.classifier;
        let (fs, net) = (&only_a[0], &only_a[1]);
        assert_eq!((fs.n, fs.pending, fs.agree), (1, 1, Some(1.0)));
        assert_eq!((net.n, net.agree), (1, Some(1.0)));
    }

    /// The judge over the window: latency percentiles, tokens, cost, error and enforced shares,
    /// agreement by band among rows with a human verdict, and how many had one.
    #[tokio::test]
    async fn judge_figures_and_agreement_count_only_reviewed_rows() {
        let pool = crate::testdb::fresh_pool().await;
        let a = run(&pool, "a").await;
        let b = run(&pool, "b").await;
        let created = "2026-10-16T10:00:00Z";
        let rows: [Verdict; 4] = [
            (
                a,
                Some("allow"),
                Some("approve"),
                Some(100),
                Some(10),
                0.1,
                None,
                0,
            ),
            (
                a,
                Some("deny"),
                Some("approve"),
                Some(300),
                Some(20),
                0.2,
                None,
                0,
            ),
            (b, Some("allow"), None, Some(200), Some(30), 0.3, None, 1),
            (b, None, None, None, None, 0.0, Some("timeout"), 0),
        ];
        // Two different actions: the allow and the deny are not one inconsistent action.
        verdict_of(&pool, created, "d", rows[0]).await;
        verdict_of(&pool, created, "e", rows[1]).await;
        verdict_of(&pool, created, "d", rows[2]).await;
        verdict_of(&pool, created, "d", rows[3]).await;
        // Outside the window: not counted.
        let old = (
            a,
            Some("allow"),
            Some("approve"),
            Some(999),
            Some(999),
            9.0,
            None,
            1,
        );
        verdict(&pool, "2026-09-01T10:00:00Z", old).await;

        let judge = window_read(&pool, None).await.judge;
        assert_eq!((judge.n, judge.reviewed), (4, 2));
        assert_eq!((judge.basis, judge.unit), ("band", "action"));
        let latency = (
            judge.latency_ms.p50,
            judge.latency_ms.p95,
            judge.latency_ms.n,
        );
        assert_eq!(latency, (Some(200), None, 3));
        assert_eq!(judge.input_tokens, 60);
        assert!(close(judge.cost_usd, 0.6), "{judge:?}");
        assert_eq!(judge.error_rate, Some(0.25));
        assert_eq!(judge.enforced_share, Some(0.25));
        // The allow was approved and the deny was approved too: one agreement of two reviewed
        // actions.
        assert_eq!(judge.agree, Some(0.5));

        let only_b = window_read(&pool, Some("b")).await.judge;
        assert_eq!((only_b.n, only_b.reviewed, only_b.agree), (2, 0, None));
    }

    /// One verdict row of the action `('Write', digest)` with every column agreement reads.
    async fn action_verdict(
        pool: &SqlitePool,
        run_id: i64,
        digest: &str,
        (band, capped, final_decision, human): (&str, i64, &str, Option<&str>),
    ) {
        sqlx::query(
            "INSERT INTO judge_verdicts (run_id, tool_name, tool_input_digest, action_class,
                                         classifier_decision, judge, model, questions_version,
                                         band, capped, final_decision, human_verdict, created_at)
             VALUES (?, 'Write', ?, 'fs.write', 'allow', 'observe', 'm', 1,
                     ?, ?, ?, ?, '2026-10-16T10:00:00Z')",
        )
        .bind(run_id)
        .bind(digest)
        .bind(band)
        .bind(capped)
        .bind(final_decision)
        .bind(human)
        .execute(pool)
        .await
        .unwrap();
    }

    /// D11 measures the judge's opinion, not its effect: an allow the cap turned into a question
    /// still agrees with a person who approved it.
    #[tokio::test]
    async fn a_capped_allow_counts_as_an_agreeing_allow() {
        let pool = crate::testdb::fresh_pool().await;
        let a = run(&pool, "a").await;
        action_verdict(
            &pool,
            a,
            "x",
            ("allow", 1, "pending_approval", Some("approve")),
        )
        .await;

        let judge = window_read(&pool, None).await.judge;
        assert_eq!((judge.reviewed, judge.agree), (1, Some(1.0)));
    }

    /// The gate's unit is the distinct action: a repeat adds no evidence, reviewed or not.
    #[tokio::test]
    async fn two_rows_of_one_action_count_as_one_reviewed_action() {
        let pool = crate::testdb::fresh_pool().await;
        let a = run(&pool, "a").await;
        action_verdict(&pool, a, "x", ("allow", 0, "allow", Some("approve"))).await;
        action_verdict(&pool, a, "x", ("allow", 0, "allow", None)).await;
        action_verdict(&pool, a, "x", ("allow", 0, "allow", Some("approve"))).await;

        let judge = window_read(&pool, None).await.judge;
        assert_eq!(judge.n, 3);
        assert_eq!((judge.reviewed, judge.agree), (1, Some(1.0)));
    }

    /// Fail-closed, as the gate: an action the judge answered in both decisive bands never agrees,
    /// whichever of its rows the person happened to answer.
    #[tokio::test]
    async fn an_action_with_an_allow_and_a_deny_never_agrees() {
        let pool = crate::testdb::fresh_pool().await;
        let a = run(&pool, "a").await;
        action_verdict(&pool, a, "x", ("allow", 0, "allow", Some("approve"))).await;
        action_verdict(&pool, a, "x", ("deny", 0, "deny", None)).await;

        let judge = window_read(&pool, None).await.judge;
        assert_eq!((judge.reviewed, judge.agree), (1, Some(0.0)));
    }

    /// The figure is the promotion gate's, windowed: over data wholly inside the window, the same
    /// project gets the same reviewed count and the same agreement from both.
    #[tokio::test]
    async fn the_figure_equals_the_promotion_gates_readiness() {
        let pool = crate::testdb::fresh_pool().await;
        let a = run(&pool, "a").await;
        let b = run(&pool, "b").await;
        let rows = [
            (a, "1", ("allow", 0, "allow", Some("approve"))),
            (a, "1", ("allow", 0, "allow", Some("approve"))),
            (a, "2", ("allow", 1, "pending_approval", Some("approve"))),
            (a, "3", ("deny", 0, "deny", Some("approve"))),
            (a, "4", ("deny", 0, "deny", Some("reject"))),
            (a, "5", ("allow", 0, "allow", Some("approve"))),
            (a, "5", ("deny", 0, "deny", None)),
            (a, "6", ("allow", 0, "allow", None)),
            // Another project: neither the gate's nor the figure's.
            (b, "7", ("deny", 0, "deny", Some("approve"))),
        ];
        for (run_id, digest, columns) in rows {
            action_verdict(&pool, run_id, digest, columns).await;
        }

        let gate = crate::judge::readiness(&pool, "a").await.unwrap();
        assert_eq!((gate.reviewed, gate.agree), (5, 3));
        let judge = window_read(&pool, Some("a")).await.judge;
        assert_eq!(judge.reviewed, gate.reviewed);
        let agree = judge.agree.unwrap();
        assert!(
            close(agree, gate.agree as f64 / gate.reviewed as f64),
            "{judge:?}"
        );
    }

    /// Spec §11: refused actions by whatever statuses exist, no fixed list.
    #[tokio::test]
    async fn refused_actions_group_by_whatever_status_values_exist() {
        let pool = crate::testdb::fresh_pool().await;
        let proposals = [
            ("refused-action", "pending"),
            ("refused-action", "pending"),
            ("refused-action", "dismissed"),
            ("refused-action", "made-up"),
            ("action-approval", "pending"),
        ];
        for (kind, status) in proposals {
            sqlx::query(
                "INSERT INTO proposals (kind, status, reasoning, created_at)
                 VALUES (?, ?, 'r', '2026-10-16T10:00:00Z')",
            )
            .bind(kind)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        }
        let refused = window_read(&pool, None).await.refused_actions;
        assert_eq!(refused.total, 4);
        let expected = BTreeMap::from([
            ("pending".to_owned(), 2),
            ("dismissed".to_owned(), 1),
            ("made-up".to_owned(), 1),
        ]);
        assert_eq!(refused.by_status, expected);
    }
}
