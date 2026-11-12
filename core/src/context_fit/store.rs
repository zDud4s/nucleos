//! The IO half of the context-fit card: gathers one node's inputs from rows already in the
//! database, runs [`super::assess`], and keeps the card in `node_context_fit`.
//!
//! Best effort end to end. A source table with nothing for the node leaves its signal at `None`
//! (it abstains); a query that fails is logged at debug and treated the same way. [`record_close`]
//! is called from `work_nodes::close` and never fails it: a card that could not be computed is
//! logged and the close stands.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use sqlx::SqlitePool;

use super::{Card, Evidence, Inputs, RULES_VERSION, Thresholds, Verdict, assess, percentile};
use crate::work_nodes::{self, Node};

/// The feed kind of the line a close with a verdict leaves behind.
pub const FEED_KIND: &str = "work_tree_context_fit";

/// The runs of node `?1`: the history table plus the current `work_nodes.run_id`.
const NODE_RUNS: &str = "(SELECT run_id FROM work_node_runs WHERE node_id = ?1 \
     UNION SELECT CAST(run_id AS INTEGER) FROM work_nodes WHERE id = ?1 AND run_id IS NOT NULL)";

/// Words in a blocked node's own text that mean it stopped for lack of information (u6).
const ASKING_WORDS: [&str; 8] = [
    "?", "need", "missing", "which", "unclear", "precisa", "falta", "não sei",
];

// ---------------------------------------------------------------------------------------------
// Thresholds
// ---------------------------------------------------------------------------------------------

/// The `context_fit` section of the `devtime` document, or the spec defaults.
pub fn thresholds() -> Thresholds {
    crate::settings::get_opt::<crate::config::DevtimeConfig>(&crate::settings::Scope::Machine)
        .map(|doc| doc.context_fit.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Gathering
// ---------------------------------------------------------------------------------------------

/// A node's layer: the first path segment most of its owned paths start with, `none` without
/// paths. `core/src/a.rs` and `core/src/b.rs` are `core`; ties go to the alphabetically first.
pub fn layer_of(owned_paths: &[String]) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for path in owned_paths {
        let normal = path.replace('\\', "/");
        let first = normal
            .trim_start_matches("./")
            .split('/')
            .next()
            .unwrap_or("");
        if !first.is_empty() && !first.contains(['*', '?']) {
            *counts.entry(first.to_owned()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map_or_else(|| "none".to_owned(), |(layer, _)| layer)
}

/// True when `path` (possibly absolute, from a transcript) lies in `given`: some trailing run of
/// its segments is contained in the glob.
fn path_is_given(path: &str, given: &[String]) -> bool {
    let normal = path.replace('\\', "/");
    let segments: Vec<&str> = normal.split('/').filter(|s| !s.is_empty()).collect();
    (0..segments.len()).any(|start| {
        let tail = segments[start..].join("/");
        given
            .iter()
            .any(|glob| work_nodes::path_contained(&tail, glob))
    })
}

fn strings(value: &str) -> Vec<String> {
    serde_json::from_str::<Vec<Value>>(value)
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// What the run's attempts say: the reads outside the given before the first edit, and how many
/// pointers were read at all. `None` when the run has no ingested attempts.
#[cfg(test)]
fn read_signals(
    attempts: &[(String, String)],
    owned: &[String],
    pointers: &[String],
) -> (Option<u32>, Option<u32>) {
    let detail = read_detail(attempts, owned, pointers, "none");
    (detail.outside_count, detail.pointers_read)
}

/// The read signals plus the paths behind them, for the pointer learner.
#[derive(Debug, Default)]
struct ReadDetail {
    outside_count: Option<u32>,
    pointers_read: Option<u32>,
    /// Outside reads, made repo-relative so nodes in different worktrees compare equal.
    outside: Vec<String>,
    read: Vec<String>,
    unread: Vec<String>,
}

/// Cuts an absolute transcript path back to the repo-relative one: from the first segment named
/// like the node's layer. Without a layer or a match the path is only slash-normalized.
fn repo_relative(path: &str, layer: &str) -> String {
    let normal = path.replace('\\', "/");
    let segments: Vec<&str> = normal.split('/').filter(|s| !s.is_empty()).collect();
    match segments.iter().position(|s| *s == layer) {
        Some(at) if layer != "none" => segments[at..].join("/"),
        _ => normal,
    }
}

fn read_detail(
    attempts: &[(String, String)],
    owned: &[String],
    pointers: &[String],
    layer: &str,
) -> ReadDetail {
    if attempts.is_empty() {
        return ReadDetail::default();
    }
    let given: Vec<String> = owned.iter().chain(pointers).cloned().collect();
    let mut outside: Vec<String> = Vec::new();
    let mut edited = false;
    let mut read_any: Vec<String> = Vec::new();
    for (reads, edits) in attempts {
        let reads = strings(reads);
        if !edited {
            for read in &reads {
                if !given.is_empty() && !path_is_given(read, &given) && !outside.contains(read) {
                    outside.push(read.clone());
                }
            }
        }
        read_any.extend(reads);
        if !strings(edits).is_empty() {
            edited = true;
        }
    }
    let outside_count = (!given.is_empty()).then_some(outside.len() as u32);
    let (read, unread): (Vec<String>, Vec<String>) = pointers.iter().cloned().partition(|pointer| {
        read_any
            .iter()
            .any(|read| path_is_given(read, std::slice::from_ref(pointer)))
    });
    ReadDetail {
        outside_count,
        pointers_read: (!pointers.is_empty()).then_some(read.len() as u32),
        outside: outside.iter().map(|p| repo_relative(p, layer)).collect(),
        read,
        unread,
    }
}

async fn count(pool: &SqlitePool, sql: &str, node_id: &str) -> u32 {
    match sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(node_id)
        .fetch_one(pool)
        .await
    {
        Ok(n) => u32::try_from(n).unwrap_or(0),
        Err(error) => {
            tracing::debug!(node = node_id, %error, "context fit: a count source is unavailable");
            0
        }
    }
}

/// Reads the inputs of `node`. Never fails: a missing source abstains.
pub async fn gather(pool: &SqlitePool, node: &Node, cfg: &Thresholds) -> Inputs {
    let id = node.id.as_str();
    let mut inputs = Inputs {
        layer: layer_of(&node.owned_paths),
        owned_paths: node.owned_paths.clone(),
        pointers: node.pointers.clone(),
        context_ceiling: node.budget_tokens,
        context_peak: node.context_peak,
        status: node.status.clone(),
        ..Inputs::default()
    };

    // The runs: first launch's initial context, last launch's tools, the peak and the tokens.
    let runs_sql = format!(
        "SELECT initial_context, tools_given, context_peak, \
         COALESCE(input_tokens, 0) + COALESCE(output_tokens, 0) \
         FROM runs WHERE id IN {NODE_RUNS} ORDER BY id"
    );
    let runs: Vec<(Option<i64>, Option<String>, Option<i64>, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(runs_sql))
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    if !runs.is_empty() {
        inputs.initial_context = runs.first().and_then(|r| r.0);
        inputs.tools_given = runs.last().and_then(|r| r.1.as_deref()).map(strings);
        inputs.context_peak = runs
            .iter()
            .filter_map(|r| r.2)
            .chain(node.context_peak)
            .max();
        inputs.tokens = Some(runs.iter().map(|r| r.3).sum());
    }

    inputs.fence_refusals = count(
        pool,
        &format!(
            "SELECT COUNT(*) FROM shadow_decisions WHERE reason = 'node_scope' \
             AND run_id IN {NODE_RUNS}"
        ),
        id,
    )
    .await;
    inputs.tools_requested = count(
        pool,
        &format!(
            "SELECT COUNT(*) FROM loadout_tools WHERE source = 'request' \
             AND CAST(run_id AS INTEGER) IN {NODE_RUNS}"
        ),
        id,
    )
    .await;

    inputs.d_rules = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT rule_id FROM devtime_findings WHERE node_id = ? ORDER BY rule_id",
    )
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let attempts_sql = format!(
        "SELECT a.reads, a.edits FROM devtime_attempts a \
         JOIN devtime_sessions s ON s.session_id = a.session_id \
         WHERE s.run_id IN {NODE_RUNS} ORDER BY a.started_at, a.attempt_id"
    );
    let attempts: Vec<(String, String)> = sqlx::query_as(sqlx::AssertSqlSafe(attempts_sql))
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let detail = read_detail(&attempts, &node.owned_paths, &node.pointers, &inputs.layer);
    inputs.outside_reads_before_edit = detail.outside_count;
    inputs.pointers_read = detail.pointers_read;
    inputs.outside_paths = detail.outside;
    inputs.pointers_read_paths = detail.read;
    inputs.pointers_unread_paths = detail.unread;

    // Fix and rescue rounds and a conflict resolution are launches of the node, keyed by kind.
    let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM tree_launches WHERE node_id = ?")
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    let launches = |prefix: &str| keys.iter().filter(|k| k.starts_with(prefix)).count() as u32;
    inputs.fix_rounds = launches("fix-");
    inputs.rescues = launches("rescue-");
    inputs.sibling_merge_conflict = launches("resolve-") > 0;

    // Handoffs: this node and its predecessors under the same parent with the same title.
    inputs.handoffs = match node.parent_id.as_deref() {
        Some(parent) => sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM work_nodes WHERE parent_id = ? AND title = ? AND status = 'handoff'",
        )
        .bind(parent)
        .bind(&node.title)
        .fetch_one(pool)
        .await
        .map_or(0, |n| u32::try_from(n).unwrap_or(0)),
        None => u32::from(node.status == "handoff"),
    };

    // A contract request lives only in the handoff file's outcome line.
    inputs.contract_requests = node
        .handoff_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map_or(0, |text| {
            u32::from(text.contains("Outcome: contract_request"))
        });
    if node.status == "blocked" {
        let text = format!(
            "{} {}",
            node.summary.as_deref().unwrap_or(""),
            node.decisions.join(" ")
        )
        .to_lowercase();
        inputs.blocked_asking_info = ASKING_WORDS.iter().any(|word| text.contains(word));
    }

    // The layer percentile of tokens per changed line, from the cards already stored. The node's
    // own tokens per line has no source yet (nothing records changed lines), so o3 abstains.
    let since =
        (chrono::Utc::now() - chrono::Duration::days(i64::from(cfg.o3_window_days))).to_rfc3339();
    let mut samples: Vec<f64> = sqlx::query_scalar::<_, f64>(
        "SELECT CAST(json_extract(used, '$.tokens_per_line') AS REAL) FROM node_context_fit \
         WHERE layer = ? AND node_id != ? AND computed_at >= ? \
         AND json_extract(used, '$.tokens_per_line') IS NOT NULL",
    )
    .bind(&inputs.layer)
    .bind(id)
    .bind(&since)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    inputs.layer_nodes = samples.len() as u32;
    inputs.layer_p90 = percentile(&mut samples, cfg.o3_pct);
    inputs
}

// ---------------------------------------------------------------------------------------------
// Storing
// ---------------------------------------------------------------------------------------------

/// Writes `card` as the node's row, replacing any earlier one.
pub async fn upsert(
    pool: &SqlitePool,
    node: &Node,
    layer: &str,
    card: &Card,
) -> Result<(), sqlx::Error> {
    let verdicts: Vec<&str> = card.verdicts.iter().map(|v| v.as_str()).collect();
    sqlx::query(
        "INSERT INTO node_context_fit \
         (node_id, tree_id, layer, verdicts, evidence, given, used, rules_version, computed_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(node_id) DO UPDATE SET tree_id = excluded.tree_id, layer = excluded.layer, \
         verdicts = excluded.verdicts, evidence = excluded.evidence, given = excluded.given, \
         used = excluded.used, rules_version = excluded.rules_version, \
         computed_at = excluded.computed_at",
    )
    .bind(&node.id)
    .bind(&node.root_id)
    .bind(layer)
    .bind(serde_json::to_string(&verdicts).unwrap_or_else(|_| "[]".into()))
    .bind(serde_json::to_string(&card.evidence).unwrap_or_else(|_| "[]".into()))
    .bind(card.given.to_string())
    .bind(card.used.to_string())
    .bind(RULES_VERSION)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Computes and stores the card of `node`, returning it.
pub async fn compute(pool: &SqlitePool, node: &Node) -> Result<Card, sqlx::Error> {
    let cfg = thresholds();
    let inputs = gather(pool, node, &cfg).await;
    let card = assess(&inputs, &cfg);
    upsert(pool, node, &inputs.layer, &card).await?;
    Ok(card)
}

/// The summary of the feed line a card with a verdict earns.
pub fn feed_summary(node: &Node, card: &Card) -> String {
    let verdicts: Vec<&str> = card.verdicts.iter().map(|v| v.as_str()).collect();
    let signals: Vec<&str> = card.evidence.iter().map(|e| e.signal).collect();
    format!(
        "{}: {} ({})",
        node.title,
        verdicts.join(", "),
        signals.join(", ")
    )
}

/// The hook `work_nodes::close` calls: computes the card of the node that just closed, stores it
/// and, when it has a verdict, leaves one `work_tree_context_fit` feed line. Fail-open.
pub async fn record_close(pool: &SqlitePool, node: &Node) {
    let card = match compute(pool, node).await {
        Ok(card) => card,
        Err(error) => {
            tracing::warn!(node = %node.id, %error, "could not store the context-fit card");
            return;
        }
    };
    // Learning reads the stored cards and is fail-open: it never changes the close.
    super::learn::run(pool, node).await;
    if card.is_fit() {
        return;
    }
    let summary = feed_summary(node, &card);
    if let Err(error) = crate::feed::append(
        pool,
        node.project_id.as_deref(),
        FEED_KIND,
        &summary,
        None,
        None,
    )
    .await
    {
        tracing::warn!(node = %node.id, %error, "could not write the context-fit feed line");
    }
}

/// Recomputes up to `limit` closed nodes whose card is missing or older than [`RULES_VERSION`].
/// Returns how many were recomputed. Not called on a timer; a caller that bumped the version
/// (or an operator) runs it.
pub async fn recompute_outdated(pool: &SqlitePool, limit: i64) -> usize {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT n.id FROM work_nodes n \
         LEFT JOIN node_context_fit f ON f.node_id = n.id \
         WHERE n.closed_at IS NOT NULL AND (f.node_id IS NULL OR f.rules_version != ?) \
         ORDER BY n.closed_at DESC LIMIT ?",
    )
    .bind(RULES_VERSION)
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut done = 0;
    for id in ids {
        let Ok(node) = work_nodes::get(pool, &id).await else {
            continue;
        };
        match compute(pool, &node).await {
            Ok(_) => done += 1,
            Err(error) => tracing::warn!(node = %id, %error, "context fit recompute failed"),
        }
    }
    done
}

// ---------------------------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------------------------

/// One stored card as the routes return it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub node_id: String,
    pub tree_id: String,
    pub layer: String,
    pub verdicts: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub given: Value,
    pub used: Value,
    pub rules_version: i64,
    pub computed_at: String,
}

type RawRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    String,
);

const ROW_COLUMNS: &str =
    "node_id, tree_id, layer, verdicts, evidence, given, used, rules_version, computed_at";

fn row_of(raw: RawRow) -> Row {
    let evidence: Vec<Value> = serde_json::from_str(&raw.4).unwrap_or_default();
    Row {
        node_id: raw.0,
        tree_id: raw.1,
        layer: raw.2,
        verdicts: serde_json::from_str(&raw.3).unwrap_or_default(),
        evidence: evidence
            .iter()
            .filter_map(|e| {
                let signal = e.get("signal")?.as_str()?;
                Some(Evidence {
                    signal: signal_name(signal)?,
                    value: e.get("value")?.as_f64()?,
                    threshold: e.get("threshold")?.as_f64()?,
                })
            })
            .collect(),
        given: serde_json::from_str(&raw.5).unwrap_or(Value::Null),
        used: serde_json::from_str(&raw.6).unwrap_or(Value::Null),
        rules_version: raw.7,
        computed_at: raw.8,
    }
}

/// The static name of a stored signal id, so a row read back is an [`Evidence`] again.
fn signal_name(id: &str) -> Option<&'static str> {
    const ALL: [&str; 15] = [
        "u1", "u2", "u3", "u4", "u5", "u6", "o1", "o2", "o3", "o4", "s1", "s2", "s3", "s4", "s5",
    ];
    ALL.into_iter().find(|name| *name == id)
}

/// Every card of one tree, oldest first.
pub async fn tree_rows(pool: &SqlitePool, tree_id: &str) -> Result<Vec<Row>, sqlx::Error> {
    let raw: Vec<RawRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ROW_COLUMNS} FROM node_context_fit WHERE tree_id = ? \
         ORDER BY computed_at, node_id"
    )))
    .bind(tree_id)
    .fetch_all(pool)
    .await?;
    Ok(raw.into_iter().map(row_of).collect())
}

/// Verdict counts; a node with two verdicts counts in both, a node with none is `fit`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VerdictCounts {
    pub under: u32,
    pub over: u32,
    pub split: u32,
    pub fit: u32,
}

impl VerdictCounts {
    fn add(&mut self, verdicts: &[String]) {
        if verdicts.is_empty() {
            self.fit += 1;
        }
        for verdict in verdicts {
            match verdict.as_str() {
                v if v == Verdict::Under.as_str() => self.under += 1,
                v if v == Verdict::Over.as_str() => self.over += 1,
                v if v == Verdict::Split.as_str() => self.split += 1,
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayerCounts {
    pub layer: String,
    #[serde(flatten)]
    pub counts: VerdictCounts,
}

/// The `/analytics/context-fit` answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Analytics {
    pub days: u32,
    pub nodes: u32,
    pub by_verdict: VerdictCounts,
    pub by_layer: Vec<LayerCounts>,
    pub recent: Vec<Row>,
}

/// How many recent cards the analytics answer lists.
const RECENT_LIMIT: i64 = 50;

/// The cards computed in the last `days` days, counted and listed.
pub async fn analytics(pool: &SqlitePool, days: u32) -> Result<Analytics, sqlx::Error> {
    let since = (chrono::Utc::now() - chrono::Duration::days(i64::from(days))).to_rfc3339();
    let all: Vec<(String, String)> =
        sqlx::query_as("SELECT layer, verdicts FROM node_context_fit WHERE computed_at >= ?")
            .bind(&since)
            .fetch_all(pool)
            .await?;
    let mut by_verdict = VerdictCounts::default();
    let mut layers: BTreeMap<String, VerdictCounts> = BTreeMap::new();
    for (layer, verdicts) in &all {
        let verdicts: Vec<String> = serde_json::from_str(verdicts).unwrap_or_default();
        by_verdict.add(&verdicts);
        layers.entry(layer.clone()).or_default().add(&verdicts);
    }
    let recent: Vec<RawRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ROW_COLUMNS} FROM node_context_fit WHERE computed_at >= ? \
         ORDER BY computed_at DESC, node_id LIMIT ?"
    )))
    .bind(&since)
    .bind(RECENT_LIMIT)
    .fetch_all(pool)
    .await?;
    Ok(Analytics {
        days,
        nodes: all.len() as u32,
        by_verdict,
        by_layer: layers
            .into_iter()
            .map(|(layer, counts)| LayerCounts { layer, counts })
            .collect(),
        recent: recent.into_iter().map(row_of).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_nodes::{CloseArgs, NodeLimits, OpenArgs};

    fn owned(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn the_layer_is_the_commonest_first_segment() {
        assert_eq!(
            layer_of(&owned(&["core/src/a.rs", "core/src/b.rs"])),
            "core"
        );
        assert_eq!(
            layer_of(&owned(&["core/src/a.rs", "shell/x.ts", "shell/y.ts"])),
            "shell"
        );
        assert_eq!(layer_of(&owned(&["shell/a", "core/b"])), "core");
        assert_eq!(layer_of(&owned(&["./sidecars\\echo/main.go"])), "sidecars");
        assert_eq!(layer_of(&owned(&["**/*.rs"])), "none");
        assert_eq!(layer_of(&[]), "none");
    }

    #[test]
    fn a_transcript_path_matches_the_given_by_a_trailing_run_of_segments() {
        let given = owned(&["core/src/a.rs", "shell/src/**"]);
        assert!(path_is_given("C:\\work\\tree\\core\\src\\a.rs", &given));
        assert!(path_is_given("/w/tree/shell/src/ui/x.ts", &given));
        assert!(!path_is_given("/w/tree/core/src/other.rs", &given));
    }

    fn attempt(reads: &[&str], edits: &[&str]) -> (String, String) {
        (
            serde_json::json!(reads).to_string(),
            serde_json::json!(edits).to_string(),
        )
    }

    #[test]
    fn reads_before_the_first_edit_count_outside_the_given_and_pointers_read_count_all() {
        let owned_paths = owned(&["core/src/a.rs"]);
        let pointers = owned(&["core/src/p.rs", "core/src/q.rs"]);
        let attempts = vec![
            attempt(
                &["/w/core/src/x.rs", "/w/core/src/x.rs", "/w/core/src/p.rs"],
                &[],
            ),
            attempt(&["/w/core/src/y.rs"], &["/w/core/src/a.rs"]),
            attempt(&["/w/core/src/z.rs"], &[]),
        ];
        let (outside, read) = read_signals(&attempts, &owned_paths, &pointers);
        assert_eq!(
            outside,
            Some(2),
            "x once and y; z comes after the first edit"
        );
        assert_eq!(read, Some(1));
        assert_eq!(read_signals(&[], &owned_paths, &pointers), (None, None));
        assert_eq!(read_signals(&attempts, &[], &[]), (None, None));
    }

    async fn pool_with_leaf(paths: &[&str], pointers: &[&str]) -> (SqlitePool, String, String) {
        let pool = crate::testdb::fresh_pool().await;
        let root = crate::work_nodes::open(
            &pool,
            None,
            &NodeLimits::default(),
            &OpenArgs {
                kind: "root".into(),
                title: "Root".into(),
                goal: "g".into(),
                session_id: Some("s-fit".into()),
                ..OpenArgs::default()
            },
        )
        .await
        .unwrap();
        let leaf = crate::work_nodes::open(
            &pool,
            None,
            &NodeLimits::default(),
            &OpenArgs {
                parent_id: Some(root.id.clone()),
                kind: "leaf".into(),
                title: "Leaf".into(),
                goal: "g".into(),
                owned_paths: owned(paths),
                pointers: owned(pointers),
                budget_tokens: Some(1000),
                ..OpenArgs::default()
            },
        )
        .await
        .unwrap();
        (pool, root.id, leaf.id)
    }

    async fn close_leaf(pool: &SqlitePool, id: &str, status: &str) {
        let file = tempfile::NamedTempFile::new().unwrap();
        crate::work_nodes::close(
            pool,
            id,
            &NodeLimits::default(),
            &CloseArgs {
                status: status.into(),
                summary: "done it".into(),
                handoff_path: file.path().to_str().unwrap().to_owned(),
            },
        )
        .await
        .unwrap();
    }

    async fn feed_lines(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'work_tree_context_fit'")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn refuse_fence(pool: &SqlitePool, node: &str, run: i64) {
        sqlx::query("INSERT INTO work_node_runs (node_id, run_id) VALUES (?, ?)")
            .bind(node)
            .bind(run)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO shadow_decisions (run_id, tool_name, tool_input, decision, reason, \
             action_class, classifier_version, created_at) \
             VALUES (?, 'Edit', '{}', 'deny', 'node_scope', 'node-scope', 1, '2026-11-12T00:00:00+00:00')",
        )
        .bind(run)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn closing_a_clean_node_stores_a_fit_row_and_no_feed_line() {
        let (pool, root, leaf) = pool_with_leaf(&["core/src/a.rs"], &[]).await;

        close_leaf(&pool, &leaf, "done").await;

        let rows = tree_rows(&pool, &root).await.unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].node_id, leaf);
        assert_eq!(rows[0].layer, "core");
        assert!(rows[0].verdicts.is_empty(), "{rows:?}");
        assert_eq!(rows[0].rules_version, RULES_VERSION);
        assert!(feed_lines(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn closing_a_node_with_a_fence_refusal_stores_under_and_one_feed_line() {
        let (pool, root, leaf) = pool_with_leaf(&["core/src/a.rs"], &[]).await;
        refuse_fence(&pool, &leaf, 42).await;

        close_leaf(&pool, &leaf, "done").await;

        let rows = tree_rows(&pool, &root).await.unwrap();
        assert_eq!(rows[0].verdicts, vec!["under".to_owned()]);
        assert_eq!(rows[0].evidence.len(), 1);
        assert_eq!(rows[0].evidence[0].signal, "u2");
        assert_eq!(rows[0].evidence[0].value, 1.0);
        assert_eq!(rows[0].used["fence_refusals"], 1);
        assert_eq!(rows[0].given["owned_paths"], 1);
        assert_eq!(feed_lines(&pool).await, vec!["Leaf: under (u2)".to_owned()]);
    }

    #[tokio::test]
    async fn a_second_computation_keeps_one_row_and_a_new_rules_version_recomputes() {
        let (pool, root, leaf) = pool_with_leaf(&["core/src/a.rs"], &[]).await;
        close_leaf(&pool, &leaf, "done").await;
        let node = crate::work_nodes::get(&pool, &leaf).await.unwrap();

        compute(&pool, &node).await.unwrap();
        compute(&pool, &node).await.unwrap();
        assert_eq!(tree_rows(&pool, &root).await.unwrap().len(), 1);

        // Only closed nodes are recomputed: the root is still open, the leaf is current.
        assert_eq!(recompute_outdated(&pool, 10).await, 0);
        sqlx::query("UPDATE node_context_fit SET rules_version = 0")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(recompute_outdated(&pool, 10).await, 1);
        assert_eq!(recompute_outdated(&pool, 10).await, 0);
        let rows = tree_rows(&pool, &root).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows.iter().all(|r| r.rules_version == RULES_VERSION),
            "{rows:?}"
        );
    }

    #[tokio::test]
    async fn a_node_with_no_source_rows_abstains_instead_of_failing() {
        let (pool, _root, leaf) = pool_with_leaf(&["core/src/a.rs"], &["core/src/p.rs"]).await;
        let node = crate::work_nodes::get(&pool, &leaf).await.unwrap();

        let inputs = gather(&pool, &node, &Thresholds::default()).await;

        assert_eq!(inputs.outside_reads_before_edit, None);
        assert_eq!(inputs.pointers_read, None);
        assert_eq!(inputs.tools_given, None);
        assert_eq!(inputs.layer_p90, None);
        assert!(assess(&inputs, &Thresholds::default()).is_fit());
    }

    #[tokio::test]
    async fn analytics_counts_verdicts_by_layer_and_lists_the_newest_first() {
        let (pool, root, leaf) = pool_with_leaf(&["core/src/a.rs"], &[]).await;
        refuse_fence(&pool, &leaf, 7).await;
        close_leaf(&pool, &leaf, "done").await;
        let node = crate::work_nodes::get(&pool, &root).await.unwrap();
        compute(&pool, &node).await.unwrap();

        let report = analytics(&pool, 7).await.unwrap();

        assert_eq!(report.days, 7);
        assert_eq!(report.nodes, 2);
        assert_eq!(
            report.by_verdict,
            VerdictCounts {
                under: 1,
                over: 0,
                split: 0,
                fit: 1
            }
        );
        let layers: Vec<_> = report.by_layer.iter().map(|l| l.layer.as_str()).collect();
        assert_eq!(layers, vec!["core", "none"]);
        assert_eq!(report.recent.len(), 2);
        assert_eq!(report.recent[0].node_id, root, "the root was computed last");
        // A window that excludes everything counts nothing.
        sqlx::query("UPDATE node_context_fit SET computed_at = '2000-01-01T00:00:00+00:00'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(analytics(&pool, 7).await.unwrap().nodes, 0);
    }

    #[tokio::test]
    async fn the_layer_percentile_comes_from_stored_cards_in_the_window() {
        let (pool, _root, leaf) = pool_with_leaf(&["core/src/a.rs"], &[]).await;
        for i in 1..=10 {
            sqlx::query(
                "INSERT INTO node_context_fit (node_id, tree_id, layer, used, rules_version, computed_at) \
                 VALUES (?, 't', 'core', ?, 1, ?)",
            )
            .bind(format!("old-{i}"))
            .bind(format!("{{\"tokens_per_line\": {i}.0}}"))
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(&pool)
            .await
            .unwrap();
        }
        let node = crate::work_nodes::get(&pool, &leaf).await.unwrap();

        let inputs = gather(&pool, &node, &Thresholds::default()).await;

        assert_eq!(inputs.layer_nodes, 10);
        assert_eq!(inputs.layer_p90, Some(9.0));
    }
}
