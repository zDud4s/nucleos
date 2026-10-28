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

/// **Pure.** A path glob in the one form containment compares: `\` read as `/`, empty and `.`
/// segments dropped (so `./a`, `a//b` and `a/b/` all clean up), and refused when it contains a `..`
/// segment, is absolute (`/x`, `C:/x`) or is left empty. Case is kept: containment is
/// case-sensitive, which on Windows can only make it refuse more, never less.
pub fn normalise_glob(raw: &str) -> Result<String, NodeError> {
    let unified = raw.trim().replace('\\', "/");
    if unified.starts_with('/') || unified.as_bytes().get(1) == Some(&b':') {
        return Err(NodeError::Invalid(format!(
            "owned path {raw:?} must be relative to the project root"
        )));
    }
    let mut kept = Vec::new();
    for segment in unified.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                return Err(NodeError::Invalid(format!(
                    "owned path {raw:?} may not contain a \"..\" segment"
                )));
            }
            other => kept.push(other),
        }
    }
    if kept.is_empty() {
        return Err(NodeError::Invalid(format!(
            "owned path {raw:?} names no file or directory"
        )));
    }
    Ok(kept.join("/"))
}

/// Whether the text carries any glob metacharacter.
fn has_wildcard(text: &str) -> bool {
    text.contains(['*', '?', '[', '{'])
}

/// Whether one path segment matches a segment pattern: `*` is any run of characters, `?` exactly
/// one. Neither crosses a `/`, because the caller has already split on it.
fn segment_match(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((&'*', rest)) => (0..=text.len()).any(|skip| segment_match(rest, &text[skip..])),
        Some((&'?', rest)) => !text.is_empty() && segment_match(rest, &text[1..]),
        Some((literal, rest)) => text.first() == Some(literal) && segment_match(rest, &text[1..]),
    }
}

/// Whether a segment list matches a pattern segment list; a `**` segment takes zero or more whole
/// segments.
fn segments_match(pattern: &[&str], text: &[&str]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((head, rest)) if *head == "**" => {
            (0..=text.len()).any(|skip| segments_match(rest, &text[skip..]))
        }
        Some((head, rest)) => match text.split_first() {
            Some((first, tail)) => {
                let pattern_chars: Vec<char> = head.chars().collect();
                let text_chars: Vec<char> = first.chars().collect();
                segment_match(&pattern_chars, &text_chars) && segments_match(rest, tail)
            }
            None => false,
        },
    }
}

/// Whether `child` is `dir` itself or lies under it, at a segment boundary.
fn under_dir(child: &str, dir: &str) -> bool {
    child == dir
        || child
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// **Pure.** Whether every file the `child` glob can name is also named by the `parent` glob. Both
/// arguments must already be `normalise_glob` output. Conservative: when unsure, `false`.
///
/// - parent without wildcards: a file or a directory. The child is the parent itself or starts
///   with `<parent>/` (a segment boundary, so `core/srcx` is not inside `core/src`), and may carry
///   wildcards after that; the same rule as `<prefix>/**` below;
/// - parent `<prefix>/**` with a wildcard-free prefix: the child is the prefix itself, or starts
///   with `<prefix>/` (a segment boundary, so `core/srcx` is not inside `core/src/**`), and may
///   carry wildcards after that;
/// - any other wildcard form (`*.rs`, `a*/b`, `**`): the child must be literal and match the parent
///   glob, where `*` and `?` stay inside a segment and `**` crosses segments. A parent with `[` or
///   `{` contains nothing, because those forms are not understood here.
pub fn path_contained(child: &str, parent: &str) -> bool {
    if !has_wildcard(parent) {
        return under_dir(child, parent);
    }
    if let Some(prefix) = parent.strip_suffix("/**")
        && !has_wildcard(prefix)
    {
        return under_dir(child, prefix);
    }
    if has_wildcard(child) || parent.contains(['[', '{']) {
        return false;
    }
    let pattern: Vec<&str> = parent.split('/').collect();
    let text: Vec<&str> = child.split('/').collect();
    segments_match(&pattern, &text)
}

/// **Pure.** The L1 rule for a child opened under `parent`: every glob the child owns must be
/// contained in some glob the parent owns. A child with no globs (a reviewer) is always accepted.
/// A parent with no globs imposes no limit when it is the root, which owns everything, and is
/// otherwise only allowed children with no globs. A malformed child glob (`..`, absolute, blank) is
/// `Invalid`; a well-formed one outside the parent is `Conflict`.
fn check_containment(parent: &Node, child_paths: &[String]) -> Result<(), NodeError> {
    let children = child_paths
        .iter()
        .map(|path| normalise_glob(path))
        .collect::<Result<Vec<_>, _>>()?;
    if children.is_empty() {
        return Ok(());
    }
    if parent.owned_paths.is_empty() {
        if parent.kind == KIND_ROOT {
            return Ok(());
        }
        return Err(NodeError::Conflict(format!(
            "parent {:?} owns no paths, so it only takes children with no owned_paths",
            parent.title
        )));
    }
    // A parent glob that does not normalise contains nothing.
    let owned: Vec<String> = parent
        .owned_paths
        .iter()
        .filter_map(|path| normalise_glob(path).ok())
        .collect();
    for (raw, child) in child_paths.iter().zip(&children) {
        if !owned.iter().any(|glob| path_contained(child, glob)) {
            return Err(NodeError::Conflict(format!(
                "owned path {raw:?} is not contained in any owned path of parent {:?} ({})",
                parent.title,
                parent.owned_paths.join(", ")
            )));
        }
    }
    Ok(())
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
/// parent's root, project and task, and starts `running`. A child's `owned_paths` must be
/// contained in its parent's (`check_containment`): a root with no paths is unrestricted, any
/// other parent with no paths takes only children with none.
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
                check_containment(&parent, &args.owned_paths)?;
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

/// The longest slice of a node's summary that goes into a feed line.
const FEED_SUMMARY_CHARS: usize = 200;

/// The feed line a close leaves behind, or `None` when it ends no tree and parks none.
///
/// `blocked` on any node is `work_tree_parked`; a root closing `done` or `failed` is
/// `work_tree_finished`. Pure so the wording is tested without a database: the handler in
/// `http/nodes.rs` is the only writer and calls this with the node `close` returned.
pub fn feed_line(root_title: &str, node: &Node) -> Option<(&'static str, String)> {
    let kind = match (node.status.as_str(), node.kind.as_str()) {
        ("blocked", _) => "work_tree_parked",
        ("done" | "failed", "root") => "work_tree_finished",
        _ => return None,
    };
    let mut line = format!("{root_title}: {}", node.status);
    let summary: String = node
        .summary
        .as_deref()
        .unwrap_or_default()
        .trim()
        .chars()
        .take(FEED_SUMMARY_CHARS)
        .collect();
    if !summary.is_empty() {
        line.push_str(" — ");
        line.push_str(&summary);
    }
    if kind == "work_tree_parked" && !node.decisions.is_empty() {
        line.push_str(" — precisa de: ");
        line.push_str(&node.decisions.join("; "));
    }
    Some((kind, line))
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
    pub owned_paths: Vec<String>,
    pub updated_at: String,
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
                owned_paths: node.owned_paths.clone(),
                updated_at: node.updated_at.clone(),
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
        assert!(globs_overlap("a/b", "a/b/x.rs"));
        assert!(globs_overlap("a/b", "a/b/**"));
        let a = vec!["x/**".to_owned(), "y/**".to_owned()];
        let b = vec!["z/**".to_owned(), "y/q.rs".to_owned()];
        assert_eq!(
            sets_overlap(&a, &b),
            Some(("y/**".to_owned(), "y/q.rs".to_owned()))
        );
        assert_eq!(sets_overlap(&a, &[]), None);
    }

    #[test]
    fn normalise_glob_cleans_and_refuses() {
        assert_eq!(normalise_glob("core/src/**").unwrap(), "core/src/**");
        assert_eq!(normalise_glob("./core/src/**").unwrap(), "core/src/**");
        assert_eq!(normalise_glob("core\\src\\x.rs").unwrap(), "core/src/x.rs");
        assert_eq!(
            normalise_glob("core//src/./x.rs/").unwrap(),
            "core/src/x.rs"
        );
        assert_eq!(normalise_glob("  a/b  ").unwrap(), "a/b");

        for bad in [
            "core/../etc/**",
            "../x",
            "a/..",
            "a\\..\\b",
            "/etc/**",
            "C:/Windows/**",
            "",
            "./",
            "   ",
        ] {
            assert!(
                matches!(normalise_glob(bad), Err(NodeError::Invalid(_))),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn path_contained_literal_parent_is_a_file_or_a_directory() {
        let dir = ".claude/worktrees/slash-cd";
        assert!(path_contained("core/src/a.rs", "core/src/a.rs"));
        // A literal parent may be a directory, so a sub-path is inside it.
        assert!(path_contained("core/src/a.rs/x", "core/src/a.rs"));
        for inside in [
            ".claude/worktrees/slash-cd/core/src/x.rs",
            ".claude/worktrees/slash-cd/**",
            ".claude/worktrees/slash-cd/core/*.rs",
            ".claude/worktrees/slash-cd/a/**/b?.rs",
        ] {
            assert!(path_contained(inside, dir), "{inside:?}");
        }

        assert!(!path_contained("core/src/b.rs", "core/src/a.rs"));
        // A wildcard child is wider than a literal parent.
        assert!(!path_contained("core/src/*.rs", "core/src/a.rs"));
        for outside in [
            ".claude/worktrees/slash-cd-evil",
            ".claude/worktrees/slash-cd-evil/x.rs",
            ".claude/worktrees/slash-cd*",
            ".claude/worktrees/slash-c",
            ".claude/worktrees",
            "other/x.rs",
        ] {
            assert!(!path_contained(outside, dir), "{outside:?}");
        }
    }

    #[test]
    fn path_contained_dir_glob_parent_respects_segment_boundary() {
        assert!(path_contained("core/src/a.rs", "core/src/**"));
        assert!(path_contained("core/src/deep/er/a.rs", "core/src/**"));
        assert!(path_contained("core/src/**", "core/src/**"));
        assert!(path_contained("core/src/*.rs", "core/src/**"));
        assert!(path_contained("core/src/a/**", "core/src/**"));
        // The prefix itself is contained: `core/src/**` covers the directory it names.
        assert!(path_contained("core/src", "core/src/**"));
        // Segment boundary: `srcx` is not `src`.
        assert!(!path_contained("core/srcx/a.rs", "core/src/**"));
        assert!(!path_contained("core/srcx", "core/src/**"));
        assert!(!path_contained("core/src*", "core/src/**"));
        // A wildcard before the end of the prefix is wider than the parent.
        assert!(!path_contained("core/*/a.rs", "core/src/**"));
        assert!(!path_contained("core/**", "core/src/**"));
        assert!(!path_contained("other/src/a.rs", "core/src/**"));
    }

    #[test]
    fn path_contained_other_wildcard_parent_needs_literal_matching_child() {
        // `*` stays inside one segment.
        assert!(path_contained("src/lib.rs", "src/*.rs"));
        assert!(!path_contained("src/a/lib.rs", "src/*.rs"));
        assert!(!path_contained("src/lib.txt", "src/*.rs"));
        // `?` is exactly one character of a segment.
        assert!(path_contained("src/a.rs", "src/?.rs"));
        assert!(!path_contained("src/ab.rs", "src/?.rs"));
        // A wildcard in a middle segment.
        assert!(path_contained("a1/b", "a*/b"));
        assert!(!path_contained("a1/c", "a*/b"));
        // `**` crosses segments, including zero of them.
        assert!(path_contained("src/deep/x.rs", "**/x.rs"));
        assert!(path_contained("x.rs", "**/x.rs"));
        assert!(!path_contained("src/deep/y.rs", "**/x.rs"));
        // A lone `**` is not a `/**` suffix: only literal children are accepted.
        assert!(path_contained("a/b.rs", "**"));
        assert!(!path_contained("a/**", "**"));
        // A wildcard child is never contained in this form.
        assert!(!path_contained("src/*.rs", "src/*.rs"));
        assert!(!path_contained("src/l?b.rs", "src/*.rs"));
        // Classes and alternatives are not understood, so they contain nothing.
        assert!(!path_contained("a/x", "[ab]/x"));
        assert!(!path_contained("a/x", "{a,b}/x"));
        // A wildcard inside a `/**` prefix also takes this form.
        assert!(path_contained("a1/b/c.rs", "a*/**"));
        assert!(!path_contained("b1/b/c.rs", "a*/**"));
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

    fn owning(kind: &str, paths: &[&str]) -> Node {
        let mut parent = node(0, "running", None);
        parent.kind = kind.into();
        parent.owned_paths = paths.iter().map(|p| (*p).to_owned()).collect();
        parent
    }

    fn strings(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn check_containment_literal_and_dir_glob_parents() {
        let parent = owning("orchestrator", &["core/src/a.rs", "shell/**"]);
        assert!(check_containment(&parent, &strings(&["core/src/a.rs"])).is_ok());
        assert!(check_containment(&parent, &strings(&["shell/src/x.tsx", "./shell/**"])).is_ok());
        let refused = check_containment(&parent, &strings(&["core/src/b.rs"]));
        assert!(
            matches!(refused, Err(NodeError::Conflict(_))),
            "{refused:?}"
        );
        // One contained glob does not excuse another that is not.
        let mixed = check_containment(&parent, &strings(&["shell/x.rs", "core/src/b.rs"]));
        assert!(matches!(mixed, Err(NodeError::Conflict(_))), "{mixed:?}");
        let message = mixed.unwrap_err().to_string();
        assert!(message.contains("core/src/b.rs"), "{message}");
    }

    #[test]
    fn check_containment_literal_parent_takes_sub_paths() {
        for owned in [
            ".claude/worktrees/slash-cd",
            ".claude/worktrees/slash-cd/",
            ".claude\\worktrees\\slash-cd",
        ] {
            let parent = owning("orchestrator", &[owned]);
            for inside in [
                ".claude/worktrees/slash-cd",
                ".claude/worktrees/slash-cd/core/src/x.rs",
                ".claude/worktrees/slash-cd/**",
                ".claude\\worktrees\\slash-cd\\core\\src\\x.rs",
                "./.claude/worktrees/slash-cd/shell/**",
            ] {
                let accepted = check_containment(&parent, &strings(&[inside]));
                assert!(accepted.is_ok(), "{owned:?} / {inside:?}: {accepted:?}");
            }
            for outside in [
                ".claude/worktrees/slash-cd-evil/x.rs",
                ".claude/worktrees/slash-cd*",
                "core/src/x.rs",
            ] {
                let refused = check_containment(&parent, &strings(&[outside]));
                assert!(
                    matches!(refused, Err(NodeError::Conflict(_))),
                    "{owned:?} / {outside:?}: {refused:?}"
                );
            }
            for bad in [
                ".claude/worktrees/slash-cd/../other/x.rs",
                "/.claude/worktrees/slash-cd/x",
                "C:/x",
            ] {
                let refused = check_containment(&parent, &strings(&[bad]));
                assert!(
                    matches!(refused, Err(NodeError::Invalid(_))),
                    "{owned:?} / {bad:?}: {refused:?}"
                );
            }
        }
    }

    #[test]
    fn check_containment_other_wildcard_parent() {
        let parent = owning("orchestrator", &["src/*.rs"]);
        assert!(check_containment(&parent, &strings(&["src/lib.rs"])).is_ok());
        let deep = check_containment(&parent, &strings(&["src/a/lib.rs"]));
        assert!(matches!(deep, Err(NodeError::Conflict(_))), "{deep:?}");
        let wild = check_containment(&parent, &strings(&["src/*.rs"]));
        assert!(matches!(wild, Err(NodeError::Conflict(_))), "{wild:?}");
    }

    #[test]
    fn check_containment_refuses_dotdot_as_invalid() {
        let parent = owning("root", &["core/**"]);
        let refused = check_containment(&parent, &strings(&["core/../etc/x"]));
        assert!(matches!(refused, Err(NodeError::Invalid(_))), "{refused:?}");
        // Even an unrestricted root refuses a `..` child.
        let free = owning("root", &[]);
        let refused = check_containment(&free, &strings(&["a/../b"]));
        assert!(matches!(refused, Err(NodeError::Invalid(_))), "{refused:?}");
    }

    #[test]
    fn check_containment_empty_parent_rules() {
        // A root with no paths owns everything.
        let root = owning("root", &[]);
        assert!(check_containment(&root, &strings(&["any/where/**", "x.rs"])).is_ok());
        // Any other role with no paths takes only children without paths.
        for kind in ["orchestrator", "leaf"] {
            let parent = owning(kind, &[]);
            assert!(check_containment(&parent, &[]).is_ok());
            let refused = check_containment(&parent, &strings(&["a/**"]));
            assert!(
                matches!(refused, Err(NodeError::Conflict(_))),
                "{kind}: {refused:?}"
            );
        }
    }

    #[test]
    fn check_containment_child_without_paths_is_always_accepted() {
        let parent = owning("orchestrator", &["core/**"]);
        assert!(check_containment(&parent, &[]).is_ok());
    }

    #[tokio::test]
    async fn open_accepts_children_contained_in_a_parent_with_paths() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let orch = child_args(&root.id, "orchestrator", "Core", &["core/src/**"]);
        let orch = open(&pool, None, &limits, &orch).await.unwrap();
        for (title, paths) in [
            ("leaf-a", vec!["core/src/a/**"]),
            ("leaf-b", vec!["core/src/b.rs"]),
            ("reviewer", vec![]),
        ] {
            let args = child_args(&orch.id, "leaf", title, &paths);
            open(&pool, None, &limits, &args)
                .await
                .unwrap_or_else(|e| panic!("{title}: {e}"));
        }
    }

    #[tokio::test]
    async fn open_accepts_sub_paths_of_a_literal_parent_path() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let orch = child_args(
            &root.id,
            "orchestrator",
            "Slash",
            &[".claude/worktrees/slash-cd"],
        );
        let orch = open(&pool, None, &limits, &orch).await.unwrap();

        // No running siblings yet, so every refusal here is the containment one.
        for outside in [".claude/worktrees/slash-cd-evil/x.rs", "other/x.rs"] {
            let args = child_args(&orch.id, "leaf", "stray", &[outside]);
            let refused = open(&pool, None, &limits, &args).await;
            assert!(
                matches!(refused, Err(NodeError::Conflict(_))),
                "{outside}: {refused:?}"
            );
        }
        let args = child_args(
            &orch.id,
            "leaf",
            "dotdot",
            &[".claude/worktrees/slash-cd/../x"],
        );
        let refused = open(&pool, None, &limits, &args).await;
        assert!(matches!(refused, Err(NodeError::Invalid(_))), "{refused:?}");

        for (title, path) in [
            ("leaf-file", ".claude/worktrees/slash-cd/core/src/x.rs"),
            ("leaf-dir", ".claude/worktrees/slash-cd/shell/**"),
        ] {
            let args = child_args(&orch.id, "leaf", title, &[path]);
            open(&pool, None, &limits, &args)
                .await
                .unwrap_or_else(|e| panic!("{title}: {e}"));
        }
    }

    #[tokio::test]
    async fn open_accepts_a_dotted_child_path_equal_to_the_parent_directory() {
        // Single child, no running siblings: `./core/src` normalises to `core/src`,
        // which is contained in the parent's `core/src/**`.
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let orch = child_args(&root.id, "orchestrator", "Core", &["core/src/**"]);
        let orch = open(&pool, None, &limits, &orch).await.unwrap();
        let args = child_args(&orch.id, "leaf", "dotted", &["./core/src"]);
        open(&pool, None, &limits, &args).await.unwrap();
    }

    #[tokio::test]
    async fn open_refuses_child_outside_the_parent_paths() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let orch = child_args(&root.id, "orchestrator", "Core", &["core/src/**"]);
        let orch = open(&pool, None, &limits, &orch).await.unwrap();

        for outside in ["shell/src/x.tsx", "core/srcx/a.rs", "core/**"] {
            let args = child_args(&orch.id, "leaf", "stray", &[outside]);
            let refused = open(&pool, None, &limits, &args).await;
            assert!(
                matches!(refused, Err(NodeError::Conflict(_))),
                "{outside}: {refused:?}"
            );
        }
        // Nothing was inserted by the refusals.
        let kids: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM work_nodes WHERE parent_id = ?")
            .bind(&orch.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kids, 0);
    }

    #[tokio::test]
    async fn open_refuses_dotdot_in_a_child_path() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let args = child_args(&root.id, "leaf", "sneaky", &["core/../etc/**"]);
        let refused = open(&pool, None, &limits, &args).await;
        assert!(matches!(refused, Err(NodeError::Invalid(_))), "{refused:?}");
    }

    #[tokio::test]
    async fn open_empty_parent_unrestricted_for_root_reviewers_only_otherwise() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        // The root has no paths: it owns everything, so a child may name any.
        let orch = child_args(&root.id, "orchestrator", "Free", &[]);
        let orch = open(&pool, None, &limits, &orch).await.unwrap();
        let anywhere = child_args(&root.id, "leaf", "Anywhere", &["x/**"]);
        open(&pool, None, &limits, &anywhere).await.unwrap();

        // The orchestrator has no paths either, but it is not a root.
        let with_paths = child_args(&orch.id, "leaf", "Paths", &["y/**"]);
        let refused = open(&pool, None, &limits, &with_paths).await;
        assert!(
            matches!(refused, Err(NodeError::Conflict(_))),
            "{refused:?}"
        );
        let reviewer = child_args(&orch.id, "leaf", "Reviewer", &[]);
        open(&pool, None, &limits, &reviewer).await.unwrap();
    }

    #[tokio::test]
    async fn open_root_with_paths_restricts_its_children() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let mut args = root_args();
        args.owned_paths = strings(&["core/**"]);
        let root = open(&pool, None, &limits, &args).await.unwrap();
        let inside = child_args(&root.id, "leaf", "In", &["core/a.rs"]);
        open(&pool, None, &limits, &inside).await.unwrap();
        let outside = child_args(&root.id, "leaf", "Out", &["shell/a.rs"]);
        let refused = open(&pool, None, &limits, &outside).await;
        assert!(
            matches!(refused, Err(NodeError::Conflict(_))),
            "{refused:?}"
        );
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
    async fn tree_nodes_carry_owned_paths_and_updated_at() {
        let pool = test_pool().await;
        let limits = NodeLimits::default();
        let root = open(&pool, None, &limits, &root_args()).await.unwrap();
        let leaf_args = child_args(&root.id, "leaf", "Leaf", &["core/src/a.rs"]);
        open(&pool, None, &limits, &leaf_args).await.unwrap();

        let built = tree(&pool, &root.id, View::Brief).await.unwrap();
        assert_eq!(
            built.nodes[1].owned_paths,
            vec!["core/src/a.rs".to_string()]
        );
        assert!(!built.nodes[1].updated_at.is_empty());
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

    #[test]
    fn a_done_or_failed_root_is_a_finished_line() {
        let root = node(0, "done", Some("All merged.".into()));
        let (kind, summary) = feed_line("Ship the tree", &root).unwrap();
        assert_eq!(kind, "work_tree_finished");
        assert_eq!(summary, "Ship the tree: done — All merged.");

        let root = node(0, "failed", Some("Gate stayed red.".into()));
        let (kind, summary) = feed_line("Ship the tree", &root).unwrap();
        assert_eq!(kind, "work_tree_finished");
        assert_eq!(summary, "Ship the tree: failed — Gate stayed red.");
    }

    #[test]
    fn a_blocked_node_is_a_parked_line_with_what_it_needs() {
        let mut leaf = node(1, "blocked", Some("Cannot pick a store.".into()));
        leaf.decisions = vec!["use sqlite".into(), "or a file".into()];
        let (kind, summary) = feed_line("Ship the tree", &leaf).unwrap();
        assert_eq!(kind, "work_tree_parked");
        assert_eq!(
            summary,
            "Ship the tree: blocked — Cannot pick a store. — precisa de: use sqlite; or a file"
        );

        // No decisions: no suffix.
        let leaf = node(1, "blocked", Some("Waiting.".into()));
        let (_, summary) = feed_line("Ship the tree", &leaf).unwrap();
        assert_eq!(summary, "Ship the tree: blocked — Waiting.");
    }

    #[test]
    fn a_blocked_root_is_parked_not_finished() {
        let root = node(0, "blocked", Some("Stopped.".into()));
        let (kind, _) = feed_line("Ship the tree", &root).unwrap();
        assert_eq!(kind, "work_tree_parked");
    }

    #[test]
    fn closes_that_do_not_end_or_park_a_tree_write_nothing() {
        assert!(feed_line("T", &node(1, "done", Some("ok".into()))).is_none());
        assert!(feed_line("T", &node(1, "failed", Some("no".into()))).is_none());
        assert!(feed_line("T", &node(0, "handoff", Some("next".into()))).is_none());
        assert!(feed_line("T", &node(1, "handoff", Some("next".into()))).is_none());
        assert!(feed_line("T", &node(1, "running", None)).is_none());
    }

    #[test]
    fn the_node_summary_is_cut_at_200_chars_on_a_char_boundary() {
        // 300 multi-byte chars: a byte slice at 200 would panic mid-codepoint.
        let long = "é".repeat(300);
        let root = node(0, "done", Some(long));
        let (_, summary) = feed_line("T", &root).unwrap();
        assert_eq!(summary, format!("T: done — {}", "é".repeat(200)));

        let exact = "x".repeat(200);
        let root = node(0, "done", Some(exact.clone()));
        let (_, summary) = feed_line("T", &root).unwrap();
        assert_eq!(summary, format!("T: done — {exact}"));
    }

    #[test]
    fn a_missing_or_blank_summary_leaves_no_dangling_dash() {
        let root = node(0, "done", None);
        let (_, summary) = feed_line("T", &root).unwrap();
        assert_eq!(summary, "T: done");

        let root = node(0, "failed", Some("   ".into()));
        let (_, summary) = feed_line("T", &root).unwrap();
        assert_eq!(summary, "T: failed");

        let mut leaf = node(1, "blocked", None);
        leaf.decisions = vec!["pick one".into()];
        let (_, summary) = feed_line("T", &leaf).unwrap();
        assert_eq!(summary, "T: blocked — precisa de: pick one");
    }
}
