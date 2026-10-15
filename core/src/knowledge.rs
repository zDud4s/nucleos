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

#[cfg(test)]
use self::history::fetch;
use self::history::known_element;
use self::select::weights;
#[cfg(test)]
use self::select::{
    NEUTRAL_UTILITY, WITH_SIM, WITHOUT_SIM, Weights, ordered_candidates, scored_candidates,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool};

mod decide;
mod history;
mod propose;
mod select;
mod signature;
mod store;

pub use self::decide::*;
pub use self::history::*;
pub use self::propose::*;
pub use self::select::*;
pub use self::signature::*;
pub use self::store::*;

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
pub const FLOOR_ITEM_CHARS: usize = 300;

const PREAMBLE: &str = "\n\nEarlier work on this project left the notes below under the two named admission rules. \
     Each item names its layer. Your brief above is still what you were asked to do; these are \
     things already known about the project you are doing it in:";

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
    /// What one job knows while it is still running. Closes with the job.
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
    /// A team of agents. Read by an agent that belongs to it, and by nobody else.
    Team(String),
    /// One agent's own memory. Read by that agent, and by nobody else.
    Agent(String),
    /// A job, and the project it belongs to when the caller knows it.
    Job {
        id: i64,
        project: Option<String>,
    },
}

impl Scope {
    /// Parse the two stored columns before using scope as an ordering signal.
    fn parse(kind: &str, id: Option<&str>) -> Option<Self> {
        match (kind, id) {
            ("machine", None) => Some(Scope::Machine),
            ("project", Some(id)) => Some(Scope::Project(id.to_owned())),
            ("team", Some(id)) => Some(Scope::Team(id.to_owned())),
            ("agent", Some(id)) => Some(Scope::Agent(id.to_owned())),
            ("job", Some(id)) => Some(Scope::Job {
                id: id.parse().ok()?,
                project: None,
            }),
            _ => None,
        }
    }

    /// The scopes a reader in this one is entitled to, most general first.
    ///
    /// `machine` → `project` → `job` inherits downwards, and the most specific wins where they
    /// contradict.
    pub(crate) fn chain(&self) -> Vec<(&'static str, Option<String>)> {
        let mut chain = vec![("machine", None)];
        match self {
            Scope::Machine => {}
            Scope::Project(id) => chain.push(("project", Some(id.clone()))),
            Scope::Team(id) => chain.push(("team", Some(id.clone()))),
            Scope::Agent(id) => chain.push(("agent", Some(id.clone()))),
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
            Scope::Team(id) => ("team", Some(id.clone())),
            Scope::Agent(id) => ("agent", Some(id.clone())),
            Scope::Job { id, .. } => ("job", Some(id.to_string())),
        }
    }

    /// Where the scope sits in the full chain `machine` -> `project` -> `team` -> `agent` -> `job`:
    /// the more specific the scope, the higher the rank.
    fn rank(&self) -> u8 {
        match self {
            Scope::Machine => 0,
            Scope::Project(_) => 1,
            Scope::Team(_) => 2,
            Scope::Agent(_) => 3,
            Scope::Job { .. } => 4,
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
    /// Briefings whose run reached an outcome. `0` is NOT MEASURED, and it is a different thing from
    /// `green_count == 0` with this above zero, which is measured badly.
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
    /// Cosine against the briefing's query vector, set by `brief`; `0.0` for any other reader.
    /// Not a column of `knowledge`, so it is not in `COLUMNS`.
    #[sqlx(default)]
    pub s_sim: f64,
    pub status: String,
    pub proposal_id: Option<i64>,
    pub supersedes: Option<i64>,
    pub origin_run_id: Option<i64>,
    pub created_at: String,
    pub activated_at: Option<String>,
    pub ended_at: Option<String>,
}

/// The kind of node whose work is being briefed, in the order the job graph uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Plan,
    Implement,
    Review,
    Replan,
}

/// What the work is, as far as the selection is allowed to know it.
#[derive(Clone)]
pub struct Context {
    /// The scope and its chain (machine -> project -> job).
    pub chain: Vec<Scope>,
    /// The files the worktree touched, or the ones the item declares.
    pub files: Vec<String>,
    /// The communities and modules of the project map those files inhabit — the signal this project
    /// has for free because it already builds the map.
    pub communities: Vec<String>,
    pub node: Option<NodeKind>,
    /// The normalised signature of the red gate, when there is one.
    pub gate: Option<String>,
    /// Whether this briefing has a query vector (spec 5.2): absence is per briefing, not per row.
    /// Set only by `brief`.
    pub query_embedded: bool,
    /// The agent doing the work, when there is one: its own memory joins the chain.
    pub agent: Option<String>,
    /// The team that agent belongs to, when there is one: the team's memory joins the chain.
    pub team: Option<String>,
}

impl Context {
    /// Every context outside a job inherits only this chain (spec sections 3.4 and 6).
    pub fn for_project(project_id: Option<&str>) -> Self {
        let mut chain = vec![Scope::Machine];
        if let Some(project_id) = project_id {
            chain.push(Scope::Project(project_id.to_owned()));
        }
        Self {
            chain,
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        }
    }

    /// Every scope this context is entitled to read, most general first:
    /// `machine` -> `project` -> `team` -> `agent` -> `job`.
    ///
    /// [`Context::chain`] carries the machine, project and job links; the team and the agent join
    /// it here, ahead of the job and behind the project, and an absent link is simply skipped. The
    /// result is ordered by [`Scope::rank`], so the more specific a scope the higher its
    /// `s_scope`, and a row of any scope outside this list is never selected.
    pub fn scopes(&self) -> Vec<Scope> {
        let mut scopes = self.chain.clone();
        if let Some(team) = &self.team {
            scopes.push(Scope::Team(team.clone()));
        }
        if let Some(agent) = &self.agent {
            scopes.push(Scope::Agent(agent.clone()));
        }
        // Stable, so links of the same rank keep the order the chain gave them.
        scopes.sort_by_key(Scope::rank);
        scopes
    }
}

/// The room, and the three numbers that decide how it is spent.
pub struct Budget {
    pub render_chars: usize,
    pub per_item_chars: usize,
    pub floor_item_chars: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            render_chars: RENDER_CHARS,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        }
    }
}

/// One candidate's TRACE and not the signal: whether it was shown and the five scores that decided
/// why it won or lost.
pub struct Scored {
    pub knowledge_id: i64,
    pub shown: bool,
    pub s_fts: f64,
    pub s_sim: f64,
    pub s_scope: f64,
    pub s_structure: f64,
    pub s_recency: f64,
    pub s_use: f64,
    /// The weighted sum of the signals above — the one number the order uses, carried so the
    /// trace can say why a row won or lost.
    pub score: f64,
}

impl Scored {
    fn weighted(&self, query_embedded: bool) -> f64 {
        let w = weights(query_embedded);
        self.s_fts * w.fts
            + self.s_sim * w.sim
            + self.s_structure * w.structure
            + self.s_use * w.use_
            + self.s_scope * w.scope
            + self.s_recency * w.recency
    }
}

/// What a node reads, and what it was not shown.
pub struct Brief {
    pub block: Option<String>,
    /// One entry per candidate, shown or not, with the five signals — this is what `brief` writes to
    /// `run_knowledge`, and the reason the trace can answer WHICH signal elected a row.
    pub trace: Vec<Scored>,
}

/// Every column of the store, in the order the migration declares them.
///
/// One constant and not five copies: [`FromRow`] matches by name, so a query that forgets a column
/// fails at runtime on the row rather than at the call site, and the five readers below would each
/// have to be corrected separately every time the table grows.
pub(crate) const COLUMNS: &str =
    "id, layer, scope_kind, scope_id, source, generator, evidence, observations,
                       fingerprint, points_at, expires_after_runs, last_confirmed_at, shown_count,
                       outcome_count, green_count, last_shown_at, kind, title, body, status,
                       proposal_id, supersedes, origin_run_id, created_at, activated_at, ended_at";

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards that checkout location and worktree generation do not split one failure identity.
    #[test]
    fn the_same_failure_in_two_worktree_layouts_normalises_to_one_signature() {
        let legacy = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos-worktrees\nucleos\run-900372\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");
        let nested = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos\.nucleos\worktrees\run-900446\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");
        let git_bash = failure_signature(
            r#"error: failed to remove file `/c/Projects/nucleos/.nucleos/worktrees/run-900451/core/target/debug/nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");

        assert_eq!(legacy.fingerprint, nested.fingerprint);
        assert_eq!(legacy.fingerprint, git_bash.fingerprint);
    }

    /// Guards that stable normalisation preserves the facts that distinguish real failures.
    #[test]
    fn two_genuinely_different_failures_do_not_collapse_into_one_signature() {
        let access_denied = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos\.nucleos\worktrees\run-900446\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 5)"#,
        )
        .expect("a failure has a signature");
        let file_in_use = failure_signature(
            r#"error: failed to remove file `C:\Projects\nucleos\.nucleos\worktrees\run-900446\core\target\debug\nucleos-core.exe`: Acesso negado. (os error 32)"#,
        )
        .expect("a failure has a signature");
        let panic = failure_signature("thread 'x' panicked at src/a.rs:10:5:")
            .expect("a failure has a signature");

        assert_ne!(access_denied.fingerprint, file_in_use.fingerprint);
        assert_ne!(access_denied.fingerprint, panic.fingerprint);
        assert_ne!(file_in_use.fingerprint, panic.fingerprint);
    }

    /// Guards that volatile locations, timings, and run names do not change failure identity.
    #[test]
    fn a_failure_that_differs_only_in_line_numbers_durations_and_run_ids_is_the_same_failure() {
        let first = failure_signature(
            "thread 'a' panicked at src/a.rs:10:5: after 12.34s\nerror: branch nucleos/run-900372\nfinished in 12.34s",
        )
        .expect("a failure has a signature");
        let second = failure_signature(
            "thread 'a' panicked at src/a.rs:99:7: after 350ms\nerror: branch nucleos/run-900999\nfinished in 350ms",
        )
        .expect("a failure has a signature");

        assert_eq!(first.fingerprint, second.fingerprint);
    }

    /// Guards that silence has no identity while non-empty unclassified output keeps a fallback.
    #[test]
    fn an_output_with_nothing_in_it_has_no_signature() {
        assert!(failure_signature("").is_none());
        assert!(failure_signature("   \n\t").is_none());
        assert!(failure_signature("test result: ok. 3 passed").is_some());
    }

    /// The last version of the schema that still had `refinements` in it.
    ///
    /// Named rather than written as a literal in five places: what these tests are about is the
    /// boundary between before and after, and a bare `142` at a call site says nothing about which
    /// side of it the caller means to be on.
    const BEFORE: i64 = 142;

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn insert_job(pool: &SqlitePool, project_id: &str, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO jobs
               (project_id, project_root, rule_name, prompt, status, max_items, gate_each, review,
                gate_retries, head_sha, max_rounds, budget_usd, created_at, team_id)
             VALUES (?, ?, NULL, 'test job', ?, 1, 1, 0, 0, NULL, NULL, NULL,
                     '2026-09-20T00:00:00+00:00', NULL)",
        )
        .bind(project_id)
        .bind(format!("/project/{project_id}"))
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_job_knowledge(
        pool: &SqlitePool,
        job_id: i64,
        layer: &str,
        status: &str,
        title: &str,
    ) -> i64 {
        sqlx::query(
            r#"INSERT INTO knowledge
                 (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
               VALUES (?, 'job', ?, 'run', '[{"t":"run","id":1}]', 'memory', ?, 'b', ?,
                       '2026-09-20T00:00:00+00:00')"#,
        )
        .bind(layer)
        .bind(job_id.to_string())
        .bind(title)
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn note_finding_refuses_evidence_over_the_cap() {
        let pool = test_pool().await;
        let job_id = insert_job(&pool, "p", "running").await;
        let run_id: i64 = sqlx::query_scalar(
            "INSERT INTO runs (project_id, prompt, status, job_id, created_at)
             VALUES ('p', 'test run', 'running', ?, '2026-10-01T00:00:00+00:00')
             RETURNING id",
        )
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let evidence = serde_json::Value::Array(
            (1..=200)
                .map(|id| serde_json::json!({"t": "run", "id": id}))
                .collect(),
        );

        assert!(matches!(
            note_finding(&pool, run_id, "too much evidence", &evidence).await,
            Err(FindingError::EvidenceTooLong)
        ));
        let written: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge WHERE layer = 'working'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(written, 0);

        assert!(
            note_finding(
                &pool,
                run_id,
                "small evidence",
                &serde_json::json!([{"t": "run", "id": run_id}]),
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn a_finding_closes_with_its_job_through_the_one_place_that_ends_a_job() {
        assert_eq!(
            crate::execution::job::TERMINAL_STATUSES.len(),
            8,
            "a ninth ending needs a deliberate finding-lifetime decision"
        );
        let pool = test_pool().await;

        for (index, terminal) in crate::execution::job::TERMINAL_STATUSES.iter().enumerate() {
            let job_id = insert_job(&pool, &format!("retired-{index}"), "running").await;
            let other_job_id = insert_job(&pool, &format!("running-{index}"), "running").await;
            let finding = insert_job_knowledge(
                &pool,
                job_id,
                "working",
                "live",
                &format!("finding-{index}"),
            )
            .await;
            let other = insert_job_knowledge(
                &pool,
                other_job_id,
                "working",
                "live",
                &format!("other-{index}"),
            )
            .await;
            let approved = insert_job_knowledge(
                &pool,
                job_id,
                "semantic",
                "active",
                &format!("approved-{index}"),
            )
            .await;

            crate::execution::job::retire(&pool, job_id, terminal)
                .await
                .unwrap();

            let (status, ended_at): (String, Option<String>) =
                sqlx::query_as("SELECT status, ended_at FROM knowledge WHERE id = ?")
                    .bind(finding)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(
                status, "closed",
                "the `{terminal}` ending left its finding live"
            );
            assert!(ended_at.is_some(), "the closed finding has no ending time");
            let event: (String, String, String) = sqlx::query_as(
                "SELECT from_status, to_status, note FROM knowledge_events WHERE knowledge_id = ?",
            )
            .bind(finding)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(
                event,
                ("live".into(), "closed".into(), "the job ended".into())
            );
            let event_count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events WHERE knowledge_id = ?")
                    .bind(finding)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(event_count, 1, "retiring wrote more than one closing event");

            let other_status: String =
                sqlx::query_scalar("SELECT status FROM knowledge WHERE id = ?")
                    .bind(other)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(other_status, "live", "another running job lost its finding");
            let approved_row: (String, Option<String>) =
                sqlx::query_as("SELECT status, ended_at FROM knowledge WHERE id = ?")
                    .bind(approved)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(approved_row, ("active".into(), None));
        }
    }

    #[tokio::test]
    async fn an_orphaned_finding_is_closed_by_the_hourly_pass() {
        let pool = test_pool().await;
        let ended_job = insert_job(&pool, "ended", "running").await;
        sqlx::query("UPDATE jobs SET status = 'failed' WHERE id = ?")
            .bind(ended_job)
            .execute(&pool)
            .await
            .unwrap();
        let running_job = insert_job(&pool, "running", "running").await;
        let ended = insert_job_knowledge(&pool, ended_job, "working", "live", "ended").await;
        let missing = insert_job_knowledge(&pool, 9_999_999, "working", "live", "missing").await;
        let running = insert_job_knowledge(&pool, running_job, "working", "live", "running").await;

        assert_eq!(close_orphaned_working(&pool).await.unwrap(), 2);
        let statuses: Vec<(i64, String)> =
            sqlx::query_as("SELECT id, status FROM knowledge WHERE id IN (?, ?, ?) ORDER BY id")
                .bind(ended)
                .bind(missing)
                .bind(running)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            statuses,
            vec![
                (ended, "closed".into()),
                (missing, "closed".into()),
                (running, "live".into()),
            ]
        );
        assert_eq!(close_orphaned_working(&pool).await.unwrap(), 0);
    }

    #[test]
    fn the_consolidator_has_no_path_to_the_working_layer() {
        let production = include_str!("consolidate.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the consolidator has a test boundary")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !production.contains("working"),
            "the consolidator became a second writer for the working layer"
        );
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

        let mut read = for_scope(&pool, &Scope::Project("mine".into()))
            .await
            .unwrap();
        assert_eq!(
            read.len(),
            2,
            "the read is where the filtering happens, and it is not: {read:?}"
        );
        // A row reaches a prompt only when it matched the query; `for_scope` leaves `s_fts` at zero,
        // so mark both as matched and keep this test about the unknown layer.
        for row in &mut read {
            row.s_fts = 1.0;
        }

        // The row exists, is `active`, is in scope, and still reaches nothing. That is the whole of
        // what "no CHECK constraints" costs and the whole of what the Rust constants buy.
        // Rendering now groups by the reading context; the assertion remains about the unknown
        // layer, so it supplies the project context without changing what it proves.
        let block =
            render(&read, &project_context_for("mine")).expect("the known layer still renders");
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

    /// A job reads all three links of the chain: `machine` -> `project` -> `job`.
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

    #[tokio::test]
    async fn a_live_finding_is_read_by_its_own_job_and_no_other_job() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'forty-one finding', 'body', 'live', '2026-09-20T00:00:00+00:00'),
               ('working', 'job', '42', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'forty-two finding', 'body', 'live', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let scope = Scope::Job {
            id: 41,
            project: Some("p".into()),
        };
        let rows = for_scope(&pool, &scope).await.unwrap();
        assert!(rows.iter().any(|row| row.title == "forty-one finding"));
        assert!(!rows.iter().any(|row| row.title == "forty-two finding"));

        let brief = crate::brief::of(&pool, &job_context(41), "finding")
            .await
            .unwrap();
        let block = brief.block.expect("the finding reaches its own job");
        assert!(block.contains("forty-one finding"));
        assert!(!block.contains("forty-two finding"));
    }

    #[tokio::test]
    async fn a_live_row_outside_the_job_scope_is_never_fetched() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'project', 'p', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'project live row', 'body', 'live', '2026-09-20T00:00:00+00:00'),
               ('semantic', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'semantic live row', 'body', 'live', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = for_scope(
            &pool,
            &Scope::Job {
                id: 41,
                project: Some("p".into()),
            },
        )
        .await
        .unwrap();
        assert!(
            rows.is_empty(),
            "ineligible live rows were fetched: {rows:?}"
        );
    }

    #[tokio::test]
    async fn a_live_finding_is_labelled_as_said_by_a_run_and_not_approved() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'forty-one finding', 'body', 'live', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let brief = crate::brief::of(&pool, &job_context(41), "finding")
            .await
            .unwrap();
        let block = brief.block.expect("the finding renders");
        assert!(block.contains("said by a run of this job, not approved"));

        let approved = select(
            &[one(1, "memory", "approved semantic", "body")],
            &project_context(),
            &Budget::default(),
        )
        .block
        .expect("the approved semantic row renders");
        assert!(!approved.contains("said by a run of this job, not approved"));
    }

    #[tokio::test]
    async fn recall_never_answers_a_live_row() {
        let pool = test_pool().await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, evidence, kind, title, body, status, created_at)
             VALUES
               ('working', 'job', '41', 'run', '[{\"t\":\"run\",\"id\":1}]', 'memory',
                'live finding', 'the recallword is here', 'live',
                '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let recalled = crate::brief::recall(
            &pool,
            &Scope::Job {
                id: 41,
                project: Some("p".into()),
            },
            "recallword",
            None,
        )
        .await
        .unwrap();
        assert!(recalled.is_empty(), "recall answered a live row");
    }

    /// Recall used to search only the oldest `MAX_READ` rows of a scope, so a row past them was
    /// never found however well it matched.
    #[tokio::test]
    async fn recall_finds_a_row_newer_than_the_first_max_read() {
        let pool = test_pool().await;
        for n in 0..=MAX_READ {
            sqlx::query(
                "INSERT INTO knowledge
                   (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
                 VALUES ('semantic', 'project', 'p', 'owner', 'memory', ?, 'filler', 'active',
                         '2026-09-20T00:00:00+00:00')",
            )
            .bind(format!("row {n}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        let newest: i64 = sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'project', 'p', 'owner', 'memory', 'the needle',
                     'zanzibarword', 'active', '2026-09-20T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let recalled =
            crate::brief::recall(&pool, &Scope::Project("p".into()), "zanzibarword", None)
                .await
                .unwrap();
        assert_eq!(
            recalled.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![newest]
        );
    }

    /// No run behind the request means the owner made it, and both the row and its first event say
    /// so — an event note claiming a run nobody started would be a history that lies.
    #[tokio::test]
    async fn an_owner_declaration_is_recorded_as_the_owners() {
        let pool = test_pool().await;
        let (knowledge_id, _) = propose(
            &pool,
            Declaration {
                project_id: None,
                origin_run_id: None,
                kind: Kind::Memory,
                title: "Owner's lesson",
                body: "Written by hand.",
                reasoning: "the owner said so",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        let source: String = sqlx::query_scalar("SELECT source FROM knowledge WHERE id = ?")
            .bind(knowledge_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(source, "owner");
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'proposed'",
        )
        .bind(knowledge_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(note, "declared by the owner");
    }

    /// `propose_in` commits nothing itself: a caller that abandons its transaction leaves neither
    /// the lesson nor the question about it behind.
    #[tokio::test]
    async fn propose_in_rolls_back_with_its_transaction() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        propose_in(
            &mut tx,
            Declaration {
                project_id: Some("mine"),
                origin_run_id: None,
                kind: Kind::Memory,
                title: "Never landed",
                body: "Rolled back.",
                reasoning: "test",
                supersedes: None,
            },
        )
        .await
        .unwrap();
        tx.rollback().await.unwrap();

        let knowledge: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&pool)
            .await
            .unwrap();
        let proposals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proposals")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((knowledge, proposals), (0, 0));
    }

    /// Propose an episodic distilled row through `propose_in_with`; returns (knowledge id, proposal id).
    async fn propose_distilled_episode(pool: &sqlx::SqlitePool, title: &str) -> (i64, i64) {
        let mut tx = pool.begin().await.unwrap();
        let ids = propose_in_with(
            &mut tx,
            Declaration {
                project_id: Some("alpha"),
                origin_run_id: None,
                kind: Kind::Memory,
                title,
                body: "b",
                reasoning: "test",
                supersedes: None,
            },
            &Provenance {
                source: Some("distiller"),
                distill_cause: Some("job_failed"),
                evidence: None,
                points_at: None,
                fingerprint: None,
                layer: Some(Layer::Episodic),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        ids
    }

    async fn quoted_events(pool: &sqlx::SqlitePool, id: i64) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT from_status, to_status FROM knowledge_events
              WHERE knowledge_id = ? AND note = 'quoted_owner_text'",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_quoted_owner_text_event_keeps_the_status() {
        let pool = test_pool().await;
        let (id, _) = propose_distilled_episode(&pool, "quoting").await;
        let mut tx = pool.begin().await.unwrap();
        note_quoted_owner_text_in(&mut tx, id).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            quoted_events(&pool, id).await,
            vec![("proposed".to_owned(), "proposed".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_quoted_owner_text_event_rolls_back_with_its_transaction() {
        let pool = test_pool().await;
        let (id, _) = propose_distilled_episode(&pool, "quoting").await;
        let mut tx = pool.begin().await.unwrap();
        note_quoted_owner_text_in(&mut tx, id).await.unwrap();
        tx.rollback().await.unwrap();
        assert!(quoted_events(&pool, id).await.is_empty());
    }

    #[tokio::test]
    async fn arming_a_proposed_episode_sets_the_trial_and_no_confirmation() {
        let pool = test_pool().await;
        let (id, _) = propose_distilled_episode(&pool, "armed").await;
        let mut tx = pool.begin().await.unwrap();
        arm_trial_in(&mut tx, id).await.unwrap();
        tx.commit().await.unwrap();
        let row = fetch(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.expires_after_runs, Some(DISTILLED_TRIAL_RUNS));
        assert_eq!(row.last_confirmed_at, None);
    }

    #[tokio::test]
    async fn approving_an_armed_episode_starts_its_trial() {
        let pool = test_pool().await;
        let (id, proposal) = propose_distilled_episode(&pool, "armed").await;
        let mut tx = pool.begin().await.unwrap();
        arm_trial_in(&mut tx, id).await.unwrap();
        tx.commit().await.unwrap();
        approve(&pool, proposal).await.unwrap();
        let row = fetch(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "active");
        assert!(row.last_confirmed_at.is_some());
        assert_eq!(row.expires_after_runs, Some(50));
    }

    #[tokio::test]
    async fn approving_an_unarmed_row_leaves_its_confirmation_alone() {
        let pool = test_pool().await;
        let (id, proposal) = propose_distilled_episode(&pool, "unarmed").await;
        approve(&pool, proposal).await.unwrap();
        let row = fetch(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.last_confirmed_at, None);
        assert_eq!(row.expires_after_runs, None);
    }

    fn distilled<'a>(title: &'a str, body: &'a str, kind: Kind) -> Declaration<'a> {
        Declaration {
            project_id: Some("p"),
            origin_run_id: None,
            kind,
            title,
            body,
            reasoning: "distilled from a closed job",
            supersedes: None,
        }
    }

    /// The same sentence typed twice must be one learning: case, spacing and a final full stop are
    /// the differences a model makes from one answer to the next.
    #[test]
    fn a_title_fingerprint_ignores_case_spacing_and_final_punctuation() {
        let canonical = title_fingerprint("Run cargo fmt before the gate").unwrap();
        assert_eq!(canonical, "title:run cargo fmt before the gate");
        for variant in [
            "RUN CARGO FMT BEFORE THE GATE",
            "  Run   cargo\tfmt\nbefore  the gate  ",
            "Run cargo fmt before the gate.",
            "Run cargo fmt before the gate!?",
            "Run cargo fmt before the gate ...",
            "Run cargo fmt before the gate:",
        ] {
            assert_eq!(
                title_fingerprint(variant).as_deref(),
                Some(canonical.as_str())
            );
        }
        assert_ne!(
            title_fingerprint("Run cargo clippy before the gate").unwrap(),
            canonical
        );
        assert_eq!(title_fingerprint(""), None);
        assert_eq!(title_fingerprint("   \n\t "), None);
        assert_eq!(title_fingerprint(" ... "), None);
    }

    /// An episode a job taught is believed from the start, on trial: active, episodic, the
    /// distiller's, expiring unless somebody reconfirms it, with its evidence and its cause. It
    /// leaves the consolidator's two columns alone.
    #[tokio::test]
    async fn a_distilled_episode_is_recorded_active_on_trial() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let id = record_distilled(
            &mut tx,
            &distilled(
                "The linker lock",
                "The gate failed on a locked exe.",
                Kind::Memory,
            ),
            &Provenance {
                distill_cause: Some("gate_recovered"),
                evidence: Some(r#"[{"t":"job","id":7},{"t":"run","id":3}]"#),
                points_at: Some("core/src/main.rs"),
                fingerprint: Some("title:the linker lock"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let row = fetch(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.layer, "episodic");
        assert_eq!(row.source, "distiller");
        assert_eq!(row.status, "active");
        assert_eq!(row.kind, "memory");
        assert_eq!(row.scope_kind, "project");
        assert_eq!(row.scope_id.as_deref(), Some("p"));
        assert_eq!(row.expires_after_runs, Some(DISTILLED_TRIAL_RUNS));
        assert_eq!(DISTILLED_TRIAL_RUNS, 50);
        assert_eq!(row.fingerprint.as_deref(), Some("title:the linker lock"));
        assert_eq!(
            row.evidence.as_deref(),
            Some(r#"[{"t":"job","id":7},{"t":"run","id":3}]"#)
        );
        assert_eq!(row.points_at.as_deref(), Some("core/src/main.rs"));
        assert!(row.last_confirmed_at.is_some());
        assert!(row.activated_at.is_some());
        assert_eq!(row.observations, None);
        assert_eq!(row.generator, None);
        assert_eq!(row.proposal_id, None);
        assert_eq!(row.origin_run_id, None);
        let cause: Option<String> =
            sqlx::query_scalar("SELECT distill_cause FROM knowledge WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cause.as_deref(), Some("gate_recovered"));

        let events: Vec<(Option<String>, String, Option<String>)> = sqlx::query_as(
            "SELECT from_status, to_status, note FROM knowledge_events WHERE knowledge_id = ?",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, None);
        assert_eq!(events[0].1, "active");
        assert_eq!(events[0].2.as_deref(), Some("created"));
    }

    /// A rule a job taught is a question for a person, not a belief: it goes through the same door
    /// as any declaration and carries where it came from.
    #[tokio::test]
    async fn a_distilled_rule_is_proposed_with_its_provenance() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let (id, proposal_id) = propose_in_with(
            &mut tx,
            distilled("Format before gating", "Run fmt first.", Kind::Memory),
            &Provenance {
                source: Some("distiller"),
                distill_cause: Some("job_failed"),
                evidence: Some(r#"[{"t":"job","id":9}]"#),
                points_at: Some("scripts/gates.sh"),
                fingerprint: Some("title:format before gating"),
                layer: Some(Layer::Procedural),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let row = fetch(&pool, id).await.unwrap().unwrap();
        assert_eq!(row.status, "proposed");
        assert_eq!(row.source, "distiller");
        assert_eq!(row.layer, "procedural");
        assert_eq!(
            row.fingerprint.as_deref(),
            Some("title:format before gating")
        );
        assert_eq!(row.evidence.as_deref(), Some(r#"[{"t":"job","id":9}]"#));
        assert_eq!(row.points_at.as_deref(), Some("scripts/gates.sh"));
        assert_eq!(row.proposal_id, Some(proposal_id));
        assert_eq!(row.observations, None);
        assert_eq!(row.generator, None);
        let cause: Option<String> =
            sqlx::query_scalar("SELECT distill_cause FROM knowledge WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cause.as_deref(), Some("job_failed"));
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'proposed'",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(note, "distilled from a job");
        let kind: String = sqlx::query_scalar("SELECT kind FROM proposals WHERE id = ?")
            .bind(proposal_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind, "refinement");
    }

    /// `propose_in` is `propose_in_with` and a default provenance: what the door wrote before this
    /// change, byte for byte, for a declaration that names no provenance.
    #[tokio::test]
    async fn a_declaration_without_provenance_writes_what_it_always_wrote() {
        let pool = test_pool().await;
        let gate_body = "error: build failed because the linker refused output.exe";
        let expected = failure_signature(gate_body)
            .map(|signature| format!("gate:{}", signature.fingerprint))
            .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let (by_owner, _) = propose_in(&mut tx, distilled("A", gate_body, Kind::Memory))
            .await
            .unwrap();
        let (plain_id, _) = propose_in(&mut tx, distilled("B", "plain words", Kind::Skill))
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let owner = fetch(&pool, by_owner).await.unwrap().unwrap();
        assert_eq!(owner.source, "owner");
        assert_eq!(owner.layer, "semantic");
        assert_eq!(owner.status, "proposed");
        assert_eq!(owner.fingerprint.as_deref(), Some(expected.as_str()));
        assert_eq!(owner.evidence, None);
        assert_eq!(owner.points_at, None);
        let cause: Option<String> =
            sqlx::query_scalar("SELECT distill_cause FROM knowledge WHERE id = ?")
                .bind(by_owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cause, None);
        let note: String = sqlx::query_scalar(
            "SELECT note FROM knowledge_events WHERE knowledge_id = ? AND to_status = 'proposed'",
        )
        .bind(by_owner)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(note, "declared by the owner");

        let plain = fetch(&pool, plain_id).await.unwrap().unwrap();
        let plain_expected = failure_signature("plain words")
            .map(|signature| format!("gate:{}", signature.fingerprint))
            .unwrap();
        assert_eq!(plain.fingerprint.as_deref(), Some(plain_expected.as_str()));
        assert_eq!(plain.layer, "procedural");
    }

    /// The same learning twice is one row that was seen again: an active episode is renewed, a
    /// proposed rule is not (nobody has believed it yet), and both gain the new evidence and a
    /// same-status event that says so.
    #[tokio::test]
    async fn the_same_title_is_reconfirmed_not_repeated() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        let episode = record_distilled(
            &mut tx,
            &distilled("Seen twice", "An episode.", Kind::Memory),
            &Provenance {
                distill_cause: Some("job_landed"),
                evidence: Some(r#"[{"t":"job","id":7},{"t":"run","id":3}]"#),
                fingerprint: Some("title:seen twice"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let (rule, _) = propose_in_with(
            &mut tx,
            distilled("A pending rule", "A rule.", Kind::Memory),
            &Provenance {
                source: Some("distiller"),
                distill_cause: Some("job_landed"),
                evidence: Some(r#"[{"t":"job","id":7}]"#),
                fingerprint: Some("title:a pending rule"),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        sqlx::query(
            "UPDATE knowledge SET last_confirmed_at = '2020-01-01T00:00:00+00:00' WHERE id = ?",
        )
        .bind(episode)
        .execute(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        let again = reconfirm_in(
            &mut tx,
            "p",
            "title:seen twice",
            r#"[{"t":"run","id":3},{"t":"run","id":4},{"t":"vibes","id":1}]"#,
        )
        .await
        .unwrap();
        let again_rule = reconfirm_in(
            &mut tx,
            "p",
            "title:a pending rule",
            r#"[{"t":"job","id":8}]"#,
        )
        .await
        .unwrap();
        let nothing = reconfirm_in(&mut tx, "p", "title:never written", "[]")
            .await
            .unwrap();
        let other_project = reconfirm_in(&mut tx, "q", "title:seen twice", "[]")
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(again, Some(episode));
        assert_eq!(again_rule, Some(rule));
        assert_eq!(nothing, None);
        assert_eq!(other_project, None);

        let renewed = fetch(&pool, episode).await.unwrap().unwrap();
        assert_ne!(
            renewed.last_confirmed_at.as_deref(),
            Some("2020-01-01T00:00:00+00:00"),
            "an active episode seen again must be renewed"
        );
        let merged: serde_json::Value =
            serde_json::from_str(renewed.evidence.as_deref().unwrap()).unwrap();
        assert_eq!(
            merged,
            serde_json::json!([
                {"t":"job","id":7},
                {"t":"run","id":3},
                {"t":"run","id":4},
            ])
        );
        let reconfirmed: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT from_status, to_status, note FROM knowledge_events
             WHERE knowledge_id = ? AND note = ?",
        )
        .bind(episode)
        .bind(NOTE_RECONFIRMED)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(reconfirmed.len(), 1);
        assert_eq!(reconfirmed[0].0, "active");
        assert_eq!(reconfirmed[0].1, "active");

        let proposed = fetch(&pool, rule).await.unwrap().unwrap();
        assert_eq!(proposed.status, "proposed");
        assert_eq!(
            proposed.last_confirmed_at, None,
            "a proposed row is not renewed"
        );
        let merged: serde_json::Value =
            serde_json::from_str(proposed.evidence.as_deref().unwrap()).unwrap();
        assert_eq!(
            merged,
            serde_json::json!([{"t":"job","id":7},{"t":"job","id":8}])
        );
        let events: Vec<(String, String)> = sqlx::query_as(
            "SELECT from_status, to_status FROM knowledge_events
             WHERE knowledge_id = ? AND note = ?",
        )
        .bind(rule)
        .bind(NOTE_RECONFIRMED)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            events,
            vec![("proposed".to_string(), "proposed".to_string())]
        );

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM knowledge")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 2, "reconfirming must never insert");
    }

    /// A closed job is a thing a finding can point at.
    #[test]
    fn a_job_is_evidence() {
        assert!(EVIDENCE_TAGS.contains(&"job"));
        assert!(known_element(&serde_json::json!({"t": "job", "id": 7})));
        assert!(known_element(&serde_json::json!({"t": "job", "id": "7"})));
        assert!(!known_element(&serde_json::json!({"t": "job", "id": 0})));
        assert_eq!(
            tagged_evidence(&serde_json::json!([{"t": "job", "id": 7}])).as_deref(),
            Some(r#"[{"t":"job","id":7}]"#)
        );
    }

    /// Spec 4.6, in the four cases that matter: what the distiller wrote reaches a node exactly
    /// when it is `active` -- an episode on trial, or a rule a person approved -- and never while a
    /// rule is `proposed` or once an episode has expired.
    #[test]
    fn distilled_rows_reach_a_node_only_as_the_amendment_allows() {
        let context = project_context();
        let reaches = |layer: &str, status: &str, proposal_id: Option<i64>| {
            let mut row = one(1, "memory", "distilled", "learned from a job");
            row.source = "distiller".into();
            row.layer = layer.into();
            row.status = status.into();
            row.proposal_id = proposal_id;
            row.observations = None;
            row.generator = None;
            select(std::slice::from_ref(&row), &context, &Budget::default())
                .block
                .is_some()
        };

        assert!(
            reaches("episodic", "active", None),
            "an active distilled episode is the owner's D3 exception"
        );
        assert!(
            !reaches("semantic", "proposed", Some(1)),
            "a proposed distilled rule reached a node"
        );
        assert!(
            !reaches("procedural", "proposed", Some(1)),
            "a proposed distilled procedure reached a node"
        );
        assert!(
            reaches("semantic", "active", Some(1)),
            "an approved distilled rule must be read"
        );
        assert!(
            reaches("procedural", "active", Some(1)),
            "an approved distilled procedure must be read"
        );
        assert!(
            !reaches("episodic", "expired", None),
            "an expired distilled episode reached a node"
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
        let mut after = for_scope(&pool, &mine).await.unwrap();
        assert_eq!(after.len(), 1, "approving did not activate the lesson");
        // Only a row that matched the query renders; `for_scope` leaves `s_fts` at zero.
        for row in &mut after {
            row.s_fts = 1.0;
        }
        // Rendering now needs the scope chain that the store read represented; approval remains
        // the assertion under test rather than the new grouping.
        assert!(
            render(&after, &project_context_for("mine")).is_some(),
            "an active lesson renders nothing"
        );

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
            s_fts: 1.0,
            s_sim: 0.0,
            status: "active".into(),
            proposal_id: Some(1),
            supersedes: None,
            origin_run_id: Some(900_001),
            created_at: "2026-08-19T00:00:00+00:00".into(),
            activated_at: Some("2026-08-19T00:00:00+00:00".into()),
            ended_at: None,
        }
    }

    fn project_context() -> Context {
        project_context_for("p")
    }

    fn project_context_for(id: &str) -> Context {
        Context {
            chain: vec![Scope::Machine, Scope::Project(id.into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        }
    }

    fn job_context(id: i64) -> Context {
        Context {
            chain: vec![
                Scope::Machine,
                Scope::Project("p".into()),
                Scope::Job {
                    id,
                    project: Some("p".into()),
                },
            ],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        }
    }

    /// `prompt_budget.rs`, `budget.rs` and `token_efficiency.rs` exist to measure this: a briefing that
    /// changes at every node BREAKS THE PROMPT CACHE. So machine and project scope are written first —
    /// they do not change between nodes of the same job — and what the task chose comes last.
    #[test]
    fn two_nodes_of_the_same_job_read_the_same_bytes_before_the_bytes_that_are_theirs() {
        let mut machine = one(1, "memory", "house", "shared house fact");
        machine.scope_kind = "machine".into();
        machine.scope_id = None;
        let project = one(2, "memory", "project", "shared project fact");
        let mut left = one(3, "memory", "left", "left-node fact");
        left.scope_kind = "job".into();
        left.scope_id = Some("77".into());
        left.points_at = Some("core/src/left.rs".into());
        let mut right = one(4, "memory", "right", "right-node fact");
        right.scope_kind = "job".into();
        right.scope_id = Some("77".into());
        right.points_at = Some("core/src/right.rs".into());
        let known = [machine, project, left, right];
        let context = |file: &str| Context {
            chain: vec![
                Scope::Machine,
                Scope::Project("p".into()),
                Scope::Job {
                    id: 77,
                    project: Some("p".into()),
                },
            ],
            files: vec![file.into()],
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };

        let left = render(&known, &context("core/src/left.rs")).expect("left node is briefed");
        let right = render(&known, &context("core/src/right.rs")).expect("right node is briefed");
        let job_heading = "\n\nKnowledge about job 77:";
        let left_job = left
            .find(job_heading)
            .expect("left brief has its job group");
        let right_job = right
            .find(job_heading)
            .expect("right brief has its job group");

        assert_eq!(&left[..left_job], &right[..right_job]);
        assert_ne!(&left[left_job..], &right[right_job..]);
    }

    /// The layer is an item tag and not a heading, and that is what the reader needs to see: the tag is
    /// what distinguishes a MEASUREMENT from something a person approved.
    #[test]
    fn every_item_says_which_layer_it_came_from() {
        let semantic = one(1, "memory", "fact", "approved fact");
        let mut episodic = one(2, "memory", "measurement", "measured result");
        episodic.layer = Layer::Episodic.as_str().into();
        episodic.source = "consolidator".into();
        episodic.observations = Some(3);
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };

        let block = render(&[semantic, episodic], &context).expect("both items render");
        assert!(block.contains("\n- [semantic] fact: approved fact"));
        assert!(block.contains("\n- [episodic] measurement: measured result"));
        assert!(!block.contains("\n\nSemantic:"));
        assert!(!block.contains("\n\nEpisodic:"));
    }

    /// A project that has learned nothing must cost nothing. The block is appended to every node of
    /// every job, so an empty store that still wrote a heading would tax every run for ever.
    #[test]
    fn a_project_with_nothing_learned_adds_nothing_to_the_brief() {
        // Scope headings are written only for present groups, so the new grouped block must still
        // cost nothing when there is no knowledge.
        assert!(render(&[], &project_context()).is_none());
    }

    /// The failure this wording exists to prevent: a node handed a standing instruction INSTEAD of
    /// its task does the standing instruction. `notes::render` had to say the same thing, and its
    /// test asserts the item's own brief is still there beside it.
    #[test]
    fn the_block_adds_to_the_brief_rather_than_replacing_it() {
        // Scope groups replace kind headings, but the preamble still has to preserve the node's
        // own brief as the governing instruction.
        let block = render(
            &[one(
                1,
                "prompt",
                "Run fmt",
                "Always run cargo fmt before finishing.",
            )],
            &project_context(),
        )
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

    /// Scope groups deliberately replace the old per-kind headings: stable machine and project
    /// bytes precede the job's own bytes, whatever order the rows happened to arrive in.
    #[test]
    fn the_kinds_arrive_in_a_fixed_order_whatever_order_the_rows_do() {
        let mut job = one(4, "subagent", "job item", "s");
        job.scope_kind = "job".into();
        job.scope_id = Some("77".into());
        let project = one(3, "skill", "project item", "k");
        let mut machine = one(2, "memory", "machine item", "m");
        machine.scope_kind = "machine".into();
        machine.scope_id = None;
        let block = render(&[job, project, machine], &job_context(77)).expect("renders");
        let at = |needle: &str| {
            block
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        assert!(
            at("machine item") < at("project item"),
            "machine scope must come before project scope: {block}"
        );
        assert!(
            at("project item") < at("job item"),
            "project scope must come before job scope: {block}"
        );
        assert!(
            !block.contains("Standing instructions:"),
            "the removed kind heading came back: {block}"
        );
    }

    /// D6 is this test and nothing else. Without the `id` tail, two rows of the same scope with the same
    /// files and rank 0 tie, two distributions become indistinguishable, and "deterministic" is a word.
    /// `render`'s `sort_by_key` carries the reason in the house's own words: "what a node reads first
    /// is a property of the store, never of the order rows happened to come back from SQLite."
    #[test]
    fn two_candidates_that_tie_on_every_signal_are_still_ordered_the_same_way_every_time() {
        let mut higher_score = one(90, "subagent", "higher-score", "s");
        higher_score.layer = Layer::Working.as_str().into();
        higher_score.s_fts = 1.0;

        // `one()` rows now count as matched (s_fts 1.0); the losers are put back to an unmatched 0.0
        // so the ordering this test pins stays the original one.
        let mut tied_second = one(20, "memory", "tied-second", "m2");
        tied_second.s_fts = 0.0;
        let mut tied_first = one(10, "memory", "tied-first", "m1");
        tied_first.s_fts = 0.0;
        let mut prompt = one(80, "prompt", "prompt", "p");
        prompt.s_fts = 0.0;
        let mut skill = one(70, "skill", "skill", "k");
        skill.s_fts = 0.0;

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
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let ids = |rows: &[Known]| {
            ordered_candidates(rows, &context)
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

    #[test]
    fn the_order_is_score_then_layer_then_kind_then_id() {
        let mut highest_score = one(50, "subagent", "highest score", "working");
        highest_score.layer = Layer::Working.as_str().into();
        highest_score.s_fts = 0.5;
        // `one()` rows now count as matched (s_fts 1.0); all but the winner are put back to 0.0.
        let mut memory = one(40, "memory", "memory", "semantic");
        memory.s_fts = 0.0;
        let mut prompt = one(30, "prompt", "prompt", "procedural");
        prompt.s_fts = 0.0;
        let mut later_skill = one(20, "skill", "later skill", "procedural");
        later_skill.s_fts = 0.0;
        let mut earlier_skill = one(10, "skill", "earlier skill", "procedural");
        earlier_skill.s_fts = 0.0;
        let rows = vec![highest_score, memory, prompt, later_skill, earlier_skill];
        let ids = |rows: &[Known]| {
            ordered_candidates(rows, &project_context())
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>()
        };

        assert_eq!(ids(&rows), vec![50, 40, 30, 10, 20]);
        let reversed = rows.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(ids(&reversed), vec![50, 40, 30, 10, 20]);
    }

    #[test]
    fn every_signal_and_the_score_are_on_the_unit_scale() {
        let context = Context {
            chain: vec![
                Scope::Machine,
                Scope::Project("p".into()),
                Scope::Job {
                    id: 77,
                    project: Some("p".into()),
                },
            ],
            files: vec!["a.rs".into(), "b.rs".into()],
            communities: vec!["core".into()],
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let mut r1 = one(1, "memory", "best", "job");
        r1.scope_kind = "job".into();
        r1.scope_id = Some("77".into());
        r1.points_at = Some(r#"["a.rs","b.rs","core"]"#.into());
        r1.s_fts = 7.0;
        r1.shown_count = 4;
        r1.outcome_count = 4;
        r1.green_count = 3;
        r1.last_shown_at = Some("2026-09-21T00:00:00+00:00".into());
        let mut r2 = one(2, "memory", "failed", "machine");
        r2.scope_kind = "machine".into();
        r2.scope_id = None;
        r2.s_fts = -2.0;
        r2.shown_count = 2;
        r2.outcome_count = 2;
        r2.last_shown_at = Some("2026-09-01T00:00:00+00:00".into());
        let mut r3 = one(3, "memory", "unknown", "project");
        r3.s_fts = f64::NAN;
        let rows = vec![r1, r2, r3];

        let scored = scored_candidates(&rows, &context);
        for (row, candidate) in &scored {
            for (name, value) in [
                ("s_fts", candidate.s_fts),
                ("s_scope", candidate.s_scope),
                ("s_structure", candidate.s_structure),
                ("s_recency", candidate.s_recency),
                ("s_use", candidate.s_use),
                ("score", candidate.score),
            ] {
                assert!(
                    (0.0..=1.0).contains(&value),
                    "row {} has {name} outside the unit scale: {value}",
                    row.id
                );
            }
        }
        let fts = |id| {
            scored
                .iter()
                .find(|(row, _)| row.id == id)
                .map(|(_, candidate)| candidate.s_fts)
                .expect("row is scored")
        };
        assert_eq!(fts(1), 1.0);
        assert_eq!(fts(2), 0.0);
        assert_eq!(fts(3), 0.0);
        assert_eq!(ordered_candidates(&rows, &context)[0].id, 1);

        let expected = scored
            .iter()
            .find(|(row, _)| row.id == 1)
            .map(|(_, candidate)| candidate.score)
            .expect("row 1 is scored");
        let traced = select(&rows, &context, &Budget::default())
            .trace
            .into_iter()
            .find(|candidate| candidate.knowledge_id == 1)
            .map(|candidate| candidate.score)
            .expect("row 1 reaches the trace");
        assert_eq!(traced, expected);
    }

    /// Spec 5.2: the six weights (with similarity) and today's five (without) are two tables, and each
    /// must sum to 1.0 so a score stays on the unit scale. Without similarity the FTS weight absorbs
    /// the similarity share, so every score a briefing without a query vector produced stays as it was.
    #[test]
    fn both_weight_tables_sum_to_one_and_the_absent_one_is_todays_five() {
        let sum = |w: &Weights| w.fts + w.sim + w.structure + w.use_ + w.scope + w.recency;
        assert!(
            (sum(&WITH_SIM) - 1.0).abs() < 1e-9,
            "WITH_SIM does not sum to 1.0"
        );
        assert!(
            (sum(&WITHOUT_SIM) - 1.0).abs() < 1e-9,
            "WITHOUT_SIM does not sum to 1.0"
        );

        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(WITHOUT_SIM.fts, 0.35));
        assert!(near(WITHOUT_SIM.sim, 0.0));
        assert!(near(WITHOUT_SIM.structure, 0.20));
        assert!(near(WITHOUT_SIM.use_, 0.20));
        assert!(near(WITHOUT_SIM.scope, 0.15));
        assert!(near(WITHOUT_SIM.recency, 0.10));

        assert!(near(WITH_SIM.fts, 0.20));
        assert!(near(WITH_SIM.sim, 0.15));
        assert!(near(WITH_SIM.structure, 0.20));
        assert!(near(WITH_SIM.use_, 0.20));
        assert!(near(WITH_SIM.scope, 0.15));
        assert!(near(WITH_SIM.recency, 0.10));

        // The guarantee the use weight documents must hold in both tables.
        for (name, w) in [("WITH_SIM", &WITH_SIM), ("WITHOUT_SIM", &WITHOUT_SIM)] {
            assert!(
                w.use_ * NEUTRAL_UTILITY >= w.recency,
                "{name}: a never-shown row could score below a failed row"
            );
        }
    }

    /// When the briefing has a query vector, the row closer to it wins, and the trace says by how much.
    #[test]
    fn with_a_query_vector_the_row_closer_to_it_scores_higher_and_the_trace_carries_both() {
        let mut far = one(1, "memory", "far", "same body");
        far.s_sim = 0.1;
        let mut near = one(2, "memory", "near", "same body");
        near.s_sim = 0.9;
        let rows = vec![far, near];
        let mut context = project_context();
        context.query_embedded = true;

        let brief = select(&rows, &context, &Budget::default());
        let by_id = |id: i64| {
            brief
                .trace
                .iter()
                .find(|row| row.knowledge_id == id)
                .expect("row reaches the trace")
        };
        assert_eq!(by_id(1).s_sim, 0.1);
        assert_eq!(by_id(2).s_sim, 0.9);
        assert!(
            by_id(2).score > by_id(1).score,
            "the closer row does not score higher: {} vs {}",
            by_id(2).score,
            by_id(1).score
        );
        let block = brief.block.expect("both rows are shown");
        assert!(
            block.find("near").expect("near is shown") < block.find("far").expect("far is shown"),
            "the closer row is not shown first: {block}"
        );
    }

    /// Absence is per briefing, not per row (spec 5.2): without a query vector the similarity column
    /// weighs nothing, so rows that differ only in it score alike.
    #[test]
    fn without_a_query_vector_similarity_weighs_nothing() {
        let mut far = one(1, "memory", "far", "same body");
        far.s_sim = 0.1;
        let mut near = one(2, "memory", "near", "same body");
        near.s_sim = 0.9;
        let rows = vec![far, near];
        let mut context = project_context();
        context.query_embedded = false;

        let brief = select(&rows, &context, &Budget::default());
        let score = |id: i64| {
            brief
                .trace
                .iter()
                .find(|row| row.knowledge_id == id)
                .map(|row| row.score)
                .expect("row reaches the trace")
        };
        assert_eq!(
            score(1),
            score(2),
            "similarity leaked into a briefing without a query vector"
        );
    }

    /// A new row does not compete from the bottom as if it had failed: the selection treats absence as
    /// absence, and what that is worth as a NUMBER is decided here and tested in a table. Without this,
    /// whoever implements it picks a value by taste and D6's table tests that taste.
    ///
    /// The utility component of a row with no outcome is the MEDIAN of the candidates that have one,
    /// floored at the neutral constant, and the neutral constant when none of them does.
    #[test]
    fn a_row_nobody_has_measured_scores_like_the_middle_of_the_ones_somebody_has() {
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let cases = [
            ("odd", vec![(1, 5), (3, 5), (5, 5)], 0.6),
            ("even", vec![(1, 5), (4, 5)], 0.5),
            ("single", vec![(3, 4)], 0.75),
            ("mostly failed", vec![(0, 5), (0, 5), (5, 5)], 0.5),
            ("low", vec![(1, 5), (1, 5)], 0.5),
            ("none", Vec::new(), NEUTRAL_UTILITY),
        ];

        for (name, measured, expected) in cases {
            let mut rows = vec![one(100, "memory", "not measured", "new")];
            for (offset, (green_count, outcome_count)) in measured.into_iter().enumerate() {
                let mut row = one(offset as i64 + 1, "memory", "measured", "old");
                row.green_count = green_count;
                row.outcome_count = outcome_count;
                rows.push(row);
            }

            let actual = scored_candidates(&rows, &context)
                .into_iter()
                .find(|(row, _)| row.id == 100)
                .map(|(_, scored)| scored.s_use)
                .expect("the unmeasured row is scored");
            assert!(
                (actual - expected).abs() < f64::EPSILON,
                "{name}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn a_never_shown_row_never_scores_below_a_recent_failure() {
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let mut recent_failure = one(1, "memory", "recent failure", "bad");
        recent_failure.shown_count = 4;
        recent_failure.outcome_count = 4;
        recent_failure.last_shown_at = Some("2026-09-20T00:00:00+00:00".into());
        let mut old_failure = one(2, "memory", "old failure", "bad");
        old_failure.shown_count = 4;
        old_failure.outcome_count = 4;
        old_failure.last_shown_at = Some("2026-09-01T00:00:00+00:00".into());
        let mut old_green = one(3, "memory", "old green", "good");
        old_green.shown_count = 4;
        old_green.outcome_count = 4;
        old_green.green_count = 4;
        old_green.last_shown_at = Some("2026-09-01T00:00:00+00:00".into());
        let never_shown = one(4, "memory", "never shown", "new");
        let rows = vec![recent_failure, old_failure, old_green, never_shown];

        let scored = scored_candidates(&rows, &context);
        let score = |id| {
            scored
                .iter()
                .find(|(row, _)| row.id == id)
                .map(|(_, scored)| scored)
                .expect("row is scored")
        };
        assert!(
            score(4).score >= score(1).score,
            "a never-shown row must not score below a recent measured failure"
        );
        assert_eq!(score(4).s_use, NEUTRAL_UTILITY);
        assert_eq!(score(4).s_recency, 0.0);
    }

    /// This is the realistic fixture: every signal except utility is equal, because the pass that
    /// records an outcome also records the show at the same instant. The never-shown row's recency
    /// is absence, not zero, so it must not order like an old measured failure.
    #[test]
    fn not_measured_does_not_order_like_measured_and_failed() {
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let not_measured = one(1, "memory", "not measured", "new");
        let mut failed = one(2, "memory", "failed", "bad");
        failed.shown_count = 4;
        failed.outcome_count = 4;
        failed.last_shown_at = Some("2026-09-20T00:00:00+00:00".into());
        let mut green = one(3, "memory", "green", "good");
        green.shown_count = 4;
        green.outcome_count = 4;
        green.green_count = 4;
        green.last_shown_at = Some("2026-09-20T00:00:00+00:00".into());
        let rows = vec![failed, not_measured, green];

        let scored = scored_candidates(&rows, &context);
        let utility = |id| {
            scored
                .iter()
                .find(|(row, _)| row.id == id)
                .map(|(_, scored)| scored.s_use)
                .expect("row is scored")
        };
        assert_ne!(utility(1), utility(2));
        assert_eq!(
            ordered_candidates(&rows, &context)
                .into_iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![3, 1, 2]
        );
    }

    /// A store that grows for a year would take the context the work needs, and the failure mode is
    /// silent: a prompt does not get slower, it gets emptier of room. So it is bounded, and it says
    /// what it left out rather than trimming in silence.
    #[test]
    fn the_block_is_bounded_and_says_what_it_left_out() {
        let many: Vec<Known> = (1..=60)
            .map(|i| one(i, "memory", &format!("fact {i}"), &"x".repeat(300)))
            .collect();
        // The new selector accounts for each scope heading and its own cut notice, so the old loose
        // byte bound becomes the exact character ceiling the grouped block promises.
        let block = render(&many, &project_context()).expect("renders");
        assert!(
            block.chars().count() <= RENDER_CHARS,
            "unbounded: {} chars from {} rows",
            block.chars().count(),
            many.len()
        );
        assert!(
            block.contains("not shown") || block.contains("more"),
            "trimmed in silence, which is the one way this may not fail: {block}"
        );
    }

    /// The test `the_block_is_bounded_and_says_what_it_left_out` cannot write today — it only
    /// asserts `<= RENDER_CHARS * 2`, because the cut notice is appended AFTER the loop. Two passes
    /// make the real ceiling assertable.
    #[test]
    fn the_block_never_passes_the_ceiling_however_much_is_on_offer() {
        let many: Vec<Known> = (1..=60)
            .map(|i| one(i, "memory", &format!("fact {i}"), &"x".repeat(1_000)))
            .collect();
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let budget = Budget {
            render_chars: RENDER_CHARS,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&many, &context, &budget);
        let block = brief.block.expect("some of sixty active rows render");
        assert!(
            block.chars().count() <= RENDER_CHARS,
            "the block passed its ceiling: {} > {RENDER_CHARS}",
            block.chars().count()
        );
        assert!(block.contains("not shown"), "the cut was silent: {block}");
    }

    /// A floor reserved for a layer with no candidate is dead budget, and two layers are born empty.
    #[test]
    fn a_layer_with_nothing_in_it_reserves_no_floor() {
        let mut first = one(1, "memory", "first", &"a".repeat(301));
        first.s_fts = 0.9;
        let mut second = one(2, "memory", "second", &"b".repeat(301));
        second.s_fts = 0.6;
        let mut third = one(3, "memory", "third", "small scored remainder");
        third.s_fts = 0.3;
        let known = [first, second, third];
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let budget = Budget {
            render_chars: 1_100,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&known, &context, &budget);
        assert_eq!(
            brief.trace.iter().filter(|row| row.shown).count(),
            3,
            "empty episodic, procedural, and working layers consumed room: {:?}",
            brief
                .trace
                .iter()
                .map(|row| (row.knowledge_id, row.shown))
                .collect::<Vec<_>>()
        );
    }

    /// Structural first: the current preamble, one heading per PRESENT SCOPE GROUP, and one cut
    /// notice per group that was actually cut. Groups of scope and not of layer — a block ordered
    /// by scope interleaves layers, so a per-layer heading has nowhere to sit.
    #[test]
    fn the_headings_and_the_notices_come_off_the_top_before_any_floor_is_reserved() {
        // The preamble no longer claims every item was approved because §4.5 names two exceptions;
        // the structural reservation still measures those exact bytes before choosing any item.
        let preamble = PREAMBLE;
        assert!(preamble.contains("two named admission rules"));
        let machine_heading = "\n\nHouse-wide knowledge:";
        let project_heading = "\n\nKnowledge about project p:";
        let notice = "\n\n(1 further admitted note in this scope is not shown here, to leave room for the work.)";
        let structural_chars = [preamble, machine_heading, project_heading, notice, notice]
            .into_iter()
            .map(|part| part.chars().count())
            .sum();

        let mut machine = one(1, "memory", "machine item", &"m".repeat(301));
        machine.scope_kind = "machine".into();
        machine.scope_id = None;
        let project = one(2, "memory", "project item", &"p".repeat(301));
        let known = [machine, project];
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let budget = Budget {
            render_chars: structural_chars,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&known, &context, &budget);
        let block = brief
            .block
            .expect("present groups still render their structure");
        assert_eq!(block.chars().count(), structural_chars);
        assert!(block.contains(machine_heading));
        assert!(block.contains(project_heading));
        assert_eq!(block.matches(notice).count(), 2);
        assert!(!block.contains("machine item"));
        assert!(!block.contains("project item"));
        assert!(brief.trace.iter().all(|row| !row.shown));
    }

    /// The two clips are different numbers and the test says which is which.
    #[test]
    fn a_floor_item_is_cut_at_three_hundred_and_one_that_won_on_score_at_six_hundred() {
        let mut floor = one(1, "memory", "floor", &"f".repeat(301));
        floor.s_fts = 0.9;
        let mut other_floor = one(2, "memory", "other floor", "short");
        other_floor.s_fts = 0.6;
        let mut scored = one(3, "memory", "score", &"s".repeat(601));
        scored.s_fts = 0.3;
        let known = [floor, other_floor, scored];
        let context = Context {
            chain: vec![Scope::Machine, Scope::Project("p".into())],
            files: Vec::new(),
            communities: Vec::new(),
            node: None,
            gate: None,
            query_embedded: false,
            agent: None,
            team: None,
        };
        let budget = Budget {
            render_chars: RENDER_CHARS,
            per_item_chars: PER_ITEM_CHARS,
            floor_item_chars: FLOOR_ITEM_CHARS,
        };

        let brief = select(&known, &context, &budget);
        let block = brief.block.expect("all three rows fit");
        assert!(
            block.contains(&format!("{}…", "f".repeat(FLOOR_ITEM_CHARS))),
            "the floor item was not clipped at {FLOOR_ITEM_CHARS}: {block}"
        );
        assert!(
            block.contains(&format!("{}…", "s".repeat(PER_ITEM_CHARS))),
            "the score winner was not clipped at {PER_ITEM_CHARS}: {block}"
        );
        assert_eq!(brief.trace.iter().filter(|row| row.shown).count(), 3);
    }

    /// The admission rule is asserted on `select`, the seam every caller passes through. Active
    /// rows still represent approval. The only rows that may bypass it are (a) an `active`
    /// consolidator `episodic` row with observations and (b) a `live` run `working` row with
    /// well-tagged evidence, read only inside the job that wrote it. Enumerating the surrounding
    /// status/source/layer space makes a third exception fail here rather than reach a prompt.
    #[test]
    fn nothing_that_a_person_has_not_approved_reaches_a_node() {
        let context = job_context(77);
        let statuses = ["active", "proposed", "live", "rejected", "reverted"];
        let sources = ["owner", "run", "consolidator", "distiller"];
        let layers = ["semantic", "episodic", "procedural", "working"];
        let mut id = 0;

        for status in statuses {
            for source in sources {
                for layer in layers {
                    id += 1;
                    let title = format!("{status}/{source}/{layer}");
                    let mut row = one(id, "memory", &title, "candidate");
                    row.status = status.into();
                    row.source = source.into();
                    row.layer = layer.into();
                    row.proposal_id =
                        (status == "active" && source != "consolidator").then_some(id);
                    if source == "consolidator" {
                        row.observations = Some(1);
                    }
                    if status == "live" {
                        row.scope_kind = "job".into();
                        row.scope_id = Some("77".into());
                        row.evidence = Some(r#"[{"t":"run","id":900001}]"#.into());
                    }

                    let measured_exception = status == "active"
                        && source == "consolidator"
                        && layer == "episodic"
                        && row.observations.is_some();
                    let working_exception = status == "live"
                        && source == "run"
                        && layer == "working"
                        && row.evidence.as_deref().is_some_and(evidence_is_tagged);
                    let approved = status == "active" && source != "consolidator";
                    assert_eq!(
                        select(std::slice::from_ref(&row), &context, &Budget::default(),)
                            .block
                            .is_some(),
                        approved || measured_exception || working_exception,
                        "the admission rule was wrong for {title}"
                    );
                    assert_eq!(
                        select(std::slice::from_ref(&row), &context, &Budget::default(),)
                            .trace
                            .iter()
                            .any(|scored| scored.knowledge_id == id),
                        approved || measured_exception || working_exception,
                        "a row the admission rule refuses was scored as a candidate for {title}"
                    );
                }
            }
        }

        let mut measurement_without_observations = one(100, "memory", "unmeasured", "body");
        measurement_without_observations.source = "consolidator".into();
        measurement_without_observations.layer = "episodic".into();
        measurement_without_observations.proposal_id = None;
        measurement_without_observations.observations = None;

        let mut working_without_evidence = one(101, "memory", "unsupported", "body");
        working_without_evidence.status = "live".into();
        working_without_evidence.source = "run".into();
        working_without_evidence.layer = "working".into();
        working_without_evidence.scope_kind = "job".into();
        working_without_evidence.scope_id = Some("77".into());
        working_without_evidence.evidence = Some("   ".into());

        let mut another_jobs_working_fact = working_without_evidence.clone();
        another_jobs_working_fact.id = 102;
        another_jobs_working_fact.title = "another job's".into();
        another_jobs_working_fact.scope_id = Some("78".into());
        another_jobs_working_fact.evidence = Some(r#"[{"t":"run","id":900002}]"#.into());

        let mut working_with_unknown_tag = working_without_evidence.clone();
        working_with_unknown_tag.id = 103;
        working_with_unknown_tag.title = "unknown evidence tag".into();
        working_with_unknown_tag.evidence = Some(r#"[{"t":"vibes","id":1}]"#.into());

        let mut working_with_untagged_evidence = working_without_evidence.clone();
        working_with_untagged_evidence.id = 104;
        working_with_untagged_evidence.title = "untagged evidence".into();
        working_with_untagged_evidence.evidence = Some("run:900001".into());

        assert!(
            select(
                &[
                    measurement_without_observations,
                    working_without_evidence,
                    another_jobs_working_fact,
                    working_with_unknown_tag,
                    working_with_untagged_evidence,
                ],
                &context,
                &Budget::default(),
            )
            .block
            .is_none(),
            "an incomplete or cross-job exception reached a node's prompt"
        );
    }

    /// Nothing a person has not approved reaches a node, except (a) an `active` consolidator
    /// `episodic` row with measured observations (including a merge successor), and (b) a `live`
    /// run `working` row with well-tagged evidence read only by the job that wrote it.
    ///
    /// This test FAILS if a third appears: an `active` row with no `proposal_id` that is not
    /// consolidator-episodic-measured, a `live` row reaching a run of another job, or a `live` row
    /// whose `evidence` is empty or badly tagged. This door checks only that evidence exists and is
    /// tagged; it does not validate that the sentence is true.
    #[tokio::test]
    async fn nothing_a_person_has_not_approved_reaches_a_node_but_the_two_named_exceptions() {
        let pool = test_pool().await;

        seed(&pool, Some("p"), "active", "owner approved").await;
        let owner_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'owner approved'")
                .fetch_one(&pool)
                .await
                .unwrap();

        let (approved_id, approved_proposal) = propose(
            &pool,
            Declaration {
                project_id: Some("p"),
                origin_run_id: Some(900_001),
                kind: Kind::Prompt,
                title: "run lesson approved",
                body: "approved body",
                reasoning: "the run declared it",
                supersedes: None,
            },
        )
        .await
        .unwrap();
        approve(&pool, approved_proposal).await.unwrap();
        let (proposed_id, _) = propose(
            &pool,
            Declaration {
                project_id: Some("p"),
                origin_run_id: Some(900_002),
                kind: Kind::Prompt,
                title: "run lesson proposed",
                body: "not approved",
                reasoning: "still waiting",
                supersedes: None,
            },
        )
        .await
        .unwrap();

        // There is no public one-row consolidator writer: its production writer is per-pass
        // machinery. These rows therefore reproduce that writer's persisted columns directly.
        sqlx::query(
            r#"INSERT INTO knowledge
                 (layer, scope_kind, scope_id, source, generator, evidence, observations, kind,
                  title, body, status, proposal_id, supersedes, created_at)
               VALUES
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 4, 'memory', 'measured episode', 'measured',
                  'active', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', NULL, 'memory', 'unmeasured episode', 'unmeasured',
                  'active', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('semantic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 4, 'memory', 'semantic consolidation', 'semantic',
                  'active', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 4, 'memory', 'proposed measurement', 'proposed',
                  'proposed', NULL, NULL, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', 8, 'memory', 'measured merge successor', 'merged',
                  'active', NULL, 4, '2026-10-01T00:00:00+00:00'),
                 ('episodic', 'project', 'p', 'consolidator', 'gate',
                  '[{"t":"run","id":1}]', NULL, 'memory', 'unmeasured merge successor',
                  'not measured', 'active', NULL, 4, '2026-10-01T00:00:00+00:00')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let measured_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'measured episode'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let unmeasured_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'unmeasured episode'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let semantic_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'semantic consolidation'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let proposed_measurement_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'proposed measurement'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let measured_successor_id: i64 =
            sqlx::query_scalar("SELECT id FROM knowledge WHERE title = 'measured merge successor'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let unmeasured_successor_id: i64 = sqlx::query_scalar(
            "SELECT id FROM knowledge WHERE title = 'unmeasured merge successor'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        for job_id in [77_i64, 78] {
            sqlx::query(
                "INSERT INTO jobs
                   (id, project_id, project_root, rule_name, prompt, status, max_items, gate_each,
                    review, gate_retries, head_sha, max_rounds, budget_usd, created_at, team_id)
                 VALUES (?, 'p', '/project/p', NULL, 'test job', 'running', 1, 1, 0, 0, NULL,
                         NULL, NULL, '2026-10-01T00:00:00+00:00', NULL)",
            )
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO runs (id, project_id, prompt, status, job_id, created_at)
                 VALUES (?, 'p', 'test run', 'running', ?, '2026-10-01T00:00:00+00:00')",
            )
            .bind(9_000 + job_id)
            .bind(job_id)
            .execute(&pool)
            .await
            .unwrap();
        }
        let finding_77_id = note_finding(
            &pool,
            9_077,
            "finding for job 77",
            &serde_json::json!([{"t": "run", "id": 9_077}]),
        )
        .await
        .unwrap();
        let finding_78_id = note_finding(
            &pool,
            9_078,
            "finding for job 78",
            &serde_json::json!([{"t": "run", "id": 9_078}]),
        )
        .await
        .unwrap();

        sqlx::query(
            r#"INSERT INTO knowledge
                 (layer, scope_kind, scope_id, source, evidence, kind, title, body, status,
                  created_at)
               VALUES
                 ('working', 'job', '77', 'run', '[{"t":"vibes","id":1}]', 'memory',
                  'unknown tag', 'bad tag', 'live', '2026-10-01T00:00:00+00:00'),
                 ('working', 'job', '77', 'run', 'run:1', 'memory',
                  'untagged evidence', 'not JSON', 'live', '2026-10-01T00:00:00+00:00'),
                 ('working', 'job', '77', 'run', '[]', 'memory',
                  'empty evidence', 'empty array', 'live', '2026-10-01T00:00:00+00:00'),
                 ('working', 'project', 'p', 'run', '[{"t":"run","id":1}]', 'memory',
                  'project live row', 'wrong scope', 'live', '2026-10-01T00:00:00+00:00')"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        let bad_live_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM knowledge WHERE title IN
             ('unknown tag', 'untagged evidence', 'empty evidence', 'project live row')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();

        let brief_77 = crate::brief::of(&pool, &job_context(77), "approved measured merge finding")
            .await
            .unwrap();
        let traced_77: std::collections::BTreeSet<i64> =
            brief_77.trace.iter().map(|row| row.knowledge_id).collect();
        let admitted_77 = std::collections::BTreeSet::from([
            owner_id,
            approved_id,
            measured_id,
            measured_successor_id,
            finding_77_id,
        ]);
        assert_eq!(
            traced_77, admitted_77,
            "a third exception entered the trace"
        );
        assert!(
            brief_77.trace.iter().all(|row| row.shown),
            "the admitted rows did not all reach the block"
        );
        let block_77 = brief_77.block.expect("the admitted set builds a briefing");
        for admitted_title in [
            "owner approved",
            "run lesson approved",
            "measured episode",
            "measured merge successor",
            "finding for job 77",
        ] {
            assert!(
                block_77.contains(admitted_title),
                "the block omitted admitted row {admitted_title}"
            );
        }
        for refused_title in [
            "run lesson proposed",
            "unmeasured episode",
            "semantic consolidation",
            "proposed measurement",
            "unmeasured merge successor",
            "finding for job 78",
            "unknown tag",
            "untagged evidence",
            "empty evidence",
            "project live row",
        ] {
            assert!(
                !block_77.contains(refused_title),
                "the block included refused row {refused_title}"
            );
        }
        for refused_id in [
            proposed_id,
            unmeasured_id,
            semantic_id,
            proposed_measurement_id,
            unmeasured_successor_id,
            finding_78_id,
        ]
        .into_iter()
        .chain(bad_live_ids.iter().copied())
        {
            assert!(
                !brief_77
                    .trace
                    .iter()
                    .any(|row| row.knowledge_id == refused_id),
                "refused row {refused_id} entered job 77's trace"
            );
        }

        let brief_78 = crate::brief::of(&pool, &job_context(78), "approved measured merge finding")
            .await
            .unwrap();
        assert!(
            brief_78
                .trace
                .iter()
                .any(|row| row.knowledge_id == finding_78_id),
            "job 78 did not receive its own finding"
        );
        assert!(
            !brief_78
                .trace
                .iter()
                .any(|row| row.knowledge_id == finding_77_id),
            "job 78 received job 77's finding"
        );

        let unapproved_active_runs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
              WHERE source = 'run' AND status = 'active' AND proposal_id IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unapproved_active_runs, 0);
        let wrongly_shaped_live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM knowledge
              WHERE status = 'live'
                AND NOT (layer = 'working' AND scope_kind = 'job' AND source = 'run')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            wrongly_shaped_live, 1,
            "a writer produced a stray live row beyond the deliberate project-scope fixture"
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

    /// The agent loadout: team and agent scopes, the relevance threshold, and approving a row into
    /// a narrower scope than the one it was declared in.
    mod loadout {
        use super::*;

        /// A row of memory kind in the given scope; `one()` already counts it as matched.
        fn scoped_row(id: i64, scope_kind: &str, scope_id: Option<&str>, title: &str) -> Known {
            let mut row = one(id, "memory", title, "body");
            row.scope_kind = scope_kind.into();
            row.scope_id = scope_id.map(str::to_owned);
            row
        }

        /// The ordinary project context of the other tests, plus the two new links.
        fn linked(agent: Option<&str>, team: Option<&str>) -> Context {
            let mut context = Context::for_project(Some("p"));
            context.agent = agent.map(str::to_owned);
            context.team = team.map(str::to_owned);
            context
        }

        fn columns_of(scopes: &[Scope]) -> Vec<(&'static str, Option<String>)> {
            scopes.iter().map(Scope::columns).collect()
        }

        fn scope_signal(brief: &Brief, id: i64) -> f64 {
            brief
                .trace
                .iter()
                .find(|scored| scored.knowledge_id == id)
                .unwrap_or_else(|| panic!("row {id} is not in the trace"))
                .s_scope
        }

        fn shown(brief: &Brief, id: i64) -> bool {
            brief
                .trace
                .iter()
                .find(|scored| scored.knowledge_id == id)
                .unwrap_or_else(|| panic!("row {id} is not in the trace"))
                .shown
        }

        /// One active, owner-approved memory row with a body of the caller's choosing.
        async fn insert_active(pool: &SqlitePool, scope: &Scope, title: &str, body: &str) -> i64 {
            let (scope_kind, scope_id) = scope.columns();
            sqlx::query(
                "INSERT INTO knowledge
                   (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
                 VALUES ('semantic', ?, ?, 'owner', 'memory', ?, ?, 'active',
                         '2026-10-01T00:00:00+00:00')",
            )
            .bind(scope_kind)
            .bind(scope_id)
            .bind(title)
            .bind(body)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
        }

        async fn events_of(
            pool: &SqlitePool,
            knowledge_id: i64,
        ) -> Vec<(Option<String>, String, Option<String>)> {
            sqlx::query_as(
                "SELECT from_status, to_status, note FROM knowledge_events
                  WHERE knowledge_id = ? ORDER BY id",
            )
            .bind(knowledge_id)
            .fetch_all(pool)
            .await
            .unwrap()
        }

        async fn event_total(pool: &SqlitePool) -> i64 {
            sqlx::query_scalar("SELECT COUNT(*) FROM knowledge_events")
                .fetch_one(pool)
                .await
                .unwrap()
        }

        async fn row_state(pool: &SqlitePool, id: i64) -> (String, String, Option<String>) {
            sqlx::query_as("SELECT status, scope_kind, scope_id FROM knowledge WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap()
        }

        async fn proposal_status(pool: &SqlitePool, id: i64) -> String {
            sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap()
        }

        #[test]
        fn a_team_and_an_agent_scope_round_trip_through_their_columns() {
            for scope in [Scope::Team("x".into()), Scope::Agent("a".into())] {
                let (kind, id) = scope.columns();
                assert_eq!(
                    Scope::parse(kind, id.as_deref()),
                    Some(scope.clone()),
                    "{scope:?} did not survive its own columns"
                );
            }
            assert_eq!(
                Scope::Team("x".into()).columns(),
                ("team", Some("x".into()))
            );
            assert_eq!(
                Scope::Agent("a".into()).columns(),
                ("agent", Some("a".into()))
            );
            // Like `project` and `job`, a team or an agent without an id is not a scope.
            assert_eq!(Scope::parse("team", None), None);
            assert_eq!(Scope::parse("agent", None), None);
        }

        #[test]
        fn the_chain_runs_machine_project_team_agent_job_and_skips_absent_links() {
            let mut context = job_context(7);
            context.team = Some("x".into());
            context.agent = Some("a".into());
            assert_eq!(
                columns_of(&context.scopes()),
                vec![
                    ("machine", None),
                    ("project", Some("p".into())),
                    ("team", Some("x".into())),
                    ("agent", Some("a".into())),
                    ("job", Some("7".into())),
                ]
            );

            context.team = None;
            assert_eq!(
                columns_of(&context.scopes()),
                vec![
                    ("machine", None),
                    ("project", Some("p".into())),
                    ("agent", Some("a".into())),
                    ("job", Some("7".into())),
                ]
            );

            let mut house = Context::for_project(None);
            house.agent = Some("a".into());
            assert_eq!(
                columns_of(&house.scopes()),
                vec![("machine", None), ("agent", Some("a".into()))]
            );
        }

        #[test]
        fn a_context_without_agent_or_team_keeps_todays_chain() {
            let context = Context::for_project(Some("p"));
            assert_eq!(
                context.scopes(),
                vec![Scope::Machine, Scope::Project("p".into())]
            );

            let project = scoped_row(1, "project", Some("p"), "project row");
            let team = scoped_row(2, "team", Some("x"), "team row");
            let agent = scoped_row(3, "agent", Some("a"), "agent row");
            let brief = select(&[project, team, agent], &context, &Budget::default());
            let block = brief.block.clone().expect("the project row still renders");
            assert!(block.contains("project row"));
            assert!(!block.contains("team row"), "{block}");
            assert!(!block.contains("agent row"), "{block}");
            assert!(
                !brief
                    .trace
                    .iter()
                    .any(|scored| scored.shown && scored.knowledge_id != 1)
            );
        }

        #[test]
        fn the_agent_outranks_the_team_and_the_team_the_project() {
            let known = [
                scoped_row(1, "project", Some("p"), "project row"),
                scoped_row(2, "team", Some("x"), "team row"),
                scoped_row(3, "agent", Some("a"), "agent row"),
            ];
            let brief = select(&known, &linked(Some("a"), Some("x")), &Budget::default());
            let project = scope_signal(&brief, 1);
            let team = scope_signal(&brief, 2);
            let agent = scope_signal(&brief, 3);
            assert!(agent > team, "agent {agent} did not outrank team {team}");
            assert!(
                team > project,
                "team {team} did not outrank project {project}"
            );
        }

        #[test]
        fn another_agents_or_teams_memory_is_never_selected() {
            let known = [
                scoped_row(1, "agent", Some("a"), "agent-a-fact"),
                scoped_row(2, "agent", Some("b"), "agent-b-fact"),
                scoped_row(3, "team", Some("x"), "team-x-fact"),
                scoped_row(4, "team", Some("y"), "team-y-fact"),
            ];
            let brief = select(&known, &linked(Some("a"), Some("x")), &Budget::default());
            let block = brief.block.clone().expect("the matching links render");
            assert!(block.contains("agent-a-fact"), "{block}");
            assert!(block.contains("team-x-fact"), "{block}");
            assert!(!block.contains("agent-b-fact"), "{block}");
            assert!(!block.contains("team-y-fact"), "{block}");
            for foreign in [2, 4] {
                assert!(
                    brief
                        .trace
                        .iter()
                        .any(|scored| scored.knowledge_id == foreign),
                    "row {foreign} is admitted and belongs in the trace"
                );
                assert!(!shown(&brief, foreign), "row {foreign} was shown");
            }
        }

        #[test]
        fn an_item_below_min_relevance_is_left_out_even_with_room_to_spare() {
            assert_eq!(MIN_RELEVANCE, 0.1);
            let mut at = scoped_row(1, "project", Some("p"), "at-the-threshold");
            at.s_fts = MIN_RELEVANCE;
            let mut below = scoped_row(2, "project", Some("p"), "just-below-it");
            below.s_fts = f64::from_bits(MIN_RELEVANCE.to_bits() - 1);

            let brief = select(&[at, below], &project_context(), &Budget::default());
            let block = brief
                .block
                .clone()
                .expect("the row at the threshold renders");
            assert!(block.contains("at-the-threshold"), "{block}");
            assert!(
                !block.contains("just-below-it"),
                "a row below the threshold was shown with the whole budget free: {block}"
            );
            assert!(shown(&brief, 1));
            assert!(!shown(&brief, 2));
        }

        #[test]
        fn similarity_alone_clears_the_threshold_only_with_a_query_vector() {
            let mut similar = scoped_row(1, "project", Some("p"), "similar-only");
            similar.s_fts = 0.0;
            similar.s_sim = 0.9;

            let without = select(
                std::slice::from_ref(&similar),
                &project_context(),
                &Budget::default(),
            );
            assert!(
                without.block.is_none() && !shown(&without, 1),
                "a similarity nobody computed cleared the threshold"
            );

            let mut embedded = project_context();
            embedded.query_embedded = true;
            let with = select(&[similar], &embedded, &Budget::default());
            assert!(shown(&with, 1), "a computed similarity did not clear it");
            assert!(with.block.expect("rendered").contains("similar-only"));
        }

        #[test]
        fn a_layer_floor_is_never_spent_on_an_item_below_the_threshold() {
            let mut episodic = scoped_row(1, "project", Some("p"), "unmatched-episode");
            episodic.layer = Layer::Episodic.as_str().into();
            episodic.source = "consolidator".into();
            episodic.observations = Some(3);
            episodic.s_fts = 0.0;
            let semantic = scoped_row(2, "project", Some("p"), "matched-fact");

            let brief = select(
                &[episodic, semantic],
                &project_context(),
                &Budget::default(),
            );
            let block = brief.block.clone().expect("the matched row renders");
            assert!(block.contains("matched-fact"), "{block}");
            assert!(
                !block.contains("unmatched-episode"),
                "the episodic floor took a row that matched nothing: {block}"
            );
            assert!(!shown(&brief, 1));
        }

        #[test]
        fn nothing_relevant_is_an_empty_block_not_an_error() {
            let mut known = vec![
                scoped_row(1, "project", Some("p"), "first"),
                scoped_row(2, "project", Some("p"), "second"),
                scoped_row(3, "machine", None, "third"),
            ];
            for row in &mut known {
                row.s_fts = 0.0;
            }

            let brief = select(&known, &project_context(), &Budget::default());
            assert!(brief.block.is_none(), "{:?}", brief.block);
            assert_eq!(
                brief.trace.len(),
                known.len(),
                "the trace must still carry every admitted row"
            );
            assert!(brief.trace.iter().all(|scored| !scored.shown));
        }

        #[tokio::test]
        async fn a_briefing_where_every_candidate_matches_shows_the_weakest_match_too() {
            let pool = test_pool().await;
            let project = Scope::Project("p".into());
            // Same length, so the only difference between the two is that one says the word twice.
            let twice =
                insert_active(&pool, &project, "zanzibar rollout twice", "zanzibar again").await;
            let once = insert_active(&pool, &project, "zanzibar rollout once", "plain words").await;
            for filler in ["one", "two", "three"] {
                insert_active(&pool, &project, &format!("plain note {filler}"), "filler").await;
            }

            let brief = crate::brief::of(&pool, &Context::for_project(Some("p")), "zanzibar")
                .await
                .unwrap();
            assert!(shown(&brief, twice), "the strongest match was not shown");
            assert!(
                shown(&brief, once),
                "the weakest match normalised to zero and fell under the threshold"
            );
            let block = brief.block.expect("both matches render");
            assert!(block.contains("zanzibar rollout twice"), "{block}");
            assert!(block.contains("zanzibar rollout once"), "{block}");
        }

        #[tokio::test]
        async fn a_briefing_fetches_its_agent_and_team_links_and_no_others() {
            let pool = test_pool().await;
            let mine = insert_active(
                &pool,
                &Scope::Agent("a".into()),
                "zanzibar agent-a-note",
                "body",
            )
            .await;
            let other = insert_active(
                &pool,
                &Scope::Agent("b".into()),
                "zanzibar agent-b-note",
                "body",
            )
            .await;
            let team = insert_active(
                &pool,
                &Scope::Team("x".into()),
                "zanzibar team-x-note",
                "body",
            )
            .await;
            let context = linked(Some("a"), Some("x"));

            let fetched: Vec<i64> = for_context(&pool, &context)
                .await
                .unwrap()
                .iter()
                .map(|row| row.id)
                .collect();
            assert!(fetched.contains(&mine) && fetched.contains(&team));
            assert!(!fetched.contains(&other), "another agent's row was fetched");

            let brief = crate::brief::of(&pool, &context, "zanzibar").await.unwrap();
            let block = brief.block.clone().expect("the linked rows render");
            assert!(block.contains("agent-a-note"), "{block}");
            assert!(block.contains("team-x-note"), "{block}");
            assert!(!block.contains("agent-b-note"), "{block}");
            assert!(
                !brief
                    .trace
                    .iter()
                    .any(|scored| scored.knowledge_id == other),
                "another agent's row reached the trace"
            );
        }

        #[tokio::test]
        async fn approving_with_a_scope_moves_the_row_and_records_the_move() {
            let pool = test_pool().await;
            let (id, proposal) = declare(&pool, Some("p"), "move me", None).await.unwrap();

            let approved = approve_as(&pool, proposal, Some(&Scope::Agent("a".into())))
                .await
                .unwrap();
            assert_eq!(approved, id);
            assert_eq!(
                row_state(&pool, id).await,
                (
                    "active".to_owned(),
                    "agent".to_owned(),
                    Some("a".to_owned())
                )
            );
            assert_eq!(proposal_status(&pool, proposal).await, "approved");

            let moves: Vec<_> = events_of(&pool, id)
                .await
                .into_iter()
                .filter(|(from, to, _)| from.as_deref() == Some("proposed") && to == "active")
                .collect();
            assert_eq!(
                moves.len(),
                1,
                "the move is not exactly one event: {moves:?}"
            );
            assert_eq!(moves[0].2.as_deref(), Some("rescoped project:p -> agent:a"));
        }

        #[tokio::test]
        async fn approving_into_a_scope_holding_the_fingerprint_archives_the_source() {
            let pool = test_pool().await;
            let target = Scope::Agent("a".into());
            let (first, first_proposal) =
                declare(&pool, Some("p"), "first copy", None).await.unwrap();
            let (second, second_proposal) = declare(&pool, Some("p"), "second copy", None)
                .await
                .unwrap();
            let fingerprints: Vec<Option<String>> = sqlx::query_scalar(
                "SELECT fingerprint FROM knowledge WHERE id IN (?, ?) ORDER BY id",
            )
            .bind(first)
            .bind(second)
            .fetch_all(&pool)
            .await
            .unwrap();
            assert!(
                fingerprints[0].is_some() && fingerprints[0] == fingerprints[1],
                "the two declarations must share one fingerprint: {fingerprints:?}"
            );

            assert_eq!(
                approve_as(&pool, first_proposal, Some(&target))
                    .await
                    .unwrap(),
                first
            );
            let first_events = events_of(&pool, first).await.len();

            let merged = approve_as(&pool, second_proposal, Some(&target))
                .await
                .unwrap();
            assert_eq!(
                merged, first,
                "the merge must answer with the row that stayed"
            );

            assert_eq!(
                row_state(&pool, first).await,
                (
                    "active".to_owned(),
                    "agent".to_owned(),
                    Some("a".to_owned())
                ),
                "the row already in the target moved"
            );
            assert_eq!(events_of(&pool, first).await.len(), first_events);
            assert_eq!(
                row_state(&pool, second).await,
                (
                    "archived".to_owned(),
                    "project".to_owned(),
                    Some("p".to_owned())
                )
            );
            let ended: Option<String> =
                sqlx::query_scalar("SELECT ended_at FROM knowledge WHERE id = ?")
                    .bind(second)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert!(ended.is_some(), "the archived source has no end time");

            let archived: Vec<_> = events_of(&pool, second)
                .await
                .into_iter()
                .filter(|(from, to, _)| from.as_deref() == Some("proposed") && to == "archived")
                .collect();
            assert_eq!(archived.len(), 1, "{archived:?}");
            assert_eq!(
                archived[0].2.as_deref(),
                Some(format!("rescoped project:p -> agent:a; merged into {first}").as_str())
            );
            assert_eq!(proposal_status(&pool, second_proposal).await, "approved");
        }

        #[tokio::test]
        async fn a_rescope_to_machine_or_job_or_with_a_predecessor_is_refused_and_writes_nothing() {
            let pool = test_pool().await;
            let (id, proposal) = declare(&pool, Some("p"), "stay put", None).await.unwrap();
            let (old, old_proposal) = declare(&pool, Some("p"), "old text", None).await.unwrap();
            approve(&pool, old_proposal).await.unwrap();
            let (successor, successor_proposal) = declare(&pool, Some("p"), "new text", Some(old))
                .await
                .unwrap();
            let before = event_total(&pool).await;

            let job = Scope::Job {
                id: 7,
                project: None,
            };
            for target in [Scope::Machine, job] {
                assert_eq!(
                    approve_as(&pool, proposal, Some(&target)).await,
                    Err(DecisionError::Malformed),
                    "{target:?} was accepted as a target"
                );
            }
            assert_eq!(
                approve_as(&pool, successor_proposal, Some(&Scope::Agent("a".into()))).await,
                Err(DecisionError::Malformed),
                "a row that replaces another moved to a different scope"
            );

            assert_eq!(
                row_state(&pool, id).await,
                (
                    "proposed".to_owned(),
                    "project".to_owned(),
                    Some("p".to_owned())
                )
            );
            assert_eq!(
                row_state(&pool, successor).await,
                (
                    "proposed".to_owned(),
                    "project".to_owned(),
                    Some("p".to_owned())
                )
            );
            assert_eq!(row_state(&pool, old).await.0, "active");
            assert_eq!(proposal_status(&pool, proposal).await, "pending");
            assert_eq!(proposal_status(&pool, successor_proposal).await, "pending");
            assert_eq!(
                event_total(&pool).await,
                before,
                "a refused move wrote events"
            );
        }
    }
}
