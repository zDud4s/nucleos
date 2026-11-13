//! The routes behind the `decision_answer` and `decision_list` MCP tools: a conductor session reads
//! and answers tree decisions without the shell.
//!
//! Answering is an owner-level act. Both routes sit on no `Scope::Run` allowlist (`auth::grants`),
//! so the key of a tree-node run is refused here before a handler runs; the MCP handler refuses a
//! worktree on a `tree/` branch as a second fence. An answer goes through `answer_and_relaunch`,
//! the path the shell uses, so a parked branch resumes or relaunches exactly as from the shell.
use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::{Value, json};

use super::decisions::{
    self, Decision, DecisionError, ORIGIN_CONDUCTOR, STATUS_ANSWERED, STATUS_OPEN,
};
use super::store::{self, TreeJob};
use crate::state::AppState;

type Refusal = (StatusCode, Json<Value>);

const STATUS_ALL: &str = "all";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tree-decisions", get(get_tree_decisions))
        .route(
            "/tree-decisions/{id}/answer",
            post(post_tree_decision_answer),
        )
}

fn refuse(status: StatusCode, message: impl Into<String>) -> Refusal {
    (status, Json(json!({ "error": message.into() })))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    tree_id: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnswerBody {
    answer: String,
}

/// The branch a decision belongs to: the one parked on it, else the one its node runs behind.
fn branch_of(job: &TreeJob, decision: &Decision) -> Option<String> {
    let parked = job.manifest.branches.values().find(|branch| {
        branch
            .park
            .as_ref()
            .and_then(|record| record.get("decision_id"))
            .and_then(Value::as_str)
            == Some(decision.id.as_str())
    });
    let by_node = || {
        let node = decision.node_id.as_deref()?;
        job.manifest
            .branches
            .values()
            .find(|branch| branch.node_id.as_deref() == Some(node))
    };
    parked.or_else(by_node).map(|branch| branch.name.clone())
}

async fn get_tree_decisions(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, Refusal> {
    let status = query.status.as_deref().unwrap_or(STATUS_OPEN);
    if ![STATUS_OPEN, STATUS_ANSWERED, STATUS_ALL].contains(&status) {
        return Err(refuse(
            StatusCode::BAD_REQUEST,
            format!("status must be open, answered or all, not {status:?}"),
        ));
    }
    let pool = &state.pool;
    let rows = match query.tree_id.as_deref() {
        Some(tree_id) => decisions::for_tree(pool, tree_id).await,
        None => decisions::list(pool).await,
    }
    .map_err(|e| refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")))?;

    let mut jobs: HashMap<String, Option<TreeJob>> = HashMap::new();
    let mut out = Vec::new();
    for decision in rows
        .into_iter()
        .filter(|d| status == STATUS_ALL || d.status == status)
    {
        let branch = match decision.tree_id.as_deref() {
            Some(tree_id) => {
                if !jobs.contains_key(tree_id) {
                    let job = store::load(pool, tree_id)
                        .await
                        .map_err(|e| refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}")))?;
                    jobs.insert(tree_id.to_owned(), job);
                }
                jobs[tree_id]
                    .as_ref()
                    .and_then(|job| branch_of(job, &decision))
            }
            None => None,
        };
        out.push(json!({
            "id": decision.id,
            "tree": decision.tree_id,
            "node": decision.node_id,
            "branch": branch,
            "question": decision.question,
            "status": decision.status,
            "answer": decision.answer,
            "origin": decision.origin,
        }));
    }
    Ok(Json(Value::Array(out)))
}

async fn post_tree_decision_answer(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<AnswerBody>, JsonRejection>,
) -> Result<Json<Value>, Refusal> {
    let Json(body) = body.map_err(|e| refuse(StatusCode::BAD_REQUEST, e.body_text()))?;
    let answer = body.answer.trim();
    if answer.is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, "the answer is empty"));
    }
    match crate::tree_job::open::answer_and_relaunch(&state, &id, answer, ORIGIN_CONDUCTOR).await {
        Ok(answered) => Ok(Json(json!({
            "id": answered.decision.id,
            "question": answered.decision.question,
            "origin": answered.decision.origin,
            "relaunched": answered.relaunched,
        }))),
        Err(DecisionError::NotFound(_)) => Err(refuse(
            StatusCode::NOT_FOUND,
            format!("decision {id} not found"),
        )),
        Err(DecisionError::AlreadyDecided(_)) => Err(refuse(
            StatusCode::CONFLICT,
            format!("decision {id} is already answered"),
        )),
        Err(e) => Err(refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::model::{Branch, Manifest};
    use super::*;

    const TREE: &str = "job-1";
    const NODE: &str = "node-api";
    const BRANCH: &str = "api";
    const QUESTION: &str = "pick A or B";
    const NOW: &str = "2026-11-13T00:00:00Z";

    async fn test_state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    /// The job `TREE` with one branch `api` behind the node `NODE`, in `state`.
    async fn seed_branch(state: &AppState, branch_state: &str) {
        let pool = &state.pool;
        let mut branch = Branch::new(BRANCH, &["core/src/api/**".to_owned()], "core", None);
        branch.state = branch_state.to_owned();
        branch.node_id = Some(NODE.to_owned());
        let mut manifest = Manifest::new(TREE, "feat/tree", "Ship it", "nowhere", &[]);
        manifest.branches.insert(branch.name.clone(), branch);
        let job = TreeJob {
            id: TREE.to_owned(),
            root_node_id: format!("{TREE}-root"),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: NOW.to_owned(),
            updated_at: NOW.to_owned(),
        };
        store::save(pool, &job).await.unwrap();
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id,
                                     tree_job_id, created_at, updated_at)
             VALUES (?, ?, 1, 'orchestrator', 'title', 'goal', 'running', NULL, ?, ?, ?)",
        )
        .bind(NODE)
        .bind(format!("{TREE}-root"))
        .bind(TREE)
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn answer(state: &AppState, id: &str, text: &str) -> Result<Json<Value>, Refusal> {
        post_tree_decision_answer(
            State(state.clone()),
            Path(id.to_owned()),
            Ok(Json(AnswerBody {
                answer: text.to_owned(),
            })),
        )
        .await
    }

    async fn list(state: &AppState, tree_id: Option<&str>, status: Option<&str>) -> Vec<Value> {
        let Json(value) = get_tree_decisions(
            State(state.clone()),
            Query(ListQuery {
                tree_id: tree_id.map(str::to_owned),
                status: status.map(str::to_owned),
            }),
        )
        .await
        .unwrap();
        value.as_array().unwrap().clone()
    }

    #[tokio::test]
    async fn an_answer_resumes_the_parked_branch_with_the_conductor_origin() {
        let state = test_state().await;
        seed_branch(&state, "building").await;
        let parked = decisions::park(&state.pool, TREE, NODE, QUESTION)
            .await
            .unwrap();
        let loaded = store::load(&state.pool, TREE).await.unwrap().unwrap();
        assert_eq!(loaded.manifest.branches[BRANCH].state, "parked");

        let Json(done) = answer(&state, &parked.id, "Pick A").await.unwrap();

        assert_eq!(done["origin"], ORIGIN_CONDUCTOR);
        let loaded = store::load(&state.pool, TREE).await.unwrap().unwrap();
        assert_eq!(loaded.manifest.branches[BRANCH].state, "building");
        let stored = decisions::for_tree(&state.pool, TREE).await.unwrap();
        assert_eq!(stored[0].status, STATUS_ANSWERED);
        assert_eq!(stored[0].answer.as_deref(), Some("Pick A"));
        assert_eq!(stored[0].origin.as_deref(), Some(ORIGIN_CONDUCTOR));
    }

    #[tokio::test]
    async fn a_second_answer_is_refused_and_the_first_stays() {
        let state = test_state().await;
        decisions::ask_ownerless(&state.pool, "d-1", QUESTION)
            .await
            .unwrap();
        answer(&state, "d-1", "first").await.unwrap();

        let (status, Json(body)) = answer(&state, "d-1", "second").await.unwrap_err();

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("already answered"));
        let stored = decisions::list(&state.pool).await.unwrap();
        assert_eq!(stored[0].answer.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn an_unknown_decision_is_not_found() {
        let state = test_state().await;

        let (status, Json(body)) = answer(&state, "nope", "x").await.unwrap_err();

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn a_blank_answer_is_refused() {
        let state = test_state().await;
        decisions::ask_ownerless(&state.pool, "d-1", QUESTION)
            .await
            .unwrap();

        let (status, _) = answer(&state, "d-1", "   ").await.unwrap_err();

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_list_is_open_only_by_default_and_filters_by_status_and_tree() {
        let state = test_state().await;
        seed_branch(&state, "building").await;
        let parked = decisions::park(&state.pool, TREE, NODE, QUESTION)
            .await
            .unwrap();
        decisions::ask_ownerless(&state.pool, "d-free", "free question")
            .await
            .unwrap();
        decisions::ask_ownerless(&state.pool, "d-done", "done question")
            .await
            .unwrap();
        answer(&state, "d-done", "ok").await.unwrap();

        let open = list(&state, None, None).await;
        let ids: Vec<&str> = open.iter().map(|d| d["id"].as_str().unwrap()).collect();
        assert_eq!(open.len(), 2, "{ids:?}");
        assert!(ids.contains(&parked.id.as_str()) && ids.contains(&"d-free"));
        let mine = open.iter().find(|d| d["id"] == parked.id.as_str()).unwrap();
        assert_eq!(mine["tree"], TREE);
        assert_eq!(mine["node"], NODE);
        assert_eq!(mine["branch"], BRANCH);
        assert_eq!(mine["question"], QUESTION);

        let answered = list(&state, None, Some("answered")).await;
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0]["id"], "d-done");
        assert_eq!(list(&state, None, Some("all")).await.len(), 3);

        let of_tree = list(&state, Some(TREE), None).await;
        assert_eq!(of_tree.len(), 1);
        assert_eq!(of_tree[0]["id"], parked.id.as_str());
    }

    #[tokio::test]
    async fn an_unknown_status_is_refused() {
        let state = test_state().await;

        let refused = get_tree_decisions(
            State(state),
            Query(ListQuery {
                tree_id: None,
                status: Some("bogus".to_owned()),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(refused.0, StatusCode::BAD_REQUEST);
    }
}
