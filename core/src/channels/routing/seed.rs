//! Routing state per channel (Anexo D D.12, table 0225): the seed, the reseed, the turn watermark
//! and the "has a destination" evaluation. The cursors live in `channel_routing_state`.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::channels::{access, registry};
use crate::governance::budget::{self, BudgetDecision};
use crate::state::AppState;

/// Whether the budget refuses new runs right now. The no-limit shortcut reads no `runs`
/// (Anexo B, B.6.3); otherwise only `budget_permits_new_run`, never the HTTP snapshot.
pub(super) async fn budget_paused(pool: &SqlitePool, now: DateTime<Utc>) -> bool {
    match budget::load_budget_config(pool).await {
        Ok(config) if config.limit_usd.is_none() && config.hourly_limit_usd.is_none() => false,
        Ok(_) => matches!(
            budget::budget_permits_new_run(pool, now).await,
            BudgetDecision::Pause { .. }
        ),
        Err(error) => {
            tracing::warn!(%error, "routing: could not read the budget configuration");
            false
        }
    }
}

/// First start or new channel: cursors = current max, last states = current (section 7.4),
/// `turn_watermark` = `max(runs.id)`. `INSERT OR IGNORE`: an existing row is left alone.
///
/// A connection alone cannot evaluate the budget (its spend is a pool read), so this seeds
/// `last_budget_paused` as false; `seed_at` takes the evaluated value and `seed_all` uses it.
pub async fn seed(conn: &mut sqlx::SqliteConnection, channel: &str) -> sqlx::Result<()> {
    seed_at(conn, channel, false, Utc::now()).await
}

/// `seed` with the budget state the caller already evaluated.
pub async fn seed_at(
    conn: &mut sqlx::SqliteConnection,
    channel: &str,
    budget_paused: bool,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO channel_routing_state \
         (channel, feed_cursor, proposal_cursor, last_kill, last_budget_paused, turn_watermark, \
          seeded_at) \
         VALUES (?1, \
                 (SELECT COALESCE(MAX(id), 0) FROM feed), \
                 (SELECT COALESCE(MAX(id), 0) FROM proposals), \
                 COALESCE((SELECT kill_switch <> 0 FROM autopilot_global LIMIT 1), 0), \
                 ?2, \
                 (SELECT COALESCE(MAX(id), 0) FROM runs), \
                 ?3)",
    )
    .bind(channel)
    .bind(budget_paused)
    .bind(now.to_rfc3339())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// `seed` for every configured channel, in one transaction. Failures are logged, not fatal: the
/// first routing pass seeds a channel that has no row yet.
pub async fn seed_all(state: &AppState) {
    if let Err(error) = seed_all_inner(state).await {
        tracing::warn!(%error, "routing: could not seed the channel routing state");
    }
}

async fn seed_all_inner(state: &AppState) -> sqlx::Result<()> {
    let now = Utc::now();
    let paused = budget_paused(&state.pool, now).await;
    let mut tx = state.pool.begin().await?;
    for manifest in registry::all()
        .iter()
        .filter(|m| registry::is_configured(state, m.id))
    {
        seed_at(&mut tx, manifest.id, paused, now).await?;
    }
    tx.commit().await
}

/// A channel that becomes primary after a time out of it (Anexo E, E.3): UPSERT of the cursors
/// and last states to the current values, so a week of history is not announced at once.
/// `turn_watermark` is NOT reset: the turn cycle runs per channel whether or not it is primary.
/// Like `seed`, the budget state is false here; `reseed_at` takes the evaluated value.
pub async fn reseed(conn: &mut sqlx::SqliteConnection, channel: &str) -> sqlx::Result<()> {
    reseed_at(conn, channel, false, Utc::now()).await
}

/// `reseed` with the budget state the caller already evaluated.
pub async fn reseed_at(
    conn: &mut sqlx::SqliteConnection,
    channel: &str,
    budget_paused: bool,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO channel_routing_state \
         (channel, feed_cursor, proposal_cursor, last_kill, last_budget_paused, turn_watermark, \
          seeded_at) \
         VALUES (?1, \
                 (SELECT COALESCE(MAX(id), 0) FROM feed), \
                 (SELECT COALESCE(MAX(id), 0) FROM proposals), \
                 COALESCE((SELECT kill_switch <> 0 FROM autopilot_global LIMIT 1), 0), \
                 ?2, \
                 (SELECT COALESCE(MAX(id), 0) FROM runs), \
                 ?3) \
         ON CONFLICT(channel) DO UPDATE SET \
            feed_cursor = excluded.feed_cursor, \
            proposal_cursor = excluded.proposal_cursor, \
            last_kill = excluded.last_kill, \
            last_budget_paused = excluded.last_budget_paused",
    )
    .bind(channel)
    .bind(budget_paused)
    .bind(now.to_rfc3339())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The reply-discovery watermark of a channel; 0 for a channel that was never seeded.
pub async fn turn_watermark(pool: &SqlitePool, channel: &str) -> sqlx::Result<i64> {
    let mark: Option<i64> =
        sqlx::query_scalar("SELECT turn_watermark FROM channel_routing_state WHERE channel = ?")
            .bind(channel)
            .fetch_optional(pool)
            .await?;
    Ok(mark.unwrap_or(0))
}

/// Moves the watermark up to `to`; a lower value is a no-op, so two racing discoveries cannot
/// walk it back.
pub async fn advance_turn_watermark(
    conn: &mut sqlx::SqliteConnection,
    channel: &str,
    to: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE channel_routing_state SET turn_watermark = MAX(turn_watermark, ?) \
         WHERE channel = ?",
    )
    .bind(to)
    .bind(channel)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Whether notifications have a destination now: the evaluation `GET /channels` shows as
/// `routing.state` (Anexo E, E.2). Without a primary it answers from one query.
pub async fn routing_state(state: &AppState) -> sqlx::Result<registry::RoutingState> {
    if access::primary(&state.pool).await?.is_none() {
        return Ok(registry::RoutingState::NoPrimary);
    }
    Ok(registry::list(state).await?.routing.state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::testkit;

    // Each helper holds its connection only for the call: the test pool has a single connection,
    // so a connection kept across a pool read would time out.
    async fn seed_now(pool: &SqlitePool, channel: &str) {
        let mut conn = pool.acquire().await.unwrap();
        seed(&mut conn, channel).await.unwrap();
    }

    async fn reseed_now(pool: &SqlitePool, channel: &str) {
        let mut conn = pool.acquire().await.unwrap();
        reseed(&mut conn, channel).await.unwrap();
    }

    async fn advance_now(pool: &SqlitePool, channel: &str, to: i64) {
        let mut conn = pool.acquire().await.unwrap();
        advance_turn_watermark(&mut conn, channel, to)
            .await
            .unwrap();
    }

    async fn run(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at) \
             VALUES ('p', 'completed', 'assistant', 'telegram:1', '2026-10-20T00:00:00Z') \
             RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn row(pool: &SqlitePool, channel: &str) -> (i64, i64, bool, bool, i64) {
        sqlx::query_as(
            "SELECT feed_cursor, proposal_cursor, last_kill, last_budget_paused, turn_watermark \
             FROM channel_routing_state WHERE channel = ?",
        )
        .bind(channel)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn feed_line(pool: &SqlitePool) -> i64 {
        nucleos_base::feed::append(pool, None, "job_done", "x", None, None)
            .await
            .unwrap()
    }

    async fn proposal(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO proposals (kind, status, reasoning, created_at) \
             VALUES ('tool', 'pending', 'why', '2026-10-20T00:00:00Z') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn seed_is_insert_or_ignore_and_sets_watermark_to_max_run() {
        let state = testkit::state().await;
        let pool = &state.pool;
        let feed = feed_line(pool).await;
        let prop = proposal(pool).await;
        run(pool).await;
        let last_run = run(pool).await;
        sqlx::query("UPDATE autopilot_global SET kill_switch = 1")
            .execute(pool)
            .await
            .unwrap();

        seed_now(pool, "telegram").await;
        assert_eq!(
            row(pool, "telegram").await,
            (feed, prop, true, false, last_run)
        );

        // A second seed after more history leaves the existing row alone.
        feed_line(pool).await;
        run(pool).await;
        seed_now(pool, "telegram").await;
        assert_eq!(
            row(pool, "telegram").await,
            (feed, prop, true, false, last_run)
        );
    }

    #[tokio::test]
    async fn reseed_upserts_cursors_but_keeps_watermark() {
        let state = testkit::state().await;
        let pool = &state.pool;
        run(pool).await;
        seed_now(pool, "telegram").await;
        let (_, _, _, _, mark) = row(pool, "telegram").await;

        let feed = feed_line(pool).await;
        let prop = proposal(pool).await;
        run(pool).await;
        sqlx::query("UPDATE autopilot_global SET kill_switch = 1")
            .execute(pool)
            .await
            .unwrap();
        reseed_now(pool, "telegram").await;
        assert_eq!(row(pool, "telegram").await, (feed, prop, true, false, mark));

        // On a channel with no row yet it inserts, with the watermark at the current max run.
        reseed_now(pool, "fake").await;
        let (_, _, _, _, fresh) = row(pool, "fake").await;
        assert!(fresh > mark);
    }

    #[tokio::test]
    async fn advance_turn_watermark_only_rises() {
        let state = testkit::state().await;
        let pool = &state.pool;
        assert_eq!(turn_watermark(pool, "telegram").await.unwrap(), 0);
        seed_now(pool, "telegram").await;
        advance_now(pool, "telegram", 40).await;
        assert_eq!(turn_watermark(pool, "telegram").await.unwrap(), 40);
        advance_now(pool, "telegram", 12).await;
        assert_eq!(turn_watermark(pool, "telegram").await.unwrap(), 40);
        advance_now(pool, "telegram", 41).await;
        assert_eq!(turn_watermark(pool, "telegram").await.unwrap(), 41);
    }

    #[tokio::test]
    async fn routing_state_reports_no_primary_primary_not_configured_and_without_home() {
        use registry::RoutingState;
        let state = testkit::state().await;
        let pool = &state.pool;
        assert_eq!(
            routing_state(&state).await.unwrap(),
            RoutingState::NoPrimary
        );

        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
            .execute(pool)
            .await
            .unwrap();
        assert_eq!(
            routing_state(&state).await.unwrap(),
            RoutingState::PrimaryNotConfigured
        );

        state
            .channels
            .presence
            .set_secrets_present("telegram", Vec::new());
        assert_eq!(
            routing_state(&state).await.unwrap(),
            RoutingState::PrimaryWithoutHome
        );

        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, home, created_at) VALUES ('telegram', '42', '7', 'private', 1, 't')",
        )
        .execute(pool)
        .await
        .unwrap();
        assert_eq!(routing_state(&state).await.unwrap(), RoutingState::Ok);
    }
}
