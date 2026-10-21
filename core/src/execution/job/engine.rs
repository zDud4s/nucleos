//! Workflow engine glue: facts, walk state, decide (M4 C4/C6/C7).
//!
//! Sections are appended by the chunks that own them; each starts with a `// ---- Cn ----` banner.
// The sections sit in the order the chunks landed, with test modules between them.
#![allow(clippy::items_after_test_module)]

use super::*;

use std::collections::BTreeMap;

use crate::workflows::effective::EffectiveGraph;
use crate::workflows::facts::{self, Emission, Facts};

// ---- C4 ---- emissions, engine facts, plan.json on M1 facts ------------------------------------

/// The one reader of an emission file's raw bytes. `NotFound` is `None`; any other IO error is an
/// error naming the path, which the old `.ok()` conflated with "no file".
pub(super) async fn read_emission_bytes(
    worktree: &Path,
    node_id: &str,
) -> Result<Option<Vec<u8>>, String> {
    let path = facts::emission_path(worktree, node_id)?;
    #[cfg(test)]
    {
        if node_id == "plan" {
            PLAN_READS.with(|reads| reads.set(reads.get() + 1));
        }
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

#[cfg(test)]
thread_local! {
    /// Counts `read_emission_bytes("plan")` calls on this thread (current-thread test runtimes), so
    /// a test can pin "plan.json is read once per landing".
    pub(super) static PLAN_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Keys of a node's emission that hold a list/object and are read by a typed parser instead.
fn opaque_for(node_id: &str) -> &'static [&'static str] {
    if node_id == "plan" { &["items"] } else { &[] }
}

/// The flat-fact reading of the plan node's bytes. The SAME bytes also feed `parse_plan`, so the
/// two interpretations cannot disagree about which file they saw.
///
/// The planner's file is a QUEUE, not a flat emission: the facts only need its scalar keys (`done`,
/// `why`). A key holding a null, a list or an object, or named in a way no condition could read, is
/// opaque here (not published) rather than an error, and the size ceiling counts what the facts
/// keep. `parse_plan` stays the authority on whether the queue itself is usable. Only a file that is
/// empty or not a JSON object at all is refused.
pub(super) fn plan_emission(bytes: Option<&[u8]>) -> Result<Emission, String> {
    let strict = facts::parse_emission("plan", bytes, opaque_for("plan"));
    let raw = match bytes {
        Some(raw) if strict.is_err() => raw,
        _ => return strict,
    };
    let Ok(serde_json::Value::Object(object)) = serde_json::from_slice::<serde_json::Value>(raw)
    else {
        return strict;
    };
    let scalars: serde_json::Map<String, serde_json::Value> = object
        .into_iter()
        .filter(|(key, value)| {
            let mut chars = key.chars();
            let named = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
                && !key.contains('.')
                && !key.chars().any(char::is_whitespace);
            named
                && matches!(
                    value,
                    serde_json::Value::String(_)
                        | serde_json::Value::Bool(_)
                        | serde_json::Value::Number(_)
                )
        })
        .collect();
    match serde_json::to_vec(&serde_json::Value::Object(scalars)) {
        Ok(filtered) => facts::parse_emission("plan", Some(&filtered), &[]).or(strict),
        Err(_) => strict,
    }
}

/// The flat-fact reading of a plan landing's bytes, for the ingest paths. A refusal is logged and
/// yields `None`: `parse_plan` stays the authority on whether the QUEUE is usable, and the facts
/// are rebuilt from disk by [`build_facts`], which does surface the error.
pub(super) fn plan_facts_of(bytes: Option<&[u8]>) -> Option<Emission> {
    match plan_emission(bytes) {
        Ok(emission) => Some(emission),
        Err(error) => {
            tracing::warn!(%error, "plan.json is not a flat emission");
            None
        }
    }
}

/// PURE. Engine-owned facts: `job.round` (`jobs.round` verbatim), `job.mode` (always "active": a
/// job requires `Mode::Active`), `job.has_team`, `job.items` (fan instances of the CURRENT round),
/// `project.id`.
pub(super) fn engine_facts(job: &JobRow, has_team: bool, items_this_round: usize) -> Facts {
    let mut facts = Facts::new();
    facts.insert_job("round", job.round.to_string());
    facts.insert_job("mode", "active");
    facts.insert_job("has_team", if has_team { "true" } else { "false" });
    facts.insert_job("items", items_this_round.to_string());
    facts.insert_project("id", job.project_id.clone());
    facts
}

/// PURE. M1 option A: `plan.done`. Emitted with `done` -> as written ("true" stays "true", anything
/// else is "false"); emitted without `done` -> "false"; not emitted -> `None` (unpublished, so the
/// `more` decision is Stuck rather than false).
pub(super) fn plan_done_fact(emission: &Emission) -> Option<&'static str> {
    match emission {
        Emission::NotEmitted => None,
        Emission::Emitted(entries) => Some(match entries.get("done").map(String::as_str) {
            Some("true") => "true",
            _ => "false",
        }),
    }
}

/// Statuses a node instance is "done" in. `passed` is the one word a finished singleton or fan item
/// ends in.
const DONE_STATUS: &str = "passed";

/// Disk + node rows -> facts. For every graph node whose LATEST instance (highest round, then
/// newest row) is done, reads `<worktree>/.nucleos/<id>.json`. A read or parse error is returned
/// (it names the node), never swallowed into an empty fact set.
pub(super) fn facts_from_rows(
    job: &JobRow,
    has_team: bool,
    rows: &[NodeRow],
    worktree: &Path,
    graph: &EffectiveGraph,
) -> Result<Facts, String> {
    let items_this_round = rows
        .iter()
        .filter(|row| row.ordinal.is_some() && row.round == job.round)
        .count();
    let mut facts = engine_facts(job, has_team, items_this_round);

    let mut latest: BTreeMap<&str, (i64, i64, &str)> = BTreeMap::new();
    for row in rows {
        let key = (row.round, row.id);
        let newer = latest
            .get(row.node_id.as_str())
            .is_none_or(|(round, id, _)| key > (*round, *id));
        if newer {
            latest.insert(
                row.node_id.as_str(),
                (row.round, row.id, row.status.as_str()),
            );
        }
    }

    for node in &graph.graph.nodes {
        let done = latest
            .get(node.id.as_str())
            .is_some_and(|(_, _, status)| *status == DONE_STATUS);
        if !done {
            continue;
        }
        let emission = if node.id == "plan" {
            // The queue is not a flat emission; see `plan_emission`.
            let path = facts::emission_path(worktree, &node.id)?;
            match std::fs::read(&path) {
                Ok(bytes) => plan_emission(Some(&bytes))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Emission::NotEmitted,
                Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
            }
        } else {
            facts::read_emission_with(worktree, &node.id, opaque_for(&node.id))?
        };
        if node.id == "plan" {
            if let Some(done) = plan_done_fact(&emission) {
                facts.insert("plan.done", done);
            }
            // `done` was just published normalised; the file's own value must not overwrite it.
            let emission = match emission {
                Emission::Emitted(mut entries) => {
                    entries.remove("done");
                    Emission::Emitted(entries)
                }
                other => other,
            };
            facts.set_emission(&node.id, emission)?;
        } else {
            facts.set_emission(&node.id, emission)?;
        }
    }
    Ok(facts)
}

/// Disk + `job_nodes` -> `Facts`. See [`facts_from_rows`].
pub(super) async fn build_facts(
    pool: &SqlitePool,
    job: &JobRow,
    worktree: &Path,
    graph: &EffectiveGraph,
) -> Result<Facts, String> {
    build_facts_typed(pool, job, worktree, graph)
        .await
        .map_err(|error| error.to_string())
}

/// [`build_facts`] telling a failed read of the job's rows from an unusable emission file.
async fn build_facts_typed(
    pool: &SqlitePool,
    job: &JobRow,
    worktree: &Path,
    graph: &EffectiveGraph,
) -> Result<Facts, DecideError> {
    let rows = nodes_of(pool, job.id)
        .await
        .map_err(|e| DecideError::Read(format!("job {}: cannot read its nodes: {e}", job.id)))?;
    let team: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT team_id FROM jobs WHERE id = ?")
            .bind(job.id)
            .fetch_optional(pool)
            .await
            .map_err(|e| DecideError::Read(format!("job {}: cannot read its team: {e}", job.id)))?
            .flatten();
    facts_from_rows(job, team.is_some(), &rows, worktree, graph).map_err(DecideError::Emission)
}

/// Deletes `<worktree>/.nucleos/<id>.json` before the node is dispatched (M1 S5). NotFound is
/// fine; any other error aborts the dispatch. For the plan/replan node this runs AFTER
/// `archive_plans` copied the file.
pub(super) async fn clear_stale_emission(worktree: &Path, node_id: &str) -> std::io::Result<()> {
    let path = facts::emission_path(worktree, node_id)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ---- C5 ---- the graph a job runs: loaded from its snapshot, never re-resolved -----------------

/// The graph job `job_id` runs, read from the snapshot frozen on its row at start.
///
/// A NULL snapshot (a job that predates the engine) is the embedded autopilot, with nothing
/// disabled (M2 owner decision 2a). This never consults the pins file or the library: editing
/// either after the job started must not change the job.
pub(super) async fn load_graph(pool: &SqlitePool, job_id: i64) -> Result<EffectiveGraph, String> {
    match load_graph_snapshot(pool, job_id)
        .await
        .map_err(|error| format!("cannot read job {job_id}'s graph snapshot: {error}"))?
    {
        Some(snapshot) => effective_from_snapshot(&snapshot)
            .map_err(|error| format!("job {job_id}'s graph snapshot does not parse: {error}")),
        None => {
            let graph = crate::workflows::graph::parse(crate::seed::graph_yaml())
                .map_err(|error| format!("the embedded autopilot graph does not parse: {error}"))?;
            Ok(EffectiveGraph {
                graph,
                ..Default::default()
            })
        }
    }
}

// ---- C6 ---- the real WalkState builder, decide, and the Step -> Action table ------------------

use crate::governance::autopilot::Mode;
use crate::workflows::graph::{Graph as WorkflowGraph, Kind, is_gate};
use crate::workflows::walk::{self, Disabled, NodeState, Step, WalkState};

/// Nodes the legacy driver runs once per JOB, not once per round: a replan opens a new round but
/// never visits the brief or the first plan again.
const ONCE_PER_JOB: [&str; 2] = ["spec", "plan"];

/// The nodes a project switched off, as the walker wants them. The mode's own set (spec 6, shadow
/// mode) is empty until the owner decides D2; the two causes stay apart in `Disabled`.
pub(super) fn effective_disabled(
    graph: &EffectiveGraph,
    _mode: Mode,
) -> BTreeMap<String, Disabled> {
    graph
        .disabled
        .iter()
        .map(|id| (id.clone(), Disabled::Project))
        .collect()
}

/// PURE. A singleton `job_nodes.status` read as the walker's node state; `None` is "not entered".
///
/// The words the code writes today are `running`, `passed`, `failed`, `gate_errored` (command
/// nodes, `command_node.rs`) and `pending`; `done`, `errored`, `cancelled` and `skipped` are the
/// words the C7 landing may add and are accepted already. A command that FAILED and is a gate has
/// no retry machinery of its own: it reads as red with no retries left.
pub(super) fn node_state_of_status(status: &str, command: bool, gate: bool) -> Option<NodeState> {
    Some(match status {
        "running" | "awaiting_approval" => NodeState::Running,
        "passed" | "done" if command => NodeState::GatePassed,
        "passed" | "done" => NodeState::Done,
        "failed" if command && gate => NodeState::GateRed {
            retries_left: false,
        },
        "failed" => NodeState::Failed,
        "gate_errored" | "errored" => NodeState::GateErrored,
        "cancelled" => NodeState::Cancelled,
        "skipped" => NodeState::Skipped,
        _ => return None,
    })
}

/// The newest run of `stage` with an id above `after`, and only its status.
async fn latest_run_status(
    pool: &SqlitePool,
    job_id: i64,
    stage: &str,
    after: i64,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT status FROM runs WHERE job_id = ? AND stage = ? AND id > ?
         ORDER BY id DESC LIMIT 1",
    )
    .bind(job_id)
    .bind(stage)
    .bind(after)
    .fetch_optional(pool)
    .await
}

/// Where the review node is, read off the round's runs exactly as `load_view` reads it. `None` is
/// "owed and not entered".
async fn review_from_runs(
    pool: &SqlitePool,
    job_id: i64,
    round_opened_at: i64,
    passed_this_round: i64,
) -> sqlx::Result<Option<NodeState>> {
    let run: Option<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT status, CASE WHEN status = 'failed' THEN stdout END,
                (SELECT COUNT(*) FROM runs WHERE job_id = ? AND stage = 'review' AND id > ?)
         FROM runs WHERE job_id = ? AND stage = 'review' AND id > ?
         ORDER BY id DESC LIMIT 1",
    )
    .bind(job_id)
    .bind(round_opened_at)
    .bind(job_id)
    .bind(round_opened_at)
    .fetch_optional(pool)
    .await?;
    Ok(match run {
        None if passed_this_round == 0 => Some(NodeState::Skipped),
        None => None,
        Some((status, _, _)) if node_in_flight(&status) => Some(NodeState::Running),
        Some((_, Some(stdout), 1))
            if crate::execution::runner::failed_on_a_transient_api_error(&stdout) =>
        {
            Some(NodeState::RetryOwed)
        }
        // A review that failed is still a review that happened.
        Some(_) => Some(NodeState::Done),
    })
}

/// Rows -> `WalkState`: what `next_step` needs, built from `job_nodes` (fans, command nodes),
/// `runs` (agent nodes, the replan in flight), `jobs` (rounds, review wanted) and `teams`.
///
/// Agent nodes `spec`, `plan` and `review` are still read from their RUNS, the way `load_view`
/// reads them, because until C7 lands the singleton rows nothing writes a row for `spec` or
/// `review`; a row, when there is one, is the fallback. `review` is `Skipped` when the round
/// queued nothing or the replan said done (the loader fact of M3 S7), and switched off in shadow
/// mode when the job wants no review.
pub(super) async fn walk_state(
    pool: &SqlitePool,
    job: &JobRow,
    graph: &EffectiveGraph,
) -> sqlx::Result<WalkState> {
    let job_id = job.id;
    let (review_wanted, gate_retries, team_id): (i64, i64, Option<String>) =
        sqlx::query_as("SELECT review, gate_retries, team_id FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(pool)
            .await?;
    let (round, dry_rounds, max_rounds, replan_done, opened_by, rule_name, budget_usd): RoundColumns =
        sqlx::query_as(
            "SELECT round, dry_rounds, max_rounds, replan_done, replan_run_id, rule_name, budget_usd
             FROM jobs WHERE id = ?",
        )
        .bind(job_id)
        .fetch_one(pool)
        .await?;
    let rows = nodes_of(pool, job_id).await?;
    let max_parallel: i64 = match team_id.as_deref() {
        Some(team) => sqlx::query_scalar("SELECT max_parallel FROM teams WHERE id = ?")
            .bind(team)
            .fetch_optional(pool)
            .await?
            .unwrap_or(1),
        None => 1,
    };

    let all_nodes = &graph.graph.nodes;
    let in_a_fan: std::collections::BTreeSet<&str> = all_nodes
        .iter()
        .filter(|node| node.kind == Kind::Fan)
        .flat_map(|fan| fan.each.iter().map(String::as_str))
        .collect();

    // Every row of each fan's first `each` node, all rounds, in ordinal order.
    let mut fans = BTreeMap::new();
    let mut queued_this_round = 0i64;
    let mut passed_this_round = 0i64;
    for fan in all_nodes.iter().filter(|node| node.kind == Kind::Fan) {
        let mut queue: Vec<&NodeRow> = match fan.each.first() {
            Some(first) => rows
                .iter()
                .filter(|row| row.node_id == *first && row.ordinal.is_some())
                .collect(),
            None => Vec::new(),
        };
        queue.sort_by_key(|row| row.ordinal);
        for row in &queue {
            if row.round == round {
                queued_this_round += 1;
                if row.status == "passed" {
                    passed_this_round += 1;
                }
            }
        }
        let items: Vec<ItemView> = queue
            .iter()
            .map(|row| ItemView {
                state: item_state_from(&row.status, row.gate_attempts, gate_retries),
                depends_on: row
                    .depends_on
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<i64>>(json).ok())
                    .unwrap_or_default(),
                files: row
                    .files
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
                    .unwrap_or_default(),
            })
            .collect();
        fans.insert(fan.id.clone(), items);
    }

    let replanning = latest_run_status(pool, job_id, "replan", 0)
        .await?
        .as_deref()
        .is_some_and(node_in_flight);

    let mut disabled = effective_disabled(graph, Mode::Active);
    let mut nodes = BTreeMap::new();
    for node in all_nodes {
        if !matches!(node.kind, Kind::Agent | Kind::Command) || in_a_fan.contains(node.id.as_str())
        {
            continue;
        }
        let command = node.kind == Kind::Command;
        let gate = is_gate(&graph.graph.edges, &node.id);
        let from_row = || -> Option<NodeState> {
            rows.iter()
                .filter(|row| {
                    row.node_id == node.id
                        && row.ordinal.is_none()
                        && (ONCE_PER_JOB.contains(&node.id.as_str()) || row.round == round)
                })
                .max_by_key(|row| (row.round, row.id))
                .and_then(|row| node_state_of_status(&row.status, command, gate))
        };

        if !command && node.id == "review" {
            if replan_done != 0 || queued_this_round == 0 {
                nodes.insert(node.id.clone(), NodeState::Skipped);
            } else if review_wanted == 0 {
                disabled.entry(node.id.clone()).or_insert(Disabled::Shadow);
            } else {
                let run_state =
                    review_from_runs(pool, job_id, opened_by.unwrap_or(0), passed_this_round)
                        .await?;
                if let Some(state) = run_state.or_else(from_row) {
                    nodes.insert(node.id.clone(), state);
                }
            }
            continue;
        }

        let run_state: Option<NodeState> = match node.id.as_str() {
            "spec" if !command => {
                let spec_run = latest_run_status(pool, job_id, "spec", 0).await?;
                let plan_run = latest_run_status(pool, job_id, "plan", 0).await?;
                let planned = plan_is_recorded(pool, job_id).await?;
                if spec_run
                    .as_deref()
                    .is_some_and(|status| !node_in_flight(status))
                    || plan_run.is_some()
                    || planned
                {
                    Some(NodeState::Done)
                } else if spec_run.as_deref().is_some_and(node_in_flight) {
                    Some(NodeState::Running)
                } else {
                    None
                }
            }
            "plan" if !command => {
                if plan_is_recorded(pool, job_id).await? {
                    Some(NodeState::Done)
                } else if latest_run_status(pool, job_id, "plan", 0)
                    .await?
                    .as_deref()
                    .is_some_and(node_in_flight)
                {
                    Some(NodeState::Running)
                } else {
                    None
                }
            }
            _ => None,
        };
        if let Some(state) = run_state.or_else(from_row) {
            nodes.insert(node.id.clone(), state);
        }
    }

    Ok(WalkState {
        round,
        max_rounds: max_rounds.unwrap_or(1).max(1),
        dry_rounds,
        replanning,
        a_broken_item_may_go_round_again: commissioned_with_an_allowance(
            rule_name.as_deref(),
            budget_usd,
        ),
        has_team: team_id.is_some(),
        max_parallel: max_parallel.max(1) as usize,
        nodes,
        fans,
        disabled,
    })
}

/// Why the walker could not answer. The two are told apart because they end differently: a read
/// that failed (database, worktree) may heal and is retried on the next tick, whereas an emission a
/// finished node left unreadable will read the same on every tick, so the job ends instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DecideError {
    /// The job's own state could not be read.
    Read(String),
    /// A finished node's emission file is unusable (corrupt, nested, null, empty, oversize).
    Emission(String),
}

impl std::fmt::Display for DecideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecideError::Read(why) | DecideError::Emission(why) => f.write_str(why),
        }
    }
}

/// The walker's whole input and answer for a job: its snapshot graph, the state built from the
/// rows, the facts read from the worktree, and the next [`Step`]. Errors name what could not be
/// read; a missing worktree is one (facts live on disk).
pub(super) async fn decide(
    state: &AppState,
    job: &JobRow,
) -> Result<(Step, Facts, EffectiveGraph), DecideError> {
    let pool = &state.pool;
    let graph = load_graph(pool, job.id).await.map_err(DecideError::Read)?;
    let walk_state = walk_state(pool, job, &graph).await.map_err(|error| {
        DecideError::Read(format!(
            "job {}: cannot read its walk state: {error}",
            job.id
        ))
    })?;
    let worktree = job_worktree(pool, job.id)
        .await
        .map_err(|error| {
            DecideError::Read(format!("job {}: cannot read its worktree: {error}", job.id))
        })?
        .ok_or_else(|| DecideError::Read(format!("job {} has no worktree on record", job.id)))?
        .0;
    let facts = build_facts_typed(pool, job, &worktree, &graph).await?;
    let step = walk::next_step(&graph.graph, &walk_state, &facts);
    Ok((step, facts, graph))
}

/// A fan-boundary operation: engine work at the seam between a fan's items and the job's branch,
/// not a node.
// `node_id` is carried for the feed and the tests; the driver acts on `item` alone.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FanOp {
    MergeItem {
        node_id: String,
        item: usize,
    },
    SettleRevert {
        node_id: String,
        item: Option<usize>,
    },
    Orphan {
        node_id: String,
        item: usize,
    },
}

/// What the executor does, derived from the walker's [`Step`] by [`action_of`].
// Some variants carry ids the driver does not read yet (`ordinal`, `node_id`, `item`).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Action {
    DispatchAgent {
        node_id: String,
        ordinal: Option<usize>,
    },
    DispatchCommand {
        node_id: String,
        ordinal: Option<usize>,
    },
    FanOut {
        node_id: String,
        items: Vec<usize>,
    },
    Replan {
        node_id: String,
    },
    FanBoundary(FanOp),
    Wait,
    Finish(Outcome),
    Stuck {
        node: String,
        key: String,
    },
    GateErrored {
        node: String,
        item: Option<usize>,
    },
}

/// PURE. The one place `Step` knowledge lives in the driver. A `RunNode` is classified by the
/// graph: the gate side of a fan item is always a command; a singleton is an agent or a command; a
/// singleton naming a decision, a fan or no node at all never comes out of the walker, and is
/// reported as stuck rather than guessed at.
pub(super) fn action_of(step: &Step, graph: &WorkflowGraph) -> Action {
    match step {
        Step::RunNode {
            node_id,
            item: Some(ordinal),
        } => Action::DispatchCommand {
            node_id: node_id.clone(),
            ordinal: Some(*ordinal),
        },
        Step::RunNode {
            node_id,
            item: None,
        } => match graph.nodes.iter().find(|node| node.id == *node_id) {
            Some(node) if node.kind == Kind::Agent => Action::DispatchAgent {
                node_id: node_id.clone(),
                ordinal: None,
            },
            Some(node) if node.kind == Kind::Command => Action::DispatchCommand {
                node_id: node_id.clone(),
                ordinal: None,
            },
            _ => Action::Stuck {
                node: node_id.clone(),
                key: "kind".to_string(),
            },
        },
        Step::FanOut { node_id, items } => Action::FanOut {
            node_id: node_id.clone(),
            items: items.clone(),
        },
        Step::MergeItem { node_id, item } => Action::FanBoundary(FanOp::MergeItem {
            node_id: node_id.clone(),
            item: *item,
        }),
        Step::SettleRevert { node_id, item } => Action::FanBoundary(FanOp::SettleRevert {
            node_id: node_id.clone(),
            item: *item,
        }),
        Step::Orphan { node_id, item } => Action::FanBoundary(FanOp::Orphan {
            node_id: node_id.clone(),
            item: *item,
        }),
        Step::Replan { node_id } => Action::Replan {
            node_id: node_id.clone(),
        },
        Step::Wait => Action::Wait,
        Step::GateErrored { node_id, item } => Action::GateErrored {
            node: node_id.clone(),
            item: *item,
        },
        Step::Stuck(stuck) => Action::Stuck {
            node: stuck.node.clone(),
            key: stuck.key.clone(),
        },
        Step::Finish(outcome) => Action::Finish(*outcome),
    }
}

// ---- C7a ---- dispatching an agent node: protocol, launcher, row, run ---------------------------

use super::dispatch::{AgentNodeLauncher, resolve_launch_with};

/// Stages whose protocol is one of the autopilot's own prompt functions. Every other agent node of
/// a bundle gets [`composed_protocol`]. (`implement` is a fan instance, started by `start_item`.)
const AUTOPILOT_STAGES: [&str; 4] = ["spec", "plan", "review", "replan"];

/// Whether `stage` is one of the nodes the legacy driver has always run by name.
pub(super) fn is_autopilot_stage(stage: &str) -> bool {
    AUTOPILOT_STAGES.contains(&stage)
}

/// The protocol half of an autopilot node's prompt, from the same prompt functions the legacy
/// driver calls, or `None` for any other stage. `replan` archives the round's plan FIRST (the node
/// is about to overwrite `plan.json`). `council_file` is whether a synthesis sits beside the queue
/// for the review.
pub(super) async fn autopilot_protocol(
    state: &AppState,
    job: &JobRow,
    view: &JobView,
    stage: &str,
    worktree: &Path,
    council_file: bool,
) -> Option<String> {
    let pool = &state.pool;
    let artifacts = artifacts_for(worktree);
    let task = job.prompt.clone().unwrap_or_default();
    Some(match stage {
        "spec" => spec_prompt(&task, &artifacts),
        "plan" => plan_prompt(
            &task,
            job.max_items.max(0) as usize,
            &artifacts,
            graph_rules(pool, job).await.as_ref(),
            spec_is_written(worktree).await,
        ),
        "review" => review_prompt(
            &task,
            job.head_sha.as_deref(),
            &artifacts,
            spec_is_written(worktree).await,
            council_file,
        ),
        "replan" => {
            let archives = archive_plans(worktree, view.rounds.round).await;
            replan_prompt(
                &task,
                view.rounds.round,
                &archives,
                &artifacts,
                graph_rules(pool, job).await.as_ref(),
                &unfinished_items(pool, job.id).await,
            )
        }
        _ => return None,
    })
}

/// The council in front of the review node, and the one-off feed line for a retried review.
/// `Ok(council_file)` starts the node; `Err(pass)` is the pass that parked the job behind a council.
pub(super) async fn before_review(
    state: &AppState,
    job: &JobRow,
    view: &JobView,
    artifacts: &str,
) -> Result<bool, Pass> {
    match council_before_review(state, job, artifacts).await {
        BeforeReview::Go { council_file } => {
            // Here, where the node actually starts, and once: a review parked behind a council comes
            // back through this on every tick, and the row belongs to the retry rather than to each
            // tick spent waiting for it.
            if view.review == ReviewState::Retry {
                say_once(
                    &state.pool,
                    job,
                    "job_review_retried",
                    &format!(
                        "job {} round {}: its review ended on a transient API error before it could \
                         judge anything, so it is run once more",
                        job.id,
                        view.rounds.round + 1
                    ),
                )
                .await;
            }
            Ok(council_file)
        }
        BeforeReview::Deliberating => Err(park(
            state,
            job,
            WAIT_COUNCIL,
            "a council is deliberating on this job before its review",
        )
        .await),
    }
}

/// PURE. The keys the workflow reads from `node_id`'s emission: every `node_id.key` named by a
/// decision's condition, without the prefix, sorted and de-duplicated.
pub(super) fn keys_read_from(graph: &WorkflowGraph, node_id: &str) -> Vec<String> {
    let prefix = format!("{node_id}.");
    let mut keys = std::collections::BTreeSet::new();
    for node in &graph.nodes {
        let Some(text) = node.condition.as_deref() else {
            continue;
        };
        let Ok(condition) = crate::workflows::condition::parse(text) else {
            continue;
        };
        for key in condition.keys() {
            if let Some(rest) = key.strip_prefix(&prefix) {
                keys.insert(rest.to_string());
            }
        }
    }
    keys.into_iter().collect()
}

/// What the driver hands to [`dispatch_agent_node`]: the node by id (looked up in the job's own
/// graph), what the walker read, and whether this is the replan visit of the plan node.
pub(super) struct AgentTarget<'a> {
    pub view: &'a JobView,
    pub facts: &'a Facts,
    pub graph: &'a EffectiveGraph,
    pub node_id: &'a str,
    /// The `Step::Replan` visit: the replan prompt and `runs.stage = "replan"`.
    pub replan: bool,
}

/// Starts one agent node (spec 3.2), asking the installed launcher what to run.
pub(super) async fn dispatch_agent_node(
    state: &AppState,
    job: &JobRow,
    target: AgentTarget<'_>,
    worktree: (PathBuf, String),
) -> Pass {
    let launcher = super::dispatch::launcher();
    dispatch_agent_node_with(launcher.as_ref(), state, job, target, worktree).await
}

/// [`dispatch_agent_node`] against an explicit launcher (what the tests use).
///
/// Order: council (review only) -> protocol -> stale emission cleared (AFTER the replan's archive,
/// BEFORE the launch) -> launcher (a refusal fails the node before a run exists) -> the node's
/// singleton row -> `spawn_node`. The autopilot's own nodes keep no singleton row: the walker reads
/// them from their runs, as `load_view` always did. A bundle node's row is `running` while its run
/// exists and is removed again when no run came of it, so a parked node is not left looking in
/// flight.
pub(super) async fn dispatch_agent_node_with(
    launcher: &dyn AgentNodeLauncher,
    state: &AppState,
    job: &JobRow,
    target: AgentTarget<'_>,
    worktree: (PathBuf, String),
) -> Pass {
    let pool = &state.pool;
    let AgentTarget {
        view,
        facts,
        graph,
        node_id,
        replan,
    } = target;
    let Some(node) = graph.graph.nodes.iter().find(|node| node.id == node_id) else {
        tracing::warn!(
            job_id = job.id,
            node_id,
            "asked to dispatch a node the job's graph lacks"
        );
        return Pass::Stopped;
    };
    let tree = worktree.0.clone();
    let artifacts = artifacts_for(&tree);
    let stage: &str = if replan { "replan" } else { &node.id };
    let round = job.round;

    let mut council_file = false;
    if stage == "review" {
        match before_review(state, job, view, &artifacts).await {
            Ok(file) => council_file = file,
            Err(pass) => return pass,
        }
    }
    let protocol = match autopilot_protocol(state, job, view, stage, &tree, council_file).await {
        Some(protocol) => protocol,
        None => {
            let keys = keys_read_from(&graph.graph, &node.id);
            let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
            composed_protocol(
                &node.id,
                &artifacts,
                job.prompt.as_deref().unwrap_or_default(),
                round,
                &keys,
            )
        }
    };

    // Every node's own emission is cleared before it runs, the autopilot's included: a file left by
    // an earlier visit (round 1's `review.json` when round 2 reviews) must not pass for this
    // visit's answer. A replan's archive of `plan.json` was taken by the protocol step above, so
    // the clear comes after it.
    if let Err(error) = clear_stale_emission(&tree, &node.id).await {
        tracing::warn!(job_id = job.id, node = %node.id, %error, "could not clear a node's stale emission; not dispatching it");
        return Pass::Stopped;
    }

    let launched = {
        let ctx = NodeCtx {
            state,
            job,
            node,
            round,
            ordinal: None,
            worktree: &tree,
            facts,
        };
        resolve_launch_with(launcher, &ctx, protocol).await
    };
    let launch = match launched {
        Ok(launch) => launch,
        Err(refused) => {
            if !replan {
                let _ = insert_singleton_node(pool, job.id, &node.id, round, "failed").await;
                let _ = set_singleton_status(
                    pool,
                    job.id,
                    &node.id,
                    round,
                    &["pending", "running"],
                    "failed",
                )
                .await;
            }
            say(
                pool,
                job,
                "job_node_refused",
                &format!(
                    "job {} node {} cannot start: {}",
                    job.id,
                    node.id,
                    refused.reason()
                ),
            )
            .await;
            let _ = finish(pool, job.id, Outcome::Failed).await;
            return Pass::Stopped;
        }
    };

    let tracked = !replan && !is_autopilot_stage(&node.id);
    if tracked
        && let Err(error) = insert_singleton_node(pool, job.id, &node.id, round, "running").await
    {
        tracing::warn!(job_id = job.id, node = %node.id, %error, "could not open an agent node's row");
    }
    let pass = spawn_node(
        state,
        job,
        NodeSpec {
            node_id: stage,
            claim: None,
            launch,
        },
        worktree,
        NoRoom::Park,
    )
    .await;
    if tracked {
        // `spawn_node` links the row to its run. A row still without one means no run came of this
        // dispatch (parked for want of a slot, a transient error): the walker must not read it as
        // a node in flight, so the next pass dispatches again from a clean slate.
        if let Err(error) = sqlx::query(
            "DELETE FROM job_nodes
             WHERE job_id = ? AND node_id = ? AND round = ? AND ordinal IS NULL
               AND status = 'running' AND run_id IS NULL",
        )
        .bind(job.id)
        .bind(&node.id)
        .bind(round)
        .execute(pool)
        .await
        {
            tracing::warn!(job_id = job.id, node = %node.id, %error, "could not clear a run-less node row");
        }
    }
    pass
}

// The landing section lives in `landing`; the tests reach it through here.
#[cfg(test)]
pub(super) use super::landing::{Landing, land_nodes_with, retried_launch, validation_verdict};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::graph::{Graph, Kind, Node};

    fn job(round: i64) -> JobRow {
        JobRow {
            id: 1,
            project_id: "proj".to_string(),
            project_root: "/p".to_string(),
            prompt: None,
            status: "running".to_string(),
            resume_status: None,
            wait_reason: None,
            max_items: 5,
            gate_each: 1,
            head_sha: None,
            round,
            budget_usd: None,
            rule_name: None,
            created_at: "2026-10-21T00:00:00Z".to_string(),
        }
    }

    fn node(id: &str) -> Node {
        Node {
            id: id.to_string(),
            kind: Kind::Agent,
            label: None,
            body: None,
            agent: None,
            tool: None,
            model: None,
            effort: None,
            contract: None,
            command: None,
            cwd: None,
            timeout: None,
            condition: None,
            source: None,
            each: Vec::new(),
            concurrency: None,
            join: None,
        }
    }

    fn graph(ids: &[&str]) -> EffectiveGraph {
        EffectiveGraph {
            graph: Graph {
                nodes: ids.iter().map(|id| node(id)).collect(),
                edges: Vec::new(),
            },
            ..EffectiveGraph::default()
        }
    }

    fn row(id: i64, node_id: &str, ordinal: Option<i64>, round: i64, status: &str) -> NodeRow {
        NodeRow {
            id,
            job_id: 1,
            node_id: node_id.to_string(),
            ordinal,
            round,
            description: String::new(),
            status: status.to_string(),
            run_id: None,
            gate_status: None,
            gate_attempts: 0,
            gate_output: None,
            checkpoint_sha: None,
            merge_base_sha: None,
            depends_on: None,
            files: None,
            agent_id: None,
        }
    }

    fn write(worktree: &Path, name: &str, contents: &str) {
        let dir = worktree.join(crate::execution::worktree::ARTIFACTS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn engine_facts_table() {
        let f = engine_facts(&job(0), false, 0);
        assert_eq!(f.get("job.round"), Some("0"));
        assert_eq!(f.get("job.mode"), Some("active"));
        assert_eq!(f.get("job.has_team"), Some("false"));
        assert_eq!(f.get("job.items"), Some("0"));
        assert_eq!(f.get("project.id"), Some("proj"));
        let f = engine_facts(&job(3), true, 4);
        assert_eq!(f.get("job.round"), Some("3"));
        assert_eq!(f.get("job.has_team"), Some("true"));
        assert_eq!(f.get("job.items"), Some("4"));
    }

    #[test]
    fn plan_done_fact_table() {
        let emitted = |pairs: &[(&str, &str)]| {
            Emission::Emitted(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        };
        assert_eq!(plan_done_fact(&emitted(&[("done", "true")])), Some("true"));
        assert_eq!(
            plan_done_fact(&emitted(&[("done", "false")])),
            Some("false")
        );
        assert_eq!(plan_done_fact(&emitted(&[("why", "x")])), Some("false"));
        assert_eq!(plan_done_fact(&emitted(&[])), Some("false"));
        assert_eq!(plan_done_fact(&Emission::NotEmitted), None);
    }

    #[tokio::test]
    async fn a_stale_emission_is_deleted_before_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "review.json", r#"{"verdict":"ok"}"#);
        let path = facts::emission_path(dir.path(), "review").unwrap();
        assert!(path.exists());
        clear_stale_emission(dir.path(), "review").await.unwrap();
        assert!(!path.exists());
        // NotFound is fine, twice over.
        clear_stale_emission(dir.path(), "review").await.unwrap();
        clear_stale_emission(dir.path(), "never").await.unwrap();
        // A directory where the file should be is not something to swallow.
        std::fs::create_dir_all(dir.path().join(".nucleos").join("odd.json")).unwrap();
        assert!(clear_stale_emission(dir.path(), "odd").await.is_err());
        // An id that is not a file stem is refused.
        assert!(clear_stale_emission(dir.path(), "../x").await.is_err());
    }

    #[test]
    fn facts_are_read_only_for_nodes_whose_latest_instance_is_done() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["triage", "build", "later", "never"] {
            write(dir.path(), &format!("{id}.json"), r#"{"size":"small"}"#);
        }
        let rows = [
            row(1, "triage", None, 0, "passed"),
            row(2, "build", None, 0, "running"),
            row(3, "later", None, 0, "passed"),
            // `later` was entered again in round 1 and has not finished: its old file is stale.
            row(4, "later", None, 1, "running"),
        ];
        let g = graph(&["triage", "build", "later", "never"]);
        let facts = facts_from_rows(&job(1), false, &rows, dir.path(), &g).unwrap();
        assert_eq!(facts.get("triage.size"), Some("small"));
        assert_eq!(facts.get("build.size"), None);
        assert_eq!(facts.get("later.size"), None);
        assert_eq!(facts.get("never.size"), None);
    }

    #[test]
    fn a_done_plan_publishes_plan_done_normalised_and_ignores_items() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "plan.json",
            r#"{"items":[{"description":"x"}],"why":"ok"}"#,
        );
        let rows = [row(1, "plan", None, 0, "passed")];
        let g = graph(&["plan"]);
        let facts = facts_from_rows(&job(0), false, &rows, dir.path(), &g).unwrap();
        assert_eq!(facts.get("plan.done"), Some("false"));
        assert_eq!(facts.get("plan.why"), Some("ok"));
        assert!(facts.has_emitted("plan"));

        write(dir.path(), "plan.json", r#"{"items":[],"done":true}"#);
        let facts = facts_from_rows(&job(0), false, &rows, dir.path(), &g).unwrap();
        assert_eq!(facts.get("plan.done"), Some("true"));

        // A done plan node with no file publishes no `plan.done` at all.
        std::fs::remove_file(dir.path().join(".nucleos").join("plan.json")).unwrap();
        let facts = facts_from_rows(&job(0), false, &rows, dir.path(), &g).unwrap();
        assert_eq!(facts.get("plan.done"), None);
    }

    #[test]
    fn job_items_counts_only_the_current_round() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            row(1, "implement", Some(0), 0, "passed"),
            row(2, "implement", Some(1), 1, "pending"),
            row(3, "implement", Some(2), 1, "pending"),
            row(4, "plan", None, 1, "pending"),
        ];
        let facts = facts_from_rows(&job(1), false, &rows, dir.path(), &graph(&[])).unwrap();
        assert_eq!(facts.get("job.items"), Some("2"));
    }

    #[test]
    fn a_corrupt_emission_names_the_node_and_stops_the_job_instead_of_publishing_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "triage.json", r#"{"size": "sma"#);
        let rows = [row(1, "triage", None, 0, "passed")];
        let err =
            facts_from_rows(&job(0), false, &rows, dir.path(), &graph(&["triage"])).unwrap_err();
        assert!(err.contains("triage"), "{err}");
    }

    #[test]
    fn the_plan_file_constant_equals_the_emission_convention() {
        let path = facts::emission_path(Path::new("wt"), "plan").unwrap();
        assert_eq!(
            path,
            Path::new("wt")
                .join(crate::execution::worktree::ARTIFACTS_DIR)
                .join(PLAN_FILE)
        );
    }

    #[test]
    fn plan_emission_skips_items_and_reads_the_rest() {
        let bytes = br#"{"items":[{"description":"x"}],"done":true}"#;
        let got = plan_emission(Some(bytes)).unwrap();
        assert_eq!(plan_done_fact(&got), Some("true"));
        assert_eq!(plan_emission(None).unwrap(), Emission::NotEmitted);
    }

    #[tokio::test]
    async fn read_emission_bytes_distinguishes_absent_from_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_emission_bytes(dir.path(), "plan").await, Ok(None));
        write(dir.path(), "plan.json", "{}");
        assert_eq!(
            read_emission_bytes(dir.path(), "plan").await,
            Ok(Some(b"{}".to_vec()))
        );
        std::fs::create_dir_all(dir.path().join(".nucleos").join("odd.json")).unwrap();
        assert!(read_emission_bytes(dir.path(), "odd").await.is_err());
    }
}

#[cfg(test)]
mod c6_tests {
    use super::*;
    use crate::workflows::walk::GraphStuck;

    fn autopilot() -> WorkflowGraph {
        crate::workflows::graph::parse(crate::seed::graph_yaml()).expect("the embedded autopilot")
    }

    #[test]
    fn action_of_table() {
        let g = autopilot();
        let run = |id: &str, item: Option<usize>| Step::RunNode {
            node_id: id.to_string(),
            item,
        };
        assert_eq!(
            action_of(&run("plan", None), &g),
            Action::DispatchAgent {
                node_id: "plan".into(),
                ordinal: None
            }
        );
        assert_eq!(
            action_of(&run("gate", Some(2)), &g),
            Action::DispatchCommand {
                node_id: "gate".into(),
                ordinal: Some(2)
            }
        );
        // A singleton command node, in a graph that has one.
        let with_lint: WorkflowGraph = serde_yaml::from_str(
            "nodes:\n  - {id: lint, type: command, command: \"true\"}\nedges: []\n",
        )
        .unwrap();
        assert_eq!(
            action_of(&run("lint", None), &with_lint),
            Action::DispatchCommand {
                node_id: "lint".into(),
                ordinal: None
            }
        );
        // A decision or a fan is never run; an unknown id is not guessed at either.
        for id in ["more", "work", "nowhere"] {
            assert_eq!(
                action_of(&run(id, None), &g),
                Action::Stuck {
                    node: id.into(),
                    key: "kind".into()
                },
                "{id}"
            );
        }
        assert_eq!(
            action_of(
                &Step::FanOut {
                    node_id: "work".into(),
                    items: vec![0, 3]
                },
                &g
            ),
            Action::FanOut {
                node_id: "work".into(),
                items: vec![0, 3]
            }
        );
        assert_eq!(
            action_of(
                &Step::MergeItem {
                    node_id: "work".into(),
                    item: 1
                },
                &g
            ),
            Action::FanBoundary(FanOp::MergeItem {
                node_id: "work".into(),
                item: 1
            })
        );
        assert_eq!(
            action_of(
                &Step::SettleRevert {
                    node_id: "work".into(),
                    item: None
                },
                &g
            ),
            Action::FanBoundary(FanOp::SettleRevert {
                node_id: "work".into(),
                item: None
            })
        );
        assert_eq!(
            action_of(
                &Step::Orphan {
                    node_id: "work".into(),
                    item: 4
                },
                &g
            ),
            Action::FanBoundary(FanOp::Orphan {
                node_id: "work".into(),
                item: 4
            })
        );
        assert_eq!(
            action_of(
                &Step::Replan {
                    node_id: "plan".into()
                },
                &g
            ),
            Action::Replan {
                node_id: "plan".into()
            }
        );
        assert_eq!(action_of(&Step::Wait, &g), Action::Wait);
        assert_eq!(
            action_of(
                &Step::GateErrored {
                    node_id: "gate".into(),
                    item: Some(1)
                },
                &g
            ),
            Action::GateErrored {
                node: "gate".into(),
                item: Some(1)
            }
        );
        assert_eq!(
            action_of(
                &Step::Stuck(GraphStuck {
                    node: "more".into(),
                    key: "plan.done".into()
                }),
                &g
            ),
            Action::Stuck {
                node: "more".into(),
                key: "plan.done".into()
            }
        );
        assert_eq!(
            action_of(&Step::Finish(Outcome::Stopped), &g),
            Action::Finish(Outcome::Stopped)
        );
    }

    #[test]
    fn node_state_of_status_reads_the_words_the_code_writes() {
        // An agent node.
        assert_eq!(node_state_of_status("pending", false, false), None);
        assert_eq!(
            node_state_of_status("running", false, false),
            Some(NodeState::Running)
        );
        assert_eq!(
            node_state_of_status("passed", false, false),
            Some(NodeState::Done)
        );
        assert_eq!(
            node_state_of_status("failed", false, false),
            Some(NodeState::Failed)
        );
        assert_eq!(
            node_state_of_status("cancelled", false, false),
            Some(NodeState::Cancelled)
        );
        // A command node: `passed` is a green gate, `gate_errored` is silence.
        assert_eq!(
            node_state_of_status("passed", true, true),
            Some(NodeState::GatePassed)
        );
        assert_eq!(
            node_state_of_status("failed", true, true),
            Some(NodeState::GateRed {
                retries_left: false
            })
        );
        assert_eq!(
            node_state_of_status("failed", true, false),
            Some(NodeState::Failed)
        );
        assert_eq!(
            node_state_of_status("gate_errored", true, true),
            Some(NodeState::GateErrored)
        );
        assert_eq!(node_state_of_status("nonsense", false, false), None);
    }

    #[test]
    fn effective_disabled_marks_the_project_switch_only() {
        let mut graph = EffectiveGraph::default();
        graph.disabled.insert("review".to_string());
        let got = effective_disabled(&graph, Mode::Active);
        assert_eq!(got.len(), 1);
        assert_eq!(got["review"], Disabled::Project);
    }
}
