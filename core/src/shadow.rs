use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, SqlitePool};

use crate::classifier::{self, Classification};

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ShadowDecision {
    pub id: i64,
    pub run_id: i64,
    pub tool_name: String,
    pub tool_input: Option<String>,
    pub decision: String,
    pub reason: Option<String>,
    pub action_class: String,
    pub classifier_version: i64,
    pub human_verdict: Option<String>,
    pub reviewed_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, FromRow)]
pub struct ClassTally {
    pub mode: String,
    pub action_class: String,
    pub total: i64,
    pub would_allow: i64,
    pub would_pend: i64,
    pub would_deny: i64,
    pub reviewed: i64,
    pub agree: i64,
    pub disagree: i64,
}

pub async fn record_decision(
    pool: &SqlitePool,
    run_id: i64,
    tool_name: &str,
    tool_input: &Value,
    classification: &Classification,
) -> sqlx::Result<i64> {
    let created_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "INSERT INTO shadow_decisions
         (run_id, tool_name, tool_input, decision, reason, action_class,
          classifier_version, human_verdict, reviewed_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
    )
    .bind(run_id)
    .bind(tool_name)
    .bind(tool_input.to_string())
    .bind(&classification.decision.decision)
    .bind(&classification.reason)
    .bind(classification.action_class)
    .bind(classifier::CLASSIFIER_VERSION as i64)
    .bind(created_at)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

pub async fn list_unreviewed(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<Vec<ShadowDecision>> {
    sqlx::query_as(
        "SELECT shadow_decisions.*
         FROM shadow_decisions
         JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE runs.project_id = ? AND shadow_decisions.human_verdict IS NULL
         ORDER BY shadow_decisions.id",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// The only two verdicts the agreement arithmetic understands. Anything else was stored happily
/// and then counted as reviewed-and-disagreeing, which silently pushed the readiness ratio down
/// and could hold promotion back forever with no way to see why.
pub const VALID_VERDICTS: &[&str] = &["approve", "reject"];

/// Records a human verdict, exactly once, and only if it is one this can count. `Ok(false)` when the
/// verdict is not recognised, when the id is unknown, or when the decision was already judged —
/// `http::post_shadow_verdict` answers 404 to all three.
///
/// The `human_verdict IS NULL` guard is the load-bearing half, and mirrors `proposals::transition`,
/// which has always carried `AND status = 'pending'`. Without it the UPDATE matched on the id alone:
/// a replay rewrote `reviewed_at` and still answered 204, contradicting the promise written at that
/// handler ("a verdict is recorded once — replaying it answers 404") and silently moving the only
/// record of WHEN a decision was judged.
pub async fn set_verdict(pool: &SqlitePool, id: i64, verdict: &str) -> sqlx::Result<bool> {
    if !VALID_VERDICTS.contains(&verdict) {
        return Ok(false);
    }
    let reviewed_at = chrono::Utc::now().to_rfc3339();
    let result = sqlx::query(
        "UPDATE shadow_decisions SET human_verdict = ?, reviewed_at = ?
         WHERE id = ? AND human_verdict IS NULL",
    )
    .bind(verdict)
    .bind(reviewed_at)
    .bind(id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

/// The asymmetric agreement rule, as a SQL expression yielding 1 when the human's verdict matched
/// the classifier. `approve` agrees only with `allow`; `reject` agrees with both `deny` and
/// `pending_approval` (rejecting an action the classifier already withheld IS agreement).
///
/// Used by `scoreboard`, which reports ACTIVITY: how many decisions were taken and how the human
/// judged each one. `shadow_readiness` deliberately does not share it — the gate counts distinct
/// ACTIONS (see `REVIEWED_DISTINCT`), because ten reviews of one command are ten data points about
/// the same thing. Same rule for what counts as agreement, applied to different units, and the
/// scoreboard's column headings are what tell the two apart on screen.
const AGREE_CASE: &str = "CASE
    WHEN shadow_decisions.human_verdict = 'approve'
         AND shadow_decisions.decision = 'allow' THEN 1
    WHEN shadow_decisions.human_verdict = 'reject'
         AND shadow_decisions.decision IN ('deny', 'pending_approval') THEN 1
    ELSE 0
END";

/// Distinct reviewed ACTIONS, not reviewed rows.
///
/// Ten reviews of the same `ls` are one piece of evidence about the classifier, not ten. Counting
/// rows meant a single shadow run looping one harmless command, bulk-approved, cleared its class
/// and unlocked active mode having proved nothing about anything risky. The tool name joins the
/// key because a `Read` of a path and a `Bash` touching it are different actions, and `char(31)`
/// is the unit separator, so the two fields cannot run together into a colliding string.
const REVIEWED_DISTINCT: &str =
    "COUNT(DISTINCT CASE WHEN shadow_decisions.human_verdict IS NOT NULL
        THEN shadow_decisions.tool_name || char(31) || COALESCE(shadow_decisions.tool_input, '')
    END)";

/// Distinct actions the classifier got right: reviewed, minus every action a human EVER rejected.
///
/// Subtracting disagreements rather than counting agreements, because the same action can be
/// reviewed more than once. Counting agreements directly would let one approval cancel out a
/// rejection of that very action, and an action anyone disagreed about is not evidence that the
/// classifier handles it correctly.
const AGREE_DISTINCT: &str = "COUNT(DISTINCT CASE WHEN shadow_decisions.human_verdict IS NOT NULL
        THEN shadow_decisions.tool_name || char(31) || COALESCE(shadow_decisions.tool_input, '')
    END)
    - COUNT(DISTINCT CASE
        WHEN shadow_decisions.human_verdict IS NOT NULL
             AND NOT (
                 (shadow_decisions.human_verdict = 'approve' AND shadow_decisions.decision = 'allow')
                 OR (shadow_decisions.human_verdict = 'reject'
                     AND shadow_decisions.decision IN ('deny', 'pending_approval'))
             )
        THEN shadow_decisions.tool_name || char(31) || COALESCE(shadow_decisions.tool_input, '')
    END)";

pub async fn scoreboard(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<ClassTally>> {
    let sql = format!(
        "SELECT
             runs.mode AS mode,
             shadow_decisions.action_class,
             COUNT(*) AS total,
             SUM(CASE WHEN shadow_decisions.decision = 'allow' THEN 1 ELSE 0 END) AS would_allow,
             SUM(CASE WHEN shadow_decisions.decision = 'pending_approval' THEN 1 ELSE 0 END) AS would_pend,
             SUM(CASE WHEN shadow_decisions.decision = 'deny' THEN 1 ELSE 0 END) AS would_deny,
             SUM(CASE WHEN shadow_decisions.human_verdict IS NOT NULL THEN 1 ELSE 0 END) AS reviewed,
             SUM({AGREE_CASE}) AS agree,
             SUM(CASE
                 WHEN shadow_decisions.human_verdict IS NOT NULL AND ({AGREE_CASE}) = 0 THEN 1
                 ELSE 0
             END) AS disagree
         FROM shadow_decisions
         JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE runs.project_id = ?
         GROUP BY runs.mode, shadow_decisions.action_class
         ORDER BY runs.mode, shadow_decisions.action_class"
    );

    // `AssertSqlSafe` because sqlx 0.9 only trusts `&'static str` by default. Safe here by
    // construction: the only interpolated fragment is the private `AGREE_CASE` const, and the
    // project id stays a bound parameter — no caller input reaches the SQL text.
    sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// The shadow-exit bar (spec §8.2/§8.8, `.ai/decisions.md` 2026-07-27): an action class earns
/// promotion once enough of its shadow decisions have been reviewed AND the classifier agreed with
/// the human on nearly all of them.
///
/// **This is the single source of truth for the rule.** The shell reads the computed
/// `classes_ready`/`promotable` off the daemon rather than recomputing them, so the button the user
/// sees and the bar the product enforces can never gate on different numbers.
pub const READINESS_MIN_REVIEWED: i64 = 10;
pub const READINESS_MIN_AGREE_PERCENT: i64 = 95;

/// Named hazard classes whose ready, uniformly withheld decisions demonstrate restraint.
///
/// Agreeing with the classifier's caution about something it did not recognise says nothing about
/// its judgement on the hazards it did. This allowlist therefore fails closed: a new classifier
/// class is not evidence of restraint until it is deliberately added here.
const RESTRAINT_EVIDENCE_CLASSES: &[&str] = &[
    "push-merge-deploy",
    "destructive",
    "self-governing-file",
    "outside-workspace",
    "executes-on-next-command",
];

/// `agree / reviewed >= 0.95` in integer arithmetic — a float ratio rounds at the boundary, and this
/// is exactly the boundary the gate is decided on.
pub fn class_ready(reviewed: i64, agree: i64) -> bool {
    reviewed >= READINESS_MIN_REVIEWED && agree * 100 >= READINESS_MIN_AGREE_PERCENT * reviewed
}

/// A project may leave shadow once it has exercised at least one action class, EVERY exercised
/// class clears the bar, and at least one of those ready classes contains ONLY decisions where the
/// classifier WITHHELD (`pending_approval` or `deny`). Classes never exercised don't block (a
/// project would otherwise wait forever on a `deploy` it never attempts), but zero exercised
/// classes is NOT promotable — "no evidence" must not read as "all the evidence is good".
///
/// The withheld requirement is the part that looks redundant and is not: a corpus made entirely of
/// `allow` decisions validates the classifier's PERMISSIVENESS — that it lets through what it
/// should — and says nothing whatsoever about its RESTRAINT. Restraint is the only property that
/// matters once the project starts acting on its own, so it has to have been checked before it may.
pub fn promotable(classes_ready: i64, classes_total: i64, withheld_classes_ready: i64) -> bool {
    classes_total > 0 && classes_ready == classes_total && withheld_classes_ready > 0
}

/// `(classes_ready, classes_total, withheld_classes_ready)` per project, over SHADOW-mode decisions
/// only.
///
/// Shadow-mode only because promotion OUT of shadow is earned by evidence gathered IN shadow: a
/// `worktree`-mode decision was actually enforced, not a hypothetical the human could still overrule.
/// Projects with no shadow decisions are absent from the map (the caller reads that as `(0, 0, 0)`).
///
/// Each action class contributes exactly one row, even when historical classifier versions recorded
/// different decisions for it. Review and agreement counts therefore cover the whole class. A class
/// counts as withheld only when every recorded decision is `pending_approval` or `deny`; any
/// `allow` makes the conservative result "not withheld" so mixed evidence cannot unlock promotion.
pub async fn shadow_readiness(
    pool: &SqlitePool,
) -> sqlx::Result<std::collections::HashMap<String, (i64, i64, i64)>> {
    let sql = format!(
        "SELECT
             runs.project_id AS project_id,
             shadow_decisions.action_class AS action_class,
             MIN(CASE
                 WHEN shadow_decisions.decision IN ('pending_approval', 'deny') THEN 1
                 ELSE 0
             END) AS withheld,
             {REVIEWED_DISTINCT} AS reviewed,
             {AGREE_DISTINCT} AS agree
         FROM shadow_decisions
         JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE runs.mode = 'shadow'
         GROUP BY runs.project_id, shadow_decisions.action_class"
    );

    // Same `AssertSqlSafe` reasoning as `scoreboard`: the only interpolation is `AGREE_CASE`.
    let rows: Vec<(String, String, i64, i64, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await?;

    let mut readiness: std::collections::HashMap<String, (i64, i64, i64)> =
        std::collections::HashMap::new();
    for (project_id, action_class, withheld, reviewed, agree) in rows {
        let entry = readiness.entry(project_id).or_insert((0, 0, 0));
        entry.1 += 1;
        if class_ready(reviewed, agree) {
            entry.0 += 1;
            if withheld != 0 && RESTRAINT_EVIDENCE_CLASSES.contains(&action_class.as_str()) {
                entry.2 += 1;
            }
        }
    }
    Ok(readiness)
}

/// `(classes_ready, classes_total, withheld_classes_ready)` for one project — same rule as
/// `shadow_readiness`, used by the promotion nudge to spot the moment a project crosses the bar.
pub async fn project_readiness(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<(i64, i64, i64)> {
    Ok(shadow_readiness(pool)
        .await?
        .remove(project_id)
        .unwrap_or((0, 0, 0)))
}

/// The project a shadow decision belongs to, or `None` when the decision id is unknown.
pub async fn project_of_decision(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT runs.project_id
         FROM shadow_decisions
         JOIN runs ON runs.id = shadow_decisions.run_id
         WHERE shadow_decisions.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::{CLASSIFIER_VERSION, Classification};
    use crate::hooks::Decision;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn insert_run(pool: &sqlx::SqlitePool, project_id: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, created_at)
             VALUES (?, 'test', 'running', '2026-07-18T00:00:00Z')",
        )
        .bind(project_id)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_run_with_mode(pool: &sqlx::SqlitePool, project_id: &str, mode: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (project_id, prompt, status, mode, created_at)
             VALUES (?, 'test', 'running', ?, '2026-07-18T00:00:00Z')",
        )
        .bind(project_id)
        .bind(mode)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// A DISTINCT action each time. Readiness counts distinct actions rather than rows, so a helper
    /// that reused one `tool_input` would quietly turn every arithmetic test into one piece of
    /// evidence repeated N times — the exact thing readiness now refuses to accept.
    async fn insert_shadow(
        pool: &sqlx::SqlitePool,
        run_id: i64,
        action_class: &str,
        decision: &str,
        verdict: Option<&str>,
    ) -> i64 {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        insert_shadow_with_input(
            pool,
            run_id,
            action_class,
            decision,
            verdict,
            &format!(r#"{{"command":"cmd-{unique}"}}"#),
        )
        .await
    }

    async fn insert_shadow_with_input(
        pool: &sqlx::SqlitePool,
        run_id: i64,
        action_class: &str,
        decision: &str,
        verdict: Option<&str>,
        tool_input: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO shadow_decisions
             (run_id, tool_name, tool_input, decision, reason, action_class,
              classifier_version, human_verdict, reviewed_at, created_at)
             VALUES (?, 'Bash', ?, ?, 'test reason', ?, ?, ?,
                     CASE WHEN ? IS NULL THEN NULL ELSE '2026-07-18T01:00:00Z' END,
                     '2026-07-18T00:00:00Z')",
        )
        .bind(run_id)
        .bind(tool_input)
        .bind(decision)
        .bind(action_class)
        .bind(CLASSIFIER_VERSION as i64)
        .bind(verdict)
        .bind(verdict)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    fn classification(decision: &str, action_class: &'static str) -> Classification {
        Classification {
            decision: Decision {
                decision: decision.to_owned(),
                reason: "classified reason".to_owned(),
            },
            action_class,
            reason: "classified reason".to_owned(),
        }
    }

    #[tokio::test]
    async fn record_decision_persists_reviewable_classifier_snapshot() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "project-a").await;
        let tool_input = json!({"command": "cargo test", "nested": {"full": true}});

        let id = record_decision(
            &pool,
            run_id,
            "Bash",
            &tool_input,
            &classification("allow", "read-local"),
        )
        .await
        .unwrap();

        let row: ShadowDecision = sqlx::query_as("SELECT * FROM shadow_decisions WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.run_id, run_id);
        assert_eq!(row.tool_name, "Bash");
        assert_eq!(
            row.tool_input.as_deref(),
            Some(tool_input.to_string().as_str())
        );
        assert_eq!(row.decision, "allow");
        assert_eq!(row.reason.as_deref(), Some("classified reason"));
        assert_eq!(row.action_class, "read-local");
        assert_eq!(row.classifier_version, CLASSIFIER_VERSION as i64);
        assert_eq!(row.human_verdict, None);
        assert_eq!(row.reviewed_at, None);
        chrono::DateTime::parse_from_rfc3339(&row.created_at).unwrap();
    }

    #[tokio::test]
    async fn scoreboard_is_project_scoped_and_counts_would_decisions_by_class() {
        let pool = test_pool().await;
        let run_a = insert_run(&pool, "project-a").await;
        let run_b = insert_run(&pool, "project-b").await;
        insert_shadow(&pool, run_a, "read-local", "allow", None).await;
        insert_shadow(&pool, run_a, "read-local", "pending_approval", None).await;
        insert_shadow(&pool, run_a, "destructive", "deny", None).await;
        insert_shadow(&pool, run_b, "read-local", "deny", Some("reject")).await;

        let tallies = scoreboard(&pool, "project-a").await.unwrap();

        assert_eq!(
            tallies,
            vec![
                ClassTally {
                    mode: "real".to_owned(),
                    action_class: "destructive".to_owned(),
                    total: 1,
                    would_allow: 0,
                    would_pend: 0,
                    would_deny: 1,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
                ClassTally {
                    mode: "real".to_owned(),
                    action_class: "read-local".to_owned(),
                    total: 2,
                    would_allow: 1,
                    would_pend: 1,
                    would_deny: 0,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
            ]
        );
    }

    #[tokio::test]
    async fn scoreboard_splits_tallies_by_run_mode() {
        let pool = test_pool().await;
        let shadow_run = insert_run_with_mode(&pool, "project-a", "shadow").await;
        let worktree_run = insert_run_with_mode(&pool, "project-a", "worktree").await;
        insert_shadow(&pool, shadow_run, "read-local", "allow", None).await;
        insert_shadow(
            &pool,
            shadow_run,
            "push-merge-deploy",
            "pending_approval",
            None,
        )
        .await;
        insert_shadow(&pool, worktree_run, "read-local", "allow", None).await;
        insert_shadow(
            &pool,
            worktree_run,
            "push-merge-deploy",
            "pending_approval",
            None,
        )
        .await;

        let tallies = scoreboard(&pool, "project-a").await.unwrap();

        assert_eq!(
            tallies,
            vec![
                ClassTally {
                    mode: "shadow".to_owned(),
                    action_class: "push-merge-deploy".to_owned(),
                    total: 1,
                    would_allow: 0,
                    would_pend: 1,
                    would_deny: 0,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
                ClassTally {
                    mode: "shadow".to_owned(),
                    action_class: "read-local".to_owned(),
                    total: 1,
                    would_allow: 1,
                    would_pend: 0,
                    would_deny: 0,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
                ClassTally {
                    mode: "worktree".to_owned(),
                    action_class: "push-merge-deploy".to_owned(),
                    total: 1,
                    would_allow: 0,
                    would_pend: 1,
                    would_deny: 0,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
                ClassTally {
                    mode: "worktree".to_owned(),
                    action_class: "read-local".to_owned(),
                    total: 1,
                    would_allow: 1,
                    would_pend: 0,
                    would_deny: 0,
                    reviewed: 0,
                    agree: 0,
                    disagree: 0,
                },
            ]
        );
    }

    #[tokio::test]
    async fn scoreboard_counts_human_agreement_and_disagreement() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "project-a").await;
        insert_shadow(&pool, run_id, "governance", "allow", Some("approve")).await;
        insert_shadow(
            &pool,
            run_id,
            "governance",
            "pending_approval",
            Some("reject"),
        )
        .await;
        insert_shadow(&pool, run_id, "governance", "deny", Some("approve")).await;

        let tallies = scoreboard(&pool, "project-a").await.unwrap();

        assert_eq!(tallies[0].mode, "real");
        assert_eq!(tallies[0].reviewed, 3);
        assert_eq!(tallies[0].agree, 2);
        assert_eq!(tallies[0].disagree, 1);
    }

    #[tokio::test]
    async fn list_unreviewed_returns_only_null_verdicts_for_project() {
        let pool = test_pool().await;
        let run_a = insert_run(&pool, "project-a").await;
        let run_b = insert_run(&pool, "project-b").await;
        let expected = insert_shadow(&pool, run_a, "read-local", "allow", None).await;
        insert_shadow(&pool, run_a, "destructive", "deny", Some("reject")).await;
        insert_shadow(&pool, run_b, "read-local", "allow", None).await;

        let decisions = list_unreviewed(&pool, "project-a").await.unwrap();

        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id, expected);
        assert_eq!(decisions[0].human_verdict, None);
    }

    #[tokio::test]
    async fn set_verdict_removes_unreviewed_and_updates_scoreboard() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "project-a").await;
        let id = insert_shadow(&pool, run_id, "read-local", "allow", None).await;

        set_verdict(&pool, id, "approve").await.unwrap();

        assert!(
            list_unreviewed(&pool, "project-a")
                .await
                .unwrap()
                .is_empty()
        );
        let row: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT human_verdict, reviewed_at FROM shadow_decisions WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0.as_deref(), Some("approve"));
        chrono::DateTime::parse_from_rfc3339(row.1.as_deref().unwrap()).unwrap();
        let tally = &scoreboard(&pool, "project-a").await.unwrap()[0];
        assert_eq!(tally.mode, "real");
        assert_eq!((tally.reviewed, tally.agree, tally.disagree), (1, 1, 0));
    }

    #[tokio::test]
    async fn a_verdict_is_recorded_once_and_a_replay_leaves_it_alone() {
        let pool = test_pool().await;
        let run_id = insert_run(&pool, "project-a").await;
        let id = insert_shadow(&pool, run_id, "read-local", "allow", None).await;

        assert!(set_verdict(&pool, id, "approve").await.unwrap());
        let first: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT human_verdict, reviewed_at FROM shadow_decisions WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();

        // The replay must change nothing AND report that it changed nothing: `post_shadow_verdict`
        // turns `Ok(false)` into 404, which is the behaviour its own comment already promises.
        // Before the `human_verdict IS NULL` guard this returned true, answered 204, and moved
        // `reviewed_at` — so a double click rewrote when the decision was judged, and a correction
        // destroyed the original instant with no trace that there had been one.
        assert!(!set_verdict(&pool, id, "reject").await.unwrap());
        let after: (Option<String>, Option<String>) =
            sqlx::query_as("SELECT human_verdict, reviewed_at FROM shadow_decisions WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after, first);
        assert_eq!(after.0.as_deref(), Some("approve"));

        // An unknown id is the other honest `false`, and the handler answers it the same way.
        assert!(!set_verdict(&pool, id + 999, "approve").await.unwrap());
    }

    #[test]
    fn class_ready_needs_both_enough_reviews_and_enough_agreement() {
        // Under the review floor, however perfect the agreement.
        assert!(!class_ready(9, 9));
        // Exactly at the floor, unanimous.
        assert!(class_ready(10, 10));
        // Exactly at the rate — 19/20 is 95%, the boundary the gate is decided on.
        assert!(class_ready(20, 19));
        // Just under: 18/20 is 90%.
        assert!(!class_ready(20, 18));
        // No reviews at all is not "vacuously perfect".
        assert!(!class_ready(0, 0));
    }

    #[test]
    fn promotable_requires_evidence_not_just_the_absence_of_failure() {
        // A project that has never exercised a class has not earned anything.
        assert!(!promotable(0, 0, 0));
        assert!(promotable(1, 1, 1));
        assert!(promotable(4, 4, 1));
        // One class still short holds the whole project.
        assert!(!promotable(3, 4, 1));
    }

    #[tokio::test]
    async fn promotable_requires_a_withheld_class_not_just_reads() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        // Both exercised classes are classifier allows, and both clear the existing review bar.
        for _ in 0..10 {
            insert_shadow(&pool, run, "read-local", "allow", Some("approve")).await;
            insert_shadow(&pool, run, "vcs-local", "allow", Some("approve")).await;
        }

        let readiness = shadow_readiness(&pool).await.unwrap();
        let (classes_ready, classes_total, withheld_classes_ready) =
            readiness.get("project-a").copied().unwrap();

        assert_eq!(
            (classes_ready, classes_total, withheld_classes_ready),
            (2, 2, 0)
        );
        assert!(!promotable(
            classes_ready,
            classes_total,
            withheld_classes_ready
        ));

        pool.close().await;
    }

    #[tokio::test]
    async fn a_class_with_two_decisions_counts_as_one_class() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        // Historical classifier versions can leave one action class carrying more than one
        // decision. Both row-groups independently clear the review bar, but they are still evidence
        // for one exercised class.
        for _ in 0..10 {
            insert_shadow(&pool, run, "read-local", "allow", Some("approve")).await;
            insert_shadow(&pool, run, "read-local", "pending_approval", Some("reject")).await;
        }

        let readiness = shadow_readiness(&pool).await.unwrap();
        let (classes_ready, classes_total, withheld_classes_ready) =
            readiness.get("project-a").copied().unwrap();

        assert_eq!(
            (classes_ready, classes_total, withheld_classes_ready),
            (1, 1, 0),
            "one action class must not become two readiness classes when its decision changes"
        );
        assert!(!promotable(
            classes_ready,
            classes_total,
            withheld_classes_ready
        ));

        pool.close().await;
    }

    #[tokio::test]
    async fn promotable_once_a_withheld_class_is_reviewed_and_ready() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        // The same ready allow-only corpus is not enough by itself.
        for _ in 0..10 {
            insert_shadow(&pool, run, "read-local", "allow", Some("approve")).await;
            insert_shadow(&pool, run, "vcs-local", "allow", Some("approve")).await;
        }
        // A reviewed class where the classifier withheld supplies the missing evidence.
        for _ in 0..10 {
            insert_shadow(
                &pool,
                run,
                "push-merge-deploy",
                "pending_approval",
                Some("reject"),
            )
            .await;
        }

        let readiness = shadow_readiness(&pool).await.unwrap();
        let (classes_ready, classes_total, withheld_classes_ready) =
            readiness.get("project-a").copied().unwrap();

        assert_eq!(
            (classes_ready, classes_total, withheld_classes_ready),
            (3, 3, 1)
        );
        assert!(promotable(
            classes_ready,
            classes_total,
            withheld_classes_ready
        ));

        pool.close().await;
    }

    #[tokio::test]
    async fn a_catch_all_class_is_not_evidence_of_restraint() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        for _ in 0..READINESS_MIN_REVIEWED {
            insert_shadow(
                &pool,
                run,
                "unrecognized",
                "pending_approval",
                Some("reject"),
            )
            .await;
        }

        let readiness = project_readiness(&pool, "project-a").await.unwrap();
        pool.close().await;

        assert_eq!(readiness, (1, 1, 0));
        assert!(!promotable(readiness.0, readiness.1, readiness.2));
    }

    #[tokio::test]
    async fn a_named_hazard_class_is_evidence_of_restraint() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        for _ in 0..READINESS_MIN_REVIEWED {
            insert_shadow(
                &pool,
                run,
                "push-merge-deploy",
                "pending_approval",
                Some("reject"),
            )
            .await;
        }

        let readiness = shadow_readiness(&pool)
            .await
            .unwrap()
            .get("project-a")
            .copied()
            .unwrap();
        pool.close().await;

        assert_eq!(readiness, (1, 1, 1));
        assert!(promotable(readiness.0, readiness.1, readiness.2));
    }

    #[tokio::test]
    async fn an_unknown_workspace_is_not_evidence_of_restraint() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        for _ in 0..READINESS_MIN_REVIEWED {
            insert_shadow(
                &pool,
                run,
                "no-workspace",
                "pending_approval",
                Some("reject"),
            )
            .await;
        }

        let readiness = project_readiness(&pool, "project-a").await.unwrap();
        pool.close().await;

        assert_eq!(readiness, (1, 1, 0));
        assert!(!promotable(readiness.0, readiness.1, readiness.2));
    }

    #[tokio::test]
    async fn zero_exercised_classes_is_still_not_promotable() {
        let pool = test_pool().await;

        let readiness = shadow_readiness(&pool).await.unwrap();
        let (classes_ready, classes_total, withheld_classes_ready) =
            readiness.get("project-a").copied().unwrap_or((0, 0, 0));

        assert_eq!(
            (classes_ready, classes_total, withheld_classes_ready),
            (0, 0, 0)
        );
        assert!(!promotable(
            classes_ready,
            classes_total,
            withheld_classes_ready
        ));

        pool.close().await;
    }

    #[tokio::test]
    async fn shadow_readiness_counts_ready_classes_per_project() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        // `read-local` clears the bar: 10 reviewed, all agreeing.
        for _ in 0..10 {
            insert_shadow(&pool, run, "read-local", "allow", Some("approve")).await;
        }
        // `push-merge-deploy` is exercised but nowhere near reviewed enough.
        insert_shadow(
            &pool,
            run,
            "push-merge-deploy",
            "pending_approval",
            Some("reject"),
        )
        .await;

        let readiness = shadow_readiness(&pool).await.unwrap();

        // The only ready class is an `allow` one, so nothing withheld has been validated yet.
        assert_eq!(readiness.get("project-a").copied(), Some((1, 2, 0)));
        assert!(!promotable(1, 2, 0));
    }

    /// The promotion bar exists to say the classifier has been checked against enough real
    /// variety. One command looped ten times and bulk-approved is one thing checked ten times, and
    /// counting rows let exactly that unlock active mode.
    #[tokio::test]
    async fn repeating_one_action_does_not_earn_promotion() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        for _ in 0..10 {
            insert_shadow_with_input(
                &pool,
                run,
                "read-local",
                "allow",
                Some("approve"),
                r#"{"command":"ls"}"#,
            )
            .await;
        }

        let readiness = shadow_readiness(&pool).await.unwrap();

        assert_eq!(
            readiness.get("project-a").copied(),
            Some((0, 1, 0)),
            "one action reviewed ten times is one piece of evidence"
        );
        assert!(!promotable(0, 1, 0));
    }

    /// A person disagreeing about an action means the classifier does not handle it, however many
    /// times it was also approved. Counting agreements per row let a later approval cancel out an
    /// earlier rejection of the very same command.
    #[tokio::test]
    async fn an_action_ever_rejected_never_counts_as_agreement() {
        let pool = test_pool().await;
        let run = insert_run_with_mode(&pool, "project-a", "shadow").await;

        // Nine distinct actions the human agreed with.
        for _ in 0..9 {
            insert_shadow(&pool, run, "read-local", "allow", Some("approve")).await;
        }
        // A tenth action, rejected once and approved once — the classifier called it `allow`.
        let contested = r#"{"command":"curl http://evil.test"}"#;
        insert_shadow_with_input(&pool, run, "read-local", "allow", Some("reject"), contested)
            .await;
        insert_shadow_with_input(
            &pool,
            run,
            "read-local",
            "allow",
            Some("approve"),
            contested,
        )
        .await;

        let readiness = shadow_readiness(&pool).await.unwrap();

        // 10 distinct actions reviewed, 9 agreed = 90%, under the 95% bar.
        assert_eq!(readiness.get("project-a").copied(), Some((0, 1, 0)));
    }

    #[tokio::test]
    async fn shadow_readiness_ignores_non_shadow_runs() {
        let pool = test_pool().await;
        let worktree_run = insert_run_with_mode(&pool, "project-a", "worktree").await;
        for _ in 0..10 {
            insert_shadow(&pool, worktree_run, "read-local", "allow", Some("approve")).await;
        }

        // Promotion out of shadow is earned by shadow evidence; an enforced worktree decision was
        // never a hypothetical the human could have overruled, so it must not count toward the bar.
        assert_eq!(
            shadow_readiness(&pool).await.unwrap().get("project-a"),
            None
        );
        assert_eq!(
            project_readiness(&pool, "project-a").await.unwrap(),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn project_readiness_scopes_to_one_project() {
        let pool = test_pool().await;
        let run_a = insert_run_with_mode(&pool, "project-a", "shadow").await;
        let run_b = insert_run_with_mode(&pool, "project-b", "shadow").await;
        for _ in 0..10 {
            insert_shadow(&pool, run_a, "read-local", "allow", Some("approve")).await;
        }
        insert_shadow(&pool, run_b, "read-local", "allow", None).await;

        assert_eq!(
            project_readiness(&pool, "project-a").await.unwrap(),
            (1, 1, 0)
        );
        assert_eq!(
            project_readiness(&pool, "project-b").await.unwrap(),
            (0, 1, 0)
        );
        assert_eq!(
            project_readiness(&pool, "project-c").await.unwrap(),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn project_of_decision_resolves_through_the_run() {
        let pool = test_pool().await;
        let run = insert_run(&pool, "project-a").await;
        let id = insert_shadow(&pool, run, "read-local", "allow", None).await;

        assert_eq!(
            project_of_decision(&pool, id).await.unwrap().as_deref(),
            Some("project-a")
        );
        assert_eq!(project_of_decision(&pool, id + 999).await.unwrap(), None);
    }
}
