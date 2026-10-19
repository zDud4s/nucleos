//! Work nodes: a bounded tree of delegated work (`root -> orchestrator -> leaf`).
//!
//! A long task is split so that no single session has to carry all of it. Every node records its
//! goal, the path globs it owns (so siblings cannot collide), its budget, and, once closed, a short
//! summary plus the path of the handoff file it wrote. The limits (depth, width, summary length)
//! are configurable per worktree through `.nucleos/workflow/speed.yaml` and enforced here, not in
//! the schema. A node that closes as `handoff` also files a `rework` row in `workflow_records`, so
//! the audit can count how often a task outgrew its context.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Map, json};
use sqlx::SqlitePool;

use crate::workflow_records::{self, RecordArgs};

/// The most characters a `View::Brief` text may hold.
pub const BRIEF_MAX_CHARS: usize = 16_000;

const TRUNCATION_MARKER: &str = "…(truncated)";

pub const KIND_ROOT: &str = "root";
pub const KIND_ORCHESTRATOR: &str = "orchestrator";
pub const KIND_LEAF: &str = "leaf";

/// The closed vocabulary of a node's status, the same six values the migration's CHECK allows.
pub const STATUSES: &[&str] = &["planned", "running", "done", "blocked", "handoff", "failed"];

/// The statuses a node can be closed with.
pub const CLOSE_STATUSES: &[&str] = &["done", "blocked", "handoff", "failed"];

/// The statuses that mean "no more work happens under this node".
const CLOSED_STATUSES: &[&str] = &["done", "failed", "handoff", "blocked"];

/// `SELECT <every column> FROM work_nodes <tail>` as a compile-time literal, so sqlx accepts it.
macro_rules! select_nodes {
    ($tail:literal) => {
        concat!(
            "SELECT id, project_id, root_id, parent_id, depth, kind, title, goal, status, \
             owned_paths, budget_tokens, context_peak, summary, handoff_path, decisions, run_id, \
             session_id, chat_id, task_id, created_at, updated_at, closed_at FROM work_nodes ",
            $tail
        )
    };
}

// ---------------------------------------------------------------------------------------------
// Limits
// ---------------------------------------------------------------------------------------------

/// How deep, how wide and how verbose a work tree may get.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct NodeLimits {
    /// The deepest `depth` a node may have (the root is 0).
    pub max_depth: i64,
    /// The most `planned`/`running` children one node may have at a time.
    pub max_width: i64,
    /// The longest `summary` accepted on close, in characters.
    pub summary_max: usize,
}

impl Default for NodeLimits {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_width: 6,
            summary_max: 1500,
        }
    }
}

impl NodeLimits {
    /// Best-effort read of `<root>/.nucleos/workflow/speed.yaml`. The keys sit at the top level or
    /// under a `nodes:` mapping; a missing file, a parse error or a wrong type leaves the default.
    pub fn load(worktree_root: Option<&Path>) -> NodeLimits {
        let mut limits = NodeLimits::default();
        let Some(root) = worktree_root else {
            return limits;
        };
        let Ok(text) = std::fs::read_to_string(root.join(".nucleos/workflow/speed.yaml")) else {
            return limits;
        };
        let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            return limits;
        };
        let pick = |key: &str| -> Option<u64> {
            doc.get("nodes")
                .and_then(|nodes| nodes.get(key))
                .or_else(|| doc.get(key))
                .and_then(serde_yaml::Value::as_u64)
        };
        if let Some(value) = pick("max_depth").and_then(|v| i64::try_from(v).ok()) {
            limits.max_depth = value;
        }
        if let Some(value) = pick("max_width").and_then(|v| i64::try_from(v).ok()) {
            limits.max_width = value;
        }
        if let Some(value) = pick("summary_max").and_then(|v| usize::try_from(v).ok()) {
            limits.summary_max = value;
        }
        limits
    }
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

#[derive(Debug)]
pub enum NodeError {
    /// 400: the request itself is malformed.
    Invalid(String),
    /// 409: the request is well formed but the tree's state refuses it.
    Conflict(String),
    /// 422: a precondition on the content failed (summary too long, handoff file missing).
    Unprocessable(String),
    /// 404.
    NotFound,
    /// 500.
    Db(sqlx::Error),
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeError::Invalid(m) | NodeError::Conflict(m) | NodeError::Unprocessable(m) => {
                f.write_str(m)
            }
            NodeError::NotFound => f.write_str("work node not found"),
            NodeError::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

impl std::error::Error for NodeError {}

impl From<sqlx::Error> for NodeError {
    fn from(error: sqlx::Error) -> Self {
        NodeError::Db(error)
    }
}

impl NodeError {
    pub fn status_code(&self) -> StatusCode {
        match self {
            NodeError::Invalid(_) => StatusCode::BAD_REQUEST,
            NodeError::Conflict(_) => StatusCode::CONFLICT,
            NodeError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            NodeError::NotFound => StatusCode::NOT_FOUND,
            NodeError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct NodeRow {
    id: String,
    project_id: Option<String>,
    root_id: String,
    parent_id: Option<String>,
    depth: i64,
    kind: String,
    title: String,
    goal: String,
    status: String,
    owned_paths: String,
    budget_tokens: Option<i64>,
    context_peak: Option<i64>,
    summary: Option<String>,
    handoff_path: Option<String>,
    decisions: String,
    run_id: Option<String>,
    session_id: Option<String>,
    chat_id: Option<String>,
    task_id: Option<String>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
}

/// One stored node, with its JSON array columns parsed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Node {
    pub id: String,
    pub project_id: Option<String>,
    pub root_id: String,
    pub parent_id: Option<String>,
    pub depth: i64,
    pub kind: String,
    pub title: String,
    pub goal: String,
    pub status: String,
    pub owned_paths: Vec<String>,
    pub budget_tokens: Option<i64>,
    pub context_peak: Option<i64>,
    pub summary: Option<String>,
    pub handoff_path: Option<String>,
    pub decisions: Vec<String>,
    pub run_id: Option<String>,
    pub session_id: Option<String>,
    pub chat_id: Option<String>,
    pub task_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

impl From<NodeRow> for Node {
    fn from(row: NodeRow) -> Self {
        Node {
            owned_paths: serde_json::from_str(&row.owned_paths).unwrap_or_default(),
            decisions: serde_json::from_str(&row.decisions).unwrap_or_default(),
            id: row.id,
            project_id: row.project_id,
            root_id: row.root_id,
            parent_id: row.parent_id,
            depth: row.depth,
            kind: row.kind,
            title: row.title,
            goal: row.goal,
            status: row.status,
            budget_tokens: row.budget_tokens,
            context_peak: row.context_peak,
            summary: row.summary,
            handoff_path: row.handoff_path,
            run_id: row.run_id,
            session_id: row.session_id,
            chat_id: row.chat_id,
            task_id: row.task_id,
            created_at: row.created_at,
            updated_at: row.updated_at,
            closed_at: row.closed_at,
        }
    }
}

/// The node with this id, or `NotFound`.
pub async fn get(pool: &SqlitePool, id: &str) -> Result<Node, NodeError> {
    let row: Option<NodeRow> = sqlx::query_as(select_nodes!("WHERE id = ?"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    row.map(Node::from).ok_or(NodeError::NotFound)
}

// ---------------------------------------------------------------------------------------------
// Path overlap
// ---------------------------------------------------------------------------------------------

/// The literal text of a glob before its first wildcard, with `\` read as `/` and a leading `./`
/// dropped.
fn literal_prefix(glob: &str) -> String {
    let normalized = glob.replace('\\', "/");
    let trimmed = normalized.strip_prefix("./").unwrap_or(&normalized);
    let end = trimmed.find(['*', '?', '[', '{']).unwrap_or(trimmed.len());
    trimmed[..end].to_owned()
}

/// **Pure.** Whether two path globs may name a common file. Deliberately conservative: it compares
/// only the literal prefixes (the text before the first wildcard) and reports an overlap when one
/// is a prefix of the other, so an empty prefix overlaps everything.
pub fn globs_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (literal_prefix(a), literal_prefix(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// **Pure.** The first pair `(from a, from b)` of globs that overlap, if any.
pub fn sets_overlap(a: &[String], b: &[String]) -> Option<(String, String)> {
    for left in a {
        for right in b {
            if globs_overlap(left, right) {
                return Some((left.clone(), right.clone()));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OpenArgs {
    #[serde(default)]
    pub parent_id: Option<String>,
    pub kind: String,
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub owned_paths: Vec<String>,
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opened {
    pub id: String,
    pub root_id: String,
    pub depth: i64,
}

/// Opens a node. Without `parent_id` it is a root; with one it is a child that inherits the
/// parent's root, project and task, and starts `running`.
pub async fn open(
    pool: &SqlitePool,
    project_id: Option<&str>,
    limits: &NodeLimits,
    args: &OpenArgs,
) -> Result<Opened, NodeError> {
    let title = args.title.trim();
    let goal = args.goal.trim();
    if title.is_empty() {
        return Err(NodeError::Invalid("title may not be blank".to_owned()));
    }
    if goal.is_empty() {
        return Err(NodeError::Invalid("goal may not be blank".to_owned()));
    }
    let kind = args.kind.trim();
    let id = crate::auth::generate_uuid_v4();
    let now = chrono::Utc::now().to_rfc3339();
    let owned = serde_json::to_string(&args.owned_paths).unwrap_or_else(|_| "[]".to_owned());

    let (root_id, depth, project, task_id): (String, i64, Option<String>, Option<String>) =
        match args.parent_id.as_deref() {
            None => {
                if kind != KIND_ROOT {
                    return Err(NodeError::Invalid(
                        "a node without a parent must have kind \"root\"".to_owned(),
                    ));
                }
                (
                    id.clone(),
                    0,
                    project_id.map(str::to_owned),
                    args.task_id.clone(),
                )
            }
            Some(parent_id) => {
                if kind != KIND_ORCHESTRATOR && kind != KIND_LEAF {
                    return Err(NodeError::Invalid(
                        "a child node must have kind \"orchestrator\" or \"leaf\"".to_owned(),
                    ));
                }
                let parent = get(pool, parent_id).await?;
                if CLOSED_STATUSES.contains(&parent.status.as_str()) {
                    return Err(NodeError::Conflict(format!(
                        "parent node is already {}; it cannot take new children",
                        parent.status
                    )));
                }
                let depth = parent.depth + 1;
                if depth > limits.max_depth {
                    return Err(NodeError::Conflict(format!(
                        "depth {depth} exceeds the limit of {}",
                        limits.max_depth
                    )));
                }
                let siblings: Vec<NodeRow> = sqlx::query_as(select_nodes!(
                    "WHERE parent_id = ? AND status IN ('planned', 'running')"
                ))
                .bind(&parent.id)
                .fetch_all(pool)
                .await?;
                if i64::try_from(siblings.len()).unwrap_or(i64::MAX) >= limits.max_width {
                    return Err(NodeError::Conflict(format!(
                        "the parent already has {} open children; the limit is {}",
                        siblings.len(),
                        limits.max_width
                    )));
                }
                for sibling in siblings.into_iter().filter(|s| s.status == "running") {
                    let sibling = Node::from(sibling);
                    if let Some((mine, theirs)) =
                        sets_overlap(&args.owned_paths, &sibling.owned_paths)
                    {
                        return Err(NodeError::Conflict(format!(
                            "owned path {mine:?} overlaps {theirs:?}, owned by running sibling {:?}",
                            sibling.title
                        )));
                    }
                }
                (
                    parent.root_id.clone(),
                    depth,
                    project_id.map(str::to_owned).or(parent.project_id.clone()),
                    args.task_id.clone().or(parent.task_id.clone()),
                )
            }
        };

    sqlx::query(
        "INSERT INTO work_nodes \
         (id, project_id, root_id, parent_id, depth, kind, title, goal, status, owned_paths, \
          budget_tokens, run_id, session_id, chat_id, task_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'running', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&project)
    .bind(&root_id)
    .bind(&args.parent_id)
    .bind(depth)
    .bind(kind)
    .bind(title)
    .bind(goal)
    .bind(&owned)
    .bind(args.budget_tokens)
    .bind(&args.run_id)
    .bind(&args.session_id)
    .bind(&args.chat_id)
    .bind(&task_id)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(Opened { id, root_id, depth })
}

// ---------------------------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateArgs {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub context_peak: Option<i64>,
}

/// Changes a node's status, appends a decision, and/or raises its context peak (kept as the max).
pub async fn update(pool: &SqlitePool, id: &str, args: &UpdateArgs) -> Result<Node, NodeError> {
    let node = get(pool, id).await?;
    let status = match args.status.as_deref().map(str::trim) {
        Some(status) if !STATUSES.contains(&status) => {
            return Err(NodeError::Invalid(format!(
                "status must be one of {}, not {status:?}",
                STATUSES.join(", ")
            )));
        }
        Some(status) => status.to_owned(),
        None => node.status.clone(),
    };
    let mut decisions = node.decisions.clone();
    if let Some(decision) = args.decision.as_deref().map(str::trim) {
        if decision.is_empty() {
            return Err(NodeError::Invalid("decision may not be blank".to_owned()));
        }
        decisions.push(decision.to_owned());
    }
    let peak = match (node.context_peak, args.context_peak) {
        (Some(old), Some(new)) => Some(old.max(new)),
        (old, new) => old.or(new),
    };
    sqlx::query(
        "UPDATE work_nodes SET status = ?, decisions = ?, context_peak = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&status)
    .bind(serde_json::to_string(&decisions).unwrap_or_else(|_| "[]".to_owned()))
    .bind(peak)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(id)
    .execute(pool)
    .await?;
    get(pool, id).await
}

// ---------------------------------------------------------------------------------------------
// Close
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CloseArgs {
    pub status: String,
    pub summary: String,
    pub handoff_path: String,
}

/// Closes a node with a summary and the handoff file it wrote.
///
/// The close is committed first and the `rework` record for a `handoff` is filed afterwards,
/// best effort: a failure to file the record is logged and never undoes the close, because the
/// node's state is the source of truth and the record is only an audit trail.
pub async fn close(
    pool: &SqlitePool,
    id: &str,
    limits: &NodeLimits,
    args: &CloseArgs,
) -> Result<Node, NodeError> {
    let status = args.status.trim();
    if !CLOSE_STATUSES.contains(&status) {
        return Err(NodeError::Invalid(format!(
            "status must be one of {}, not {status:?}",
            CLOSE_STATUSES.join(", ")
        )));
    }
    let node = get(pool, id).await?;
    if node.closed_at.is_some() {
        return Err(NodeError::Conflict(format!(
            "node is already closed as {}",
            node.status
        )));
    }
    let summary = args.summary.trim();
    let length = summary.chars().count();
    if length > limits.summary_max {
        return Err(NodeError::Unprocessable(format!(
            "summary is {length} characters; the limit is {}",
            limits.summary_max
        )));
    }
    let handoff = args.handoff_path.trim();
    if handoff.is_empty() || !Path::new(handoff).is_file() {
        return Err(NodeError::Unprocessable(format!(
            "handoff_path {handoff:?} is not an existing file"
        )));
    }
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE work_nodes SET status = ?, summary = ?, handoff_path = ?, closed_at = ?, \
         updated_at = ? WHERE id = ?",
    )
    .bind(status)
    .bind(summary)
    .bind(handoff)
    .bind(&now)
    .bind(&now)
    .bind(id)
    .execute(pool)
    .await?;
    let closed = get(pool, id).await?;
    if status == "handoff"
        && let Err(error) = file_handoff_record(pool, &closed).await
    {
        tracing::warn!(node = %closed.id, %error, "could not file the handoff rework record");
    }
    Ok(closed)
}

/// Files the `rework` row that says a node outgrew its context and handed off.
async fn file_handoff_record(pool: &SqlitePool, node: &Node) -> Result<(), String> {
    let mut payload = Map::new();
    payload.insert("kind".to_owned(), json!("decomposition"));
    payload.insert("node_id".to_owned(), json!(node.id));
    payload.insert("root_id".to_owned(), json!(node.root_id));
    payload.insert("depth".to_owned(), json!(node.depth));
    payload.insert("budget_tokens".to_owned(), json!(node.budget_tokens));
    payload.insert("context_peak".to_owned(), json!(node.context_peak));
    let args = RecordArgs {
        kind: workflow_records::KIND_REWORK.to_owned(),
        task_id: node.task_id.clone().unwrap_or_else(|| node.root_id.clone()),
        phase: Some("node".to_owned()),
        caught_by: Some("budget".to_owned()),
        payload,
        worktree: None,
    };
    let record =
        workflow_records::validate(&args, node.project_id.as_deref().unwrap_or("unknown"), None)?;
    workflow_records::insert(pool, &record)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Active / tree
// ---------------------------------------------------------------------------------------------

/// The newest `running` root bound to this session or chat, as its `root_id`.
pub async fn active(
    pool: &SqlitePool,
    session_id: Option<&str>,
    chat_id: Option<&str>,
) -> Result<Option<String>, NodeError> {
    if session_id.is_none() && chat_id.is_none() {
        return Err(NodeError::Invalid(
            "session_id or chat_id is required".to_owned(),
        ));
    }
    let found: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM work_nodes WHERE parent_id IS NULL AND status = 'running' \
         AND ((? IS NOT NULL AND session_id = ?) OR (? IS NOT NULL AND chat_id = ?)) \
         ORDER BY created_at DESC, rowid DESC LIMIT 1",
    )
    .bind(session_id)
    .bind(session_id)
    .bind(chat_id)
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;
    Ok(found.map(|(id,)| id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Brief,
    Full,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TreeNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub depth: i64,
    pub kind: String,
    pub title: String,
    pub status: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tree {
    pub root_id: String,
    pub goal: String,
    pub status: String,
    /// The root's own decisions.
    pub decisions: Vec<String>,
    pub nodes: Vec<TreeNode>,
    /// The markdown rendering; bounded by `BRIEF_MAX_CHARS` for `View::Brief`.
    pub text: String,
}

/// Every node of the tree under `root_id`, depth first, siblings oldest first.
async fn ordered_nodes(pool: &SqlitePool, root_id: &str) -> Result<Vec<Node>, NodeError> {
    let rows: Vec<NodeRow> = sqlx::query_as(select_nodes!(
        "WHERE root_id = ? ORDER BY created_at, rowid"
    ))
    .bind(root_id)
    .fetch_all(pool)
    .await?;
    let all: Vec<Node> = rows.into_iter().map(Node::from).collect();
    let mut children: HashMap<Option<String>, Vec<usize>> = HashMap::new();
    for (index, node) in all.iter().enumerate() {
        children
            .entry(node.parent_id.clone())
            .or_default()
            .push(index);
    }
    let mut ordered = Vec::with_capacity(all.len());
    let mut stack: Vec<usize> = children
        .get(&None::<String>)
        .map(|roots| roots.iter().rev().copied().collect())
        .unwrap_or_default();
    while let Some(index) = stack.pop() {
        ordered.push(all[index].clone());
        if let Some(kids) = children.get(&Some(all[index].id.clone())) {
            stack.extend(kids.iter().rev().copied());
        }
    }
    Ok(ordered)
}

/// The tree under `root_id`, with its markdown rendering.
pub async fn tree(pool: &SqlitePool, root_id: &str, view: View) -> Result<Tree, NodeError> {
    let nodes = ordered_nodes(pool, root_id).await?;
    let root = nodes
        .iter()
        .find(|node| node.id == root_id && node.parent_id.is_none())
        .ok_or(NodeError::NotFound)?;
    let text = render(&nodes, view);
    Ok(Tree {
        root_id: root.id.clone(),
        goal: root.goal.clone(),
        status: root.status.clone(),
        decisions: root.decisions.clone(),
        nodes: nodes
            .iter()
            .map(|node| TreeNode {
                id: node.id.clone(),
                parent_id: node.parent_id.clone(),
                depth: node.depth,
                kind: node.kind.clone(),
                title: node.title.clone(),
                status: node.status.clone(),
                summary: node.summary.clone(),
            })
            .collect(),
        text,
    })
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

/// **Pure.** Markdown for a tree given depth first with the root at index 0.
///
/// `View::Full` is rendered whole. `View::Brief` is held to `BRIEF_MAX_CHARS` by shedding, in
/// order: the summaries of `done` nodes (oldest closed first), then the `done` nodes' lines
/// (oldest closed first), and as a last resort a hard cut that ends in a truncation marker.
pub fn render(nodes: &[Node], view: View) -> String {
    let none = HashSet::new();
    let full = render_with(nodes, &none, &none);
    if view == View::Full || full.chars().count() <= BRIEF_MAX_CHARS {
        return full;
    }
    let mut done: Vec<&Node> = nodes.iter().filter(|node| node.status == "done").collect();
    done.sort_by(|a, b| a.closed_at.cmp(&b.closed_at));

    let mut no_summary: HashSet<String> = HashSet::new();
    for node in &done {
        no_summary.insert(node.id.clone());
        let text = render_with(nodes, &no_summary, &none);
        if text.chars().count() <= BRIEF_MAX_CHARS {
            return text;
        }
    }
    let mut hidden: HashSet<String> = HashSet::new();
    for node in &done {
        hidden.insert(node.id.clone());
        let text = render_with(nodes, &no_summary, &hidden);
        if text.chars().count() <= BRIEF_MAX_CHARS {
            return text;
        }
    }
    let text = render_with(nodes, &no_summary, &hidden);
    let keep = BRIEF_MAX_CHARS - TRUNCATION_MARKER.chars().count();
    let mut cut: String = text.chars().take(keep).collect();
    cut.push_str(TRUNCATION_MARKER);
    cut
}

fn render_with(nodes: &[Node], no_summary: &HashSet<String>, hidden: &HashSet<String>) -> String {
    let Some(root) = nodes.first() else {
        return String::new();
    };
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", root.title));
    out.push_str(&format!("Goal: {}\n", root.goal));
    out.push_str(&format!("Status: {}\n\n", root.status));

    out.push_str("## Decisions\n\n");
    let mut any = false;
    for node in nodes {
        for decision in &node.decisions {
            any = true;
            if node.parent_id.is_none() {
                out.push_str(&format!("- {decision}\n"));
            } else {
                out.push_str(&format!("- {}: {decision}\n", node.title));
            }
        }
    }
    if !any {
        out.push_str("(none)\n");
    }

    out.push_str("\n## Nodes\n\n");
    for node in nodes.iter().filter(|node| !hidden.contains(&node.id)) {
        let indent = "  ".repeat(usize::try_from(node.depth).unwrap_or(0));
        out.push_str(&format!(
            "{indent}- [{}] {} ({}, {})\n",
            node.status, node.title, node.kind, node.id
        ));
        if !no_summary.contains(&node.id)
            && let Some(summary) = node.summary.as_deref().filter(|s| !s.is_empty())
        {
            for line in summary.lines() {
                out.push_str(&format!("{indent}  {line}\n"));
            }
        }
    }

    out.push_str("\n## Next\n\n");
    let mut open = false;
    for node in nodes {
        if matches!(
            node.status.as_str(),
            "planned" | "running" | "blocked" | "handoff"
        ) {
            open = true;
            out.push_str(&format!(
                "- [{}] {} ({})\n",
                node.status, node.title, node.id
            ));
        }
    }
    if !open {
        out.push_str("(nothing open)\n");
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn root_args() -> OpenArgs {
        OpenArgs {
            kind: "root".into(),
            title: "Root".into(),
            goal: "Ship it".into(),
            session_id: Some("s1".into()),
            ..OpenArgs::default()
        }
    }

    fn child_args(parent: &str, kind: &str, title: &str, paths: &[&str]) -> OpenArgs {
        OpenArgs {
            parent_id: Some(parent.to_owned()),
            kind: kind.into(),
            title: title.into(),
            goal: format!("goal of {title}"),
            owned_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            ..OpenArgs::default()
        }
    }

    fn node(i: usize, status: &str, summary: Option<String>) -> Node {
        Node {
            id: format!("n{i}"),
            project_id: None,
            root_id: "n0".into(),
            parent_id: if i == 0 { None } else { Some("n0".into()) },
            depth: i64::from(i != 0),
            kind: if i == 0 { "root".into() } else { "leaf".into() },
            title: format!("title-{i:04}"),
            goal: "the goal".into(),
            status: status.into(),
            owned_paths: vec![],
            budget_tokens: None,
            context_peak: None,
            summary,
            handoff_path: None,
            decisions: vec![],
            run_id: None,
            session_id: None,
            chat_id: None,
            task_id: None,
            created_at: format!("{i:05}"),
            updated_at: format!("{i:05}"),
            closed_at: if status == "done" {
                Some(format!("{i:05}"))
            } else {
                None
            },
        }
    }

    fn close_args(summary: &str, handoff: &str, status: &str) -> CloseArgs {
        CloseArgs {
            status: status.into(),
            summary: summary.into(),
            handoff_path: handoff.into(),
        }
    }

    fn handoff_file() -> (tempfile::NamedTempFile, String) {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap().to_owned();
        (file, path)
    }

    #[test]
    fn globs_overlap_cases() {
        assert!(globs_overlap("src/**", "src/http/x.rs"));
        assert!(globs_overlap("src/http/x.rs", "src/**"));
        assert!(!globs_overlap("core/a/**", "core/b/**"));
        assert!(globs_overlap("**", "core/b/x.rs"));
        assert!(globs_overlap("./src/**", "src\\lib.rs"));
        assert!(!globs_overlap("src/a.rs", "src/b.rs"));
        let a = vec!["x/**".to_owned(), "y/**".to_owned()];
        let b = vec!["z/**".to_owned(), "y/q.rs".to_owned()];
        assert_eq!(
            sets_overlap(&a, &b),
            Some(("y/**".to_owned(), "y/q.rs".to_owned()))
        );
        assert_eq!(sets_overlap(&a, &[]), None);
    }

    #[test]
    fn render_includes_goal_decisions_status() {
        let mut root = node(0, "running", None);
        root.decisions = vec!["use sqlite".into()];
        let mut child = node(1, "done", Some("did the thing".into()));
        child.decisions = vec!["skip cache".into()];
        let text = render(&[root, child], View::Brief);
        assert!(text.contains("# title-0000"));
        assert!(text.contains("Goal: the goal"));
        assert!(text.contains("Status: running"));
        assert!(text.contains("- use sqlite"));
        assert!(text.contains("- title-0001: skip cache"));
        assert!(text.contains("did the thing"));
        assert!(text.contains("## Next"));
    }

    #[test]
    fn render_brief_drops_oldest_done_summaries_first() {
        let mut nodes = vec![node(0, "running", None)];
        for i in 1..=12 {
            let summary = format!("SUM{i:02}-{}", "x".repeat(1400));
            nodes.push(node(i, "done", Some(summary)));
        }
        assert!(render(&nodes, View::Full).chars().count() > BRIEF_MAX_CHARS);
        let brief = render(&nodes, View::Brief);
        assert!(brief.chars().count() <= BRIEF_MAX_CHARS);
        assert!(!brief.contains("SUM01-"));
        assert!(brief.contains("SUM12-"));
        assert!(brief.contains("title-0001"));
    }

    #[test]
    fn render_brief_drops_oldest_done_nodes_then_truncates() {
        let mut nodes = vec![node(0, "running", None)];
        // ~34 chars per line: 400 nodes (~13.6k) would fit whole, 800 (~27k) must shed.
        for i in 1..=800 {
            nodes.push(node(i, "done", None));
        }
        let brief = render(&nodes, View::Brief);
        assert!(brief.chars().count() <= BRIEF_MAX_CHARS);
        assert!(!brief.contains("title-0001 "));
        assert!(brief.contains("title-0800"));

        // A huge open list cannot be shed, so the last resort cuts it.
        let mut nodes = vec![node(0, "running", None)];
        for i in 1..=400 {
            nodes.push(node(i, "running", None));
        }
        let brief = render(&nodes, View::Brief);
        assert!(brief.chars().count() <= BRIEF_MAX_CHARS);
        assert!(brief.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn limits_load_reads_top_level_and_nested_keys() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(NodeLimits::load(Some(dir.path())), NodeLimits::default());
        std::fs::create_dir_all(dir.path().join(".nucleos/workflow")).unwrap();
        let file = dir.path().join(".nucleos/workflow/speed.yaml");
        std::fs::write(&file, "max_depth: 5\nnodes:\n  max_width: 2\n").unwrap();
        let limits = NodeLimits::load(Some(dir.path()));
        assert_eq!(
            (limits.max_depth, limits.max_width, limits.summary_max),
            (5, 2, 1500)
        );
        std::fs::write(&file, ": not yaml [").unwrap();
        assert_eq!(NodeLimits::load(Some(dir.path())), NodeLimits::default());
    }

    #[tokio::test]
    async fn open_root_and_child() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, Some("p1"), &limits, &root_args())
            .await
            .unwrap();
        assert_eq!(root.depth, 0);
        assert_eq!(root.root_id, root.id);
        let child = open(
            &pool,
            None,
            &limits,
            &child_args(&root.id, "leaf", "A", &["a/**"]),
        )
        .await
        .unwrap();
        assert_eq!(child.depth, 1);
        assert_eq!(child.root_id, root.id);
        let stored = get(&pool, &child.id).await.unwrap();
        assert_eq!(stored.project_id.as_deref(), Some("p1"));
        assert_eq!(stored.status, "running");
        assert_eq!(stored.owned_paths, vec!["a/**".to_owned()]);
        let wrong_kind = child_args(&root.id, "root", "B", &[]);
        assert!(matches!(
            open(&pool, None, &limits, &wrong_kind).await,
            Err(NodeError::Invalid(_))
        ));
        let orphan = child_args("nope", "leaf", "B", &[]);
        assert!(matches!(
            open(&pool, None, &limits, &orphan).await,
            Err(NodeError::NotFound)
        ));
    }

    #[tokio::test]
    async fn open_refuses_depth_beyond_limit() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let mut parent = open(&pool, None, &limits, &root_args()).await.unwrap().id;
        for depth in 1..=3 {
            let title = format!("o{depth}");
            let args = child_args(&parent, "orchestrator", &title, &[]);
            parent = open(&pool, None, &limits, &args).await.unwrap().id;
        }
        let deep = child_args(&parent, "leaf", "deep", &[]);
        let refused = open(&pool, None, &limits, &deep).await;
        assert!(
            matches!(refused, Err(NodeError::Conflict(_))),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn open_refuses_seventh_sibling() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        for i in 0..6 {
            let title = format!("leaf{i}");
            let args = child_args(&root.id, "leaf", &title, &[]);
            open(&pool, None, &limits, &args).await.unwrap();
        }
        let seventh = child_args(&root.id, "leaf", "7th", &[]);
        let refused = open(&pool, None, &limits, &seventh).await;
        assert!(
            matches!(refused, Err(NodeError::Conflict(_))),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn open_refuses_overlap_with_running_sibling_only() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let first = child_args(&root.id, "leaf", "A", &["src/**"]);
        let a = open(&pool, None, &limits, &first).await.unwrap();
        let clash = child_args(&root.id, "leaf", "B", &["src/x.rs"]);
        let refused = open(&pool, None, &limits, &clash).await;
        assert!(
            matches!(refused, Err(NodeError::Conflict(_))),
            "{refused:?}"
        );

        let (_file, path) = handoff_file();
        close(&pool, &a.id, &limits, &close_args("ok", &path, "done"))
            .await
            .unwrap();
        open(&pool, None, &limits, &clash).await.unwrap();
    }

    #[tokio::test]
    async fn close_validates_summary_and_handoff_file() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let (_file, path) = handoff_file();

        let long = "a".repeat(1501);
        let refused = close(&pool, &root.id, &limits, &close_args(&long, &path, "done")).await;
        assert!(
            matches!(refused, Err(NodeError::Unprocessable(_))),
            "{refused:?}"
        );
        let gone = close_args("ok", "/no/such/file.md", "done");
        let missing = close(&pool, &root.id, &limits, &gone).await;
        assert!(
            matches!(missing, Err(NodeError::Unprocessable(_))),
            "{missing:?}"
        );
        let running = close_args("ok", &path, "running");
        let bad = close(&pool, &root.id, &limits, &running).await;
        assert!(matches!(bad, Err(NodeError::Invalid(_))), "{bad:?}");

        let ok = close_args("ok", &path, "done");
        let closed = close(&pool, &root.id, &limits, &ok).await.unwrap();
        assert_eq!(closed.status, "done");
        assert_eq!(closed.summary.as_deref(), Some("ok"));
        assert!(closed.closed_at.is_some());
        let again = close(&pool, &root.id, &limits, &ok).await;
        assert!(matches!(again, Err(NodeError::Conflict(_))), "{again:?}");
    }

    #[tokio::test]
    async fn close_as_handoff_files_one_rework_record() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, Some("p1"), &limits, &root_args())
            .await
            .unwrap();
        let leaf_args = child_args(&root.id, "leaf", "A", &[]);
        let leaf = open(&pool, None, &limits, &leaf_args).await.unwrap();
        let peak = UpdateArgs {
            context_peak: Some(190_000),
            ..UpdateArgs::default()
        };
        update(&pool, &leaf.id, &peak).await.unwrap();
        let (_file, path) = handoff_file();
        let args = close_args("ran out", &path, "handoff");
        close(&pool, &leaf.id, &limits, &args).await.unwrap();

        let records = workflow_records::for_task(&pool, "p1", &root.id)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, "rework");
        assert_eq!(records[0].caught_by.as_deref(), Some("budget"));
        let payload: serde_json::Value = serde_json::from_str(&records[0].payload).unwrap();
        assert_eq!(payload["kind"], "decomposition");
        assert_eq!(payload["node_id"], leaf.id.as_str());
        assert_eq!(payload["context_peak"], 190_000);
    }

    #[tokio::test]
    async fn update_keeps_peak_max_and_appends_decisions() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let up = |peak: i64, decision: &str| UpdateArgs {
            status: None,
            decision: Some(decision.to_owned()),
            context_peak: Some(peak),
        };
        update(&pool, &root.id, &up(500, "first")).await.unwrap();
        let node = update(&pool, &root.id, &up(300, "second")).await.unwrap();
        assert_eq!(node.context_peak, Some(500));
        assert_eq!(
            node.decisions,
            vec!["first".to_owned(), "second".to_owned()]
        );
        let weird = UpdateArgs {
            status: Some("weird".into()),
            ..UpdateArgs::default()
        };
        assert!(matches!(
            update(&pool, &root.id, &weird).await,
            Err(NodeError::Invalid(_))
        ));
        assert!(matches!(
            update(&pool, "nope", &up(1, "x")).await,
            Err(NodeError::NotFound)
        ));
    }

    #[tokio::test]
    async fn active_finds_running_root_until_closed() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        assert_eq!(
            active(&pool, Some("s1"), None).await.unwrap(),
            Some(root.id.clone())
        );
        assert_eq!(active(&pool, Some("other"), None).await.unwrap(), None);
        assert!(matches!(
            active(&pool, None, None).await,
            Err(NodeError::Invalid(_))
        ));
        let (_file, path) = handoff_file();
        close(&pool, &root.id, &limits, &close_args("ok", &path, "done"))
            .await
            .unwrap();
        assert_eq!(active(&pool, Some("s1"), None).await.unwrap(), None);
    }

    #[tokio::test]
    async fn tree_brief_has_nodes_and_text() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let orch_args = child_args(&root.id, "orchestrator", "Orch", &[]);
        let orch = open(&pool, None, &limits, &orch_args).await.unwrap();
        let leaf_args = child_args(&orch.id, "leaf", "Leaf", &[]);
        open(&pool, None, &limits, &leaf_args).await.unwrap();

        let built = tree(&pool, &root.id, View::Brief).await.unwrap();
        assert_eq!(built.root_id, root.id);
        assert_eq!(built.goal, "Ship it");
        assert_eq!(built.nodes.len(), 3);
        assert_eq!(built.nodes[1].title, "Orch");
        assert_eq!(built.nodes[2].title, "Leaf");
        assert!(built.text.contains("Leaf"));
        assert!(matches!(
            tree(&pool, "nope", View::Full).await,
            Err(NodeError::NotFound)
        ));
    }
}
