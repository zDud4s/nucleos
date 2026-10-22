//! Reading a note's surroundings: the notes pinned to a target, and the notes that resemble one.
//!
//! Part of the owner-notes boundary (`.ai/specs/2026-10-02-second-brain-design.md` §5): this module
//! reads `owner_notes*` and nothing else on the agent's side does. It never logs, and nothing it
//! returns reaches a prompt.

use std::collections::HashMap;

use sqlx::SqlitePool;

use super::{LinkError, NOTE_COLUMNS, NoteError, OwnerNote, TARGET_KINDS};

/// Weight of the embedding signal: the cosine itself, once it clears [`EMBEDDING_FLOOR`].
const EMBEDDING_WEIGHT: f64 = 0.5;
/// Cosines below this are noise between unrelated texts and add neither score nor a reason.
const EMBEDDING_FLOOR: f64 = 0.5;
/// Weight of the first shared `(target_kind, target_ref)`.
const SHARED_BASE: f64 = 0.3;
/// Each further shared target adds this much, up to [`SHARED_EXTRA_MAX`] extra targets.
const SHARED_EXTRA: f64 = 0.05;
const SHARED_EXTRA_MAX: i64 = 2;
/// Weight of the text signal: the bm25 strength scaled so the best FTS match in the pass is 1.0.
const TEXT_WEIGHT: f64 = 0.2;
/// FTS matches weaker than this fraction of the pass's best match add neither score nor a reason.
const TEXT_FLOOR: f64 = 0.2;
/// How many distinct words of the source note go into the OR query.
const MAX_TEXT_TERMS: usize = 8;
/// How many FTS matches are considered at most.
const MAX_TEXT_CANDIDATES: i64 = 50;

/// One note that points at a target, with the link that does it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TargetNote {
    pub note: OwnerNote,
    pub link_id: i64,
    pub link_type: String,
}

#[derive(sqlx::FromRow)]
struct TargetRow {
    #[sqlx(flatten)]
    note: OwnerNote,
    link_id: i64,
    link_type: String,
}

/// The notes linked to `(kind, target_ref)`: active ones first, then archived, each newest first
/// (`created_at` then `id`, descending). The target itself is not looked up, so a target that is gone
/// still lists the notes that were written about it.
pub async fn by_target(
    pool: &SqlitePool,
    kind: &str,
    target_ref: &str,
) -> Result<Vec<TargetNote>, LinkError> {
    if !TARGET_KINDS.contains(&kind) {
        return Err(LinkError::UnknownKind);
    }
    let rows = sqlx::query_as::<_, TargetRow>(
        "SELECT n.id, n.note_text, n.origin, n.state, n.created_at, n.updated_at,
                l.id AS link_id, l.link_type
         FROM owner_note_links l JOIN owner_notes n ON n.id = l.note_id
         WHERE l.target_kind = ? AND l.target_ref = ?
         ORDER BY CASE n.state WHEN 'active' THEN 0 ELSE 1 END,
                  n.created_at DESC, n.id DESC, l.id DESC",
    )
    .bind(kind)
    .bind(target_ref)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| TargetNote {
            note: row.note,
            link_id: row.link_id,
            link_type: row.link_type,
        })
        .collect())
}

/// A note that resembles another, with why.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Related {
    pub note: OwnerNote,
    pub score: f64,
    /// Any of `embedding`, `shared_target`, `text`, in that order.
    pub reasons: Vec<&'static str>,
}

/// What is known about one candidate while the signals are gathered.
#[derive(Default)]
struct Signals {
    embedding: Option<f64>,
    shared: i64,
    text: Option<f64>,
}

impl Signals {
    /// The documented weighted sum:
    /// `0.5 * cosine` (cosine >= 0.5, same model only)
    /// `+ 0.3 + 0.05 * (extra shared targets, at most 2)` (when any target is shared)
    /// `+ 0.2 * text` (text = bm25 strength relative to the best match, in (0, 1]).
    fn score(&self) -> f64 {
        let mut score = 0.0;
        if let Some(cosine) = self.embedding {
            score += EMBEDDING_WEIGHT * cosine;
        }
        if self.shared > 0 {
            score += SHARED_BASE + SHARED_EXTRA * (self.shared - 1).min(SHARED_EXTRA_MAX) as f64;
        }
        if let Some(text) = self.text {
            score += TEXT_WEIGHT * text;
        }
        score
    }

    fn reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if self.embedding.is_some() {
            reasons.push("embedding");
        }
        if self.shared > 0 {
            reasons.push("shared_target");
        }
        if self.text.is_some() {
            reasons.push("text");
        }
        reasons
    }
}

/// The active notes most like note `id`, best first, at most `limit` of them (clamping `limit` to the
/// public maximum is the handler's job). The note itself and archived notes never appear. Ties break
/// on the lower id so the answer is the same on every call.
pub async fn related(pool: &SqlitePool, id: i64, limit: usize) -> Result<Vec<Related>, NoteError> {
    let source = sqlx::query_as::<_, OwnerNote>(sqlx::AssertSqlSafe(format!(
        "SELECT {NOTE_COLUMNS} FROM owner_notes WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(NoteError::NotFound)?;

    let mut signals: HashMap<i64, Signals> = HashMap::new();
    gather_embeddings(pool, id, &mut signals).await?;
    gather_shared_targets(pool, id, &mut signals).await?;
    gather_text(pool, id, &source.note_text, &mut signals).await?;

    let mut ranked: Vec<(i64, f64, Vec<&'static str>)> = signals
        .into_iter()
        .map(|(note_id, signal)| (note_id, signal.score(), signal.reasons()))
        .filter(|(_, _, reasons)| !reasons.is_empty())
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(limit);

    let mut out = Vec::with_capacity(ranked.len());
    for (note_id, score, reasons) in ranked {
        let note = sqlx::query_as::<_, OwnerNote>(sqlx::AssertSqlSafe(format!(
            "SELECT {NOTE_COLUMNS} FROM owner_notes WHERE id = ?"
        )))
        .bind(note_id)
        .fetch_one(pool)
        .await?;
        out.push(Related {
            note,
            score,
            reasons,
        });
    }
    Ok(out)
}

/// Cosine against every other active note embedded by the same model as the source. A source with no
/// vector, a vector that does not decode, or a model nobody else used contributes nothing.
async fn gather_embeddings(
    pool: &SqlitePool,
    id: i64,
    signals: &mut HashMap<i64, Signals>,
) -> Result<(), NoteError> {
    let source: Option<(String, i64, Vec<u8>)> =
        sqlx::query_as("SELECT model, dim, vector FROM owner_note_embeddings WHERE note_id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let Some((model, dim, blob)) = source else {
        return Ok(());
    };
    let Some(source_vector) = crate::embed::decode(&blob, dim) else {
        return Ok(());
    };
    let others: Vec<(i64, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT e.note_id, e.dim, e.vector
         FROM owner_note_embeddings e JOIN owner_notes n ON n.id = e.note_id
         WHERE e.model = ? AND e.note_id != ? AND n.state = 'active'",
    )
    .bind(&model)
    .bind(id)
    .fetch_all(pool)
    .await?;
    for (note_id, other_dim, other_blob) in others {
        let Some(vector) = crate::embed::decode(&other_blob, other_dim) else {
            continue;
        };
        let Some(cosine) = crate::embed::cosine(&source_vector, &vector) else {
            continue;
        };
        let cosine = f64::from(cosine);
        if cosine >= EMBEDDING_FLOOR {
            signals.entry(note_id).or_default().embedding = Some(cosine.min(1.0));
        }
    }
    Ok(())
}

/// How many distinct `(target_kind, target_ref)` each other active note shares with the source.
async fn gather_shared_targets(
    pool: &SqlitePool,
    id: i64,
    signals: &mut HashMap<i64, Signals>,
) -> Result<(), NoteError> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT o.note_id, COUNT(DISTINCT o.target_kind || ':' || o.target_ref)
         FROM (SELECT DISTINCT target_kind, target_ref FROM owner_note_links WHERE note_id = ?) s
         JOIN owner_note_links o
           ON o.target_kind = s.target_kind AND o.target_ref = s.target_ref
         JOIN owner_notes n ON n.id = o.note_id
         WHERE o.note_id != ? AND n.state = 'active'
         GROUP BY o.note_id",
    )
    .bind(id)
    .bind(id)
    .fetch_all(pool)
    .await?;
    for (note_id, shared) in rows {
        signals.entry(note_id).or_default().shared = shared;
    }
    Ok(())
}

/// The source's distinct words (the longest first, so stopwords lose the cap), already quoted by
/// `search::fts_query`, joined into an OR expression. `None` when the note has no searchable word.
fn text_expression(text: &str) -> Option<String> {
    let quoted = crate::search::fts_query(text);
    let mut seen: Vec<String> = Vec::new();
    let mut terms: Vec<(usize, &str)> = Vec::new();
    for term in quoted.split(' ') {
        let inner = term.trim_matches('"');
        if !inner.chars().any(char::is_alphanumeric) {
            continue;
        }
        let folded = inner.to_lowercase();
        if seen.contains(&folded) {
            continue;
        }
        seen.push(folded);
        terms.push((inner.chars().count(), term));
    }
    // Longest first; stable, so equal lengths keep their order in the note.
    terms.sort_by_key(|term| std::cmp::Reverse(term.0));
    terms.truncate(MAX_TEXT_TERMS);
    (!terms.is_empty()).then(|| {
        terms
            .into_iter()
            .map(|(_, term)| term)
            .collect::<Vec<_>>()
            .join(" OR ")
    })
}

/// FTS5 matches on any of the source's words, ranked by bm25, active notes other than the source.
async fn gather_text(
    pool: &SqlitePool,
    id: i64,
    text: &str,
    signals: &mut HashMap<i64, Signals>,
) -> Result<(), NoteError> {
    let Some(expression) = text_expression(text) else {
        return Ok(());
    };
    let rows: Vec<(i64, f64)> = sqlx::query_as(
        "SELECT n.id, bm25(owner_notes_fts)
         FROM owner_notes n JOIN owner_notes_fts ON owner_notes_fts.rowid = n.id
         WHERE owner_notes_fts MATCH ? AND n.id != ? AND n.state = 'active'
         ORDER BY bm25(owner_notes_fts), n.id
         LIMIT ?",
    )
    .bind(expression)
    .bind(id)
    .bind(MAX_TEXT_CANDIDATES)
    .fetch_all(pool)
    .await?;
    // bm25 is negative, more negative being better: flip it, then scale so the best match is 1.0.
    let best = rows
        .iter()
        .map(|(_, rank)| (-rank).max(0.0))
        .fold(0.0_f64, f64::max);
    for (note_id, rank) in rows {
        if let Some(strength) = text_strength(rank, best) {
            signals.entry(note_id).or_default().text = Some(strength);
        }
    }
    Ok(())
}

/// The text signal of one FTS match: its bm25 `rank` relative to the pass's `best` (the largest
/// flipped bm25). `None` below [`TEXT_FLOOR`], so a single stray shared word does not earn the
/// `text` reason next to a far better match. The best match itself always passes.
fn text_strength(rank: f64, best: f64) -> Option<f64> {
    let strength = if best > 0.0 {
        ((-rank).max(0.0) / best).clamp(0.0, 1.0)
    } else {
        1.0
    };
    (strength >= TEXT_FLOOR).then_some(strength)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn note(pool: &SqlitePool, text: &str) -> i64 {
        super::super::create(pool, text, "shell").await.unwrap()
    }

    async fn link(pool: &SqlitePool, note_id: i64, kind: &str, target_ref: &str) {
        sqlx::query(
            "INSERT INTO owner_note_links (note_id, link_type, target_kind, target_ref, created_at)
             VALUES (?, 'relates', ?, ?, 'now')",
        )
        .bind(note_id)
        .bind(kind)
        .bind(target_ref)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn archive(pool: &SqlitePool, note_id: i64) {
        sqlx::query("UPDATE owner_notes SET state = 'archived' WHERE id = ?")
            .bind(note_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn embed(pool: &SqlitePool, note_id: i64, model: &str, vector: &[f32]) {
        sqlx::query(
            "INSERT INTO owner_note_embeddings
                 (note_id, model, dim, vector, text_hash, created_at)
             VALUES (?, ?, ?, ?, 'h', 'now')",
        )
        .bind(note_id)
        .bind(model)
        .bind(vector.len() as i64)
        .bind(crate::embed::encode(vector))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_shared_target_relates_two_notes_without_any_text_in_common() {
        let pool = crate::testdb::fresh_pool().await;
        let a = note(&pool, "alpha").await;
        let b = note(&pool, "zulu").await;
        let c = note(&pool, "yankee").await;
        link(&pool, a, "contact", "7").await;
        link(&pool, b, "contact", "7").await;
        link(&pool, c, "contact", "8").await;

        let found = related(&pool, a, 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].note.id, b);
        assert_eq!(found[0].reasons, vec!["shared_target"]);
        assert!((found[0].score - SHARED_BASE).abs() < 1e-9);
    }

    #[tokio::test]
    async fn text_alone_relates_notes_through_the_fts_index() {
        let pool = crate::testdb::fresh_pool().await;
        let a = note(&pool, "quarterly budget review with finance").await;
        let b = note(&pool, "finance wants the budget by Friday").await;
        let _c = note(&pool, "buy oat milk").await;

        let found = related(&pool, a, 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].note.id, b);
        assert_eq!(found[0].reasons, vec!["text"]);
        assert!(found[0].score > 0.0 && found[0].score <= TEXT_WEIGHT);
    }

    #[tokio::test]
    async fn embeddings_count_only_between_vectors_of_the_same_model() {
        let pool = crate::testdb::fresh_pool().await;
        let a = note(&pool, "one").await;
        let same = note(&pool, "two").await;
        let other = note(&pool, "three").await;
        embed(&pool, a, "model-a", &[1.0, 0.0]).await;
        embed(&pool, same, "model-a", &[0.9, 0.1]).await;
        embed(&pool, other, "model-b", &[1.0, 0.0]).await;

        let found = related(&pool, a, 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].note.id, same);
        assert_eq!(found[0].reasons, vec!["embedding"]);
        assert!(found[0].score > 0.4);

        // From the other model's side the picture is the mirror: nobody shares its model.
        assert!(related(&pool, other, 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_note_itself_and_archived_notes_are_left_out() {
        let pool = crate::testdb::fresh_pool().await;
        let a = note(&pool, "kitchen renovation plan").await;
        let live = note(&pool, "kitchen renovation quote").await;
        let gone = note(&pool, "kitchen renovation invoice").await;
        for id in [a, live, gone] {
            link(&pool, id, "project", "home").await;
        }
        embed(&pool, a, "m", &[1.0, 0.0]).await;
        embed(&pool, live, "m", &[1.0, 0.0]).await;
        embed(&pool, gone, "m", &[1.0, 0.0]).await;
        archive(&pool, gone).await;

        let found = related(&pool, a, 10).await.unwrap();
        let ids: Vec<i64> = found.iter().map(|r| r.note.id).collect();
        assert_eq!(ids, vec![live]);
        assert_eq!(found[0].reasons, vec!["embedding", "shared_target", "text"]);

        // An archived source still answers, and still never returns itself.
        let from_archived = related(&pool, gone, 10).await.unwrap();
        assert!(from_archived.iter().all(|r| r.note.id != gone));
    }

    #[tokio::test]
    async fn the_limit_truncates_after_ranking_and_ties_break_on_id() {
        let pool = crate::testdb::fresh_pool().await;
        let a = note(&pool, "source").await;
        let weak = note(&pool, "weak").await;
        let strong = note(&pool, "strong").await;
        let tie_low = note(&pool, "tie low").await;
        let tie_high = note(&pool, "tie high").await;
        link(&pool, a, "mail", "1").await;
        link(&pool, a, "mail", "2").await;
        // `strong` shares two targets, the others one each.
        link(&pool, strong, "mail", "1").await;
        link(&pool, strong, "mail", "2").await;
        link(&pool, weak, "mail", "1").await;
        link(&pool, tie_low, "mail", "2").await;
        link(&pool, tie_high, "mail", "2").await;

        let all = related(&pool, a, 10).await.unwrap();
        let ids: Vec<i64> = all.iter().map(|r| r.note.id).collect();
        assert_eq!(ids, vec![strong, weak, tie_low, tie_high]);

        let two = related(&pool, a, 2).await.unwrap();
        assert_eq!(
            two.iter().map(|r| r.note.id).collect::<Vec<_>>(),
            vec![strong, weak]
        );
        assert!(related(&pool, a, 0).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_note_is_not_found() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(matches!(
            related(&pool, 999, 5).await,
            Err(NoteError::NotFound)
        ));
    }

    #[tokio::test]
    async fn by_target_lists_active_notes_first_then_archived_newest_first() {
        let pool = crate::testdb::fresh_pool().await;
        let old_active = note(&pool, "old active").await;
        let new_active = note(&pool, "new active").await;
        let archived = note(&pool, "archived").await;
        let elsewhere = note(&pool, "elsewhere").await;
        for (id, created) in [
            (old_active, "2026-01-01T00:00:00Z"),
            (new_active, "2026-03-01T00:00:00Z"),
            (archived, "2026-06-01T00:00:00Z"),
            (elsewhere, "2026-07-01T00:00:00Z"),
        ] {
            sqlx::query("UPDATE owner_notes SET created_at = ? WHERE id = ?")
                .bind(created)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        archive(&pool, archived).await;
        link(&pool, old_active, "job", "42").await;
        link(&pool, new_active, "job", "42").await;
        link(&pool, archived, "job", "42").await;
        link(&pool, elsewhere, "job", "43").await;

        // The job 42 is not a row anywhere: a gone target still lists its notes.
        let listed = by_target(&pool, "job", "42").await.unwrap();
        let ids: Vec<i64> = listed.iter().map(|t| t.note.id).collect();
        assert_eq!(ids, vec![new_active, old_active, archived]);
        assert!(listed.iter().all(|t| t.link_type == "relates"));
        assert!(listed.iter().all(|t| t.link_id > 0));

        assert!(by_target(&pool, "job", "999").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn by_target_refuses_an_unknown_kind() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(matches!(
            by_target(&pool, "planet", "1").await,
            Err(LinkError::UnknownKind)
        ));
    }

    #[test]
    fn a_text_match_far_weaker_than_the_best_earns_no_reason() {
        // bm25 ranks are negative; -10 is the pass's best.
        assert_eq!(text_strength(-10.0, 10.0), Some(1.0));
        assert_eq!(text_strength(-5.0, 10.0), Some(0.5));
        assert_eq!(text_strength(-2.0, 10.0), Some(0.2));
        assert_eq!(text_strength(-1.0, 10.0), None);
        // No usable bm25 anywhere in the pass: every match counts in full.
        assert_eq!(text_strength(0.0, 0.0), Some(1.0));
    }

    #[test]
    fn the_text_expression_caps_dedupes_and_quotes_its_terms() {
        assert_eq!(text_expression("   "), None);
        assert_eq!(text_expression("- ... !!"), None);

        let expression = text_expression(
            "alpha Alpha \"beta\" gamma delta epsilon zeta theta iota kappa lambda a",
        )
        .unwrap();
        let terms: Vec<&str> = expression.split(" OR ").collect();
        assert_eq!(terms.len(), MAX_TEXT_TERMS);
        assert!(terms.iter().all(|t| t.starts_with('"') && t.ends_with('"')));
        // Longest words win the cap, the one-letter word does not make it.
        assert!(!terms.contains(&"\"a\""));
        let alphas = terms
            .iter()
            .filter(|t| t.eq_ignore_ascii_case("\"alpha\""))
            .count();
        assert_eq!(alphas, 1);
    }
}
