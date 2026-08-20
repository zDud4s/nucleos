//! What the agent has learned, and what a run is told because of it.
//!
//! The gap this closes was named from outside this repository: 87 migrations and no table for
//! anything the agent learned, so every run started from the same prompt with the same blind spots
//! for ever. `0088_refinements.sql` carries the design; this is the half that decides what a node
//! actually reads.
//!
//! **Split the way `classifier.rs` is split from `hooks.rs`.** [`render`] is pure and holds the
//! only interesting question — what does a node see, in what order, and how much of it — so it is
//! table tests with no database. The storage below is deliberately dumb.

use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

/// How much of the refinement layer a single node's prompt may carry.
///
/// A ceiling and not a target. The run pays for every token of its own brief, and a layer that
/// grows for a year would silently take the context the work needs — the failure mode being that
/// nobody notices, because a prompt does not get slower, it gets emptier of room.
pub const RENDER_CHARS: usize = 4_000;

/// The four kinds, in the order a node reads them.
///
/// Ordered deliberately and not alphabetically: an instruction changes what the node does, a fact
/// changes what it believes, and the last two only matter once it is doing the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Kind {
    Prompt,
    Memory,
    Skill,
    Subagent,
}

impl Kind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "prompt" => Some(Kind::Prompt),
            "memory" => Some(Kind::Memory),
            "skill" => Some(Kind::Skill),
            "subagent" => Some(Kind::Subagent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Prompt => "prompt",
            Kind::Memory => "memory",
            Kind::Skill => "skill",
            Kind::Subagent => "subagent",
        }
    }

    /// What this kind is called where a node reads it.
    fn heading(self) -> &'static str {
        match self {
            Kind::Prompt => "Standing instructions",
            Kind::Memory => "What earlier runs found out about this project",
            Kind::Skill => "How recurring work here is done",
            Kind::Subagent => "Delegations that have worked before",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Refinement {
    pub id: i64,
    pub project_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub proposal_id: Option<i64>,
    pub supersedes: Option<i64>,
    pub origin_run_id: Option<i64>,
    pub created_at: String,
    pub activated_at: Option<String>,
    pub ended_at: Option<String>,
}

/// PURE: the block a node's brief gains because of what earlier runs learned.
///
/// Appended to the brief and never replacing it, exactly as `notes::render` is — a node handed a
/// standing instruction instead of its task does the standing instruction.
pub fn render(refinements: &[Refinement]) -> Option<String> {
    // Only what a person approved. Filtered here rather than trusted from the caller's query: this
    // function is the last thing between a `proposed` row and a node's prompt, and a refinement that
    // reaches a prompt unapproved makes the approval decorative, which is the entire mechanism.
    let mut live: Vec<(Kind, &Refinement)> = refinements
        .iter()
        .filter(|refinement| refinement.status == "active")
        .filter_map(|refinement| Kind::parse(&refinement.kind).map(|kind| (kind, refinement)))
        .collect();
    if live.is_empty() {
        return None;
    }
    // Kind first, id second: what a node reads first is a property of the layer, never of the order
    // rows happened to come back from SQLite.
    live.sort_by_key(|(kind, refinement)| (*kind, refinement.id));

    let mut block = String::from(
        "\n\nEarlier work on this project left the notes below, and a person approved every one of \
         them before it reached you. Your brief above is still what you were asked to do; these are \
         things already known about the project you are doing it in:",
    );

    let mut shown = 0usize;
    let mut heading_written: Option<Kind> = None;
    for (kind, refinement) in &live {
        // Rendered before it is measured, so the decision to include it is made on the length of
        // what will actually be written rather than on an estimate of it.
        let mut piece = String::new();
        if heading_written != Some(*kind) {
            piece.push_str(&format!("\n\n{}:", kind.heading()));
        }
        piece.push_str(&format!(
            "\n- {}: {}",
            refinement.title,
            clip(&refinement.body)
        ));

        if block.len() + piece.len() > RENDER_CHARS {
            break;
        }
        block.push_str(&piece);
        heading_written = Some(*kind);
        shown += 1;
    }

    // Said, never silent. A layer trimmed without saying so reads as the whole of what is known, and
    // a node that believes it has been told everything stops asking.
    let omitted = live.len() - shown;
    if omitted > 0 {
        block.push_str(&format!(
            "\n\n({omitted} further approved {} not shown here, to leave room for the work.)",
            if omitted == 1 { "note is" } else { "notes are" }
        ));
    }
    Some(block)
}

/// One refinement's share of the room.
///
/// By chars and not bytes: a slice through a UTF-8 boundary panics on exactly the inputs nobody
/// writes tests with, and a refinement is free text somebody wrote.
fn clip(body: &str) -> String {
    const PER_ITEM_CHARS: usize = 600;
    if body.chars().count() <= PER_ITEM_CHARS {
        return body.to_owned();
    }
    let mut cut: String = body.chars().take(PER_ITEM_CHARS).collect();
    cut.push('…');
    cut
}

/// What a node of this project is entitled to be told.
///
/// `project_id IS NULL` rows come too: those are machine-wide, and the scoping rule the migration
/// argues for is about not letting one project's lesson become another's lie — not about hiding
/// what was learned about the house itself.
///
/// Ordered here as well as in [`render`], so a caller that skips the renderer still gets a stable
/// list, and so the LIMIT below cuts the tail rather than an arbitrary middle.
pub async fn active_for(
    pool: &SqlitePool,
    project_id: Option<&str>,
) -> sqlx::Result<Vec<Refinement>> {
    sqlx::query_as::<_, Refinement>(
        "SELECT id, project_id, kind, title, body, status, proposal_id, supersedes, origin_run_id,
                created_at, activated_at, ended_at
           FROM refinements
          WHERE status = 'active' AND (project_id IS NULL OR project_id = ?)
          ORDER BY id
          LIMIT ?",
    )
    .bind(project_id)
    // A ceiling on the QUERY as well as on the rendering, because the two protect different things:
    // `RENDER_CHARS` keeps a prompt affordable, and this keeps a project that has approved ten
    // thousand refinements from reading all of them into memory to render forty.
    .bind(MAX_READ as i64)
    .fetch_all(pool)
    .await
}

/// How many approved refinements are read before rendering ever begins.
const MAX_READ: usize = 200;

/// What somebody is asking the layer to learn.
///
/// A struct and not eight positional arguments: the call site of the earlier shape was four string
/// literals in a row, where swapping `title` and `body` compiles, passes, and is found by a person
/// reading a strange prompt a week later.
pub struct Declaration<'a> {
    pub project_id: Option<&'a str>,
    pub origin_run_id: Option<i64>,
    pub kind: Kind,
    pub title: &'a str,
    pub body: &'a str,
    pub reasoning: &'a str,
    /// The refinement this one replaces — ended if and when THIS one is approved, never before.
    pub supersedes: Option<i64>,
}

/// The two things a declaration can say that the layer must refuse.
#[derive(Debug)]
pub enum ProposeError {
    Db(sqlx::Error),
    /// `supersedes` names a refinement that is not there — a chain nobody could read back.
    UnknownPredecessor(i64),
    /// `supersedes` names a refinement in another scope. Refused because it is the one column that
    /// writes across the project boundary the rest of this module exists to hold.
    ForeignPredecessor(i64),
}

impl std::fmt::Display for ProposeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposeError::Db(error) => write!(formatter, "{error}"),
            ProposeError::UnknownPredecessor(id) => {
                write!(formatter, "there is no refinement {id} to replace")
            }
            ProposeError::ForeignPredecessor(id) => {
                write!(formatter, "refinement {id} belongs to another project")
            }
        }
    }
}

impl std::error::Error for ProposeError {}

impl From<sqlx::Error> for ProposeError {
    fn from(error: sqlx::Error) -> Self {
        ProposeError::Db(error)
    }
}

/// A run declaring something it thinks the next run should know.
///
/// Writes the refinement `proposed` AND the proposal that asks about it, in one transaction — the
/// two are one act, and a crash between them would leave either a lesson nobody can approve or a
/// question about a lesson that is not there.
///
/// The row is written now rather than on approval, unlike `create_calendar_event`'s shape, and the
/// difference is deliberate: a rejected event is nothing, but a rejected LESSON is a record worth
/// keeping — it is how somebody later sees what the agent kept trying to learn and was told no to.
pub async fn propose(
    pool: &SqlitePool,
    declaration: Declaration<'_>,
) -> Result<(i64, i64), ProposeError> {
    let Declaration {
        project_id,
        origin_run_id,
        kind,
        title,
        body,
        reasoning,
        supersedes,
    } = declaration;

    // Checked before anything is written, and checked here rather than left to the foreign key:
    // SQLite would accept a link to another project's row without a word, and the failure would
    // surface as one repository's history quietly containing another's.
    if let Some(predecessor) = supersedes {
        let owner: Option<Option<String>> =
            sqlx::query_scalar("SELECT project_id FROM refinements WHERE id = ?")
                .bind(predecessor)
                .fetch_optional(pool)
                .await?;
        let owner = owner.ok_or(ProposeError::UnknownPredecessor(predecessor))?;
        if owner.as_deref() != project_id {
            return Err(ProposeError::ForeignPredecessor(predecessor));
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;

    let refinement_id = sqlx::query(
        "INSERT INTO refinements
           (project_id, kind, title, body, status, supersedes, origin_run_id, created_at)
         VALUES (?, ?, ?, ?, 'proposed', ?, ?, ?)",
    )
    .bind(project_id)
    .bind(kind.as_str())
    .bind(title)
    .bind(body)
    .bind(supersedes)
    .bind(origin_run_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO refinement_events (refinement_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'proposed', 'declared by a run', ?)",
    )
    .bind(refinement_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    // `tool_input` carries the id and nothing a reader would have to join to understand the
    // question. A proposal a person cannot answer without opening another screen is a proposal that
    // waits until morning and then gets approved unread.
    let tool_input = serde_json::json!({
        "refinement_id": refinement_id,
        "kind": kind.as_str(),
        "title": title,
        "body": body,
        // Carried so the question reads as what it is. "Approve this note" and "approve this note
        // INSTEAD of the one you approved in March" are different decisions, and only one of them
        // costs you something you already have.
        "supersedes": supersedes,
    })
    .to_string();

    let proposal_id = sqlx::query(
        "INSERT INTO proposals
           (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input,
            created_at, decided_at)
         VALUES ('refinement', 'pending', ?, NULL, ?, NULL, ?, ?, ?, NULL)",
    )
    .bind(origin_run_id)
    .bind(project_id)
    .bind(reasoning)
    .bind(&tool_input)
    .bind(&now)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'pending', 'created', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE refinements SET proposal_id = ? WHERE id = ?")
        .bind(proposal_id)
        .bind(refinement_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok((refinement_id, proposal_id))
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecisionError {
    NotFound,
    NotPending,
    Malformed,
}

/// Which refinement a pending proposal is asking about, or why it is not answerable.
///
/// Shared by both answers deliberately: a yes and a no must agree about what counts as a question,
/// or the pair drifts into a proposal that can be approved and not refused — which is exactly what
/// this layer shipped with, `proposals::reject_proposal` taking `action-approval` alone.
async fn pending_refinement(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let row: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT kind, status, tool_input FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| DecisionError::NotFound)?;
    let (kind, status, tool_input) = row.ok_or(DecisionError::NotFound)?;
    if kind != "refinement" {
        return Err(DecisionError::NotFound);
    }
    if status != "pending" {
        return Err(DecisionError::NotPending);
    }
    tool_input
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| {
            value
                .get("refinement_id")
                .and_then(serde_json::Value::as_i64)
        })
        .ok_or(DecisionError::Malformed)
}

/// A person said yes, and only now does anything reach a prompt.
///
/// One transaction, like the calendar's: a dropped request must not leave the proposal and the
/// layer disagreeing about whether the agent was allowed to learn something.
pub async fn approve(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let refinement_id = pending_refinement(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    // Guarded on `proposed`, so a second approval of the same row is a no-op rather than a second
    // activation stamp over the first.
    let activated = sqlx::query(
        "UPDATE refinements SET status = 'active', activated_at = ?
          WHERE id = ? AND status = 'proposed'",
    )
    .bind(&now)
    .bind(refinement_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;
    if activated.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    sqlx::query(
        "INSERT INTO refinement_events (refinement_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'active', 'approved by the owner', ?)",
    )
    .bind(refinement_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    // The chain moves here and nowhere else. A successor that ended its predecessor when it was
    // merely *declared* would let a question nobody answered delete the answer already in force,
    // so the old text stands until the moment somebody chooses the new one over it.
    let predecessor: Option<i64> =
        sqlx::query_scalar("SELECT supersedes FROM refinements WHERE id = ?")
            .bind(refinement_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| DecisionError::NotFound)?;
    if let Some(predecessor) = predecessor {
        let ended = sqlx::query(
            "UPDATE refinements SET status = 'superseded', ended_at = ?
              WHERE id = ? AND status = 'active'",
        )
        .bind(&now)
        .bind(predecessor)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;
        // Not an error when it matches nothing: the predecessor may have been reverted while this
        // successor waited for an answer, and what the person just approved is still approved.
        if ended.rows_affected() == 1 {
            sqlx::query(
                "INSERT INTO refinement_events (refinement_id, from_status, to_status, note, at)
                 VALUES (?, 'active', 'superseded', ?, ?)",
            )
            .bind(predecessor)
            .bind(format!("replaced by refinement {refinement_id}"))
            .bind(&now)
            .execute(&mut *tx)
            .await
            .map_err(|_| DecisionError::NotFound)?;
        }
    }

    sqlx::query("UPDATE proposals SET status = 'approved', decided_at = ? WHERE id = ?")
        .bind(&now)
        .bind(proposal_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', 'approved', 'refinement activated', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    tx.commit().await.map_err(|_| DecisionError::NotFound)?;
    Ok(refinement_id)
}

/// A person said no, and the refusal is kept.
///
/// The layer shipped without this and it was not a missing nicety: `proposals::reject_proposal`
/// answers `action-approval` alone, so a refinement proposal had one button. A queue you can only
/// say yes to is a queue where everything is eventually approved — and the thing being approved
/// here is what every later run is told.
///
/// `rejected` and not deleted, for the reason the module's own doc gives: what the agent kept
/// trying to learn and was told no to is a record worth having.
pub async fn reject(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let refinement_id = pending_refinement(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    let refused = sqlx::query(
        "UPDATE refinements SET status = 'rejected', ended_at = ?
          WHERE id = ? AND status = 'proposed'",
    )
    .bind(&now)
    .bind(refinement_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;
    if refused.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    sqlx::query(
        "INSERT INTO refinement_events (refinement_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'rejected', 'refused by the owner', ?)",
    )
    .bind(refinement_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    sqlx::query("UPDATE proposals SET status = 'rejected', decided_at = ? WHERE id = ?")
        .bind(&now)
        .bind(proposal_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| DecisionError::NotFound)?;

    sqlx::query(
        "INSERT INTO proposal_events (proposal_id, from_status, to_status, note, at)
         VALUES (?, 'pending', 'rejected', 'refinement refused', ?)",
    )
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    tx.commit().await.map_err(|_| DecisionError::NotFound)?;
    Ok(refinement_id)
}

/// Taking one back, which is the half that makes approving safe to do.
///
/// `reverted` and not deleted: the history is the feature. A layer somebody can only add to is one
/// nobody dares add to.
pub async fn revert(pool: &SqlitePool, refinement_id: i64, note: &str) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE refinements SET status = 'reverted', ended_at = ? WHERE id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(refinement_id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO refinement_events (refinement_id, from_status, to_status, note, at)
         VALUES (?, 'active', 'reverted', ?, ?)",
    )
    .bind(refinement_id)
    .bind(note)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// One decision in a refinement's life, as a person reads it back.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Event {
    pub id: i64,
    pub from_status: Option<String>,
    pub to_status: String,
    pub note: Option<String>,
    pub at: String,
}

/// What this says, what it said before, and what replaced it — the reviewable history the whole
/// layer is for, in one answer.
///
/// One call and not three, because the three are only useful together: "revert this" is a decision
/// a person makes by reading the text that would come back, and a screen that made them fetch it
/// separately is a screen where they revert without having read it.
#[derive(Debug, Serialize)]
pub struct History {
    pub refinement: Refinement,
    pub events: Vec<Event>,
    /// Newest first: what this one replaced, then what THAT replaced, back to the first text.
    pub replaced: Vec<Refinement>,
    /// What replaced this one, if a person has approved a successor.
    pub replaced_by: Option<Refinement>,
}

/// How far back a chain is read before the walk stops and says no more.
const MAX_CHAIN: usize = 50;

/// Read one refinement, its own decisions, and the chain on both sides of it.
pub async fn history(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<History>> {
    let Some(refinement) = fetch(pool, id).await? else {
        return Ok(None);
    };

    let events = sqlx::query_as::<_, Event>(
        "SELECT id, from_status, to_status, note, at
           FROM refinement_events WHERE refinement_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let mut replaced: Vec<Refinement> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::from([id]);
    let mut next = refinement.supersedes;
    while let Some(previous) = next {
        // No path through this module can write a cycle — a predecessor must already exist, so
        // links only ever point backwards — but this is a loop over data, and a loop over data that
        // trusts it terminates until the day the data is wrong, and then it hangs the daemon
        // holding the connection instead of returning a poor answer.
        if !seen.insert(previous) || replaced.len() >= MAX_CHAIN {
            break;
        }
        let Some(row) = fetch(pool, previous).await? else {
            break;
        };
        next = row.supersedes;
        replaced.push(row);
    }

    // The newest successor, because a chain forked by two proposals approved out of order is a
    // thing SQLite will happily store and a person should still be able to read.
    let replaced_by = sqlx::query_as::<_, Refinement>(
        "SELECT id, project_id, kind, title, body, status, proposal_id, supersedes, origin_run_id,
                created_at, activated_at, ended_at
           FROM refinements WHERE supersedes = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(Some(History {
        refinement,
        events,
        replaced,
        replaced_by,
    }))
}

async fn fetch(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<Refinement>> {
    sqlx::query_as::<_, Refinement>(
        "SELECT id, project_id, kind, title, body, status, proposal_id, supersedes, origin_run_id,
                created_at, activated_at, ended_at
           FROM refinements WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn seed(pool: &sqlx::SqlitePool, project: Option<&str>, status: &str, title: &str) {
        sqlx::query(
            "INSERT INTO refinements (project_id, kind, title, body, status, created_at)
             VALUES (?, 'memory', ?, 'body', ?, '2026-08-19T00:00:00+00:00')",
        )
        .bind(project)
        .bind(title)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The scoping rule the migration argues for, asserted in both directions at once. A lesson
    /// about one repository's build is a lie about another's — and a lesson about the house is not
    /// hidden by that rule, which is the half a `project_id = ?` alone would get wrong.
    #[tokio::test]
    async fn a_node_reads_its_own_projects_lessons_and_the_houses_and_no_others() {
        let pool = test_pool().await;
        seed(&pool, Some("mine"), "active", "mine-active").await;
        seed(&pool, None, "active", "house-wide").await;
        seed(&pool, Some("other"), "active", "someone-elses").await;
        seed(&pool, Some("mine"), "proposed", "mine-unapproved").await;

        let read = active_for(&pool, Some("mine")).await.unwrap();
        let titles: Vec<&str> = read.iter().map(|r| r.title.as_str()).collect();

        assert!(
            titles.contains(&"mine-active"),
            "own project's lesson missing: {titles:?}"
        );
        assert!(
            titles.contains(&"house-wide"),
            "machine-wide lesson missing: {titles:?}"
        );
        assert!(
            !titles.contains(&"someone-elses"),
            "another project's lesson leaked in: {titles:?}"
        );
        assert!(
            !titles.contains(&"mine-unapproved"),
            "a refinement nobody approved was read for a prompt: {titles:?}"
        );
    }

    /// The whole mechanism, end to end and in the order it happens: a run declares, nothing reaches
    /// a prompt, a person says yes, and only then does it. The middle assertion is the one that
    /// matters — it is what "the agent declares and the core activates" means when it is true.
    #[tokio::test]
    async fn a_declared_lesson_reaches_no_prompt_until_a_person_approves_it() {
        let pool = test_pool().await;
        let (refinement_id, proposal_id) = propose(
            &pool,
            Declaration {
                project_id: Some("mine"),
                origin_run_id: Some(900_001),
                kind: Kind::Memory,
                title: "The suite needs Git's usr/bin on PATH",
                body: "Nine tests spawn echo as a program and Windows has no real echo.exe but \
                       Git's.",
                reasoning: "learned it the hard way in this run",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        assert!(
            active_for(&pool, Some("mine")).await.unwrap().is_empty(),
            "a lesson nobody approved was already reaching prompts"
        );

        assert_eq!(approve(&pool, proposal_id).await.unwrap(), refinement_id);
        let after = active_for(&pool, Some("mine")).await.unwrap();
        assert_eq!(after.len(), 1, "approving did not activate the lesson");
        assert!(render(&after).is_some(), "an active lesson renders nothing");

        // Second approval is a no-op rather than a second activation stamp.
        assert_eq!(
            approve(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        );

        assert!(
            revert(&pool, refinement_id, "made things worse")
                .await
                .unwrap()
        );
        assert!(
            active_for(&pool, Some("mine")).await.unwrap().is_empty(),
            "a reverted lesson still reaches prompts"
        );

        // The history is the feature: four rows, not a deleted row.
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM refinement_events WHERE refinement_id = ?")
                .bind(refinement_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events, 3, "the refinement's history is not reviewable");
    }

    fn one(id: i64, kind: &str, title: &str, body: &str) -> Refinement {
        Refinement {
            id,
            project_id: Some("p".into()),
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
            status: "active".into(),
            proposal_id: Some(1),
            supersedes: None,
            origin_run_id: Some(900_001),
            created_at: "2026-08-19T00:00:00+00:00".into(),
            activated_at: Some("2026-08-19T00:00:00+00:00".into()),
            ended_at: None,
        }
    }

    /// A project that has learned nothing must cost nothing. The block is appended to every node of
    /// every job, so an empty layer that still wrote a heading would tax every run for ever.
    #[test]
    fn a_project_with_nothing_learned_adds_nothing_to_the_brief() {
        assert!(render(&[]).is_none());
    }

    /// The failure this wording exists to prevent: a node handed a standing instruction INSTEAD of
    /// its task does the standing instruction. `notes::render` had to say the same thing, and its
    /// test asserts the item's own brief is still there beside it.
    #[test]
    fn the_block_adds_to_the_brief_rather_than_replacing_it() {
        let block = render(&[one(
            1,
            "prompt",
            "Run fmt",
            "Always run cargo fmt before finishing.",
        )])
        .expect("one active refinement renders");
        assert!(block.contains("Run fmt"), "the title is missing: {block}");
        assert!(
            block.contains("Always run cargo fmt before finishing."),
            "the body is missing: {block}"
        );
        assert!(
            block.to_lowercase().contains("still"),
            "nothing tells the node its own brief still stands: {block}"
        );
    }

    /// Grouped, and in the order of the enum rather than the order rows happen to arrive. A node
    /// reading an instruction after four delegation specs has already spent its attention.
    #[test]
    fn the_kinds_arrive_in_a_fixed_order_whatever_order_the_rows_do() {
        let block = render(&[
            one(4, "subagent", "zzz-delegation", "s"),
            one(3, "skill", "zzz-skill", "k"),
            one(2, "memory", "zzz-fact", "m"),
            one(1, "prompt", "zzz-instruction", "p"),
        ])
        .expect("renders");
        // Distinctive needles, because the first version of this test used "P"/"M"/"K"/"S" and "S"
        // matched the S of "Standing instructions" — it was comparing a heading against an item and
        // failing on a renderer that was right.
        let at = |needle: &str| {
            block
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        assert!(
            at("zzz-instruction") < at("zzz-fact"),
            "instructions must come before facts: {block}"
        );
        assert!(
            at("zzz-fact") < at("zzz-skill"),
            "facts must come before skills: {block}"
        );
        assert!(
            at("zzz-skill") < at("zzz-delegation"),
            "skills must come before delegations: {block}"
        );
    }

    /// A layer that grows for a year would take the context the work needs, and the failure mode is
    /// silent: a prompt does not get slower, it gets emptier of room. So it is bounded, and it says
    /// what it left out rather than trimming in silence.
    #[test]
    fn the_block_is_bounded_and_says_what_it_left_out() {
        let many: Vec<Refinement> = (1..=60)
            .map(|i| one(i, "memory", &format!("fact {i}"), &"x".repeat(300)))
            .collect();
        let block = render(&many).expect("renders");
        assert!(
            block.len() <= RENDER_CHARS * 2,
            "unbounded: {} chars from {} refinements",
            block.len(),
            many.len()
        );
        assert!(
            block.contains("not shown") || block.contains("more"),
            "trimmed in silence, which is the one way this may not fail: {block}"
        );
    }

    /// Only what a person approved. A `proposed` row reaching a prompt would make the approval
    /// decorative, which is the whole mechanism.
    #[test]
    fn nothing_that_a_person_has_not_approved_reaches_a_node() {
        let mut waiting = one(1, "prompt", "Not yet", "This was never approved.");
        waiting.status = "proposed".into();
        let mut taken_back = one(2, "prompt", "Taken back", "This was reverted.");
        taken_back.status = "reverted".into();
        assert!(
            render(&[waiting, taken_back]).is_none(),
            "a refinement nobody approved reached a node's prompt"
        );
    }

    /// A short way to say "somebody declared this", so the tests below read as the sequence they
    /// assert rather than as seven fields of noise repeated four times.
    async fn declare(
        pool: &sqlx::SqlitePool,
        project: Option<&str>,
        title: &str,
        replaces: Option<i64>,
    ) -> Result<(i64, i64), ProposeError> {
        propose(
            pool,
            Declaration {
                project_id: project,
                origin_run_id: None,
                kind: Kind::Prompt,
                title,
                body: "body",
                reasoning: "because",
                supersedes: replaces,
            },
        )
        .await
    }

    /// The column the migration argued for, asserted at the moment it means anything: **approving
    /// the successor** is what ends the predecessor, and nothing before that does.
    ///
    /// The middle assertion is the one worth the test. A successor that ended the old text the
    /// moment it was *declared* would let a rejected proposal delete what it failed to replace —
    /// the layer would lose a lesson by way of a question nobody said yes to.
    #[tokio::test]
    async fn approving_a_successor_is_what_ends_the_one_it_replaces() {
        let pool = test_pool().await;
        let (first, first_proposal) = declare(&pool, Some("mine"), "old text", None)
            .await
            .unwrap();
        approve(&pool, first_proposal).await.unwrap();

        let (second, second_proposal) = declare(&pool, Some("mine"), "new text", Some(first))
            .await
            .unwrap();
        let live: Vec<i64> = active_for(&pool, Some("mine"))
            .await
            .unwrap()
            .iter()
            .map(|refinement| refinement.id)
            .collect();
        assert_eq!(
            live,
            vec![first],
            "an unapproved successor already ended the text it wants to replace"
        );

        approve(&pool, second_proposal).await.unwrap();
        let live: Vec<i64> = active_for(&pool, Some("mine"))
            .await
            .unwrap()
            .iter()
            .map(|refinement| refinement.id)
            .collect();
        assert_eq!(
            live,
            vec![second],
            "both texts are in force at once, which is the pile the chain exists to prevent"
        );

        let (status, ended): (String, Option<String>) =
            sqlx::query_as("SELECT status, ended_at FROM refinements WHERE id = ?")
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "superseded", "the predecessor kept a wrong status");
        assert!(ended.is_some(), "the predecessor ended at no time at all");

        // Named, not merely ended: "this stopped applying" and "this was replaced by that" are
        // different things to read six months later, and only one of them can be acted on.
        let note: String = sqlx::query_scalar(
            "SELECT note FROM refinement_events WHERE refinement_id = ? AND to_status = 'superseded'",
        )
        .bind(first)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            note.contains(&second.to_string()),
            "the history does not say what replaced it: {note}"
        );
    }

    /// Two refusals that protect the same rule the scoping does. A successor naming nothing would
    /// leave a dangling chain nobody can read back; a successor naming ANOTHER project's lesson
    /// would let one repository end another's — the exact poisoning `project_id` exists to stop,
    /// arriving through the one column that writes across the boundary.
    #[tokio::test]
    async fn a_successor_may_not_name_nothing_nor_another_projects_lesson() {
        let pool = test_pool().await;
        let (theirs, _) = declare(&pool, Some("theirs"), "their lesson", None)
            .await
            .unwrap();

        assert!(
            matches!(
                declare(&pool, Some("mine"), "replaces a ghost", Some(4242)).await,
                Err(ProposeError::UnknownPredecessor(4242))
            ),
            "a refinement was allowed to replace something that does not exist"
        );
        assert!(
            matches!(
                declare(&pool, Some("mine"), "reaches across", Some(theirs)).await,
                Err(ProposeError::ForeignPredecessor(_))
            ),
            "one project was allowed to end another project's lesson"
        );
    }

    /// Saying no, kept as a refusal rather than as an absence.
    ///
    /// Written after finding the layer shipped able to approve and unable to refuse — a queue with
    /// one button is a queue where everything is eventually approved, and what is being approved
    /// here is what every later run is told.
    #[tokio::test]
    async fn a_refused_lesson_is_kept_as_a_refusal_rather_than_deleted() {
        let pool = test_pool().await;
        let (refinement_id, proposal_id) = declare(&pool, Some("mine"), "not this one", None)
            .await
            .unwrap();

        assert_eq!(reject(&pool, proposal_id).await.unwrap(), refinement_id);
        let refused = fetch(&pool, refinement_id)
            .await
            .unwrap()
            .expect("the refusal was deleted rather than recorded");
        assert_eq!(refused.status, "rejected");
        assert!(refused.ended_at.is_some(), "a refusal with no time on it");
        assert!(
            active_for(&pool, Some("mine")).await.unwrap().is_empty(),
            "a refused lesson is reaching prompts"
        );

        // Answered once: a second refusal is a conflict, not a second decision written over the
        // first. Same guard the approval carries, and asserted here because the two must agree.
        assert_eq!(
            reject(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        );
        assert_eq!(
            approve(&pool, proposal_id).await,
            Err(DecisionError::NotPending),
            "a refused proposal could still be approved afterwards"
        );

        let decided: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(decided, "rejected", "the question is still in the queue");
    }

    /// What the history screen reads: the chain back through every text this one replaced, and the
    /// successor that replaced it, oldest question first — "what did this say before I changed it".
    ///
    /// The cycle at the end is forced with a raw UPDATE because no path in this module can create
    /// one (a predecessor must already exist, so links only ever point backwards). It is asserted
    /// anyway: a walk that trusts its data terminates until the day the data is wrong, and then it
    /// hangs the daemon instead of returning a bad answer.
    #[tokio::test]
    async fn the_history_reads_back_through_everything_a_refinement_replaced() {
        let pool = test_pool().await;
        let (first, first_proposal) = declare(&pool, Some("mine"), "first text", None)
            .await
            .unwrap();
        approve(&pool, first_proposal).await.unwrap();
        let (second, second_proposal) = declare(&pool, Some("mine"), "second text", Some(first))
            .await
            .unwrap();
        approve(&pool, second_proposal).await.unwrap();
        let (third, third_proposal) = declare(&pool, Some("mine"), "third text", Some(second))
            .await
            .unwrap();
        approve(&pool, third_proposal).await.unwrap();

        // `middle` and not `history`: a local of the same name shadows the function, and the next
        // call in this test reads as calling a struct.
        let middle = history(&pool, second)
            .await
            .unwrap()
            .expect("a refinement that exists has a history");
        assert_eq!(middle.refinement.id, second);
        assert_eq!(
            middle.replaced.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![first],
            "the text this one replaced is not readable from it"
        );
        assert_eq!(
            middle.replaced_by.as_ref().map(|r| r.id),
            Some(third),
            "the text that replaced this one is not readable from it"
        );
        // proposed → active → superseded, all three still there.
        assert_eq!(
            middle.events.len(),
            3,
            "the row's own history is incomplete: {:?}",
            middle.events
        );

        assert!(
            history(&pool, 4242).await.unwrap().is_none(),
            "a refinement that does not exist reported a history"
        );

        sqlx::query("UPDATE refinements SET supersedes = ? WHERE id = ?")
            .bind(third)
            .bind(first)
            .execute(&pool)
            .await
            .unwrap();
        let looped = history(&pool, third)
            .await
            .unwrap()
            .expect("the walk returned rather than hanging");
        let mut ids: Vec<i64> = looped.replaced.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            looped.replaced.len(),
            "the walk went round the cycle and read a row twice"
        );
    }
}
