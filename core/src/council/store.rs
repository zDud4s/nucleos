//! The council's steps: one row per (seat, round, phase) in `council_rounds`, and the council's
//! position, progress and synthesis on `council_runs`. See `0154_council_rounds.sql` for the shape
//! and why it replaced a column triple per phase.

use sqlx::SqlitePool;

use crate::council::{SEAT_CANCELLED, SEAT_ERROR, SEAT_PENDING, STATUS_RUNNING};

/// Round 0: each seat answers the question.
pub const PHASE_ANSWER: &str = "answer";
/// Round 1 and later: each seat reviews and ranks the others' answers.
pub const PHASE_CRITIQUE: &str = "critique";
/// Round 1 and later: each seat revises its own answer in the light of that round's critique.
pub const PHASE_REVISE: &str = "revise";

/// A step whose run finished but whose output could not be read — distinct from `error`, where the
/// run itself failed, because the remedy differs: the model spoke, just not in the shape asked for.
#[allow(dead_code)] // The structured critique parser arrives in a later packet.
pub const STEP_INVALID: &str = "invalid";

/// One step as stored.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct StepRow {
    pub round: i64,
    pub seat_idx: i64,
    pub phase: String,
    pub run_id: Option<i64>,
    pub status: String,
    pub error: Option<String>,
    pub payload: Option<String>,
}

/// Writes one step, replacing every column of the step with the same key if there is one. A step
/// is rewritten as it progresses (pending, then its run, then how it ended), and the last write is
/// the truth — there is nothing in an earlier version worth merging.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_step(
    pool: &SqlitePool,
    id: &str,
    round: i64,
    seat_idx: i64,
    phase: &str,
    run_id: Option<i64>,
    status: &str,
    error: Option<&str>,
    payload: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO council_rounds
           (council_id, round, seat_idx, phase, run_id, status, error, payload)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (council_id, round, seat_idx, phase) DO UPDATE SET
           run_id = excluded.run_id, status = excluded.status, error = excluded.error,
           payload = excluded.payload",
    )
    .bind(id)
    .bind(round)
    .bind(seat_idx)
    .bind(phase)
    .bind(run_id)
    .bind(status)
    .bind(error)
    .bind(payload)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Every step of a council, seat by seat, and within a seat in the order the steps happen. The
/// phase order is spelled out because the alphabet would put `critique` before `answer`.
pub async fn steps_of(pool: &SqlitePool, id: &str) -> sqlx::Result<Vec<StepRow>> {
    sqlx::query_as::<_, StepRow>(
        "SELECT round, seat_idx, phase, run_id, status, error, payload
           FROM council_rounds
          WHERE council_id = ?
          ORDER BY seat_idx, round,
                   CASE phase WHEN 'answer' THEN 0 WHEN 'critique' THEN 1 WHEN 'revise' THEN 2
                              ELSE 3 END",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}

/// Moves the council to a round and phase. Guarded on `running`, for the reason `set_stage` gives:
/// a cancel that landed first is not undone by a boundary crossed a moment later.
pub async fn set_position(
    pool: &SqlitePool,
    id: &str,
    round: i64,
    phase: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE council_runs SET current_round = ?, current_phase = ? WHERE id = ? AND status = ?",
    )
    .bind(round)
    .bind(phase)
    .bind(id)
    .bind(STATUS_RUNNING)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Records how many critique rounds ran and whether the council stopped before it was asked to.
#[allow(dead_code)] // The round driver arrives in a later packet.
pub async fn set_progress(
    pool: &SqlitePool,
    id: &str,
    rounds_run: i64,
    stopped_early: bool,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE council_runs SET rounds_run = ?, stopped_early = ? WHERE id = ?")
        .bind(rounds_run)
        .bind(stopped_early)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Records the chairman's structured synthesis and how producing it ended.
#[allow(dead_code)] // The chairman's structured output arrives in a later packet.
pub async fn set_synthesis(
    pool: &SqlitePool,
    id: &str,
    json: Option<&str>,
    status: &str,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE council_runs SET synthesis_json = ?, synthesis_status = ? WHERE id = ?")
        .bind(json)
        .bind(status)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Marks every step of a council still `pending` as `cancelled`, when the council is cancelled. A
/// step that already ended keeps how it ended.
#[allow(dead_code)] // Cancellation moves onto the steps in a later packet.
pub async fn cancel_pending_steps(pool: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE council_rounds SET status = ? WHERE council_id = ? AND status = ?")
        .bind(SEAT_CANCELLED)
        .bind(id)
        .bind(SEAT_PENDING)
        .execute(pool)
        .await
        .map(|_| ())
}

/// At startup, after `council::reconcile`: a step still `pending` in a council that is no longer
/// `running` will never be finished by anyone, so it is settled as `error` rather than left drawn as
/// in progress for ever. Returns how many steps it settled.
#[allow(dead_code)] // Wired into startup in a later packet.
pub async fn error_orphan_steps(pool: &SqlitePool) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE council_rounds SET status = ?, error = ?
          WHERE status = ?
            AND council_id IN (SELECT id FROM council_runs WHERE status <> ?)",
    )
    .bind(SEAT_ERROR)
    .bind("the council ended before this step finished")
    .bind(SEAT_PENDING)
    .bind(STATUS_RUNNING)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::{CouncilSeat, SeatKind};
    use crate::council::{
        Ranking, SEAT_OK, SEAT_PENDING, STAGE_RANKING, STAGE_REVISION, insert_council,
        set_revision, set_stage, set_stage1, set_stage2,
    };

    /// A database as it stood the day before 0154: the three migrations that shaped the council
    /// tables, applied raw. `migrate!()` would also work and would prove nothing — it runs 0154
    /// against EMPTY tables, and the data copy is the part of 0154 that can be wrong. Same reasoning
    /// as `email::tests::the_rebuild_keeps_the_mail_and_its_attachments`.
    async fn legacy_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            include_str!("../../migrations/0065_council.sql"),
            include_str!("../../migrations/0085_council_agents.sql"),
            include_str!("../../migrations/0136_council_revision.sql"),
        ] {
            sqlx::raw_sql(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn apply_0154(pool: &sqlx::SqlitePool) {
        sqlx::raw_sql(include_str!("../../migrations/0154_council_rounds.sql"))
            .execute(pool)
            .await
            .unwrap();
    }

    /// An old-shape council row, with only the columns the pre-0154 schema requires.
    async fn old_council(pool: &sqlx::SqlitePool, id: &str, status: &str, stage: i64, rounds: i64) {
        sqlx::query(
            "INSERT INTO council_runs
               (id, created_at, question, status, stage, anon_seed, chairman_kind, chairman_ref,
                rounds)
             VALUES (?, '2026-09-01T00:00:00+00:00', 'why?', ?, ?, ?, 'cloud', 'claude-opus-4-8', ?)",
        )
        .bind(id)
        .bind(status)
        .bind(stage)
        .bind(id)
        .bind(rounds)
        .execute(pool)
        .await
        .unwrap();
    }

    /// An old-shape seat. Every phase is passed as `(run_id, status, error)`; the revision columns
    /// are written explicitly so a test can leave them at the one-round `pending` the old schema
    /// defaulted them to.
    #[allow(clippy::too_many_arguments)]
    async fn old_seat(
        pool: &sqlx::SqlitePool,
        id: &str,
        seat_idx: i64,
        stage1: (Option<i64>, &str, Option<&str>),
        stage2: (Option<i64>, &str, Option<&str>),
        rankings: Option<&str>,
        revision: (Option<i64>, &str, Option<&str>),
    ) {
        sqlx::query(
            "INSERT INTO council_seats
               (council_id, seat_idx, kind, model_ref,
                stage1_run_id, stage1_status, stage1_error,
                stage2_run_id, stage2_status, stage2_error, rankings,
                revision_run_id, revision_status, revision_error)
             VALUES (?, ?, 'cloud', 'claude-opus-4-8', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(seat_idx)
        .bind(stage1.0)
        .bind(stage1.1)
        .bind(stage1.2)
        .bind(stage2.0)
        .bind(stage2.1)
        .bind(stage2.2)
        .bind(rankings)
        .bind(revision.0)
        .bind(revision.1)
        .bind(revision.2)
        .execute(pool)
        .await
        .unwrap();
    }

    /// One step as a comparable tuple, payload left out — the payload is asserted on its own,
    /// parsed, because its byte layout is SQLite's `json_object` and not a contract.
    fn shape(step: &StepRow) -> (i64, i64, String, Option<i64>, String, Option<String>) {
        (
            step.seat_idx,
            step.round,
            step.phase.clone(),
            step.run_id,
            step.status.clone(),
            step.error.clone(),
        )
    }

    fn payload_of(step: &StepRow) -> Option<serde_json::Value> {
        step.payload
            .as_deref()
            .map(|text| serde_json::from_str(text).expect("a step payload is JSON"))
    }

    async fn position(pool: &sqlx::SqlitePool, id: &str) -> (i64, String, i64) {
        sqlx::query_as(
            "SELECT current_round, current_phase, rounds_run FROM council_runs WHERE id = ?",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn store_migration_moves_an_old_council_into_rounds() {
        let pool = legacy_pool().await;
        old_council(&pool, "c1", "done", 4, 2).await;
        old_seat(
            &pool,
            "c1",
            0,
            (Some(10), "ok", None),
            (Some(20), "ok", None),
            Some(r#"[{"anon":"A","rank":1}]"#),
            (Some(30), "ok", None),
        )
        .await;
        old_seat(
            &pool,
            "c1",
            1,
            (Some(11), "timeout", Some("slow")),
            (None, "skipped", None),
            None,
            (None, "skipped", None),
        )
        .await;

        apply_0154(&pool).await;

        let steps = steps_of(&pool, "c1").await.unwrap();
        let shapes: Vec<_> = steps.iter().map(shape).collect();
        assert_eq!(
            shapes,
            vec![
                (
                    0,
                    0,
                    PHASE_ANSWER.to_string(),
                    Some(10),
                    "ok".to_string(),
                    None
                ),
                (
                    0,
                    1,
                    PHASE_CRITIQUE.to_string(),
                    Some(20),
                    "ok".to_string(),
                    None
                ),
                (
                    0,
                    1,
                    PHASE_REVISE.to_string(),
                    Some(30),
                    "ok".to_string(),
                    None
                ),
                (
                    1,
                    0,
                    PHASE_ANSWER.to_string(),
                    Some(11),
                    "timeout".to_string(),
                    Some("slow".to_string())
                ),
                (
                    1,
                    1,
                    PHASE_CRITIQUE.to_string(),
                    None,
                    "skipped".to_string(),
                    None
                ),
                (
                    1,
                    1,
                    PHASE_REVISE.to_string(),
                    None,
                    "skipped".to_string(),
                    None
                ),
            ],
            "every phase that happened becomes one step, in seat/round/phase order"
        );
        // An answer and a revision carry their prose in the run's transcript, never in the row.
        assert_eq!(steps[0].payload, None);
        assert_eq!(steps[2].payload, None);

        // A settled council reads as done at the round it reached.
        assert_eq!(position(&pool, "c1").await, (1, "done".to_string(), 1));

        // The new columns arrive at their neutral values on an old row.
        let (stopped_early, synthesis_json, synthesis_status): (
            i64,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT stopped_early, synthesis_json, synthesis_status FROM council_runs
                 WHERE id = 'c1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            (stopped_early, synthesis_json, synthesis_status),
            (0, None, None)
        );
        let roles: Vec<(Option<String>,)> =
            sqlx::query_as("SELECT role FROM council_seats WHERE council_id = 'c1'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(roles, vec![(None,), (None,)]);

        // The steps follow their council out, as the seats do.
        sqlx::query("DELETE FROM council_runs WHERE id = 'c1'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(steps_of(&pool, "c1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn store_migration_converts_rankings_to_an_ordered_ballot() {
        let pool = legacy_pool().await;
        old_council(&pool, "c1", "done", 3, 1).await;
        // Out of order on purpose, with a tie: rank first, then the position in the stored array.
        old_seat(
            &pool,
            "c1",
            0,
            (Some(10), "ok", None),
            (Some(20), "ok", None),
            Some(r#"[{"anon":"C","rank":2},{"anon":"A","rank":1},{"anon":"B","rank":2}]"#),
            (None, "pending", None),
        )
        .await;
        // A blank vote stays a blank ballot, not a missing one.
        old_seat(
            &pool,
            "c1",
            1,
            (Some(11), "ok", None),
            (Some(21), "ok", None),
            Some("[]"),
            (None, "pending", None),
        )
        .await;
        // No rankings at all — the seat failed phase 2 — is no payload.
        old_seat(
            &pool,
            "c1",
            2,
            (Some(12), "ok", None),
            (Some(22), "error", Some("refused")),
            None,
            (None, "pending", None),
        )
        .await;

        apply_0154(&pool).await;

        let steps = steps_of(&pool, "c1").await.unwrap();
        let critique = |seat: i64| {
            steps
                .iter()
                .find(|s| s.seat_idx == seat && s.phase == PHASE_CRITIQUE)
                .unwrap_or_else(|| panic!("seat {seat} has a critique step"))
        };
        assert_eq!(critique(0).round, 1);
        assert_eq!(
            payload_of(critique(0)),
            Some(serde_json::json!({ "reviews": [], "ranking": ["A", "C", "B"] }))
        );
        assert_eq!(
            payload_of(critique(1)),
            Some(serde_json::json!({ "reviews": [], "ranking": [] }))
        );
        assert_eq!(payload_of(critique(2)), None);
        assert_eq!(critique(2).error.as_deref(), Some("refused"));
    }

    #[tokio::test]
    async fn store_migration_copies_no_pending_default_phase() {
        let pool = legacy_pool().await;
        // A one-round council: its revision columns are the untouched `pending` default, which
        // records a phase that was never part of it.
        old_council(&pool, "one", "done", 3, 1).await;
        old_seat(
            &pool,
            "one",
            0,
            (Some(10), "ok", None),
            (Some(20), "ok", None),
            Some("[]"),
            (None, "pending", None),
        )
        .await;
        // A council still in phase 1: seat 0 has not started, seat 1 has a run in flight.
        old_council(&pool, "live", "running", 1, 1).await;
        old_seat(
            &pool,
            "live",
            0,
            (None, "pending", None),
            (None, "pending", None),
            None,
            (None, "pending", None),
        )
        .await;
        old_seat(
            &pool,
            "live",
            1,
            (Some(40), "pending", None),
            (None, "pending", None),
            None,
            (None, "pending", None),
        )
        .await;

        apply_0154(&pool).await;

        let one: Vec<String> = steps_of(&pool, "one")
            .await
            .unwrap()
            .iter()
            .map(|s| s.phase.clone())
            .collect();
        assert_eq!(
            one,
            vec![PHASE_ANSWER.to_string(), PHASE_CRITIQUE.to_string()],
            "no revise step for a council that never had a revision"
        );

        let live: Vec<_> = steps_of(&pool, "live")
            .await
            .unwrap()
            .iter()
            .map(shape)
            .collect();
        assert_eq!(
            live,
            vec![(
                1,
                0,
                PHASE_ANSWER.to_string(),
                Some(40),
                "pending".to_string(),
                None
            )],
            "a pending phase with a run attached is in flight and is copied; one with none is not"
        );
    }

    #[tokio::test]
    async fn store_migration_maps_stage_to_round_and_phase() {
        let pool = legacy_pool().await;
        old_council(&pool, "s1", "running", 1, 1).await;
        old_council(&pool, "s2", "running", 2, 1).await;
        old_council(&pool, "s3-revise", "running", 3, 2).await;
        old_council(&pool, "s3-chair", "running", 3, 1).await;
        old_council(&pool, "s4-chair", "running", 4, 2).await;
        // Settled with every critique skipped: no round of critique ever ran.
        old_council(&pool, "skipped", "done", 3, 1).await;
        old_seat(
            &pool,
            "skipped",
            0,
            (Some(10), "ok", None),
            (None, "skipped", None),
            None,
            (None, "pending", None),
        )
        .await;
        // Cancelled during phase 2 after one critique landed.
        old_council(&pool, "cancelled", "cancelled", 2, 1).await;
        old_seat(
            &pool,
            "cancelled",
            0,
            (Some(11), "ok", None),
            (Some(21), "ok", None),
            Some("[]"),
            (None, "pending", None),
        )
        .await;

        apply_0154(&pool).await;

        let at = |round: i64, phase: &str| (round, phase.to_string());
        let mut seen = Vec::new();
        for id in [
            "s1",
            "s2",
            "s3-revise",
            "s3-chair",
            "s4-chair",
            "skipped",
            "cancelled",
        ] {
            let (round, phase, _) = position(&pool, id).await;
            seen.push((id, (round, phase)));
        }
        assert_eq!(
            seen,
            vec![
                ("s1", at(0, "answer")),
                ("s2", at(1, "critique")),
                ("s3-revise", at(1, "revise")),
                ("s3-chair", at(1, "chairman")),
                ("s4-chair", at(1, "chairman")),
                ("skipped", at(0, "done")),
                ("cancelled", at(1, "done")),
            ]
        );
        assert_eq!(
            position(&pool, "skipped").await.2,
            0,
            "a skipped critique is no round run"
        );
        assert_eq!(position(&pool, "cancelled").await.2, 1);
    }

    async fn migrated_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn convened(pool: &sqlx::SqlitePool, id: &str, rounds: i64) {
        let chair = CouncilSeat::of_model(SeatKind::Cloud, "claude-opus-4-8");
        insert_council(
            pool,
            id,
            "why?",
            &chair,
            &[
                CouncilSeat::of_model(SeatKind::Cloud, "claude-opus-4-8"),
                CouncilSeat::of_model(SeatKind::Local, "qwen3.5:4b"),
            ],
            rounds,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn store_upsert_step_overwrites_one_step() {
        let pool = migrated_pool().await;
        convened(&pool, "c1", 2).await;

        // Written out of order, so the read order is the store's and not the insertion's.
        upsert_step(&pool, "c1", 1, 0, PHASE_REVISE, None, "pending", None, None)
            .await
            .unwrap();
        upsert_step(
            &pool,
            "c1",
            1,
            0,
            PHASE_CRITIQUE,
            None,
            "pending",
            None,
            None,
        )
        .await
        .unwrap();
        upsert_step(&pool, "c1", 0, 1, PHASE_ANSWER, Some(7), "ok", None, None)
            .await
            .unwrap();
        upsert_step(&pool, "c1", 0, 0, PHASE_ANSWER, None, "pending", None, None)
            .await
            .unwrap();

        // The same key again: one row, every column replaced.
        upsert_step(
            &pool,
            "c1",
            0,
            0,
            PHASE_ANSWER,
            Some(5),
            STEP_INVALID,
            Some("unreadable"),
            Some(r#"{"x":1}"#),
        )
        .await
        .unwrap();

        let steps = steps_of(&pool, "c1").await.unwrap();
        let shapes: Vec<_> = steps.iter().map(shape).collect();
        assert_eq!(
            shapes,
            vec![
                (
                    0,
                    0,
                    PHASE_ANSWER.to_string(),
                    Some(5),
                    STEP_INVALID.to_string(),
                    Some("unreadable".to_string())
                ),
                (
                    0,
                    1,
                    PHASE_CRITIQUE.to_string(),
                    None,
                    "pending".to_string(),
                    None
                ),
                (
                    0,
                    1,
                    PHASE_REVISE.to_string(),
                    None,
                    "pending".to_string(),
                    None
                ),
                (
                    1,
                    0,
                    PHASE_ANSWER.to_string(),
                    Some(7),
                    "ok".to_string(),
                    None
                ),
            ]
        );
        assert_eq!(payload_of(&steps[0]), Some(serde_json::json!({ "x": 1 })));
        assert_eq!(STEP_INVALID, "invalid");
    }

    #[tokio::test]
    async fn store_legacy_setters_also_write_steps() {
        let pool = migrated_pool().await;
        convened(&pool, "c1", 2).await;

        set_stage1(&pool, "c1", 0, Some(1), SEAT_OK, None)
            .await
            .unwrap();
        set_stage1(&pool, "c1", 1, None, SEAT_PENDING, None)
            .await
            .unwrap();
        set_stage(&pool, "c1", STAGE_RANKING).await.unwrap();
        assert_eq!(
            (position(&pool, "c1").await.0, position(&pool, "c1").await.1),
            (1, "critique".to_string())
        );

        let ballot = [
            Ranking {
                anon: "B".to_string(),
                rank: 2,
            },
            Ranking {
                anon: "A".to_string(),
                rank: 1,
            },
        ];
        set_stage2(&pool, "c1", 0, Some(2), SEAT_OK, None, Some(&ballot))
            .await
            .unwrap();
        set_stage2(&pool, "c1", 1, Some(4), "error", Some("refused"), None)
            .await
            .unwrap();
        set_stage(&pool, "c1", STAGE_REVISION).await.unwrap();
        assert_eq!(
            (position(&pool, "c1").await.0, position(&pool, "c1").await.1),
            (1, "revise".to_string())
        );

        set_revision(&pool, "c1", 0, Some(3), SEAT_OK, None)
            .await
            .unwrap();
        // The last stage of a two-round council is the chairman's.
        set_stage(&pool, "c1", STAGE_REVISION + 1).await.unwrap();
        assert_eq!(
            (position(&pool, "c1").await.0, position(&pool, "c1").await.1),
            (1, "chairman".to_string())
        );

        let steps = steps_of(&pool, "c1").await.unwrap();
        let shapes: Vec<_> = steps.iter().map(shape).collect();
        assert_eq!(
            shapes,
            vec![
                (
                    0,
                    0,
                    PHASE_ANSWER.to_string(),
                    Some(1),
                    "ok".to_string(),
                    None
                ),
                (
                    0,
                    1,
                    PHASE_CRITIQUE.to_string(),
                    Some(2),
                    "ok".to_string(),
                    None
                ),
                (
                    0,
                    1,
                    PHASE_REVISE.to_string(),
                    Some(3),
                    "ok".to_string(),
                    None
                ),
                (
                    1,
                    0,
                    PHASE_ANSWER.to_string(),
                    None,
                    "pending".to_string(),
                    None
                ),
                (
                    1,
                    1,
                    PHASE_CRITIQUE.to_string(),
                    Some(4),
                    "error".to_string(),
                    Some("refused".to_string())
                ),
            ]
        );
        // The legacy `[{anon, rank}]` vote arrives as the new ordered ballot.
        assert_eq!(
            payload_of(&steps[1]),
            Some(serde_json::json!({ "reviews": [], "ranking": ["A", "B"] }))
        );
        assert_eq!(payload_of(&steps[4]), None);
    }
}
