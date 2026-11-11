//! What `spec_backlog` hands the model: a summary of `GET /backlog` by default, one spec or the
//! full list on request, and a plain "never discovered" when the backlog is empty.
//!
//! The route answers every spec of every project as one array. Served whole, 100+ specs came to
//! one JSON line of ~57k characters, over the MCP client's token ceiling, so the client dumped it
//! to a file instead of showing it. The summary keeps the answer small, and any list it does
//! return goes one spec per line.

use super::*;

/// The hint served instead of `[]`. `GET /backlog` cannot tell "scanned and empty" from "never
/// scanned" (it lists the stored rows and nothing else), and a project with specs on disk reads as
/// empty until its first scan, so an empty backlog is read as never discovered.
pub(super) const NEVER_DISCOVERED_HINT: &str =
    "no spec of this project was discovered yet: run spec_finish or POST /specs/scan";

/// PURE: the `spec_backlog` answer for the backlog the daemon returned and the caller's query.
pub(super) fn backlog_answer(backlog: serde_json::Value, query: &SpecBacklogParams) -> String {
    let specs = match backlog {
        serde_json::Value::Array(specs) => specs,
        // Not the array the route answers: hand it through rather than guess at its shape.
        other => return other.to_string(),
    };
    if specs.is_empty() {
        return serde_json::json!({ "never_discovered": true, "hint": NEVER_DISCOVERED_HINT })
            .to_string();
    }

    if let Some(spec_id) = query.spec_id.as_deref().filter(|id| !id.trim().is_empty()) {
        return match specs.iter().find(|spec| text(spec, "id") == Some(spec_id)) {
            Some(spec) => {
                serde_json::to_string_pretty(spec).unwrap_or_else(|e| error_json(e.to_string()))
            }
            None => error_json(format!("spec {spec_id} is not in the backlog")),
        };
    }

    let wanted = query
        .status
        .as_deref()
        .filter(|status| !status.trim().is_empty());
    let chosen: Vec<&serde_json::Value> = specs
        .iter()
        .filter(|spec| wanted.is_none_or(|status| text(spec, "status") == Some(status)))
        .collect();

    if query.full.unwrap_or(false) {
        return lines(chosen.iter().map(|spec| spec.to_string()));
    }
    summary(&chosen)
}

/// The default answer: how many specs stand in each status, which carry a tree, which an error.
fn summary(specs: &[&serde_json::Value]) -> String {
    let mut counts: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    let mut with_tree = Vec::new();
    let mut with_error = Vec::new();
    for spec in specs {
        let status = text(spec, "status").unwrap_or("unknown");
        *counts.entry(status).or_default() += 1;
        let spec_id = text(spec, "id").unwrap_or_default();
        if let Some(tree_id) = text(spec, "tree_id") {
            with_tree.push(
                serde_json::json!({ "spec_id": spec_id, "status": status, "tree_id": tree_id })
                    .to_string(),
            );
        }
        if let Some(error) = spec_error(spec, status) {
            with_error.push(
                serde_json::json!({ "spec_id": spec_id, "status": status, "error": error })
                    .to_string(),
            );
        }
    }
    let counts = serde_json::to_string(&counts).unwrap_or_else(|_| "{}".to_owned());
    format!(
        "{{\n\"counts\": {counts},\n\"total\": {total},\n\"with_tree\": {trees},\n\"with_error\": {errors}\n}}",
        total = specs.len(),
        trees = lines(with_tree.into_iter()),
        errors = lines(with_error.into_iter()),
    )
}

/// A spec's error text: its `error` field when the row carries one, else the `status_reason` of a
/// spec that is not done (a done spec's reason is a note, not an error).
fn spec_error<'a>(spec: &'a serde_json::Value, status: &str) -> Option<&'a str> {
    text(spec, "error").or_else(|| {
        if status == "done" {
            None
        } else {
            text(spec, "status_reason")
        }
    })
}

/// A non-empty string field of a spec row.
fn text<'a>(spec: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    spec.get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

/// A JSON array written one already-serialised item per line.
fn lines(items: impl Iterator<Item = String>) -> String {
    let items: Vec<String> = items.collect();
    if items.is_empty() {
        return "[]".to_owned();
    }
    format!("[\n{}\n]", items.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(
        id: &str,
        status: &str,
        tree_id: Option<&str>,
        reason: Option<&str>,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "project_id": "p",
            "path": format!("specs/{id}.md"),
            "title": id,
            "status": status,
            "status_reason": reason,
            "tree_id": tree_id,
        })
    }

    fn backlog() -> serde_json::Value {
        serde_json::json!([
            spec("a", "done", Some("tree-a"), Some("a note")),
            spec("b", "blocked", None, Some("criteria doubt")),
            spec("c", "in_progress", Some("tree-c"), None),
            spec("d", "blocked", None, None),
        ])
    }

    fn query(status: Option<&str>, spec_id: Option<&str>, full: Option<bool>) -> SpecBacklogParams {
        SpecBacklogParams {
            status: status.map(str::to_owned),
            spec_id: spec_id.map(str::to_owned),
            full,
        }
    }

    fn parsed(answer: &str) -> serde_json::Value {
        serde_json::from_str(answer).unwrap_or_else(|e| panic!("not JSON ({e}): {answer}"))
    }

    #[test]
    fn spec_backlog_summarises_by_default() {
        let answer = backlog_answer(backlog(), &SpecBacklogParams::default());
        let value = parsed(&answer);

        assert_eq!(value["total"], 4);
        assert_eq!(
            value["counts"],
            serde_json::json!({ "blocked": 2, "done": 1, "in_progress": 1 })
        );
        assert_eq!(
            value["with_tree"],
            serde_json::json!([
                { "spec_id": "a", "status": "done", "tree_id": "tree-a" },
                { "spec_id": "c", "status": "in_progress", "tree_id": "tree-c" },
            ])
        );
        assert_eq!(
            value["with_error"],
            serde_json::json!([{ "spec_id": "b", "status": "blocked", "error": "criteria doubt" }])
        );
        assert!(
            !answer.contains("specs/"),
            "the summary carried whole rows: {answer}"
        );
    }

    #[test]
    fn spec_backlog_summary_filters_by_status() {
        let value = parsed(&backlog_answer(
            backlog(),
            &query(Some("blocked"), None, None),
        ));

        assert_eq!(value["total"], 2);
        assert_eq!(value["counts"], serde_json::json!({ "blocked": 2 }));
        assert_eq!(value["with_tree"], serde_json::json!([]));
    }

    #[test]
    fn spec_backlog_full_lists_one_spec_per_line_and_still_filters() {
        let answer = backlog_answer(backlog(), &query(None, None, Some(true)));
        let value = parsed(&answer);
        assert_eq!(value.as_array().map(Vec::len), Some(4));
        assert_eq!(value[1]["id"], "b");
        // `[`, four rows, `]`: no row shares a line, and no row spans several.
        assert_eq!(answer.lines().count(), 6, "{answer}");

        let blocked = parsed(&backlog_answer(
            backlog(),
            &query(Some("blocked"), None, Some(true)),
        ));
        let ids: Vec<&str> = blocked
            .as_array()
            .unwrap()
            .iter()
            .map(|spec| spec["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["b", "d"]);
    }

    #[test]
    fn spec_backlog_returns_one_spec_in_full() {
        let value = parsed(&backlog_answer(
            backlog(),
            &query(Some("done"), Some("b"), None),
        ));
        assert_eq!(value, spec("b", "blocked", None, Some("criteria doubt")));

        let missing = parsed(&backlog_answer(backlog(), &query(None, Some("zz"), None)));
        assert!(
            missing["error"].as_str().unwrap_or_default().contains("zz"),
            "{missing}"
        );
    }

    #[test]
    fn spec_backlog_empty_reads_as_never_discovered() {
        let value = parsed(&backlog_answer(
            serde_json::json!([]),
            &SpecBacklogParams::default(),
        ));
        assert_eq!(value["never_discovered"], true);
        assert_eq!(value["hint"], NEVER_DISCOVERED_HINT);

        // A filter that matches nothing is a summary of nothing, not "never discovered".
        let none = parsed(&backlog_answer(
            backlog(),
            &query(Some("queued"), None, None),
        ));
        assert_eq!(none["total"], 0);
        assert!(none.get("never_discovered").is_none(), "{none}");
    }

    #[test]
    fn spec_backlog_prefers_an_error_field_over_the_reason() {
        let mut row = spec("e", "needs_owner", None, Some("reason"));
        row["error"] = serde_json::json!("the extract failed");
        let answer = backlog_answer(serde_json::json!([row]), &SpecBacklogParams::default());
        let value = parsed(&answer);
        assert_eq!(value["with_error"][0]["error"], "the extract failed");
    }
}
