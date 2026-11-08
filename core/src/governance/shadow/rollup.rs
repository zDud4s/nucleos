//! Retention for `shadow_decisions` (review finding scale#0, migration `0291_shadow_rollup.sql`).
//!
//! The table gets a row on every governed tool call of a `shadow` or `worktree` run and nothing
//! ever removed one. The owner's decision: raw rows older than [`RAW_RETENTION_DAYS`] go, and
//! before they go their counts are folded into `shadow_decision_days` (one row per day, project,
//! run mode, action class and policy digest) and their distinct reviewed actions (SHA-256
//! hashes, never the raw `tool_input`) into
//! `shadow_reviewed_actions`, both kept forever. The readers that look past the window —
//! `shadow::scoreboard`, `shadow::shadow_readiness` and the analytics classifier block — read
//! the raw rows and the rollup together, so their totals do not move when a day ages out.
//!
//! **Idempotent by construction, not by bookkeeping.** One transaction per day folds a set of
//! rows and deletes exactly that set, so a row is counted in the rollup exactly when it stops
//! being counted raw. Re-running a day finds nothing left to fold and adds nothing; a crash
//! mid-day rolls the whole day back. Only complete days are touched: the cutoff is a UTC
//! midnight at least [`RAW_RETENTION_DAYS`] ago, and today is never older than that.
//!
//! **What is never pruned, whatever its age**, because a person is still going to look at it:
//! - an unreviewed decision of a `shadow`-mode run — the review queue (`shadow::list_unreviewed`)
//!   and the WIP brake (`wip::open_review_items`) count it, and its verdict is still to come;
//! - a decision an open item of the judge's review queue joins for its `tool_input`
//!   (`judge::review::list_unreviewed`, an INNER join: pruning the row would drop the item);
//! - a decision an open item of the resolver's queue joins (`judge::resolve_review`).
//!
//! Such a row is folded on the first sweep after it leaves its queue, into the day it was
//! created. That is why every rollup counter is added to on conflict rather than written once.
//!
//! What is lost, deliberately: the per-row detail of old decisions — `GET /runs/{id}/stop` of a
//! run older than the window shows no decisions leading up to its stop, and the judge's
//! "recent actions" of such a run are empty. Both describe a run long finished.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use sqlx::SqlitePool;

use super::{AGREE_CASE, ReviewedActionRow, TALLY_NAMES, action_hash, tally_select};

/// How long a raw `shadow_decisions` row is kept before it is folded into the daily rollup and
/// deleted. The same 30 days as a run's transcript (`DEFAULT_TRANSCRIPT_RETENTION_DAYS`), and long
/// enough that the analytics windows of 7 and 30 days read raw rows only.
pub const RAW_RETENTION_DAYS: i64 = 30;

/// What one sweep folded and deleted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    /// Days that had at least one row folded.
    pub days: u64,
    /// Raw rows deleted, each one now counted in the rollup.
    pub rows: u64,
}

/// The rows of one day (`?1` <= `created_at` < `?2`) that may go: everything except what a person
/// is still going to look at (see the module comment). Written against `shadow_decisions LEFT JOIN
/// runs` — LEFT so a decision whose run row is gone still ages out.
///
/// The queue exemptions are uncorrelated `NOT IN` lists, evaluated once per statement: neither
/// `judge_verdicts.shadow_decision_id` nor `judge_resolutions.event_ref` is indexed, and a
/// correlated `NOT EXISTS` would scan the verdicts once per decision. Each list filters out NULLs,
/// without which `NOT IN` is never true. Each mirrors its queue's own conditions — an action leaves
/// the judge's queue once any verdict of it in the project has a human answer, a resolver unit once
/// any row of it has a human outcome, and `moot` rows never enter — so a row is held only while
/// its item can actually be shown.
const PRUNABLE: &str = "shadow_decisions.created_at >= ?1 AND shadow_decisions.created_at < ?2
    AND NOT (COALESCE(runs.mode, '') = 'shadow' AND shadow_decisions.human_verdict IS NULL)
    AND shadow_decisions.id NOT IN (
        SELECT jv.shadow_decision_id FROM judge_verdicts jv
        WHERE jv.shadow_decision_id IS NOT NULL
          AND jv.human_verdict IS NULL
          AND jv.band IN ('allow', 'deny')
          AND NOT EXISTS (
              SELECT 1 FROM judge_verdicts seen
              JOIN runs seen_run ON seen_run.id = seen.run_id
              JOIN runs jv_run ON jv_run.id = jv.run_id
              WHERE seen_run.project_id = jv_run.project_id
                AND seen.tool_name = jv.tool_name
                AND seen.tool_input_digest = jv.tool_input_digest
                AND seen.human_verdict IS NOT NULL))
    AND shadow_decisions.id NOT IN (
        SELECT jr.event_ref FROM judge_resolutions jr
        WHERE jr.event_ref IS NOT NULL
          AND jr.event <> 'gate_failed'
          AND jr.judge_outcome IS NOT NULL
          AND jr.final_outcome <> 'moot'
          AND jr.human_outcome IS NULL
          AND NOT EXISTS (
              SELECT 1 FROM judge_resolutions seen
              WHERE seen.lineage_root_id = jr.lineage_root_id
                AND seen.event = jr.event
                AND seen.tool_input_digest = jr.tool_input_digest
                AND seen.human_outcome IS NOT NULL))";

/// Adds the day's prunable rows to `shadow_decision_days`, counted by `shadow::tally_select` — the
/// very expression `scoreboard` counts the live rows with. `oldest_unreviewed_at` combines by MIN
/// (scalar `MIN` of a NULL is NULL, hence the COALESCE); every other counter by sum.
fn fold_days_sql() -> String {
    let tally = tally_select();
    let mut adds: Vec<String> = TALLY_NAMES
        .split(',')
        .map(str::trim)
        .filter(|name| *name != "oldest_unreviewed_at")
        .map(|name| format!("{name} = {name} + excluded.{name}"))
        .collect();
    adds.push(
        "oldest_unreviewed_at = COALESCE(MIN(oldest_unreviewed_at, excluded.oldest_unreviewed_at), \
         oldest_unreviewed_at, excluded.oldest_unreviewed_at)"
            .to_owned(),
    );
    let adds = adds.join(",\n             ");
    format!(
        "INSERT INTO shadow_decision_days
             (day, project_id, mode, action_class, policy_digest, {TALLY_NAMES})
         SELECT ?1, COALESCE(runs.project_id, ''), COALESCE(runs.mode, ''),
                shadow_decisions.action_class, COALESCE(shadow_decisions.policy_digest, ''),
                {tally}
         FROM shadow_decisions LEFT JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE {PRUNABLE}
         GROUP BY COALESCE(runs.project_id, ''), COALESCE(runs.mode, ''),
                  shadow_decisions.action_class, COALESCE(shadow_decisions.policy_digest, '')
         ON CONFLICT (day, project_id, mode, action_class, policy_digest) DO UPDATE SET
             {adds}"
    )
}

/// Selects the distinct reviewed actions of the day's prunable `shadow`-mode rows — the unit
/// `shadow_readiness` counts in — with whether a person ever disagreed about one (MAX, so a
/// disagreement is never forgotten by a later agreement). Raw, because SQLite cannot hash:
/// [`keep_actions`] hashes them and stores only the hash.
fn select_actions_sql() -> String {
    format!(
        "SELECT COALESCE(runs.project_id, ''), shadow_decisions.action_class,
                shadow_decisions.tool_name, COALESCE(shadow_decisions.tool_input, ''),
                MAX(CASE WHEN ({AGREE_CASE}) = 0 THEN 1 ELSE 0 END)
         FROM shadow_decisions LEFT JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE {PRUNABLE}
           AND runs.mode = 'shadow' AND shadow_decisions.human_verdict IS NOT NULL
         GROUP BY COALESCE(runs.project_id, ''), shadow_decisions.action_class,
                  shadow_decisions.tool_name, COALESCE(shadow_decisions.tool_input, '')"
    )
}

/// Upserts one reviewed action by its hash; the raw `tool_input` is never written.
const KEEP_ACTION_SQL: &str =
    "INSERT INTO shadow_reviewed_actions (project_id, action_class, action_hash, disagreed)
     VALUES (?, ?, ?, ?)
     ON CONFLICT (project_id, action_class, action_hash) DO UPDATE SET
         disagreed = MAX(disagreed, excluded.disagreed)";

async fn keep_actions(
    tx: &mut sqlx::SqliteConnection,
    select: &str,
    day: &str,
    next: &str,
) -> sqlx::Result<()> {
    let rows: Vec<ReviewedActionRow> = sqlx::query_as(sqlx::AssertSqlSafe(select.to_owned()))
        .bind(day)
        .bind(next)
        .fetch_all(&mut *tx)
        .await?;
    for (project, class, name, input, disagreed) in rows {
        sqlx::query(KEEP_ACTION_SQL)
            .bind(project)
            .bind(class)
            .bind(action_hash(&name, &input))
            .bind(disagreed)
            .execute(&mut *tx)
            .await?;
    }
    Ok(())
}

fn delete_sql() -> String {
    format!(
        "DELETE FROM shadow_decisions WHERE id IN (
             SELECT shadow_decisions.id
             FROM shadow_decisions LEFT JOIN runs ON runs.id = shadow_decisions.run_id
             WHERE {PRUNABLE})"
    )
}

/// Folds and deletes every prunable row of the complete days older than `retain_days` before
/// `now`, one transaction per day, oldest first.
///
/// `retain_days <= 0` does nothing, the reading `runs::prune_transcripts` gives the same typo.
/// The cutoff is a `YYYY-MM-DD` string and `created_at` is RFC 3339 starting with its date, so
/// `created_at < cutoff` is exactly "created on an earlier day", whatever the time part looks like.
pub async fn prune(
    pool: &SqlitePool,
    retain_days: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<Pruned> {
    if retain_days <= 0 {
        return Ok(Pruned::default());
    }
    let cutoff = (now.date_naive() - Duration::days(retain_days)).to_string();
    let days: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT substr(created_at, 1, 10) FROM shadow_decisions
         WHERE created_at < ? ORDER BY 1",
    )
    .bind(&cutoff)
    .fetch_all(pool)
    .await?;

    // Built once: all three are the same text every day, only the bound day changes.
    let (fold_days, select_actions, delete) = (fold_days_sql(), select_actions_sql(), delete_sql());
    let mut pruned = Pruned::default();
    for day in days {
        let Ok(date) = NaiveDate::parse_from_str(&day, "%Y-%m-%d") else {
            // A `created_at` that does not start with a date was not written by `record_decision`;
            // leaving it is safer than guessing which day it belongs to.
            tracing::warn!(%day, "shadow: a decision's created_at is not a date, left unpruned");
            continue;
        };
        let next = (date + Duration::days(1)).to_string();
        // `AssertSqlSafe` because the text is built at run time. Safe by construction: the only
        // interpolations are this module's and `shadow`'s private consts; the day is bound.
        let mut tx = pool.begin().await?;
        sqlx::query(sqlx::AssertSqlSafe(fold_days.clone()))
            .bind(&day)
            .bind(&next)
            .execute(&mut *tx)
            .await?;
        keep_actions(&mut tx, &select_actions, &day, &next).await?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(delete.clone()))
            .bind(&day)
            .bind(&next)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if rows > 0 {
            pruned.days += 1;
            pruned.rows += rows;
        }
    }
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::governance::shadow::{scoreboard, set_verdict, shadow_readiness};

    /// 2026-11-08 noon: with 30 days the cutoff is 2026-10-09, so `OLD` is folded and `RECENT` kept.
    fn now() -> DateTime<Utc> {
        "2026-11-08T12:00:00Z".parse().unwrap()
    }
    const OLD: &str = "2026-09-01";
    const RECENT: &str = "2026-11-01";

    async fn run(pool: &SqlitePool, project: &str, mode: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'p', 'completed', ?, '2026-09-01T00:00:00Z') RETURNING id",
        )
        .bind(project)
        .bind(mode)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// One decision: `(day, class, decision, verdict, input)`, created at 10:00 UTC of `day`.
    async fn decision(
        pool: &SqlitePool,
        run_id: i64,
        (day, class, decision, verdict, input): (&str, &str, &str, Option<&str>, &str),
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO shadow_decisions
             (run_id, tool_name, tool_input, decision, reason, action_class, classifier_version,
              policy_digest, human_verdict, reviewed_at, created_at)
             VALUES (?, 'Bash', ?, ?, 'r', ?, 1, 'digest-a', ?, NULL, ? || 'T10:00:00+00:00')
             RETURNING id",
        )
        .bind(run_id)
        .bind(input)
        .bind(decision)
        .bind(class)
        .bind(verdict)
        .bind(day)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn raw_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM shadow_decisions")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// `(day, project, mode, class, total, reviewed, agree, unreviewed, oldest_unreviewed_at)` of
    /// every rollup row, in key order.
    type DayRow = (
        String,
        String,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        Option<String>,
    );

    async fn day_rows(pool: &SqlitePool) -> Vec<DayRow> {
        sqlx::query_as(
            "SELECT day, project_id, mode, action_class, total, reviewed, agree, unreviewed,
                    oldest_unreviewed_at
             FROM shadow_decision_days ORDER BY day, project_id, mode, action_class",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// A complete old day is folded with the scoreboard's own arithmetic, its rows deleted, and a
    /// second sweep adds nothing: the counts after two runs are the counts after one.
    #[tokio::test]
    async fn a_complete_day_folds_once_and_a_rerun_adds_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let shadow = run(&pool, "p", "shadow").await;
        let worktree = run(&pool, "p", "worktree").await;
        decision(
            &pool,
            shadow,
            (OLD, "destructive", "deny", Some("reject"), "a"),
        )
        .await;
        decision(
            &pool,
            shadow,
            (OLD, "destructive", "allow", Some("reject"), "b"),
        )
        .await;
        decision(&pool, worktree, (OLD, "destructive", "allow", None, "c")).await;

        let first = prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap();
        assert_eq!(first, Pruned { days: 1, rows: 3 });
        assert_eq!(raw_count(&pool).await, 0);
        let expected: Vec<DayRow> = vec![
            (
                OLD.into(),
                "p".into(),
                "shadow".into(),
                "destructive".into(),
                2,
                2,
                1,
                0,
                None,
            ),
            (
                OLD.into(),
                "p".into(),
                "worktree".into(),
                "destructive".into(),
                1,
                0,
                0,
                1,
                Some(format!("{OLD}T10:00:00+00:00")),
            ),
        ];
        assert_eq!(day_rows(&pool).await, expected);

        let second = prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap();
        assert_eq!(second, Pruned::default());
        assert_eq!(
            day_rows(&pool).await,
            expected,
            "a rerun must not double-count"
        );
    }

    /// Only days wholly before the cutoff are touched: a row inside the window stays raw and has no
    /// rollup row, and so does a row created at the very first instant of the cutoff day.
    #[tokio::test]
    async fn rows_inside_the_window_are_neither_folded_nor_deleted() {
        let pool = crate::testdb::fresh_pool().await;
        let worktree = run(&pool, "p", "worktree").await;
        decision(&pool, worktree, (RECENT, "net", "allow", None, "a")).await;
        sqlx::query(
            "INSERT INTO shadow_decisions (run_id, tool_name, decision, action_class,
                                           classifier_version, created_at)
             VALUES (?, 'Bash', 'allow', 'net', 1, '2026-10-09T00:00:00+00:00')",
        )
        .bind(worktree)
        .execute(&pool)
        .await
        .unwrap();
        decision(&pool, worktree, ("2026-10-08", "net", "allow", None, "b")).await;

        let pruned = prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap();
        assert_eq!(pruned, Pruned { days: 1, rows: 1 });
        assert_eq!(raw_count(&pool).await, 2);
        let days: Vec<String> = day_rows(&pool).await.into_iter().map(|row| row.0).collect();
        assert_eq!(days, vec!["2026-10-08".to_owned()]);
    }

    /// An unreviewed decision of a shadow run is the review queue: it is not deleted however old,
    /// and once a person answers it the next sweep folds it into the day it was created, adding to
    /// what that day already held.
    #[tokio::test]
    async fn an_open_review_item_is_kept_until_answered_then_folded_into_its_own_day() {
        let pool = crate::testdb::fresh_pool().await;
        let shadow = run(&pool, "p", "shadow").await;
        decision(
            &pool,
            shadow,
            (OLD, "destructive", "deny", Some("reject"), "a"),
        )
        .await;
        let open = decision(&pool, shadow, (OLD, "destructive", "deny", None, "b")).await;

        assert_eq!(
            prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap(),
            Pruned { days: 1, rows: 1 }
        );
        let queue = crate::governance::shadow::list_unreviewed(&pool, "p")
            .await
            .unwrap();
        assert_eq!(queue.iter().map(|row| row.id).collect::<Vec<_>>(), [open]);
        let before = scoreboard(&pool, "p").await.unwrap();
        assert_eq!((before[0].total, before[0].unreviewed), (2, 1));

        assert!(set_verdict(&pool, open, "reject").await.unwrap());
        assert_eq!(
            prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap(),
            Pruned { days: 1, rows: 1 }
        );
        assert_eq!(raw_count(&pool).await, 0);
        let rows = day_rows(&pool).await;
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].4, rows[0].5, rows[0].6, rows[0].7), (2, 2, 2, 0));
    }

    /// The judge's queue joins the decision for its `tool_input`: an old decision an open verdict
    /// points at survives the sweep, so the item is still listed, and goes once it is answered.
    #[tokio::test]
    async fn an_open_judge_item_keeps_the_decision_it_shows() {
        let pool = crate::testdb::fresh_pool().await;
        let worktree = run(&pool, "p", "worktree").await;
        let shown = decision(&pool, worktree, (OLD, "net", "allow", None, "a")).await;
        let verdict: i64 = sqlx::query_scalar(
            "INSERT INTO judge_verdicts (run_id, shadow_decision_id, tool_name, tool_input_digest,
                                         action_class, classifier_decision, judge, model,
                                         questions_version, band, final_decision, created_at)
             VALUES (?, ?, 'Bash', 'd', 'net', 'allow', 'observe', 'm', 1, 'allow', 'allow',
                     '2026-09-01T10:00:00Z')
             RETURNING id",
        )
        .bind(worktree)
        .bind(shown)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(
            prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap(),
            Pruned::default()
        );
        let queue = crate::judge::list_unreviewed(&pool, "p").await.unwrap();
        assert_eq!(
            queue.iter().map(|item| item.id).collect::<Vec<_>>(),
            [verdict]
        );

        sqlx::query("UPDATE judge_verdicts SET human_verdict = 'approve' WHERE id = ?")
            .bind(verdict)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap(),
            Pruned { days: 1, rows: 1 }
        );
    }

    /// The point of the rollup: the scoreboard and readiness give the same answer before and after
    /// a sweep. Readiness is the discriminating half — it counts DISTINCT reviewed actions, ten of
    /// them for a ready class, so it only survives if the pruned actions' keys were kept — and the
    /// repeat of action `a` on a recent day must still count once.
    #[tokio::test]
    async fn readers_return_the_same_totals_before_and_after_pruning() {
        let pool = crate::testdb::fresh_pool().await;
        let shadow = run(&pool, "p", "shadow").await;
        let worktree = run(&pool, "p", "worktree").await;
        let other = run(&pool, "q", "shadow").await;
        for n in 0..10 {
            let input = format!("rm-{n}");
            decision(
                &pool,
                shadow,
                (OLD, "destructive", "deny", Some("reject"), input.as_str()),
            )
            .await;
        }
        decision(
            &pool,
            shadow,
            (RECENT, "destructive", "deny", Some("reject"), "rm-0"),
        )
        .await;
        // An action rejected while allowed, then approved: never agreement, on either side.
        decision(&pool, shadow, (OLD, "net", "allow", Some("reject"), "curl")).await;
        decision(
            &pool,
            shadow,
            (RECENT, "net", "allow", Some("approve"), "curl"),
        )
        .await;
        decision(&pool, shadow, (OLD, "net", "allow", None, "open")).await;
        decision(&pool, worktree, (OLD, "net", "pending_approval", None, "w")).await;
        decision(
            &pool,
            worktree,
            ("2026-08-20", "net", "allow", Some("approve"), "w2"),
        )
        .await;
        decision(&pool, other, (OLD, "net", "allow", Some("approve"), "x")).await;

        let board_p = scoreboard(&pool, "p").await.unwrap();
        let board_q = scoreboard(&pool, "q").await.unwrap();
        let ready = shadow_readiness(&pool).await.unwrap();
        assert_eq!(ready.get("p"), Some(&(1, 2, 1)), "{ready:?}");

        let pruned = prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap();
        // Ten `rm-*`, the old `curl`, `w`, `w2` and `x`: everything old but the open shadow item.
        assert_eq!(pruned, Pruned { days: 2, rows: 14 });

        assert_eq!(scoreboard(&pool, "p").await.unwrap(), board_p);
        assert_eq!(scoreboard(&pool, "q").await.unwrap(), board_q);
        assert_eq!(shadow_readiness(&pool).await.unwrap(), ready);

        // And the sweep after changes nothing either.
        assert_eq!(
            prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap(),
            Pruned::default()
        );
        assert_eq!(scoreboard(&pool, "p").await.unwrap(), board_p);
        assert_eq!(shadow_readiness(&pool).await.unwrap(), ready);
    }

    /// Nine distinct reviewed actions are not ten, wherever their rows are: a repeat of a pruned
    /// action on a live day must not be counted as a second action, and one new action is what
    /// makes the class ready.
    #[tokio::test]
    async fn a_distinct_action_split_across_the_window_counts_once() {
        let pool = crate::testdb::fresh_pool().await;
        let shadow = run(&pool, "p", "shadow").await;
        for n in 0..9 {
            let input = format!("rm-{n}");
            decision(
                &pool,
                shadow,
                (OLD, "destructive", "deny", Some("reject"), input.as_str()),
            )
            .await;
        }
        prune(&pool, RAW_RETENTION_DAYS, now()).await.unwrap();
        decision(
            &pool,
            shadow,
            (RECENT, "destructive", "deny", Some("reject"), "rm-0"),
        )
        .await;
        assert_eq!(
            shadow_readiness(&pool).await.unwrap().get("p"),
            Some(&(0, 1, 0))
        );
        decision(
            &pool,
            shadow,
            (RECENT, "destructive", "deny", Some("reject"), "rm-9"),
        )
        .await;
        assert_eq!(
            shadow_readiness(&pool).await.unwrap().get("p"),
            Some(&(1, 1, 1))
        );
    }

    /// A zero or negative window is a typo, not a policy: it removes nothing.
    #[tokio::test]
    async fn a_non_positive_window_prunes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let worktree = run(&pool, "p", "worktree").await;
        decision(&pool, worktree, (OLD, "net", "allow", None, "a")).await;
        assert_eq!(prune(&pool, 0, now()).await.unwrap(), Pruned::default());
        assert_eq!(raw_count(&pool).await, 1);
    }
}
