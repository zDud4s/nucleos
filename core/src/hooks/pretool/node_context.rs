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

/// Restricts a docs branch to declared writes and shell actions that alter its checkout. This is
/// before the classifier so a refusal is immediate rather than an approval a tree run cannot answer.
pub(super) async fn docs_write_decision(
    state: &AppState,
    payload: &PreToolUsePayload,
    run_id: i64,
    cwd: Option<&Path>,
) -> Option<Json<Decision>> {
    let is_write = crate::governance::classifier::WRITE_TOOLS.contains(&payload.tool_name.as_str());
    let is_shell = matches!(payload.tool_name.as_str(), "Bash" | "PowerShell");
    if !is_write && !is_shell {
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
    let (node_id, manifest) = docs_node?;
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

    if is_shell {
        let command = payload.tool_input.get("command").and_then(Value::as_str)?;
        if docs_shell_changes_checkout(command, cwd, &payload.tool_name) {
            return Some(docs_shell_deny());
        }
        return None;
    }

    // A docs branch writes its declared files into the project's state directory, outside the
    // checkout, so an absolute path under it is mapped back to the `<state_dir>/...` spelling the
    // manifest declares.
    let state_dir = crate::agents::prefix::state_dir_for(Path::new(&manifest.project_root));
    let paths: Vec<Option<String>> = ["file_path", "notebook_path"]
        .iter()
        .filter_map(|key| payload.tool_input.get(*key).and_then(Value::as_str))
        .map(|path| docs_relative_path(path, cwd, state_dir.as_deref()))
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
    if !allowed {
        return Some(docs_write_deny(&docs_branch.paths));
    }
    // A declared state_dir file lies outside the workspace, where the node scope fence and the
    // classifier below cannot answer for a tree run, so the hook allows it itself.
    let into_state_dir = paths.iter().all(|path| {
        path.as_deref()
            .is_some_and(|path| path.starts_with(STATE_DIR_PREFIX))
    });
    into_state_dir.then(|| {
        Json(Decision {
            decision: "allow".to_owned(),
            reason: "a docs branch may write its declared state_dir files".to_owned(),
        })
    })
}

/// The spelling of a path inside the project's state directory, as a docs manifest declares it.
const STATE_DIR_TOKEN: &str = "<state_dir>";
const STATE_DIR_PREFIX: &str = "<state_dir>/";

fn docs_shell_deny() -> Json<Decision> {
    Json(Decision {
        decision: "deny".to_owned(),
        reason: "docs branches may not change the main checkout through the shell".to_owned(),
    })
}

fn docs_shell_changes_checkout(command: &str, cwd: Option<&Path>, tool_name: &str) -> bool {
    let shell = crate::governance::classifier::shell_for(tool_name);
    match shell {
        crate::command_reader::Shell::Posix => match crate::command_reader::parse(command, shell) {
            crate::command_reader::Parsed::Commands(commands) => commands
                .iter()
                .any(|command| docs_segment_changes_checkout(&command.text, cwd, shell)),
            crate::command_reader::Parsed::Unreadable(_) => true,
        },
        crate::command_reader::Shell::PowerShell => {
            match crate::command_reader::read(command, shell) {
                crate::command_reader::Reading::Sequence(segments) => segments
                    .into_iter()
                    .any(|segment| docs_segment_changes_checkout(segment, cwd, shell)),
                crate::command_reader::Reading::Unreadable(_) => true,
            }
        }
    }
}

fn docs_segment_changes_checkout(
    segment: &str,
    cwd: Option<&Path>,
    shell: crate::command_reader::Shell,
) -> bool {
    let inner = docs_heavy_wrapper_inner(segment).unwrap_or(segment);
    docs_git_changes_index_or_history(inner)
        || (!docs_workspace_navigation_or_creation(inner)
            && crate::governance::classifier::lands_inside_the_workspace(inner, cwd, shell)
            && !crate::governance::classifier::checks_formatting_without_writing(inner))
}

fn docs_workspace_navigation_or_creation(segment: &str) -> bool {
    crate::governance::classifier::split_first_token(segment).is_some_and(|(program, _)| {
        ["cd", "chdir", "set-location", "mkdir", "md"]
            .iter()
            .any(|name| program.eq_ignore_ascii_case(name))
    })
}

/// This guard only refuses. It may therefore inspect a mutable in-checkout `heavy.py` wrapper,
/// unlike the general classifier, which cannot trust it when deciding to allow a command.
fn docs_heavy_wrapper_inner(segment: &str) -> Option<&str> {
    let (program, rest) = crate::governance::classifier::split_first_token(segment)?;
    let program = std::path::Path::new(&program)
        .file_name()?
        .to_string_lossy()
        .to_ascii_lowercase();
    if !(program.starts_with("python") || program == "py") {
        return None;
    }
    let (script, rest) = crate::governance::classifier::split_first_token(rest)?;
    if !crate::governance::classifier::path_has_suffix(
        &script.replace('\\', "/"),
        "scripts/heavy.py",
    ) {
        return None;
    }
    crate::governance::classifier::heavy_wrapper_arguments(rest)
}

fn docs_git_changes_index_or_history(segment: &str) -> bool {
    let words = crate::governance::classifier::shell_words(segment);
    if !words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("git"))
    {
        return false;
    }
    let mut words = words.into_iter().skip(1);
    while let Some(word) = words.next() {
        if ["-C", "-c", "--git-dir", "--work-tree", "--namespace"]
            .iter()
            .any(|option| word.eq_ignore_ascii_case(option))
        {
            let _ = words.next();
        } else if !word.starts_with('-') {
            return word.eq_ignore_ascii_case("add") || word.eq_ignore_ascii_case("commit");
        }
    }
    false
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
            "docs branches may write only declared paths ({declared}), each by its absolute path under `<state_dir>` and with Write/Edit, not the shell; close the node blocked if you need another path"
        ),
    })
}

/// Returns a lexical path relative to the checkout root, or `<state_dir>/<rest>` for an absolute
/// path under the project's state directory. The docs policy must not resolve a traversal before
/// checking it, because a spelling that leaves and re-enters the checkout (or the state directory)
/// is not a declared docs path. The `<state_dir>` token itself is never valid input: spelled as a
/// path it would name a directory of that name inside the checkout.
fn docs_relative_path(path: &str, cwd: Option<&Path>, state_dir: Option<&Path>) -> Option<String> {
    let cwd = cwd?;
    let path = path.replace('\\', "/");
    let cwd = cwd.to_string_lossy().replace('\\', "/");
    let absolute = crate::governance::classifier::is_absolute_path(&path);
    let path = docs_clean_path(&path)?;
    let cwd = docs_clean_path(&cwd)?;
    if absolute {
        let state_dir =
            state_dir.and_then(|dir| docs_clean_path(&dir.to_string_lossy().replace('\\', "/")));
        if let Some(rest) = state_dir
            .as_deref()
            .and_then(|dir| strip_root_prefix(&path, dir, cfg!(windows)))
        {
            return Some(format!("{STATE_DIR_PREFIX}{rest}"));
        }
        let relative = strip_root_prefix(&path, &cwd, cfg!(windows))?;
        (!names_state_dir_token(relative)).then(|| relative.to_owned())
    } else {
        (!names_state_dir_token(&path)).then_some(path)
    }
}

/// Whether a cleaned checkout-relative path starts with the literal `<state_dir>` segment.
fn names_state_dir_token(relative: &str) -> bool {
    relative.split('/').next() == Some(STATE_DIR_TOKEN)
}

/// Whether `path` names a file under the retired `.ai/` directory at one of `roots` (the run's
/// checkout and the project's main checkout). Backslashes read as slashes, and the path is cleaned
/// lexically: a `..` that stays inside is resolved, one that climbs above the start is not a match.
/// Only the root `.ai` counts, never a nested `x/.ai` nor a lookalike such as `.aix`.
fn names_legacy_ai_dir(path: &str, roots: &[std::path::PathBuf]) -> bool {
    let path = path.replace('\\', "/");
    let absolute = crate::governance::classifier::is_absolute_path(&path);
    let Some(path) = resolve_clean_path(&path) else {
        return false;
    };
    let under_ai = |relative: &str| {
        let mut segments = relative.split('/');
        let first = segments.next().unwrap_or_default();
        let is_ai = if cfg!(windows) {
            first.eq_ignore_ascii_case(".ai")
        } else {
            first == ".ai"
        };
        is_ai && segments.next().is_some()
    };
    if !absolute {
        // A relative path is taken from each root alike, so the root itself does not matter.
        return !roots.is_empty() && under_ai(&path);
    }
    roots.iter().any(|root| {
        resolve_clean_path(&root.to_string_lossy().replace('\\', "/"))
            .and_then(|root| strip_root_prefix(&path, &root, cfg!(windows)).map(under_ai))
            .unwrap_or(false)
    })
}

/// A lexical clean that resolves `..` against the segments before it. `None` when a `..` climbs
/// above the first segment or nothing is left. The leading `/` is dropped, as `docs_clean_path`
/// does, so the result compares only with another path cleaned the same way.
fn resolve_clean_path(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Refuses a Write/Edit under the project's retired `.ai/` directory, pointing at `<state_dir>`.
/// `None` for any other tool and for a run not in flight (no `cwd`): fail-open, like the node
/// scope fence.
pub(super) fn legacy_ai_write_decision(
    payload: &PreToolUsePayload,
    cwd: Option<&Path>,
) -> Option<Json<Decision>> {
    let tool = payload.tool_name.as_str();
    if !crate::governance::classifier::WRITE_TOOLS.contains(&tool) && tool != "MultiEdit" {
        return None;
    }
    let cwd = cwd?;
    let project_root = crate::agents::prefix::project_root_from(cwd);
    let mut roots = vec![cwd.to_path_buf()];
    if let Some(root) = &project_root
        && root.as_path() != cwd
    {
        roots.push(root.clone());
    }
    let hits = ["file_path", "notebook_path"]
        .iter()
        .filter_map(|key| payload.tool_input.get(*key).and_then(Value::as_str))
        .any(|path| names_legacy_ai_dir(path, &roots));
    if !hits {
        return None;
    }
    let state_dir = crate::agents::prefix::state_dir_for(project_root.as_deref().unwrap_or(cwd));
    let resolved = state_dir
        .map(|dir| format!(" ({})", dir.to_string_lossy()))
        .unwrap_or_default();
    Some(Json(Decision {
        decision: "deny".to_owned(),
        reason: format!(
            "`.ai/` is the retired agent-material layout and takes no new writes. Write plans, specs, notes, spikes and handoffs under `{STATE_DIR_TOKEN}`{resolved} instead"
        ),
    }))
}

fn strip_root_prefix<'a>(path: &'a str, root: &str, case_insensitive: bool) -> Option<&'a str> {
    let prefix = format!("{}/", root.trim_end_matches('/'));
    let candidate = path.get(..prefix.len())?;
    let matches = if case_insensitive {
        candidate.eq_ignore_ascii_case(&prefix)
    } else {
        candidate == prefix
    };
    matches.then(|| &path[prefix.len()..])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_run_case_insensitive_absolute_path_preserves_declared_file_case() {
        let path = "C:/PROJ/.ai/specs/README.md";
        let relative = strip_root_prefix(path, "c:/proj", true).expect("path below root");

        assert_eq!(relative, ".ai/specs/README.md");
        assert!(crate::tree_job::docs_paths::owned_glob_matches(
            ".ai/specs/README.md",
            relative,
        ));
    }

    /// The hook looks a node up by run id on every tool call; that lookup must be an index search.
    #[tokio::test]
    async fn node_lookup_by_run_id_uses_an_index() {
        let pool = crate::testdb::fresh_pool().await;
        let plan: Vec<(i64, i64, i64, String)> =
            sqlx::query_as("EXPLAIN QUERY PLAN SELECT id FROM work_nodes WHERE run_id = ?")
                .bind("1")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(
            plan.iter().any(|row| row.3.contains("USING INDEX")),
            "no index on work_nodes.run_id: {plan:?}"
        );
    }

    #[test]
    fn docs_relative_path_maps_an_absolute_state_dir_path_to_the_token() {
        let cwd = Some(Path::new("/repo"));
        let state_dir = Some(Path::new("/state"));

        assert_eq!(
            docs_relative_path("/state/notes/x.md", cwd, state_dir).as_deref(),
            Some("<state_dir>/notes/x.md")
        );
        assert_eq!(
            docs_relative_path("\\state\\notes\\x.md", cwd, state_dir).as_deref(),
            Some("<state_dir>/notes/x.md")
        );
        // An absolute path in the checkout keeps the checkout-relative spelling.
        assert_eq!(
            docs_relative_path("/repo/docs/x.md", cwd, state_dir).as_deref(),
            Some("docs/x.md")
        );
        // Without a known state_dir an outside absolute path maps to nothing.
        assert_eq!(docs_relative_path("/state/notes/x.md", cwd, None), None);
    }

    #[test]
    fn docs_relative_path_refuses_the_token_spelled_relatively() {
        let cwd = Some(Path::new("/repo"));

        for state_dir in [Some(Path::new("/state")), None] {
            assert_eq!(
                docs_relative_path("<state_dir>/notes/x.md", cwd, state_dir),
                None
            );
            assert_eq!(
                docs_relative_path("./<state_dir>/notes/x.md", cwd, state_dir),
                None
            );
        }
    }

    #[test]
    fn docs_relative_path_refuses_state_dir_traversal() {
        let cwd = Some(Path::new("/repo"));
        let state_dir = Some(Path::new("/state"));

        assert_eq!(
            docs_relative_path("/state/notes/../memory.md", cwd, state_dir),
            None
        );
        assert_eq!(docs_relative_path("/state/../x", cwd, state_dir), None);
    }

    fn write_payload(tool: &str, path: &str) -> PreToolUsePayload {
        PreToolUsePayload {
            run_id: 1,
            tool_name: tool.to_owned(),
            tool_input: serde_json::json!({"file_path": path, "content": "x"}),
            agent_id: None,
            agent_type: None,
        }
    }

    #[test]
    fn legacy_ai_names_a_file_under_the_root_dot_ai() {
        let roots = [std::path::PathBuf::from("/repo")];

        assert!(names_legacy_ai_dir("/repo/.ai/specs/x.md", &roots));
        assert!(names_legacy_ai_dir(".ai/specs/x.md", &roots));
        assert!(names_legacy_ai_dir(".ai\\specs\\x.md", &roots));
        assert!(names_legacy_ai_dir("\\repo\\.ai\\specs\\x.md", &roots));
        assert!(names_legacy_ai_dir("src/../.ai/x", &roots));
    }

    #[test]
    fn legacy_ai_does_not_name_lookalikes_or_other_roots() {
        let roots = [std::path::PathBuf::from("/repo")];

        assert!(!names_legacy_ai_dir(".aix/x", &roots));
        assert!(!names_legacy_ai_dir(".ai-old/x", &roots));
        assert!(!names_legacy_ai_dir("docs/.ai/x", &roots));
        assert!(!names_legacy_ai_dir("/other/.ai/x", &roots));
        assert!(!names_legacy_ai_dir(".ai", &roots));
        assert!(!names_legacy_ai_dir("../.ai/x", &roots));
    }

    #[test]
    fn legacy_ai_deny_names_the_state_dir_from_project_json() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let state = dir.path().join("state-dir");
        std::fs::create_dir_all(root.join(".nucleos/workflow")).unwrap();
        std::fs::write(
            root.join(".nucleos/workflow/project.json"),
            serde_json::json!({"state_dir": state.to_str().unwrap()}).to_string(),
        )
        .unwrap();
        let target = root.join(".ai").join("notes").join("x.md");
        let payload = write_payload("Write", target.to_str().unwrap());

        let Json(decision) =
            legacy_ai_write_decision(&payload, Some(root.as_path())).expect("a deny");

        assert_eq!(decision.decision, "deny", "{}", decision.reason);
        assert!(
            decision.reason.contains("<state_dir>"),
            "got {:?}",
            decision.reason
        );
        assert!(
            decision.reason.contains(state.to_string_lossy().as_ref()),
            "the reason must carry the resolved state dir, got {:?}",
            decision.reason
        );
    }

    #[test]
    fn legacy_ai_write_decision_ignores_reads_and_a_run_that_is_not_in_flight() {
        let root = Path::new("/repo");

        let read = write_payload("Read", "/repo/.ai/x.md");
        assert!(legacy_ai_write_decision(&read, Some(root)).is_none());

        let write = write_payload("Write", "/repo/.ai/x.md");
        assert!(legacy_ai_write_decision(&write, None).is_none());
    }
}
