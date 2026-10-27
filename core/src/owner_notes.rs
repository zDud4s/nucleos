//! The owner's own notes: storage for a second brain that belongs to the person.
//!
//! **Owner only.** Like `notes.rs` (`job_notes`, see its module doc at lines 19-22), this is not a
//! channel an agent can write to, and it goes further: nothing in the agent's memory reads this
//! module. `brief`, `knowledge` and `mcp_tools` never touch these tables, so a note reaches no
//! prompt unless a person deliberately teaches it (a separate, proposal-gated step). The one
//! exception is `distill.rs`, which reads project-linked notes as dossier context only
//! (`.ai/specs/2026-10-05-destilador-design.md` D4).
//!
//! This module owns the `owner_notes*` SQL. The vocabularies below are checked here, before the
//! write, because the migration carries no CHECK constraints (the house rule of 0143).

use sqlx::SqlitePool;

pub mod embeddings;
pub mod related;
pub mod review;

/// Where a note was written from. Checked before the insert.
pub const ORIGINS: [&str; 1] = ["shell"];

/// Whether a note may carry this origin: the shell, or `channel:<id>` for a channel whose manifest
/// is compiled in. The bare v1 `telegram` is no longer accepted (migration 0235 rewrote it).
pub fn origin_allowed(origin: &str) -> bool {
    origin == "shell"
        || origin
            .strip_prefix("channel:")
            .is_some_and(|id| crate::channels::registry::by_id(id).is_some())
}
/// The two states a note can be in. Archiving hides a note; nothing is ever deleted.
pub const STATES: [&str; 2] = ["active", "archived"];

/// One note, as the API serialises it (`note_text` goes out as `text`).
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct OwnerNote {
    pub id: i64,
    #[serde(rename = "text")]
    pub note_text: String,
    pub origin: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

/// One entry of a note's history. `detail` holds the PREVIOUS text for an `edited` event.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Event {
    pub id: i64,
    pub note_id: i64,
    pub kind: String,
    pub detail: Option<String>,
    pub at: String,
}

#[derive(Debug)]
pub enum NoteError {
    Empty,
    UnknownOrigin,
    UnknownState,
    NotFound,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for NoteError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for NoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a note needs some text"),
            Self::UnknownOrigin => formatter.write_str("unknown origin"),
            Self::UnknownState => formatter.write_str("unknown state"),
            Self::NotFound => formatter.write_str("note not found"),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

/// Which notes `list` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    Active,
    Archived,
    All,
}

/// Named once so the readers cannot drift into selecting different shapes of the same row.
const NOTE_COLUMNS: &str = "id, note_text, origin, state, created_at, updated_at";

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Files a note, active, with a `created` event, in one transaction. The text is trimmed; nothing is
/// written for an empty one or an unknown origin.
pub async fn create(pool: &SqlitePool, text: &str, origin: &str) -> Result<i64, NoteError> {
    let mut tx = pool.begin().await?;
    let id = insert_note_in(&mut tx, text, origin).await?;
    tx.commit().await?;
    // Best-effort and asynchronous: the embedder is woken, never awaited.
    embeddings::nudge();
    Ok(id)
}

// Lives here, not in `capture.rs`, so that `capture.rs` names the notes module in exactly one place.
impl From<NoteError> for crate::capture::AnswerError {
    fn from(error: NoteError) -> Self {
        match error {
            NoteError::Empty => Self::Empty,
            NoteError::UnknownOrigin => Self::UnknownOrigin,
            NoteError::Db(error) => Self::Db(error),
            other @ (NoteError::UnknownState | NoteError::NotFound) => {
                Self::Internal(format!("{other:?}"))
            }
        }
    }
}

/// Files a note already linked to a job (`relates job:<id>`), on the caller's connection so the
/// caller's transaction decides whether it stands. The job is not looked up: the caller holds the
/// request that names it.
pub async fn create_linked_to_job_in(
    conn: &mut sqlx::SqliteConnection,
    text: &str,
    origin: &str,
    job_id: i64,
) -> Result<i64, NoteError> {
    let id = insert_note_in(conn, text, origin).await?;
    let at = now();
    let target_ref = job_id.to_string();
    sqlx::query(
        "INSERT INTO owner_note_links (note_id, link_type, target_kind, target_ref, created_at)
         VALUES (?, 'relates', 'job', ?, ?)",
    )
    .bind(id)
    .bind(&target_ref)
    .bind(&at)
    .execute(&mut *conn)
    .await?;
    let detail = format!("relates job:{target_ref}");
    record_raw(conn, id, "linked", Some(&detail), &at).await?;
    Ok(id)
}

/// The shared body of `create`: validates, inserts the note `active` and records `created`.
async fn insert_note_in(
    conn: &mut sqlx::SqliteConnection,
    text: &str,
    origin: &str,
) -> Result<i64, NoteError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(NoteError::Empty);
    }
    if !origin_allowed(origin) {
        return Err(NoteError::UnknownOrigin);
    }
    let at = now();
    let id = sqlx::query(
        "INSERT INTO owner_notes (note_text, origin, state, created_at, updated_at)
         VALUES (?, ?, 'active', ?, ?)",
    )
    .bind(text)
    .bind(origin)
    .bind(&at)
    .bind(&at)
    .execute(&mut *conn)
    .await?
    .last_insert_rowid();
    record(conn, id, "created", None, &at).await?;
    Ok(id)
}

pub async fn get(pool: &SqlitePool, id: i64) -> Result<OwnerNote, NoteError> {
    sqlx::query_as::<_, OwnerNote>(sqlx::AssertSqlSafe(format!(
        "SELECT {NOTE_COLUMNS} FROM owner_notes WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(NoteError::NotFound)
}

/// Newest first; `id` breaks ties between notes written in the same instant.
pub async fn list(pool: &SqlitePool, filter: StateFilter) -> Result<Vec<OwnerNote>, NoteError> {
    let clause = match filter {
        StateFilter::Active => "WHERE state = 'active'",
        StateFilter::Archived => "WHERE state = 'archived'",
        StateFilter::All => "",
    };
    Ok(sqlx::query_as::<_, OwnerNote>(sqlx::AssertSqlSafe(format!(
        "SELECT {NOTE_COLUMNS} FROM owner_notes {clause} ORDER BY created_at DESC, id DESC"
    )))
    .fetch_all(pool)
    .await?)
}

/// Edits the text and/or the state. An edit records the PREVIOUS text in its event; a state change
/// records `archived` or `restored`; a change that changes nothing records nothing.
pub async fn update(
    pool: &SqlitePool,
    id: i64,
    text: Option<&str>,
    state: Option<&str>,
) -> Result<OwnerNote, NoteError> {
    if let Some(state) = state
        && !STATES.contains(&state)
    {
        return Err(NoteError::UnknownState);
    }
    let text = text.map(str::trim);
    if text == Some("") {
        return Err(NoteError::Empty);
    }
    let mut tx = pool.begin().await?;
    let current = sqlx::query_as::<_, OwnerNote>(sqlx::AssertSqlSafe(format!(
        "SELECT {NOTE_COLUMNS} FROM owner_notes WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(NoteError::NotFound)?;

    let at = now();
    let new_text = text.filter(|t| *t != current.note_text);
    let new_state = state.filter(|s| *s != current.state);
    if new_text.is_none() && new_state.is_none() {
        tx.commit().await?;
        return Ok(current);
    }
    sqlx::query("UPDATE owner_notes SET note_text = ?, state = ?, updated_at = ? WHERE id = ?")
        .bind(new_text.unwrap_or(&current.note_text))
        .bind(new_state.unwrap_or(&current.state))
        .bind(&at)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if new_text.is_some() {
        record(&mut tx, id, "edited", Some(&current.note_text), &at).await?;
    }
    if let Some(state) = new_state {
        let kind = if state == "archived" {
            "archived"
        } else {
            "restored"
        };
        record(&mut tx, id, kind, None, &at).await?;
    }
    tx.commit().await?;
    if new_text.is_some() {
        embeddings::nudge();
    }
    get(pool, id).await
}

async fn record(
    tx: &mut sqlx::SqliteConnection,
    note_id: i64,
    kind: &str,
    detail: Option<&str>,
    at: &str,
) -> Result<(), NoteError> {
    Ok(record_raw(tx, note_id, kind, detail, at).await?)
}

async fn record_raw(
    tx: &mut sqlx::SqliteConnection,
    note_id: i64,
    kind: &str,
    detail: Option<&str>,
    at: &str,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO owner_note_events (note_id, kind, detail, at) VALUES (?, ?, ?, ?)")
        .bind(note_id)
        .bind(kind)
        .bind(detail)
        .bind(at)
        .execute(tx)
        .await?;
    Ok(())
}

/// A note's history, oldest first.
pub async fn events(pool: &SqlitePool, id: i64) -> Result<Vec<Event>, NoteError> {
    Ok(sqlx::query_as::<_, Event>(
        "SELECT id, note_id, kind, detail, at FROM owner_note_events WHERE note_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?)
}

/// Full-text search ranked by bm25. The raw words go through `search::fts_query`, so FTS5 syntax in
/// them is quoted into literals; input with no searchable word returns nothing rather than erroring.
pub async fn search(pool: &SqlitePool, raw: &str) -> Result<Vec<OwnerNote>, NoteError> {
    let query = crate::search::fts_query(raw);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_as::<_, OwnerNote>(
        "SELECT n.id, n.note_text, n.origin, n.state, n.created_at, n.updated_at
         FROM owner_notes n JOIN owner_notes_fts ON owner_notes_fts.rowid = n.id
         WHERE owner_notes_fts MATCH ?
         ORDER BY bm25(owner_notes_fts)",
    )
    .bind(query)
    .fetch_all(pool)
    .await?)
}

/// The closed vocabulary of how a note relates to what it points at.
pub const LINK_TYPES: [&str; 5] = [
    "relates",
    "supports",
    "contradicts",
    "details",
    "supersedes",
];
/// What a note can point at. `note`, `knowledge`, `contact`, `mail` and `job` resolve in SQL; `project` and
/// `file` are resolved by the HTTP layer, which owns the roster and the files root.
pub const TARGET_KINDS: [&str; 7] = [
    "note",
    "knowledge",
    "project",
    "contact",
    "mail",
    "file",
    "job",
];

#[derive(Debug)]
pub enum LinkError {
    UnknownType,
    UnknownKind,
    /// `supersedes` only makes sense between two notes.
    SupersedesNeedsNote,
    SelfLink,
    /// A `job` target must be a positive id written canonically.
    BadRef,
    NotFound,
    Duplicate,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for LinkError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownType => formatter.write_str("unknown link type"),
            Self::UnknownKind => formatter.write_str("unknown target kind"),
            Self::SupersedesNeedsNote => formatter.write_str("supersedes can only target a note"),
            Self::SelfLink => formatter.write_str("a note cannot link to itself"),
            Self::BadRef => formatter.write_str("a job target must be a positive job id"),
            Self::NotFound => formatter.write_str("not found"),
            Self::Duplicate => formatter.write_str("that link already exists"),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

/// One typed edge from a note to a target.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Link {
    pub id: i64,
    pub note_id: i64,
    pub link_type: String,
    pub target_kind: String,
    pub target_ref: String,
    pub created_at: String,
}

/// The rules a link must satisfy before anything is written. Pure, so the route and the tests share it.
pub fn link_allowed(
    link_type: &str,
    target_kind: &str,
    note_id: i64,
    target_ref: &str,
) -> Result<(), LinkError> {
    if !LINK_TYPES.contains(&link_type) {
        return Err(LinkError::UnknownType);
    }
    if !TARGET_KINDS.contains(&target_kind) {
        return Err(LinkError::UnknownKind);
    }
    if link_type == "supersedes" && target_kind != "note" {
        return Err(LinkError::SupersedesNeedsNote);
    }
    if target_kind == "note" && target_ref.trim().parse::<i64>() == Ok(note_id) {
        return Err(LinkError::SelfLink);
    }
    // "007" and "+7" would name job 7 and still be a different link row from "7".
    if target_kind == "job" {
        let canonical = target_ref
            .parse::<i64>()
            .is_ok_and(|id| id >= 1 && id.to_string() == target_ref);
        if !canonical {
            return Err(LinkError::BadRef);
        }
    }
    Ok(())
}

/// Adds a link and its `linked` event in one transaction.
pub async fn add_link(
    pool: &SqlitePool,
    note_id: i64,
    link_type: &str,
    target_kind: &str,
    target_ref: &str,
) -> Result<i64, LinkError> {
    link_allowed(link_type, target_kind, note_id, target_ref)?;
    let mut tx = pool.begin().await?;
    let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM owner_notes WHERE id = ?")
        .bind(note_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(LinkError::NotFound);
    }
    let at = now();
    let inserted = sqlx::query(
        "INSERT INTO owner_note_links (note_id, link_type, target_kind, target_ref, created_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(note_id)
    .bind(link_type)
    .bind(target_kind)
    .bind(target_ref)
    .bind(&at)
    .execute(&mut *tx)
    .await;
    let id = match inserted {
        Ok(done) => done.last_insert_rowid(),
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|database| database.is_unique_violation()) =>
        {
            return Err(LinkError::Duplicate);
        }
        Err(error) => return Err(error.into()),
    };
    let detail = format!("{link_type} {target_kind}:{target_ref}");
    record_raw(&mut tx, note_id, "linked", Some(&detail), &at).await?;
    tx.commit().await?;
    Ok(id)
}

#[derive(Debug)]
pub enum TeachError {
    NotFound,
    /// Only an active note can be taught; an archived one was set aside on purpose.
    Archived,
    /// The note already points at a lesson that is still proposed or active.
    AlreadyTaught,
    Propose(crate::knowledge::ProposeError),
    Database(sqlx::Error),
}

impl From<sqlx::Error> for TeachError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<crate::knowledge::ProposeError> for TeachError {
    fn from(error: crate::knowledge::ProposeError) -> Self {
        Self::Propose(error)
    }
}

impl std::fmt::Display for TeachError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("note not found"),
            Self::Archived => formatter.write_str("an archived note cannot be taught"),
            Self::AlreadyTaught => formatter.write_str("this note's lesson is already standing"),
            Self::Propose(error) => write!(formatter, "could not propose: {error}"),
            Self::Database(error) => write!(formatter, "database error: {error}"),
        }
    }
}

/// Maximum length, in characters, of a title derived from a note's first line.
const TAUGHT_TITLE_MAX: usize = 80;

/// The deliberate step that lets a note reach the agent: it becomes a `proposed` lesson owned by the
/// owner (no run behind it), and nothing changes in any prompt until that proposal is approved.
///
/// The proposal, the `relates` link from the note to the lesson and the `taught` event are written
/// in one transaction. Returns `(knowledge_id, proposal_id, link_id)`.
pub async fn teach(
    pool: &SqlitePool,
    note_id: i64,
    kind: crate::knowledge::Kind,
    title: Option<&str>,
    project_id: Option<&str>,
) -> Result<(i64, i64, i64), TeachError> {
    let mut tx = pool.begin().await?;
    let note: Option<(String, String)> =
        sqlx::query_as("SELECT note_text, state FROM owner_notes WHERE id = ?")
            .bind(note_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (text, state) = note.ok_or(TeachError::NotFound)?;
    if state != "active" {
        return Err(TeachError::Archived);
    }
    // Read from the note's own `taught` events, not from its links: a `relates` link the owner
    // drew by hand to some lesson has taught nothing, and unlinking a taught lesson does not
    // unteach it. A rejected or reverted lesson does not count: the owner may teach again.
    let standing = sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM owner_note_events e
         JOIN knowledge k ON e.detail LIKE 'knowledge:' || k.id || ' proposal:%'
         WHERE e.note_id = ? AND e.kind = 'taught'
           AND k.status IN ('proposed', 'active')
         LIMIT 1",
    )
    .bind(note_id)
    .fetch_optional(&mut *tx)
    .await?;
    if standing.is_some() {
        return Err(TeachError::AlreadyTaught);
    }

    let derived: String = text
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(TAUGHT_TITLE_MAX)
        .collect();
    let title = title
        .map(str::trim)
        .filter(|given| !given.is_empty())
        .unwrap_or(&derived);
    let reasoning = format!("taught from owner note #{note_id}");
    let (knowledge_id, proposal_id) = crate::knowledge::propose_in(
        &mut tx,
        crate::knowledge::Declaration {
            project_id,
            origin_run_id: None,
            kind,
            title,
            body: &text,
            reasoning: &reasoning,
            supersedes: None,
        },
    )
    .await?;

    let at = now();
    let target_ref = knowledge_id.to_string();
    let link_id = sqlx::query(
        "INSERT INTO owner_note_links (note_id, link_type, target_kind, target_ref, created_at)
         VALUES (?, 'relates', 'knowledge', ?, ?)",
    )
    .bind(note_id)
    .bind(&target_ref)
    .bind(&at)
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    let linked = format!("relates knowledge:{target_ref}");
    record_raw(&mut tx, note_id, "linked", Some(&linked), &at).await?;
    let taught = format!("knowledge:{knowledge_id} proposal:{proposal_id}");
    record_raw(&mut tx, note_id, "taught", Some(&taught), &at).await?;
    tx.commit().await?;
    // Only after the commit: a waker must never see a proposal the database does not hold yet.
    nucleos_base::wake::bump(nucleos_base::wake::Topic::Proposals);
    Ok((knowledge_id, proposal_id, link_id))
}

/// Deletes a link and records `unlinked` on the note that owned it.
pub async fn remove_link(pool: &SqlitePool, link_id: i64) -> Result<(), LinkError> {
    let mut tx = pool.begin().await?;
    let link = sqlx::query_as::<_, Link>(
        "SELECT id, note_id, link_type, target_kind, target_ref, created_at
         FROM owner_note_links WHERE id = ?",
    )
    .bind(link_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(LinkError::NotFound)?;
    sqlx::query("DELETE FROM owner_note_links WHERE id = ?")
        .bind(link_id)
        .execute(&mut *tx)
        .await?;
    let detail = format!(
        "{} {}:{}",
        link.link_type, link.target_kind, link.target_ref
    );
    record_raw(&mut tx, link.note_id, "unlinked", Some(&detail), &now()).await?;
    tx.commit().await?;
    Ok(())
}

/// What a note points at, oldest link first.
pub async fn links_out(pool: &SqlitePool, note_id: i64) -> sqlx::Result<Vec<Link>> {
    sqlx::query_as::<_, Link>(
        "SELECT id, note_id, link_type, target_kind, target_ref, created_at
         FROM owner_note_links WHERE note_id = ? ORDER BY id",
    )
    .bind(note_id)
    .fetch_all(pool)
    .await
}

/// What points at a target, found through the `(target_kind, target_ref)` index.
pub async fn links_in(pool: &SqlitePool, kind: &str, target_ref: &str) -> sqlx::Result<Vec<Link>> {
    sqlx::query_as::<_, Link>(
        "SELECT id, note_id, link_type, target_kind, target_ref, created_at
         FROM owner_note_links WHERE target_kind = ? AND target_ref = ? ORDER BY id",
    )
    .bind(kind)
    .bind(target_ref)
    .fetch_all(pool)
    .await
}

/// Every link, for the graph. Oldest first.
pub async fn all_links(pool: &SqlitePool) -> sqlx::Result<Vec<Link>> {
    sqlx::query_as::<_, Link>(
        "SELECT id, note_id, link_type, target_kind, target_ref, created_at
         FROM owner_note_links ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

fn sql_table(kind: &str) -> Option<&'static str> {
    match kind {
        "note" => Some("owner_notes"),
        "knowledge" => Some("knowledge"),
        "contact" => Some("contacts"),
        "mail" => Some("emails"),
        "job" => Some("jobs"),
        _ => None,
    }
}

/// The text of every ACTIVE note linked to a project, or to the given job, oldest first and each
/// note once even when it is linked to both.
///
/// This exists solely for the distiller's dossier, under D4 of
/// `.ai/specs/2026-10-05-destilador-design.md` - the one sanctioned exception to the rule that
/// agents never read owner notes. The text is context for the dossier and must never be injected
/// into a prompt run or quoted into knowledge; the only direct note -> knowledge door stays
/// `teach`.
pub async fn active_note_texts_for_project(
    pool: &SqlitePool,
    project_id: &str,
    job_id: Option<i64>,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT n.note_text FROM owner_notes n
          WHERE n.state = 'active'
            AND EXISTS (SELECT 1 FROM owner_note_links l WHERE l.note_id = n.id
                         AND ((l.target_kind = 'project' AND l.target_ref = ?)
                           OR (l.target_kind = 'job' AND l.target_ref = ?)))
          ORDER BY n.id",
    )
    .bind(project_id)
    // `target_ref` is TEXT; a NULL bind never matches.
    .bind(job_id.map(|id| id.to_string()))
    .fetch_all(pool)
    .await
}

/// Vectors of the ACTIVE notes embedded by `model`, by id. Ids and vectors only: the theme clustering
/// (spec 2026-10-22 section 6.2) needs no note text, so none is returned (section 10).
pub async fn vectors_for_clustering(
    pool: &SqlitePool,
    model: &str,
) -> sqlx::Result<Vec<(i64, Vec<f32>)>> {
    let rows: Vec<(i64, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT e.note_id, e.dim, e.vector
           FROM owner_note_embeddings e JOIN owner_notes n ON n.id = e.note_id
          WHERE e.model = ? AND n.state = 'active' ORDER BY e.note_id",
    )
    .bind(model)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, dim, blob)| crate::embed::decode(&blob, dim).map(|v| (id, v)))
        .collect())
}

/// Text of the ACTIVE notes among `ids`, by id. For the two sanctioned readers of spec 2026-10-22
/// section 10 only: the theme synthesis (`themes.rs`) and the generalisation pass (`generalise.rs`),
/// as owner context that is never quoted and never injected into an agent's prompt.
pub async fn texts_for_ids(pool: &SqlitePool, ids: &[i64]) -> sqlx::Result<Vec<(i64, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let marks = vec!["?"; ids.len()].join(", ");
    // `AssertSqlSafe`, audited: only literal `?` placeholders are interpolated, as in
    // `embed::vectors_for`.
    let sql = sqlx::AssertSqlSafe(format!(
        "SELECT id, note_text FROM owner_notes WHERE state = 'active' AND id IN ({marks}) ORDER BY id"
    ));
    let mut q = sqlx::query_as(sql);
    for id in ids {
        q = q.bind(id);
    }
    q.fetch_all(pool).await
}

/// Active notes with cosine >= `floor` to `vector` (same `model`), best first, ties on the lower id,
/// at most `limit`. Ids and cosines only: a caller that needs the text asks `texts_for_ids`.
pub async fn nearest_to_vector(
    pool: &SqlitePool,
    model: &str,
    vector: &[f32],
    floor: f32,
    limit: usize,
) -> sqlx::Result<Vec<(i64, f32)>> {
    let mut scored: Vec<(i64, f32)> = vectors_for_clustering(pool, model)
        .await?
        .into_iter()
        .filter_map(|(id, v)| {
            crate::embed::cosine(vector, &v)
                .filter(|c| *c >= floor)
                .map(|c| (id, c))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(limit);
    Ok(scored)
}

/// Whether a target exists, for the kinds this module can answer from SQL. `None` for `project` and
/// `file` (and anything unknown): the HTTP layer resolves those.
pub async fn target_exists(
    pool: &SqlitePool,
    kind: &str,
    target_ref: &str,
) -> sqlx::Result<Option<bool>> {
    let Some(table) = sql_table(kind) else {
        return Ok(None);
    };
    let Ok(id) = target_ref.trim().parse::<i64>() else {
        return Ok(Some(false));
    };
    // `AssertSqlSafe`, audited: `table` is one of the literals from `sql_table`.
    let found = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "SELECT 1 FROM {table} WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(Some(found.is_some()))
}

/// A link target as the graph shows it. `label` is `None` when the target has no name to show;
/// `missing` is true when the row it pointed at is gone.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct Target {
    pub kind: String,
    pub r#ref: String,
    pub label: Option<String>,
    pub missing: bool,
}

/// Labels for the SQL kinds: a note's first line (at most 80 characters), a knowledge title, a
/// contact's display name, a mail subject. Other kinds come back unlabelled and not missing, for the
/// caller to resolve. The labels are never logged.
///
/// A database error is returned, not read as "gone": a failed lookup would otherwise draw every
/// target in the graph as missing.
pub async fn resolve_sql_labels(
    pool: &SqlitePool,
    wanted: &[(&str, &str)],
) -> sqlx::Result<Vec<Target>> {
    let mut targets = Vec::with_capacity(wanted.len());
    for (kind, target_ref) in wanted {
        let mut target = Target {
            kind: (*kind).to_string(),
            r#ref: (*target_ref).to_string(),
            label: None,
            missing: false,
        };
        let column = match *kind {
            "note" => Some("note_text"),
            "knowledge" => Some("title"),
            "contact" => Some("display_name"),
            "mail" => Some("subject"),
            _ => None,
        };
        if let (Some(column), Some(table)) = (column, sql_table(kind)) {
            let row = match target_ref.trim().parse::<i64>() {
                // `AssertSqlSafe`, audited: table and column come from the literals above.
                Ok(id) => {
                    sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(format!(
                        "SELECT {column} FROM {table} WHERE id = ?"
                    )))
                    .bind(id)
                    .fetch_optional(pool)
                    .await?
                }
                Err(_) => None,
            };
            match row {
                None => target.missing = true,
                Some(text) => {
                    target.label = text.map(|text| {
                        text.lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(80)
                            .collect::<String>()
                    });
                }
            }
        }
        targets.push(target);
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_scan::production_code;

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    #[tokio::test]
    async fn teaching_creates_one_owner_proposal_and_links_it() {
        let pool = test_pool().await;
        let id = create(
            &pool,
            "Deploys go out on Tuesdays\nnever on Fridays",
            "shell",
        )
        .await
        .unwrap();

        let (knowledge_id, proposal_id, link_id) = teach(
            &pool,
            id,
            crate::knowledge::Kind::Memory,
            None,
            Some("mine"),
        )
        .await
        .unwrap();

        let (source, status, title, body, proposal): (String, String, String, String, i64) =
            sqlx::query_as(
                "SELECT source, status, title, body, proposal_id FROM knowledge WHERE id = ?",
            )
            .bind(knowledge_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(source, "owner");
        assert_eq!(status, "proposed");
        assert_eq!(
            title, "Deploys go out on Tuesdays",
            "the title is the first line"
        );
        assert_eq!(body, "Deploys go out on Tuesdays\nnever on Fridays");
        assert_eq!(proposal, proposal_id);

        let (status, run_id, reasoning): (String, Option<i64>, String) =
            sqlx::query_as("SELECT status, run_id, reasoning FROM proposals WHERE id = ?")
                .bind(proposal_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(run_id, None);
        assert_eq!(reasoning, format!("taught from owner note #{id}"));
        let proposals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proposals")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(proposals, 1);

        let links = links_out(&pool, id).await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].id, link_id);
        assert_eq!(links[0].link_type, "relates");
        assert_eq!(links[0].target_kind, "knowledge");
        assert_eq!(links[0].target_ref, knowledge_id.to_string());

        let kinds: Vec<String> = events(&pool, id)
            .await
            .unwrap()
            .into_iter()
            .map(|event| event.kind)
            .collect();
        assert_eq!(kinds, ["created", "linked", "taught"]);
    }

    #[tokio::test]
    async fn only_the_notes_own_teaching_counts_as_taught_not_a_hand_made_link() {
        let pool = test_pool().await;
        let first = create(&pool, "the lesson someone else taught", "shell")
            .await
            .unwrap();
        let (lesson, _, teach_link) =
            teach(&pool, first, crate::knowledge::Kind::Memory, None, None)
                .await
                .unwrap();

        // A note the owner merely relates to that lesson by hand has taught nothing.
        let second = create(&pool, "a note about that lesson", "shell")
            .await
            .unwrap();
        add_link(&pool, second, "relates", "knowledge", &lesson.to_string())
            .await
            .unwrap();
        teach(&pool, second, crate::knowledge::Kind::Memory, None, None)
            .await
            .unwrap();

        // And unlinking the taught lesson does not reopen the note while the lesson still stands.
        remove_link(&pool, teach_link).await.unwrap();
        let again = teach(&pool, first, crate::knowledge::Kind::Memory, None, None).await;
        assert!(matches!(again, Err(TeachError::AlreadyTaught)), "{again:?}");
    }

    #[tokio::test]
    async fn a_note_cannot_be_taught_twice_while_its_lesson_stands() {
        let pool = test_pool().await;
        let id = create(&pool, "one lesson", "shell").await.unwrap();
        let (_, proposal_id, _) = teach(&pool, id, crate::knowledge::Kind::Memory, None, None)
            .await
            .unwrap();

        let again = teach(&pool, id, crate::knowledge::Kind::Memory, None, None).await;
        assert!(matches!(again, Err(TeachError::AlreadyTaught)), "{again:?}");

        // Once the lesson is rejected it no longer stands, and the note may be taught again.
        crate::knowledge::reject(&pool, proposal_id).await.unwrap();
        teach(
            &pool,
            id,
            crate::knowledge::Kind::Memory,
            Some("retry"),
            None,
        )
        .await
        .unwrap();

        let missing = teach(&pool, 9_999, crate::knowledge::Kind::Memory, None, None).await;
        assert!(matches!(missing, Err(TeachError::NotFound)), "{missing:?}");
        let archived = create(&pool, "set aside", "shell").await.unwrap();
        update(&pool, archived, None, Some("archived"))
            .await
            .unwrap();
        let refused = teach(&pool, archived, crate::knowledge::Kind::Memory, None, None).await;
        assert!(matches!(refused, Err(TeachError::Archived)), "{refused:?}");
    }

    #[tokio::test]
    async fn a_note_is_created_active_with_its_origin_and_a_created_event() {
        let pool = test_pool().await;
        let id = create(&pool, "  remember the milk  ", "channel:telegram")
            .await
            .unwrap();

        let note = get(&pool, id).await.unwrap();
        assert_eq!(note.id, id);
        assert_eq!(note.note_text, "remember the milk", "the text is trimmed");
        assert_eq!(note.origin, "channel:telegram");
        assert_eq!(note.state, "active");
        assert!(!note.created_at.is_empty());
        assert_eq!(note.created_at, note.updated_at);

        let events = events(&pool, id).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].note_id, id);
        assert_eq!(events[0].kind, "created");
        assert!(!events[0].at.is_empty());

        let json = serde_json::to_value(&note).unwrap();
        assert_eq!(
            json["text"], "remember the milk",
            "note_text serialises as `text`"
        );
    }

    #[tokio::test]
    async fn an_empty_note_or_unknown_origin_is_refused() {
        let pool = test_pool().await;
        assert!(matches!(
            create(&pool, "", "shell").await,
            Err(NoteError::Empty)
        ));
        assert!(matches!(
            create(&pool, "  \n\t ", "shell").await,
            Err(NoteError::Empty)
        ));
        assert!(matches!(
            create(&pool, "real text", "carrier-pigeon").await,
            Err(NoteError::UnknownOrigin)
        ));
        assert!(
            list(&pool, StateFilter::All).await.unwrap().is_empty(),
            "a refused note leaves no row behind"
        );
    }

    #[tokio::test]
    async fn an_edit_keeps_the_previous_text_in_history() {
        let pool = test_pool().await;
        let id = create(&pool, "first draft", "shell").await.unwrap();

        let edited = update(&pool, id, Some("second draft"), None).await.unwrap();
        assert_eq!(edited.note_text, "second draft");

        let events = events(&pool, id).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "created");
        assert_eq!(events[1].kind, "edited");
        assert_eq!(events[1].detail.as_deref(), Some("first draft"));

        // Writing the same text again changes nothing and records nothing.
        update(&pool, id, Some("second draft"), None).await.unwrap();
        assert_eq!(super::events(&pool, id).await.unwrap().len(), 2);

        assert!(matches!(
            update(&pool, 9999, Some("x"), None).await,
            Err(NoteError::NotFound)
        ));
    }

    #[tokio::test]
    async fn archive_and_restore_are_events_and_filter_the_list() {
        let pool = test_pool().await;
        let keep = create(&pool, "stays active", "shell").await.unwrap();
        let gone = create(&pool, "goes away", "shell").await.unwrap();

        update(&pool, gone, None, Some("archived")).await.unwrap();
        let active = list(&pool, StateFilter::Active).await.unwrap();
        assert_eq!(active.iter().map(|n| n.id).collect::<Vec<_>>(), vec![keep]);
        let archived = list(&pool, StateFilter::Archived).await.unwrap();
        assert_eq!(
            archived.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![gone]
        );
        assert_eq!(list(&pool, StateFilter::All).await.unwrap().len(), 2);

        update(&pool, gone, None, Some("active")).await.unwrap();
        assert_eq!(list(&pool, StateFilter::Active).await.unwrap().len(), 2);
        assert!(list(&pool, StateFilter::Archived).await.unwrap().is_empty());

        let kinds: Vec<String> = events(&pool, gone)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, vec!["created", "archived", "restored"]);

        // Setting the state it already has writes no event.
        update(&pool, gone, None, Some("active")).await.unwrap();
        assert_eq!(events(&pool, gone).await.unwrap().len(), 3);

        assert!(matches!(
            update(&pool, gone, None, Some("deleted")).await,
            Err(NoteError::UnknownState)
        ));
    }

    #[tokio::test]
    async fn search_finds_a_note_by_a_word_and_survives_fts_syntax() {
        let pool = test_pool().await;
        let hit = create(&pool, "the invoice from the plumber", "shell")
            .await
            .unwrap();
        create(&pool, "unrelated thought", "shell").await.unwrap();

        let found = search(&pool, "plumber").await.unwrap();
        assert_eq!(found.iter().map(|n| n.id).collect::<Vec<_>>(), vec![hit]);

        // FTS5 operators and stray quotes in the raw input must not become a syntax error.
        for hostile in ["foo\" OR (", "\"", "AND", "(((", "plumber*  NEAR("] {
            assert!(
                search(&pool, hostile).await.is_ok(),
                "raw input {hostile:?} must not error"
            );
        }
        assert!(search(&pool, "   ").await.unwrap().is_empty());
    }

    #[test]
    fn the_link_vocabulary_is_closed_and_supersedes_targets_notes_only() {
        assert!(matches!(
            link_allowed("befriends", "note", 1, "2"),
            Err(LinkError::UnknownType)
        ));
        assert!(matches!(
            link_allowed("relates", "planet", 1, "2"),
            Err(LinkError::UnknownKind)
        ));
        for link_type in LINK_TYPES {
            assert!(link_allowed(link_type, "note", 1, "2").is_ok());
        }
        for kind in TARGET_KINDS {
            assert!(link_allowed("relates", kind, 1, "2").is_ok());
        }
        for kind in ["knowledge", "project", "contact", "mail", "file", "job"] {
            assert!(
                matches!(
                    link_allowed("supersedes", kind, 1, "2"),
                    Err(LinkError::SupersedesNeedsNote)
                ),
                "supersedes must refuse {kind}"
            );
        }
    }

    #[test]
    fn a_job_target_must_be_written_canonically() {
        assert!(link_allowed("relates", "job", 1, "7").is_ok());
        for bad in ["007", "+7", "0", "-3", "", " 7", "7 ", "x"] {
            assert!(
                matches!(
                    link_allowed("relates", "job", 1, bad),
                    Err(LinkError::BadRef)
                ),
                "{bad:?} must be refused"
            );
        }
        // Other kinds keep taking free-form refs.
        assert!(link_allowed("relates", "file", 1, "007").is_ok());
    }

    #[tokio::test]
    async fn a_note_cannot_link_to_itself() {
        let pool = test_pool().await;
        let id = create(&pool, "alone", "shell").await.unwrap();
        assert!(matches!(
            add_link(&pool, id, "relates", "note", &id.to_string()).await,
            Err(LinkError::SelfLink)
        ));
        // The same number as a different kind of target is not a self link.
        assert!(
            add_link(&pool, id, "relates", "contact", &id.to_string())
                .await
                .is_ok()
        );
        assert!(matches!(
            add_link(&pool, 9999, "relates", "contact", "1").await,
            Err(LinkError::NotFound)
        ));
    }

    #[tokio::test]
    async fn a_duplicate_link_is_refused_and_removal_is_an_event() {
        let pool = test_pool().await;
        let id = create(&pool, "linker", "shell").await.unwrap();
        let other = create(&pool, "linked", "shell").await.unwrap();
        let target = other.to_string();

        let link = add_link(&pool, id, "supports", "note", &target)
            .await
            .unwrap();
        assert!(matches!(
            add_link(&pool, id, "supports", "note", &target).await,
            Err(LinkError::Duplicate)
        ));
        // A different type to the same target is a different edge.
        add_link(&pool, id, "relates", "note", &target)
            .await
            .unwrap();
        assert_eq!(links_out(&pool, id).await.unwrap().len(), 2);

        remove_link(&pool, link).await.unwrap();
        assert_eq!(links_out(&pool, id).await.unwrap().len(), 1);
        assert!(matches!(
            remove_link(&pool, link).await,
            Err(LinkError::NotFound)
        ));

        let history = events(&pool, id).await.unwrap();
        let kinds: Vec<&str> = history.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, vec!["created", "linked", "linked", "unlinked"]);
        let expected = format!("supports note:{target}");
        assert_eq!(history[1].detail.as_deref(), Some(expected.as_str()));
        assert_eq!(history[3].detail.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn incoming_links_are_found_by_target() {
        let pool = test_pool().await;
        let first = create(&pool, "first", "shell").await.unwrap();
        let second = create(&pool, "second", "shell").await.unwrap();
        add_link(&pool, first, "relates", "contact", "42")
            .await
            .unwrap();
        add_link(&pool, second, "details", "contact", "42")
            .await
            .unwrap();
        add_link(&pool, second, "relates", "contact", "43")
            .await
            .unwrap();

        let incoming = links_in(&pool, "contact", "42").await.unwrap();
        assert_eq!(
            incoming.iter().map(|l| l.note_id).collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(links_in(&pool, "contact", "44").await.unwrap().is_empty());
        assert!(links_in(&pool, "mail", "42").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_missing_sql_target_resolves_as_missing() {
        let pool = test_pool().await;
        let note = create(&pool, "first line of the note\nsecond line", "shell")
            .await
            .unwrap();
        let contact =
            sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES ('Ada', 'now')")
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid();
        let contact_ref = contact.to_string();
        let note_ref = note.to_string();

        assert_eq!(
            target_exists(&pool, "contact", &contact_ref).await.unwrap(),
            Some(true)
        );
        assert_eq!(
            target_exists(&pool, "contact", "9999").await.unwrap(),
            Some(false)
        );
        assert_eq!(
            target_exists(&pool, "mail", "9999").await.unwrap(),
            Some(false)
        );
        assert_eq!(
            target_exists(&pool, "knowledge", "not-a-number")
                .await
                .unwrap(),
            Some(false)
        );
        assert_eq!(target_exists(&pool, "project", "x").await.unwrap(), None);
        assert_eq!(target_exists(&pool, "file", "a.txt").await.unwrap(), None);

        let targets = resolve_sql_labels(
            &pool,
            &[
                ("note", note_ref.as_str()),
                ("contact", contact_ref.as_str()),
                ("mail", "9999"),
                ("project", "p"),
            ],
        )
        .await
        .unwrap();
        assert_eq!(targets[0].label.as_deref(), Some("first line of the note"));
        assert!(!targets[0].missing);
        assert_eq!(targets[1].label.as_deref(), Some("Ada"));
        assert!(targets[2].missing);
        assert_eq!(targets[2].label, None);
        assert!(
            !targets[3].missing,
            "a kind this module cannot resolve is not called missing"
        );
        assert_eq!(serde_json::to_value(&targets[0]).unwrap()["ref"], note_ref);
    }

    /// `distill.rs` may name the notes module in exactly one place: the call that reads a project's
    /// notes for the dossier (spec `2026-10-07-fronteira-prompts-design.md` section 5.2). Production
    /// code only - the file is cut at its `#[cfg(test)] mod tests`, since its tests must create notes -
    /// and `//` comment lines are skipped.
    fn distill_mentions_are_sanctioned(source: &str) -> bool {
        let needle = ["owner", "note"].join("_");
        one_call_is_sanctioned(source, &format!("{needle}s::active_note_texts_for_project"))
    }

    /// `capture.rs` may name the notes module in exactly one place: the call that files the owner's
    /// answer as a note. A channel capture answer is the owner writing a note; `capture.rs` only
    /// forwards it and never reads notes back.
    fn capture_mentions_are_sanctioned(source: &str) -> bool {
        let needle = ["owner", "note"].join("_");
        one_call_is_sanctioned(source, &format!("{needle}s::create_linked_to_job_in"))
    }

    /// `channels/commands.rs` may name the notes module in exactly one place: the call `/note` uses
    /// to file the text. `/note` is the owner writing a note from a channel; `commands.rs` only
    /// forwards it and never reads notes back.
    fn commands_mentions_are_sanctioned(source: &str) -> bool {
        let needle = ["owner", "note"].join("_");
        one_call_is_sanctioned(source, &format!("{needle}s::create("))
    }

    /// `themes.rs` may name the notes module in exactly two places: the call that reads the vectors of
    /// the active notes for the theme grouping, and the call that reads the text of the member notes
    /// for the synthesis of an accepted theme. Both are owner-only readers (spec 2026-10-22
    /// section 10): grouping needs vectors, never text; the synthesis text never leaves the Brain.
    fn themes_mentions_are_sanctioned(source: &str) -> bool {
        let needle = ["owner", "note"].join("_");
        calls_are_sanctioned(
            source,
            &[
                &format!("{needle}s::vectors_for_clustering"),
                &format!("{needle}s::texts_for_ids"),
            ],
        )
    }

    /// `distill/generalise.rs` may name the notes module in exactly two places: the nearest-notes lookup
    /// (ids and cosines) and the text of those notes as owner context (spec 2026-10-22 section 10).
    fn generalise_mentions_are_sanctioned(source: &str) -> bool {
        let needle = ["owner", "note"].join("_");
        calls_are_sanctioned(
            source,
            &[
                &format!("{needle}s::nearest_to_vector"),
                &format!("{needle}s::texts_for_ids"),
            ],
        )
    }

    /// True when the production code of `source` names the notes module only inside the listed
    /// `calls`, and each of them exactly once.
    fn calls_are_sanctioned(source: &str, calls: &[&str]) -> bool {
        let needle = ["owner", "note"].join("_");
        let mut seen = vec![0usize; calls.len()];
        for line in production_code(source).lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let mut here = 0;
            for (i, call) in calls.iter().enumerate() {
                let n = line.matches(call).count();
                seen[i] += n;
                here += n;
            }
            if line.matches(needle.as_str()).count() != here {
                return false;
            }
        }
        seen.iter().all(|n| *n == 1)
    }

    fn one_call_is_sanctioned(source: &str, call: &str) -> bool {
        calls_are_sanctioned(source, &[call])
    }

    #[tokio::test]
    async fn texts_for_ids_returns_active_notes_only_in_id_order() {
        let pool = test_pool().await;
        let a = create(&pool, "alpha", "shell").await.unwrap();
        let b = create(&pool, "beta", "shell").await.unwrap();
        sqlx::query("UPDATE owner_notes SET state='archived' WHERE id = ?")
            .bind(b)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            texts_for_ids(&pool, &[b, a, 9999]).await.unwrap(),
            vec![(a, "alpha".to_string())]
        );
        assert!(texts_for_ids(&pool, &[]).await.unwrap().is_empty());
    }

    async fn embedded_note(pool: &sqlx::SqlitePool, text: &str, model: &str, v: [f32; 2]) -> i64 {
        let id = create(pool, text, "shell").await.unwrap();
        sqlx::query(
            "INSERT INTO owner_note_embeddings (note_id, model, dim, vector, text_hash, created_at)
             VALUES (?, ?, 2, ?, 'h', 't')",
        )
        .bind(id)
        .bind(model)
        .bind(crate::embed::encode(&v))
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn nearest_to_vector_gives_ids_and_cosines_above_the_floor_capped_and_ordered() {
        let pool = test_pool().await;
        let near = embedded_note(&pool, "n", "m", [1.0, 0.1]).await;
        let nearer = embedded_note(&pool, "n2", "m", [1.0, 0.0]).await;
        let _far = embedded_note(&pool, "f", "m", [0.0, 1.0]).await;
        let _other_model = embedded_note(&pool, "o", "x", [1.0, 0.0]).await;
        let got = nearest_to_vector(&pool, "m", &[1.0, 0.0], 0.80, 5)
            .await
            .unwrap();
        assert_eq!(
            got.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![nearer, near]
        );
        assert!(got[0].1 > got[1].1);
        assert_eq!(
            nearest_to_vector(&pool, "m", &[1.0, 0.0], 0.80, 1)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn clustering_vectors_are_the_active_notes_of_one_model_and_carry_no_text() {
        let pool = test_pool().await;
        let live = create(&pool, "live", "shell").await.unwrap();
        let archived = create(&pool, "gone", "shell").await.unwrap();
        let other = create(&pool, "other model", "shell").await.unwrap();
        sqlx::query("UPDATE owner_notes SET state = 'archived' WHERE id = ?")
            .bind(archived)
            .execute(&pool)
            .await
            .unwrap();
        for (id, model, v) in [
            (live, "m1", [1.0_f32, 0.0]),
            (archived, "m1", [0.0, 1.0]),
            (other, "m2", [1.0, 1.0]),
        ] {
            sqlx::query(
                "INSERT INTO owner_note_embeddings (note_id, model, dim, vector, text_hash, created_at)
                 VALUES (?, ?, 2, ?, 'h', 't')",
            )
            .bind(id)
            .bind(model)
            .bind(crate::embed::encode(&v))
            .execute(&pool)
            .await
            .unwrap();
        }
        let got = vectors_for_clustering(&pool, "m1").await.unwrap();
        assert_eq!(got, vec![(live, vec![1.0_f32, 0.0])]);
    }

    #[test]
    fn calls_are_sanctioned_takes_a_list_and_counts_each_call_once() {
        let needle = ["owner", "note"].join("_");
        let a = format!("let v = {needle}s::vectors_for_clustering(&pool, m).await;");
        let b = format!("let t = {needle}s::texts_for_ids(&pool, &ids).await;");
        let calls = [
            format!("{needle}s::vectors_for_clustering"),
            format!("{needle}s::texts_for_ids"),
        ];
        let calls: Vec<&str> = calls.iter().map(String::as_str).collect();
        assert!(calls_are_sanctioned(&format!("{a}
{b}"), &calls));
        assert!(
            !calls_are_sanctioned(&a, &calls),
            "every listed call must occur once"
        );
        assert!(!calls_are_sanctioned(&format!("{a}
{a}
{b}"), &calls));
        assert!(!calls_are_sanctioned(
            &format!("{a}
{b}
use crate::{needle}s;"),
            &calls
        ));
        assert!(calls_are_sanctioned(
            &format!("{a}
{b}
// mentions {needle}s"),
            &calls
        ));
        // The one-call helper keeps its meaning.
        assert!(one_call_is_sanctioned(&a, calls[0]));
    }

    #[test]
    fn the_distill_rule_holds_one_call_and_nothing_else() {
        let needle = ["owner", "note"].join("_");
        let call = format!("let t = {needle}s::active_note_texts_for_project(&pool, p).await;");
        assert!(distill_mentions_are_sanctioned(&call));
        assert!(!distill_mentions_are_sanctioned(&format!("{call}\n{call}")));
        assert!(!distill_mentions_are_sanctioned(&format!(
            "use crate::{needle}s;\n{call}"
        )));
        assert!(!distill_mentions_are_sanctioned("fn nothing() {}"));
        assert!(distill_mentions_are_sanctioned(&format!(
            "{call}\n// mentions {needle}s"
        )));
        assert!(distill_mentions_are_sanctioned(&format!(
            "{call}\n#[cfg(test)]\nmod tests {{\n{needle}s::create(&pool, \"x\", \"y\");\n}}"
        )));
        // A test-only helper above production code does not end the sweep.
        assert!(!distill_mentions_are_sanctioned(&format!(
            "#[cfg(test)]\nfn helper() {{}}\n{call}\nuse crate::{needle}s;"
        )));
    }

    #[test]
    fn the_themes_rule_holds_its_two_calls_and_nothing_else() {
        let n = ["owner", "note"].join("_");
        let vecs = format!("let v = {n}s::vectors_for_clustering(&pool, m).await;");
        let texts = format!("let t = {n}s::texts_for_ids(&pool, &ids).await?;");
        assert!(themes_mentions_are_sanctioned(&format!("{vecs}\n{texts}")));
        assert!(!themes_mentions_are_sanctioned(&vecs), "both calls must occur");
        assert!(!themes_mentions_are_sanctioned(&texts), "both calls must occur");
        assert!(!themes_mentions_are_sanctioned(&format!("{vecs}\n{texts}\n{texts}")));
        assert!(!themes_mentions_are_sanctioned("fn nothing() {}"));
        assert!(!themes_mentions_are_sanctioned(&format!(
            "use crate::{n}s;\n{vecs}\n{texts}"
        )));
        assert!(!themes_mentions_are_sanctioned(&format!(
            "{vecs}\n{texts}\nlet x = {n}s::list(&pool, f).await;"
        )));
        assert!(themes_mentions_are_sanctioned(&format!(
            "{vecs}\n{texts}\n// mentions {n}s"
        )));
        assert!(themes_mentions_are_sanctioned(&format!(
            "{vecs}\n{texts}\n#[cfg(test)]\nmod tests {{\n{n}s::create(&pool, \"x\", \"y\");\n}}"
        )));
        // A test-only helper above production code does not end the sweep.
        assert!(!themes_mentions_are_sanctioned(&format!(
            "#[cfg(test)]\nfn helper() {{}}\n{vecs}\n{texts}\nuse crate::{n}s;"
        )));
    }

    #[test]
    fn the_generalise_rule_holds_its_two_calls_and_nothing_else() {
        let n = ["owner", "note"].join("_");
        let near = format!("let ids = {n}s::nearest_to_vector(&pool, m, &c, 0.80, 5).await?;");
        let texts = format!("let t = {n}s::texts_for_ids(&pool, &ids).await?;");
        assert!(generalise_mentions_are_sanctioned(&format!("{near}\n{texts}")));
        assert!(!generalise_mentions_are_sanctioned(&near), "both calls must occur");
        assert!(!generalise_mentions_are_sanctioned(&format!("{near}\n{texts}\n{texts}")));
        assert!(!generalise_mentions_are_sanctioned(&format!(
            "use crate::{n}s;\n{near}\n{texts}"
        )));
        assert!(!generalise_mentions_are_sanctioned(&format!(
            "{near}\n{texts}\nlet x = {n}s::list(&pool, f).await;"
        )));
        assert!(generalise_mentions_are_sanctioned(&format!(
            "{near}\n{texts}\n#[cfg(test)]\nmod tests {{\n{n}s::create(&pool, \"x\", \"y\");\n}}"
        )));
        assert!(!generalise_mentions_are_sanctioned(&format!(
            "#[cfg(test)]\nfn helper() {{}}\n{near}\n{texts}\nuse crate::{n}s;"
        )));
    }

    #[test]
    fn the_capture_rule_holds_one_call_and_nothing_else() {
        let needle = ["owner", "note"].join("_");
        let call =
            format!("let id = crate::{needle}s::create_linked_to_job_in(&mut tx, t, o, j).await?;");
        assert!(capture_mentions_are_sanctioned(&call));
        assert!(!capture_mentions_are_sanctioned(&format!("{call}\n{call}")));
        assert!(!capture_mentions_are_sanctioned(&format!(
            "use crate::{needle}s;\n{call}"
        )));
        assert!(!capture_mentions_are_sanctioned(&format!(
            "{call}\nlet n = crate::{needle}s::get(&pool, 1).await;"
        )));
        assert!(!capture_mentions_are_sanctioned("fn nothing() {}"));
        assert!(capture_mentions_are_sanctioned(&format!(
            "{call}\n// mentions {needle}s"
        )));
    }

    #[test]
    fn the_commands_rule_holds_one_call_and_nothing_else() {
        let needle = ["owner", "note"].join("_");
        let call = format!("match crate::{needle}s::create(&state.pool, text, origin).await {{}}");
        assert!(commands_mentions_are_sanctioned(&call));
        assert!(!commands_mentions_are_sanctioned(&format!(
            "{call}\n{call}"
        )));
        assert!(!commands_mentions_are_sanctioned(&format!(
            "use crate::{needle}s;\n{call}"
        )));
        assert!(!commands_mentions_are_sanctioned(&format!(
            "{call}\nlet n = crate::{needle}s::get(&pool, 1).await;"
        )));
        assert!(!commands_mentions_are_sanctioned("fn nothing() {}"));
        assert!(commands_mentions_are_sanctioned(&format!(
            "{call}\n// mentions {needle}s"
        )));
        assert!(commands_mentions_are_sanctioned(&format!(
            "{call}\n#[cfg(test)]\nmod tests {{\nSELECT 1 FROM {needle}s;\n}}"
        )));
    }

    #[test]
    fn the_generic_sweep_ignores_test_modules_but_not_production_code() {
        let needle = ["owner", "note"].join("_");
        let only_in_tests =
            format!("fn run() {{}}\n#[cfg(test)]\nmod tests {{\nJOIN {needle}s;\n}}");
        assert!(!production_code(&only_in_tests).contains(&needle));
        let in_production = format!("use crate::{needle}s;\n#[cfg(test)]\nmod tests {{\n}}");
        assert!(production_code(&in_production).contains(&needle));
        // A test-only helper above production code does not end the sweep.
        let helper_first = format!("#[cfg(test)]\nfn helper() {{}}\nuse crate::{needle}s;");
        assert!(production_code(&helper_first).contains(&needle));
    }

    /// Whether a `source_scan::module_path` names a file below this module's own directory. The
    /// sweep skips these by that prefix and nothing else.
    fn is_owner_notes_submodule(module_path: &str) -> bool {
        module_path.starts_with("owner_notes/")
    }

    /// Whether `path` is one of this module's own submodule files: below `core/src/owner_notes/`,
    /// and not below the same-named folder of another crate's source root (`module_path` strips
    /// any root, so the prefix alone cannot tell them apart).
    fn is_owner_notes_submodule_file(
        path: &std::path::Path,
        core_src: &std::path::Path,
        module_path: &str,
    ) -> bool {
        path.starts_with(core_src) && is_owner_notes_submodule(module_path)
    }

    #[test]
    fn the_sweep_skips_owner_notes_submodules_of_the_core_src_root_only() {
        let core_src = std::path::Path::new("/repo/core/src");
        let skipped = |path: &str, module_path: &str| {
            is_owner_notes_submodule_file(std::path::Path::new(path), core_src, module_path)
        };
        assert!(skipped(
            "/repo/core/src/owner_notes/embeddings.rs",
            "owner_notes/embeddings.rs"
        ));
        // Another crate's `owner_notes/` folder has the same module path and is scanned.
        assert!(!skipped(
            "/repo/core/crates/x/src/owner_notes/a.rs",
            "owner_notes/a.rs"
        ));
        assert!(!skipped("/repo/core/src/brief.rs", "brief.rs"));
    }

    /// The first line of `source`'s production code that names a `runner`, `prompt` or `distill`
    /// module path: the word followed by `::`, or the word as an item of a `use` or `mod` line.
    /// Comment lines and the test module are not scanned.
    fn names_a_forbidden_module(source: &str) -> Option<String> {
        const FORBIDDEN: [&str; 3] = ["runner", "prompt", "distill"];
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        for line in production_code(source).lines() {
            let line = line.trim();
            if line.starts_with("//") {
                continue;
            }
            let declares = [
                "use ",
                "pub use ",
                "pub(crate) use ",
                "mod ",
                "pub mod ",
                "pub(crate) mod ",
            ]
            .iter()
            .any(|head| line.starts_with(head));
            for word in FORBIDDEN {
                for (at, _) in line.match_indices(word) {
                    let before = line[..at].chars().next_back();
                    let after = line[at + word.len()..].chars().next();
                    if before.is_some_and(is_ident) || after.is_some_and(is_ident) {
                        continue;
                    }
                    if line[at + word.len()..].starts_with("::") || declares {
                        return Some(line.to_string());
                    }
                }
            }
        }
        None
    }

    #[test]
    fn the_forbidden_module_scan_flags_paths_and_declarations_only() {
        assert!(names_a_forbidden_module("use crate::runner::Run;").is_some());
        assert!(names_a_forbidden_module("let p = crate::prompt::build();").is_some());
        assert!(names_a_forbidden_module("use crate::{distill, db};").is_some());
        assert!(names_a_forbidden_module("pub(crate) mod runner;").is_some());
        // Words inside identifiers, prose in comments and the test module are not paths.
        assert!(names_a_forbidden_module("let prompt_text = 1;").is_none());
        assert!(names_a_forbidden_module("// the prompt::x is never named").is_none());
        assert!(names_a_forbidden_module("fn distiller() {}").is_none());
        let in_tests = "fn a() {}\n#[cfg(test)]\nmod tests {\nuse crate::runner::R;\n}";
        assert!(names_a_forbidden_module(in_tests).is_none());
    }

    #[test]
    fn owner_notes_submodules_name_no_runner_prompt_or_distill_module() {
        let built_in = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory must be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "built and running checkouts differ"
        );
        let dir = built_in.join("src").join("owner_notes");
        let mut scanned = 0;
        for path in crate::source_scan::rust_files() {
            if !path.starts_with(&dir) || crate::source_scan::is_test_only_module(&path) {
                continue;
            }
            scanned += 1;
            let source = std::fs::read_to_string(&path).expect("source file must be readable");
            if let Some(line) = names_a_forbidden_module(&source) {
                panic!(
                    "{} names a runner, prompt or distill module (`{line}`): the notes module \
                     must stay out of the agent's run and prompt path",
                    path.display()
                );
            }
        }
        assert!(scanned > 0, "the scan found no owner_notes submodule");
    }

    #[test]
    fn the_sweep_skips_owner_notes_submodules_by_prefix_only() {
        assert!(is_owner_notes_submodule("owner_notes/embeddings.rs"));
        assert!(is_owner_notes_submodule("owner_notes/deeper/x.rs"));
        // The module file itself is allowed by name, not by this prefix.
        assert!(!is_owner_notes_submodule("owner_notes.rs"));
        // A look-alike sibling, or the name further down a path, is scanned like any other file.
        assert!(!is_owner_notes_submodule("owner_notes_extra/x.rs"));
        assert!(!is_owner_notes_submodule("owner_notes_x.rs"));
        assert!(!is_owner_notes_submodule("council/owner_notes/x.rs"));
        assert!(!is_owner_notes_submodule("brief.rs"));
    }

    /// The guard below scans production code only: a test module may read the notes table to
    /// assert behaviour (D4 is about production reach, not about what a test inspects).
    #[test]
    fn no_core_module_but_eight_mentions_owner_notes() {
        // Same guard as `redact.rs`: refuse to scan another checkout's sources.
        let built_in = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory must be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "this test binary was compiled in {} and is running in {} - a shared target directory \
             handed this checkout a binary built somewhere else. Touch this file to force a rebuild.",
            built_in.display(),
            running_in.display(),
        );
        // `distill.rs` is the one sanctioned exception, and only for its single call to
        // `active_note_texts_for_project`: D4 of `.ai/specs/2026-10-05-destilador-design.md` lets the
        // distiller's dossier read the notes linked to a project, as context only.
        // `capture.rs` is the other: one call, `create_linked_to_job_in`. A channel capture answer is
        // the owner writing a note; `capture.rs` only forwards it and never reads notes back.
        // `channels/commands.rs` is the third: one call, `create(`, for `/note`. That command is the
        // owner writing a note from a channel; `commands.rs` only forwards it and never reads notes
        // back (plan A2 C2b).
        // `lib.rs` only declares the module (`pub mod owner_notes;`) since the core lib/bin split.
        let allowed = ["owner_notes.rs", "http.rs", "main.rs", "lib.rs"];
        let needle = ["owner", "note"].join("_");
        let mut scanned = 0;
        // Recursive, over every source root: a module directory (`council/`, `judge/`) is as much
        // the agent's memory as a top-level file. Only the top-level files of a root are allowed,
        // so a `http.rs` nested in some module directory is scanned like any other.
        for path in crate::source_scan::rust_files() {
            scanned += 1;
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if crate::source_scan::is_top_level(&path) && allowed.contains(&name) {
                continue;
            }
            // `http.rs`'s children are the same door, split by route family.
            if crate::source_scan::module_path(&path).starts_with("http/") {
                continue;
            }
            // The submodules of this module (`owner_notes/embeddings.rs`, ...) are this module,
            // but only under `core/src`: another crate's `owner_notes/` folder is scanned.
            if is_owner_notes_submodule_file(
                &path,
                &built_in.join("src"),
                &crate::source_scan::module_path(&path),
            ) {
                continue;
            }
            // An out-of-line `#[cfg(test)] mod x;` is test code from its first line.
            if crate::source_scan::is_test_only_module(&path) {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("source file must be readable");
            if crate::source_scan::is_top_level(&path) && name == "distill.rs" {
                assert!(
                    distill_mentions_are_sanctioned(&source),
                    "distill.rs may name the notes only in its one call to \
                     active_note_texts_for_project (spec 2026-10-07 section 5.2)"
                );
                continue;
            }
            if crate::source_scan::is_top_level(&path) && name == "capture.rs" {
                assert!(
                    capture_mentions_are_sanctioned(&source),
                    "capture.rs may name the notes only in its one call to \
                     create_linked_to_job_in (the owner's answer is a note being written)"
                );
                continue;
            }
            if name == "commands.rs"
                && crate::source_scan::module_path(&path).starts_with("channels/")
            {
                assert!(
                    commands_mentions_are_sanctioned(&source),
                    "channels/commands.rs may name the notes only in its one call to create \
                     (`/note` is the owner writing a note; it never reads notes back)"
                );
                continue;
            }
            if crate::source_scan::is_top_level(&path) && name == "themes.rs" {
                assert!(
                    themes_mentions_are_sanctioned(&source),
                    "themes.rs may name the notes only in its calls to vectors_for_clustering \
                     and texts_for_ids (spec 2026-10-22 section 10: grouping needs vectors, \
                     never text; the synthesis text never leaves the Brain)"
                );
                continue;
            }
            if crate::source_scan::module_path(&path) == "distill/generalise.rs" {
                assert!(
                    generalise_mentions_are_sanctioned(&source),
                    "distill/generalise.rs may name the notes only in its calls to nearest_to_vector \
                     and texts_for_ids (spec 2026-10-22 section 10: nearby notes are owner context, \
                     never quoted)"
                );
                continue;
            }
            assert!(
                !production_code(&source).contains(&needle),
                "{} mentions `{needle}` in production code (test modules are not scanned): the \
                 owner's notes are reachable from owner_notes.rs, \
                 http.rs and main.rs (spec D4), from distill.rs, themes.rs and distill/generalise.rs through their sanctioned read calls, and from \
                 capture.rs and channels/commands.rs through one write call each, only, so that \
                 nothing in the agent's memory can read them",
                path.display()
            );
        }
        assert!(scanned > 20, "the scan found only {scanned} source files");
    }

    async fn job_note(pool: &sqlx::SqlitePool, text: &str, job_id: i64) -> i64 {
        let mut conn = pool.acquire().await.unwrap();
        create_linked_to_job_in(&mut conn, text, "shell", job_id)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_job_note_reaches_its_job_and_no_other() {
        let pool = test_pool().await;
        job_note(&pool, "about job seven", 7).await;
        let seven = active_note_texts_for_project(&pool, "p", Some(7))
            .await
            .unwrap();
        assert_eq!(seven, vec!["about job seven".to_string()]);
        assert!(
            active_note_texts_for_project(&pool, "p", Some(8))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            active_note_texts_for_project(&pool, "p", None)
                .await
                .unwrap()
                .is_empty()
        );
        // The link and its event are written the way `add_link` writes them.
        let detail: String = sqlx::query_scalar(
            "SELECT detail FROM owner_note_events WHERE kind = 'linked' ORDER BY id LIMIT 1",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(detail, "relates job:7");
    }

    #[tokio::test]
    async fn a_note_linked_to_both_is_read_once() {
        let pool = test_pool().await;
        let id = job_note(&pool, "both", 7).await;
        add_link(&pool, id, "relates", "project", "p")
            .await
            .unwrap();
        let other = create(&pool, "project only", "shell").await.unwrap();
        add_link(&pool, other, "relates", "project", "p")
            .await
            .unwrap();
        let texts = active_note_texts_for_project(&pool, "p", Some(7))
            .await
            .unwrap();
        assert_eq!(texts, vec!["both".to_string(), "project only".to_string()]);
    }

    #[tokio::test]
    async fn an_archived_job_note_stays_out() {
        let pool = test_pool().await;
        let id = job_note(&pool, "set aside", 7).await;
        update(&pool, id, None, Some("archived")).await.unwrap();
        assert!(
            active_note_texts_for_project(&pool, "p", Some(7))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn note_origin_accepts_channel_id_of_a_compiled_manifest() {
        assert!(origin_allowed("channel:telegram"));
        assert!(origin_allowed("shell"));
    }

    #[test]
    fn note_origin_rejects_the_bare_legacy_telegram() {
        assert!(origin_allowed("channel:telegram"));
        assert!(!origin_allowed("telegram"));
    }

    #[test]
    fn note_origin_rejects_unknown_channel_and_empty_id() {
        assert!(!origin_allowed("channel:nonexistent"));
        assert!(!origin_allowed("channel:"));
        assert!(!origin_allowed("channel"));
        assert!(!origin_allowed(""));
    }

    #[tokio::test]
    async fn capture_answer_accepts_a_channel_origin() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO capture_requests
                 (job_id, project_id, causes, prompt_text, state, deadline, created_at)
             VALUES (7, 'web', '[]', 'why?', 'open', '2099-01-01T00:00:00', '2026-10-07T10:00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let (note_id, released) = crate::capture::answer(&pool, 7, "x", "channel:telegram")
            .await
            .unwrap();
        assert!(released);
        let note = get(&pool, note_id).await.unwrap();
        assert_eq!(note.origin, "channel:telegram");
        let (state, linked): (String, Option<i64>) =
            sqlx::query_as("SELECT state, note_id FROM capture_requests WHERE job_id = 7")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((state.as_str(), linked), ("answered", Some(note_id)));
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owner_note_links")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(links, 1);
    }

    #[tokio::test]
    async fn wake_signal_owner_note_teach_bumps_proposals() {
        let pool = test_pool().await;
        let id = create(&pool, "a lesson to teach", "shell").await.unwrap();
        let mut rx = nucleos_base::wake::subscribe(nucleos_base::wake::Topic::Proposals);
        let before = *rx.borrow_and_update();
        teach(&pool, id, crate::knowledge::Kind::Memory, None, None)
            .await
            .unwrap();
        // Other tests of this binary share the global bus: assert a lower bound only.
        assert!(rx.borrow().wrapping_sub(before) >= 1);
    }

    #[tokio::test]
    async fn create_linked_to_job_refuses_empty_and_unknown_origin() {
        let pool = test_pool().await;
        let mut conn = pool.acquire().await.unwrap();
        assert!(matches!(
            create_linked_to_job_in(&mut conn, "   ", "shell", 7).await,
            Err(NoteError::Empty)
        ));
        assert!(matches!(
            create_linked_to_job_in(&mut conn, "text", "nowhere", 7).await,
            Err(NoteError::UnknownOrigin)
        ));
        drop(conn);
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owner_note_links")
            .fetch_one(&pool)
            .await
            .unwrap();
        let notes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owner_notes")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((notes, links), (0, 0));
    }

    #[tokio::test]
    async fn a_note_never_appears_in_what_the_agent_knows() {
        let pool = test_pool().await;
        let id = create(&pool, "zebraquartz7 is a private thought", "shell")
            .await
            .unwrap();
        add_link(&pool, id, "relates", "contact", "1")
            .await
            .unwrap();

        let everything = crate::knowledge::all(&pool).await.unwrap();
        let machine = crate::knowledge::for_scope(&pool, &crate::knowledge::Scope::Machine)
            .await
            .unwrap();
        for known in everything.iter().chain(machine.iter()) {
            assert!(!known.title.contains("zebraquartz7"));
            assert!(!known.body.contains("zebraquartz7"));
        }
    }
}
