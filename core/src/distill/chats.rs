//! Conversations as a distiller source: scope ids of a chat, the window of exchanges one queue row
//! reads, and the owner's explicit ingestion mark. Spec 2026-10-22 4.1, B8, B14.

use super::sources::{self, CHAT_KEY};
use super::{
    Cause, DOSSIER_CEILING, DossierInputs, QueueRow, Subject, Written, ask, close_row, dossier,
    embed_items, extraction_prompt, fail, items_format, parse_items, standing_for, write_one_item,
};
use crate::embed::Embedder;
use crate::map::intent::Extractor;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

/// Fewest exchanges a chat needs before a batch is queued.
pub const MIN_EXCHANGES: usize = 4;
/// Minutes without a new turn before a chat counts as idle.
pub const IDLE_MINUTES: i64 = 90;
/// Most exchanges one queue row reads.
pub const BATCH_EXCHANGES: i64 = 40;
/// Most characters kept of each side of an exchange.
pub const SIDE_CHARS: usize = 1_200;
/// Daily ceilings (spec B14); a guess to be tuned.
pub const MAX_CHAT_ROWS_PER_DAY: i64 = 6;
pub const MAX_CHAT_ROWS_PER_CHAT_DAY: i64 = 2;
pub const MAX_CHAT_CANDIDATES_PER_ROW: usize = 3;
pub const MAX_OBS_PER_ROW: usize = 8;
pub const MAX_OBS_PER_CHAT_DAY: i64 = 10;

pub fn chat_scope_id(project: &str, chat_key: i64) -> String {
    format!(
        "{}:chat:{chat_key}",
        if project.is_empty() { "-" } else { project }
    )
}

/// `<project|->:chat:<digits>`; `rsplit_once` because a project id may itself contain ':'.
pub fn parse_chat_scope(scope_id: &str) -> Option<(&str, i64)> {
    let (project, key) = scope_id.rsplit_once(":chat:")?;
    if project.is_empty() || key.is_empty() || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((project, key.parse().ok()?))
}

/// The instant the local day of `now` began, in UTC.
pub fn local_day_start<Tz: chrono::TimeZone>(
    now: chrono::DateTime<Tz>,
) -> chrono::DateTime<chrono::Utc> {
    let midnight = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists");
    now.timezone()
        .from_local_datetime(&midnight)
        .earliest()
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(|| now.with_timezone(&chrono::Utc) - chrono::Duration::hours(24))
}

/// One exchange of a chat: what the owner wrote and the answer, each clipped to [`SIDE_CHARS`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub run_id: i64,
    pub asked: String,
    pub answered: String,
}

/// The oldest [`BATCH_EXCHANGES`] exchanges of `chat_id` up to run `up_to`, after the latest of: the
/// last turn that read untrusted text, the owner's `/clear`, the previous batch of this chat and
/// (P8) the history cut, unless the owner seeded this chat. It is `history_window`'s floor
/// (`send.rs`) plus relays out: a relayed turn is not something the owner wrote.
pub(super) async fn window(
    pool: &SqlitePool,
    chat_id: &str,
    up_to: i64,
) -> sqlx::Result<Vec<Turn>> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT id, prompt, stdout FROM runs
          WHERE chat_id = ?1 AND mode = 'assistant' AND status = 'completed'
            AND from_relay_id IS NULL AND stdout IS NOT NULL AND stdout <> '' AND id <= ?2
            AND id > max(
                  (SELECT COALESCE(MAX(id), 0) FROM runs WHERE chat_id = ?1 AND read_untrusted = 1),
                  COALESCE((SELECT cleared_after_run_id FROM chats WHERE chat_id = ?1), 0),
                  (SELECT COALESCE(MAX(run_id), 0) FROM distill_queue
                    WHERE cause = 'chat_exchanges' AND chat_id = ?1 AND run_id < ?2),
                  -- P8: the history cut, unless the owner seeded this chat (then its window starts at the floor above)
                  CASE WHEN EXISTS (SELECT 1 FROM distill_queue WHERE cause = 'chat_exchanges'
                                      AND chat_id = ?1 AND error = 'ingest') THEN 0 ELSE ?4 END)
          ORDER BY id LIMIT ?3",
    )
    .bind(chat_id)
    .bind(up_to)
    .bind(BATCH_EXCHANGES)
    .bind(super::sources::chat_floor(pool).await)
    .fetch_all(pool)
    .await?;
    let clip = |t: String| t.chars().take(SIDE_CHARS).collect::<String>();
    Ok(rows
        .into_iter()
        .map(|(run_id, asked, answered)| Turn {
            run_id,
            asked: clip(asked),
            answered: clip(answered),
        })
        .collect())
}

/// Marks `chat_id` as owner-ingested (P8): one `chat_exchanges` row, `done`, `error = 'ingest'`,
/// written once. `true` when written, `false` when the chat was already seeded (or has no runs).
/// Never calls the model. The row's `run_id` is the chat key negated: unique per chat under the
/// queue's once-index, and no watermark (a watermark is the largest `run_id` and this is below
/// zero); the daily caps skip `error = 'ingest'`.
pub(super) async fn seed(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<bool> {
    let key: Option<i64> = sqlx::query_scalar("SELECT MIN(id) FROM runs WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_one(pool)
        .await?;
    let Some(key) = key else {
        return Ok(false);
    };
    let now = chrono::Utc::now().to_rfc3339();
    let written = sqlx::query(
        "INSERT INTO distill_queue
           (cause, project_id, run_id, chat_id, status, attempts, error, created_at, finished_at)
         SELECT 'chat_exchanges', '', ?2, ?1, 'done', 0, 'ingest', ?3, ?3
          WHERE NOT EXISTS (SELECT 1 FROM distill_queue
                             WHERE cause = 'chat_exchanges' AND chat_id = ?1 AND error = 'ingest')",
    )
    .bind(chat_id)
    .bind(-key)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(written.rows_affected() > 0)
}

/// Queue the next batch of `chat_id` when it holds at least `min_exchanges` exchanges and the
/// daily ceilings leave room. `true` when a row was written. The row's `run_id` is the batch's last
/// exchange (the watermark), its `project_id` the only project the batch's turns carry, else `''`.
pub async fn enqueue(
    pool: &SqlitePool,
    chat_id: &str,
    min_exchanges: usize,
    now: DateTime<Utc>,
    day_start: DateTime<Utc>,
) -> sqlx::Result<bool> {
    if !sources::enabled(pool, CHAT_KEY).await {
        return Ok(false);
    }
    // An owner-asked seed (`error = 'ingest'`) is no work done today and counts toward no ceiling.
    let since = day_start.to_rfc3339();
    let today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM distill_queue
          WHERE cause = 'chat_exchanges' AND created_at >= ?1 AND error IS NOT 'ingest'",
    )
    .bind(&since)
    .fetch_one(pool)
    .await?;
    let of_chat: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM distill_queue
          WHERE cause = 'chat_exchanges' AND created_at >= ?1 AND chat_id = ?2
            AND error IS NOT 'ingest'",
    )
    .bind(&since)
    .bind(chat_id)
    .fetch_one(pool)
    .await?;
    if today >= MAX_CHAT_ROWS_PER_DAY || of_chat >= MAX_CHAT_ROWS_PER_CHAT_DAY {
        return Ok(false);
    }
    // With the largest limit the previous-batch watermark is the whole mark: this is the next batch.
    let turns = window(pool, chat_id, i64::MAX).await?;
    let Some(last) = turns.last().map(|t| t.run_id) else {
        return Ok(false);
    };
    if turns.len() < min_exchanges {
        return Ok(false);
    }
    let ids = serde_json::Value::from(turns.iter().map(|t| t.run_id).collect::<Vec<_>>());
    let projects: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT project_id FROM runs
          WHERE id IN (SELECT value FROM json_each(?)) AND project_id IS NOT NULL AND project_id <> ''",
    )
    .bind(ids.to_string())
    .fetch_all(pool)
    .await?;
    let project = match projects.as_slice() {
        [only] => only.as_str(),
        _ => "",
    };
    let written = sqlx::query(
        "INSERT OR IGNORE INTO distill_queue
           (cause, project_id, run_id, chat_id, status, attempts, created_at)
         VALUES ('chat_exchanges', ?, ?, ?, 'pending', 0, ?)",
    )
    .bind(project)
    .bind(last)
    .bind(chat_id)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;
    Ok(written.rows_affected() > 0)
}

/// Queue the idle chats that have a new batch. A chat with no new exchange since its last batch, or
/// one that is not idle, is left alone; P8: a chat that is all history and was never seeded is not
/// even looked at, so it takes none of the 50 places of a round.
pub async fn sweep(
    pool: &SqlitePool,
    now: DateTime<Utc>,
    day_start: DateTime<Utc>,
) -> sqlx::Result<()> {
    if !sources::enabled(pool, CHAT_KEY).await {
        return Ok(());
    }
    let idle_before = (now - chrono::Duration::minutes(IDLE_MINUTES)).to_rfc3339();
    let chats: Vec<String> = sqlx::query_scalar(
        "SELECT chat_id FROM runs
          WHERE chat_id IS NOT NULL AND mode = 'assistant' AND status = 'completed'
            AND from_relay_id IS NULL AND stdout <> ''
          GROUP BY chat_id
         HAVING MAX(created_at) <= ?1
            AND MAX(id) > COALESCE((SELECT MAX(run_id) FROM distill_queue q
                                     WHERE q.cause = 'chat_exchanges' AND q.chat_id = runs.chat_id), 0)
            AND (MAX(id) > ?2 OR EXISTS (SELECT 1 FROM distill_queue s
                                          WHERE s.cause = 'chat_exchanges' AND s.chat_id = runs.chat_id
                                            AND s.error = 'ingest'))
          ORDER BY MAX(id) DESC LIMIT 50",
    )
    .bind(idle_before)
    .bind(sources::chat_floor(pool).await)
    .fetch_all(pool)
    .await?;
    for chat in chats {
        if let Err(error) = enqueue(pool, &chat, MIN_EXCHANGES, now, day_start).await {
            tracing::warn!(%error, "could not queue a chat for distillation");
        }
    }
    Ok(())
}

/// A rotation closes a conversation: queue what it said, idle or not, from one exchange up.
pub async fn enqueue_rotated(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<bool> {
    enqueue_rotated_at(
        pool,
        chat_id,
        Utc::now(),
        local_day_start(chrono::Local::now()),
    )
    .await
}

pub(crate) async fn enqueue_rotated_at(
    pool: &SqlitePool,
    chat_id: &str,
    now: DateTime<Utc>,
    day_start: DateTime<Utc>,
) -> sqlx::Result<bool> {
    enqueue(pool, chat_id, 1, now, day_start).await
}

/// Which chats an owner-asked ingestion covers.
pub enum Selection {
    All,
    Project(String),
    Chats(Vec<String>),
}

/// Owner-asked ingestion of past chats (P8): seeds one `done` row per chat, never calls the model.
/// Returns `(seeded, skipped)`; a chat that already has a seed, or does not exist, is skipped.
pub async fn ingest(pool: &SqlitePool, sel: Selection) -> sqlx::Result<(u32, u32)> {
    let (mut seeded, mut skipped) = (0u32, 0u32);
    let ids: Vec<String> = match sel {
        Selection::All => {
            sqlx::query_scalar(
                "SELECT DISTINCT chat_id FROM runs
                  WHERE chat_id IS NOT NULL AND mode = 'assistant' AND status = 'completed'",
            )
            .fetch_all(pool)
            .await?
        }
        Selection::Project(project) => {
            sqlx::query_scalar(
                "SELECT DISTINCT chat_id FROM runs
                  WHERE chat_id IS NOT NULL AND mode = 'assistant' AND status = 'completed'
                    AND project_id = ?",
            )
            .bind(project)
            .fetch_all(pool)
            .await?
        }
        Selection::Chats(asked) => {
            let mut known = Vec::new();
            for id in asked {
                let exists: Option<i64> =
                    sqlx::query_scalar("SELECT 1 FROM chats WHERE chat_id = ?")
                        .bind(&id)
                        .fetch_optional(pool)
                        .await?;
                if exists.is_some() {
                    known.push(id);
                } else {
                    skipped += 1;
                }
            }
            known
        }
    };
    for id in ids {
        if seed(pool, &id).await? {
            seeded += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((seeded, skipped))
}

/// Close a row done without asking the model; a failed write is logged, not retried.
async fn close(pool: &SqlitePool, row: &QueueRow, now: DateTime<Utc>, note: Option<&str>) {
    let closed = async {
        let mut tx = pool.begin().await?;
        close_row(&mut tx, row.id, now, note).await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = closed {
        tracing::warn!(%error, "distillation: could not close chat row {}", row.id);
    }
}

/// Distil one `chat_exchanges` row. The conversation is the whole dossier (no owner notes); its
/// episodic items become `observed` observations, the rest `proposed` project candidates when the
/// batch has one project. Nothing is ever written `active`.
pub(super) async fn process(
    pool: &SqlitePool,
    asked: Extractor<'_>,
    embedder: Option<&dyn Embedder>,
    row: &QueueRow,
    now: DateTime<Utc>,
) {
    if !sources::enabled(pool, CHAT_KEY).await {
        close(pool, row, now, Some("disabled")).await;
        return;
    }
    let (Some(chat_id), Some(last)) = (row.chat_id.as_deref(), row.run_id) else {
        close(pool, row, now, None).await;
        return;
    };
    let turns = match window(pool, chat_id, last).await {
        Ok(turns) => turns,
        Err(_) => {
            fail(pool, row, "db", "reading the chat failed", now).await;
            return;
        }
    };
    if turns.is_empty() {
        close(pool, row, now, None).await;
        return;
    }
    let known_titles: Vec<String> = if row.project_id.is_empty() {
        Vec::new()
    } else {
        match sqlx::query_scalar(
            "SELECT title FROM knowledge
              WHERE scope_kind = 'project' AND scope_id = ? AND status IN ('active', 'proposed')
              ORDER BY id DESC LIMIT 50",
        )
        .bind(&row.project_id)
        .fetch_all(pool)
        .await
        {
            Ok(titles) => titles,
            Err(_) => {
                fail(pool, row, "db", "reading the chat failed", now).await;
                return;
            }
        }
    };
    let cause = Cause::ChatExchanges;
    let inputs = DossierInputs {
        cause,
        owner_context: Vec::new(),
        reviews: Vec::new(),
        job_prompt: None,
        items: Vec::new(),
        outcome: None,
        recovery: None,
        known_titles,
        gate_outputs: Vec::new(),
        conversation: turns
            .iter()
            .map(|t| (t.run_id, t.asked.clone(), t.answered.clone()))
            .collect(),
    };
    let built = dossier(&inputs, DOSSIER_CEILING);
    let answer = match ask(
        asked,
        extraction_prompt(cause, &built.text),
        &standing_for(cause),
        items_format(),
    )
    .await
    {
        Ok(answer) => answer,
        Err(error) => {
            fail(pool, row, "model", &format!("{:?}", error.kind()), now).await;
            return;
        }
    };
    let items = match parse_items(&answer) {
        Ok(items) => items,
        Err(_) => {
            fail(pool, row, "parse", "the answer is not a JSON array", now).await;
            return;
        }
    };
    let vectors = embed_items(embedder, &items).await;
    let model = embedder.map(|e| e.model());
    match write(
        pool,
        row,
        chat_id,
        &turns,
        &built.runs,
        &items,
        &vectors,
        model,
        now,
    )
    .await
    {
        Ok((observations, candidates, dropped)) => tracing::info!(
            "distillation: chat row {}: {} parsed, {observations} observations, {candidates} candidates, {dropped} dropped",
            row.id,
            items.len()
        ),
        Err(_) => fail(pool, row, "db", "writing the learnings failed", now).await,
    }
}

/// The writes of one chat row in a single transaction. Returns (observations, candidates, dropped).
#[allow(clippy::too_many_arguments)]
async fn write(
    pool: &SqlitePool,
    row: &QueueRow,
    chat_id: &str,
    turns: &[Turn],
    runs: &[i64],
    items: &[super::Item],
    vectors: &[Option<Vec<f32>>],
    model: Option<&str>,
    now: DateTime<Utc>,
) -> sqlx::Result<(usize, usize, usize)> {
    use crate::knowledge::Layer;
    let key: i64 = sqlx::query_scalar("SELECT COALESCE(MIN(id), 0) FROM runs WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_one(pool)
        .await?;
    // The evidence names the chat by its numeric key (never the UUID), then the runs read.
    let mut evidence_list = vec![serde_json::json!({"t": "chat", "id": key})];
    evidence_list.extend(
        runs.iter()
            .map(|r| serde_json::json!({"t": "run", "id": r})),
    );
    let evidence = serde_json::Value::Array(evidence_list).to_string();
    let reasoning = "distilled from a conversation (chat_exchanges)".to_string();
    let sources: Vec<&str> = turns.iter().map(|t| t.asked.as_str()).collect();
    let scope_id = chat_scope_id(&row.project_id, key);
    let day_start = local_day_start(now.with_timezone(&chrono::Local)).to_rfc3339();

    let mut tx = pool.begin().await?;
    let today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM knowledge
          WHERE scope_kind = 'observation' AND scope_id = ? AND created_at >= ?",
    )
    .bind(&scope_id)
    .bind(&day_start)
    .fetch_one(&mut *tx)
    .await?;
    let mut obs_left = MAX_OBS_PER_ROW.min((MAX_OBS_PER_CHAT_DAY - today).max(0) as usize);
    let (mut observations, mut candidates, mut dropped) = (0usize, 0usize, 0usize);
    let mut proposed_any = false;
    let subject = Subject {
        cause: Cause::ChatExchanges,
        project_id: &row.project_id,
        evidence: &evidence,
        reasoning: &reasoning,
        sources: &sources,
        model,
    };
    for (index, item) in items.iter().enumerate() {
        let vector = vectors.get(index).and_then(|v| v.as_deref());
        let embedded = match (vector, model) {
            (Some(vector), Some(model)) => Some((vector, model)),
            _ => None,
        };
        if item.layer == Layer::Episodic {
            if obs_left == 0 {
                dropped += 1;
                continue;
            }
            obs_left -= 1;
            let points_at = if item.files.is_empty() {
                None
            } else {
                Some(serde_json::json!(item.files).to_string())
            };
            let fingerprint = crate::knowledge::title_fingerprint(&item.title);
            let id = crate::knowledge::record_observation_in(
                &mut tx,
                &scope_id,
                super::door(item.layer).0,
                &item.title,
                &item.body,
                &crate::knowledge::Provenance {
                    distill_cause: Some(Cause::ChatExchanges.as_str()),
                    evidence: Some(&evidence),
                    points_at: points_at.as_deref(),
                    fingerprint: fingerprint.as_deref(),
                    ..Default::default()
                },
            )
            .await?;
            if let Some((vector, model)) = embedded {
                crate::embed::store_in(&mut tx, id, model, vector).await?;
            }
            // The row stays `observed`; the event says the text is the owner's own.
            if crate::governance::quote_guard::quotes(
                &format!("{}\n{}", item.title, item.body),
                &sources,
            ) {
                crate::knowledge::note_quoted_owner_text_in(&mut tx, id).await?;
            }
            observations += 1;
        } else if row.project_id.is_empty() || candidates >= MAX_CHAT_CANDIDATES_PER_ROW {
            dropped += 1;
        } else {
            match write_one_item(&mut tx, &subject, item, vector).await? {
                Written::Skipped => {}
                Written::Recorded => candidates += 1,
                Written::Proposed => {
                    proposed_any = true;
                    candidates += 1;
                }
            }
        }
    }
    close_row(&mut tx, row.id, now, None).await?;
    tx.commit().await?;
    // Only after the commit: a waker must never see a proposal the database does not hold yet.
    if proposed_any {
        nucleos_base::wake::bump(nucleos_base::wake::Topic::Proposals);
    }
    Ok((observations, candidates, dropped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_scope_ids_are_read_back_by_the_last_marker_with_digits_only() {
        assert_eq!(chat_scope_id("alpha", 5), "alpha:chat:5");
        assert_eq!(chat_scope_id("", 5), "-:chat:5");
        assert_eq!(parse_chat_scope("-:chat:5"), Some(("-", 5)));
        // a project id may hold ':'; the LAST marker wins, so `a:chat:5` is the project here
        assert_eq!(parse_chat_scope("a:chat:5:chat:9"), Some(("a:chat:5", 9)));
        for bad in [
            "week:2026-W43",
            "alpha:chat:",
            "alpha:chat:5x",
            "alpha",
            ":chat:5",
            "alpha:chat:+1",
        ] {
            assert_eq!(parse_chat_scope(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_local_day_starts_at_local_midnight() {
        use chrono::{FixedOffset, TimeZone, Utc};
        let now = FixedOffset::east_opt(3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 22, 0, 30, 0)
            .unwrap();
        assert_eq!(
            local_day_start(now),
            Utc.with_ymd_and_hms(2026, 10, 21, 23, 0, 0).unwrap()
        );
    }

    use super::fixtures::*;

    #[tokio::test]
    async fn the_window_is_the_forty_oldest_exchanges_clipped_per_side() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let mut ids = Vec::new();
        for i in 0..45 {
            ids.push(
                turn(
                    &pool,
                    &chat,
                    None,
                    &format!("q{i}{}", "x".repeat(2_000)),
                    "a",
                )
                .await,
            );
        }
        let got = window(&pool, &chat, ids[39]).await.unwrap();
        assert_eq!(
            got.iter().map(|t| t.run_id).collect::<Vec<_>>(),
            ids[..40].to_vec()
        );
        assert!(got.iter().all(|t| t.asked.chars().count() <= SIDE_CHARS));
    }

    #[tokio::test]
    async fn the_window_starts_after_a_tainted_turn_a_clear_and_the_previous_batch() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let mut ids = Vec::new();
        for _ in 0..12 {
            ids.push(turn(&pool, &chat, None, "q", "a").await);
        }
        async fn first(pool: &SqlitePool, chat: &str, last: i64) -> i64 {
            window(pool, chat, last)
                .await
                .unwrap()
                .first()
                .unwrap()
                .run_id
        }
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(ids[2])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            first(&pool, &chat, ids[11]).await,
            ids[3],
            "the last tainted turn is the floor"
        );
        sqlx::query("UPDATE chats SET cleared_after_run_id = ? WHERE chat_id = ?")
            .bind(ids[5])
            .bind(&chat)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            first(&pool, &chat, ids[11]).await,
            ids[6],
            "a clear is a floor too"
        );
        sqlx::query(
            "INSERT INTO distill_queue (cause, project_id, run_id, chat_id, status, attempts, created_at)
             VALUES ('chat_exchanges', '', ?, ?, 'done', 0, '2026-10-02T00:00:00Z')",
        )
        .bind(ids[8])
        .bind(&chat)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            first(&pool, &chat, ids[11]).await,
            ids[9],
            "the previous batch is the watermark"
        );
    }

    #[tokio::test]
    async fn relayed_failed_and_empty_turns_are_not_exchanges() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let good = turn(&pool, &chat, None, "q", "a").await;
        let relayed = turn(&pool, &chat, None, "q", "a").await;
        sqlx::query("UPDATE runs SET from_relay_id = 1 WHERE id = ?")
            .bind(relayed)
            .execute(&pool)
            .await
            .unwrap();
        let failed = turn(&pool, &chat, None, "q", "a").await;
        sqlx::query("UPDATE runs SET status = 'failed' WHERE id = ?")
            .bind(failed)
            .execute(&pool)
            .await
            .unwrap();
        let last = turn(&pool, &chat, None, "q", "").await;
        let got = window(&pool, &chat, last).await.unwrap();
        assert_eq!(got.iter().map(|t| t.run_id).collect::<Vec<_>>(), vec![good]);
    }

    #[tokio::test]
    async fn the_history_cut_hides_old_turns_until_the_chat_is_seeded() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let mut ids = Vec::new();
        for _ in 0..6 {
            ids.push(turn(&pool, &chat, None, "q", "a").await);
        }
        set_floor(&pool, ids[3]).await;
        let first = |w: Vec<Turn>| w.first().unwrap().run_id;
        assert_eq!(
            first(window(&pool, &chat, ids[5]).await.unwrap()),
            ids[4],
            "the cut hides turns up to ids[3]"
        );
        assert!(seed(&pool, &chat).await.unwrap());
        assert!(
            !seed(&pool, &chat).await.unwrap(),
            "seeding twice writes nothing"
        );
        assert_eq!(
            first(window(&pool, &chat, ids[5]).await.unwrap()),
            ids[0],
            "a seeded chat ignores the cut"
        );
    }

    #[tokio::test]
    async fn two_chats_can_both_be_seeded() {
        let pool = crate::testdb::fresh_pool().await;
        let one = chat_with(&pool).await;
        let two = chat_with(&pool).await;
        turn(&pool, &one, None, "q", "a").await;
        turn(&pool, &two, None, "q", "a").await;
        assert!(seed(&pool, &one).await.unwrap());
        assert!(seed(&pool, &two).await.unwrap());
    }

    fn now() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap()
    }

    fn day() -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 10, 22, 0, 0, 0).unwrap()
    }

    /// (chat_id, project_id, run_id) of the queued batches; owner seeds are not batches.
    async fn rows(pool: &SqlitePool) -> Vec<(String, String, Option<i64>)> {
        sqlx::query_as(
            "SELECT chat_id, project_id, run_id FROM distill_queue
              WHERE cause = 'chat_exchanges' AND error IS NOT 'ingest' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn four_exchanges_queue_one_row_for_the_last_run_with_the_single_project() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        for _ in 0..3 {
            turn(&pool, &chat, Some("alpha"), "q", "a").await;
        }
        assert!(
            !enqueue(&pool, &chat, MIN_EXCHANGES, now(), day())
                .await
                .unwrap(),
            "three are not enough"
        );
        let last = turn(&pool, &chat, Some("alpha"), "q", "a").await;
        assert!(
            enqueue(&pool, &chat, MIN_EXCHANGES, now(), day())
                .await
                .unwrap()
        );
        assert!(
            !enqueue(&pool, &chat, MIN_EXCHANGES, now(), day())
                .await
                .unwrap(),
            "the watermark moved"
        );
        assert_eq!(rows(&pool).await, vec![(chat, "alpha".into(), Some(last))]);
    }

    #[tokio::test]
    async fn a_long_chat_is_cut_into_batches_and_stops_at_the_per_chat_cap() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        for _ in 0..130 {
            turn(&pool, &chat, None, "q", "a").await;
        }
        for _ in 0..5 {
            sweep(&pool, now(), day()).await.unwrap();
        }
        assert_eq!(rows(&pool).await.len() as i64, MAX_CHAT_ROWS_PER_CHAT_DAY);
    }

    #[tokio::test]
    async fn the_global_daily_cap_bounds_the_rows_and_history_resumes_next_day() {
        let pool = crate::testdb::fresh_pool().await;
        for _ in 0..(MAX_CHAT_ROWS_PER_DAY + 2) {
            let c = chat_with(&pool).await;
            for _ in 0..4 {
                turn(&pool, &c, None, "q", "a").await;
            }
        }
        sweep(&pool, now(), day()).await.unwrap();
        assert_eq!(rows(&pool).await.len() as i64, MAX_CHAT_ROWS_PER_DAY);
        let tomorrow = chrono::Duration::days(1);
        sweep(&pool, now() + tomorrow, day() + tomorrow)
            .await
            .unwrap();
        assert_eq!(rows(&pool).await.len() as i64, MAX_CHAT_ROWS_PER_DAY + 2);
    }

    #[tokio::test]
    async fn two_projects_or_none_give_an_empty_project_id() {
        let pool = crate::testdb::fresh_pool().await;
        let mixed = chat_with(&pool).await;
        for p in ["alpha", "alpha", "bravo", "bravo"] {
            turn(&pool, &mixed, Some(p), "q", "a").await;
        }
        let unstamped = chat_with(&pool).await;
        for _ in 0..4 {
            turn(&pool, &unstamped, None, "q", "a").await;
        }
        assert!(
            enqueue(&pool, &mixed, MIN_EXCHANGES, now(), day())
                .await
                .unwrap()
        );
        assert!(
            enqueue(&pool, &unstamped, MIN_EXCHANGES, now(), day())
                .await
                .unwrap()
        );
        let projects: Vec<String> = rows(&pool).await.into_iter().map(|r| r.1).collect();
        assert_eq!(projects, vec![String::new(), String::new()]);
    }

    #[tokio::test]
    async fn only_idle_chats_are_swept() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        for _ in 0..4 {
            turn(&pool, &chat, None, "q", "a").await;
        }
        let ten_minutes_ago = (now() - chrono::Duration::minutes(10)).to_rfc3339();
        sqlx::query("UPDATE runs SET created_at = ? WHERE chat_id = ?")
            .bind(ten_minutes_ago)
            .bind(&chat)
            .execute(&pool)
            .await
            .unwrap();
        sweep(&pool, now(), day()).await.unwrap();
        assert!(
            rows(&pool).await.is_empty(),
            "the chat spoke ten minutes ago"
        );
        sweep(
            &pool,
            now() + chrono::Duration::minutes(IDLE_MINUTES),
            day(),
        )
        .await
        .unwrap();
        assert_eq!(rows(&pool).await.len(), 1);
    }

    #[tokio::test]
    async fn a_rotation_ignores_idleness_and_needs_one_exchange() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let only = turn(&pool, &chat, None, "q", "a").await;
        sqlx::query("UPDATE runs SET created_at = ? WHERE chat_id = ?")
            .bind(now().to_rfc3339())
            .bind(&chat)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            enqueue_rotated_at(&pool, &chat, now(), day())
                .await
                .unwrap()
        );
        assert_eq!(rows(&pool).await, vec![(chat, String::new(), Some(only))]);
    }

    #[tokio::test]
    async fn a_switched_off_source_queues_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        for _ in 0..4 {
            turn(&pool, &chat, None, "q", "a").await;
        }
        sources::set(&pool, CHAT_KEY, false).await.unwrap();
        sweep(&pool, now(), day()).await.unwrap();
        assert!(
            !enqueue_rotated_at(&pool, &chat, now(), day())
                .await
                .unwrap()
        );
        assert!(rows(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn an_old_chat_is_not_swept_until_seeded() {
        let pool = crate::testdb::fresh_pool().await;
        let chat = chat_with(&pool).await;
        let mut last = 0;
        for _ in 0..4 {
            last = turn(&pool, &chat, None, "q", "a").await;
        }
        set_floor(&pool, last).await;
        sweep(&pool, now(), day()).await.unwrap();
        assert!(
            rows(&pool).await.is_empty(),
            "history is not ingested by itself"
        );
        assert!(seed(&pool, &chat).await.unwrap());
        sweep(&pool, now(), day()).await.unwrap();
        assert_eq!(rows(&pool).await.len(), 1, "an ingested chat is swept");
    }

    #[tokio::test]
    async fn a_seed_row_counts_toward_no_ceiling() {
        let pool = crate::testdb::fresh_pool().await;
        for _ in 0..(MAX_CHAT_ROWS_PER_DAY + 1) {
            let c = chat_with(&pool).await;
            for _ in 0..4 {
                turn(&pool, &c, None, "q", "a").await;
            }
            assert!(seed(&pool, &c).await.unwrap());
        }
        // The seeds were written "today" by the real clock; the ceiling day is the real one too.
        sweep(
            &pool,
            Utc::now() + chrono::Duration::days(1),
            local_day_start(chrono::Local::now()),
        )
        .await
        .unwrap();
        assert_eq!(rows(&pool).await.len() as i64, MAX_CHAT_ROWS_PER_DAY);
    }

    /// A chat with one completed turn stamped with `project`.
    async fn chat_in(pool: &SqlitePool, project: &str) -> String {
        let chat = chat_with(pool).await;
        turn(pool, &chat, Some(project), "q", "a").await;
        chat
    }

    #[tokio::test]
    async fn ingest_by_project_seeds_only_that_projects_chats() {
        let pool = crate::testdb::fresh_pool().await;
        chat_in(&pool, "alpha").await;
        chat_in(&pool, "alpha").await;
        chat_in(&pool, "bravo").await;
        assert_eq!(
            ingest(&pool, Selection::Project("alpha".into()))
                .await
                .unwrap(),
            (2, 0)
        );
        assert_eq!(
            ingest(&pool, Selection::Project("alpha".into()))
                .await
                .unwrap(),
            (0, 2)
        );
        let seeds: Vec<(i64, String, Option<String>)> = sqlx::query_as(
            "SELECT run_id, status, error FROM distill_queue WHERE cause = 'chat_exchanges'",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(seeds.len(), 2);
        for (run_id, status, error) in seeds {
            assert!(run_id <= 0, "a seed is no watermark");
            assert_eq!(
                (status.as_str(), error.as_deref()),
                ("done", Some("ingest"))
            );
        }
    }

    #[tokio::test]
    async fn ingest_all_seeds_every_chat_and_a_second_call_skips_them() {
        let pool = crate::testdb::fresh_pool().await;
        for _ in 0..3 {
            chat_in(&pool, "alpha").await;
        }
        assert_eq!(ingest(&pool, Selection::All).await.unwrap(), (3, 0));
        assert_eq!(ingest(&pool, Selection::All).await.unwrap(), (0, 3));
    }

    #[tokio::test]
    async fn ingest_of_an_unknown_uuid_is_skipped_not_seeded() {
        let pool = crate::testdb::fresh_pool().await;
        assert_eq!(
            ingest(&pool, Selection::Chats(vec!["no-such".into()]))
                .await
                .unwrap(),
            (0, 1)
        );
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM distill_queue")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}

#[cfg(any(test, feature = "testkit"))]
pub mod fixtures {
    use sqlx::SqlitePool;

    pub async fn chat_with(pool: &SqlitePool) -> String {
        crate::chats::create(pool, crate::chats::Brain::Local, None)
            .await
            .unwrap()
    }

    /// The chat's numeric key: its oldest run (spec 4.1, `chat_key`).
    pub async fn chat_key(pool: &SqlitePool, chat: &str) -> i64 {
        sqlx::query_scalar("SELECT MIN(id) FROM runs WHERE chat_id = ?")
            .bind(chat)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// One completed assistant turn of `chat`; returns the run id.
    pub async fn turn(
        pool: &SqlitePool,
        chat: &str,
        project: Option<&str>,
        asked: &str,
        answered: &str,
    ) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, stdout, project_id, created_at)
             VALUES (?, 'completed', 'assistant', 's', ?, ?, ?, '2026-10-01T10:00:00+00:00')",
        )
        .bind(asked)
        .bind(chat)
        .bind(answered)
        .bind(project)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Moves the P8 history cut to `run_id`.
    pub async fn set_floor(pool: &SqlitePool, run_id: i64) {
        sqlx::query(
            "INSERT INTO schema_meta (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(super::super::sources::CHAT_FLOOR_KEY)
        .bind(run_id.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    /// An `observed` observation created `days` days ago; returns its id.
    pub async fn stale_observation(pool: &SqlitePool, days: i64) -> i64 {
        let at = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('episodic', 'observation', 'alpha:chat:1', 'distiller', 'memory',
                     'stale', 'stale', 'observed', ?)",
        )
        .bind(at)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }
}
