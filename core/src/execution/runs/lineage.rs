//! The chain of runs one piece of work went through, as `GET /runs/{id}` serves it, and what the
//! chain is part of (a tree node or a job item).
//!
//! Only `successor_run_id` links define the chain; `lineage_root_id` does not.

use crate::handoff::SuccessorReason;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// How many runs the walk takes behind and ahead of the run asked about. A longer chain is cut and
/// reported `lineage_truncated`.
pub const LINEAGE_CAP: usize = 50;

/// One run of a chain.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LineageLeg {
    pub id: i64,
    pub status: String,
    pub num_turns: Option<i64>,
    /// `runs.created_at`.
    pub started_at: Option<String>,
    /// `runs.completed_at`.
    pub ended_at: Option<String>,
    /// The wire value of a KNOWN `SuccessorReason`; `None` for the last leg and for a value this
    /// build does not know.
    pub ended_because: Option<String>,
    pub gate_status: Option<String>,
    pub gate_exit_code: Option<i32>,
    pub exit_code: Option<i32>,
}

/// What a chain belongs to. Data only: the shell has no route for either.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PartOf {
    Tree {
        tree_id: String,
        tree_title: String,
        node_id: String,
        node_title: String,
    },
    Job {
        job_id: i64,
        job_title: String,
        item_id: i64,
        item_ordinal: i64,
    },
}

async fn successor_of(pool: &sqlx::SqlitePool, id: i64) -> sqlx::Result<Option<i64>> {
    let next: Option<Option<i64>> =
        sqlx::query_scalar("SELECT successor_run_id FROM runs WHERE id = ?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(next.flatten())
}

/// Takes up to `LINEAGE_CAP` steps from `start` with `step`, never revisiting an id in `seen`.
/// Returns the ids in walk order and whether a cycle or the cap cut the walk.
async fn walk<F, Fut>(
    start: i64,
    seen: &mut HashSet<i64>,
    mut step: F,
) -> sqlx::Result<(Vec<i64>, bool)>
where
    F: FnMut(i64) -> Fut,
    Fut: std::future::Future<Output = sqlx::Result<Option<i64>>>,
{
    let mut ids = Vec::new();
    let mut at = start;
    loop {
        let Some(next) = step(at).await? else {
            return Ok((ids, false));
        };
        if seen.contains(&next) || ids.len() == LINEAGE_CAP {
            return Ok((ids, true));
        }
        seen.insert(next);
        ids.push(next);
        at = next;
    }
}

type LegRow = (
    i64,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i32>,
    Option<i32>,
);

/// The chain `run_id` belongs to, root to newest, and whether it was cut (cycle or cap).
pub(super) async fn load(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> sqlx::Result<(Vec<LineageLeg>, bool)> {
    let mut seen = HashSet::from([run_id]);
    let (mut behind, cut_back) = walk(run_id, &mut seen, |id| {
        super::handoff::predecessor_of(pool, id)
    })
    .await?;
    let (ahead, cut_ahead) = walk(run_id, &mut seen, |id| successor_of(pool, id)).await?;
    behind.reverse();
    let mut ids = behind;
    ids.push(run_id);
    ids.extend(ahead);

    let list = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into());
    let rows: Vec<LegRow> = sqlx::query_as(
        "SELECT id, status, num_turns, created_at, completed_at, successor_reason, gate_status,
                gate_exit_code, exit_code
         FROM runs WHERE id IN (SELECT value FROM json_each(?))",
    )
    .bind(list)
    .fetch_all(pool)
    .await?;
    let mut by_id: HashMap<i64, LegRow> = rows.into_iter().map(|row| (row.0, row)).collect();
    let legs = ids
        .iter()
        .filter_map(|id| by_id.remove(id))
        .map(|row| LineageLeg {
            id: row.0,
            status: row.1,
            num_turns: row.2,
            started_at: row.3,
            ended_at: row.4,
            ended_because: row
                .5
                .as_deref()
                .and_then(SuccessorReason::from_wire)
                .map(|reason| reason.as_wire().to_owned()),
            gate_status: row.6,
            gate_exit_code: row.7,
            exit_code: row.8,
        })
        .collect();
    Ok((legs, cut_back || cut_ahead))
}

/// The tree node, else the job item, this chain is part of; `None` for a loose run.
pub(super) async fn part_of(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    chain_ids: &[i64],
) -> sqlx::Result<Option<PartOf>> {
    // Chain ids, not just this run: a relaunched node's `run_id` may point at another leg.
    let texts: Vec<String> = chain_ids.iter().map(i64::to_string).collect();
    let list = serde_json::to_string(&texts).unwrap_or_else(|_| "[]".into());
    let node: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT n.id, n.title, n.root_id, (SELECT title FROM work_nodes WHERE id = n.root_id)
         FROM work_nodes n
         WHERE n.run_id IN (SELECT CAST(value AS TEXT) FROM json_each(?))
         ORDER BY n.updated_at DESC LIMIT 1",
    )
    .bind(list)
    .fetch_optional(pool)
    .await?;
    if let Some((node_id, node_title, tree_id, tree_title)) = node {
        return Ok(Some(PartOf::Tree {
            tree_title: tree_title.unwrap_or_else(|| tree_id.clone()),
            tree_id,
            node_id,
            node_title,
        }));
    }

    let job: Option<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT j.id,
                COALESCE(NULLIF(j.rule_name, ''), NULLIF(substr(j.prompt, 1, 80), ''),
                         'Job ' || j.id),
                i.id, i.ordinal
         FROM runs r
         JOIN jobs j ON j.id = r.job_id
         JOIN job_items i ON i.id = r.item_id
         WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    Ok(job.map(|(job_id, job_title, item_id, item_ordinal)| PartOf::Job {
        job_id,
        job_title,
        item_id,
        item_ordinal,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runs::api::{RunStatusResponse, get_run};
    use crate::execution::runs::testkit::test_state_with_runner;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use tower::ServiceExt;

    async fn insert(pool: &sqlx::SqlitePool, id: i64, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, created_at)
             VALUES (?, 'p', ?, 'worktree', '2026-11-01T00:00:00Z')",
        )
        .bind(id)
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn link(pool: &sqlx::SqlitePool, from: i64, to: i64, reason: Option<&str>) {
        sqlx::query("UPDATE runs SET successor_run_id = ?, successor_reason = ? WHERE id = ?")
            .bind(to)
            .bind(reason)
            .bind(from)
            .execute(pool)
            .await
            .unwrap();
    }

    fn ids(legs: &[LineageLeg]) -> Vec<i64> {
        legs.iter().map(|leg| leg.id).collect()
    }

    #[tokio::test]
    async fn a_standalone_run_is_a_chain_of_one() {
        let pool = crate::testdb::fresh_pool().await;
        insert(&pool, 1, "completed").await;
        let (legs, truncated) = load(&pool, 1).await.unwrap();
        assert_eq!(legs.len(), 1);
        assert_eq!(legs[0].id, 1);
        assert!(!truncated);
        assert_eq!(legs[0].ended_because, None);
    }

    #[tokio::test]
    async fn the_chain_is_ordered_root_to_newest_from_any_member() {
        let pool = crate::testdb::fresh_pool().await;
        for id in 1..=3 {
            insert(&pool, id, "completed").await;
        }
        link(&pool, 1, 2, Some("context_full")).await;
        link(&pool, 2, 3, Some("approval_approved")).await;
        for from in 1..=3 {
            let (legs, truncated) = load(&pool, from).await.unwrap();
            assert_eq!(ids(&legs), vec![1, 2, 3]);
            let reasons: Vec<_> = legs.iter().map(|l| l.ended_because.clone()).collect();
            assert_eq!(
                reasons,
                vec![
                    Some("context_full".to_owned()),
                    Some("approval_approved".to_owned()),
                    None
                ]
            );
            assert!(!truncated);
        }
    }

    #[tokio::test]
    async fn an_unknown_reason_is_reported_as_none() {
        let pool = crate::testdb::fresh_pool().await;
        insert(&pool, 1, "completed").await;
        insert(&pool, 2, "completed").await;
        link(&pool, 1, 2, Some("from_the_future")).await;
        let (legs, _) = load(&pool, 1).await.unwrap();
        assert_eq!(legs[0].ended_because, None);
        assert_eq!(legs[0].status, "completed");
    }

    #[tokio::test]
    async fn a_cycle_stops_the_walk_and_marks_it_truncated() {
        let pool = crate::testdb::fresh_pool().await;
        for id in 1..=3 {
            insert(&pool, id, "completed").await;
        }
        link(&pool, 1, 2, None).await;
        link(&pool, 2, 1, None).await;
        let (legs, truncated) = load(&pool, 1).await.unwrap();
        let mut got = ids(&legs);
        got.sort();
        assert_eq!(got, vec![1, 2]);
        assert!(truncated);

        link(&pool, 3, 3, None).await;
        let (legs, truncated) = load(&pool, 3).await.unwrap();
        assert_eq!(ids(&legs), vec![3]);
        assert!(truncated);
    }

    #[tokio::test]
    async fn a_chain_longer_than_the_cap_is_cut_in_both_directions() {
        let pool = crate::testdb::fresh_pool().await;
        for id in 1..=130 {
            insert(&pool, id, "completed").await;
        }
        for id in 1..130 {
            link(&pool, id, id + 1, Some("context_full")).await;
        }
        let (legs, truncated) = load(&pool, 65).await.unwrap();
        assert_eq!(legs.len(), 2 * LINEAGE_CAP + 1);
        assert!(truncated);
        assert_eq!(legs.first().unwrap().id, 15);
        assert_eq!(legs.last().unwrap().id, 115);
    }

    async fn node(pool: &sqlx::SqlitePool, id: &str, root: &str, title: &str, run: Option<&str>) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id,
                                     created_at, updated_at)
             VALUES (?, ?, 0, ?, ?, 'g', 'running', ?, '2026-11-01T00:00:00Z', '2026-11-01T00:00:00Z')",
        )
        .bind(id)
        .bind(root)
        .bind(if id == root { "root" } else { "leaf" })
        .bind(title)
        .bind(run)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn part_of_names_the_tree_and_node_when_a_node_holds_a_run_of_the_chain() {
        let pool = crate::testdb::fresh_pool().await;
        insert(&pool, 1, "completed").await;
        insert(&pool, 2, "running").await;
        link(&pool, 1, 2, Some("context_full")).await;
        node(&pool, "r1", "r1", "Tree A", None).await;
        node(&pool, "n1", "r1", "Leaf B", Some("2")).await;
        let got = part_of(&pool, 1, &[1, 2]).await.unwrap();
        assert_eq!(
            got,
            Some(PartOf::Tree {
                tree_id: "r1".into(),
                tree_title: "Tree A".into(),
                node_id: "n1".into(),
                node_title: "Leaf B".into(),
            })
        );
    }

    async fn job_run(
        pool: &sqlx::SqlitePool,
        run: i64,
        rule: Option<&str>,
        prompt: Option<&str>,
    ) -> (i64, i64) {
        let job: i64 = sqlx::query_scalar(
            "INSERT INTO jobs (project_id, project_root, rule_name, prompt, status, max_items,
                               created_at)
             VALUES ('p', '/p', ?, ?, 'implementing', 1, '2026-11-01T00:00:00Z') RETURNING id",
        )
        .bind(rule)
        .bind(prompt)
        .fetch_one(pool)
        .await
        .unwrap();
        let item: i64 = sqlx::query_scalar(
            "INSERT INTO job_items (job_id, ordinal, description, status)
             VALUES (?, 3, 'd', 'running') RETURNING id",
        )
        .bind(job)
        .fetch_one(pool)
        .await
        .unwrap();
        insert(pool, run, "running").await;
        sqlx::query("UPDATE runs SET job_id = ?, item_id = ? WHERE id = ?")
            .bind(job)
            .bind(item)
            .bind(run)
            .execute(pool)
            .await
            .unwrap();
        (job, item)
    }

    #[tokio::test]
    async fn part_of_names_the_job_and_item_for_a_job_run() {
        let pool = crate::testdb::fresh_pool().await;
        let (job, item) = job_run(&pool, 1, Some("nightly"), None).await;
        assert_eq!(
            part_of(&pool, 1, &[1]).await.unwrap(),
            Some(PartOf::Job {
                job_id: job,
                job_title: "nightly".into(),
                item_id: item,
                item_ordinal: 3
            })
        );
        let (job2, _) = job_run(&pool, 2, None, None).await;
        match part_of(&pool, 2, &[2]).await.unwrap() {
            Some(PartOf::Job { job_title, .. }) => assert_eq!(job_title, format!("Job {job2}")),
            other => panic!("expected a job, got {other:?}"),
        }
        insert(&pool, 3, "completed").await;
        assert_eq!(part_of(&pool, 3, &[3]).await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_run_serialises_the_lineage_and_part_of() {
        let (state, _) = test_state_with_runner(None, crate::state::DEFAULT_RUN_TIMEOUT).await;
        insert(&state.pool, 1, "completed").await;
        insert(&state.pool, 2, "running").await;
        link(&state.pool, 1, 2, Some("context_full")).await;
        let app = Router::new()
            .route("/runs/{id}", get(get_run))
            .with_state(state);
        let response = app
            .oneshot(Request::get("/runs/2").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["lineage"].as_array().unwrap().len(), 2);
        assert_eq!(json["lineage"][0]["ended_because"], "context_full");
        assert_eq!(json["lineage_truncated"], false);
        assert!(json["part_of"].is_null());
        let _: RunStatusResponse = serde_json::from_value(json).unwrap();
    }
}
