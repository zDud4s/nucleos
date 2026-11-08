//! Offering a ticket to its most competent holder, and declining it (spec 2026-11-06-tickets-globais,
//! "this ticket is yours").
//!
//! An offer is not a status: an offered ticket is an `open` ticket whose `offered_to` names a holder.
//! It is advisory -- anyone may still claim, the first claim wins and clears the offer
//! (`store::take`). `route` picks the best candidate of `competence` that has not declined; `decline`
//! records a refusal (`declined`) or a hand-over (`ceded`, with a suggested holder) and, when the
//! decliner was the offeree, moves the offer on. Every offer writes an `offered` event whose holder is
//! the OFFEREE, which `sessions::take_notices` delivers once. A run has no session of its own, so an
//! offer to a run registers the pseudo-session `run-<id>` with its cursor just before that event.

use chrono::{DateTime, Utc};
use sqlx::{SqliteConnection, SqlitePool};

use super::competence::{self, Source};
use super::store::{begin, encode_holder, insert_event_on, record_event_on, row_by_id};
use super::{Holder, HolderKind, Status, TaskRow, sessions};

/// What routing did to a ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Routed {
    /// The ticket is now offered to this holder.
    Offered(Holder),
    /// Nobody to offer it to (no competent live holder, or the ticket is not `open`): nothing changed.
    NoCandidates,
    /// Every competent holder declined it: nothing changed.
    AllDeclined,
}

fn source_word(source: Source) -> &'static str {
    match source {
        Source::Owned => "owned",
        Source::Declared => "declared",
        Source::Observed => "observed",
    }
}

fn label(holder: &Holder) -> String {
    format!("{}:{}", holder.kind.as_str(), holder.id)
}

async fn load(pool: &SqlitePool, task_id: i64) -> sqlx::Result<TaskRow> {
    let mut conn = pool.acquire().await?;
    row_by_id(&mut conn, task_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Offers an `open` ticket to the best competent holder that has not declined or ceded it. A ticket
/// that is not `open` (claimed, closed, probing) is never offered: `NoCandidates`, nothing changed.
/// A missing ticket is `sqlx::Error::RowNotFound`.
pub async fn route(pool: &SqlitePool, task_id: i64, now: DateTime<Utc>) -> sqlx::Result<Routed> {
    let task = load(pool, task_id).await?;
    if task.status != Status::Open {
        return Ok(Routed::NoCandidates);
    }
    let found = competence::candidates(pool, &task, now).await?;
    if found.is_empty() {
        return Ok(Routed::NoCandidates);
    }
    let declined = competence::declined_set(pool, task_id).await?;
    let Some(best) = competence::rank(found, &declined).into_iter().next() else {
        return Ok(Routed::AllDeclined);
    };
    let note = format!("routed: {} ({})", source_word(best.source), best.doing);
    let mut tx = begin(pool).await?;
    let Some(event_id) = offer_on(&mut tx, task_id, &best.holder, &note, now).await? else {
        // Claimed (or closed) between the read and the lock: a claimed ticket is never offered.
        return Ok(Routed::NoCandidates);
    };
    tx.commit().await?;
    after_offer(pool, &task, &best.holder, event_id, now).await?;
    Ok(Routed::Offered(best.holder))
}

/// [`route`] for a ticket that was just created, fail-open: the ticket stands whether or not it could
/// be offered, so an error is logged and never reaches the caller that opened it.
pub async fn route_or_warn(pool: &SqlitePool, task_id: i64, now: DateTime<Utc>) {
    if let Err(error) = route(pool, task_id, now).await {
        tracing::warn!(%error, task_id, "could not route a new ticket to a competent holder");
    }
}

/// `who` turns the ticket down. Without `suggest` it is a `declined` event (note: `message`); with
/// one it is a `ceded` event (note: `to <kind>:<id>: <message>`); in both the event's holder is the
/// decliner, so `route` never proposes it again.
///
/// Only the current offeree moves the offer: with `suggest` the ticket is offered straight to the
/// suggested holder (`Offered(suggested)`), without it the offer is cleared and the ticket routed
/// again (`route`'s answer). Anyone else's decline is only recorded and the answer is the ticket as it
/// stands: `Offered(offeree)` while it is offered to somebody, else `NoCandidates`.
pub async fn decline(
    pool: &SqlitePool,
    task_id: i64,
    who: &Holder,
    message: &str,
    suggest: Option<Holder>,
    now: DateTime<Utc>,
) -> sqlx::Result<Routed> {
    let mut tx = begin(pool).await?;
    let task = row_by_id(&mut tx, task_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    match &suggest {
        Some(to) => {
            let note = format!("to {}: {message}", label(to));
            record_event_on(&mut tx, task_id, "ceded", Some(who), &note, now).await?;
        }
        None => record_event_on(&mut tx, task_id, "declined", Some(who), message, now).await?,
    }
    let offeree = task
        .offered_to
        .clone()
        .filter(|_| task.status == Status::Open);
    let was_offeree = offeree.as_ref().is_some_and(|to| to.same_owner(who));
    if !was_offeree {
        tx.commit().await?;
        return Ok(offeree.map_or(Routed::NoCandidates, Routed::Offered));
    }
    match suggest {
        Some(to) => {
            let note = format!("ceded by {}: {message}", label(who));
            let event_id = offer_on(&mut tx, task_id, &to, &note, now).await?;
            tx.commit().await?;
            match event_id {
                Some(event_id) => {
                    after_offer(pool, &task, &to, event_id, now).await?;
                    Ok(Routed::Offered(to))
                }
                None => Ok(Routed::NoCandidates),
            }
        }
        None => {
            sqlx::query(
                "UPDATE task_claims SET offered_to = NULL, offered_at = NULL
                 WHERE id = ? AND status = 'open'",
            )
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            route(pool, task_id, now).await
        }
    }
}

/// Sets the offer and writes its `offered` event (holder = the offeree) inside the caller's
/// transaction. `None` when the ticket is no longer `open`: nothing is written.
async fn offer_on(
    conn: &mut SqliteConnection,
    task_id: i64,
    to: &Holder,
    note: &str,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<i64>> {
    let moved = sqlx::query(
        "UPDATE task_claims SET offered_to = ?, offered_at = ? WHERE id = ? AND status = 'open'",
    )
    .bind(encode_holder(to))
    .bind(crate::execution::wave::stamp(now))
    .bind(task_id)
    .execute(&mut *conn)
    .await?;
    if moved.rows_affected() == 0 {
        return Ok(None);
    }
    insert_event_on(conn, task_id, "offered", Some(to), note, now)
        .await
        .map(Some)
}

/// A run has no hook session: give it the cursor row `run-<id>` that starts just before the offer, so
/// its next `take_notices` delivers it. An already-registered run keeps its older cursor.
async fn after_offer(
    pool: &SqlitePool,
    task: &TaskRow,
    to: &Holder,
    event_id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    if to.kind != HolderKind::Run {
        return Ok(());
    }
    sessions::register_pseudo(
        pool,
        &format!("run-{}", to.id),
        &task.project_id,
        &to.worktree,
        event_id - 1,
        now,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::wave::stamp;
    use crate::task_claims::{ClaimOutcome, TaskKind, store, testing};
    use chrono::{Duration, TimeZone};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 7, 10, 0, 0).unwrap()
    }

    fn session(id: &str) -> Holder {
        Holder {
            kind: HolderKind::Session,
            id: id.to_owned(),
            worktree: format!("/wt/{id}"),
        }
    }

    /// An `open` declared ticket of project `alpha` guarding `core/a.rs`.
    async fn ticket(pool: &SqlitePool) -> i64 {
        let mut new = testing::new_task("file:core/a.rs", TaskKind::Declared);
        new.guard_paths = vec!["core/a.rs".to_owned()];
        store::create_or_adopt_open(pool, "alpha", new, t0())
            .await
            .unwrap()
            .id
    }

    async fn insert_session(pool: &SqlitePool, id: &str, seen: DateTime<Utc>) {
        sqlx::query(
            "INSERT INTO task_sessions (session_id, project_id, worktree, last_seen)
             VALUES (?, 'alpha', ?, ?)",
        )
        .bind(id)
        .bind(format!("/wt/{id}"))
        .bind(stamp(seen))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_run(pool: &SqlitePool) -> i64 {
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', 'running', 't')")
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    /// A `running` work node of project `alpha` owning `core/a.rs`, held by a run or a session.
    async fn owning_node(
        pool: &SqlitePool,
        id: &str,
        run_id: Option<i64>,
        session_id: Option<&str>,
        updated_at: &str,
    ) {
        sqlx::query(
            "INSERT INTO work_nodes (id, project_id, root_id, depth, kind, title, goal, status,
                                     owned_paths, run_id, session_id, created_at, updated_at)
             VALUES (?, 'alpha', ?, 0, 'leaf', ?, 'goal', 'running', '[\"core/a.rs\"]', ?, ?, 't', ?)",
        )
        .bind(id)
        .bind(id)
        .bind(format!("node {id}"))
        .bind(run_id.map(|id| id.to_string()))
        .bind(session_id)
        .bind(updated_at)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A live session `id` owning the ticket's path through a running work node.
    async fn competent_session(pool: &SqlitePool, id: &str, updated_at: &str) {
        insert_session(pool, id, t0() - Duration::seconds(60)).await;
        owning_node(pool, &format!("n-{id}"), None, Some(id), updated_at).await;
    }

    async fn offered_to(pool: &SqlitePool, task_id: i64) -> Option<Holder> {
        let mut conn = pool.acquire().await.unwrap();
        row_by_id(&mut conn, task_id)
            .await
            .unwrap()
            .unwrap()
            .offered_to
    }

    /// `(id, event, holder_kind, holder_id, note)` of the ticket's events after `created`.
    async fn events(
        pool: &SqlitePool,
        task_id: i64,
    ) -> Vec<(i64, String, Option<String>, Option<String>, String)> {
        sqlx::query_as(
            "SELECT id, event, holder_kind, holder_id, note FROM task_claim_events
             WHERE task_id = ? AND event != 'created' ORDER BY id",
        )
        .bind(task_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn route_offers_the_best_candidate_and_records_the_event_for_the_offeree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;
        competent_session(&pool, "s1", "2026-11-07T09:00:00Z").await;
        let run = insert_run(&pool).await;
        owning_node(&pool, "n-run", Some(run), None, "2026-11-07T09:30:00Z").await;

        let routed = route(&pool, id, t0()).await.unwrap();
        let Routed::Offered(to) = &routed else {
            panic!("expected an offer, got {routed:?}")
        };
        assert_eq!(
            (to.kind, to.id.as_str()),
            (HolderKind::Run, run.to_string().as_str()),
            "both own the path; the most recently active wins"
        );
        assert_eq!(offered_to(&pool, id).await.as_ref(), Some(to));
        let mut conn = pool.acquire().await.unwrap();
        let row = row_by_id(&mut conn, id).await.unwrap().unwrap();
        assert_eq!(row.status, Status::Open, "an offer is not a status");
        assert_eq!(row.offered_at, Some(stamp(t0())));
        drop(conn);

        let log = events(&pool, id).await;
        let [(event_id, event, kind, holder_id, note)] = log.as_slice() else {
            panic!("expected one offered event, got {log:?}")
        };
        assert_eq!(event, "offered");
        assert_eq!(kind.as_deref(), Some("run"));
        assert_eq!(holder_id.as_deref(), Some(run.to_string().as_str()));
        assert!(note.starts_with("routed: owned"), "{note}");

        let cursor: i64 =
            sqlx::query_scalar("SELECT notice_cursor FROM task_sessions WHERE session_id = ?")
                .bind(format!("run-{run}"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            cursor,
            event_id - 1,
            "the run's cursor starts just before its offer"
        );
    }

    #[tokio::test]
    async fn route_skips_declined_candidates() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;
        competent_session(&pool, "s1", "2026-11-07T09:30:00Z").await;
        competent_session(&pool, "s2", "2026-11-07T09:00:00Z").await;
        store::record_event(&pool, id, "declined", Some(&session("s1")), "busy", t0())
            .await
            .unwrap();

        let routed = route(&pool, id, t0()).await.unwrap();
        assert!(
            matches!(&routed, Routed::Offered(to) if to.id == "s2"),
            "{routed:?}"
        );
    }

    #[tokio::test]
    async fn route_with_no_candidates_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;

        assert_eq!(route(&pool, id, t0()).await.unwrap(), Routed::NoCandidates);
        assert_eq!(offered_to(&pool, id).await, None);
        assert!(events(&pool, id).await.is_empty());
    }

    #[tokio::test]
    async fn route_when_every_candidate_declined_says_all_declined() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;
        competent_session(&pool, "s1", "2026-11-07T09:00:00Z").await;
        store::record_event(&pool, id, "declined", Some(&session("s1")), "busy", t0())
            .await
            .unwrap();

        assert_eq!(route(&pool, id, t0()).await.unwrap(), Routed::AllDeclined);
        assert_eq!(offered_to(&pool, id).await, None);
        assert_eq!(events(&pool, id).await.len(), 1, "only the decline");
    }

    #[tokio::test]
    async fn decline_with_a_suggestion_offers_to_the_suggested_holder_and_records_ceded() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;
        competent_session(&pool, "s1", "2026-11-07T09:00:00Z").await;
        assert!(matches!(
            route(&pool, id, t0()).await.unwrap(),
            Routed::Offered(_)
        ));

        let routed = decline(
            &pool,
            id,
            &session("s1"),
            "s2 wrote this",
            Some(session("s2")),
            t0(),
        )
        .await
        .unwrap();
        assert_eq!(routed, Routed::Offered(session("s2")));
        assert_eq!(offered_to(&pool, id).await, Some(session("s2")));

        let log = events(&pool, id).await;
        let tail: Vec<(&str, Option<&str>, &str)> = log
            .iter()
            .skip(1)
            .map(|(_, event, _, holder_id, note)| {
                (event.as_str(), holder_id.as_deref(), note.as_str())
            })
            .collect();
        assert_eq!(
            tail,
            vec![
                ("ceded", Some("s1"), "to session:s2: s2 wrote this"),
                ("offered", Some("s2"), "ceded by session:s1: s2 wrote this"),
            ]
        );
    }

    #[tokio::test]
    async fn decline_by_someone_not_offered_is_only_recorded() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;
        competent_session(&pool, "s1", "2026-11-07T09:00:00Z").await;
        route(&pool, id, t0()).await.unwrap();

        let routed = decline(&pool, id, &session("other"), "not mine", None, t0())
            .await
            .unwrap();
        assert_eq!(routed, Routed::Offered(session("s1")));
        assert_eq!(offered_to(&pool, id).await, Some(session("s1")));
        let log = events(&pool, id).await;
        assert_eq!(
            log.iter().map(|e| e.1.as_str()).collect::<Vec<_>>(),
            vec!["offered", "declined"]
        );
        assert_eq!(log[1].3.as_deref(), Some("other"));
    }

    #[tokio::test]
    async fn a_claim_by_anyone_clears_the_offer() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;
        competent_session(&pool, "s1", "2026-11-07T09:00:00Z").await;
        route(&pool, id, t0()).await.unwrap();
        assert_eq!(offered_to(&pool, id).await, Some(session("s1")));

        let outcome = store::claim_by_id(&pool, id, &session("stranger"), t0())
            .await
            .unwrap();
        let ClaimOutcome::Won { task, .. } = outcome else {
            panic!("an offer is advisory: anyone may claim")
        };
        assert_eq!(task.offered_to, None);
        assert_eq!(task.offered_at, None);
    }

    #[tokio::test]
    async fn a_claimed_ticket_is_never_offered() {
        let pool = crate::testdb::fresh_pool().await;
        let id = ticket(&pool).await;
        competent_session(&pool, "s1", "2026-11-07T09:00:00Z").await;
        store::claim_by_id(&pool, id, &session("s2"), t0())
            .await
            .unwrap();

        assert_eq!(route(&pool, id, t0()).await.unwrap(), Routed::NoCandidates);
        assert_eq!(offered_to(&pool, id).await, None);
        assert!(
            events(&pool, id).await.iter().all(|e| e.1 != "offered"),
            "no offered event"
        );
    }
}
