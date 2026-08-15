//! Two jobs somebody asked not to run at the same time.
//!
//! It lives apart from `concurrency.rs` for the reason `collision.rs` already gave. That one answers
//! *how much work fits*, and the answer is an invariant held by a primary key; this answers *what
//! somebody asked for*, and the answer is a rule with an author, an approval and a revocation. A
//! slot is arithmetic. This is a decision, and decisions have a history.
//!
//! **Asking writes no rule.** Drawing an exclusion mints a proposal and nothing else; the row in
//! `fleet_exclusions` appears when that proposal is approved. This is the difference between this
//! design and october.dev's canvas, where an edge grants access the moment it is drawn — an edge
//! that acts on being drawn is a way to make a governance decision without passing through the place
//! where governance decisions are made.
//!
//! The pair is ordered on the way in and never at read time. See the migration for why: the order is
//! also the tie-break, so both jobs parked on each other is a state this feature cannot reach.

use sqlx::SqlitePool;

/// The two jobs, ordered, or `None` when they are the same job.
///
/// `None` and not "the pair (7, 7)": a job excluded from itself is a job that holds its own slot and
/// therefore blocks forever, and the unique index would happily store it. The refusal belongs here,
/// at the one door every write goes through, rather than in each caller.
pub fn pair(a: i64, b: i64) -> Option<(i64, i64)> {
    if a == b {
        return None;
    }
    Some(if a < b { (a, b) } else { (b, a) })
}

/// Why an exclusion could not be asked for.
#[derive(Debug)]
pub enum ProposeError {
    /// Both ids name the same job.
    SameJob,
    /// One of the two ids names no job.
    UnknownJob(i64),
    /// The two jobs belong to different projects.
    DifferentProjects,
    /// This pair already has a request waiting for a decision.
    AlreadyAsked,
    /// This pair already has a rule in force.
    AlreadyExcluded,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ProposeError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// The key a request carries so a second request for the same pair can be recognised.
///
/// A field of its own rather than a match against `job_low` and `job_high` in the stored JSON,
/// because that match would depend on the order `serde_json` writes an object's keys in — a detail
/// no test in this repository pins and any future crate feature could change. `calendar.rs` searches
/// `tool_input` with a LIKE in the same way; this only gives the LIKE something stable to find.
fn pair_key(low: i64, high: i64) -> String {
    format!("{low}:{high}")
}

/// Asks that two jobs not run at the same time. Writes a proposal, and no rule.
///
/// The project is read from the jobs rather than supplied, which removes the one call that could
/// file an exclusion under a project neither job belongs to. Both jobs have to be in the same
/// project because the thing being serialised is a project's slots.
pub async fn propose(
    pool: &SqlitePool,
    job_a: i64,
    job_b: i64,
    paths: &[String],
) -> Result<i64, ProposeError> {
    let (low, high) = pair(job_a, job_b).ok_or(ProposeError::SameJob)?;

    let project_low = project_of(pool, low)
        .await?
        .ok_or(ProposeError::UnknownJob(low))?;
    let project_high = project_of(pool, high)
        .await?
        .ok_or(ProposeError::UnknownJob(high))?;
    if project_low != project_high {
        return Err(ProposeError::DifferentProjects);
    }

    if is_excluded(pool, low, high).await? {
        return Err(ProposeError::AlreadyExcluded);
    }
    if pending_request_for(pool, low, high).await? {
        return Err(ProposeError::AlreadyAsked);
    }

    let tool_input = serde_json::json!({
        "pair": pair_key(low, high),
        "job_low": low,
        "job_high": high,
        "paths": paths,
    })
    .to_string();
    let reasoning = match paths {
        [] => format!("jobs {low} and {high} are not to run at the same time"),
        [only] => format!("jobs {low} and {high} both touch {only}"),
        _ => format!(
            "jobs {low} and {high} both touch {} files, including {}",
            paths.len(),
            paths[0]
        ),
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;
    let proposal_id: i64 = sqlx::query_scalar(
        "INSERT INTO proposals
         (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
         VALUES ('fleet-exclusion', 'pending', NULL, NULL, ?, NULL, ?, ?, ?, NULL)
         RETURNING id",
    )
    .bind(&project_low)
    .bind(reasoning)
    .bind(tool_input)
    .bind(&now)
    .fetch_one(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'pending', 'created', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(proposal_id)
}

async fn project_of(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar("SELECT project_id FROM jobs WHERE id = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await
}

/// Whether this pair already has a rule in force.
pub async fn is_excluded(pool: &SqlitePool, low: i64, high: i64) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM fleet_exclusions
          WHERE job_low = ? AND job_high = ? AND revoked_at IS NULL
          LIMIT 1",
    )
    .bind(low)
    .bind(high)
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

/// Whether this pair already has a request waiting on a person.
///
/// Without it, drawing the same edge twice mints two proposals, and approving both would meet the
/// unique index as a 500 — the second approval failing for a reason the person could not act on.
async fn pending_request_for(pool: &SqlitePool, low: i64, high: i64) -> sqlx::Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM proposals
          WHERE kind = 'fleet-exclusion' AND status = 'pending' AND tool_input LIKE ?
          LIMIT 1",
    )
    .bind(format!("%\"pair\":\"{}\"%", pair_key(low, high)))
    .fetch_optional(pool)
    .await?;
    Ok(found.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    async fn test_pool() -> SqlitePool {
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

    async fn add_job(pool: &SqlitePool, project_id: &str) -> i64 {
        sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES (?, ?, 'implementing', 5, '2026-08-15T00:00:00Z')",
        )
        .bind(project_id)
        .bind(format!("C:/projects/{project_id}"))
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Drawn either way round, it is one rule.
    ///
    /// The relation is symmetric and the storage is not, so this is the only place the two are
    /// reconciled. Were it not, the same request drawn backwards would be a second row the unique
    /// index cannot see, and revoking one would leave the other holding the job.
    #[test]
    fn the_pair_is_the_same_pair_drawn_either_way() {
        assert_eq!(pair(41, 42), Some((41, 42)));
        assert_eq!(pair(42, 41), Some((41, 42)));
        assert_eq!(pair(41, 42), pair(42, 41));
    }

    /// A job does not exclude itself, and the refusal is here rather than in the callers.
    ///
    /// The row would be accepted by the table: `job_low = job_high = 7` breaks no constraint. What
    /// it would do is park job 7 for as long as job 7 holds a slot, which is a job that can never
    /// run again and no error message anywhere to say why.
    #[test]
    fn a_job_is_not_excluded_from_itself() {
        assert_eq!(pair(7, 7), None);
    }

    /// Asking writes a request and NOT a rule.
    ///
    /// This is decision 1 of the design, and it is the whole difference from october.dev's canvas,
    /// where drawing an edge grants what it describes. Here the edge is a question; the table below
    /// stays empty until somebody answers it.
    #[tokio::test]
    async fn asking_mints_a_proposal_and_writes_no_rule() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;

        let proposal_id = propose(&pool, high, low, &[]).await.unwrap();

        let (kind, status, project_id, run_id): (String, String, Option<String>, Option<i64>) =
            sqlx::query_as("SELECT kind, status, project_id, run_id FROM proposals WHERE id = ?")
                .bind(proposal_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, "fleet-exclusion");
        assert_eq!(status, "pending");
        assert_eq!(project_id.as_deref(), Some("alpha"));
        assert_eq!(run_id, None, "an exclusion is about jobs, not about a run");

        let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fleet_exclusions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rules, 0, "asking must write no rule");
    }

    /// The same edge drawn twice is one question, not two.
    ///
    /// Two pending requests for one pair would both be approvable, and the second approval would
    /// meet `one_live_exclusion_per_pair` as a database error — a refusal with nothing in it the
    /// person could act on.
    #[tokio::test]
    async fn a_pair_already_asked_about_is_refused() {
        let pool = test_pool().await;
        let low = add_job(&pool, "alpha").await;
        let high = add_job(&pool, "alpha").await;

        propose(&pool, low, high, &[]).await.unwrap();
        // Backwards, to prove the check is on the pair and not on the argument order.
        let second = propose(&pool, high, low, &[]).await;

        assert!(
            matches!(second, Err(ProposeError::AlreadyAsked)),
            "{second:?}"
        );
    }

    /// Two jobs of different projects share no slots, so there is nothing to serialise.
    #[tokio::test]
    async fn two_jobs_of_different_projects_cannot_be_excluded() {
        let pool = test_pool().await;
        let alpha = add_job(&pool, "alpha").await;
        let beta = add_job(&pool, "beta").await;

        let asked = propose(&pool, alpha, beta, &[]).await;

        assert!(
            matches!(asked, Err(ProposeError::DifferentProjects)),
            "{asked:?}"
        );
    }

    #[tokio::test]
    async fn a_job_that_does_not_exist_is_named_in_the_refusal() {
        let pool = test_pool().await;
        let job = add_job(&pool, "alpha").await;

        let asked = propose(&pool, job, 404, &[]).await;

        assert!(
            matches!(asked, Err(ProposeError::UnknownJob(404))),
            "{asked:?}"
        );
    }

    #[tokio::test]
    async fn asking_about_the_same_job_twice_is_refused() {
        let pool = test_pool().await;
        let job = add_job(&pool, "alpha").await;

        let asked = propose(&pool, job, job, &[]).await;

        assert!(matches!(asked, Err(ProposeError::SameJob)), "{asked:?}");
    }
}
