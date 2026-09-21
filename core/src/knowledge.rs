//! What the agent knows, and what a run is told because of it.
//!
//! The gap this closes was named from outside this repository: 87 migrations and no table for
//! anything the agent learned, so every run started from the same prompt with the same blind spots
//! for ever. `0088_refinements.sql` carried the first half of the design and
//! `0143_knowledge.sql` carries this one: ONE store, read in four layers, where the layer is the
//! nature of the knowledge rather than four tables behind a facade.
//!
//! **Split the way `classifier.rs` is split from `hooks.rs`.** [`render`] is pure and holds the
//! only interesting question — what does a node see, in what order, and how much of it — so it is
//! table tests with no database. The storage below is deliberately dumb.
//!
//! This is the phase that moves the store; it deliberately changes no behaviour. The block a node
//! reads after this file lands is the block it read before, over the table's new name and under the
//! old rules. Selection, the five signals and the budget arrive next, and the pool-facing half of
//! them lands in `brief.rs` rather than here.

use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

/// How much of what is known a single node's prompt may carry.
///
/// A ceiling and not a target. The run pays for every token of its own brief, and a store that
/// grows for a year would silently take the context the work needs — the failure mode being that
/// nobody notices, because a prompt does not get slower, it gets emptier of room.
pub const RENDER_CHARS: usize = 4_000;

/// One item's share of the room when it got in on its score.
pub const PER_ITEM_CHARS: usize = 600;

/// And one FLOOR item's share, which is half of it, because five floors at 600 would eat 3,000 of the
/// ~3,400 useful characters and leave less than one whole item behind. The five signals would then
/// decide WHICH row fills each floor and nothing else, which is the blindness this module exists to
/// end. At 300 the floors cost ~1,500, three whole items fit in what is left, and the scoring layer
/// decides something again.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg_attr(test, expect(dead_code))]
pub const FLOOR_ITEM_CHARS: usize = 300;

/// The nature of what is known, which is what makes one store rather than four.
///
/// Unknown values are not an error and not a default: [`Layer::parse`] returns `Option` and every
/// reader `filter_map`s it, so a row whose layer this binary does not recognise is INVISIBLE rather
/// than counted. That is the second defence the migration leans on when it declines to write a
/// CHECK constraint — the vocabulary lives here, and the write is checked against it before it
/// happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Layer {
    /// Facts about the thing being worked on.
    Semantic,
    /// What happened, measured: the consolidator's half.
    Episodic,
    /// How work is done here.
    Procedural,
    /// What one job knows while it is still running, and only for as long as it runs.
    Working,
}

impl Layer {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "semantic" => Some(Layer::Semantic),
            "episodic" => Some(Layer::Episodic),
            "procedural" => Some(Layer::Procedural),
            "working" => Some(Layer::Working),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Semantic => "semantic",
            Layer::Episodic => "episodic",
            Layer::Procedural => "procedural",
            Layer::Working => "working",
        }
    }
}

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

    /// Which layer a kind of 0088's vintage belongs to.
    ///
    /// The same translation `0143_knowledge.sql` writes over the migrated rows, kept here so the
    /// door and the migration cannot disagree about what a `memory` is. A fact about the project is
    /// semantic; the other three are how work is done here.
    fn layer(self) -> Layer {
        match self {
            Kind::Memory => Layer::Semantic,
            Kind::Prompt | Kind::Skill | Kind::Subagent => Layer::Procedural,
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

/// Whose knowledge this is, and — through [`Scope::chain`] — what it inherits.
///
/// Two columns rather than one, so the hot-path index can serve the question. `scope_id` is TEXT
/// and polymorphic, because a project id is TEXT and a job id is INTEGER, which is also why the
/// store carries no foreign key for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The house. `scope_id` is NULL, and only here.
    Machine,
    Project(String),
    /// An errand, which is the crooked case — see [`Scope::chain`].
    ///
    /// **Nothing writes an errand-scoped row, and that is the design rather than an omission**: the
    /// owner writes `project` or `machine`, a run does the same, and the consolidator runs per
    /// project. It is a vocabulary value with an inheritance rule of its own and no writer, kept
    /// because reading an errand still has to know what it inherits — which is nothing but the
    /// house. `cfg_attr` and not a bare `allow`, so the day something does construct one the test
    /// build still says the attribute is stale.
    #[cfg_attr(not(test), allow(dead_code))]
    Errand(String),
    /// A job, and the project it belongs to when the caller knows it.
    ///
    /// Read by nothing outside the tests until the working layer exists to put rows here.
    #[cfg_attr(not(test), allow(dead_code))]
    Job {
        id: i64,
        project: Option<String>,
    },
}

impl Scope {
    /// The scopes a reader in this one is entitled to, most general first.
    ///
    /// `machine` → `project` → `job` inherits downwards, and the most specific wins where they
    /// contradict. **`errand` does not hang off `project`**: `0074_errands.sql:19` gives an errand
    /// no `project_id` at all — it has `chat_key`, `brain`, `folder` — so an errand scope inherits
    /// from `machine` alone. Said here rather than left implicit, because the natural query is the
    /// wrong one and nothing about its result looks wrong.
    fn chain(&self) -> Vec<(&'static str, Option<String>)> {
        let mut chain = vec![("machine", None)];
        match self {
            Scope::Machine => {}
            Scope::Project(id) => chain.push(("project", Some(id.clone()))),
            Scope::Errand(id) => chain.push(("errand", Some(id.clone()))),
            Scope::Job { id, project } => {
                if let Some(project) = project {
                    chain.push(("project", Some(project.clone())));
                }
                chain.push(("job", Some(id.to_string())));
            }
        }
        chain
    }

    /// The two columns as they are written.
    fn columns(&self) -> (&'static str, Option<String>) {
        match self {
            Scope::Machine => ("machine", None),
            Scope::Project(id) => ("project", Some(id.clone())),
            Scope::Errand(id) => ("errand", Some(id.clone())),
            Scope::Job { id, .. } => ("job", Some(id.to_string())),
        }
    }

    /// The scope 0088 could express, which is the only one its rows can have had.
    fn of_project(project_id: Option<&str>) -> Scope {
        match project_id {
            None => Scope::Machine,
            Some(id) => Scope::Project(id.to_owned()),
        }
    }
}

/// One candidate, with everything the pure selection needs and nothing it would have to fetch.
///
/// `s_fts` arrives ALREADY CALCULATED, and that is what purity costs: the rank is SQLite's, computed
/// by `brief` — the one function here that talks to a database. A pure function that had to rank text
/// itself would be a second, worse implementation of FTS5.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Known {
    pub id: i64,
    pub layer: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub source: String,
    pub generator: Option<String>,
    pub evidence: Option<String>,
    pub observations: Option<i64>,
    pub fingerprint: Option<String>,
    pub points_at: Option<String>,
    pub expires_after_runs: Option<i64>,
    pub last_confirmed_at: Option<String>,
    pub shown_count: i64,
    pub outcome_count: i64,
    pub green_count: i64,
    pub last_shown_at: Option<String>,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// SQLite's rank for this row against the context's query, normalised. `0.0` when the row
    /// did not match at all, which on day one is every row.
    ///
    /// Not a column of `knowledge`: the rank belongs to a QUERY, so `#[sqlx(default)]` leaves it
    /// zero for every reader that selects `COLUMNS`, and `brief` sets it after the fetch.
    #[sqlx(default)]
    pub s_fts: f64,
    pub status: String,
    pub proposal_id: Option<i64>,
    pub supersedes: Option<i64>,
    pub origin_run_id: Option<i64>,
    pub created_at: String,
    pub activated_at: Option<String>,
    pub ended_at: Option<String>,
}

/// The kind of node whose work is being briefed, in the order the job graph uses them.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg_attr(test, expect(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Plan,
    Implement,
    Review,
    Replan,
}

/// What the work is, as far as the selection is allowed to know it.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg_attr(test, expect(dead_code))]
pub struct Context {
    /// The scope and its chain (machine -> project -> job; an errand inherits from machine alone).
    pub chain: Vec<Scope>,
    /// The files the worktree touched, or the ones the item declares.
    pub files: Vec<String>,
    /// The communities and modules of the project map those files inhabit — the signal this project
    /// has for free because it already builds the map.
    pub communities: Vec<String>,
    pub node: Option<NodeKind>,
    /// The normalised signature of the red gate, when there is one.
    pub gate: Option<String>,
}

/// The room, and the three numbers that decide how it is spent.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg_attr(test, expect(dead_code))]
pub struct Budget {
    pub render_chars: usize,
    pub per_item_chars: usize,
    pub floor_item_chars: usize,
}

/// One candidate's TRACE and not the signal: whether it was shown and the five scores that decided
/// why it won or lost.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg_attr(test, expect(dead_code))]
pub struct Scored {
    pub knowledge_id: i64,
    pub shown: bool,
    pub s_fts: f64,
    pub s_scope: f64,
    pub s_structure: f64,
    pub s_recency: f64,
    pub s_use: f64,
}

/// What a node reads, and what it was not shown.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg_attr(test, expect(dead_code))]
pub struct Brief {
    pub block: Option<String>,
    /// One entry per candidate, shown or not, with the five signals — this is what `brief` writes to
    /// `run_knowledge`, and the reason the trace can answer WHICH signal elected a row.
    pub trace: Vec<Scored>,
}

/// The candidates in the order the selector will consider them.
#[cfg_attr(not(test), allow(dead_code))]
fn ordered_candidates(known: &[Known]) -> Vec<&Known> {
    let mut candidates: Vec<(f64, Layer, Kind, &Known)> = known
        .iter()
        .filter_map(|row| {
            Some((
                row.s_fts,
                Layer::parse(&row.layer)?,
                Kind::parse(&row.kind)?,
                row,
            ))
        })
        .collect();
    candidates.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or_else(|| right.0.total_cmp(&left.0))
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.3.id.cmp(&right.3.id))
    });
    candidates.into_iter().map(|(_, _, _, row)| row).collect()
}

/// Every column of the store, in the order the migration declares them.
///
/// One constant and not five copies: [`FromRow`] matches by name, so a query that forgets a column
/// fails at runtime on the row rather than at the call site, and the five readers below would each
/// have to be corrected separately every time the table grows.
const COLUMNS: &str = "id, layer, scope_kind, scope_id, source, generator, evidence, observations,
                       fingerprint, points_at, expires_after_runs, last_confirmed_at, shown_count,
                       outcome_count, green_count, last_shown_at, kind, title, body, status,
                       proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at";

/// PURE: the block a node's brief gains because of what earlier runs learned.
///
/// Appended to the brief and never replacing it, exactly as `notes::render` is — a node handed a
/// standing instruction instead of its task does the standing instruction.
pub fn render(known: &[Known]) -> Option<String> {
    // Only what a person approved. Filtered here rather than trusted from the caller's query: this
    // function is the last thing between a `proposed` row and a node's prompt, and something that
    // reaches a prompt unapproved makes the approval decorative, which is the entire mechanism.
    let mut live: Vec<(Kind, &Known)> = known
        .iter()
        .filter(|row| row.status == "active")
        // Both vocabularies, and neither is decoration. The store carries no CHECK constraints, so
        // this pair is what makes an unrecognised value an INVISIBLE row rather than a counted one
        // — a row whose layer this binary cannot name is a row it cannot honestly render.
        .filter(|row| Layer::parse(&row.layer).is_some())
        .filter_map(|row| Kind::parse(&row.kind).map(|kind| (kind, row)))
        .collect();
    if live.is_empty() {
        return None;
    }
    // Kind first, id second: what a node reads first is a property of the store, never of the order
    // rows happened to come back from SQLite.
    live.sort_by_key(|(kind, row)| (*kind, row.id));

    let mut block = String::from(
        "\n\nEarlier work on this project left the notes below, and a person approved every one of \
         them before it reached you. Your brief above is still what you were asked to do; these are \
         things already known about the project you are doing it in:",
    );

    let mut shown = 0usize;
    let mut heading_written: Option<Kind> = None;
    for (kind, row) in &live {
        // Rendered before it is measured, so the decision to include it is made on the length of
        // what will actually be written rather than on an estimate of it.
        let mut piece = String::new();
        if heading_written != Some(*kind) {
            piece.push_str(&format!("\n\n{}:", kind.heading()));
        }
        piece.push_str(&format!("\n- {}: {}", row.title, clip(&row.body)));

        if block.len() + piece.len() > RENDER_CHARS {
            break;
        }
        block.push_str(&piece);
        heading_written = Some(*kind);
        shown += 1;
    }

    // Said, never silent. A store trimmed without saying so reads as the whole of what is known, and
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

/// One row's share of the room.
///
/// By chars and not bytes: a slice through a UTF-8 boundary panics on exactly the inputs nobody
/// writes tests with, and a body is free text somebody wrote.
fn clip(body: &str) -> String {
    if body.chars().count() <= PER_ITEM_CHARS {
        return body.to_owned();
    }
    let mut cut: String = body.chars().take(PER_ITEM_CHARS).collect();
    cut.push('…');
    cut
}

/// What a node in this scope is entitled to be told.
///
/// The chain comes too, and that is the whole reason this takes a [`Scope`] rather than a project
/// id: machine-wide rows are not hidden by the scoping rule, which is about not letting one
/// project's lesson become another's lie. `scope_id IS ?` rather than `=`, because the machine
/// scope's id is NULL and `= NULL` is never true — the bug would be a briefing silently missing
/// everything known about the house.
///
/// Ordered here as well as in [`render`], so a caller that skips the renderer still gets a stable
/// list, and so the LIMIT below cuts the tail rather than an arbitrary middle.
pub async fn for_scope(pool: &SqlitePool, scope: &Scope) -> sqlx::Result<Vec<Known>> {
    let chain = scope.chain();
    let terms: Vec<&str> = chain
        .iter()
        .map(|_| "(scope_kind = ? AND scope_id IS ?)")
        .collect();
    let sql = format!(
        "SELECT {COLUMNS}
           FROM knowledge
          WHERE status = 'active' AND ({})
          ORDER BY id
          LIMIT ?",
        terms.join(" OR ")
    );
    // `AssertSqlSafe` because sqlx 0.9 takes only `&'static str` otherwise, and the audit it asks
    // for is short: the interpolated parts are [`COLUMNS`], which is a literal, and one
    // `(scope_kind = ? AND scope_id IS ?)` per link of the chain, which is also a literal. Every
    // value the caller supplies is bound below.
    let mut query = sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(sql));
    for (kind, id) in &chain {
        query = query.bind(*kind).bind(id.clone());
    }
    query
        // A ceiling on the QUERY as well as on the rendering, because the two protect different
        // things: `RENDER_CHARS` keeps a prompt affordable, and this keeps a project that has
        // approved ten thousand rows from reading all of them into memory to render forty.
        .bind(MAX_READ as i64)
        .fetch_all(pool)
        .await
}

/// How many approved rows are read before rendering ever begins.
const MAX_READ: usize = 200;

/// Everything the store holds, in every status, newest first.
///
/// Not filtered to `active`, deliberately: the reviewable history IS the feature, and a screen that
/// showed only what is in force could not answer "what did it try to learn that I said no to".
///
/// Here rather than written out at the one call site it has, which is where it used to live. The
/// column list is [`COLUMNS`] and nothing else, so a reader outside this module cannot fall behind
/// the table by naming twelve of its twenty-six columns — which is precisely how the handler that
/// used to hold this query would have survived `0143` by silently returning nothing.
pub async fn all(pool: &SqlitePool) -> sqlx::Result<Vec<Known>> {
    // `AssertSqlSafe`, audited: the only interpolation is [`COLUMNS`], a literal.
    sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge ORDER BY id DESC LIMIT 500"
    )))
    .fetch_all(pool)
    .await
}

/// What somebody is asking the store to learn.
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
    /// The row this one replaces — ended if and when THIS one is approved, never before.
    pub supersedes: Option<i64>,
}

/// The two things a declaration can say that the store must refuse.
#[derive(Debug)]
pub enum ProposeError {
    Db(sqlx::Error),
    /// `supersedes` names a row that is not there — a chain nobody could read back.
    UnknownPredecessor(i64),
    /// `supersedes` names a row in another scope. Refused because it is the one column that writes
    /// across the boundary the rest of this module exists to hold.
    ForeignPredecessor(i64),
}

impl std::fmt::Display for ProposeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProposeError::Db(error) => write!(formatter, "{error}"),
            ProposeError::UnknownPredecessor(id) => {
                write!(formatter, "there is nothing known with id {id} to replace")
            }
            ProposeError::ForeignPredecessor(id) => {
                write!(
                    formatter,
                    "what is known with id {id} belongs to another scope"
                )
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
/// Writes the row `proposed` AND the proposal that asks about it, in one transaction — the two are
/// one act, and a crash between them would leave either a lesson nobody can approve or a question
/// about a lesson that is not there.
///
/// The row is written now rather than on approval, unlike `create_calendar_event`'s shape, and the
/// difference is deliberate: a rejected event is nothing, but a rejected LESSON is a record worth
/// keeping — it is how somebody later sees what the agent kept trying to learn and was told no to.
///
/// **The three new columns are derived here and not asked for**, by exactly the translation
/// `0143_knowledge.sql` applies to the rows that came before: the layer from the kind, the scope
/// from the project id, and the source from whether a run is behind the request. It is what keeps
/// this phase a move rather than a change — the door writes what it wrote. Deriving the scope from
/// the run instead of from the caller is a later decision, with its own reasons.
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

    let scope = Scope::of_project(project_id);
    let (scope_kind, scope_id) = scope.columns();

    // Checked before anything is written, and checked here rather than left to the foreign key:
    // SQLite would accept a link to another scope's row without a word, and the failure would
    // surface as one repository's history quietly containing another's.
    if let Some(predecessor) = supersedes {
        let owner: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT scope_kind, scope_id FROM knowledge WHERE id = ?")
                .bind(predecessor)
                .fetch_optional(pool)
                .await?;
        let owner = owner.ok_or(ProposeError::UnknownPredecessor(predecessor))?;
        if owner.0 != scope_kind || owner.1.as_deref() != scope_id.as_deref() {
            return Err(ProposeError::ForeignPredecessor(predecessor));
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;

    let knowledge_id = sqlx::query(
        "INSERT INTO knowledge
           (layer, scope_kind, scope_id, source, kind, title, body, status, supersedes,
            origin_run_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'proposed', ?, ?, ?)",
    )
    .bind(kind.layer().as_str())
    .bind(scope_kind)
    .bind(scope_id.as_deref())
    // Never read from the body: who knocked at the door is a fact the door has and the text does
    // not.
    .bind(if origin_run_id.is_some() {
        "run"
    } else {
        "owner"
    })
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
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, NULL, 'proposed', 'declared by a run', ?)",
    )
    .bind(knowledge_id)
    .bind(&now)
    .execute(&mut *tx)
    .await?;

    // `tool_input` carries the id and nothing a reader would have to join to understand the
    // question. A proposal a person cannot answer without opening another screen is a proposal that
    // waits until morning and then gets approved unread.
    //
    // **`refinement_id` keeps its name here, and `kind = 'refinement'` below keeps its value.**
    // Both are data already written to disk, in rows this migration does not rewrite — renaming
    // either would orphan every question still waiting for an answer, which is a worse thing than
    // an old word in a JSON payload.
    let tool_input = serde_json::json!({
        "refinement_id": knowledge_id,
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

    sqlx::query("UPDATE knowledge SET proposal_id = ? WHERE id = ?")
        .bind(proposal_id)
        .bind(knowledge_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok((knowledge_id, proposal_id))
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecisionError {
    NotFound,
    NotPending,
    Malformed,
}

/// Which row a pending proposal is asking about, or why it is not answerable.
///
/// Shared by both answers deliberately: a yes and a no must agree about what counts as a question,
/// or the pair drifts into a proposal that can be approved and not refused — which is exactly what
/// this layer shipped with, `proposals::reject_proposal` taking `action-approval` alone.
async fn pending_knowledge(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
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
/// store disagreeing about whether the agent was allowed to learn something.
pub async fn approve(pool: &SqlitePool, proposal_id: i64) -> Result<i64, DecisionError> {
    let knowledge_id = pending_knowledge(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    // Guarded on `proposed`, so a second approval of the same row is a no-op rather than a second
    // activation stamp over the first.
    let activated = sqlx::query(
        "UPDATE knowledge SET status = 'active', activated_at = ?
          WHERE id = ? AND status = 'proposed'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;
    if activated.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'active', 'approved by the owner', ?)",
    )
    .bind(knowledge_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;

    // The chain moves here and nowhere else. A successor that ended its predecessor when it was
    // merely *declared* would let a question nobody answered delete the answer already in force,
    // so the old text stands until the moment somebody chooses the new one over it.
    let predecessor: Option<i64> =
        sqlx::query_scalar("SELECT supersedes FROM knowledge WHERE id = ?")
            .bind(knowledge_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| DecisionError::NotFound)?;
    if let Some(predecessor) = predecessor {
        let ended = sqlx::query(
            "UPDATE knowledge SET status = 'superseded', ended_at = ?
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
                "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
                 VALUES (?, 'active', 'superseded', ?, ?)",
            )
            .bind(predecessor)
            .bind(format!("replaced by {knowledge_id}"))
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
    Ok(knowledge_id)
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
    let knowledge_id = pending_knowledge(pool, proposal_id).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await.map_err(|_| DecisionError::NotFound)?;

    let refused = sqlx::query(
        "UPDATE knowledge SET status = 'rejected', ended_at = ?
          WHERE id = ? AND status = 'proposed'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| DecisionError::NotFound)?;
    if refused.rows_affected() != 1 {
        return Err(DecisionError::NotPending);
    }

    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'proposed', 'rejected', 'refused by the owner', ?)",
    )
    .bind(knowledge_id)
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
    Ok(knowledge_id)
}

/// Taking one back, which is the half that makes approving safe to do.
///
/// `reverted` and not deleted: the history is the feature. A store somebody can only add to is one
/// nobody dares add to.
pub async fn revert(pool: &SqlitePool, knowledge_id: i64, note: &str) -> sqlx::Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE knowledge SET status = 'reverted', ended_at = ? WHERE id = ? AND status = 'active'",
    )
    .bind(&now)
    .bind(knowledge_id)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO knowledge_events (knowledge_id, from_status, to_status, note, at)
         VALUES (?, 'active', 'reverted', ?, ?)",
    )
    .bind(knowledge_id)
    .bind(note)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// One decision in a row's life, as a person reads it back.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Event {
    pub id: i64,
    pub from_status: Option<String>,
    pub to_status: String,
    pub note: Option<String>,
    pub at: String,
}

/// What this says, what it said before, and what replaced it — the reviewable history the whole
/// store is for, in one answer.
///
/// One call and not three, because the three are only useful together: "revert this" is a decision
/// a person makes by reading the text that would come back, and a screen that made them fetch it
/// separately is a screen where they revert without having read it.
#[derive(Debug, Serialize)]
pub struct History {
    pub known: Known,
    pub events: Vec<Event>,
    /// Newest first: what this one replaced, then what THAT replaced, back to the first text.
    pub replaced: Vec<Known>,
    /// What replaced this one, if a person has approved a successor.
    pub replaced_by: Option<Known>,
}

/// How far back a chain is read before the walk stops and says no more.
const MAX_CHAIN: usize = 50;

/// Read one row, its own decisions, and the chain on both sides of it.
pub async fn history(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<History>> {
    let Some(known) = fetch(pool, id).await? else {
        return Ok(None);
    };

    let events = sqlx::query_as::<_, Event>(
        "SELECT id, from_status, to_status, note, at
           FROM knowledge_events WHERE knowledge_id = ? ORDER BY id",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;

    let mut replaced: Vec<Known> = Vec::new();
    let mut seen: std::collections::HashSet<i64> = std::collections::HashSet::from([id]);
    let mut next = known.supersedes;
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
    let replaced_by = sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge WHERE supersedes = ? ORDER BY id DESC LIMIT 1"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(Some(History {
        known,
        events,
        replaced,
        replaced_by,
    }))
}

async fn fetch(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<Known>> {
    sqlx::query_as::<_, Known>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM knowledge WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The last version of the schema that still had `refinements` in it.
    ///
    /// Named rather than written as a literal in five places: what these tests are about is the
    /// boundary between before and after, and a bare `142` at a call site says nothing about which
    /// side of it the caller means to be on.
    const BEFORE: i64 = 142;

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

    /// One row of 0088's table, written while that table still exists.
    ///
    /// The translation is a property of the migration, so the only honest way to test it is to put
    /// rows where the migration will find them — which means stopping the chain at [`BEFORE`]
    /// rather than asking `#[sqlx::test]` for a database where `refinements` is already gone.
    async fn seed_refinement(
        pool: &sqlx::SqlitePool,
        project: Option<&str>,
        kind: &str,
        status: &str,
        title: &str,
        origin_run_id: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO refinements
               (project_id, kind, title, body, status, origin_run_id, created_at)
             VALUES (?, ?, ?, 'body', ?, ?, '2026-08-19T00:00:00+00:00')",
        )
        .bind(project)
        .bind(kind)
        .bind(title)
        .bind(status)
        .bind(origin_run_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed(pool: &sqlx::SqlitePool, project: Option<&str>, status: &str, title: &str) {
        let scope = Scope::of_project(project);
        let (scope_kind, scope_id) = scope.columns();
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', ?, ?, 'owner', 'memory', ?, 'body', ?,
                     '2026-08-19T00:00:00+00:00')",
        )
        .bind(scope_kind)
        .bind(scope_id)
        .bind(title)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    /// `episodic` and `working` are empty the moment the store exists, and that is the claim: 0088
    /// had nothing that could translate into either, so a row in one of them right after the
    /// migration would mean the translation invented knowledge.
    #[tokio::test]
    async fn the_measured_and_the_working_layers_are_empty_the_moment_the_store_is_created() {
        let pool = crate::testdb::pool_migrated_through(BEFORE).await;
        for kind in ["prompt", "memory", "skill", "subagent"] {
            seed_refinement(&pool, Some("mine"), kind, "active", kind, None).await;
        }
        crate::testdb::apply_migrations_after(&pool, BEFORE).await;

        let counted = |layer: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM knowledge WHERE layer = ?")
                    .bind(layer)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };

        assert_eq!(
            counted("episodic").await,
            0,
            "the translation invented a measurement nobody measured"
        );
        assert_eq!(
            counted("working").await,
            0,
            "the translation invented a job's working knowledge out of a table with no jobs in it"
        );
        // And nothing was dropped on the way: the four kinds landed in the two layers that existed.
        assert_eq!(counted("semantic").await, 1);
        assert_eq!(counted("procedural").await, 3);
    }

    /// The translation of one row, asserted rather than assumed. 0088 had no `source` column, so the
    /// criterion is the only one available: a run is the only thing that could have written a row
    /// without a person.
    #[tokio::test]
    async fn a_lesson_a_person_wrote_migrates_as_the_owners_and_one_a_run_wrote_as_a_runs() {
        let pool = crate::testdb::pool_migrated_through(BEFORE).await;
        seed_refinement(&pool, Some("mine"), "memory", "active", "by hand", None).await;
        seed_refinement(&pool, None, "prompt", "active", "by a run", Some(900_001)).await;
        crate::testdb::apply_migrations_after(&pool, BEFORE).await;

        let source = |title: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_as::<_, (String, String, Option<String>)>(
                    "SELECT source, scope_kind, scope_id FROM knowledge WHERE title = ?",
                )
                .bind(title)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };

        assert_eq!(
            source("by hand").await,
            (
                "owner".to_owned(),
                "project".to_owned(),
                Some("mine".to_owned())
            ),
            "a row with no run behind it did not migrate as the owner's"
        );
        assert_eq!(
            source("by a run").await,
            ("run".to_owned(), "machine".to_owned(), None),
            "a row a run wrote did not migrate as a run's, or lost its machine scope"
        );
    }

    /// Unknown vocabulary is an invisible row, never a counted one (D10). Written by direct SQL,
    /// which is the only writer that skips the Rust constants.
    #[tokio::test]
    async fn a_row_written_by_direct_sql_with_an_unknown_layer_reaches_no_brief() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('astrology', 'project', 'mine', 'owner', 'memory', 'mercury is retrograde',
                     'so the build is flaky', 'active', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        seed(&pool, Some("mine"), "active", "a layer this binary knows").await;

        let read = for_scope(&pool, &Scope::Project("mine".into()))
            .await
            .unwrap();
        assert_eq!(
            read.len(),
            2,
            "the read is where the filtering happens, and it is not: {read:?}"
        );

        // The row exists, is `active`, is in scope, and still reaches nothing. That is the whole of
        // what "no CHECK constraints" costs and the whole of what the Rust constants buy.
        let block = render(&read).expect("the known layer still renders");
        assert!(
            !block.contains("mercury is retrograde"),
            "a row this binary cannot name reached a node's prompt: {block}"
        );
        assert!(
            block.contains("a layer this binary knows"),
            "the unknown row took the known one down with it: {block}"
        );
    }

    /// The mirror carries the corpus that predates it. The assertion the spec did not ask for:
    /// without the backfill the first of the five signals is dead on every row that existed before
    /// today.
    #[tokio::test]
    async fn a_row_that_existed_before_the_mirror_is_still_findable_by_its_words() {
        let pool = crate::testdb::pool_migrated_through(BEFORE).await;
        sqlx::query(
            "INSERT INTO refinements (project_id, kind, title, body, status, created_at)
             VALUES ('mine', 'memory', 'the estuary at dawn', 'herons stand in the shallows',
                     'active', '2026-08-19T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        crate::testdb::apply_migrations_after(&pool, BEFORE).await;

        let found: Vec<i64> =
            sqlx::query_scalar("SELECT rowid FROM knowledge_fts WHERE knowledge_fts MATCH ?")
                .bind("herons")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            found.len(),
            1,
            "the corpus that predates the mirror is invisible to it, and silently"
        );
    }

    /// And the mirror does not outlive what it indexes.
    #[tokio::test]
    async fn a_deleted_row_leaves_no_terms_behind() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (id, layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES (7, 'semantic', 'project', 'mine', 'owner', 'memory', 'the estuary at dawn',
                     'herons stand in the shallows', 'active', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let matching = || {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM knowledge_fts WHERE knowledge_fts MATCH ?",
                )
                .bind("herons")
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(matching().await, 1, "the insert trigger wrote no terms");

        sqlx::query("DELETE FROM knowledge WHERE id = 7")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            matching().await,
            0,
            "the index kept the terms of a row that is gone, which is the bug 0057 was written for"
        );
    }

    /// The scoping rule the migration argues for, asserted in both directions at once. A lesson
    /// about one repository's build is a lie about another's — and a lesson about the house is not
    /// hidden by that rule, which is the half a `scope_id = ?` alone would get wrong.
    #[tokio::test]
    async fn a_node_reads_its_own_projects_lessons_and_the_houses_and_no_others() {
        let pool = test_pool().await;
        seed(&pool, Some("mine"), "active", "mine-active").await;
        seed(&pool, None, "active", "house-wide").await;
        seed(&pool, Some("other"), "active", "someone-elses").await;
        seed(&pool, Some("mine"), "proposed", "mine-unapproved").await;

        let read = for_scope(&pool, &Scope::Project("mine".into()))
            .await
            .unwrap();
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
            "something nobody approved was read for a prompt: {titles:?}"
        );
    }

    /// An errand inherits from the house and from nothing else, which is the one place where the
    /// obvious query is the wrong one: `errands` has no `project_id` (`0074_errands.sql:19`), so
    /// there is no project for an errand to inherit from, and a chain that reached for one would be
    /// picking a project at random.
    #[tokio::test]
    async fn an_errand_reads_the_house_and_no_projects_lessons() {
        let pool = test_pool().await;
        seed(&pool, None, "active", "house-wide").await;
        seed(&pool, Some("mine"), "active", "a project's").await;

        let read = for_scope(&pool, &Scope::Errand("chat-7".into()))
            .await
            .unwrap();
        let titles: Vec<&str> = read.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["house-wide"],
            "an errand read something no errand inherits: {titles:?}"
        );
    }

    /// A job reads all three links of the chain, and the straight case is worth asserting beside
    /// the crooked one above: `machine` -> `project` -> `job`, where the errand has only the first.
    ///
    /// The project arrives beside the job id rather than being looked up, because `scope_id` is a
    /// polymorphic TEXT column with no foreign key — there is nothing for a join to follow, and a
    /// reader that guessed would be guessing which project a job belongs to.
    #[tokio::test]
    async fn a_job_reads_the_house_its_project_and_its_own() {
        let pool = test_pool().await;
        seed(&pool, None, "active", "house-wide").await;
        seed(&pool, Some("mine"), "active", "the project's").await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('working', 'job', '41', 'run', 'memory', 'this job''s own', 'body', 'active',
                     '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Another job's, which the chain must not reach: a job id is the most specific link there
        // is, and inheritance goes downwards only.
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('working', 'job', '42', 'run', 'memory', 'another job''s', 'body', 'active',
                     '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let scope = Scope::Job {
            id: 41,
            project: Some("mine".into()),
        };
        let titles: Vec<String> = for_scope(&pool, &scope)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.title)
            .collect();
        assert_eq!(
            titles,
            vec!["house-wide", "the project's", "this job's own"],
            "a job did not read its whole chain, or read past the end of it: {titles:?}"
        );
    }

    /// The whole mechanism, end to end and in the order it happens: a run declares, nothing reaches
    /// a prompt, a person says yes, and only then does it. The middle assertion is the one that
    /// matters — it is what "the agent declares and the core activates" means when it is true.
    #[tokio::test]
    async fn a_declared_lesson_reaches_no_prompt_until_a_person_approves_it() {
        let pool = test_pool().await;
        let mine = Scope::Project("mine".into());
        let (knowledge_id, proposal_id) = propose(
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
            for_scope(&pool, &mine).await.unwrap().is_empty(),
            "a lesson nobody approved was already reaching prompts"
        );

        assert_eq!(approve(&pool, proposal_id).await.unwrap(), knowledge_id);
        let after = for_scope(&pool, &mine).await.unwrap();
        assert_eq!(after.len(), 1, "approving did not activate the lesson");
        assert!(render(&after).is_some(), "an active lesson renders nothing");

        // The door derives what 0088 could not say, and derives it the way the migration does.
        assert_eq!(
            after[0].layer, "semantic",
            "a fact landed in the wrong layer"
        );
        assert_eq!(
            after[0].source, "run",
            "a run's lesson is not marked as one"
        );

        // Second approval is a no-op rather than a second activation stamp.
        assert_eq!(
            approve(&pool, proposal_id).await,
            Err(DecisionError::NotPending)
        );

        assert!(
            revert(&pool, knowledge_id, "made things worse")
                .await
                .unwrap()
        );
        assert!(
            for_scope(&pool, &mine).await.unwrap().is_empty(),
            "a reverted lesson still reaches prompts"
        );

        // The history is the feature: three rows, not a deleted row.
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ?")
                .bind(knowledge_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events, 3, "the row's history is not reviewable");
    }

    fn one(id: i64, kind: &str, title: &str, body: &str) -> Known {
        Known {
            id,
            layer: Kind::parse(kind).unwrap().layer().as_str().into(),
            scope_kind: "project".into(),
            scope_id: Some("p".into()),
            source: "run".into(),
            generator: None,
            evidence: None,
            observations: None,
            fingerprint: None,
            points_at: None,
            expires_after_runs: None,
            last_confirmed_at: None,
            shown_count: 0,
            outcome_count: 0,
            green_count: 0,
            last_shown_at: None,
            kind: kind.into(),
            title: title.into(),
            body: body.into(),
            s_fts: 0.0,
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
    /// every job, so an empty store that still wrote a heading would tax every run for ever.
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
        .expect("one active row renders");
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

    /// D6 is this test and nothing else. Without the `id` tail, two rows of the same scope with the same
    /// files and rank 0 tie, two distributions become indistinguishable, and "deterministic" is a word.
    /// `knowledge.rs:257-258` carries the reason in the house's own words: "what a node reads first is a
    /// property of the store, never of the order rows happened to come back from SQLite."
    #[test]
    fn two_candidates_that_tie_on_every_signal_are_still_ordered_the_same_way_every_time() {
        let mut higher_score = one(90, "subagent", "higher-score", "s");
        higher_score.layer = Layer::Working.as_str().into();
        higher_score.s_fts = 1.0;

        let tied_second = one(20, "memory", "tied-second", "m2");
        let tied_first = one(10, "memory", "tied-first", "m1");
        let prompt = one(80, "prompt", "prompt", "p");
        let skill = one(70, "skill", "skill", "k");

        // The tied pair shares project scope `p`, this one selection context (and therefore its files),
        // and rank 0.0. The other rows make each earlier key observable before the id tail is asserted.
        let first = vec![
            higher_score.clone(),
            tied_second.clone(),
            prompt.clone(),
            tied_first.clone(),
            skill.clone(),
        ];
        let second = vec![skill, tied_first, prompt, tied_second, higher_score];
        let ids = |rows: &[Known]| {
            ordered_candidates(rows)
                .into_iter()
                .map(|row| row.id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };

        let first_ids = ids(&first);
        let second_ids = ids(&second);
        assert_eq!(first_ids.as_bytes(), second_ids.as_bytes());
        assert_eq!(first_ids, "90,10,20,80,70");
    }

    /// A store that grows for a year would take the context the work needs, and the failure mode is
    /// silent: a prompt does not get slower, it gets emptier of room. So it is bounded, and it says
    /// what it left out rather than trimming in silence.
    #[test]
    fn the_block_is_bounded_and_says_what_it_left_out() {
        let many: Vec<Known> = (1..=60)
            .map(|i| one(i, "memory", &format!("fact {i}"), &"x".repeat(300)))
            .collect();
        let block = render(&many).expect("renders");
        assert!(
            block.len() <= RENDER_CHARS * 2,
            "unbounded: {} chars from {} rows",
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
            "something nobody approved reached a node's prompt"
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
    /// the store would lose a lesson by way of a question nobody said yes to.
    #[tokio::test]
    async fn approving_a_successor_is_what_ends_the_one_it_replaces() {
        let pool = test_pool().await;
        let mine = Scope::Project("mine".into());
        let (first, first_proposal) = declare(&pool, Some("mine"), "old text", None)
            .await
            .unwrap();
        approve(&pool, first_proposal).await.unwrap();

        let (second, second_proposal) = declare(&pool, Some("mine"), "new text", Some(first))
            .await
            .unwrap();
        let live: Vec<i64> = for_scope(&pool, &mine)
            .await
            .unwrap()
            .iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(
            live,
            vec![first],
            "an unapproved successor already ended the text it wants to replace"
        );

        approve(&pool, second_proposal).await.unwrap();
        let live: Vec<i64> = for_scope(&pool, &mine)
            .await
            .unwrap()
            .iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(
            live,
            vec![second],
            "both texts are in force at once, which is the pile the chain exists to prevent"
        );

        let (status, ended): (String, Option<String>) =
            sqlx::query_as("SELECT status, ended_at FROM knowledge WHERE id = ?")
                .bind(first)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "superseded", "the predecessor kept a wrong status");
        assert!(ended.is_some(), "the predecessor ended at no time at all");

        // Named, not merely ended: "this stopped applying" and "this was replaced by that" are
        // different things to read six months later, and only one of them can be acted on.
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'superseded'",
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
    /// leave a dangling chain nobody can read back; a successor naming ANOTHER scope's row would
    /// let one repository end another's — the exact poisoning the scope columns exist to stop,
    /// arriving through the one column that writes across the boundary.
    #[tokio::test]
    async fn a_successor_may_not_name_nothing_nor_another_scopes_lesson() {
        let pool = test_pool().await;
        let (theirs, _) = declare(&pool, Some("theirs"), "their lesson", None)
            .await
            .unwrap();

        assert!(
            matches!(
                declare(&pool, Some("mine"), "replaces a ghost", Some(4242)).await,
                Err(ProposeError::UnknownPredecessor(4242))
            ),
            "something was allowed to replace a row that does not exist"
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
        let (knowledge_id, proposal_id) = declare(&pool, Some("mine"), "not this one", None)
            .await
            .unwrap();

        assert_eq!(reject(&pool, proposal_id).await.unwrap(), knowledge_id);
        let refused = fetch(&pool, knowledge_id)
            .await
            .unwrap()
            .expect("the refusal was deleted rather than recorded");
        assert_eq!(refused.status, "rejected");
        assert!(refused.ended_at.is_some(), "a refusal with no time on it");
        assert!(
            for_scope(&pool, &Scope::Project("mine".into()))
                .await
                .unwrap()
                .is_empty(),
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
    async fn the_history_reads_back_through_everything_a_row_replaced() {
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
            .expect("a row that exists has a history");
        assert_eq!(middle.known.id, second);
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
            "a row that does not exist reported a history"
        );

        sqlx::query("UPDATE knowledge SET supersedes = ? WHERE id = ?")
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
