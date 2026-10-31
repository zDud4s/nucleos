//! The daemon's close path for a node of a tree job (R2): it validates the agent's report before
//! accepting the close, returns every problem to the agent while it still lives, writes the handoff
//! from the report, and only then closes the node. It also holds the two rules that decide whether
//! a valid report may close at all: the node checklist (R10) and the top branch node's cycle (R3).

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use axum::http::StatusCode;
use sqlx::SqlitePool;

use super::checklist::may_close;
use super::engine::branch_node_may_close;
use super::model::JOB_TERMINAL_STATE;
use super::node_run::{self, NodeRunError};
use super::report::{Report, parse_report, render_handoff, report_problems};
use super::store::{self, StoreError};
use crate::work_nodes::{self, CloseArgs, Node, NodeError, NodeLimits};

/// The longest report the daemon accepts, in non-blank lines: the default of `tree_state.py`.
pub const REPORT_MAX_LINES: usize = 60;

#[derive(Debug)]
pub enum CloseError {
    /// 422: every problem found in the report and the checklist, worded for the agent.
    Invalid(Vec<String>),
    /// The node layer's own refusal (404, 409, 400, 422, 500).
    Node(NodeError),
    /// 500: the node's tree job is gone or unreadable.
    Store(StoreError),
    /// 500.
    Db(sqlx::Error),
    /// 500: the handoff file could not be written.
    Io(std::io::Error),
}

impl CloseError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            CloseError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            CloseError::Node(NodeError::Invalid(_)) => StatusCode::BAD_REQUEST,
            CloseError::Node(NodeError::Conflict(_)) => StatusCode::CONFLICT,
            CloseError::Node(NodeError::Unprocessable(_)) => StatusCode::UNPROCESSABLE_ENTITY,
            CloseError::Node(NodeError::NotFound) => StatusCode::NOT_FOUND,
            CloseError::Node(NodeError::Db(_))
            | CloseError::Store(_)
            | CloseError::Db(_)
            | CloseError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The problems of an `Invalid`, for a 422 body; empty for every other variant.
    pub fn problems(&self) -> &[String] {
        match self {
            CloseError::Invalid(problems) => problems,
            _ => &[],
        }
    }
}

impl fmt::Display for CloseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CloseError::Invalid(problems) => write!(f, "invalid report: {}", problems.join("; ")),
            CloseError::Node(error) => write!(f, "{error}"),
            CloseError::Store(error) => write!(f, "{error}"),
            CloseError::Db(error) => write!(f, "database error: {error}"),
            CloseError::Io(error) => write!(f, "could not write the handoff: {error}"),
        }
    }
}

impl std::error::Error for CloseError {}

impl From<NodeError> for CloseError {
    fn from(error: NodeError) -> Self {
        CloseError::Node(error)
    }
}

impl From<StoreError> for CloseError {
    fn from(error: StoreError) -> Self {
        CloseError::Store(error)
    }
}

impl From<sqlx::Error> for CloseError {
    fn from(error: sqlx::Error) -> Self {
        CloseError::Db(error)
    }
}

impl From<std::io::Error> for CloseError {
    fn from(error: std::io::Error) -> Self {
        CloseError::Io(error)
    }
}

/// The tree job a node belongs to, or `None` for a plain node or an unknown id.
pub async fn tree_job_of(pool: &SqlitePool, node_id: &str) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT tree_job_id FROM work_nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(job_id,)| job_id))
}

/// The ids a contract defines, as `contract_ids` in `tree_state.py`: a heading (`#` to `######`)
/// or the first cell of a table row whose first token is an id like `A3`, skipping fenced blocks.
pub fn contract_ids(text: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            // Both marks toggle the same flag, as they do in the Python.
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(id) = heading_id(line).or_else(|| table_id(trimmed)) {
            ids.insert(id);
        }
    }
    ids
}

/// A leading letter and digits, and what follows them.
fn split_id(text: &str) -> Option<(&str, &str)> {
    let mut chars = text.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let digits = text[1..].bytes().take_while(u8::is_ascii_digit).count();
    (digits > 0).then(|| text.split_at(1 + digits))
}

fn is_mark(c: char) -> bool {
    matches!(c, '*' | '`')
}

/// `^#{1,6}\s+[*`]*ID` with no word character after the id.
fn heading_id(line: &str) -> Option<String> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start().trim_start_matches(is_mark);
    let (id, after) = split_id(rest)?;
    if after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(id.to_owned())
}

/// `^\s*\|\s*[*`]*ID[*`]*\s*\|`; the caller has trimmed the leading space.
fn table_id(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix('|')?;
    let rest = rest.trim_start().trim_start_matches(is_mark);
    let (id, after) = split_id(rest)?;
    let after = after.trim_start_matches(is_mark).trim_start();
    after.starts_with('|').then(|| id.to_owned())
}

/// Closes a node of a tree job from the agent's report (R2).
///
/// Everything is checked before anything is written: the node must be open, must belong to a tree
/// job and must not be a branch's top node whose branch is still at work; then the report (against
/// the job's contract), the node it names and the checklist (R10) are validated, and every problem
/// is returned together as `Invalid`. Only then does the daemon write the handoff from the parsed
/// report, into `<state_dir>/nodes/<root>/<node>.md` (the node's own ids, never the report's),
/// and close the node.
///
/// A top branch node whose branch is not `merged` or `dropped` (a docs branch: `done` or `dropped`),
/// and the root of a job that is not finished, do not close on a report. A valid `done`, `blocked`,
/// `failed` or `handoff` report is recorded instead: the handoff file is written, `handoff_path` is
/// set, the `report` transition is stored under the node's run id, and the node stays `running`
/// for the driver to close later. A `contract_request` or `split` report on a top branch node is
/// still refused (409); a node with no run cannot record and is refused too.
pub async fn close_node(
    pool: &SqlitePool,
    state_dir: &Path,
    node_id: &str,
    limits: &NodeLimits,
    report: &str,
    checklist: Option<&str>,
) -> Result<Node, CloseError> {
    let node = work_nodes::get(pool, node_id).await?;
    let Some(job_id) = tree_job_of(pool, node_id).await? else {
        return Err(NodeError::Conflict(format!("node {node_id} is not a tree node")).into());
    };
    if let Some(closed_at) = &node.closed_at {
        return Err(NodeError::Conflict(format!(
            "node is already closed as {} at {closed_at}",
            node.status
        ))
        .into());
    }
    let job = store::load(pool, &job_id)
        .await?
        .ok_or_else(|| StoreError::NotFound(job_id.clone()))?;

    // The top node of a branch that may not close yet, or the root of an unfinished job, does not
    // close on a report: it records it and stays running (see below).
    let held_branch = job
        .manifest
        .branches
        .values()
        .find(|branch| {
            branch.node_id.as_deref() == Some(node_id) && !branch_node_may_close(branch)
        });
    let holds = held_branch.is_some()
        || (job.root_node_id == node.id && job.state != JOB_TERMINAL_STATE);

    let parsed = parse_report(report);
    let known = job.contract.as_deref().map(contract_ids);
    let mut problems = report_problems(&parsed, REPORT_MAX_LINES, known.as_ref());
    if !parsed.node_id.is_empty() && parsed.node_id != node.id {
        problems.push(format!(
            "Node ID is '{}' but this node is {}",
            parsed.node_id, node.id
        ));
    }
    if !parsed.root_id.is_empty() && parsed.root_id != node.root_id {
        problems.push(format!(
            "Root ID is '{}' but this node's root is {}",
            parsed.root_id, node.root_id
        ));
    }
    if let Some(text) = checklist
        && let Err(reason) = may_close(text)
    {
        problems.push(reason);
    }
    if !problems.is_empty() {
        return Err(CloseError::Invalid(problems));
    }

    let records = holds
        && matches!(
            parsed.outcome.as_str(),
            "done" | "blocked" | "failed" | "handoff"
        );
    if let Some(branch) = held_branch
        && !records
    {
        return Err(NodeError::Conflict(format!(
            "node {node_id} is the top node of branch {}, which is {}; it closes only once the \
             branch is merged or dropped, and a {} report cannot be recorded meanwhile",
            branch.name, branch.state, parsed.outcome
        ))
        .into());
    }
    if records {
        return record_report(pool, state_dir, &node, &parsed).await;
    }

    let dir = state_dir.join("nodes").join(&node.root_id);
    tokio::fs::create_dir_all(&dir).await?;
    let file = dir.join(format!("{}.md", node.id));
    tokio::fs::write(&file, render_handoff(&parsed)).await?;
    let args = CloseArgs {
        status: parsed.status.clone(),
        summary: parsed.summary.clone(),
        handoff_path: file.to_string_lossy().into_owned(),
    };
    Ok(work_nodes::close(pool, &node.id, limits, &args).await?)
}

/// Records a valid report on a node that may not close yet, and leaves the node `running`.
///
/// The record is the `report` transition keyed by the node's run id, with the report's status as
/// result; the only column written is `handoff_path`. A node with no run has nothing to key the
/// record on, so it is refused (409). A second report for the same run is answered with the stored
/// row, and neither the file nor the first result changes.
async fn record_report(
    pool: &SqlitePool,
    state_dir: &Path,
    node: &Node,
    parsed: &Report,
) -> Result<Node, CloseError> {
    let Some(run_id) = node.run_id.as_deref() else {
        return Err(NodeError::Conflict(format!(
            "node {} has no run, so its report cannot be recorded",
            node.id
        ))
        .into());
    };
    let recorded: Option<String> = sqlx::query_scalar(
        "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = 'report' AND key = ?",
    )
    .bind(&node.id)
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    if recorded.is_some() {
        return Ok(work_nodes::get(pool, &node.id).await?);
    }

    let dir = state_dir.join("nodes").join(&node.root_id);
    tokio::fs::create_dir_all(&dir).await?;
    let file = dir.join(format!("{}.md", node.id));
    tokio::fs::write(&file, render_handoff(parsed)).await?;
    sqlx::query("UPDATE work_nodes SET handoff_path = ? WHERE id = ?")
        .bind(file.to_string_lossy().into_owned())
        .bind(&node.id)
        .execute(pool)
        .await?;
    node_run::apply(pool, &node.id, "report", run_id, &parsed.status)
        .await
        .map_err(report_error)?;
    Ok(work_nodes::get(pool, &node.id).await?)
}

/// `node_run::apply` only fails on the database or, never here, on the store.
fn report_error(error: NodeRunError) -> CloseError {
    match error {
        NodeRunError::Db(error) => CloseError::Db(error),
        NodeRunError::Store(error) => CloseError::Store(error),
        other => CloseError::Node(NodeError::Conflict(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    use axum::http::StatusCode;
    use sqlx::SqlitePool;

    use crate::tree_job::checklist::may_close;
    use crate::tree_job::model::{BRANCH_STATES, Branch, Manifest};
    use crate::tree_job::report::{OUTCOME_STATUS, parse_report, render_handoff, report_problems};
    use crate::tree_job::store::{self, StoreError, TreeJob};
    use crate::work_nodes::{self, Node, NodeError, NodeLimits, OpenArgs};

    const JOB: &str = "job-1";
    const STAMP: &str = "2026-10-30T10:00:00+00:00";
    const SUMMARY: &str = "Did the work.";
    /// Defines A1 and B2. Z9 appears only inside a fence, so it is not defined.
    const CONTRACT: &str =
        "# Contract\n\n## A1 The route\n\n| B2 | the type |\n\n```\n## Z9 fenced\n```\n";

    /// A tree job with a root and one leaf under it, both linked to the job.
    struct Fixture {
        pool: SqlitePool,
        dir: tempfile::TempDir,
        root: Node,
        leaf: Node,
    }

    impl Fixture {
        async fn new(contract: Option<&str>) -> Fixture {
            let pool = crate::testdb::fresh_pool().await;
            let root = open_root(&pool).await;
            let manifest = Manifest::new(JOB, "feat/tree", "Ship it", "C:/Projects/nucleos", &[]);
            store::save(
                &pool,
                &TreeJob {
                    id: JOB.to_owned(),
                    root_node_id: root.id.clone(),
                    project_id: None,
                    feature_branch: Some("feat/tree".to_owned()),
                    goal: "Ship it".to_owned(),
                    state: "open".to_owned(),
                    manifest,
                    contract: contract.map(str::to_owned),
                    created_at: STAMP.to_owned(),
                    updated_at: STAMP.to_owned(),
                },
            )
            .await
            .unwrap();
            link(&pool, &root.id, JOB).await;
            let leaf = open_child(&pool, &root.id).await;
            link(&pool, &leaf.id, JOB).await;
            Fixture {
                pool,
                dir: tempfile::tempdir().unwrap(),
                root,
                leaf,
            }
        }

        /// A new node of the job, under `parent`.
        async fn child_of(&self, parent: &str) -> Node {
            let child = open_child(&self.pool, parent).await;
            link(&self.pool, &child.id, JOB).await;
            child
        }

        async fn send(
            &self,
            node: &Node,
            report: &str,
            checklist: Option<&str>,
        ) -> Result<Node, CloseError> {
            close_node(
                &self.pool,
                self.dir.path(),
                &node.id,
                &NodeLimits::default(),
                report,
                checklist,
            )
            .await
        }

        fn handoff(&self, node: &Node) -> PathBuf {
            handoff_at(self.dir.path(), node)
        }

        /// Asserts the close left no trace: the row is the open node and no handoff exists.
        async fn assert_untouched(&self, node: &Node) {
            let row = work_nodes::get(&self.pool, &node.id).await.unwrap();
            assert_eq!(row.status, "running");
            assert_eq!(row.summary, None);
            assert_eq!(row.handoff_path, None);
            assert_eq!(row.closed_at, None);
            let file = self.handoff(node);
            assert!(!file.exists(), "no handoff expected at {}", file.display());
        }

        /// Asserts the close happened and the file on disk is the handoff of `report`.
        async fn assert_closed(&self, node: &Node, returned: &Node, report: &str) {
            let parsed = parse_report(report);
            let row = work_nodes::get(&self.pool, &node.id).await.unwrap();
            assert_eq!(&row, returned, "the returned node is the stored row");
            assert_eq!(row.status, parsed.status);
            assert_eq!(row.summary.as_deref(), Some(parsed.summary.as_str()));
            assert!(row.closed_at.is_some());
            let file = self.handoff(node);
            let stored = row.handoff_path.expect("a closed node names its handoff");
            assert_eq!(Path::new(&stored), file);
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                render_handoff(&parsed)
            );
        }

        /// Asserts the report was recorded and not closed: the node is still `running` with only its
        /// handoff path set, the file is the handoff of `report`, and the one transition row is
        /// `report` / `run_id` / `result`.
        async fn assert_recorded(
            &self,
            node: &Node,
            returned: &Node,
            report: &str,
            run_id: &str,
            result: &str,
        ) {
            let row = work_nodes::get(&self.pool, &node.id).await.unwrap();
            assert_eq!(&row, returned, "the returned node is the stored row");
            assert_eq!(row.status, "running");
            assert_eq!(row.summary, None);
            assert_eq!(row.closed_at, None);
            let file = self.handoff(node);
            assert_eq!(
                row.handoff_path.as_deref().map(Path::new),
                Some(file.as_path())
            );
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                render_handoff(&parse_report(report))
            );
            assert_eq!(
                transitions(&self.pool, &node.id).await,
                [("report".to_owned(), run_id.to_owned(), result.to_owned())]
            );
        }

        /// Replaces the job's branches, through the store as the engine would.
        async fn set_branches(&self, branches: Vec<Branch>) {
            let mut job = store::load(&self.pool, JOB).await.unwrap().unwrap();
            job.manifest.branches = branches.into_iter().map(|b| (b.name.clone(), b)).collect();
            store::update(&self.pool, &job).await.unwrap();
        }
    }

    fn handoff_at(state_dir: &Path, node: &Node) -> PathBuf {
        state_dir
            .join("nodes")
            .join(&node.root_id)
            .join(format!("{}.md", node.id))
    }

    async fn open_root(pool: &SqlitePool) -> Node {
        let args = OpenArgs {
            kind: "root".into(),
            title: "Root".into(),
            goal: "Ship it".into(),
            ..OpenArgs::default()
        };
        let opened = work_nodes::open(pool, None, &NodeLimits::default(), &args)
            .await
            .unwrap();
        work_nodes::get(pool, &opened.id).await.unwrap()
    }

    async fn open_child(pool: &SqlitePool, parent: &str) -> Node {
        let args = OpenArgs {
            parent_id: Some(parent.to_owned()),
            kind: "leaf".into(),
            title: "Leaf".into(),
            goal: "Do a part".into(),
            ..OpenArgs::default()
        };
        let opened = work_nodes::open(pool, None, &NodeLimits::default(), &args)
            .await
            .unwrap();
        work_nodes::get(pool, &opened.id).await.unwrap()
    }

    /// `Node` has no `tree_job_id` field, so the link is written and read on the table.
    async fn link(pool: &SqlitePool, node_id: &str, job_id: &str) {
        sqlx::query("UPDATE work_nodes SET tree_job_id = ? WHERE id = ?")
            .bind(job_id)
            .bind(node_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Gives the node the run a report is recorded under; the fixture's nodes have none.
    async fn set_run(pool: &SqlitePool, node_id: &str, run_id: &str) {
        sqlx::query("UPDATE work_nodes SET run_id = ? WHERE id = ?")
            .bind(run_id)
            .bind(node_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Every keyed transition of the node, as (transition, key, result), ordered by key.
    async fn transitions(pool: &SqlitePool, node_id: &str) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT transition, key, result FROM tree_transitions WHERE node_id = ? ORDER BY key",
        )
        .bind(node_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// A report that `report_problems` accepts for any outcome, given a well-formed `contract_used`.
    fn report_for(
        node_id: &str,
        root_id: &str,
        outcome: &str,
        contract_used: &str,
        summary: &str,
    ) -> String {
        let status = OUTCOME_STATUS
            .iter()
            .find(|(name, _)| *name == outcome)
            .map_or("done", |&(_, status)| status);
        let (role, files) = if outcome == "split" {
            ("orchestrator", "none")
        } else {
            ("leaf", "- core/src/tree_job/close.rs")
        };
        let request = if outcome == "contract_request" {
            "Need A9."
        } else {
            "none"
        };
        let reason = if matches!(outcome, "blocked" | "failed") {
            "Stuck on the gate."
        } else {
            "none"
        };
        format!(
            "# Node Handoff\n\nNode ID: {node_id}\nRoot ID: {root_id}\nStatus: {status}\n\
             Role: {role}\nOutcome: {outcome}\nContract used: {contract_used}\n\n\
             ## Summary\n{summary}\n\n## What was done\n- did it\n\n## Decisions\nnone\n\n\
             ## Files touched\n{files}\n\n## Commands for the root\nnone\n\n\
             ## Open items / next step\nnone\n\n## Contract request\n{request}\n\n\
             ## Blocked reason\n{reason}\n"
        )
    }

    fn done_report(node: &Node) -> String {
        report_for(&node.id, &node.root_id, "done", "none", SUMMARY)
    }

    fn outcome_report(node: &Node, outcome: &str) -> String {
        report_for(&node.id, &node.root_id, outcome, "none", SUMMARY)
    }

    fn with_contract_used(node: &Node, used: &str) -> String {
        report_for(&node.id, &node.root_id, "done", used, SUMMARY)
    }

    fn with_extra_lines(report: &str, extra: usize) -> String {
        let section = "## What was done\n";
        report.replacen(section, &format!("{section}{}", "- more\n".repeat(extra)), 1)
    }

    fn branch(name: &str, layer: &str, state: &str, node_id: &str) -> Branch {
        let mut branch = Branch::new(name, &[], layer, None);
        branch.state = state.to_owned();
        branch.node_id = Some(node_id.to_owned());
        branch
    }

    fn ids(expected: &[&str]) -> HashSet<String> {
        expected.iter().map(|id| (*id).to_owned()).collect()
    }

    /// The problems of an `Invalid`, after checking its status; anything else fails the test.
    fn invalid(result: Result<Node, CloseError>) -> Vec<String> {
        let error = result.unwrap_err();
        assert_eq!(error.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        match error {
            CloseError::Invalid(problems) => problems,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    /// The message of a 409; anything else fails the test.
    fn conflict(result: Result<Node, CloseError>) -> String {
        let error = result.unwrap_err();
        assert_eq!(error.status_code(), StatusCode::CONFLICT);
        match error {
            CloseError::Node(NodeError::Conflict(message)) => message,
            other => panic!("expected Node(Conflict), got {other:?}"),
        }
    }

    // --- Behaviour 1: a valid report closes the node ------------------------------------------

    #[tokio::test]
    async fn a_valid_report_closes_the_node_and_writes_the_rendered_handoff() {
        let fx = Fixture::new(None).await;
        let report = done_report(&fx.leaf);

        let closed = fx.send(&fx.leaf, &report, None).await.unwrap();

        assert_eq!(closed.status, "done");
        assert_eq!(closed.summary.as_deref(), Some(SUMMARY));
        fx.assert_closed(&fx.leaf, &closed, &report).await;
    }

    #[tokio::test]
    async fn every_outcome_closes_the_node_with_the_status_the_report_carries() {
        let fx = Fixture::new(None).await;

        for (outcome, status) in OUTCOME_STATUS {
            let node = fx.child_of(&fx.root.id).await;
            let report = outcome_report(&node, outcome);

            let closed = fx.send(&node, &report, None).await.unwrap();

            assert_eq!(closed.status, status, "outcome {outcome}");
            fx.assert_closed(&node, &closed, &report).await;
        }
    }

    #[tokio::test]
    async fn the_handoff_is_the_rendered_report_and_not_the_text_the_agent_sent() {
        let fx = Fixture::new(None).await;
        let report = format!(
            "<!-- a scratch note -->\n{}",
            done_report(&fx.leaf).replace("Status: done", "Status: DONE")
        );

        fx.send(&fx.leaf, &report, None).await.unwrap();

        let written = std::fs::read_to_string(fx.handoff(&fx.leaf)).unwrap();
        assert_eq!(written, render_handoff(&parse_report(&report)));
        assert!(!written.contains("scratch"), "got {written:?}");
        assert!(written.contains("Status: done\n"), "got {written:?}");
    }

    // --- Behaviour 2: an invalid report, then the same agent's correction ---------------------

    #[tokio::test]
    async fn an_invalid_report_returns_all_its_problems_and_the_corrected_report_closes() {
        let fx = Fixture::new(None).await;
        let broken = report_for(&fx.leaf.id, &fx.leaf.root_id, "shipped", "a1", "");
        let expected = report_problems(&parse_report(&broken), REPORT_MAX_LINES, None);
        assert_eq!(expected.len(), 3, "the sample has three defects: {expected:?}");

        let problems = invalid(fx.send(&fx.leaf, &broken, None).await);

        assert_eq!(problems, expected);
        fx.assert_untouched(&fx.leaf).await;

        let corrected = done_report(&fx.leaf);
        let closed = fx.send(&fx.leaf, &corrected, None).await.unwrap();
        fx.assert_closed(&fx.leaf, &closed, &corrected).await;
    }

    #[tokio::test]
    async fn problems_of_every_kind_are_returned_together() {
        let fx = Fixture::new(Some(CONTRACT)).await;
        let checklist = "- [ ] write it\n";
        let report = report_for(&fx.leaf.id, "other-root", "done", "Z9", "");

        let problems = invalid(fx.send(&fx.leaf, &report, Some(checklist)).await);

        assert_eq!(problems.len(), 4, "got {problems:?}");
        for expected in [
            "summary is empty".to_owned(),
            "Contract used Z9 is not in contract.md".to_owned(),
            may_close(checklist).unwrap_err(),
        ] {
            assert!(problems.contains(&expected), "{expected:?} in {problems:?}");
        }
        assert!(
            problems.iter().any(|p| p.contains("other-root")),
            "got {problems:?}"
        );
        fx.assert_untouched(&fx.leaf).await;
    }

    #[test]
    fn the_report_line_cap_is_the_default_of_tree_state() {
        assert_eq!(REPORT_MAX_LINES, 60);
    }

    #[tokio::test]
    async fn the_line_cap_refuses_one_line_over_and_accepts_the_limit_itself() {
        let fx = Fixture::new(None).await;
        let base = done_report(&fx.leaf);
        let room = REPORT_MAX_LINES - parse_report(&base).line_count;

        let over = with_extra_lines(&base, room + 1);
        let problems = invalid(fx.send(&fx.leaf, &over, None).await);
        assert_eq!(
            problems,
            ["report has 61 lines; the limit is 60 (HTML comments not counted)"]
        );
        fx.assert_untouched(&fx.leaf).await;

        let at_limit = with_extra_lines(&base, room);
        let closed = fx.send(&fx.leaf, &at_limit, None).await.unwrap();
        fx.assert_closed(&fx.leaf, &closed, &at_limit).await;
    }

    /// `limits` reaches `work_nodes::close`. Only the 422 is pinned: GREEN may refuse this as
    /// `Invalid` or as `Node(Unprocessable)`, and may or may not leave a file the row never names.
    #[tokio::test]
    async fn a_summary_over_the_node_limit_refuses_and_a_shorter_one_closes() {
        let fx = Fixture::new(None).await;
        let limits = NodeLimits {
            summary_max: 5,
            ..NodeLimits::default()
        };
        let state_dir = fx.dir.path();

        let error = close_node(
            &fx.pool,
            state_dir,
            &fx.leaf.id,
            &limits,
            &done_report(&fx.leaf),
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(error.status_code(), StatusCode::UNPROCESSABLE_ENTITY);
        let row = work_nodes::get(&fx.pool, &fx.leaf.id).await.unwrap();
        assert_eq!(row.status, "running");
        assert_eq!(row.closed_at, None);

        let short = report_for(&fx.leaf.id, &fx.leaf.root_id, "done", "none", "Done.");
        let closed = close_node(&fx.pool, state_dir, &fx.leaf.id, &limits, &short, None)
            .await
            .unwrap();
        fx.assert_closed(&fx.leaf, &closed, &short).await;
    }

    // --- Behaviour 3: the report must be this node's ------------------------------------------

    #[tokio::test]
    async fn a_report_naming_another_node_is_invalid_and_closes_neither_node() {
        let fx = Fixture::new(None).await;
        let sibling = fx.child_of(&fx.root.id).await;
        let report = done_report(&sibling);

        let problems = invalid(fx.send(&fx.leaf, &report, None).await);

        assert_eq!(problems.len(), 1, "got {problems:?}");
        assert!(problems[0].contains("Node ID"), "got {problems:?}");
        assert!(problems[0].contains(&sibling.id), "got {problems:?}");
        assert!(problems[0].contains(&fx.leaf.id), "got {problems:?}");
        fx.assert_untouched(&fx.leaf).await;
        fx.assert_untouched(&sibling).await;
    }

    #[tokio::test]
    async fn a_report_naming_another_root_is_invalid_and_closes_nothing() {
        let fx = Fixture::new(None).await;
        let report = report_for(&fx.leaf.id, "other-root", "done", "none", SUMMARY);

        let problems = invalid(fx.send(&fx.leaf, &report, None).await);

        assert_eq!(problems.len(), 1, "got {problems:?}");
        assert!(problems[0].contains("Root ID"), "got {problems:?}");
        assert!(problems[0].contains("other-root"), "got {problems:?}");
        assert!(problems[0].contains(&fx.leaf.root_id), "got {problems:?}");
        fx.assert_untouched(&fx.leaf).await;
        assert!(
            !fx.dir.path().join("nodes").join("other-root").exists(),
            "the id in the report must not choose where a file goes"
        );
    }

    // --- Behaviour 4: the contract ------------------------------------------------------------

    #[tokio::test]
    async fn with_a_contract_an_id_it_does_not_define_is_a_problem() {
        let fx = Fixture::new(Some(CONTRACT)).await;

        // Z9 sits inside a fence in the contract, so it is not defined there.
        let unknown = with_contract_used(&fx.leaf, "A1, Z9");
        let problems = invalid(fx.send(&fx.leaf, &unknown, None).await);
        assert_eq!(problems, ["Contract used Z9 is not in contract.md"]);
        fx.assert_untouched(&fx.leaf).await;

        let known = with_contract_used(&fx.leaf, "A1, B2");
        let closed = fx.send(&fx.leaf, &known, None).await.unwrap();
        fx.assert_closed(&fx.leaf, &closed, &known).await;
    }

    #[tokio::test]
    async fn with_a_contract_a_report_that_used_none_of_it_closes() {
        let fx = Fixture::new(Some(CONTRACT)).await;
        let report = with_contract_used(&fx.leaf, "none");

        let closed = fx.send(&fx.leaf, &report, None).await.unwrap();

        fx.assert_closed(&fx.leaf, &closed, &report).await;
    }

    #[tokio::test]
    async fn without_a_contract_only_the_shape_of_an_id_is_checked() {
        let fx = Fixture::new(None).await;

        let malformed = with_contract_used(&fx.leaf, "a1");
        let problems = invalid(fx.send(&fx.leaf, &malformed, None).await);
        assert_eq!(problems, ["Contract used 'a1' is not an id like A3 or E2"]);
        fx.assert_untouched(&fx.leaf).await;

        let any_id = with_contract_used(&fx.leaf, "Z9");
        let closed = fx.send(&fx.leaf, &any_id, None).await.unwrap();
        fx.assert_closed(&fx.leaf, &closed, &any_id).await;
    }

    // --- Behaviour 5: the checklist (R10) -----------------------------------------------------

    #[tokio::test]
    async fn a_checklist_that_may_not_close_is_invalid_with_the_reason_of_may_close() {
        let fx = Fixture::new(None).await;

        for checklist in ["- [x] one\n- [ ] still open\n", "no items at all\n"] {
            let problems = invalid(
                fx.send(&fx.leaf, &done_report(&fx.leaf), Some(checklist))
                    .await,
            );

            assert_eq!(problems, [may_close(checklist).unwrap_err()]);
            fx.assert_untouched(&fx.leaf).await;
        }
    }

    #[tokio::test]
    async fn a_checklist_closes_with_all_items_done_or_with_one_blocked_item() {
        for checklist in [
            "- [x] one\n* [X] two\n",
            "- [!] needs the owner\n",
            "- [ ] still open\n- [!] needs the owner\n",
        ] {
            let fx = Fixture::new(None).await;
            let report = done_report(&fx.leaf);

            let closed = fx.send(&fx.leaf, &report, Some(checklist)).await.unwrap();

            fx.assert_closed(&fx.leaf, &closed, &report).await;
        }
    }

    #[tokio::test]
    async fn no_checklist_skips_the_check() {
        let fx = Fixture::new(None).await;
        let report = done_report(&fx.leaf);

        let closed = fx.send(&fx.leaf, &report, None).await.unwrap();

        fx.assert_closed(&fx.leaf, &closed, &report).await;
    }

    // --- Behaviour 6: the top branch node (R3) ------------------------------------------------

    /// R3 still holds, but a top node that has finished its work must be able to say so: a
    /// `done`, `blocked`, `failed` or `handoff` report is recorded as the node's result for its run
    /// and the node stays `running`, so the driver can see the work is finished while the branch
    /// is still ahead of its merge. The node closes later, when the branch is merged or dropped.
    #[tokio::test]
    async fn a_branch_top_node_reporting_done_is_recorded_and_stays_running() {
        for outcome in ["done", "blocked", "failed"] {
            let fx = Fixture::new(None).await;
            let top = &fx.leaf;
            fx.set_branches(vec![branch("api", "backend", "building", &top.id)])
                .await;
            set_run(&fx.pool, &top.id, "7").await;
            let report = outcome_report(top, outcome);

            let recorded = fx.send(top, &report, None).await.unwrap();

            assert_eq!(recorded.status, "running", "{outcome}");
            fx.assert_recorded(top, &recorded, &report, "7", outcome).await;
        }
    }

    #[tokio::test]
    async fn a_branch_top_node_reporting_handoff_writes_the_handoff_file_and_records_the_run() {
        let fx = Fixture::new(None).await;
        let top = &fx.leaf;
        fx.set_branches(vec![branch("api", "backend", "building", &top.id)])
            .await;
        set_run(&fx.pool, &top.id, "12").await;
        let report = outcome_report(top, "handoff");
        assert!(report.contains("Status: handoff\n"), "got {report:?}");

        let recorded = fx.send(top, &report, None).await.unwrap();

        fx.assert_recorded(top, &recorded, &report, "12", "handoff").await;
        let written = std::fs::read_to_string(fx.handoff(top)).unwrap();
        assert!(written.contains("Status: handoff\n"), "got {written:?}");
    }

    /// The same run reporting twice (a retried call) changes nothing: the first result stands and
    /// the second call is answered, not refused.
    #[tokio::test]
    async fn a_second_report_for_the_same_run_is_a_replay() {
        let fx = Fixture::new(None).await;
        let top = &fx.leaf;
        fx.set_branches(vec![branch("api", "backend", "building", &top.id)])
            .await;
        set_run(&fx.pool, &top.id, "7").await;
        let first = done_report(top);
        let recorded = fx.send(top, &first, None).await.unwrap();
        fx.assert_recorded(top, &recorded, &first, "7", "done").await;

        // Valid on its own and different from the first, so a second record would show.
        let second = outcome_report(top, "blocked");
        fx.send(top, &second, None).await.unwrap();

        assert_eq!(
            transitions(&fx.pool, &top.id).await,
            [("report".to_owned(), "7".to_owned(), "done".to_owned())]
        );
        let row = work_nodes::get(&fx.pool, &top.id).await.unwrap();
        assert_eq!(row.status, "running");
        assert_eq!(row.closed_at, None);
    }

    /// Recording is not a way round the checks: a broken report is returned to the agent as before,
    /// and a report that is not about the node's own work (a contract request, a split) is still
    /// a 409 while the branch is at work. Neither leaves a trace.
    #[tokio::test]
    async fn a_branch_top_node_still_refuses_an_invalid_report() {
        let fx = Fixture::new(None).await;
        let top = &fx.leaf;
        fx.set_branches(vec![branch("api", "backend", "building", &top.id)])
            .await;
        set_run(&fx.pool, &top.id, "7").await;
        let broken = report_for(&top.id, &top.root_id, "shipped", "a1", "");
        let expected = report_problems(&parse_report(&broken), REPORT_MAX_LINES, None);
        assert_eq!(expected.len(), 3, "the sample has three defects: {expected:?}");

        let problems = invalid(fx.send(top, &broken, None).await);

        assert_eq!(problems, expected);
        fx.assert_untouched(top).await;
        assert!(transitions(&fx.pool, &top.id).await.is_empty());

        for outcome in ["contract_request", "split"] {
            let message = conflict(fx.send(top, &outcome_report(top, outcome), None).await);

            assert!(message.contains("building"), "{outcome}: {message:?}");
            fx.assert_untouched(top).await;
            assert!(transitions(&fx.pool, &top.id).await.is_empty(), "{outcome}");
        }
    }

    #[tokio::test]
    async fn a_top_branch_node_records_its_report_in_every_state_short_of_merged_or_dropped() {
        for state in BRANCH_STATES {
            if matches!(state, "merged" | "dropped") {
                continue;
            }
            let fx = Fixture::new(None).await;
            let top = &fx.leaf;
            fx.set_branches(vec![branch("api", "backend", state, &top.id)])
                .await;
            set_run(&fx.pool, &top.id, "7").await;
            let report = done_report(top);

            let recorded = fx.send(top, &report, None).await.unwrap();

            fx.assert_recorded(top, &recorded, &report, "7", "done").await;
        }
    }

    #[tokio::test]
    async fn a_top_branch_node_closes_once_its_branch_is_merged_or_dropped() {
        for state in ["merged", "dropped"] {
            let fx = Fixture::new(None).await;
            let top = &fx.leaf;
            let report = done_report(top);
            set_run(&fx.pool, &top.id, "7").await;

            fx.set_branches(vec![branch("api", "backend", "building", &top.id)])
                .await;
            let recorded = fx.send(top, &report, None).await.unwrap();
            fx.assert_recorded(top, &recorded, &report, "7", "done").await;

            fx.set_branches(vec![branch("api", "backend", state, &top.id)])
                .await;
            let closed = fx.send(top, &report, None).await.unwrap();

            fx.assert_closed(top, &closed, &report).await;
        }
    }

    #[tokio::test]
    async fn a_docs_branch_top_node_closes_on_done_or_dropped_and_records_before() {
        for state in ["done", "dropped"] {
            let fx = Fixture::new(None).await;
            let top = &fx.leaf;
            let report = done_report(top);
            set_run(&fx.pool, &top.id, "7").await;

            fx.set_branches(vec![branch("spec", "docs", "building", &top.id)])
                .await;
            let recorded = fx.send(top, &report, None).await.unwrap();
            fx.assert_recorded(top, &recorded, &report, "7", "done").await;

            fx.set_branches(vec![branch("spec", "docs", state, &top.id)])
                .await;
            let closed = fx.send(top, &report, None).await.unwrap();

            fx.assert_closed(top, &closed, &report).await;
        }
    }

    /// Only the top node of a branch at work and the root of an unfinished job record instead of
    /// closing; a node under the top closes normally. The root is the last to finish, so its
    /// report waits for the job as the top node's waits for the branch.
    #[tokio::test]
    async fn other_nodes_close_normally_while_the_branch_is_building_and_the_root_records() {
        let fx = Fixture::new(None).await;
        let top = &fx.leaf;
        let under_top = fx.child_of(&top.id).await;
        fx.set_branches(vec![branch("api", "backend", "building", &top.id)])
            .await;
        set_run(&fx.pool, &top.id, "7").await;
        set_run(&fx.pool, &fx.root.id, "3").await;

        let report = done_report(&under_top);
        let closed = fx.send(&under_top, &report, None).await.unwrap();
        fx.assert_closed(&under_top, &closed, &report).await;

        let root_report = done_report(&fx.root);
        let recorded = fx.send(&fx.root, &root_report, None).await.unwrap();
        fx.assert_recorded(&fx.root, &recorded, &root_report, "3", "done").await;

        // Neither changed anything for the top node itself, which records its own.
        let top_report = done_report(top);
        let recorded = fx.send(top, &top_report, None).await.unwrap();
        fx.assert_recorded(top, &recorded, &top_report, "7", "done").await;
    }

    // --- Behaviour 7: tree nodes and plain nodes ----------------------------------------------

    #[tokio::test]
    async fn tree_job_of_names_the_job_of_a_tree_node_and_nothing_else() {
        let fx = Fixture::new(None).await;
        let plain = open_root(&fx.pool).await;

        assert_eq!(
            tree_job_of(&fx.pool, &fx.leaf.id).await.unwrap().as_deref(),
            Some(JOB)
        );
        assert_eq!(
            tree_job_of(&fx.pool, &fx.root.id).await.unwrap().as_deref(),
            Some(JOB)
        );
        assert_eq!(tree_job_of(&fx.pool, &plain.id).await.unwrap(), None);
        assert_eq!(tree_job_of(&fx.pool, "no-such-node").await.unwrap(), None);
    }

    #[tokio::test]
    async fn close_node_refuses_a_plain_node_and_leaves_it_running() {
        let fx = Fixture::new(None).await;
        let plain = open_root(&fx.pool).await;

        conflict(fx.send(&plain, &done_report(&plain), None).await);

        fx.assert_untouched(&plain).await;
    }

    /// Not a listed behaviour: this is the one path that produces `Store`. A link to a job that is
    /// gone is a broken tree, so it must neither close the node nor pass as a plain node.
    #[tokio::test]
    async fn a_node_linked_to_a_missing_job_is_a_store_error_and_stays_running() {
        let fx = Fixture::new(None).await;
        let orphan = fx.child_of(&fx.root.id).await;
        link(&fx.pool, &orphan.id, "gone").await;

        let error = fx
            .send(&orphan, &done_report(&orphan), None)
            .await
            .unwrap_err();

        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        match error {
            CloseError::Store(StoreError::NotFound(id)) => assert_eq!(id, "gone"),
            other => panic!("expected Store(NotFound), got {other:?}"),
        }
        fx.assert_untouched(&orphan).await;
    }

    // --- Behaviour 8: an unknown node ---------------------------------------------------------

    #[tokio::test]
    async fn an_unknown_node_is_not_found_and_writes_nothing() {
        let fx = Fixture::new(None).await;
        let report = report_for("no-such-node", &fx.root.id, "done", "none", SUMMARY);

        let result = close_node(
            &fx.pool,
            fx.dir.path(),
            "no-such-node",
            &NodeLimits::default(),
            &report,
            None,
        )
        .await;

        let error = result.unwrap_err();
        assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
        assert!(
            matches!(error, CloseError::Node(NodeError::NotFound)),
            "got {error:?}"
        );
        let file = fx.dir.path().join("nodes").join(&fx.root.id).join("no-such-node.md");
        assert!(!file.exists());
    }

    // --- Behaviour 9: an already closed node --------------------------------------------------

    #[tokio::test]
    async fn closing_twice_is_refused_and_leaves_the_first_handoff_and_row_as_they_were() {
        let fx = Fixture::new(None).await;
        let first = done_report(&fx.leaf);
        let closed = fx.send(&fx.leaf, &first, None).await.unwrap();
        let file = fx.handoff(&fx.leaf);
        let bytes = std::fs::read(&file).unwrap();

        // Valid on its own and different from the first, so a write would show.
        let second = report_for(&fx.leaf.id, &fx.leaf.root_id, "done", "none", "A second story.");
        let message = conflict(fx.send(&fx.leaf, &second, None).await);

        assert!(message.contains("already closed"), "got {message:?}");
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        assert_eq!(work_nodes::get(&fx.pool, &fx.leaf.id).await.unwrap(), closed);
    }

    // --- Behaviour 10: contract_ids, against `contract_ids` in tree_state.py ------------------

    #[test]
    fn contract_ids_reads_a_heading_whose_first_token_is_an_id() {
        for (line, id) in [
            ("# A1", "A1"),
            ("###### B22 title", "B22"),
            ("##\tC3", "C3"),
            ("## **D4**", "D4"),
            ("## `E5`: the route", "E5"),
            ("## *`F6`*", "F6"),
            ("## G7-foo", "G7"),
            ("## H8.", "H8"),
            // Only the first token counts.
            ("## I9 J1", "I9"),
            // The lookahead sees the first `*`, not the `f`, once the regex backtracks.
            ("## **K2**foo", "K2"),
        ] {
            assert_eq!(contract_ids(line), ids(&[id]), "line {line:?}");
        }
    }

    #[test]
    fn contract_ids_ignores_a_heading_that_does_not_start_with_an_id() {
        for line in [
            "## A. Foo",
            "## A3x",
            "## A3_x",
            "## A3é",
            "## AB1",
            "## a1",
            "## A",
            "## 1A",
            "####### A1",
            "##A1",
            "  ## A1",
            "text ## A1",
        ] {
            assert_eq!(contract_ids(line), ids(&[]), "line {line:?}");
        }
    }

    #[test]
    fn contract_ids_reads_the_first_cell_of_a_table_row() {
        for (line, id) in [
            ("| A1 | x |", "A1"),
            ("  | **B2** | y |", "B2"),
            ("|C3|", "C3"),
            ("| `D4` |", "D4"),
            ("\t| E5 |", "E5"),
        ] {
            assert_eq!(contract_ids(line), ids(&[id]), "line {line:?}");
        }
    }

    #[test]
    fn contract_ids_ignores_a_table_row_whose_first_cell_is_not_just_an_id() {
        for line in [
            "| A1 x | y |",
            "| x | A1 |",
            "| a1 |",
            "| A | y |",
            "| A1",
            "| ## A1 |",
        ] {
            assert_eq!(contract_ids(line), ids(&[]), "line {line:?}");
        }
    }

    #[test]
    fn contract_ids_skips_fenced_blocks() {
        for text in [
            "```\n## A1\n```\n## B2",
            "~~~\n## A1\n~~~\n## B2",
            "  ```rust\n## A1\n  ```\n## B2",
            // The two fence marks toggle the same flag, as they do in the Python.
            "```\n## A1\n~~~\n## B2",
        ] {
            assert_eq!(contract_ids(text), ids(&["B2"]), "text {text:?}");
        }
        // A fence never closed hides the rest of the text.
        assert_eq!(contract_ids("```\n## A1\n## B2"), ids(&[]));
    }

    #[test]
    fn contract_ids_collects_a_whole_text_once_per_id() {
        assert_eq!(contract_ids(""), ids(&[]));
        assert_eq!(contract_ids("## A1\n## A1\n| A1 |"), ids(&["A1"]));
        assert_eq!(contract_ids("## A1\r\n## B2\r\n"), ids(&["A1", "B2"]));
        assert_eq!(contract_ids(CONTRACT), ids(&["A1", "B2"]));
    }

    // --- Behaviour 11: the handoff cannot be written ------------------------------------------

    #[tokio::test]
    async fn a_failed_handoff_write_is_an_io_error_and_the_node_stays_running() {
        let fx = Fixture::new(None).await;
        let report = done_report(&fx.leaf);
        // A file where the state directory should be, so nothing can be created under it.
        let not_a_dir = tempfile::NamedTempFile::new().unwrap();

        let error = close_node(
            &fx.pool,
            not_a_dir.path(),
            &fx.leaf.id,
            &NodeLimits::default(),
            &report,
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(matches!(error, CloseError::Io(_)), "got {error:?}");
        fx.assert_untouched(&fx.leaf).await;

        // Nothing was lost: the same report closes once the directory is usable.
        let closed = fx.send(&fx.leaf, &report, None).await.unwrap();
        fx.assert_closed(&fx.leaf, &closed, &report).await;
    }

    // --- status_code --------------------------------------------------------------------------

    #[test]
    fn status_code_maps_each_variant() {
        let cases = [
            (
                CloseError::Invalid(vec!["a problem".to_owned()]),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (CloseError::Node(NodeError::NotFound), StatusCode::NOT_FOUND),
            (
                CloseError::Node(NodeError::Conflict("no".to_owned())),
                StatusCode::CONFLICT,
            ),
            (
                CloseError::Node(NodeError::Invalid("no".to_owned())),
                StatusCode::BAD_REQUEST,
            ),
            (
                CloseError::Store(StoreError::NotFound(JOB.to_owned())),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                CloseError::Db(sqlx::Error::RowNotFound),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
            (
                CloseError::Io(std::io::Error::other("disk")),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.status_code(), expected, "{error:?}");
        }
    }
}
