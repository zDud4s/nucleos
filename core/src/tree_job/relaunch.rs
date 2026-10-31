//! Starting a node's agent again: what each `launch` was asked to start (`tree_launches`), and the
//! recovery that reads it back after a boot (`relaunch_resumed`) or an answered decision
//! (`answer_and_relaunch`).
//!
//! Every relaunch runs under a key, so a second call with the same key starts nothing new. The
//! public names are re-exported by `open`, which is where callers import them from.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use sqlx::SqlitePool;

use super::decisions::{self, DecisionError};
use super::engine::branch_node_may_close;
use super::open::now;
use super::store::{self, TreeJob};
use crate::execution::concurrency::LIVE_RUN_STATUSES;
use crate::state::AppState;
use crate::tree_cost::budget::{self, Permit};

const PARKED: &str = "parked";
/// The feed kind of a node that needed a relaunch and did not get one.
const RELAUNCH_SKIPPED_KIND: &str = "work_tree_relaunch_skipped";

// ----- stored launches -----

/// What a `launch` was asked to start, as `create_run_inner` was given it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLaunch {
    pub node_id: String,
    pub key: String,
    pub prompt: String,
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub mode: String,
}

type LaunchRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);

fn launch_from(row: LaunchRow) -> StoredLaunch {
    let (node_id, key, prompt, project_id, cwd, mode) = row;
    StoredLaunch {
        node_id,
        key,
        prompt,
        project_id,
        cwd,
        mode,
    }
}

const LAUNCH_COLUMNS: &str =
    "SELECT node_id, key, prompt, project_id, cwd, mode FROM tree_launches";

/// Stores a launch. The first write per `(node, key)` stays, as the launch itself runs once per key.
pub async fn save_launch(pool: &SqlitePool, launch: &StoredLaunch) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO tree_launches (node_id, key, prompt, project_id, cwd, mode, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (node_id, key) DO NOTHING",
    )
    .bind(&launch.node_id)
    .bind(&launch.key)
    .bind(&launch.prompt)
    .bind(&launch.project_id)
    .bind(&launch.cwd)
    .bind(&launch.mode)
    .bind(now())
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn stored_launch(
    pool: &SqlitePool,
    node_id: &str,
    key: &str,
) -> Result<Option<StoredLaunch>, sqlx::Error> {
    let row: Option<LaunchRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LAUNCH_COLUMNS} WHERE node_id = ? AND key = ?"
    )))
    .bind(node_id)
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(launch_from))
}

/// The launch written last for `node_id`: `seq` orders the writes, not the key or a clock.
pub async fn latest_launch(
    pool: &SqlitePool,
    node_id: &str,
) -> Result<Option<StoredLaunch>, sqlx::Error> {
    let row: Option<LaunchRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LAUNCH_COLUMNS} WHERE node_id = ? ORDER BY seq DESC LIMIT 1"
    )))
    .bind(node_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(launch_from))
}

/// Key prefixes of the relaunches that carry a one-off suffix (the wrap, died, fix and resume
/// relaunches of `wrap`, `driver` and `relaunch_resumed`). Their stored prompts are never a base
/// for a new suffix, or each relaunch would repeat the sentences of the earlier ones.
pub(super) const WRAP_PREFIX: &str = "wrap-";
pub(super) const DIED_PREFIX: &str = "died-";
pub(super) const FIX_PREFIX: &str = "fix-";
const RESUME_PREFIX: &str = "resume-";
const SUFFIXED_KEY_PREFIXES: [&str; 4] = [WRAP_PREFIX, DIED_PREFIX, FIX_PREFIX, RESUME_PREFIX];

/// The launch a suffix is appended to: the latest one that is not itself a suffixed relaunch, i.e.
/// the original prompt, or the last answer relaunch (whose prompt carries the owner's answers, which
/// must survive a later wrap or fix). Falls back to the latest launch when every one is suffixed.
pub(super) async fn base_launch(
    pool: &SqlitePool,
    node_id: &str,
) -> Result<Option<StoredLaunch>, sqlx::Error> {
    let rows: Vec<LaunchRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LAUNCH_COLUMNS} WHERE node_id = ? ORDER BY seq DESC"
    )))
    .bind(node_id)
    .fetch_all(pool)
    .await?;
    let mut launches: Vec<StoredLaunch> = rows.into_iter().map(launch_from).collect();
    let base = launches.iter().position(|launch| {
        !SUFFIXED_KEY_PREFIXES
            .iter()
            .any(|prefix| launch.key.starts_with(prefix))
    });
    Ok(match base {
        Some(index) => Some(launches.swap_remove(index)),
        None if launches.is_empty() => None,
        None => Some(launches.swap_remove(0)),
    })
}

// ----- relaunch -----

/// What one relaunch pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relaunched {
    /// `(node id, run id)` of every node whose agent was started (or found started) by the pass.
    pub started: Vec<(String, i64)>,
    /// `(node id, reason)` of every node that needed a relaunch and did not get one.
    pub skipped: Vec<(String, String)>,
}

async fn note_skipped(pool: &SqlitePool, relaunched: &mut Relaunched, node: &str, reason: String) {
    let summary = format!("node {node} was not started again: {reason}");
    tracing::warn!(node, %reason, "relaunch skipped");
    let _ = crate::feed::append(pool, None, RELAUNCH_SKIPPED_KIND, &summary, None, None).await;
    relaunched.skipped.push((node.to_owned(), reason));
}

pub(super) async fn run_is_live(pool: &SqlitePool, run_id: &str) -> Result<bool, sqlx::Error> {
    let Ok(run_number) = run_id.parse::<i64>() else {
        return Ok(false);
    };
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
        .bind(run_number)
        .fetch_optional(pool)
        .await?;
    Ok(status.is_some_and(|status| LIVE_RUN_STATUSES.contains(&status.as_str())))
}

async fn start_again(
    state: &AppState,
    launch: &StoredLaunch,
    key: &str,
    prompt: String,
) -> Result<i64, String> {
    store::launch_recovering(
        state,
        &launch.node_id,
        key,
        prompt,
        launch.project_id.clone(),
        launch.cwd.clone(),
        &launch.mode,
    )
    .await
    .map_err(|error| error.to_string())
}

async fn cached_job<'a>(
    pool: &SqlitePool,
    jobs: &'a mut HashMap<String, Option<TreeJob>>,
    id: &str,
) -> Option<&'a TreeJob> {
    if !jobs.contains_key(id) {
        let loaded = store::load(pool, id).await.ok().flatten();
        jobs.insert(id.to_owned(), loaded);
    }
    jobs.get(id).and_then(Option::as_ref)
}

/// Whether the branch behind `node_id` is parked: only an answer starts such a node again.
async fn branch_is_parked(
    pool: &SqlitePool,
    jobs: &mut HashMap<String, Option<TreeJob>>,
    node_id: &str,
) -> bool {
    let linked: Result<Option<Option<String>>, _> =
        sqlx::query_scalar("SELECT tree_job_id FROM work_nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(pool)
            .await;
    let Ok(Some(Some(job_id))) = linked else {
        return false;
    };
    cached_job(pool, jobs, &job_id).await.is_some_and(|job| {
        job.manifest
            .branches
            .values()
            .any(|branch| branch.node_id.as_deref() == Some(node_id) && branch.state == PARKED)
    })
}

/// Why the tree budget refuses an automatic relaunch of `node_id`, or `None` when it permits one
/// (or the node has no tree row). The owner's answer relaunch does not ask: the owner decided.
pub(super) async fn budget_refusal(pool: &SqlitePool, node_id: &str) -> Option<String> {
    let root: Option<String> = sqlx::query_scalar("SELECT root_id FROM work_nodes WHERE id = ?")
        .bind(node_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    match budget::permits_next_node(pool, &root?).await {
        Permit::Yes => None,
        Permit::No { reason } => Some(reason),
    }
}

/// The question asked when the budget stopped an automatic relaunch of `node_id`.
pub(super) fn refusal_question(node_id: &str, reason: &str) -> String {
    format!(
        "Node {node_id} was not started again: the tree budget does not permit it ({reason}). \
         Raise the budget, or say how the node should go on."
    )
}

/// Starts again the agents that boot recovery left without a run, once per pass:
///
/// - every launch whose empty reservation `resume_all` cleared, from its stored prompt and under
///   its own key (a launch that was never stored, or whose branch is parked, is skipped);
/// - every running node of a branch of an unfinished job that `reconcile_running` spared and whose
///   run is no longer live, from its latest launch, under the key `resume-<dead run id>`.
///
/// A failure of one node goes in `skipped` and the feed and stops no other. Call it once per boot
/// where an `AppState` exists. A second pass over the same `Resumed` starts nothing new while the
/// runs of the first are live: the keys replay, and a spared node with a live run is left alone. A
/// run that died between two passes is a new dead run with a new key, and gets a second start.
pub async fn relaunch_resumed(state: &AppState, resumed: &store::Resumed) -> Relaunched {
    let pool = &state.pool;
    let mut relaunched = Relaunched::default();
    let mut jobs: HashMap<String, Option<TreeJob>> = HashMap::new();
    let mut handled: HashSet<String> = HashSet::new();

    for (node, key) in &resumed.cleared_launches {
        if branch_is_parked(pool, &mut jobs, node).await {
            relaunched.skipped.push((
                node.clone(),
                "its branch is parked; only an answer starts it".to_owned(),
            ));
            continue;
        }
        match stored_launch(pool, node, key).await {
            Ok(Some(launch)) => {
                match start_again(state, &launch, key, launch.prompt.clone()).await {
                    Ok(run_id) => {
                        handled.insert(node.clone());
                        relaunched.started.push((node.clone(), run_id));
                    }
                    Err(reason) => note_skipped(pool, &mut relaunched, node, reason).await,
                }
            }
            Ok(None) => {
                let reason = format!("no prompt was stored for launch {key}");
                note_skipped(pool, &mut relaunched, node, reason).await;
            }
            Err(error) => note_skipped(pool, &mut relaunched, node, error.to_string()).await,
        }
    }

    for job_id in &resumed.jobs {
        let nodes: Vec<String> = match cached_job(pool, &mut jobs, job_id).await {
            Some(job) => job
                .manifest
                .branches
                .values()
                .filter(|branch| branch.state != PARKED && !branch_node_may_close(branch))
                .filter_map(|branch| branch.node_id.clone())
                .collect(),
            None => continue,
        };
        for node in nodes {
            if handled.contains(&node) || resumed.failed_nodes.contains(&node) {
                continue;
            }
            let row: Result<Option<(String, Option<String>)>, _> =
                sqlx::query_as("SELECT status, run_id FROM work_nodes WHERE id = ?")
                    .bind(&node)
                    .fetch_optional(pool)
                    .await;
            let Ok(Some((status, Some(run_id)))) = row else {
                continue;
            };
            if status != "running" || run_is_live(pool, &run_id).await.unwrap_or(true) {
                continue;
            }
            if let Some(reason) = budget_refusal(pool, &node).await {
                // The boot does not spend past the tree's limit: the owner is asked once.
                let question = refusal_question(&node, &reason);
                if let Err(error) = decisions::park_or_ask(pool, job_id, &node, &question).await {
                    tracing::warn!(%node, %error, "could not park a node over the tree budget");
                }
                note_skipped(pool, &mut relaunched, &node, reason).await;
                continue;
            }
            let key = format!("{RESUME_PREFIX}{run_id}");
            match relaunch_node(state, &node, &run_id, &key, None).await {
                Ok(new_run) => {
                    handled.insert(node.clone());
                    relaunched.started.push((node, new_run));
                }
                Err(reason) => note_skipped(pool, &mut relaunched, &node, reason).await,
            }
        }
    }
    relaunched
}

/// Starts `node_id` again under `key`, with the stored project, cwd and mode. Without a suffix the
/// prompt is the latest stored launch's, unchanged. With one it is the node's base launch (see
/// [`base_launch`]) plus `prompt_suffix`, so suffixes of earlier relaunches are not repeated.
/// `dead_run` only names the run that ended, for the error when no launch was ever stored. The key
/// replays, so a second call with the same key starts nothing new.
pub(super) async fn relaunch_node(
    state: &AppState,
    node_id: &str,
    dead_run: &str,
    key: &str,
    prompt_suffix: Option<&str>,
) -> Result<i64, String> {
    let found = match prompt_suffix {
        Some(_) => base_launch(&state.pool, node_id).await,
        None => latest_launch(&state.pool, node_id).await,
    };
    match found {
        Ok(Some(launch)) => {
            let prompt = match prompt_suffix {
                Some(suffix) => format!("{}{suffix}", launch.prompt),
                None => launch.prompt.clone(),
            };
            start_again(state, &launch, key, prompt).await
        }
        Ok(None) => Err(format!("run {dead_run} is gone and no launch was stored")),
        Err(error) => Err(error.to_string()),
    }
}

// ----- answer and relaunch -----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answered {
    pub decision: decisions::Decision,
    /// The run started for the node of the resumed branch, if one was.
    pub relaunched: Option<i64>,
}

const DECISION_COLUMNS: &str =
    "SELECT id, tree_id, node_id, question, answer, origin, at, status FROM decisions";

/// The node of the branch that waits on this still-open decision, read before `answer` moves it:
/// `answer` returns only the decision, and afterwards the park record is gone.
async fn node_waiting_on(pool: &SqlitePool, decision_id: &str) -> Option<String> {
    let decision: decisions::Decision = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{DECISION_COLUMNS} WHERE id = ?"
    )))
    .bind(decision_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let (tree_id, node_id) = (decision.tree_id?, decision.node_id?);
    let job = store::load(pool, &tree_id).await.ok().flatten()?;
    // A root without a branch waits on the decision its `root_park` record names.
    let root_waits = job.root_node_id == node_id
        && job
            .manifest
            .extra
            .get(decisions::ROOT_PARK_KEY)
            .and_then(|record| record.get("decision_id"))
            .and_then(Value::as_str)
            == Some(decision_id);
    if root_waits {
        return Some(node_id);
    }
    let waits = job.manifest.branches.values().any(|branch| {
        branch.node_id.as_deref() == Some(node_id.as_str())
            && branch.state == PARKED
            && branch
                .park
                .as_ref()
                .and_then(|record| record.get("decision_id"))
                .and_then(Value::as_str)
                == Some(decision_id)
    });
    waits.then_some(node_id)
}

/// Answers a decision and, when that put a parked branch back to work and its node has no live
/// run, starts the node's agent again from its latest stored launch plus the answer, under the key
/// `decision-<id>`. The answer is kept whatever the relaunch does; a relaunch that cannot happen
/// is a feed line, not an error. Every entry point that answers a decision calls this.
pub async fn answer_and_relaunch(
    state: &AppState,
    decision_id: &str,
    text: &str,
    origin: &str,
) -> Result<Answered, DecisionError> {
    let pool = &state.pool;
    let waiting = node_waiting_on(pool, decision_id).await;
    let decision = decisions::answer(pool, decision_id, text, origin).await?;
    let Some(node) = waiting else {
        return Ok(Answered {
            decision,
            relaunched: None,
        });
    };
    let relaunched = relaunch_after_answer(state, &decision, &node, text).await;
    Ok(Answered {
        decision,
        relaunched,
    })
}

async fn relaunch_after_answer(
    state: &AppState,
    decision: &decisions::Decision,
    node: &str,
    text: &str,
) -> Option<i64> {
    let pool = &state.pool;
    let mut skipped = Relaunched::default();
    let run: Result<Option<Option<String>>, _> =
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(node)
            .fetch_optional(pool)
            .await;
    match run {
        Ok(Some(Some(run_id))) => match run_is_live(pool, &run_id).await {
            Ok(false) => {}
            Ok(true) => return None,
            Err(error) => {
                note_skipped(pool, &mut skipped, node, error.to_string()).await;
                return None;
            }
        },
        Ok(_) => {}
        Err(error) => {
            note_skipped(pool, &mut skipped, node, error.to_string()).await;
            return None;
        }
    }
    // The base launch, not the latest: a wrap or fix suffix of an earlier relaunch is not carried
    // into the answer's prompt, while an earlier answer (a `decision-` launch) is.
    let launch = match base_launch(pool, node).await {
        Ok(Some(launch)) => launch,
        Ok(None) => {
            let reason = "no launch was stored to start it from".to_owned();
            note_skipped(pool, &mut skipped, node, reason).await;
            return None;
        }
        Err(error) => {
            note_skipped(pool, &mut skipped, node, error.to_string()).await;
            return None;
        }
    };
    let prompt = format!(
        "{}\n\nYou asked the owner: {}\nThe owner answered: {text}",
        launch.prompt, decision.question
    );
    let key = format!("decision-{}", decision.id);
    match start_again(state, &launch, &key, prompt).await {
        Ok(run_id) => Some(run_id),
        Err(reason) => {
            note_skipped(pool, &mut skipped, node, reason).await;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tree_job::model::{Branch, Manifest};

    const JOB: &str = "relaunch-job";
    const NODE: &str = "relaunch-node";
    const ROOT: &str = "relaunch-root";
    const CREATED: &str = "2026-10-31T00:00:00+00:00";

    #[tokio::test]
    async fn successor_key_first_proposal_wins() {
        let pool = crate::testdb::fresh_pool().await;

        assert_eq!(
            successor_key(&pool, NODE, "55", "resume-55").await.unwrap(),
            "resume-55"
        );
        assert_eq!(
            successor_key(&pool, NODE, "55", "died-55").await.unwrap(),
            "resume-55"
        );
        assert_eq!(
            successor_key(&pool, NODE, "56", "died-56").await.unwrap(),
            "died-56"
        );
    }

    /// An owner's answer decides, so the tree budget does not stop it. This passes today (the answer
    /// relaunch has no budget check) and is here so that a budget check added to the automatic
    /// relaunches cannot reach `answer_and_relaunch`.
    #[tokio::test]
    async fn an_owners_answer_relaunches_even_past_the_budget() {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        let pool = &state.pool;

        let mut manifest = Manifest::new(JOB, "feat/relaunch", "Answer it", "C:/nowhere", &[]);
        let mut branch = Branch::new("branch", &["core/src/**".to_owned()], "core", None);
        branch.state = "building".to_owned();
        branch.node_id = Some(NODE.to_owned());
        manifest.branches.insert(branch.name.clone(), branch);
        store::save(
            pool,
            &TreeJob {
                id: JOB.to_owned(),
                root_node_id: ROOT.to_owned(),
                project_id: None,
                feature_branch: Some("feat/relaunch".to_owned()),
                goal: "Answer it".to_owned(),
                state: "open".to_owned(),
                manifest,
                contract: None,
                created_at: CREATED.to_owned(),
                updated_at: CREATED.to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, created_at) VALUES (71, 'p', 'interrupted', ?)",
        )
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     tree_job_id, created_at, updated_at) \
             VALUES (?, ?, 1, 'orchestrator', 'title', 'goal', 'running', '71', ?, ?, ?)",
        )
        .bind(NODE)
        .bind(ROOT)
        .bind(JOB)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
        save_launch(
            pool,
            &StoredLaunch {
                node_id: NODE.to_owned(),
                key: "initial".to_owned(),
                prompt: "original prompt".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
        let asked = decisions::park(pool, JOB, NODE, "which way?")
            .await
            .unwrap();
        // The tree spent five dollars of a one dollar limit.
        sqlx::query(
            "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
             VALUES (?, 1.0, 80.0, ?, ?)",
        )
        .bind(ROOT)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE runs SET cost_usd = 5.0 WHERE id = 71")
            .execute(pool)
            .await
            .unwrap();

        let answered = answer_and_relaunch(&state, &asked.id, "left", "shell")
            .await
            .unwrap();

        assert!(
            answered.relaunched.is_some(),
            "the owner decided: the node starts again"
        );
        let started: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tree_transitions \
             WHERE node_id = ? AND transition = 'launch' AND key = ?",
        )
        .bind(NODE)
        .bind(format!("decision-{}", asked.id))
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(started, 1);
    }

    /// The boot relaunch is automatic, so the tree budget stops it: the node keeps its dead run and
    /// the owner is asked, once.
    #[tokio::test]
    async fn a_resumed_node_at_boot_is_not_relaunched_past_the_budget() {
        let (state, _runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        let pool = &state.pool;
        let mut manifest = Manifest::new(JOB, "feat/relaunch", "Resume it", "C:/nowhere", &[]);
        let mut branch = Branch::new("branch", &["core/src/**".to_owned()], "core", None);
        branch.state = "building".to_owned();
        branch.node_id = Some(NODE.to_owned());
        manifest.branches.insert(branch.name.clone(), branch);
        store::save(
            pool,
            &TreeJob {
                id: JOB.to_owned(),
                root_node_id: ROOT.to_owned(),
                project_id: None,
                feature_branch: Some("feat/relaunch".to_owned()),
                goal: "Resume it".to_owned(),
                state: "open".to_owned(),
                manifest,
                contract: None,
                created_at: CREATED.to_owned(),
                updated_at: CREATED.to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, cost_usd, created_at) \
             VALUES (72, 'p', 'interrupted', 5.0, ?)",
        )
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id, \
                                     tree_job_id, created_at, updated_at) \
             VALUES (?, ?, 1, 'orchestrator', 'title', 'goal', 'running', '72', ?, ?, ?)",
        )
        .bind(NODE)
        .bind(ROOT)
        .bind(JOB)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
        save_launch(
            pool,
            &StoredLaunch {
                node_id: NODE.to_owned(),
                key: "initial".to_owned(),
                prompt: "original prompt".to_owned(),
                project_id: None,
                cwd: None,
                mode: "shadow".to_owned(),
            },
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tree_budgets (tree_id, limit_usd, soft_pct, created_at, updated_at) \
             VALUES (?, 1.0, 80.0, ?, ?)",
        )
        .bind(ROOT)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
        let resumed = store::Resumed {
            jobs: vec![JOB.to_owned()],
            ..Default::default()
        };

        let relaunched = relaunch_resumed(&state, &resumed).await;
        relaunch_resumed(&state, &resumed).await;

        assert!(relaunched.started.is_empty(), "{relaunched:?}");
        let run: String = sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(NODE)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(run, "72", "no new run was started");
        let asked: Vec<String> =
            sqlx::query_scalar("SELECT question FROM decisions WHERE node_id = ? AND status = ?")
                .bind(NODE)
                .bind(decisions::STATUS_OPEN)
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(asked.len(), 1, "the owner is told once: {asked:?}");
        assert!(asked[0].to_lowercase().contains("budget"), "{:?}", asked[0]);
        let stored = store::load(pool, JOB).await.unwrap().unwrap();
        assert_eq!(stored.manifest.branches["branch"].state, "parked");
    }
}
