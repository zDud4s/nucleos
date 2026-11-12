//! The outcome step's read queries over the sessions' findings, marks and proposals, and the
//! retention prune of the shadow log. Split out of `shadow.rs` to keep it under the file ceiling.

use super::*;

/// One non-cross finding of a session, as the outcome step needs it (ids and numbers only).
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SessionFinding {
    pub rule_id: String,
    pub finding_key: String,
    pub rule_version: i64,
    pub cost_ms: i64,
}

/// The `devtime_findings` rows of `session_id` whose `scope <> 'cross'`, every rule and every
/// `rule_version`, ordered by `finding_key`.
pub async fn session_findings(
    pool: &SqlitePool,
    session_id: &str,
) -> sqlx::Result<Vec<SessionFinding>> {
    sqlx::query_as::<_, SessionFinding>(
        "SELECT rule_id, finding_key, rule_version, cost_ms FROM devtime_findings \
         WHERE session_id = ? AND scope <> 'cross' ORDER BY finding_key",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
}

/// The `verified` value of every `devtime_attempt_marks` row of `session_id` (`passed` | `failed` |
/// `not_measured`), in any order; empty when the session has no marks.
pub async fn session_attempt_verified(
    pool: &SqlitePool,
    session_id: &str,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT verified FROM devtime_attempt_marks WHERE session_id = ?")
        .bind(session_id)
        .fetch_all(pool)
        .await
}

/// Whether a `cross` finding keyed `finding_key` ended inside `(after, until]`: the key of the
/// first such finding, if any.
pub async fn cross_reappearance(
    pool: &SqlitePool,
    finding_key: &str,
    after: &str,
    until: &str,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT finding_key FROM devtime_findings WHERE scope = 'cross' AND finding_key = ? \
         AND ended_at > ? AND ended_at <= ? LIMIT 1",
    )
    .bind(finding_key)
    .bind(after)
    .bind(until)
    .fetch_optional(pool)
    .await
}

/// The `status` of `proposals` row `id`; `None` when there is no such proposal.
pub async fn proposal_status(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// Days a closed shadow row is kept before the maintenance sweep deletes it (spec §3.2).
pub const SHADOW_RETENTION_DAYS: i64 = 180;

/// Deletes the closed (`resolved`, `expired` or `void`) rows older than `cutoff`, judged by
/// `resolved_at` (the decision time when a row never got one, as a `void_stale` row). `pending`
/// rows are never touched. Simple delete: the spec's "aggregated, then deleted" is not done, the
/// promotion report only reads rows inside its own window. Returns the rows deleted.
pub async fn prune_closed_before(pool: &SqlitePool, cutoff: &str) -> sqlx::Result<u64> {
    let res = sqlx::query(
        "DELETE FROM devtime_shadow WHERE outcome_state IN ('resolved', 'expired', 'void') \
         AND COALESCE(resolved_at, decided_at) < ?",
    )
    .bind(cutoff)
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}
