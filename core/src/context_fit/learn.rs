//! Closing the loop of the context-fit card (spec 2026-11-12, section 6, items 1 and 2): what the
//! cards of past nodes say about the *next* node's context.
//!
//! * **Learned pointers.** Per layer, a file read outside the given set in at least
//!   [`MIN_NODES`] nodes is suggested as a pointer; a pointer never read in at least [`MIN_NODES`]
//!   nodes is suggested for removal. `owned_paths` is never touched: widening it stays the parent's
//!   decision.
//! * **Budget.** Per layer, the share of `split` nodes and of fits that peaked under 30% of their
//!   ceiling suggests moving `leaf_budget` down or up. Never applied by this module.
//!
//! Shadow first (spec 6, criterion 10): the two `devtime_action_state` pairs
//! (`context_fit`, `pointers`) and (`context_fit`, `budget`) default to `shadow`, where each
//! suggestion is only logged in `devtime_shadow` and nothing reaches a node. `live` makes the
//! pointers join a new node's list ([`with_learned`]) and the budget a `proposals` row of kind
//! `context-fit-budget` for the owner. The pure half ([`suggest_pointers`], [`propose_budget`])
//! reads [`Sample`]s; the IO half reads `node_context_fit` and is fail-open: [`run`] is called
//! from `store::record_close` and never errors the close.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::store::layer_of;
use crate::work_nodes::{self, Node};

/// `devtime_shadow.action` and `devtime_action_state.action` of this module.
pub const ACTION: &str = "context_fit";
/// The rule id of the pointer suggestions.
pub const POINTERS_RULE: &str = "pointers";
/// The rule id of the budget proposal.
pub const BUDGET_RULE: &str = "budget";
/// `proposals.kind` of a live budget proposal.
pub const BUDGET_KIND: &str = "context-fit-budget";

/// Nodes that must agree before a pointer is suggested or retired (spec 6.1).
pub const MIN_NODES: usize = 3;
/// Cards a layer needs before its budget is judged.
const BUDGET_MIN_NODES: usize = 10;
/// Share of `split` cards that proposes a smaller budget.
const SPLIT_SHARE: f64 = 0.3;
/// Share of fits under [`SMALL_PEAK_PERCENT`] of the ceiling that proposes a larger budget.
const SMALL_FIT_SHARE: f64 = 0.5;
/// A fit whose peak stays under this percent of its ceiling used little of it (spec 6.2).
const SMALL_PEAK_PERCENT: i64 = 30;
/// The proposed budget as a percent of the current one, down and up.
const STEP_DOWN_PERCENT: i64 = 75;
const STEP_UP_PERCENT: i64 = 125;
/// Cards older than this are not looked at.
const WINDOW_DAYS: i64 = 30;
/// At most this many cards per layer are read.
const SAMPLE_LIMIT: i64 = 300;
/// At most this many learned pointers are appended to one node.
const MAX_LEARNED: usize = 5;

// ---------------------------------------------------------------------------------------------
// Pure half
// ---------------------------------------------------------------------------------------------

/// What one stored card says, for one layer's worth of grouping.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub verdicts: Vec<String>,
    /// Repo-relative files read outside the given set before the first edit.
    pub outside: Vec<String>,
    /// The pointers the node read, and those it never opened.
    pub read: Vec<String>,
    pub unread: Vec<String>,
    pub peak: Option<i64>,
    pub ceiling: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Add,
    Remove,
}

impl Change {
    fn as_str(self) -> &'static str {
        match self {
            Change::Add => "add",
            Change::Remove => "remove",
        }
    }
}

/// One pointer to add to, or retire from, the nodes of a layer, and how many nodes back it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub change: Change,
    pub path: String,
    pub nodes: usize,
}

fn distinct(paths: &[String]) -> BTreeSet<&str> {
    paths.iter().map(String::as_str).collect()
}

/// The pointer changes the samples of one layer support. Pure; never mentions `owned_paths`.
pub fn suggest_pointers(samples: &[Sample]) -> Vec<Suggestion> {
    let mut outside: BTreeMap<&str, usize> = BTreeMap::new();
    let mut unread: BTreeMap<&str, usize> = BTreeMap::new();
    let mut ever_read: BTreeSet<&str> = BTreeSet::new();
    for sample in samples {
        for path in distinct(&sample.outside) {
            *outside.entry(path).or_default() += 1;
        }
        for path in distinct(&sample.unread) {
            *unread.entry(path).or_default() += 1;
        }
        ever_read.extend(sample.read.iter().map(String::as_str));
    }
    let mut out: Vec<Suggestion> = outside
        .into_iter()
        .filter(|(_, nodes)| *nodes >= MIN_NODES)
        .map(|(path, nodes)| Suggestion {
            change: Change::Add,
            path: path.to_owned(),
            nodes,
        })
        .chain(
            unread
                .into_iter()
                .filter(|(path, nodes)| *nodes >= MIN_NODES && !ever_read.contains(path))
                .map(|(path, nodes)| Suggestion {
                    change: Change::Remove,
                    path: path.to_owned(),
                    nodes,
                }),
        )
        .collect();
    out.sort_by(|a, b| {
        b.nodes
            .cmp(&a.nodes)
            .then(a.change.cmp(&b.change))
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// A `leaf_budget` adjustment for one layer, with the counts behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub current: i64,
    pub proposed: i64,
    pub nodes: usize,
    pub split: usize,
    pub over: usize,
    /// Fits whose peak stayed under [`SMALL_PEAK_PERCENT`] of their ceiling.
    pub small_fit: usize,
}

fn rounded(tokens: i64) -> i64 {
    ((tokens + 500) / 1000 * 1000).max(1000)
}

/// The budget the samples of one layer argue for, or `None` when they are too few or agree with
/// the current one. Down with many `split`, up with many fits that used under 30% of the ceiling.
pub fn propose_budget(samples: &[Sample]) -> Option<Budget> {
    if samples.len() < BUDGET_MIN_NODES {
        return None;
    }
    let mut ceilings: BTreeMap<i64, usize> = BTreeMap::new();
    for ceiling in samples.iter().filter_map(|s| s.ceiling).filter(|c| *c > 0) {
        *ceilings.entry(ceiling).or_default() += 1;
    }
    let current = ceilings
        .into_iter()
        .max_by_key(|(ceiling, count)| (*count, *ceiling))
        .map(|(ceiling, _)| ceiling)?;
    let has = |s: &Sample, verdict: &str| s.verdicts.iter().any(|v| v == verdict);
    let split = samples.iter().filter(|s| has(s, "split")).count();
    let over = samples.iter().filter(|s| has(s, "over")).count();
    let small_fit = samples
        .iter()
        .filter(|s| s.verdicts.is_empty())
        .filter(|s| match (s.peak, s.ceiling) {
            (Some(peak), Some(ceiling)) if ceiling > 0 => peak * 100 < ceiling * SMALL_PEAK_PERCENT,
            _ => false,
        })
        .count();
    let n = samples.len() as f64;
    let percent = if split as f64 / n >= SPLIT_SHARE {
        STEP_DOWN_PERCENT
    } else if small_fit as f64 / n >= SMALL_FIT_SHARE {
        STEP_UP_PERCENT
    } else {
        return None;
    };
    let proposed = rounded(current * percent / 100);
    (proposed != current).then_some(Budget {
        current,
        proposed,
        nodes: samples.len(),
        split,
        over,
        small_fit,
    })
}

// ---------------------------------------------------------------------------------------------
// IO half
// ---------------------------------------------------------------------------------------------

/// A pair's mode in `devtime_action_state`; a missing row is `Shadow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Shadow,
    Live,
}

/// The mode of `(context_fit, rule)`. A read error is `Off`: no suggestion without knowing it
/// may be made.
pub async fn mode(pool: &SqlitePool, rule: &str) -> Mode {
    match sqlx::query_scalar::<_, String>(
        "SELECT mode FROM devtime_action_state WHERE action = ? AND rule_id = ?",
    )
    .bind(ACTION)
    .bind(rule)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(mode)) => match mode.as_str() {
            "live" => Mode::Live,
            "off" => Mode::Off,
            _ => Mode::Shadow,
        },
        Ok(None) => Mode::Shadow,
        Err(error) => {
            tracing::debug!(%error, "context fit learn: the action state is unreadable");
            Mode::Off
        }
    }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The recent cards of `layer`, newest first.
async fn samples_of(pool: &SqlitePool, layer: &str) -> Vec<Sample> {
    let since = (chrono::Utc::now() - chrono::Duration::days(WINDOW_DAYS)).to_rfc3339();
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT verdicts, given, used FROM node_context_fit \
         WHERE layer = ? AND computed_at >= ? ORDER BY computed_at DESC LIMIT ?",
    )
    .bind(layer)
    .bind(&since)
    .bind(SAMPLE_LIMIT)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.into_iter()
        .map(|(verdicts, given, used)| {
            let given: Value = serde_json::from_str(&given).unwrap_or(Value::Null);
            let used: Value = serde_json::from_str(&used).unwrap_or(Value::Null);
            Sample {
                verdicts: serde_json::from_str(&verdicts).unwrap_or_default(),
                outside: strings(&used["outside_paths"]),
                read: strings(&used["pointers_read_paths"]),
                unread: strings(&used["pointers_unread_paths"]),
                peak: used["peak"].as_i64(),
                ceiling: given["ceiling"].as_i64(),
            }
        })
        .collect()
}

/// Logs one suggestion in `devtime_shadow`. The unique key makes a repeat a no-op. The row is
/// written `resolved`: nothing is observed for it, so the outcome sweeper must not expire it.
async fn log_shadow(
    pool: &SqlitePool,
    rule: &str,
    project_id: &str,
    subject_key: &str,
    params: &Value,
) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    sqlx::query(
        "INSERT INTO devtime_shadow (action, rule_id, project_id, subject_key, mode, decision, \
         template_id, params, rule_version, action_version, thresholds_hash, rules_version, \
         decided_at, window_end_at, outcome_state, resolved_at) \
         VALUES (?, ?, ?, ?, 'shadow', 'fire', ?, ?, 1, 1, '', ?, ?, ?, 'resolved', ?) \
         ON CONFLICT DO NOTHING",
    )
    .bind(ACTION)
    .bind(rule)
    .bind(project_id)
    .bind(subject_key)
    .bind(format!("{ACTION}.{rule}.v1"))
    .bind(params.to_string())
    .bind(super::RULES_VERSION.to_string())
    .bind(&now)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Files the owner's proposal for a layer's budget unless one is already pending for it.
async fn file_budget_proposal(
    pool: &SqlitePool,
    project_id: Option<&str>,
    layer: &str,
    budget: &Budget,
) -> Result<(), sqlx::Error> {
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM proposals WHERE kind = ? AND status = 'pending' \
         AND json_extract(tool_input, '$.layer') = ?",
    )
    .bind(BUDGET_KIND)
    .bind(layer)
    .fetch_one(pool)
    .await?;
    if open > 0 {
        return Ok(());
    }
    let payload = budget_payload(layer, budget);
    let reasoning = format!(
        "Context fit: of the last {} {layer} nodes, {} were split and {} fit under {SMALL_PEAK_PERCENT}% of \
         the ceiling; proposing leaf_budget {} -> {}.",
        budget.nodes, budget.split, budget.small_fit, budget.current, budget.proposed
    );
    sqlx::query(
        "INSERT INTO proposals (kind, status, project_id, reasoning, tool_input, created_at) \
         VALUES (?, 'pending', ?, ?, ?, ?)",
    )
    .bind(BUDGET_KIND)
    .bind(project_id)
    .bind(reasoning)
    .bind(payload.to_string())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    nucleos_base::wake::bump(nucleos_base::wake::Topic::Proposals);
    Ok(())
}

fn budget_payload(layer: &str, budget: &Budget) -> Value {
    json!({
        "layer": layer,
        "current": budget.current,
        "proposed": budget.proposed,
        "evidence": {
            "nodes": budget.nodes,
            "split": budget.split,
            "over": budget.over,
            "small_fit": budget.small_fit,
        },
    })
}

/// Runs both learners for the layer of the node that just closed. Fail-open: a failure is
/// logged and never reaches the close.
pub async fn run(pool: &SqlitePool, node: &Node) {
    let layer = layer_of(&node.owned_paths);
    if let Err(error) = run_for_layer(pool, &layer, node.project_id.as_deref()).await {
        tracing::warn!(node = %node.id, %error, "context fit learning failed");
    }
}

/// The body of [`run`], for one layer.
pub async fn run_for_layer(
    pool: &SqlitePool,
    layer: &str,
    project_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    if layer == "none" {
        return Ok(());
    }
    let (pointers_mode, budget_mode) = (mode(pool, POINTERS_RULE).await, mode(pool, BUDGET_RULE).await);
    if pointers_mode == Mode::Off && budget_mode == Mode::Off {
        return Ok(());
    }
    let samples = samples_of(pool, layer).await;
    // Live pointers are not recorded: they are read fresh by `with_learned` when a node opens.
    if pointers_mode == Mode::Shadow {
        for suggestion in suggest_pointers(&samples) {
            let key = format!("{layer}|{}|{}", suggestion.change.as_str(), suggestion.path);
            let params = json!({
                "layer": layer,
                "change": suggestion.change.as_str(),
                "path": suggestion.path,
                "nodes": suggestion.nodes,
            });
            log_shadow(pool, POINTERS_RULE, project_id.unwrap_or(""), &key, &params).await?;
        }
    }
    if budget_mode == Mode::Off {
        return Ok(());
    }
    let Some(budget) = propose_budget(&samples) else {
        return Ok(());
    };
    if budget_mode == Mode::Live {
        file_budget_proposal(pool, project_id, layer, &budget).await?;
    } else {
        let key = format!("{layer}|{}->{}", budget.current, budget.proposed);
        let params = budget_payload(layer, &budget);
        log_shadow(pool, BUDGET_RULE, project_id.unwrap_or(""), &key, &params).await?;
    }
    Ok(())
}

/// The pointers a node about to open should get: `given` plus, only when the pointer pair is
/// `live`, the learned `add` suggestions of its layer that are neither already pointers nor inside
/// the owned paths. In shadow (the default) or off, `given` comes back unchanged. Never changes
/// owned paths; never removes a pointer.
pub async fn with_learned(
    pool: &SqlitePool,
    owned_paths: &[String],
    given: Vec<String>,
) -> Vec<String> {
    let layer = layer_of(owned_paths);
    if layer == "none" || mode(pool, POINTERS_RULE).await != Mode::Live {
        return given;
    }
    let mut pointers = given;
    let learned = suggest_pointers(&samples_of(pool, &layer).await)
        .into_iter()
        .filter(|s| s.change == Change::Add)
        .filter(|s| !pointers.contains(&s.path))
        .filter(|s| !owned_paths.iter().any(|glob| work_nodes::path_contained(&s.path, glob)))
        .take(MAX_LEARNED)
        .map(|s| s.path)
        .collect::<Vec<_>>();
    pointers.extend(learned);
    pointers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(outside: &[&str], read: &[&str], unread: &[&str]) -> Sample {
        let own = |xs: &[&str]| xs.iter().map(|x| (*x).to_owned()).collect();
        Sample {
            outside: own(outside),
            read: own(read),
            unread: own(unread),
            ..Sample::default()
        }
    }

    fn shaped(verdict: &[&str], peak: i64) -> Sample {
        Sample {
            verdicts: verdict.iter().map(|v| (*v).to_owned()).collect(),
            peak: Some(peak),
            ceiling: Some(100_000),
            ..Sample::default()
        }
    }

    #[test]
    fn three_nodes_reading_the_same_outside_file_suggest_one_pointer() {
        let samples = vec![
            s(&["core/src/x.rs"], &[], &[]),
            s(&["core/src/x.rs", "core/src/y.rs"], &[], &[]),
            s(&["core/src/x.rs"], &[], &[]),
        ];
        assert_eq!(
            suggest_pointers(&samples),
            vec![Suggestion {
                change: Change::Add,
                path: "core/src/x.rs".into(),
                nodes: 3
            }]
        );
    }

    #[test]
    fn two_nodes_suggest_nothing() {
        let samples = vec![s(&["core/src/x.rs"], &[], &[]), s(&["core/src/x.rs"], &[], &[])];
        assert!(suggest_pointers(&samples).is_empty());
    }

    #[test]
    fn a_pointer_never_read_in_three_nodes_is_suggested_for_removal() {
        let samples = vec![
            s(&[], &[], &["docs/a.md"]),
            s(&[], &[], &["docs/a.md"]),
            s(&[], &[], &["docs/a.md"]),
        ];
        let out = suggest_pointers(&samples);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].change, Change::Remove);
        assert_eq!(out[0].path, "docs/a.md");

        let mut with_a_read = samples;
        with_a_read.push(s(&[], &["docs/a.md"], &[]));
        assert!(suggest_pointers(&with_a_read).is_empty(), "one read keeps it");
    }

    #[test]
    fn the_budget_goes_down_with_many_split_and_up_with_many_small_fits() {
        let mut split: Vec<Sample> = (0..4).map(|_| shaped(&["split"], 90_000)).collect();
        split.extend((0..6).map(|_| shaped(&[], 60_000)));
        let down = propose_budget(&split).expect("4 of 10 split");
        assert_eq!((down.current, down.proposed, down.split), (100_000, 75_000, 4));

        let small: Vec<Sample> = (0..10).map(|_| shaped(&[], 20_000)).collect();
        let up = propose_budget(&small).expect("all small fits");
        assert_eq!((up.proposed, up.small_fit), (125_000, 10));

        assert_eq!(propose_budget(&small[..9]), None, "too few cards");
        let calm: Vec<Sample> = (0..10).map(|_| shaped(&[], 60_000)).collect();
        assert_eq!(propose_budget(&calm), None);
    }

    async fn insert_card(pool: &SqlitePool, node: &str, layer: &str, used: &Value, given: &Value) {
        sqlx::query(
            "INSERT INTO node_context_fit \
             (node_id, tree_id, layer, verdicts, evidence, given, used, rules_version, computed_at) \
             VALUES (?, 't', ?, '[]', '[]', ?, ?, 2, ?)",
        )
        .bind(node)
        .bind(layer)
        .bind(given.to_string())
        .bind(used.to_string())
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn count(pool: &SqlitePool, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    async fn set_live(pool: &SqlitePool, rule: &str) {
        sqlx::query(
            "INSERT INTO devtime_action_state (action, rule_id, mode, since, changed_by) \
             VALUES (?, ?, 'live', 'now', 'owner')",
        )
        .bind(ACTION)
        .bind(rule)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn three_nodes_reading_x(pool: &SqlitePool) {
        for node in ["a", "b", "c"] {
            let used = json!({"outside_paths": ["core/src/x.rs"], "peak": 10_000});
            insert_card(pool, node, "core", &used, &json!({"ceiling": 100_000})).await;
        }
    }

    #[tokio::test]
    async fn shadow_logs_the_suggestion_once_and_injects_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        three_nodes_reading_x(&pool).await;

        run_for_layer(&pool, "core", Some("p")).await.unwrap();
        run_for_layer(&pool, "core", Some("p")).await.unwrap();

        let logged = count(
            &pool,
            "SELECT COUNT(*) FROM devtime_shadow WHERE action = 'context_fit' \
             AND rule_id = 'pointers' AND mode = 'shadow'",
        )
        .await;
        assert_eq!(logged, 1, "deduped by key");
        let owned = vec!["core/src/a/**".to_owned()];
        let given = vec!["docs/a.md".to_owned()];
        assert_eq!(with_learned(&pool, &owned, given.clone()).await, given);
    }

    #[tokio::test]
    async fn live_appends_the_learned_pointer_and_leaves_owned_paths_alone() {
        let pool = crate::testdb::fresh_pool().await;
        three_nodes_reading_x(&pool).await;
        set_live(&pool, POINTERS_RULE).await;

        let owned = vec!["core/src/a/**".to_owned()];
        let got = with_learned(&pool, &owned, vec!["docs/a.md".into()]).await;
        assert_eq!(got, vec!["docs/a.md".to_owned(), "core/src/x.rs".to_owned()]);
        assert_eq!(owned, vec!["core/src/a/**".to_owned()]);

        let inside = vec!["core/src/x.rs".to_owned()];
        assert_eq!(
            with_learned(&pool, &inside, Vec::new()).await,
            Vec::<String>::new(),
            "a path the node already owns is not a pointer"
        );
    }

    #[tokio::test]
    async fn the_budget_proposal_is_filed_once_per_layer_and_never_applied() {
        let pool = crate::testdb::fresh_pool().await;
        for i in 0..10 {
            let used = json!({"peak": 10_000});
            insert_card(&pool, &format!("n{i}"), "core", &used, &json!({"ceiling": 100_000})).await;
        }
        set_live(&pool, BUDGET_RULE).await;

        run_for_layer(&pool, "core", Some("p")).await.unwrap();
        run_for_layer(&pool, "core", Some("p")).await.unwrap();

        let (kind, payload): (String, String) =
            sqlx::query_as("SELECT kind, tool_input FROM proposals")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, BUDGET_KIND);
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["layer"], "core");
        assert_eq!(payload["current"], 100_000);
        assert_eq!(payload["proposed"], 125_000);
        assert_eq!(payload["evidence"]["small_fit"], 10);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM proposals").await, 1);
    }

    #[tokio::test]
    async fn an_off_pair_writes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        three_nodes_reading_x(&pool).await;
        sqlx::query(
            "INSERT INTO devtime_action_state (action, rule_id, mode, since, changed_by) \
             VALUES ('context_fit', 'pointers', 'off', 'now', 'owner'), \
             ('context_fit', 'budget', 'off', 'now', 'owner')",
        )
        .execute(&pool)
        .await
        .unwrap();
        run_for_layer(&pool, "core", None).await.unwrap();
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM devtime_shadow").await, 0);
    }
}
