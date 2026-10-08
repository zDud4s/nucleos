//! Somewhere to leave words for a job that is already in flight.
//!
//! A job is a sequence of runs over one worktree, and each of its nodes is born with a clean context
//! window — so nothing said to one node ever reaches the next. The steering channel is not the way
//! in either: `POST /runs/{id}/message` writes into an in-memory handle on a LIVE process's stdin,
//! so it needs its target running and spawned `steerable`, and `create_job_node_run` passes `false`
//! on purpose. An owner watching a night job therefore had nowhere to put "when you get to item 3,
//! update the docs too": the words had to be said to a process that did not exist yet.
//!
//! They are said to the JOB instead, and the next node reads them on its way to being started. That
//! is the mechanism migration 0069 already built for `job_items.gate_output` — text stored on a row,
//! carried into the next prompt — with a person at the writing end rather than the gate.
//!
//! **This module owns the `job_notes` SQL and nothing else**, the way `runs.rs` and `job.rs` own
//! theirs, so `storage.rs` stays table-agnostic. It does NOT decide when a node runs: `job.rs` holds
//! that, calls in here at `spawn_node` — the one seam every node kind passes through — and reports a
//! delivery only once the run carrying it exists.
//!
//! **Owner only, and not a channel an agent can write to.** There is no MCP tool for this and no
//! route below Admin that reaches it; `author` is written from the key that left the note, never
//! from a body field. A run that could leave notes for another run is a separate change with a
//! governance question attached, and this module is deliberately not built toward it.

use sqlx::SqlitePool;

/// The author written on a note left through the daemon's own front door.
///
/// A constant rather than something read off the request, because the only scopes that reach the
/// route are Control and Admin, and both of them ARE the person — there is no second identity to
/// distinguish. `auth::Scope` carries no name to borrow, and inventing one from a token's label
/// would attribute the words to a credential that can be revoked and reissued while the note it
/// signed lives on.
pub const OWNER: &str = "the owner";

/// One thing an owner said to a job, and whether it has been said to anything yet.
///
/// `delivered_at` / `delivered_to_run_id` are `Option` because being empty IS the queue: a note is
/// pending exactly while nothing has read it out, so there is no status column to keep in step and
/// no way for the two to disagree.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Note {
    pub id: i64,
    pub job_id: i64,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub delivered_at: Option<String>,
    pub delivered_to_run_id: Option<i64>,
}

/// Named once so the two readers below cannot drift into selecting different shapes of the same row.
const NOTE_COLUMNS: &str =
    "id, job_id, body, author, created_at, delivered_at, delivered_to_run_id";

/// Files the words. Nothing else happens: no node is woken, no job is nudged.
///
/// The wait is the design rather than a limitation. A note is for whichever node comes next, and
/// what makes that safe is that no node is interrupted — the words arrive at the top of a fresh
/// context window, as part of the brief, instead of into the middle of work already under way.
pub async fn leave(pool: &SqlitePool, job_id: i64, body: &str, author: &str) -> sqlx::Result<i64> {
    Ok(
        sqlx::query("INSERT INTO job_notes (job_id, body, author, created_at) VALUES (?, ?, ?, ?)")
            .bind(job_id)
            .bind(body)
            .bind(author)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(pool)
            .await?
            .last_insert_rowid(),
    )
}

/// The notes this job has not been told yet, oldest first.
///
/// **A read, and never a claim.** Selecting the rows and stamping them in one breath would look
/// identical from the caller's side right up until the node they were read for never starts — the
/// item was claimed by another pass, the project's slot was taken, the tree could not be provisioned
/// — and the owner's words would be marked delivered to nothing, with nothing anywhere saying so.
/// [`mark_delivered`] is the only thing that empties this queue, and `job::spawn_node` calls it only
/// once a run exists.
pub async fn pending(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Vec<Note>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {NOTE_COLUMNS} FROM job_notes
         WHERE job_id = ? AND delivered_at IS NULL ORDER BY id"
    )))
    .bind(job_id)
    .fetch_all(pool)
    .await
}

/// Every note left on this job, delivered or not, oldest first.
///
/// What `JobDetail` shows, and deliberately wider than [`pending`]: an owner needs to see the note
/// while it waits — otherwise a queued note and a dropped one look the same, and the natural
/// response to that is to leave it again — and needs to still see it afterwards, because the
/// question then becomes which node was told and whether that was in time.
pub async fn all(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Vec<Note>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {NOTE_COLUMNS} FROM job_notes WHERE job_id = ? ORDER BY id"
    )))
    .bind(job_id)
    .fetch_all(pool)
    .await
}

/// Records that these notes reached `run_id`, which takes them out of the queue.
///
/// Guarded by `delivered_at IS NULL` so a second call cannot rewrite which run was told first. Both
/// columns are written together, because "it was delivered" and "it reached THAT node" are the two
/// halves of the only question asked afterwards — whether the words arrived in time to change
/// anything — and the run is what holds the prompt they were appended to.
pub async fn mark_delivered(pool: &SqlitePool, ids: &[i64], run_id: i64) -> sqlx::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new("UPDATE job_notes SET delivered_at = ");
    query.push_bind(chrono::Utc::now().to_rfc3339());
    query.push(", delivered_to_run_id = ");
    query.push_bind(run_id);
    query.push(" WHERE delivered_at IS NULL AND id IN (");
    let mut listed = query.separated(", ");
    for id in ids {
        listed.push_bind(*id);
    }
    query.push(")");
    query.build().execute(pool).await?;
    Ok(())
}

/// PURE: the paragraph a node is given, or `None` when there is nothing to say.
///
/// `None` and never `Some("")`. `spawn_node` appends what this returns to a brief it was handed, so
/// an empty string — or a header with nothing under it — would put at least a blank line into the
/// prompt of every plan, implement and review node in the house, on behalf of a feature none of them
/// uses. A job with no notes has to produce the prompt it produces today, byte for byte.
///
/// The wording is this function's whole responsibility, and it is doing three things at once:
///
/// - it QUOTES. The body travels verbatim, because an owner who typed a sentence gets to have that
///   sentence read rather than the daemon's summary of it.
/// - it ATTRIBUTES. Unattributed text appended to a brief is indistinguishable from the brief, so a
///   node could not weigh it, nor say where an instruction came from when the two disagree.
/// - it INTRODUCES rather than issues. The words never open the block: a node reading an
///   unattributed imperative at the top of a paragraph does that thing in place of its own item.
///
/// The same reasoning bans a whole vocabulary from this text. A note ADDS to the node's brief; any
/// phrasing that tells the node to drop what it was given turns one remark into a re-tasking, and
/// the owner who typed one sentence did not ask for their item to be abandoned.
pub fn render(notes: &[Note]) -> Option<String> {
    if notes.is_empty() {
        return None;
    }

    let mut block = String::from(if notes.len() == 1 {
        "\n\nThe owner of this job left a note on it after the work started, for whichever node came \
         next. It is quoted below word for word. Your brief above is still what you were asked to \
         do, and this is something the owner has added to it:"
    } else {
        "\n\nThe owner of this job left notes on it after the work started, for whichever node came \
         next. They are quoted below word for word, oldest first. Your brief above is still what you \
         were asked to do, and these are things the owner has added to it:"
    });
    for note in notes {
        block.push_str(&format!(
            "\n\n{} wrote at {}:\n\n{}",
            note.author, note.created_at, note.body
        ));
    }
    Some(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// A job for the notes to belong to.
    ///
    /// `job_notes.job_id` references `jobs(id)`, and every question this module answers is asked
    /// per job — so a test that left notes against an id nothing owns would be exercising a shape
    /// the daemon never produces.
    async fn seed_job(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES ('project-a', 'C:/projects/project-a', 'implementing', 5,
                     '2026-08-15T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// The words an owner would actually leave, used by every test here so that what is asserted
    /// about them is asserted about one thing.
    const NOTE: &str = "when you get to item 3, update the docs too";
    const OWNER: &str = "helena";

    /// `None` and not `Some("")`: the empty queue has to leave the prompt untouched byte for byte.
    ///
    /// This is the assertion that keeps the feature invisible to every job that never uses it —
    /// which, on the day it ships, is every job in the house. `spawn_node` appends what this returns
    /// to a prompt it was handed, so a `Some` of an empty string, or of a header with nothing under
    /// it, would put at least a blank line and at most a paragraph about notes into the brief of
    /// every plan, implement and review node ever started. That is a change to every prompt in the
    /// system, made to say nothing, and it would land where nobody is looking for it: prompts are
    /// compared by eye, and a trailing newline is exactly the difference an eye does not catch.
    #[test]
    fn an_empty_queue_leaves_a_prompt_exactly_as_it_was() {
        assert_eq!(
            render(&[]),
            None,
            "an empty queue has to render as nothing to append, not as an empty something"
        );
    }

    /// The node is told what the owner said, and told whose words they are.
    ///
    /// The wording itself is `render`'s to choose and this test does not pin it. What it pins is the
    /// three properties that decide how a node reads the paragraph, and each of them is a way the
    /// feature can be built wrong while looking finished:
    ///
    /// - the words survive verbatim. A `render` that summarised or rephrased would deliver the
    ///   owner's intent as the daemon understood it, which is the one thing a note must not become —
    ///   an owner who typed a sentence gets to have that sentence read.
    /// - the note is attributed. Anonymous text appended to a brief is indistinguishable from the
    ///   brief, so a node cannot weigh it, cannot say where an instruction came from when the two
    ///   conflict, and has no reason to treat "update the docs too" as an addition rather than as a
    ///   correction of everything above it.
    /// - the words are introduced rather than issued. `find(..) > 0` is the cheapest test of that:
    ///   a `render` that returns the body alone, or the body first, has written a fresh imperative
    ///   into the prompt, and a node reading an unattributed imperative at the top of a paragraph
    ///   obeys it in place of its own item.
    ///
    /// The vocabulary check is the same property from the other side. A node still has its own
    /// brief; a note adds to it. Wording that tells the node to ignore, disregard or replace what it
    /// was given turns a remark into a re-tasking, and the owner who typed one sentence did not ask
    /// for the item to be abandoned.
    #[test]
    fn a_note_is_rendered_as_the_owners_words_rather_than_a_fresh_instruction() {
        let note = Note {
            id: 1,
            job_id: 7,
            body: NOTE.to_owned(),
            author: OWNER.to_owned(),
            created_at: "2026-08-15T01:00:00Z".to_owned(),
            delivered_at: None,
            delivered_to_run_id: None,
        };

        let rendered = render(std::slice::from_ref(&note)).expect("one note renders as something");

        let at = rendered
            .find(NOTE)
            .unwrap_or_else(|| panic!("the owner's words did not survive rendering: {rendered}"));
        assert!(
            rendered.contains(OWNER),
            "nothing says who left the note, so the node reads it as its own brief: {rendered}"
        );
        assert!(
            at > 0,
            "the words open the paragraph with nothing introducing them, which is an instruction \
             and not a quotation: {rendered}"
        );

        let lowered = rendered.to_lowercase();
        for overriding in ["ignore", "disregard", "instead of", "replace"] {
            assert!(
                !lowered.contains(overriding),
                "a note adds to the node's brief; `{overriding}` tells it to drop it: {rendered}"
            );
        }
    }

    /// The queue empties as it is read out, and `mark_delivered` is what empties it.
    ///
    /// Without this, the obvious implementation of `pending` — every note of the job — would hand
    /// the same words to every node of the night. An owner who wrote one sentence at midnight would
    /// have it appended to item 4, item 5, the replan and the review, each of them reading it as
    /// something newly said about the work in front of it.
    ///
    /// The run id is asserted for the same reason it is a parameter at all. A `mark_delivered` that
    /// only stamped `delivered_at` would satisfy every other assertion here while leaving the note's
    /// most useful column empty: afterwards the owner's question is not "was it delivered" but
    /// "did it reach anything in time", and only the run holds the prompt that answers it.
    #[tokio::test]
    async fn a_note_is_delivered_once_and_then_never_again() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool).await;

        let note_id = leave(&pool, job_id, NOTE, OWNER)
            .await
            .expect("leaving a note on a live job");

        let waiting = pending(&pool, job_id).await.expect("read the queue");
        assert_eq!(waiting.len(), 1, "the note that was left is not waiting");
        assert_eq!(waiting[0].id, note_id);
        assert_eq!(
            waiting[0].body, NOTE,
            "the queue has to give back what was written, not a normalised version of it"
        );
        assert_eq!(
            waiting[0].author, OWNER,
            "a note whose author is lost cannot be quoted as anyone's"
        );
        assert!(
            waiting[0].delivered_at.is_none(),
            "a note that already reads as delivered is one nobody will ever be given"
        );

        mark_delivered(&pool, &[note_id], 42)
            .await
            .expect("record the delivery");

        assert!(
            pending(&pool, job_id)
                .await
                .expect("read the queue again")
                .is_empty(),
            "the note is still queued after being delivered, so every later node gets it again"
        );

        let (delivered_at, delivered_to): (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT delivered_at, delivered_to_run_id FROM job_notes WHERE id = ?")
                .bind(note_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            delivered_at.is_some(),
            "nothing recorded when the note was read out"
        );
        assert_eq!(
            delivered_to,
            Some(42),
            "the note does not say which run was told, so nobody can check whether it arrived in time"
        );
    }

    /// Reading the queue is a read. It does not take the note out of it.
    ///
    /// This is the half that cannot be seen from the delivery test, and it is the failure mode with
    /// no symptom: a `pending` written as a claim — SELECT the rows, stamp them, return them — looks
    /// identical from the caller's side right up until the node it was read for never starts. The
    /// item was claimed by another pass, the project's slot was taken, the tree could not be
    /// provisioned; the run is never created, the prompt is never used, and the owner's words are
    /// marked delivered to nothing. Nothing in the feed says so, the note is gone from the queue,
    /// and the owner finds out by watching the job finish without ever doing what they asked.
    ///
    /// So the note has to survive being looked at. `mark_delivered` is the only thing that consumes
    /// it, and `spawn_node` calls that only once a run exists.
    #[tokio::test]
    async fn a_note_survives_a_node_that_never_started() {
        let pool = test_pool().await;
        let job_id = seed_job(&pool).await;

        let note_id = leave(&pool, job_id, NOTE, OWNER)
            .await
            .expect("leaving a note on a live job");

        let first = pending(&pool, job_id).await.expect("read the queue");
        let second = pending(&pool, job_id)
            .await
            .expect("read the queue a second time");

        assert_eq!(
            first.iter().map(|note| note.id).collect::<Vec<_>>(),
            vec![note_id],
            "the note was not waiting even on the first read"
        );
        assert_eq!(
            second.iter().map(|note| note.id).collect::<Vec<_>>(),
            vec![note_id],
            "reading the queue consumed the note, so a node that then failed to start would have \
             swallowed words nobody was ever told"
        );
    }
}
