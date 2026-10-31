//! A node's report: parse it, list what makes it unacceptable, and render it as a handoff.

use std::collections::{HashMap, HashSet};

/// Each outcome a node can report, with the status that outcome requires.
pub const OUTCOME_STATUS: [(&str, &str); 6] = [
    ("done", "done"),
    ("split", "done"),
    ("contract_request", "blocked"),
    ("blocked", "blocked"),
    ("failed", "failed"),
    ("handoff", "handoff"),
];

/// The longest accepted summary, in characters.
pub const SUMMARY_MAX: usize = 1500;

const ROLES: [&str; 3] = ["leaf", "orchestrator", "review"];
const HEADER_NAMES: [&str; 6] = [
    "Node ID",
    "Root ID",
    "Status",
    "Role",
    "Outcome",
    "Contract used",
];

/// A filled node handoff. A field the report leaves out or fills with `none` is empty.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub node_id: String,
    pub root_id: String,
    pub status: String,
    pub role: String,
    pub outcome: String,
    pub contract_used: Vec<String>,
    pub contract_used_raw: String,
    pub summary: String,
    pub done: Vec<String>,
    pub decisions: Vec<String>,
    pub files: Vec<String>,
    pub commands: Vec<String>,
    pub open_items: String,
    pub contract_request: String,
    pub blocked_reason: String,
    pub line_count: usize,
}

/// Parses a filled handoff. HTML comments count as absent, so an unfilled template yields empty
/// fields. Headers are only read before the first `## ` section.
pub fn parse_report(text: &str) -> Report {
    let cleaned = strip_comments(text);
    let lines: Vec<&str> = cleaned.lines().map(str::trim_end).collect();

    let mut headers: HashMap<&str, &str> = HashMap::new();
    let mut sections: HashMap<String, Vec<&str>> = HashMap::new();
    let mut current: Option<String> = None;
    for &line in &lines {
        if let Some(name) = line.strip_prefix("## ") {
            let name = name.trim().to_lowercase();
            sections.insert(name.clone(), Vec::new());
            current = Some(name);
        } else if let Some(name) = &current {
            if let Some(section) = sections.get_mut(name) {
                section.push(line);
            }
        } else if let Some((name, value)) = line.split_once(':')
            && HEADER_NAMES.contains(&name)
        {
            headers.insert(name, value.trim_matches([' ', '\t']));
        }
    }

    let header = |name: &str| headers.get(name).copied().unwrap_or("");
    let body = |name: &str| {
        sections
            .get(name)
            .map(|section| section.join("\n"))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let prose = |name: &str| {
        let text = body(name);
        if is_none(&text) { String::new() } else { text }
    };

    let raw = header("Contract used");
    let mut contract_used: Vec<String> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect();
    if contract_used.len() == 1 && contract_used[0].to_lowercase() == "none" {
        contract_used.clear();
    }

    Report {
        node_id: header("Node ID").to_string(),
        root_id: header("Root ID").to_string(),
        status: header("Status").to_lowercase(),
        role: header("Role").to_lowercase(),
        outcome: header("Outcome").to_lowercase(),
        contract_used,
        contract_used_raw: raw.to_string(),
        summary: body("summary"),
        done: items(&body("what was done")),
        decisions: items(&body("decisions")),
        files: items(&body("files touched")),
        commands: items(&body("commands for the root")),
        open_items: prose("open items / next step"),
        contract_request: prose("contract request"),
        blocked_reason: prose("blocked reason"),
        line_count: lines.iter().filter(|line| !line.trim().is_empty()).count(),
    }
}

/// Every reason the report is not acceptable, worded for the agent that wrote it; empty means
/// valid. `known_ids` is the contract's id set, or `None` to check only the shape of each id.
pub fn report_problems(
    report: &Report,
    max_lines: usize,
    known_ids: Option<&HashSet<String>>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let role = report.role.as_str();
    let outcome = report.outcome.as_str();
    let status = report.status.as_str();

    if report.node_id.is_empty() {
        problems.push("node_id is empty (header line `Node ID:`)".to_string());
    }
    if report.root_id.is_empty() {
        problems.push("root_id is empty (header line `Root ID:`)".to_string());
    }
    if !ROLES.contains(&role) {
        let roles = quoted_list(ROLES.iter().copied());
        problems.push(format!("Role must be one of {roles}, not '{role}'"));
    }
    match expected_status(outcome) {
        None => {
            let outcomes = quoted_list(OUTCOME_STATUS.iter().map(|&(name, _)| name));
            problems.push(format!(
                "Outcome must be one of {outcomes}, not '{outcome}'"
            ));
        }
        Some(expected) if status != expected => problems.push(format!(
            "Status '{status}' does not match Outcome '{outcome}' (expected '{expected}')"
        )),
        Some(_) => {}
    }
    if outcome == "split" && matches!(role, "leaf" | "review") {
        problems.push(format!("only an orchestrator can split, not a {role}"));
    }
    if matches!(role, "orchestrator" | "review") && !report.files.is_empty() {
        problems.push(format!(
            "a {role} edits no file, yet Files touched lists {}",
            report.files.len()
        ));
    }
    if report.contract_used_raw.trim().is_empty() {
        problems.push("Contract used is empty: write the ids or `none`".to_string());
    }
    for id in &report.contract_used {
        if !is_contract_id(id) {
            problems.push(format!("Contract used '{id}' is not an id like A3 or E2"));
        } else if known_ids.is_some_and(|known| !known.contains(id)) {
            problems.push(format!("Contract used {id} is not in contract.md"));
        }
    }
    let summary_len = report.summary.chars().count();
    if summary_len == 0 {
        problems.push("summary is empty".to_string());
    } else if summary_len > SUMMARY_MAX {
        problems.push(format!(
            "summary is {summary_len} characters; the limit is {SUMMARY_MAX}"
        ));
    }
    if outcome == "contract_request" && report.contract_request.is_empty() {
        problems
            .push("Outcome contract_request needs text under `## Contract request`".to_string());
    }
    if outcome != "contract_request" && !report.contract_request.is_empty() {
        problems
            .push("`## Contract request` has text but Outcome is not contract_request".to_string());
    }
    if matches!(outcome, "blocked" | "failed") && report.blocked_reason.is_empty() {
        problems.push(format!(
            "Outcome {outcome} needs text under `## Blocked reason`"
        ));
    }
    if report.line_count > max_lines {
        problems.push(format!(
            "report has {} lines; the limit is {max_lines} (HTML comments not counted)",
            report.line_count
        ));
    }
    problems
}

/// A filled handoff, the inverse of [`parse_report`]. Empty lists and prose are written as `none`;
/// an empty summary stays empty, because `none` is not special there.
pub fn render_handoff(report: &Report) -> String {
    let contract_used = if report.contract_used.is_empty() {
        "none".to_string()
    } else {
        report.contract_used.join(", ")
    };
    format!(
        "# Node Handoff\n\nNode ID: {}\nRoot ID: {}\nStatus: {}\nRole: {}\nOutcome: {}\n\
         Contract used: {}\n\n## Summary\n{}\n\n## What was done\n{}\n\n## Decisions\n{}\n\n\
         ## Files touched\n{}\n\n## Commands for the root\n{}\n\n\
         ## Open items / next step\n{}\n\n## Contract request\n{}\n\n## Blocked reason\n{}\n",
        report.node_id,
        report.root_id,
        report.status,
        report.role,
        report.outcome,
        contract_used,
        report.summary,
        bullets(&report.done),
        bullets(&report.decisions),
        bullets(&report.files),
        bullets(&report.commands),
        or_none(&report.open_items),
        or_none(&report.contract_request),
        or_none(&report.blocked_reason),
    )
}

/// Removes every closed `<!-- ... -->`, across lines. An unclosed `<!--` stays as text.
fn strip_comments(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        let inside = &rest[start + "<!--".len()..];
        let Some(end) = inside.find("-->") else {
            break;
        };
        kept.push_str(&rest[..start]);
        rest = &inside[end + "-->".len()..];
    }
    kept.push_str(rest);
    kept
}

fn is_none(text: &str) -> bool {
    let text = text.trim().trim_end_matches('.').to_lowercase();
    text.is_empty() || text == "none"
}

/// The lines of a list section, without their bullets. `none` only means an empty list when it is
/// the sole item.
fn items(text: &str) -> Vec<String> {
    let items: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| strip_bullet(line).to_string())
        .collect();
    if items.len() == 1 && is_none(&items[0]) {
        return Vec::new();
    }
    items
}

/// Drops a leading `- ` or `* `. A bullet with no whitespace after it is ordinary text.
fn strip_bullet(line: &str) -> &str {
    match line.strip_prefix(['-', '*']) {
        Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
        _ => line,
    }
}

fn expected_status(outcome: &str) -> Option<&'static str> {
    OUTCOME_STATUS
        .iter()
        .find(|(name, _)| *name == outcome)
        .map(|&(_, status)| status)
}

/// One uppercase ASCII letter followed by at least one digit, like `A3`.
fn is_contract_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|first| first.is_ascii_uppercase())
        && !chars.as_str().is_empty()
        && chars.all(|c| c.is_ascii_digit())
}

fn quoted_list<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let quoted: Vec<String> = names.map(|name| format!("'{name}'")).collect();
    format!("[{}]", quoted.join(", "))
}

fn bullets(items: &[String]) -> String {
    if items.is_empty() {
        return "none".to_string();
    }
    let lines: Vec<String> = items.iter().map(|item| format!("- {item}")).collect();
    lines.join("\n")
}

fn or_none(text: &str) -> &str {
    if text.is_empty() { "none" } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const MAX_LINES: usize = 60;

    const FILLED: &str = "\
# Node Handoff

Node ID: n-1
Root ID: r-1
Status: DONE
Role: Leaf
Outcome: Done
Contract used: A1, E2 B3

## Summary
Added the parser.
Status: failed

## WHAT WAS DONE
- wrote the parser
* wrote the tests
plain line

## Decisions
- kept it pure

## Files touched
- core/src/tree_job/report.rs

## Commands for the root
- cargo test -p nucleos-core --lib tree_job::report

## Open items / next step
Wire it into the close path.

## Contract request
none

## Blocked reason
None.
";

    /// A valid report with one edit applied, for tests that add a single defect.
    fn filled_with(edit: impl FnOnce(&mut Report)) -> Report {
        let mut report = parse_report(FILLED);
        edit(&mut report);
        report
    }

    fn problems(report: &Report) -> Vec<String> {
        report_problems(report, MAX_LINES, None)
    }

    fn known(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn assert_single_problem(report: &Report, fragment: &str) {
        let found = problems(report);
        assert_eq!(found.len(), 1, "expected one problem, got {found:?}");
        assert!(
            found[0].contains(fragment),
            "expected {fragment:?} in {:?}",
            found[0]
        );
    }

    #[test]
    fn parse_report_reads_every_field_of_a_filled_report() {
        let report = parse_report(FILLED);

        assert_eq!(report.node_id, "n-1");
        assert_eq!(report.root_id, "r-1");
        // Status, Role and Outcome are lowercased, and a header-like line inside a section
        // stays in that section's text.
        assert_eq!(report.status, "done");
        assert_eq!(report.role, "leaf");
        assert_eq!(report.outcome, "done");
        assert_eq!(report.contract_used, vec!["A1", "E2", "B3"]);
        assert_eq!(report.contract_used_raw, "A1, E2 B3");
        assert_eq!(report.summary, "Added the parser.\nStatus: failed");
        assert_eq!(
            report.done,
            vec!["wrote the parser", "wrote the tests", "plain line"]
        );
        assert_eq!(report.decisions, vec!["kept it pure"]);
        assert_eq!(report.files, vec!["core/src/tree_job/report.rs"]);
        assert_eq!(
            report.commands,
            vec!["cargo test -p nucleos-core --lib tree_job::report"]
        );
        assert_eq!(report.open_items, "Wire it into the close path.");
        assert_eq!(report.contract_request, "");
        assert_eq!(report.blocked_reason, "");
        assert_eq!(report.line_count, 26);
    }

    #[test]
    fn parse_report_leaves_missing_headers_and_sections_empty() {
        let report = parse_report("Node ID: n-1\n\n## Summary\nOnly a summary.\n");

        assert_eq!(report.node_id, "n-1");
        assert_eq!(report.summary, "Only a summary.");
        assert_eq!(report.root_id, "");
        assert_eq!(report.status, "");
        assert_eq!(report.role, "");
        assert_eq!(report.outcome, "");
        assert!(report.contract_used.is_empty());
        assert_eq!(report.contract_used_raw, "");
        assert!(report.done.is_empty());
        assert!(report.decisions.is_empty());
        assert!(report.files.is_empty());
        assert!(report.commands.is_empty());
        assert_eq!(report.open_items, "");
        assert_eq!(report.contract_request, "");
        assert_eq!(report.blocked_reason, "");
        assert_eq!(report.line_count, 3);

        assert_eq!(parse_report("").line_count, 0);
    }

    #[test]
    fn parse_report_reads_html_comments_and_none_as_absent() {
        let report = parse_report(
            "\
Node ID: <!-- the id -->
Status: done
Contract used: None
<!-- a comment
Status: failed
## Decisions
- hidden
-->
## Summary
<!-- write here -->

## What was done
none

## Decisions
- None.

## Files touched
- none
- a.rs

## Open items / next step
none

## Contract request
None.

## Blocked reason
",
        );

        assert_eq!(report.node_id, "");
        assert_eq!(report.status, "done");
        assert_eq!(report.summary, "");
        // `none` empties the id list, while the raw text still shows the field was filled in.
        assert!(report.contract_used.is_empty());
        assert_eq!(report.contract_used_raw, "None");
        assert!(report.done.is_empty());
        assert!(report.decisions.is_empty());
        // `none` only means an empty list when it is the sole item.
        assert_eq!(report.files, vec!["none", "a.rs"]);
        assert_eq!(report.open_items, "");
        assert_eq!(report.contract_request, "");
        assert_eq!(report.blocked_reason, "");
        // Lines inside comments and blank lines do not count.
        assert_eq!(report.line_count, 16);

        // The summary is plain text: `none` is not special there.
        assert_eq!(parse_report("## Summary\nnone\n").summary, "none");
    }

    #[test]
    fn report_problems_is_empty_for_a_valid_report() {
        let report = parse_report(FILLED);

        assert_eq!(problems(&report), Vec::<String>::new());
        let ids = known(&["A1", "E2", "B3"]);
        assert_eq!(
            report_problems(&report, MAX_LINES, Some(&ids)),
            Vec::<String>::new()
        );

        let split = filled_with(|r| {
            r.role = "orchestrator".into();
            r.outcome = "split".into();
            r.files.clear();
        });
        assert_eq!(problems(&split), Vec::<String>::new());
    }

    #[test]
    fn report_problems_flags_each_contract_id_that_is_unknown_or_malformed() {
        let report = parse_report(FILLED);

        // Only E2 and B3 are missing from the contract, one problem each.
        let found = report_problems(&report, MAX_LINES, Some(&known(&["A1"])));
        assert_eq!(
            found,
            vec![
                "Contract used E2 is not in contract.md",
                "Contract used B3 is not in contract.md",
            ]
        );

        // Without a known set, a well-formed id is accepted and a malformed one is not.
        let malformed = filled_with(|r| r.contract_used = vec!["A1".into(), "a1".into()]);
        assert_single_problem(&malformed, "is not an id like A3 or E2");
        assert!(problems(&malformed)[0].contains("a1"));

        // A malformed id is reported as malformed, not also as unknown.
        let found = report_problems(&malformed, MAX_LINES, Some(&known(&["A1"])));
        assert_eq!(found.len(), 1, "got {found:?}");
    }

    #[test]
    fn report_problems_flags_an_unknown_outcome_once_and_lists_the_valid_ones() {
        let report = filled_with(|r| r.outcome = "shipped".into());

        assert_single_problem(&report, "Outcome must be one of");
        let message = &problems(&report)[0];
        assert!(message.contains("shipped"), "got {message:?}");
        for (outcome, _) in OUTCOME_STATUS.iter().copied() {
            assert!(
                message.contains(outcome),
                "{outcome} missing from {message:?}"
            );
        }
    }

    #[test]
    fn report_problems_gives_one_problem_per_defect_of_a_sparse_report() {
        let sparse = parse_report("Node ID: n-1\n\n## Summary\nOnly a summary.\n");
        let found = problems(&sparse);
        assert_eq!(found.len(), 4, "got {found:?}");
        for fragment in [
            "root_id is empty (header line `Root ID:`)",
            "Role must be one of",
            "Outcome must be one of",
            "Contract used is empty: write the ids or `none`",
        ] {
            let matches = found.iter().filter(|p| p.contains(fragment)).count();
            assert_eq!(matches, 1, "{fragment:?} in {found:?}");
        }

        let empty = problems(&parse_report(""));
        assert_eq!(empty.len(), 6, "got {empty:?}");
        assert!(empty.iter().any(|p| p.contains("node_id is empty")));
        assert!(empty.iter().any(|p| p == "summary is empty"));

        assert_single_problem(&filled_with(|r| r.role = "boss".into()), "boss");
    }

    #[test]
    fn report_problems_enforces_the_rules_between_role_outcome_and_sections() {
        let split_by = |role: &str| {
            let role = role.to_string();
            filled_with(move |r| {
                r.role = role;
                r.outcome = "split".into();
                r.files.clear();
            })
        };
        assert_single_problem(
            &split_by("leaf"),
            "only an orchestrator can split, not a leaf",
        );
        assert_single_problem(
            &split_by("review"),
            "only an orchestrator can split, not a review",
        );

        for role in ["orchestrator", "review"] {
            let report = filled_with(|r| r.role = role.into());
            assert_single_problem(&report, "edits no file, yet Files touched lists 1");
        }

        let request_without_text = filled_with(|r| {
            r.outcome = "contract_request".into();
            r.status = "blocked".into();
        });
        assert_single_problem(
            &request_without_text,
            "Outcome contract_request needs text under `## Contract request`",
        );

        let text_without_request = filled_with(|r| r.contract_request = "need A9".into());
        assert_single_problem(
            &text_without_request,
            "`## Contract request` has text but Outcome is not contract_request",
        );

        for (outcome, status) in [("blocked", "blocked"), ("failed", "failed")] {
            let report = filled_with(|r| {
                r.outcome = outcome.into();
                r.status = status.into();
            });
            let expected = format!("Outcome {outcome} needs text under `## Blocked reason`");
            assert_single_problem(&report, &expected);
        }
    }

    #[test]
    fn report_problems_caps_the_summary_in_characters_and_the_report_in_lines() {
        // A multi-byte character proves the cap counts characters, not bytes.
        let at_cap = filled_with(|r| r.summary = "é".repeat(SUMMARY_MAX));
        assert_eq!(SUMMARY_MAX, 1500);
        assert_eq!(problems(&at_cap), Vec::<String>::new());

        let over_cap = filled_with(|r| r.summary = "é".repeat(SUMMARY_MAX + 1));
        assert_single_problem(&over_cap, "summary is 1501 characters; the limit is 1500");

        let at_limit = filled_with(|r| r.line_count = MAX_LINES);
        assert_eq!(problems(&at_limit), Vec::<String>::new());

        let over_limit = filled_with(|r| r.line_count = MAX_LINES + 1);
        assert_single_problem(
            &over_limit,
            "report has 61 lines; the limit is 60 (HTML comments not counted)",
        );
    }

    #[test]
    fn render_handoff_writes_the_handoff_template_filled_in() {
        let rendered = render_handoff(&parse_report(FILLED));

        assert_eq!(
            rendered,
            "\
# Node Handoff

Node ID: n-1
Root ID: r-1
Status: done
Role: leaf
Outcome: done
Contract used: A1, E2, B3

## Summary
Added the parser.
Status: failed

## What was done
- wrote the parser
- wrote the tests
- plain line

## Decisions
- kept it pure

## Files touched
- core/src/tree_job/report.rs

## Commands for the root
- cargo test -p nucleos-core --lib tree_job::report

## Open items / next step
Wire it into the close path.

## Contract request
none

## Blocked reason
none
"
        );
    }

    #[test]
    fn render_handoff_writes_none_for_empty_fields_and_parses_back_to_the_same_report() {
        let sparse = parse_report("Node ID: n-1\n\n## Summary\nOnly a summary.\n");
        let rendered = render_handoff(&sparse);
        for expected in [
            "Contract used: none\n",
            "## What was done\nnone\n",
            "## Decisions\nnone\n",
            "## Files touched\nnone\n",
            "## Commands for the root\nnone\n",
            "## Open items / next step\nnone\n",
            "## Contract request\nnone\n",
            "## Blocked reason\nnone\n",
        ] {
            assert!(rendered.contains(expected), "{expected:?} in {rendered:?}");
        }

        for original in [sparse, parse_report(FILLED)] {
            let mut back = parse_report(&render_handoff(&original));
            // Rendering normalizes the raw id list and adds the template's lines.
            back.contract_used_raw = original.contract_used_raw.clone();
            back.line_count = original.line_count;
            assert_eq!(back, original);
        }
    }

    #[test]
    fn outcome_status_maps_the_six_outcomes_and_report_problems_enforces_it() {
        assert_eq!(
            OUTCOME_STATUS.to_vec(),
            vec![
                ("done", "done"),
                ("split", "done"),
                ("contract_request", "blocked"),
                ("blocked", "blocked"),
                ("failed", "failed"),
                ("handoff", "handoff"),
            ]
        );

        for (outcome, status) in OUTCOME_STATUS.iter().copied() {
            let mut report = filled_with(|r| {
                r.role = "orchestrator".into();
                r.files.clear();
                r.outcome = outcome.into();
                r.status = status.into();
                r.blocked_reason = "stuck".into();
                if outcome == "contract_request" {
                    r.contract_request = "need A9".into();
                }
            });
            assert_eq!(problems(&report), Vec::<String>::new(), "outcome {outcome}");

            report.status = "wrong".into();
            assert_single_problem(&report, "does not match Outcome");
        }
    }

    /// A node that ran out of room reports `handoff` so the daemon can relaunch it with a wrap
    /// prompt; the status it carries is `handoff`, the value `work_nodes` already knows.
    #[test]
    fn handoff_is_an_accepted_outcome_with_status_handoff() {
        assert!(
            OUTCOME_STATUS.contains(&("handoff", "handoff")),
            "got {OUTCOME_STATUS:?}"
        );

        let report = filled_with(|r| {
            r.outcome = "handoff".into();
            r.status = "handoff".into();
        });
        assert_eq!(problems(&report), Vec::<String>::new());

        // The pairing is enforced like any other: `done` does not go with `handoff`.
        let mismatched = filled_with(|r| r.outcome = "handoff".into());
        assert_single_problem(&mismatched, "does not match Outcome");
        assert!(
            problems(&mismatched)[0].contains("expected 'handoff'"),
            "got {:?}",
            problems(&mismatched)
        );

        // And the outcome list the agent is shown names it.
        let unknown = filled_with(|r| r.outcome = "shipped".into());
        assert!(
            problems(&unknown)[0].contains("handoff"),
            "got {:?}",
            problems(&unknown)
        );
    }
}
