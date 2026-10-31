//! The branches of the PreToolUse decision that read a node run's context fill: the hint in the
//! warn band and the forced split once the wrap limit is reached.

use super::*;
use crate::conductor::context::{Fill, allowed_for_node, handoff_path};

/// The context verdict of the node a run works for, `None` for a run without a node and when the
/// verdict cannot be read (fail open: a read error must not stop a run, and the next call asks
/// again). The limits come from the run's own `speed.yaml`; the model-window cap is not applied
/// because the hook does not know the run's model.
async fn node_verdict(
    state: &AppState,
    run_id: i64,
) -> Option<crate::conductor::context::NodeRunVerdict> {
    use crate::conductor::context::verdict_for_run;
    let verdict = match verdict_for_run(&state.pool, run_id, None, None).await {
        Ok(Some(verdict)) => verdict,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(run_id, %error, "pretooluse-decision: could not read the node's context verdict");
            return None;
        }
    };
    let cwd: Option<String> = sqlx::query_scalar("SELECT cwd FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .flatten();
    let yaml = cwd.and_then(|cwd| {
        std::fs::read_to_string(Path::new(&cwd).join(".nucleos/workflow/speed.yaml")).ok()
    });
    let Some(yaml) = yaml else {
        return Some(verdict);
    };
    match verdict_for_run(&state.pool, run_id, Some(&yaml), None).await {
        Ok(Some(with_limits)) => Some(with_limits),
        _ => Some(verdict),
    }
}

/// The sentence a tree run gets instead of a park nobody would ever answer.
pub(super) const TREE_RUN_CANNOT_WAIT_FOR_APPROVAL: &str = "this run belongs to an autonomous tree job, so it cannot wait for an approval nobody will \
     answer, and nothing is waiting on one. Write files inside your worktree with the Write tool, \
     not with shell redirects. If the action is truly needed, close the node as blocked and give \
     the reason.";

/// Whether the run works for a node of a tree job (a branch node bound to the job, or the job's
/// root). Such a run is refused rather than parked: nothing answers a tree run's park, and
/// `reconcile_running` reads it as live, so the tree would hang holding a slot. Fails closed: an
/// error or a missing row answers `false` and the run parks as before.
pub(super) async fn is_tree_run(state: &AppState, run_id: i64) -> bool {
    let answer: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM work_nodes n
         WHERE n.run_id = ?
           AND (n.tree_job_id IS NOT NULL
                OR EXISTS (SELECT 1 FROM tree_jobs t WHERE t.root_node_id = n.id))
         LIMIT 1",
    )
    .bind(run_id.to_string())
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);
    answer.is_some()
}

/// What a node run in its warn band is told, so it splits before the hard limit refuses every
/// other tool. `None` for a run without a node or below the warn band.
pub(super) async fn warn_hint(state: &AppState, run_id: i64) -> Option<String> {
    match node_verdict(state, run_id).await {
        Some(v) if v.fill == Fill::Warn => Some(
            "Your context is filling up. Finish the step in hand, then write your handoff and \
             close the node (node_close), or split the rest into child nodes, before the \
             hard limit refuses every other tool."
                .to_owned(),
        ),
        _ => None,
    }
}

/// The forced split (spec §4.2). A run behind a work node whose context reached its wrap limit
/// may do nothing but wind the node down: `allowed_for_node` names what is left (closing or
/// updating the node, writing its own handoff, and opening a child for an orchestrator). It
/// answers before the classifier, so a refusal never terminates the run or mints a proposal, and
/// the allowed calls do not depend on the run's mode. A run with no node, or a fill not yet
/// measured, is not touched: `None` means fall through to the rules that follow.
pub(super) async fn wind_down_decision(
    state: &AppState,
    payload: &PreToolUsePayload,
    run_id: i64,
) -> Option<Json<Decision>> {
    let node = node_verdict(state, run_id).await?;
    if !matches!(node.fill, Fill::Wrap | Fill::Block) {
        return None;
    }
    let own_handoff = state
        .machine_config_root
        .as_deref()
        .zip(node.project_id.as_deref())
        .and_then(|(root, project)| crate::project::state::dir(root, project))
        .map(|dir| handoff_path(&dir, &node.root_id, &node.node_id));
    Some(
        if allowed_for_node(
            &payload.tool_name,
            &payload.tool_input,
            &node.kind,
            own_handoff.as_deref(),
        ) {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "a node over its context limit may still wind itself down".to_owned(),
            })
        } else {
            Json(Decision {
                decision: "deny".to_owned(),
                reason: "this node's context is full: write your handoff and close the node \
                         (node_close), or open child nodes if you orchestrate; nothing else is \
                         allowed"
                    .to_owned(),
            })
        },
    )
}

/// Restricts a docs branch to the paths it declared when the tree was opened. This is before the
/// classifier so a refusal is immediate rather than an approval a tree run cannot answer.
pub(super) async fn docs_write_decision(
    state: &AppState,
    payload: &PreToolUsePayload,
    run_id: i64,
    cwd: Option<&Path>,
) -> Option<Json<Decision>> {
    if !crate::governance::classifier::WRITE_TOOLS.contains(&payload.tool_name.as_str()) {
        return None;
    }

    // The JSON predicate leaves ordinary and code branch runs at one indexed node lookup. Only a
    // matching docs node pays to deserialize its manifest and inspect its declared paths.
    let docs_node: Option<(String, String)> = match sqlx::query_as(
        "SELECT n.id, j.manifest
         FROM work_nodes n
         JOIN tree_jobs j ON j.id = n.tree_job_id
         WHERE n.run_id = ?
           AND EXISTS (
               SELECT 1 FROM json_each(j.manifest, '$.branches') b
               WHERE json_extract(b.value, '$.layer') = 'docs'
                 AND json_extract(b.value, '$.node_id') = n.id
           )
         LIMIT 1",
    )
    .bind(run_id.to_string())
    .fetch_optional(&state.pool)
    .await
    {
        Ok(docs_node) => docs_node,
        Err(error) => {
            tracing::warn!(run_id, %error, "pretooluse-decision: could not look up docs node");
            return Some(docs_write_deny(&[]));
        }
    };
    let Some((node_id, manifest)) = docs_node else {
        return None;
    };
    let manifest: crate::tree_job::model::Manifest = match serde_json::from_str(&manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(run_id, %error, "pretooluse-decision: could not read docs manifest");
            return Some(docs_write_deny(&[]));
        }
    };
    let docs_branch = manifest.branches.values().find(|branch| {
        branch.layer == "docs" && branch.node_id.as_deref() == Some(node_id.as_str())
    });
    let Some(docs_branch) = docs_branch else {
        return Some(docs_write_deny(&[]));
    };

    let paths: Vec<Option<String>> = ["file_path", "notebook_path"]
        .iter()
        .filter_map(|key| payload.tool_input.get(*key).and_then(Value::as_str))
        .map(|path| docs_relative_path(path, cwd))
        .collect();
    let allowed = !paths.is_empty()
        && paths.iter().all(|path| {
            path.as_deref().is_some_and(|path| {
                crate::tree_job::docs_paths::is_allowed_docs_path(path)
                    && docs_branch
                        .paths
                        .iter()
                        .any(|glob| crate::tree_job::docs_paths::owned_glob_matches(glob, path))
            })
        });
    (!allowed).then(|| docs_write_deny(&docs_branch.paths))
}

fn docs_write_deny(paths: &[String]) -> Json<Decision> {
    let declared = if paths.is_empty() {
        "the declared docs paths".to_owned()
    } else {
        paths.join(", ")
    };
    Json(Decision {
        decision: "deny".to_owned(),
        reason: format!(
            "docs branches may write only declared paths ({declared}); close the node blocked if you need another path"
        ),
    })
}

/// Returns a lexical path relative to the checkout root. The docs policy must not resolve a
/// traversal before checking it, because a spelling that leaves and re-enters the checkout is not
/// a declared docs path.
fn docs_relative_path(path: &str, cwd: Option<&Path>) -> Option<String> {
    let cwd = cwd?;
    let path = path.replace('\\', "/");
    let cwd = cwd.to_string_lossy().replace('\\', "/");
    let path = docs_clean_path(&path)?;
    let cwd = docs_clean_path(&cwd)?;
    if path.as_bytes().get(1).is_some_and(|byte| *byte == b':') || path.starts_with('/') {
        let prefix = format!("{}/", cwd.trim_end_matches('/'));
        path.strip_prefix(&prefix).map(str::to_owned)
    } else {
        Some(path)
    }
}

fn docs_clean_path(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            _ => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}
