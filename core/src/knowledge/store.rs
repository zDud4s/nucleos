//! Knowledge-store reads and closing working rows.

use super::*;

/// Closes every live working finding owned by one job, preserving the transition as history.
pub async fn close_working_for_job(pool: &SqlitePool, job_id: i64) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let at = chrono::Utc::now().to_rfc3339();
    let scope_id = job_id.to_string();
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         SELECT id, 'live', 'closed', 'the job ended', ?
           FROM knowledge
          WHERE layer = 'working' AND scope_kind = 'job' AND scope_id = ? AND status = 'live'",
    )
    .bind(&at)
    .bind(&scope_id)
    .execute(&mut *tx)
    .await?;
    let closed = sqlx::query(
        "UPDATE knowledge SET status = 'closed', ended_at = ?
          WHERE layer = 'working' AND scope_kind = 'job' AND scope_id = ? AND status = 'live'",
    )
    .bind(&at)
    .bind(&scope_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(closed)
}

/// Closes live working findings whose job has ended or no longer exists.
pub async fn close_orphaned_working(pool: &SqlitePool) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    let at = chrono::Utc::now().to_rfc3339();
    let placeholders = vec!["?"; crate::execution::job::TERMINAL_STATUSES.len()].join(", ");
    let predicate = format!(
        "layer = 'working' AND scope_kind = 'job' AND status = 'live'
         AND NOT EXISTS (
             SELECT 1 FROM jobs j
              WHERE CAST(j.id AS TEXT) = knowledge.scope_id
                AND j.status NOT IN ({placeholders})
         )"
    );

    // `AssertSqlSafe`, audited: the only interpolation is one `?` per status in the fixed
    // `TERMINAL_STATUSES` list. Every status is bound below.
    let mut events = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         SELECT id, 'live', 'closed', 'the job ended', ? FROM knowledge WHERE {predicate}"
    )))
    .bind(&at);
    for status in crate::execution::job::TERMINAL_STATUSES {
        events = events.bind(status);
    }
    events.execute(&mut *tx).await?;

    // Same audit as above; `ended_at` is bound before the fixed status list.
    let mut update = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE knowledge SET status = 'closed', ended_at = ? WHERE {predicate}"
    )))
    .bind(&at);
    for status in crate::execution::job::TERMINAL_STATUSES {
        update = update.bind(status);
    }
    let closed = update.execute(&mut *tx).await?.rows_affected();
    tx.commit().await?;
    Ok(closed)
}

/// What a node in this scope is entitled to be told.
///
/// The chain comes too, and that is the whole reason this takes a [`Scope`] rather than a project
/// id: machine-wide rows are not hidden by the scoping rule, which is about not letting one
/// project's lesson become another's lie. `scope_id IS ?` rather than `=`, because the machine
/// scope's id is NULL and `= NULL` is never true — the bug would be a briefing silently missing
/// everything known about the house.
///
/// Approved rows are fetched across the whole chain. A `live` row is fetched only when its stored
/// shape is a job-scoped working finding; [`select`] and its [`admitted`] gate still decide whether
/// that row belongs to this exact job and is evidenced enough to reach the prompt.
///
/// Ordered here as well as in [`render`], so a caller that skips the renderer still gets a stable
/// list, and so the LIMIT below cuts the tail rather than an arbitrary middle.
pub async fn for_scope(pool: &SqlitePool, scope: &Scope) -> sqlx::Result<Vec<Known>> {
    for_columns(pool, &scope.chain()).await
}

/// What a briefing in this context is entitled to be told: [`for_scope`]'s query over the columns
/// of every link of [`Context::scopes`], so the agent's and the team's own rows are fetched with
/// the project's, and another agent's or team's never are.
pub async fn for_context(pool: &SqlitePool, context: &Context) -> sqlx::Result<Vec<Known>> {
    let columns: Vec<(&'static str, Option<String>)> =
        context.scopes().iter().map(Scope::columns).collect();
    for_columns(pool, &columns).await
}

/// The query behind [`for_scope`] and [`for_context`], over the `(scope_kind, scope_id)` pairs
/// the caller is entitled to.
pub(super) async fn for_columns(
    pool: &SqlitePool,
    chain: &[(&'static str, Option<String>)],
) -> sqlx::Result<Vec<Known>> {
    let terms: Vec<&str> = chain
        .iter()
        .map(|_| "(scope_kind = ? AND scope_id IS ?)")
        .collect();
    let sql = format!(
        "SELECT {COLUMNS}
           FROM knowledge
          WHERE (status = 'active'
                 OR (status = 'live' AND layer = 'working' AND scope_kind = 'job'))
            AND ({})
          ORDER BY id
          LIMIT ?",
        terms.join(" OR ")
    );
    // `AssertSqlSafe` because sqlx 0.9 takes only `&'static str` otherwise, and the audit it asks
    // for is short: the interpolated parts are [`COLUMNS`], which is a literal, and one
    // `(scope_kind = ? AND scope_id IS ?)` per link of the chain, which is also a literal. Every
    // value the caller supplies is bound below.
    let mut query = sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(sql));
    for (kind, id) in chain {
        query = query.bind(*kind).bind(id.clone());
    }
    query
        // A ceiling on the QUERY as well as on the rendering, because the two protect different
        // things: `RENDER_CHARS` keeps a prompt affordable, and this keeps a project that has
        // approved ten thousand rows from reading all of them into memory to render forty.
        .bind(MAX_READ as i64)
        .fetch_all(pool)
        .await
}

/// How many approved rows are read before rendering ever begins.
pub const MAX_READ: usize = 200;

/// Everything the store holds, in every status, newest first.
///
/// Not filtered to `active`, deliberately: the reviewable history IS the feature, and a screen that
/// showed only what is in force could not answer "what did it try to learn that I said no to".
///
/// Here rather than written out at the one call site it has, which is where it used to live. The
/// column list is [`COLUMNS`] and nothing else, so a reader outside this module cannot fall behind
/// the table by naming twelve of its twenty-six columns — which is precisely how the handler that
/// used to hold this query would have survived `0143` by silently returning nothing.
pub async fn all(pool: &SqlitePool) -> sqlx::Result<Vec<Known>> {
    // `AssertSqlSafe`, audited: the only interpolation is [`COLUMNS`], a literal.
    sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge ORDER BY id DESC LIMIT 500"
    )))
    .fetch_all(pool)
    .await
}

/// Every row of exactly one scope, in every status, newest first: [`all`], narrowed to the
/// `(scope_kind, scope_id)` of `scope`, so a screen showing one agent's or one team's memory is
/// not at the mercy of how many rows every other scope has.
pub async fn in_scope(pool: &SqlitePool, scope: &Scope) -> sqlx::Result<Vec<Known>> {
    let (kind, id) = scope.columns();
    // `AssertSqlSafe`, audited: the only interpolation is [`COLUMNS`], a literal.
    sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge
          WHERE scope_kind = ? AND scope_id IS ?
          ORDER BY id DESC LIMIT 500"
    )))
    .bind(kind)
    .bind(id)
    .fetch_all(pool)
    .await
}
