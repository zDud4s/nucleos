//! The read side the routes serve: job summaries, the live list and one job's detail.

use super::*;

/// One job, as the shell lists it.
///
/// `Deserialize` is here for the route tests rather than for production: without it the shape of
/// `/jobs` can only be asserted as untyped JSON, and a field that silently stopped being sent would
/// go unnoticed.
#[derive(Debug, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct JobSummary {
    pub id: i64,
    pub project_id: String,
    pub rule_name: Option<String>,
    pub status: String,
    /// Why a `waiting` job is waiting. Budget and contention ask opposite things of a reader — one
    /// is "spend more or wait", the other is "something else is using the project" — so `waiting`
    /// alone would leave them guessing which.
    pub wait_reason: Option<String>,
    pub max_items: i64,
    /// The round the job is on, counted from zero, and how many it may run.
    ///
    /// Carried because without them a job with rounds is unreadable from outside: a queue of eight
    /// items where the first five passed and the last three are pending looks the same whether the
    /// plan node asked for eight at once — which `MAX_ITEMS_CEILING` forbids — or asked for five,
    /// finished them, and had a replan node ask for three more. This pair is what says which.
    pub round: i64,
    pub max_rounds: i64,
    pub created_at: String,
    pub completed_at: Option<String>,
    /// The slot this job holds, or `None` if it holds none.
    ///
    /// Left-joined from `project_slots` rather than kept as a column on `jobs`, because the slots
    /// table is the authority: a finished job still has its row in `jobs` and has already given the
    /// number back. Copying the slot onto `jobs` would give two truths that can disagree, and the
    /// one the daemon obeys would be the other.
    pub slot: Option<i64>,
    /// The team directing this job, or `None` for the sequential job in one shared checkout.
    ///
    /// Carried on the SUMMARY and not only on the detail, because it changes how every other number
    /// beside it reads. Three items `running` at once is a stuck queue in a job without a team and
    /// the entire point of one with it, and nothing else in this struct tells a reader which of the
    /// two they are looking at.
    pub team_id: Option<String>,
    /// The team's name, joined so that nothing downstream has to render an id.
    ///
    /// `teams.id` is slugged from the name and then survives a rename — the same promise
    /// `agent.rs:203` makes about agents, for the same reason — so the two really are different
    /// strings, and the one a person recognises is this one. A shell holding only the id would have
    /// to fetch the whole catalogue to draw one label.
    pub team_name: Option<String>,
    /// How many of this job's items the team allows at once.
    ///
    /// The number that answers the first question this feature provokes — *why is only one item
    /// moving?* — and it belongs to the team rather than to the job, which is exactly why it is read
    /// at the moment the job is looked at rather than copied onto `jobs` when it started. A ceiling
    /// raised this morning applies to the job running now, and `job_view` already reads it that way.
    pub team_max_parallel: Option<i64>,
    /// The workflow bundle this job's graph snapshot came from (`graph_bundle_ref`), so a page can
    /// say which version the job runs. `None` for a job that predates the engine.
    pub bundle: Option<String>,
    /// What the job is doing right now, derived from `runs` and `job_nodes` after the row is loaded
    /// (see [`current_node`]); not a column, hence `skip`.
    #[sqlx(skip)]
    pub node: Option<CurrentNode>,
}

/// One item of a job's queue, as the shell shows it.
///
/// **Not `FromRow` any more.** Two of these fields are JSON text in the database, so the mapping is
/// spelled out in `detail` beside the parse — the same shape `job_view` uses for the same two
/// columns, and for the same reason: a list that will not parse has to be given a meaning, and a
/// derive has nowhere to put one.
#[derive(Debug, serde::Serialize)]
pub struct JobItemView {
    pub ordinal: i64,
    pub description: String,
    pub status: String,
    /// The round this item was queued in. `ordinal` cannot stand in for it: ordinals continue
    /// across rounds rather than restart, so nothing in the number itself marks where one round
    /// ended and the next began.
    pub round: i64,
    /// The node that did it, so the shell can link to the transcript.
    pub run_id: Option<i64>,
    /// Carried separately from `status` because they answer different questions: `status` says
    /// where the item got to, `gate_status` says whether anything measured it. An item that reads
    /// `passed` with no gate status was never measured — the project has no gate command, or this
    /// was an intermediate item under `gate_after_each_item: false`.
    pub gate_status: Option<String>,
    /// Which agent of the team was given this item, and what they are called.
    ///
    /// `None` for every item of every job without a team, which is what `0103` says the NULL means:
    /// nobody was asked. Both halves are carried for the reason `team_name` gives — the id is a slug
    /// and the name is what a person recognises — and they are separate fields rather than one
    /// because only the id is a stable handle to link by.
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    /// The ordinals this item may not start before, and the paths its director said it would touch.
    ///
    /// Empty for a job without a team, where the columns are NULL and mean *nobody was asked*.
    /// `JobSummary::team_id` is what separates that from a genuine `[]`, so nothing here has to: an
    /// item of a directed job with an empty `depends_on` really does wait for nobody, and that is
    /// worth showing.
    ///
    /// The two lists are the whole explanation of why a queue is running the items it is running,
    /// and until now they existed only inside the fold. A person watching two of five items move
    /// could see WHICH two and never why those two.
    pub depends_on: Vec<i64>,
    pub files: Vec<String>,
}

/// A job with its queue.
#[derive(Debug, serde::Serialize)]
pub struct JobDetail {
    #[serde(flatten)]
    pub job: JobSummary,
    pub items: Vec<JobItemView>,
    /// The branch the work is on, so a stopped job's partial can be found. `None` once the GC has
    /// taken the worktree.
    pub branch: Option<String>,
    /// What the owner has said to this job, delivered or still waiting.
    ///
    /// Every note and not only the pending ones, because both states answer a question the owner
    /// actually asks. Before delivery this is the only thing separating a note that is queued from
    /// one that was dropped — and an owner who cannot tell those apart leaves it a second time.
    /// After delivery, `delivered_to_run_id` is the join back to the prompt it was appended to,
    /// which is how "did it arrive in time" gets answered at all.
    pub notes: Vec<crate::notes::Note>,
}

/// Every column of a [`JobSummary`], and the two joins that produce the four it does not own.
///
/// **One constant rather than the four copies that were here.** The same column list was written out
/// in `ONE_SUMMARY_SQL`, twice in `list` and in `LIVE_LIST_SQL`, and `query_as` matches a row to a
/// struct BY NAME at run time — so a field added to three of the four does not fail to compile. It
/// fails on whichever route was forgotten, the first time somebody calls it, with a
/// `ColumnNotFound`. Adding `team_id` would have made that four copies of fourteen columns; this
/// makes it one of them.
///
/// Both joins are LEFT and both predicates belong in the `ON` clause. In a `WHERE` the first would
/// turn into an inner join and drop every job holding no slot — most of them — and the second would
/// drop every job without a team, which is nearly all of them.
pub(super) const SUMMARY_SQL: &str =
    "SELECT jobs.id, jobs.project_id, jobs.rule_name, jobs.status, jobs.wait_reason,
            jobs.max_items, jobs.round, jobs.max_rounds, jobs.created_at, jobs.completed_at,
            project_slots.slot AS slot,
            jobs.team_id, teams.name AS team_name, teams.max_parallel AS team_max_parallel,
            jobs.graph_bundle_ref AS bundle
     FROM jobs
     LEFT JOIN project_slots
       ON project_slots.owner_kind = 'job' AND project_slots.owner_id = jobs.id
     LEFT JOIN teams ON teams.id = jobs.team_id";

/// `AssertSqlSafe` because sqlx 0.9 only trusts `&'static str` and these are assembled. Everything
/// interpolated is a constant of this module; every caller value is a bind parameter, so none of
/// them ever reaches the string. Same reasoning, and same shape, as `runs::purge`.
pub(super) fn summary_query(tail: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(format!("{SUMMARY_SQL} {tail}"))
}

/// The most recent jobs, newest first.
///
/// Not filtered to live ones: a job that stopped for the budget or ran out of clock is exactly the
/// one the user needs to see, and it would vanish the moment it mattered.
pub async fn list(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<JobSummary>> {
    let jobs = match project_id {
        Some(project_id) => {
            sqlx::query_as(summary_query(
                "WHERE jobs.project_id = ? ORDER BY jobs.id DESC LIMIT ?",
            ))
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as(summary_query("ORDER BY jobs.id DESC LIMIT ?"))
                .bind(limit)
                .fetch_all(pool)
                .await?
        }
    };
    with_nodes(pool, jobs).await
}

/// Fills [`JobSummary::node`] for each job: one extra indexed read per job, bounded by the callers' `limit`.
async fn with_nodes(pool: &SqlitePool, mut jobs: Vec<JobSummary>) -> sqlx::Result<Vec<JobSummary>> {
    for job in &mut jobs {
        job.node = current_node(pool, job.id).await?;
    }
    Ok(jobs)
}

/// Spelled out rather than assembled from `LIVE_STATUSES`, because sqlx refuses SQL built at
/// runtime — the same trade `LIVE_JOBS_SQL` makes, with the same guard:
/// `the_live_listing_names_every_live_status` compares this text against the constant.
///
/// Only the predicate, now that the columns live in `SUMMARY_SQL`. That is what the guard wanted all
/// along: it used to count quoted words in the whole statement and carry a `+1` for the `'job'` in
/// the slot join, which is a test that had to know about a join it was not about.
pub(super) const LIVE_WHERE_SQL: &str =
    "WHERE jobs.status IN ('running','awaiting_approval','waiting')";

/// The live jobs, optionally filtered to one project.
///
/// A separate function rather than a parameter on `list`: `list` answers *what happened in this
/// project*, and its own comment says why it does not filter to live ones — a job that stopped for
/// the budget is exactly what the user needs to see. This answers a different question, *what is in
/// flight now*, and it is the only one the canvas asks.
pub async fn list_live(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limit: i64,
) -> sqlx::Result<Vec<JobSummary>> {
    let jobs = match project_id {
        Some(project_id) => {
            sqlx::query_as(summary_query(&format!(
                "{LIVE_WHERE_SQL} AND jobs.project_id = ? ORDER BY jobs.id DESC LIMIT ?"
            )))
            .bind(project_id)
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as(summary_query(&format!(
                "{LIVE_WHERE_SQL} ORDER BY jobs.id DESC LIMIT ?"
            )))
            .bind(limit)
            .fetch_all(pool)
            .await?
        }
    };
    with_nodes(pool, jobs).await
}

pub async fn detail(pool: &SqlitePool, job_id: i64) -> sqlx::Result<Option<JobDetail>> {
    let Some(mut job): Option<JobSummary> = sqlx::query_as(summary_query("WHERE jobs.id = ?"))
        .bind(job_id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok(None);
    };
    job.node = current_node(pool, job_id).await?;

    // LEFT JOIN, because `agent_id` is NULL for every item of every job without a team and an inner
    // join would return an empty queue for all of them — the whole listing gone, not one column.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        String,
        String,
        i64,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT job_nodes.ordinal, job_nodes.description, job_nodes.status, job_nodes.round,
                job_nodes.run_id, job_nodes.gate_status, job_nodes.agent_id, agents.name,
                job_nodes.depends_on, job_nodes.files
         FROM job_nodes
         LEFT JOIN agents ON agents.id = job_nodes.agent_id
         WHERE job_nodes.job_id = ? AND job_nodes.node_id = 'implement'
           AND job_nodes.ordinal IS NOT NULL
         ORDER BY job_nodes.ordinal",
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;

    let items: Vec<JobItemView> = rows
        .into_iter()
        .map(
            |(
                ordinal,
                description,
                status,
                round,
                run_id,
                gate_status,
                agent_id,
                agent_name,
                depends_on,
                files,
            )| JobItemView {
                ordinal,
                description,
                status,
                round,
                run_id,
                gate_status,
                agent_id,
                agent_name,
                // A list that will not parse is read as no list, in both directions, and here that
                // is safe in a way it is not in `job_view`: this is a thing to look at, not a thing
                // to schedule on. The worst an unreadable list costs a reader is a row that
                // explains itself less well; there it would start work early.
                depends_on: depends_on
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<i64>>(json).ok())
                    .unwrap_or_default(),
                files: files
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
                    .unwrap_or_default(),
            },
        )
        .collect();

    let branch: Option<String> = sqlx::query_scalar(
        "SELECT branch FROM worktrees WHERE owner_kind = 'job' AND owner_id = ?",
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?;

    let notes = crate::notes::all(pool, job_id).await?;

    Ok(Some(JobDetail {
        job,
        items,
        branch,
        notes,
    }))
}
