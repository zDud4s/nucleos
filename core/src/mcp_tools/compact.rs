//! Compact-by-default answers for the MCP tools whose full output is large (`tree_status`,
//! `get_run`, and `node_tree` after them). `detail: "full"` always bypasses all of this.
//!
//! The reusable part is [`cap_value`] / [`compact_value`]: they bring a JSON value under a
//! character cap and attach a `truncated` note saying what was dropped and how to get more.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// The cap on a summary answer, in characters of its JSON text.
pub const DEFAULT_CAP: usize = 8000;
/// Room kept free under the cap for the `truncated` note.
const NOTE_RESERVE: usize = 700;
/// Longest `dropped` list the note spells out.
const MAX_NOTE_ITEMS: usize = 6;
/// The hint a `tree_status` / `get_run` note gives for getting the whole answer.
pub const FULL_HINT: &str = "call again with detail:\"full\" for the complete answer";

/// How much a tool answers: a bounded summary (the default) or everything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Detail {
    #[default]
    Summary,
    Full,
}

impl Detail {
    /// `None` and `"summary"` are the summary; `"full"` is everything; anything else is refused.
    pub fn parse(raw: Option<&str>) -> Result<Detail, String> {
        match raw {
            None | Some("summary") => Ok(Detail::Summary),
            Some("full") => Ok(Detail::Full),
            Some(other) => Err(format!(
                "detail must be \"summary\" or \"full\", not {other:?}"
            )),
        }
    }
}

/// `text` cut to `max` characters, with how much was cut said at the end.
pub fn truncate_text(text: &str, max: usize) -> String {
    let total = text.chars().count();
    if total <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}… [+{} chars]", total - max)
}

fn size(value: &Value) -> usize {
    value.to_string().chars().count()
}

#[derive(Default)]
struct Cuts {
    strings: usize,
    arrays: BTreeMap<String, usize>,
    removed: Vec<String>,
}

fn shrink(value: &mut Value, str_max: usize, arr_max: usize, path: &str, cuts: &mut Cuts) {
    match value {
        Value::String(text) if text.chars().count() > str_max => {
            *text = truncate_text(text, str_max);
            cuts.strings += 1;
        }
        Value::Array(items) => {
            if items.len() > arr_max {
                *bump(&mut cuts.arrays, path) += items.len() - arr_max;
                items.truncate(arr_max);
            }
            for item in items {
                shrink(item, str_max, arr_max, path, cuts);
            }
        }
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                shrink(child, str_max, arr_max, &format!("{path}.{key}"), cuts);
            }
        }
        _ => {}
    }
}

fn bump<'a>(map: &'a mut BTreeMap<String, usize>, path: &str) -> &'a mut usize {
    map.entry(path.to_owned()).or_insert(0)
}

/// Drops the tail of the biggest array, or the biggest key, once. False when nothing is left.
fn trim_once(value: &mut Value, path: &str, cuts: &mut Cuts) -> bool {
    match value {
        Value::Array(items) => {
            if items.is_empty() {
                return false;
            }
            let drop = if items.len() > 8 { items.len() / 4 } else { 1 };
            let keep = items.len() - drop;
            items.truncate(keep);
            *bump(&mut cuts.arrays, path) += drop;
            true
        }
        Value::Object(map) => {
            let Some(biggest) = map
                .iter()
                .max_by_key(|(_, child)| size(child))
                .map(|(key, _)| key.clone())
            else {
                return false;
            };
            let child_path = format!("{path}.{biggest}");
            if let Some(child) = map.get_mut(&biggest) {
                if trim_once(child, &child_path, cuts) {
                    return true;
                }
            }
            map.remove(&biggest);
            cuts.removed.push(child_path);
            true
        }
        _ => false,
    }
}

/// `value` with long strings cut to `str_max` characters and arrays to `arr_max` items, then
/// brought under `cap` characters, with a `truncated` note when anything was cut. A value that
/// needed nothing comes back untouched.
pub fn compact_value(
    mut value: Value,
    str_max: usize,
    arr_max: usize,
    cap: usize,
    how_to_get_more: &str,
) -> Value {
    let mut cuts = Cuts::default();
    shrink(&mut value, str_max, arr_max, "$", &mut cuts);
    let budget = cap.saturating_sub(NOTE_RESERVE);
    for limit in [1000, 300, 120, 60] {
        if size(&value) <= budget {
            break;
        }
        shrink(&mut value, limit.min(str_max), arr_max, "$", &mut cuts);
    }
    while size(&value) > budget && trim_once(&mut value, "$", &mut cuts) {}

    if cuts.strings == 0 && cuts.arrays.is_empty() && cuts.removed.is_empty() {
        return value;
    }
    let mut dropped: Vec<String> = cuts
        .arrays
        .iter()
        .map(|(path, n)| format!("{path}: {n} more items"))
        .chain(cuts.removed.iter().map(|path| format!("{path}: removed")))
        .collect();
    let more = dropped.len().saturating_sub(MAX_NOTE_ITEMS);
    dropped.truncate(MAX_NOTE_ITEMS);
    if more > 0 {
        dropped.push(format!("and {more} more cuts"));
    }
    let note = json!({
        "cut_texts": cuts.strings,
        "dropped": dropped,
        "how_to_get_more": how_to_get_more,
    });
    let mut out = match value {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("value".to_owned(), other);
            map
        }
    };
    out.insert("truncated".to_owned(), note.clone());
    let out = Value::Object(out);
    if size(&out) <= cap {
        return out;
    }
    json!({
        "truncated": note,
        "preview": truncate_text(&out.to_string(), cap / 2),
    })
}

/// [`compact_value`] with no per-string or per-array limit: only the overall cap.
pub fn cap_value(value: Value, cap: usize, how_to_get_more: &str) -> Value {
    compact_value(value, usize::MAX, usize::MAX, cap, how_to_get_more)
}

// ----- tree_status -----

/// Branch states that are over: such a branch is counted, not listed.
const FINISHED: &[&str] = &["merged", "dropped", "done", "landed", "abandoned"];

fn text_of(value: Option<&Value>, max: usize) -> Option<String> {
    match value? {
        Value::Null => None,
        Value::String(text) => Some(truncate_text(text, max)),
        other => Some(truncate_text(&other.to_string(), max)),
    }
}

fn last_of(branch: &Value, key: &str) -> Value {
    match branch
        .get(key)
        .and_then(Value::as_array)
        .and_then(|a| a.last())
    {
        Some(last) => compact_value(last.clone(), 200, 4, 1500, FULL_HINT),
        None => Value::Null,
    }
}

/// Why a branch is in the state it is in: the question it is parked on, or why it was dropped.
fn reason_of(branch: &Value) -> Option<String> {
    let park = branch.get("park").filter(|park| !park.is_null());
    text_of(park.and_then(|p| p.get("question")), 300)
        .or_else(|| text_of(park.and_then(|p| p.get("reason")), 300))
        .or_else(|| text_of(branch.get("drop_reason"), 300))
        .or_else(|| text_of(park, 300))
}

fn branch_entry(name: &str, branch: &Value, nodes: &[Value], deep: bool) -> Value {
    let mut entry = Map::new();
    entry.insert("branch".to_owned(), json!(name));
    for key in ["node_id", "state", "parent", "layer"] {
        if let Some(value) = branch.get(key).filter(|value| !value.is_null()) {
            entry.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(reason) = reason_of(branch) {
        entry.insert("reason".to_owned(), json!(reason));
    }
    entry.insert("last_gate".to_owned(), last_of(branch, "gates"));
    entry.insert("last_review".to_owned(), last_of(branch, "reviews"));
    if let Some(object) = branch.as_object() {
        for (key, value) in object {
            if key.ends_with("_at") && !value.is_null() {
                entry.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(node) = nodes
        .iter()
        .find(|node| node.get("branch").and_then(Value::as_str) == Some(name))
    {
        for key in ["kind", "depth"] {
            if let Some(value) = node.get(key) {
                entry.insert(key.to_owned(), value.clone());
            }
        }
        if let Some(reason) = text_of(node.get("descent").and_then(|d| d.get("reason")), 200) {
            entry.insert("descent_reason".to_owned(), json!(reason));
        }
        if node
            .get("degraded")
            .is_some_and(|d| !d.is_null() && d != &json!(false))
        {
            entry.insert("degraded".to_owned(), node["degraded"].clone());
        }
    }
    if deep {
        entry.insert(
            "gates".to_owned(),
            compact_value(
                branch.get("gates").cloned().unwrap_or(json!([])),
                300,
                5,
                3000,
                FULL_HINT,
            ),
        );
    }
    Value::Object(entry)
}

/// The bounded view of `GET /trees/{id}`: ids, states, counts per state, every unfinished branch
/// with its reason and latest gate/review, finished branches collapsed to a count. With `node_id`
/// only the branch bound to that node (or named so) is answered, finished or not.
pub fn summarize_tree(status: Value, node_id: Option<&str>) -> Value {
    let nodes: Vec<Value> = status
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let branches = status
        .get("manifest")
        .and_then(|m| m.get("branches"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut live = Vec::new();
    let mut finished = 0usize;
    let mut picked = Vec::new();
    for (name, branch) in &branches {
        let state = branch.get("state").and_then(Value::as_str).unwrap_or("?");
        *counts.entry(state.to_owned()).or_insert(0) += 1;
        if let Some(wanted) = node_id {
            let bound = branch.get("node_id").and_then(Value::as_str) == Some(wanted);
            if bound || name == wanted {
                picked.push(branch_entry(name, branch, &nodes, true));
            }
        } else if FINISHED.contains(&state) {
            finished += 1;
        } else {
            live.push(branch_entry(name, branch, &nodes, false));
        }
    }

    let mut out = Map::new();
    out.insert("detail".to_owned(), json!("summary"));
    for key in [
        "id",
        "root_node_id",
        "project_id",
        "feature_branch",
        "state",
        "created_at",
        "updated_at",
    ] {
        if let Some(value) = status.get(key).filter(|value| !value.is_null()) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(goal) = text_of(status.get("goal"), 400) {
        out.insert("goal".to_owned(), json!(goal));
    }
    if let Some(failure) = text_of(status.get("root_start_failure"), 300) {
        out.insert("root_start_failure".to_owned(), json!(failure));
    }
    out.insert("counts".to_owned(), json!(counts));
    if node_id.is_some() {
        out.insert("node_filter".to_owned(), json!(node_id));
        out.insert("branches".to_owned(), Value::Array(picked));
    } else {
        out.insert("branches".to_owned(), Value::Array(live));
        out.insert("finished_collapsed".to_owned(), json!(finished));
    }
    let decisions: Vec<Value> = status
        .get("open_decisions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|d| {
                    json!({
                        "id": d.get("id"),
                        "branch": d.get("branch"),
                        "node_id": d.get("node_id"),
                        "question": text_of(d.get("question"), 300),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    out.insert("open_decisions".to_owned(), Value::Array(decisions));
    if let Some(refusals) = status.get("start_refusals").and_then(Value::as_array) {
        let recent: Vec<Value> = refusals
            .iter()
            .rev()
            .take(3)
            .map(|r| compact_value(r.clone(), 200, 4, 1200, FULL_HINT))
            .collect();
        out.insert(
            "start_refusals".to_owned(),
            json!({"count": refusals.len(), "latest": recent}),
        );
    }
    cap_value(Value::Object(out), DEFAULT_CAP, FULL_HINT)
}

// ----- get_run -----

/// The bounded view of a run: scalars and timestamps kept, long text (logs, transcripts, output)
/// cut, long lists cut to their head.
pub fn summarize_run(run: Value) -> Value {
    let mut out = compact_value(run, 500, 10, DEFAULT_CAP, FULL_HINT);
    if let Value::Object(map) = &mut out {
        map.insert("detail".to_owned(), json!("summary"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_value_comes_back_untouched() {
        let value = json!({"id": 1, "status": "done"});
        assert_eq!(cap_value(value.clone(), DEFAULT_CAP, FULL_HINT), value);
    }

    #[test]
    fn a_huge_value_is_capped_with_a_note_saying_how_to_get_more() {
        let items: Vec<Value> = (0..2000)
            .map(|i| json!({"n": i, "text": "x".repeat(200)}))
            .collect();
        let out = cap_value(json!({"id": 7, "items": items}), DEFAULT_CAP, FULL_HINT);
        assert!(size(&out) <= DEFAULT_CAP, "was {}", size(&out));
        assert_eq!(out["id"], json!(7));
        let note = &out["truncated"];
        assert!(note["how_to_get_more"].as_str().unwrap().contains("full"));
        assert!(!note["dropped"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_non_object_root_is_wrapped_to_carry_the_note() {
        let out = cap_value(
            Value::Array(vec![json!("y".repeat(100)); 400]),
            2000,
            FULL_HINT,
        );
        assert!(size(&out) <= 2000);
        assert!(out.get("truncated").is_some());
    }

    #[test]
    fn detail_parses_summary_full_and_refuses_the_rest() {
        assert_eq!(Detail::parse(None), Ok(Detail::Summary));
        assert_eq!(Detail::parse(Some("summary")), Ok(Detail::Summary));
        assert_eq!(Detail::parse(Some("full")), Ok(Detail::Full));
        assert!(Detail::parse(Some("everything")).is_err());
    }

    fn big_tree() -> Value {
        let mut branches = Map::new();
        for i in 0..60 {
            branches.insert(
                format!("done-{i}"),
                json!({"name": format!("done-{i}"), "state": "merged", "node_id": format!("n{i}"),
                       "gates": [{"log": "g".repeat(500)}], "merged_at": "t"}),
            );
        }
        branches.insert(
            "stuck".to_owned(),
            json!({"name": "stuck", "state": "parked", "node_id": "nx",
                   "park": {"question": "q".repeat(2000)},
                   "gates": [{"log": "old"}, {"log": "new".repeat(900)}]}),
        );
        json!({
            "id": "tree-1", "state": "running", "goal": "g".repeat(5000),
            "manifest": {"branches": branches},
            "nodes": [{"branch": "stuck", "kind": "leaf", "depth": 1}],
            "open_decisions": [], "start_refusals": [],
        })
    }

    #[test]
    fn tree_summary_collapses_finished_branches_and_keeps_the_stuck_one() {
        let out = summarize_tree(big_tree(), None);
        assert!(size(&out) <= DEFAULT_CAP);
        assert_eq!(out["finished_collapsed"], json!(60));
        assert_eq!(out["counts"]["merged"], json!(60));
        let branches = out["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0]["branch"], json!("stuck"));
        assert_eq!(branches[0]["kind"], json!("leaf"));
        assert!(branches[0]["reason"].as_str().unwrap().contains("chars]"));
        assert!(out["goal"].as_str().unwrap().len() < 600);
    }

    #[test]
    fn tree_summary_filters_to_one_node_even_when_finished() {
        let out = summarize_tree(big_tree(), Some("n3"));
        let branches = out["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0]["branch"], json!("done-3"));
        assert!(out.get("finished_collapsed").is_none());
    }

    #[test]
    fn run_summary_cuts_logs_and_keeps_the_scalars() {
        let run = json!({"id": 5, "status": "failed", "created_at": "t0",
                         "log": "L".repeat(50_000), "events": vec![json!({"a": 1}); 100]});
        let out = summarize_run(run.clone());
        assert!(size(&out) <= DEFAULT_CAP);
        assert_eq!(out["id"], json!(5));
        assert_eq!(out["status"], json!("failed"));
        assert_eq!(out["events"].as_array().unwrap().len(), 10);
        assert!(out["truncated"]["how_to_get_more"].is_string());
        assert_eq!(out["detail"], json!("summary"));
    }
}
