//! The rules read/write model: findings, cross-session comparisons, feedback, rates and turn statistics.

use super::*;

/// What the rules pass needs to know about a session before it reads the rest: its project, its
/// span of time, and the two stamps that decide whether the rules still owe it work.
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct SessionHead {
    pub session_id: String,
    pub project_id: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub updated_at: String,
    /// 1 until `write_rule_results` has seen the rows as they are now.
    pub dirty: i64,
    pub rules_version: Option<String>,
}

pub async fn session_head(
    pool: &SqlitePool,
    session_id: &str,
) -> sqlx::Result<Option<SessionHead>> {
    sqlx::query_as::<_, SessionHead>(
        "SELECT session_id, project_id, started_at, ended_at, updated_at, dirty, rules_version
         FROM devtime_sessions WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
}

/// A session's spans, with their ids, in lane and time order.
pub async fn session_spans(pool: &SqlitePool, session_id: &str) -> sqlx::Result<Vec<SpanRow>> {
    sqlx::query_as::<_, SpanRow>(
        "SELECT id, session_id, lane, kind, started_at, ended_at, attempt_id, attempt_ids,
                context_tokens, waste, rule_id, confidence, parser_version
         FROM devtime_spans WHERE session_id = ? ORDER BY lane, started_at, id",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
}

/// The sessions whose rule results are missing or out of date, newest first: dirty ones, ones never
/// ruled, and ones ruled under another `rules_version` fingerprint.
pub async fn sessions_needing_rules(
    pool: &SqlitePool,
    fingerprint: &str,
    limit: u32,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT session_id FROM devtime_sessions
         WHERE dirty = 1 OR rules_version IS NULL OR rules_version <> ?
         ORDER BY updated_at DESC, session_id LIMIT ?",
    )
    .bind(fingerprint)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
}

/// One rule finding, session-scoped or cross-session. The two version columns are the engine's
/// stamps, carried on the row so a cross finding (written without a session pass) has them too.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct FindingRow {
    pub finding_key: String,
    pub project_id: String,
    pub session_id: String,
    /// One of [`FINDING_SCOPES`].
    pub scope: String,
    pub rule_id: String,
    pub rule_version: i64,
    /// One of [`LEVELS`].
    pub level: String,
    /// One of [`WASTES`].
    pub waste: String,
    /// One of [`LEVERS`]: the rule's first lever.
    pub lever: String,
    /// One of [`CONFIDENCE`].
    pub confidence: String,
    pub lane: String,
    pub started_at: String,
    pub ended_at: String,
    pub cost_ms: i64,
    pub count: i64,
    /// JSON array; empty is written as `[]`.
    pub attempt_ids: String,
    /// JSON array of the sessions a cross finding spans; empty is written as `[]`.
    pub sessions: String,
    pub rules_version: String,
    pub parser_version: i64,
}

/// The winning annotation of one span.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpanMark {
    pub span_id: i64,
    pub waste: Option<String>,
    pub rule_id: Option<String>,
    pub lever: Option<String>,
    pub finding_key: Option<String>,
}

/// One attempt's winning annotation and its three-valued verification result.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct AttemptMarkRow {
    pub attempt_id: String,
    pub session_id: String,
    pub waste: Option<String>,
    pub rule_id: Option<String>,
    pub lever: Option<String>,
    pub finding_key: Option<String>,
    /// One of [`VERIFIED`].
    pub verified: String,
    /// The rule that set `verified`, when one did.
    pub verified_by: Option<String>,
}

/// One main-lane turn's time, and how much of it a rule explains.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct TurnStatRow {
    pub session_id: String,
    pub turn_seq: i64,
    pub project_id: String,
    pub turn_class: String,
    pub started_at: String,
    pub calls: i64,
    pub active_ms: i64,
    pub explained_ms: i64,
}

/// Everything one session's rules pass writes, in one transaction.
#[derive(Debug, Clone, Default)]
pub struct RuleWrite {
    pub findings: Vec<FindingRow>,
    pub span_marks: Vec<SpanMark>,
    pub attempt_marks: Vec<AttemptMarkRow>,
    pub turn_stats: Vec<TurnStatRow>,
}

pub(super) async fn insert_finding(
    conn: &mut SqliteConnection,
    row: &FindingRow,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO devtime_findings
            (finding_key, project_id, session_id, scope, rule_id, rule_version, level, waste, lever,
             confidence, lane, started_at, ended_at, cost_ms, count, attempt_ids, sessions,
             rules_version, parser_version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(finding_key) DO NOTHING",
    )
    .bind(&row.finding_key)
    .bind(&row.project_id)
    .bind(&row.session_id)
    .bind(&row.scope)
    .bind(&row.rule_id)
    .bind(row.rule_version)
    .bind(&row.level)
    .bind(&row.waste)
    .bind(&row.lever)
    .bind(&row.confidence)
    .bind(&row.lane)
    .bind(&row.started_at)
    .bind(&row.ended_at)
    .bind(row.cost_ms)
    .bind(row.count)
    .bind(json_or_empty(&row.attempt_ids))
    .bind(json_or_empty(&row.sessions))
    .bind(&row.rules_version)
    .bind(row.parser_version)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Replaces what the rules concluded about one session, wholesale and in one transaction: its
/// session-scoped findings, the annotations on its spans, its attempt marks and its turn stats.
/// Then stamps the session with `fingerprint` and clears `dirty`, but only if `updated_at` is still
/// what `head` read: rows ingested while the rules ran leave it dirty, so the next cycle runs again.
pub async fn write_rule_results(
    pool: &SqlitePool,
    head: &SessionHead,
    write: &RuleWrite,
    fingerprint: &str,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM devtime_findings WHERE session_id = ? AND scope = 'session'")
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    for finding in &write.findings {
        insert_finding(&mut tx, finding).await?;
    }
    sqlx::query(
        "UPDATE devtime_spans SET waste = NULL, rule_id = NULL, lever = NULL, finding_key = NULL
         WHERE session_id = ?",
    )
    .bind(&head.session_id)
    .execute(&mut *tx)
    .await?;
    for mark in &write.span_marks {
        sqlx::query(
            "UPDATE devtime_spans SET waste = ?, rule_id = ?, lever = ?, finding_key = ?
             WHERE id = ? AND session_id = ?",
        )
        .bind(&mark.waste)
        .bind(&mark.rule_id)
        .bind(&mark.lever)
        .bind(&mark.finding_key)
        .bind(mark.span_id)
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM devtime_attempt_marks WHERE session_id = ?")
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    for mark in &write.attempt_marks {
        sqlx::query(
            "INSERT OR REPLACE INTO devtime_attempt_marks
                (attempt_id, session_id, waste, rule_id, lever, finding_key, verified, verified_by,
                 rules_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&mark.attempt_id)
        .bind(&mark.session_id)
        .bind(&mark.waste)
        .bind(&mark.rule_id)
        .bind(&mark.lever)
        .bind(&mark.finding_key)
        .bind(&mark.verified)
        .bind(&mark.verified_by)
        .bind(fingerprint)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM devtime_turn_stats WHERE session_id = ?")
        .bind(&head.session_id)
        .execute(&mut *tx)
        .await?;
    for stat in &write.turn_stats {
        sqlx::query(
            "INSERT OR REPLACE INTO devtime_turn_stats
                (session_id, turn_seq, project_id, turn_class, started_at, calls, active_ms,
                 explained_ms, rules_version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&stat.session_id)
        .bind(stat.turn_seq)
        .bind(&stat.project_id)
        .bind(&stat.turn_class)
        .bind(&stat.started_at)
        .bind(stat.calls)
        .bind(stat.active_ms)
        .bind(stat.explained_ms)
        .bind(fingerprint)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE devtime_sessions SET rules_version = ?, rules_at = ?,
            dirty = CASE WHEN updated_at = ? THEN 0 ELSE dirty END
         WHERE session_id = ?",
    )
    .bind(fingerprint)
    .bind(now())
    .bind(&head.updated_at)
    .bind(&head.session_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Replaces a project's cross-session findings for the given rules. Session-scoped findings and
/// other rules' cross findings are untouched.
pub async fn replace_cross_findings(
    pool: &SqlitePool,
    project_id: &str,
    rule_ids: &[&str],
    rows: &[FindingRow],
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for rule_id in rule_ids {
        sqlx::query(
            "DELETE FROM devtime_findings WHERE project_id = ? AND scope = 'cross' AND rule_id = ?",
        )
        .bind(project_id)
        .bind(*rule_id)
        .execute(&mut *tx)
        .await?;
    }
    for row in rows {
        insert_finding(&mut tx, row).await?;
    }
    tx.commit().await
}

/// What the cross-session rules (F) read of one attempt.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct CrossAttempt {
    pub session_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub tool_name: String,
    pub cmd_program: Option<String>,
    pub cmd_hash: Option<String>,
    pub outcome: String,
    pub error_class: Option<String>,
    /// A JSON array of `{path, offset}`.
    pub reads: String,
}

/// The main-lane attempts of a project's sessions since `since` (a UTC timestamp), in session and
/// time order.
pub async fn cross_session_rows(
    pool: &SqlitePool,
    project_id: &str,
    since: &str,
) -> sqlx::Result<Vec<CrossAttempt>> {
    sqlx::query_as::<_, CrossAttempt>(
        "SELECT a.session_id, a.started_at, a.ended_at, a.tool_name, a.cmd_program, a.cmd_hash,
                a.outcome, a.error_class, a.reads
         FROM devtime_attempts a
         JOIN devtime_sessions s ON s.session_id = a.session_id
         WHERE s.project_id = ? AND a.lane = 'main' AND a.started_at >= ?
         ORDER BY a.session_id, a.started_at, a.attempt_id",
    )
    .bind(project_id)
    .bind(since)
    .fetch_all(pool)
    .await
}

pub async fn finding_by_key(pool: &SqlitePool, key: &str) -> sqlx::Result<Option<FindingRow>> {
    sqlx::query_as::<_, FindingRow>(
        "SELECT finding_key, project_id, session_id, scope, rule_id, rule_version, level, waste,
                lever, confidence, lane, started_at, ended_at, cost_ms, count, attempt_ids, sessions,
                rules_version, parser_version
         FROM devtime_findings WHERE finding_key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
}

/// The owner's standing mark on one finding.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct FeedbackRow {
    pub finding_key: String,
    pub rule_id: String,
    pub session_id: Option<String>,
    /// One of [`FEEDBACK_VERDICTS`].
    pub verdict: String,
    /// One of [`FEEDBACK_CAUSES`] or [`LEVERS`].
    pub cause: Option<String>,
    pub rule_version: Option<i64>,
    /// Empty on write means now.
    pub marked_at: String,
}

/// One standing mark per finding: marking again replaces the earlier mark.
pub async fn upsert_feedback(pool: &SqlitePool, row: &FeedbackRow) -> sqlx::Result<()> {
    let marked_at = if row.marked_at.is_empty() {
        now()
    } else {
        row.marked_at.clone()
    };
    sqlx::query(
        "INSERT INTO devtime_feedback
            (finding_key, rule_id, session_id, verdict, cause, rule_version, marked_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(finding_key) DO UPDATE SET
            rule_id = excluded.rule_id, session_id = excluded.session_id,
            verdict = excluded.verdict, cause = excluded.cause,
            rule_version = excluded.rule_version, marked_at = excluded.marked_at",
    )
    .bind(&row.finding_key)
    .bind(&row.rule_id)
    .bind(&row.session_id)
    .bind(&row.verdict)
    .bind(&row.cause)
    .bind(row.rule_version)
    .bind(marked_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// How many current findings of a rule exist and how many of them the owner has judged.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct RuleCaseCounts {
    pub rule_id: String,
    /// Findings whose confidence is `exact`.
    pub exact: i64,
    /// Findings that are not `exact` but carry a mark.
    pub inferred_marked: i64,
    /// Findings marked `not_rework`.
    pub not_rework: i64,
    pub total: i64,
}

/// Per rule, over the findings that exist now: a mark whose finding was recomputed away is kept in
/// `devtime_feedback` for history but is no longer a case, so the join runs from the findings.
pub async fn rule_case_counts(
    pool: &SqlitePool,
    since: Option<&str>,
) -> sqlx::Result<Vec<RuleCaseCounts>> {
    sqlx::query_as::<_, RuleCaseCounts>(
        "SELECT f.rule_id AS rule_id,
                COALESCE(SUM(CASE WHEN f.confidence = 'exact' THEN 1 ELSE 0 END), 0) AS exact,
                COALESCE(SUM(CASE WHEN f.confidence <> 'exact' AND b.finding_key IS NOT NULL
                                  THEN 1 ELSE 0 END), 0) AS inferred_marked,
                COALESCE(SUM(CASE WHEN b.verdict = 'not_rework' THEN 1 ELSE 0 END), 0)
                    AS not_rework,
                COUNT(*) AS total
         FROM devtime_findings f
         LEFT JOIN devtime_feedback b ON b.finding_key = f.finding_key
         WHERE (? IS NULL OR f.started_at >= ?)
         GROUP BY f.rule_id
         ORDER BY f.rule_id",
    )
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await
}

/// How many sessions fired each rule, out of the sessions that have at least one attempt (a session
/// with none could not have fired anything, so it is not the denominator). Cross findings belong to
/// no single session's run and are left out.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn rule_base_rates(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: Option<&str>,
) -> sqlx::Result<(i64, Vec<(String, i64)>)> {
    let with_tools: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devtime_sessions s
         WHERE EXISTS (SELECT 1 FROM devtime_attempts a WHERE a.session_id = s.session_id)
           AND (? IS NULL OR s.project_id = ?)
           AND (? IS NULL OR s.started_at >= ?)",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(since)
    .bind(since)
    .fetch_one(pool)
    .await?;
    let fired: Vec<(String, i64)> = sqlx::query_as(
        "SELECT f.rule_id, COUNT(DISTINCT f.session_id)
         FROM devtime_findings f
         JOIN devtime_sessions s ON s.session_id = f.session_id
         WHERE f.scope = 'session'
           AND EXISTS (SELECT 1 FROM devtime_attempts a WHERE a.session_id = s.session_id)
           AND (? IS NULL OR s.project_id = ?)
           AND (? IS NULL OR s.started_at >= ?)
         GROUP BY f.rule_id
         ORDER BY f.rule_id",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await?;
    Ok((with_tools, fired))
}

/// Stored per-turn stats, for the unexplained listing.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn turn_stats_rows(
    pool: &SqlitePool,
    project_id: Option<&str>,
    since: Option<&str>,
) -> sqlx::Result<Vec<TurnStatRow>> {
    sqlx::query_as::<_, TurnStatRow>(
        "SELECT session_id, turn_seq, project_id, turn_class, started_at, calls, active_ms,
                explained_ms
         FROM devtime_turn_stats
         WHERE (? IS NULL OR project_id = ?) AND (? IS NULL OR started_at >= ?)
         ORDER BY session_id, turn_seq",
    )
    .bind(project_id)
    .bind(project_id)
    .bind(since)
    .bind(since)
    .fetch_all(pool)
    .await
}
