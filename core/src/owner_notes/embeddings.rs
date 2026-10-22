//! Embeddings of the owner's notes (second-brain v2, contract A2).
//!
//! Best-effort and asynchronous: creating or editing a note never waits for an embedding and never
//! fails because of one. A periodic pass (`run_loop`) fills the notes that have no vector, whose
//! vector comes from another model, or whose text changed since (`text_hash` differs).
//!
//! The vectors come from the embedder installed for knowledge (`embed::installed()`) and use its
//! `encode`/`decode`/`cosine`; `embed.rs` is not touched. Nothing here logs a note's text or a
//! vector: ids and counts only. SQL on `owner_notes` is allowed in this module (§5 boundary).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sqlx::{Row, SqlitePool};

use crate::embed::{Embedder, encode};

/// Consecutive embed errors after which a pass takes the embedder for down and stops, as
/// `embed::backfill` does.
const DOWN_AFTER: usize = 3;

/// Failures of one unchanged note after which the passes skip it.
const NOTE_FAILURES: usize = 3;

/// How long a skipped note stays skipped. A change of its text releases it at once; this expiry
/// only bounds the damage of an embedder outage that happened to fail the first notes three passes
/// running, which would otherwise park them until the process restarts.
const PARK_FOR: Duration = Duration::from_secs(60 * 60);

/// Per note, in memory only (no schema change): the text hash that failed, how many times in a
/// row, and when it last did. Without it, `NOTE_FAILURES` permanently failing notes at the front of
/// the id order would end every pass before it reached the notes behind them.
static FAILURES: LazyLock<Mutex<FailureMap>> = LazyLock::new(Mutex::default);

type FailureMap = HashMap<i64, (String, usize, Instant)>;

fn failures() -> std::sync::MutexGuard<'static, FailureMap> {
    FAILURES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the passes skip this note: it failed `NOTE_FAILURES` times on this very text, lately.
fn is_parked(id: i64, hash: &str) -> bool {
    failures().get(&id).is_some_and(|(failed_hash, count, at)| {
        failed_hash == hash && *count >= NOTE_FAILURES && at.elapsed() < PARK_FOR
    })
}

fn record_failure(id: i64, hash: &str) {
    let mut map = failures();
    let entry = map
        .entry(id)
        .or_insert_with(|| (hash.to_string(), 0, Instant::now()));
    // New text, or an expired park: the count starts over.
    if entry.0 != hash || entry.2.elapsed() >= PARK_FOR {
        *entry = (hash.to_string(), 0, Instant::now());
    }
    entry.1 += 1;
    entry.2 = Instant::now();
}

fn clear_failure(id: i64) {
    failures().remove(&id);
}

/// What may be logged about an embed error: its `io::ErrorKind` and, for an Ollama non-2xx, the
/// HTTP status. Never the message, which for a non-2xx carries the response body and that may echo
/// the note's text. The status is read from the `Ollama returned <status>: ...` prefix `embed.rs`
/// writes; anything that does not parse leaves the kind alone.
fn error_kind(error: &std::io::Error) -> String {
    let message = error.to_string();
    let status = message
        .strip_prefix("Ollama returned ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|code| code.parse::<u16>().ok());
    match status {
        Some(code) => format!("{:?}, HTTP {code}", error.kind()),
        None => format!("{:?}", error.kind()),
    }
}

/// Notes embedded per `backfill` call made by the loop; a full batch is followed by another.
const BATCH: i64 = 50;

const TICK: Duration = Duration::from_secs(60);

/// This module's own wake-up. `embed::nudge` is not used: its single `notify_one` is consumed by
/// the distiller's loop, so a nudge sent there would never reach this one.
static WAKE: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);

/// Wakes `run_loop` ahead of its next tick (a note was created or edited).
pub fn nudge() {
    WAKE.notify_one();
}

/// FNV-1a, 64 bits, as 16 lowercase hex digits. Change detection only, not a security hash.
pub fn text_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Whether a stored row (`model`, `text_hash`), if any, still matches this embedder and this text.
fn is_current(stored: Option<(&str, &str)>, model: &str, hash: &str) -> bool {
    matches!(stored, Some((m, h)) if m == model && h == hash)
}

/// Upserts the vector of `note_id`. `hash` is the hash of the text the vector was computed from.
async fn store(
    pool: &SqlitePool,
    note_id: i64,
    model: &str,
    vector: &[f32],
    hash: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO owner_note_embeddings (note_id, model, dim, vector, text_hash, created_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(note_id) DO UPDATE SET
           model = excluded.model,
           dim = excluded.dim,
           vector = excluded.vector,
           text_hash = excluded.text_hash,
           created_at = excluded.created_at",
    )
    .bind(note_id)
    .bind(model)
    .bind(vector.len() as i64)
    .bind(encode(vector))
    .bind(hash)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Embeds one note; `true` when a vector was written.
///
/// `false` (never an error) when the note is missing or not active, when its stored vector is
/// already current for this model and text, or when the embedder failed: the row is then left as
/// it was and the periodic pass tries again. Only a database error is returned.
pub async fn embed_note(
    pool: &SqlitePool,
    embedder: &dyn Embedder,
    id: i64,
) -> sqlx::Result<bool> {
    let Some(note) = sqlx::query(
        "SELECT n.note_text, e.model AS stored_model, e.text_hash AS stored_hash
           FROM owner_notes n
           LEFT JOIN owner_note_embeddings e ON e.note_id = n.id
          WHERE n.id = ? AND n.state = 'active'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(false);
    };
    let text: String = note.get("note_text");
    let stored_model: Option<String> = note.get("stored_model");
    let stored_hash: Option<String> = note.get("stored_hash");
    let hash = text_hash(&text);
    let stored = stored_model.as_deref().zip(stored_hash.as_deref());
    if is_current(stored, embedder.model(), &hash) {
        return Ok(false);
    }
    let vector = match embedder.embed(&text).await {
        Ok(vector) if !vector.is_empty() => vector,
        Ok(_) => {
            tracing::warn!("owner note embeddings: note {id} got an empty vector");
            return Ok(false);
        }
        Err(error) => {
            tracing::warn!(
                "owner note embeddings: note {id} not embedded ({})",
                error_kind(&error)
            );
            return Ok(false);
        }
    };
    store(pool, id, embedder.model(), &vector, &hash).await?;
    clear_failure(id);
    Ok(true)
}

/// Embeds up to `limit` active notes that have no vector, a vector from another model, or a vector
/// computed from different text, lowest id first, and returns how many it embedded.
///
/// `DOWN_AFTER` errors in a row end the pass (the embedder is down); a success resets the count.
/// A failing embedder therefore writes nothing and the call still returns `Ok`. A note that failed
/// `NOTE_FAILURES` times on its current text is skipped (see `FAILURES`), so it cannot starve the
/// notes behind it. The first error of a pass is logged, as a kind or status only.
///
/// The text hash cannot be computed in SQL, so every active note is read and filtered here. That is
/// a few thousand short rows at worst, once a minute, and it keeps the table free of a second
/// staleness signal that could drift from the text.
pub async fn backfill(
    pool: &SqlitePool,
    embedder: &dyn Embedder,
    limit: i64,
) -> sqlx::Result<usize> {
    if limit <= 0 {
        return Ok(0);
    }
    let rows = sqlx::query(
        "SELECT n.id, n.note_text, e.model AS stored_model, e.text_hash AS stored_hash
           FROM owner_notes n
           LEFT JOIN owner_note_embeddings e ON e.note_id = n.id
          WHERE n.state = 'active'
          ORDER BY n.id",
    )
    .fetch_all(pool)
    .await?;
    let mut done = 0;
    let mut errors_in_a_row = 0;
    let mut warned = false;
    for row in rows {
        if done as i64 >= limit {
            break;
        }
        let id: i64 = row.get("id");
        let text: String = row.get("note_text");
        let stored_model: Option<String> = row.get("stored_model");
        let stored_hash: Option<String> = row.get("stored_hash");
        let hash = text_hash(&text);
        let stored = stored_model.as_deref().zip(stored_hash.as_deref());
        if is_current(stored, embedder.model(), &hash) {
            continue;
        }
        if is_parked(id, &hash) {
            continue;
        }
        let outcome = match embedder.embed(&text).await {
            Ok(vector) if !vector.is_empty() => Ok(vector),
            Ok(_) => Err("empty vector".to_string()),
            Err(error) => Err(error_kind(&error)),
        };
        match outcome {
            Ok(vector) => {
                errors_in_a_row = 0;
                store(pool, id, embedder.model(), &vector, &hash).await?;
                clear_failure(id);
                done += 1;
            }
            Err(kind) => {
                record_failure(id, &hash);
                if !warned {
                    warned = true;
                    tracing::warn!("owner note embeddings: note {id} not embedded ({kind})");
                }
                errors_in_a_row += 1;
                if errors_in_a_row >= DOWN_AFTER {
                    tracing::warn!(
                        "owner note embeddings: {DOWN_AFTER} errors in a row, ending the pass"
                    );
                    break;
                }
            }
        }
    }
    Ok(done)
}

/// The worker: every 60 seconds, or sooner on `nudge`, embed the notes that need it. The embedder
/// is read per tick, so installing or swapping it needs no restart; with none installed the tick
/// does nothing.
pub async fn run_loop(state: crate::state::AppState) {
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = WAKE.notified() => {}
        }
        let Some(embedder) = crate::embed::installed() else {
            continue;
        };
        let mut total = 0;
        loop {
            match backfill(&state.pool, embedder.as_ref(), BATCH).await {
                Ok(done) => {
                    total += done;
                    if (done as i64) < BATCH {
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!("owner note embeddings: backfill failed: {error}");
                    break;
                }
            }
        }
        if total > 0 {
            tracing::info!("owner note embeddings: embedded {total} note(s)");
        }
    }
}

#[cfg(test)]
mod tests {
    use sqlx::{Row, SqlitePool};

    use super::{backfill, embed_note, text_hash};
    use crate::embed::{FakeEmbedder, decode};

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn fake(model: &str) -> FakeEmbedder {
        FakeEmbedder::new(model, Vec::new(), Some(vec![1.0, 0.0]))
    }

    async fn note(pool: &SqlitePool, text: &str) -> i64 {
        crate::owner_notes::create(pool, text, "shell")
            .await
            .unwrap()
    }

    async fn rows(pool: &SqlitePool) -> Vec<(i64, String, i64, String)> {
        sqlx::query(
            "SELECT note_id, model, dim, text_hash FROM owner_note_embeddings ORDER BY note_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.get("note_id"),
                r.get("model"),
                r.get("dim"),
                r.get("text_hash"),
            )
        })
        .collect()
    }

    fn calls(embedder: &FakeEmbedder) -> usize {
        embedder.calls.lock().unwrap().len()
    }

    #[test]
    fn the_hash_is_fnv1a_64_in_sixteen_hex_digits() {
        assert_eq!(text_hash(""), "cbf29ce484222325");
        assert_eq!(text_hash("a"), "af63dc4c8601ec8c");
        assert_ne!(text_hash("note one"), text_hash("note two"));
        assert_eq!(text_hash("note one"), text_hash("note one"));
    }

    #[tokio::test]
    async fn backfill_fills_the_active_notes_that_have_no_row() {
        let pool = test_pool().await;
        let first = note(&pool, "first note").await;
        let second = note(&pool, "second note").await;
        let archived = note(&pool, "archived note").await;
        sqlx::query("UPDATE owner_notes SET state = 'archived' WHERE id = ?")
            .bind(archived)
            .execute(&pool)
            .await
            .unwrap();

        let embedder = fake("fake");
        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 2);

        let stored = rows(&pool).await;
        let ids: Vec<i64> = stored.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec![first, second]);
        assert_eq!(stored[0].1, "fake");
        assert_eq!(stored[0].2, 2);
        assert_eq!(stored[0].3, text_hash("first note"));
        let blob: Vec<u8> = sqlx::query_scalar("SELECT vector FROM owner_note_embeddings LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(decode(&blob, 2), Some(vec![1.0, 0.0]));
    }

    #[tokio::test]
    async fn backfill_honours_its_limit() {
        let pool = test_pool().await;
        for text in ["one", "two", "three"] {
            note(&pool, text).await;
        }
        let embedder = fake("fake");
        assert_eq!(backfill(&pool, &embedder, 2).await.unwrap(), 2);
        assert_eq!(rows(&pool).await.len(), 2);
        assert_eq!(backfill(&pool, &embedder, 2).await.unwrap(), 1);
        assert_eq!(rows(&pool).await.len(), 3);
    }

    #[tokio::test]
    async fn an_edited_note_is_embedded_again() {
        let pool = test_pool().await;
        let id = note(&pool, "before the edit").await;
        let embedder = fake("fake");
        backfill(&pool, &embedder, 10).await.unwrap();

        sqlx::query("UPDATE owner_notes SET note_text = ? WHERE id = ?")
            .bind("after the edit")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 1);

        let stored = rows(&pool).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].3, text_hash("after the edit"));
    }

    #[tokio::test]
    async fn an_unchanged_note_is_skipped() {
        let pool = test_pool().await;
        let id = note(&pool, "steady note").await;
        let embedder = fake("fake");
        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 1);
        assert_eq!(calls(&embedder), 1);

        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 0);
        assert!(!embed_note(&pool, &embedder, id).await.unwrap());
        assert_eq!(calls(&embedder), 1, "no further embed call");
    }

    #[tokio::test]
    async fn a_row_from_another_model_is_replaced() {
        let pool = test_pool().await;
        note(&pool, "model swap").await;
        backfill(&pool, &fake("old-model"), 10).await.unwrap();
        assert_eq!(rows(&pool).await[0].1, "old-model");

        let newer = FakeEmbedder::new("new-model", Vec::new(), Some(vec![0.0, 1.0, 0.0]));
        assert_eq!(backfill(&pool, &newer, 10).await.unwrap(), 1);

        let stored = rows(&pool).await;
        assert_eq!(stored.len(), 1, "one row per note");
        assert_eq!(stored[0].1, "new-model");
        assert_eq!(stored[0].2, 3);
    }

    #[tokio::test]
    async fn a_failing_embedder_writes_nothing_and_still_returns_ok() {
        let pool = test_pool().await;
        for text in ["a", "b", "c", "d", "e"] {
            note(&pool, text).await;
        }
        let embedder = FakeEmbedder::failing();

        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 0);
        assert!(rows(&pool).await.is_empty());
        assert_eq!(calls(&embedder), 3, "the pass ends after three errors");

        let id = note(&pool, "f").await;
        assert!(!embed_note(&pool, &embedder, id).await.unwrap());
        assert!(rows(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn notes_that_keep_failing_stop_starving_the_ones_behind_them() {
        let pool = test_pool().await;
        // Texts unique to this test: the failure map is process-wide and note ids repeat across tests.
        let poisoned = [
            note(&pool, "starve-poison one").await,
            note(&pool, "starve-poison two").await,
            note(&pool, "starve-poison three").await,
        ];
        let healthy = note(&pool, "starve-good note").await;
        let embedder = FakeEmbedder::new(
            "fake",
            vec![("starve-good".to_string(), vec![1.0, 0.0])],
            None,
        );

        // Three passes of three errors each: the front notes reach their third failure.
        for _ in 0..3 {
            assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 0);
        }
        assert_eq!(calls(&embedder), 9);

        // Now they are skipped and the note behind them is reached.
        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 1);
        assert_eq!(calls(&embedder), 10, "only the healthy note was tried");
        let ids: Vec<i64> = rows(&pool).await.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec![healthy]);

        // Editing a skipped note changes its hash, which releases it.
        sqlx::query("UPDATE owner_notes SET note_text = ? WHERE id = ?")
            .bind("starve-good again")
            .bind(poisoned[0])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(backfill(&pool, &embedder, 10).await.unwrap(), 1);
        assert_eq!(calls(&embedder), 11);
    }

    #[test]
    fn an_error_is_logged_as_its_kind_and_status_never_its_message() {
        let body = std::io::Error::other("Ollama returned 404 Not Found: model 'secret note text'");
        let logged = super::error_kind(&body);
        assert_eq!(logged, "Other, HTTP 404");
        assert!(!logged.contains("secret"));

        let timeout = std::io::Error::new(std::io::ErrorKind::TimedOut, "slow");
        assert_eq!(super::error_kind(&timeout), "TimedOut");
    }

    #[tokio::test]
    async fn embed_note_writes_one_note_and_ignores_missing_or_archived_ones() {
        let pool = test_pool().await;
        let id = note(&pool, "just this one").await;
        let other = note(&pool, "not this one").await;
        let embedder = fake("fake");

        assert!(embed_note(&pool, &embedder, id).await.unwrap());
        assert!(!embed_note(&pool, &embedder, 9999).await.unwrap());
        sqlx::query("UPDATE owner_notes SET state = 'archived' WHERE id = ?")
            .bind(other)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!embed_note(&pool, &embedder, other).await.unwrap());

        let stored = rows(&pool).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].0, id);
    }
}
