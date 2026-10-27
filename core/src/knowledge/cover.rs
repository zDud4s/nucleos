//! A general lesson covers the undecided rows it was made from (spec 5.5): approving it archives them, reverting it
//! gives them back. Nothing here decides for the owner: a covered row's question expires, it is never rejected.
use sqlx::{Sqlite, SqlitePool, Transaction};

/// The note of the event that archives a covered row; `uncover_in` finds its rows by it.
pub const NOTE_COVERED_BY: &str = "covered_by:";
/// The note of the event that gives a covered row back.
pub const NOTE_UNCOVERED: &str = "uncovered";

/// The `id` of every `{"t":"knowledge"}` entry of an evidence array; malformed evidence is none.
fn knowledge_ids(evidence: Option<&str>) -> Vec<i64> {
    let parsed = evidence
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(raw).ok())
        .unwrap_or_default();
    parsed
        .iter()
        .filter(|entry| entry.get("t").and_then(serde_json::Value::as_str) == Some("knowledge"))
        .filter_map(|entry| entry.get("id").and_then(serde_json::Value::as_i64))
        .collect()
}

/// For every `active` general lesson: archive its `proposed` candidates and `observed` observations, expire their pending
/// proposals (`expired`, not `rejected`: the owner said nothing) and note `covers:[ids]` on the lesson. Returns rows covered.
pub async fn tidy_covered(pool: &SqlitePool) -> sqlx::Result<u64> {
    let lessons: Vec<(i64, Option<String>)> = sqlx::query_as(
        "SELECT id, evidence FROM knowledge
          WHERE distill_cause = 'generalised' AND status = 'active' ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    let mut total = 0;
    for (lesson, evidence) in lessons {
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = pool.begin().await?;
        let mut covered = Vec::<i64>::new();
        for id in knowledge_ids(evidence.as_deref()) {
            let row: Option<(String, Option<i64>)> = sqlx::query_as(
                "SELECT status, proposal_id FROM knowledge
                  WHERE id = ? AND id <> ? AND source = 'distiller'
                    AND ((status = 'proposed' AND scope_kind <> 'observation')
                      OR (status = 'observed' AND scope_kind = 'observation'))",
            )
            .bind(id)
            .bind(lesson)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((from, proposal)) = row else {
                continue;
            };
            sqlx::query("UPDATE knowledge SET status = 'archived', ended_at = ? WHERE id = ?")
                .bind(&now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                 VALUES (?, ?, 'archived', ?, ?)",
            )
            .bind(id)
            .bind(&from)
            .bind(format!("{NOTE_COVERED_BY}{lesson}"))
            .bind(&now)
            .execute(&mut *tx)
            .await?;
            if let Some(proposal) = proposal {
                let expired: Vec<i64> = sqlx::query_scalar(
                    "UPDATE proposals SET status = 'expired', decided_at = ?
                      WHERE id = ? AND status = 'pending' RETURNING id",
                )
                .bind(&now)
                .bind(proposal)
                .fetch_all(&mut *tx)
                .await?;
                crate::proposals::record_expiries(
                    &mut tx,
                    &expired,
                    "covered by an approved general lesson",
                    &now,
                )
                .await?;
            }
            covered.push(id);
        }
        if !covered.is_empty() {
            super::propose::same_status_event_in(
                &mut tx,
                lesson,
                format!("covers:{}", serde_json::json!(covered)),
            )
            .await?;
        }
        tx.commit().await?;
        total += covered.len() as u64;
    }
    Ok(total)
}

/// Give back what `lesson_id` covered: rows still `archived` whose LAST event is that cover (so a row decided meanwhile is
/// left alone). A candidate returns to `proposed` WITH a fresh pending proposal (an expired one cannot be approved); an
/// observation returns to `observed`. Returns how many questions were reopened.
pub async fn uncover_in(tx: &mut Transaction<'_, Sqlite>, lesson_id: i64) -> sqlx::Result<usize> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT k.id, k.scope_kind FROM knowledge k
          WHERE k.status = 'archived'
            AND (SELECT e.note FROM knowledge_events e
                  WHERE e.knowledge_id = k.id ORDER BY e.id DESC LIMIT 1) = ?
          ORDER BY k.id",
    )
    .bind(format!("{NOTE_COVERED_BY}{lesson_id}"))
    .fetch_all(&mut **tx)
    .await?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut reopened = 0;
    for (id, scope_kind) in rows {
        let back = if scope_kind == "observation" {
            "observed"
        } else {
            "proposed"
        };
        sqlx::query("UPDATE knowledge SET status = ?, ended_at = NULL WHERE id = ?")
            .bind(back)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        sqlx::query(
            "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
             VALUES (?, 'archived', ?, ?, ?)",
        )
        .bind(id)
        .bind(back)
        .bind(NOTE_UNCOVERED)
        .bind(&now)
        .execute(&mut **tx)
        .await?;
        if back == "proposed" {
            super::propose::reopen_proposal_in(tx, id).await?;
            reopened += 1;
        }
    }
    Ok(reopened)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{Declaration, Kind, Layer, Provenance, approve, propose_in_with, revert};

    async fn propose(
        pool: &SqlitePool,
        project: Option<&str>,
        title: &str,
        cause: &str,
        evidence: &str,
    ) -> (i64, i64) {
        let mut tx = pool.begin().await.unwrap();
        let ids = propose_in_with(
            &mut tx,
            Declaration {
                project_id: project,
                origin_run_id: None,
                kind: Kind::Prompt,
                title,
                body: "body",
                reasoning: "seeded",
                supersedes: None,
            },
            &Provenance {
                source: Some("distiller"),
                distill_cause: Some(cause),
                evidence: Some(evidence),
                fingerprint: crate::knowledge::title_fingerprint(title).as_deref(),
                layer: Some(Layer::Procedural),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        ids
    }
    async fn col(pool: &SqlitePool, table: &str, id: i64) -> String {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT status FROM {table} WHERE id = {id}"
        )))
        .fetch_one(pool)
        .await
        .unwrap()
    }
    /// alpha + bravo candidates covered by an approved machine lesson: (a, b, lesson, proposal of a).
    async fn covered_pair(pool: &SqlitePool) -> (i64, i64, i64, i64) {
        let (a, pa) = propose(
            pool,
            Some("alpha"),
            "Use PATH",
            "job_failed",
            r#"[{"t":"job","id":1}]"#,
        )
        .await;
        let (b, _) = propose(
            pool,
            Some("bravo"),
            "Use PATH too",
            "job_failed",
            r#"[{"t":"job","id":2}]"#,
        )
        .await;
        let evidence = format!(r#"[{{"t":"knowledge","id":{a}}},{{"t":"knowledge","id":{b}}}]"#);
        let (lesson, lp) = propose(
            pool,
            None,
            "Put Git usr/bin first",
            "generalised",
            &evidence,
        )
        .await;
        approve(pool, lp).await.unwrap();
        assert_eq!(tidy_covered(pool).await.unwrap(), 2);
        (a, b, lesson, pa)
    }

    #[tokio::test]
    async fn an_active_general_lesson_archives_what_it_covers_and_expires_their_questions() {
        let pool = crate::testdb::fresh_pool().await;
        let (a, b, lesson, pa) = covered_pair(&pool).await;
        for id in [a, b] {
            assert_eq!(col(&pool, "knowledge", id).await, "archived");
        }
        assert_eq!(col(&pool, "proposals", pa).await, "expired"); // not "rejected": the owner said nothing
        let covers: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ? AND note LIKE 'covers:%'",
        )
        .bind(lesson)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(covers, 1);
        assert_eq!(tidy_covered(&pool).await.unwrap(), 0, "nothing left to cover");
    }

    #[tokio::test]
    async fn reverting_the_lesson_restores_the_candidates_and_their_approval_works() {
        let pool = crate::testdb::fresh_pool().await;
        let (a, b, lesson, pa) = covered_pair(&pool).await;
        sqlx::query("UPDATE knowledge SET status = 'rejected' WHERE id = ?")
            .bind(b)
            .execute(&pool)
            .await
            .unwrap(); // decided meanwhile
        assert!(revert(&pool, lesson, "wrong").await.unwrap());

        assert_eq!(col(&pool, "knowledge", a).await, "proposed");
        assert_eq!(
            col(&pool, "knowledge", b).await,
            "rejected",
            "a decided row is not touched"
        );
        let fresh: i64 = sqlx::query_scalar("SELECT proposal_id FROM knowledge WHERE id = ?")
            .bind(a)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(fresh, pa);
        assert_eq!(col(&pool, "proposals", fresh).await, "pending");
        assert_eq!(
            col(&pool, "proposals", pa).await,
            "expired",
            "history stays"
        );
        assert_eq!(approve(&pool, fresh).await.unwrap(), a);
        assert_eq!(col(&pool, "knowledge", a).await, "active");
    }
}
