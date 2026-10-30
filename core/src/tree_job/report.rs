//! A node's report: parse it, list what makes it unacceptable, and render it as a handoff.

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
            assert!(message.contains(outcome), "{outcome} missing from {message:?}");
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
        assert_single_problem(&split_by("leaf"), "only an orchestrator can split, not a leaf");
        assert_single_problem(&split_by("review"), "only an orchestrator can split, not a review");

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
    fn outcome_status_maps_the_five_outcomes_and_report_problems_enforces_it() {
        assert_eq!(
            OUTCOME_STATUS.to_vec(),
            vec![
                ("done", "done"),
                ("split", "done"),
                ("contract_request", "blocked"),
                ("blocked", "blocked"),
                ("failed", "failed"),
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
}
