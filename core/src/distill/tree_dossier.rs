//! The dossier of a finished tree (spec 2026-11-16-memoria-agentes-captura, F3): the blocks the
//! gatherer reads, cut by priority and clipped per block, with every text scrubbed.
//!
//! RED stage: every body below is a stub that compiles and does nothing, so the tests of this file
//! fail at run time. The GREEN packet replaces them.

use super::{DOSSIER_CEILING, Dossier};

/// The tree dossier is cut at the same ceiling as the job dossier.
pub const TREE_DOSSIER_CEILING: usize = DOSSIER_CEILING;
/// Characters kept of the diff stat.
pub const DIFF_CHARS: usize = 1500;
/// Characters kept of the commit subjects.
pub const COMMITS_CHARS: usize = 1500;
/// Commit subjects kept.
pub const COMMIT_LINES: usize = 30;
/// Characters kept of one node report.
pub const NODE_CHARS: usize = 1000;
/// Node reports kept.
pub const MAX_NODES: usize = 8;
/// Characters kept of one answered decision.
pub const DECISION_CHARS: usize = 600;
/// Answered decisions kept.
pub const MAX_DECISIONS: usize = 6;
/// Characters kept of one failed verification.
pub const FAILURE_CHARS: usize = 1500;
/// Failed verifications kept.
pub const MAX_FAILURES: usize = 4;
/// Lines of a failed verification kept around its first error line.
pub const SIGNAL_LINES: usize = 12;
/// How far back a dead lesson still counts as refused.
pub const REFUSED_DAYS: i64 = 90;
/// Refused titles listed.
pub const MAX_REFUSED: usize = 20;

/// One node of the tree, as its handoff and its row say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeReport {
    pub node_id: String,
    pub status: String,
    pub text: String,
    pub has_open_items: bool,
}

/// Everything the gatherer found about a tree, before it is cut to a dossier.
#[derive(Debug, Clone, Default)]
pub struct TreeBlocks {
    pub outcome: Option<String>,
    pub node_reports: Vec<NodeReport>,
    pub bad_nodes: usize,
    pub open_item_nodes: usize,
    pub decisions: Vec<String>,
    pub red_failures: Vec<(i64, String)>,
    pub agent_final: Vec<(i64, String)>,
    pub diff_stat: Option<String>,
    pub commits: Option<String>,
    pub known_titles: Vec<String>,
    pub refused: Vec<String>,
}

/// Build the dossier of a tree: blocks in priority order, the last one going first when over
/// `ceiling`.
pub fn tree_dossier(_blocks: &TreeBlocks, _ceiling: usize) -> Dossier {
    Dossier {
        text: String::new(),
        runs: vec![],
    }
}

/// `SIGNAL_LINES` lines around the first line that names an error, else the tail.
pub fn signal_window(_output: &str, _lines: usize) -> String {
    String::new()
}

/// The summary, decisions, open items and blocked reason of a node report, clipped to `NODE_CHARS`.
pub fn node_report_block(_report: &crate::tree_job::report::Report, _status: &str) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::{
        COMMITS_CHARS, DIFF_CHARS, NODE_CHARS, NodeReport, SIGNAL_LINES, TreeBlocks,
        node_report_block, signal_window, tree_dossier,
    };
    use crate::distill::{Cause, DOSSIER_CEILING, TREE_CAUSES};
    use crate::tree_job::report::Report;

    /// A long text that starts with `tag` and carries no marker of the dossier.
    fn filler(tag: &str, repeats: usize) -> String {
        format!("{tag} {}", "lorem ipsum ".repeat(repeats))
    }

    fn node(id: &str, text: String) -> NodeReport {
        NodeReport {
            node_id: id.to_owned(),
            status: "done".to_owned(),
            text,
            has_open_items: false,
        }
    }

    /// Every block present and over its own cap, so the whole is far over a small ceiling.
    fn every_block() -> TreeBlocks {
        TreeBlocks {
            outcome: Some(filler("OUTCOME-BLOCK", 400)),
            node_reports: (0..8)
                .map(|i| node(&format!("n{i}"), filler(&format!("NODE-{i}"), 300)))
                .collect(),
            bad_nodes: 1,
            open_item_nodes: 1,
            decisions: (0..6).map(|i| filler(&format!("DECISION-{i}"), 100)).collect(),
            red_failures: (0..4)
                .map(|i| (11 + i, filler(&format!("FAILURE-{i}"), 200)))
                .collect(),
            agent_final: vec![(21, filler("FINAL-MESSAGE", 300))],
            diff_stat: Some(filler("DIFF-STAT", 300)),
            commits: Some(filler("COMMIT-LIST", 300)),
            known_titles: (0..50).map(|i| format!("Known title {i}")).collect(),
            refused: (0..20).map(|i| format!("Refused title {i}")).collect(),
        }
    }

    fn report(
        summary: &str,
        decisions: &[&str],
        open_items: &str,
        blocked_reason: &str,
    ) -> Report {
        Report {
            node_id: "n-api".to_owned(),
            root_id: "job-1-root".to_owned(),
            status: "done".to_owned(),
            role: "leaf".to_owned(),
            outcome: "done".to_owned(),
            contract_used: Vec::new(),
            contract_used_raw: String::new(),
            summary: summary.to_owned(),
            done: Vec::new(),
            decisions: decisions.iter().map(|d| (*d).to_owned()).collect(),
            files: Vec::new(),
            commands: Vec::new(),
            open_items: open_items.to_owned(),
            contract_request: String::new(),
            blocked_reason: blocked_reason.to_owned(),
            line_count: 0,
        }
    }

    // ----- tree_dossier -----

    #[test]
    fn tree_dossier_with_every_block_stays_within_the_ceiling() {
        let ceiling = 9_000;
        let out = tree_dossier(&every_block(), ceiling);
        // The only thing allowed past the ceiling is the one closing line that announces the cut.
        assert!(
            out.text.chars().count() <= ceiling + 120,
            "{} chars",
            out.text.chars().count()
        );
        assert!(
            out.text.contains("NODE-0"),
            "the highest-priority block survives the cut"
        );
        // And with the real ceiling the whole is still bounded.
        let real = tree_dossier(&every_block(), DOSSIER_CEILING);
        assert!(real.text.chars().count() <= DOSSIER_CEILING + 120);
        assert!(real.text.contains("NODE-0"));
    }

    #[test]
    fn tree_dossier_keeps_red_failures_when_lower_blocks_overflow() {
        let blocks = TreeBlocks {
            node_reports: vec![node("n1", "NODE-SMALL did the thing".to_owned())],
            decisions: vec!["DECISION-SMALL pick A".to_owned()],
            red_failures: vec![(41, "RED-FAILURE-MARKER gate failed on foo".to_owned())],
            diff_stat: Some(filler("DIFF-MARKER", 300)),
            commits: Some(filler("COMMIT-MARKER", 300)),
            known_titles: (0..50).map(|i| format!("Known title {i}")).collect(),
            refused: (0..20).map(|i| format!("Refused title {i}")).collect(),
            ..TreeBlocks::default()
        };
        let out = tree_dossier(&blocks, 1_200);
        assert!(out.text.contains("RED-FAILURE-MARKER"), "{}", out.text);
        assert!(out.text.contains("NODE-SMALL"));
        assert!(
            !out.text.contains("DIFF-MARKER"),
            "the lower blocks go first"
        );
        assert!(!out.text.contains("COMMIT-MARKER"));
        assert!(out.runs.contains(&41), "runs of the kept blocks: {:?}", out.runs);
    }

    #[test]
    fn a_block_over_its_own_cap_is_clipped_not_dropped() {
        let blocks = TreeBlocks {
            node_reports: vec![node(
                "n1",
                format!("{}NODE-TAIL-MARKER", filler("NODE-HEAD", 400)),
            )],
            diff_stat: Some(format!("{}DIFF-TAIL-MARKER", filler("DIFF-HEAD", 400))),
            ..TreeBlocks::default()
        };
        let out = tree_dossier(&blocks, DOSSIER_CEILING);
        assert!(out.text.contains("NODE-HEAD"), "clipped, not dropped");
        assert!(out.text.contains("DIFF-HEAD"), "clipped, not dropped");
        assert!(!out.text.contains("NODE-TAIL-MARKER"));
        assert!(!out.text.contains("DIFF-TAIL-MARKER"));
        assert!(
            out.text.chars().count() <= NODE_CHARS + DIFF_CHARS + 400,
            "{} chars",
            out.text.chars().count()
        );
        let commits = TreeBlocks {
            commits: Some(format!("{}COMMIT-TAIL-MARKER", filler("COMMIT-HEAD", 400))),
            ..TreeBlocks::default()
        };
        let out = tree_dossier(&commits, DOSSIER_CEILING);
        assert!(out.text.contains("COMMIT-HEAD"));
        assert!(!out.text.contains("COMMIT-TAIL-MARKER"));
        assert!(out.text.chars().count() <= COMMITS_CHARS + 200);
    }

    #[test]
    fn tree_dossier_without_diff_or_commits_still_builds() {
        let blocks = TreeBlocks {
            outcome: Some("Tree finished".to_owned()),
            node_reports: vec![node("n1", "NODE-ONLY added the cache".to_owned())],
            decisions: vec!["DECISION-ONLY pick LRU".to_owned()],
            diff_stat: None,
            commits: None,
            ..TreeBlocks::default()
        };
        let out = tree_dossier(&blocks, DOSSIER_CEILING);
        assert!(out.text.contains("NODE-ONLY"), "{}", out.text);
        assert!(out.text.contains("DECISION-ONLY"));
        assert!(!out.text.contains("None"), "an absent block is omitted");
    }

    #[test]
    fn tree_dossier_has_no_escape_character() {
        let ansi = |tag: &str| format!("\u{1b}[31m{tag}\u{1b}[0m text");
        let blocks = TreeBlocks {
            outcome: Some(ansi("OUTCOME-RED")),
            node_reports: vec![node("n1", ansi("NODE-RED"))],
            decisions: vec![ansi("DECISION-RED")],
            red_failures: vec![(1, ansi("FAILURE-RED"))],
            agent_final: vec![(2, ansi("FINAL-RED"))],
            diff_stat: Some(ansi("DIFF-RED")),
            commits: Some(ansi("COMMIT-RED")),
            known_titles: vec![ansi("KNOWN-RED")],
            refused: vec![ansi("REFUSED-RED")],
            ..TreeBlocks::default()
        };
        let out = tree_dossier(&blocks, DOSSIER_CEILING);
        assert!(!out.text.contains('\u{1b}'), "no escape character survives");
        assert!(out.text.contains("NODE-RED"), "the words stay");
        assert!(out.text.contains("FAILURE-RED"));
    }

    #[test]
    fn refused_titles_get_their_own_already_refused_section() {
        let blocks = TreeBlocks {
            node_reports: vec![node("n1", "NODE-ONE did the thing".to_owned())],
            known_titles: vec!["Known title B".to_owned()],
            refused: vec!["Refused title A".to_owned()],
            ..TreeBlocks::default()
        };
        let out = tree_dossier(&blocks, DOSSIER_CEILING);
        let text = &out.text;
        let known_head = text.find("Already known").expect("the known section");
        let refused_head = text.find("Already refused").expect("the refused section");
        let known = text.find("Known title B").expect("the known title");
        let refused = text.find("Refused title A").expect("the refused title");
        assert!(known_head < known && known < refused_head, "{text}");
        assert!(refused_head < refused, "{text}");
    }

    // ----- signal_window -----

    #[test]
    fn signal_window_returns_twelve_lines_around_the_first_error_line() {
        let lines: Vec<String> = (1..=40)
            .map(|i| match i {
                20 => "error[E0599]: no method named foo".to_owned(),
                35 => "error: a second one that must not centre the window".to_owned(),
                _ => format!("line {i:02}"),
            })
            .collect();
        let out = signal_window(&lines.join("\n"), SIGNAL_LINES);
        assert_eq!(out.lines().count(), SIGNAL_LINES, "{out}");
        assert!(out.contains("error[E0599]: no method named foo"));
        assert!(out.contains("line 16") && out.contains("line 24"), "{out}");
        assert!(!out.contains("line 01") && !out.contains("line 40"), "{out}");

        // No error line: the tail.
        let quiet: Vec<String> = (1..=40).map(|i| format!("line {i:02}")).collect();
        let tail = signal_window(&quiet.join("\n"), SIGNAL_LINES);
        assert_eq!(tail.lines().count(), SIGNAL_LINES, "{tail}");
        assert!(tail.contains("line 40") && tail.contains("line 29"), "{tail}");
        assert!(!tail.contains("line 28"), "{tail}");
    }

    // ----- node_report_block -----

    #[test]
    fn node_report_block_keeps_summary_decisions_open_items_and_blocked_reason() {
        let full = report(
            "Added the cache layer",
            &["Use LRU eviction"],
            "Wire the metrics",
            "Waiting for the API key",
        );
        let block = node_report_block(&full, "blocked");
        for wanted in [
            "Added the cache layer",
            "Use LRU eviction",
            "Wire the metrics",
            "Waiting for the API key",
            "blocked",
        ] {
            assert!(block.contains(wanted), "{wanted:?} missing from {block}");
        }
        let long = report(&filler("SUMMARY", 600), &[], "", "");
        let clipped = node_report_block(&long, "done");
        assert!(clipped.contains("SUMMARY"));
        assert!(clipped.chars().count() <= NODE_CHARS, "{}", clipped.chars().count());
    }

    // ----- scrub and the standing instruction (P2, P4) -----

    #[test]
    fn scrub_strips_ansi_sequences() {
        let out = super::super::scrub("\u{1b}[31mred words\u{1b}[0m and \u{1b}[1;32mgreen\u{1b}[0m");
        assert!(!out.contains('\u{1b}'), "{out:?}");
        assert!(out.contains("red words") && out.contains("green"), "{out:?}");
    }

    #[test]
    fn standing_head_asks_for_cause_and_remedy_and_allows_empty_only_without_both() {
        let text = super::super::standing_for(Cause::TreeFailed);
        assert!(text.contains("remedy"), "{text}");
        assert!(
            text.contains("`[]` when nothing has a cause and a remedy"),
            "{text}"
        );
        assert!(
            !text.contains("`[]` is a good answer when nothing is worth keeping"),
            "the old, permissive wording is gone: {text}"
        );
        // The contract the head keeps.
        assert!(text.contains("another project"), "{text}");
        assert!(text.contains("Never quote the owner context"), "{text}");
        assert!(text.contains("Do not repeat the known titles"), "{text}");
        assert!(text.contains("0 to 5 items"), "{text}");
    }

    #[test]
    fn standing_tails_name_the_new_causes() {
        for (cause, phrase) in [
            (Cause::TreeLanded, "what let it land and what nearly blocked it"),
            (Cause::TreeFailed, "the root cause and what the next agent must avoid"),
            (Cause::TreeParked, "what the owner's answer teaches"),
            (Cause::BranchRecovered, "what failed and what fixed it"),
            (Cause::BranchLanded, "what the commit messages and the diff teach"),
        ] {
            let text = super::super::standing_for(cause);
            assert!(text.contains(phrase), "{cause:?}: {text}");
        }
    }

    #[test]
    fn standing_keeps_the_pinned_invariants_for_every_new_cause() {
        for cause in TREE_CAUSES {
            let text = super::super::standing_for(cause);
            assert!(!text.contains("THIS project"), "{cause:?}");
            assert!(text.contains("another project"), "{cause:?}");
            assert!(text.contains("Never quote the owner context"), "{cause:?}");
            assert!(!text.contains("made it land"), "{cause:?}: only a job landing says so");
        }
    }
}
