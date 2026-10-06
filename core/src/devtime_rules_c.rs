//! Family C: spec and clarity (C1 to C3): lost direction, corrections and reverted work. C4 is deferred and
//! is never emitted. Rules read the session's facts and the vocabularies in `DevtimeRulesConfig`; they
//! never write a row. A finding carries only ids, hashes and closed tokens, never message text.
//!
//! - C1: a turn the human interrupted (exact, the turn's own interval on the main lane).
//! - C2: a turn that the next prompt corrected (inferred, the corrected turn's interval).
//! - C3: work undone, by an edit that restores the previous content, by a path-scoped revert command
//!   (`git restore a.rs`) or by a pathless one (`git reset`, inferred).

use std::collections::BTreeSet;

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{
    AttemptFact, Confidence, Finding, Outcome, RuleOutput, SessionFacts, SpanPolicy, lane_attempts,
};
use crate::devtime_rules_cmd::matches_any;

pub fn run(facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
    let mut out = RuleOutput::default();
    c1_interrupted_turns(facts, &mut out);
    c2_corrected_turns(facts, &mut out);
    c3_edit_pairs(facts, &mut out);
    c3_reverts(facts, &mut out);
    out
}

/// The main-lane attempts that started inside the turn with this `seq`.
fn turn_attempt_ids(facts: &SessionFacts, seq: i64) -> Vec<String> {
    facts
        .attempts
        .iter()
        .filter(|a| a.is_main && a.turn_seq == Some(seq))
        .map(|a| a.attempt_id.clone())
        .collect()
}

/// C1: a turn flagged `interrupted`. The interval is the turn's own.
fn c1_interrupted_turns(facts: &SessionFacts, out: &mut RuleOutput) {
    for turn in facts.turns.iter().filter(|turn| turn.interrupted) {
        out.findings.push(Finding {
            rule_id: "C1",
            lane: "main".to_string(),
            started_ms: turn.started_ms,
            ended_ms: turn.ended_ms,
            cost_ms: (turn.ended_ms - turn.started_ms).max(0),
            attempt_ids: turn_attempt_ids(facts, turn.seq),
            confidence: Confidence::Exact,
            count: 1,
            policy: SpanPolicy::Interval,
        });
    }
}

/// C2: turn k+1 opens with a correction, so turn k went the wrong way. `None` (a row written before
/// parser v2, or not evaluated) never fires.
fn c2_corrected_turns(facts: &SessionFacts, out: &mut RuleOutput) {
    for pair in facts.turns.windows(2) {
        let (previous, next) = (&pair[0], &pair[1]);
        if next.opens_with_correction != Some(true) {
            continue;
        }
        out.findings.push(Finding {
            rule_id: "C2",
            lane: "main".to_string(),
            started_ms: previous.started_ms,
            ended_ms: previous.ended_ms,
            cost_ms: (previous.ended_ms - previous.started_ms).max(0),
            attempt_ids: turn_attempt_ids(facts, previous.seq),
            confidence: Confidence::Inferred,
            count: 1,
            policy: SpanPolicy::Interval,
        });
    }
}

/// Whether an attempt is a successful edit-tool call that recorded at least one edit.
fn is_recorded_edit(a: &AttemptFact) -> bool {
    a.is_edit && a.outcome == Outcome::Ok && !a.edits.is_empty()
}

/// C3 (a): E1 changes a path from x to y and a later E2 on the same path in the same lane changes it back
/// from y to x. Both hashes must be non-empty. The nearest matching E1 wins; the edits of that path
/// between the two are part of the finding.
fn c3_edit_pairs(facts: &SessionFacts, out: &mut RuleOutput) {
    let mut lanes: Vec<&str> = Vec::new();
    for a in &facts.attempts {
        if !lanes.contains(&a.lane.as_str()) {
            lanes.push(a.lane.as_str());
        }
    }
    for lane in lanes {
        let edits: Vec<&AttemptFact> = lane_attempts(facts, lane)
            .filter(|a| is_recorded_edit(a))
            .collect();
        let mut seen: Vec<(String, String)> = Vec::new();
        for (j, second) in edits.iter().enumerate() {
            for undo in &second.edits {
                if undo.before.is_empty() || undo.after.is_empty() {
                    continue;
                }
                let mut found: Option<usize> = None;
                for k in (0..j).rev() {
                    let matches_pair = edits[k].edits.iter().any(|done| {
                        done.path == undo.path
                            && !done.before.is_empty()
                            && !done.after.is_empty()
                            && done.after == undo.before
                            && done.before == undo.after
                    });
                    if matches_pair {
                        found = Some(k);
                        break;
                    }
                }
                let Some(k) = found else {
                    continue;
                };
                let first = edits[k];
                let pair = (first.attempt_id.clone(), second.attempt_id.clone());
                if seen.contains(&pair) {
                    continue;
                }
                seen.push(pair);
                let mut ids = vec![first.attempt_id.clone()];
                for between in &edits[k + 1..j] {
                    if between.edits.iter().any(|edit| edit.path == undo.path)
                        && !ids.contains(&between.attempt_id)
                    {
                        ids.push(between.attempt_id.clone());
                    }
                }
                ids.push(second.attempt_id.clone());
                out.findings.push(Finding {
                    rule_id: "C3",
                    lane: lane.to_string(),
                    started_ms: first.started_ms,
                    ended_ms: second.done_ms,
                    cost_ms: (second.done_ms - first.started_ms).max(0),
                    attempt_ids: ids,
                    confidence: Confidence::Exact,
                    count: 1,
                    policy: SpanPolicy::Attempts,
                });
            }
        }
    }
}

/// Start of the last successful commit before `before_ms`, or `i64::MIN` for the session start.
fn last_commit_before(facts: &SessionFacts, before_ms: i64) -> i64 {
    facts
        .attempts
        .iter()
        .filter(|a| a.is_commit && a.outcome == Outcome::Ok && a.started_ms < before_ms)
        .map(|a| a.started_ms)
        .max()
        .unwrap_or(i64::MIN)
}

/// Successful edit-tool attempts after `from_ms` and before `to_ms`, any lane, in start order.
fn edit_tool_calls_between<'a>(
    facts: &'a SessionFacts,
    from_ms: i64,
    to_ms: i64,
) -> Vec<&'a AttemptFact> {
    facts
        .attempts
        .iter()
        .filter(|a| {
            a.is_edit && a.outcome == Outcome::Ok && from_ms < a.started_ms && a.started_ms < to_ms
        })
        .collect()
}

/// Whether the edit-tool attempt touched this path, by its recorded edits or its written files.
fn touches(a: &AttemptFact, path: &str) -> bool {
    a.edits.iter().any(|edit| edit.path == path) || a.files.iter().any(|file| file == path)
}

/// The distinct paths an attempt touched.
fn touched_paths(a: &AttemptFact) -> Vec<String> {
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for edit in &a.edits {
        paths.insert(edit.path.clone());
    }
    for file in &a.files {
        paths.insert(file.clone());
    }
    paths.into_iter().collect()
}

/// C3 (b) and (c): a shell revert after edits. With paths (`revert_with_paths` and non-empty `files`) the
/// edits of exactly those paths since the last commit were undone (exact). Pathless (`revert_pathless`)
/// every edit since the last commit was (inferred). No edits means no finding.
fn c3_reverts(facts: &SessionFacts, out: &mut RuleOutput) {
    let interpreters = &facts.cmds.interpreters;
    for revert in &facts.attempts {
        if !revert.is_shell || revert.outcome != Outcome::Ok {
            continue;
        }
        let Some(program) = revert.cmd_program.as_deref() else {
            continue;
        };
        let with_paths = matches_any(&facts.cmds.revert_with_paths, program, interpreters)
            && !revert.files.is_empty();
        let pathless =
            !with_paths && matches_any(&facts.cmds.revert_pathless, program, interpreters);
        if !with_paths && !pathless {
            continue;
        }
        let floor = last_commit_before(facts, revert.started_ms);
        let candidates = edit_tool_calls_between(facts, floor, revert.started_ms);
        let (undone, count, confidence): (Vec<&AttemptFact>, i64, Confidence) = if with_paths {
            let hit: Vec<&AttemptFact> = candidates
                .into_iter()
                .filter(|a| revert.files.iter().any(|file| touches(a, file)))
                .collect();
            let files_hit = revert
                .files
                .iter()
                .filter(|file| hit.iter().any(|a| touches(a, file)))
                .count() as i64;
            (hit, files_hit, Confidence::Exact)
        } else {
            let mut paths: BTreeSet<String> = BTreeSet::new();
            for a in &candidates {
                for path in touched_paths(a) {
                    paths.insert(path);
                }
            }
            let count = (paths.len() as i64).max(1);
            (candidates, count, Confidence::Inferred)
        };
        let Some(first) = undone.first() else {
            continue;
        };
        let started_ms = first.started_ms;
        let mut ids: Vec<String> = undone.iter().map(|a| a.attempt_id.clone()).collect();
        ids.push(revert.attempt_id.clone());
        out.findings.push(Finding {
            rule_id: "C3",
            lane: revert.lane.clone(),
            started_ms,
            ended_ms: revert.done_ms,
            cost_ms: (revert.done_ms - started_ms).max(0),
            attempt_ids: ids,
            confidence,
            count,
            policy: SpanPolicy::Attempts,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules::evaluate;
    use crate::devtime_rules_fixture::{at, script};

    fn findings_of(text: &str, rule: &str) -> Vec<Finding> {
        let facts = script(text).facts(&DevtimeRulesConfig::default());
        run(&facts, &DevtimeRulesConfig::default())
            .findings
            .into_iter()
            .filter(|finding| finding.rule_id == rule)
            .collect()
    }

    #[test]
    fn c1_fires_on_an_interrupted_turn_with_two_tools() {
        let found = findings_of(
            "
            turn main 0+20 interrupted=1
            read main 4+1 path=a.rs aid=r1
            grep main 8+1 aid=g1
            ",
            "C1",
        );
        assert_eq!(found.len(), 1);
        let finding = &found[0];
        assert_eq!(finding.lane, "main");
        assert_eq!(finding.started_ms, at(0));
        assert_eq!(finding.ended_ms, at(20));
        assert_eq!(
            finding.attempt_ids,
            vec!["r1".to_string(), "g1".to_string()]
        );
        assert_eq!(finding.confidence, Confidence::Exact);
    }

    #[test]
    fn c1_silent_on_a_normal_turn() {
        let found = findings_of(
            "
            turn main 0+20
            read main 4+1 path=a.rs aid=r1
            ",
            "C1",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn c2_fires_on_flagged_next_turn() {
        let found = findings_of(
            "
            turn main 0+20 correction=0
            read main 4+1 path=a.rs aid=r1
            turn main 30+10 correction=1
            ",
            "C2",
        );
        assert_eq!(found.len(), 1);
        let finding = &found[0];
        assert_eq!(finding.started_ms, at(0));
        assert_eq!(finding.ended_ms, at(20));
        assert_eq!(finding.attempt_ids, vec!["r1".to_string()]);
        assert_eq!(finding.confidence, Confidence::Inferred);
    }

    #[test]
    fn c2_silent_on_none_and_false() {
        let found = findings_of(
            "
            turn main 0+20
            read main 4+1 path=a.rs aid=r1
            turn main 30+10 correction=0
            turn main 50+10
            ",
            "C2",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn c2_never_fires_for_a_first_turn_flagged_alone() {
        let found = findings_of(
            "
            turn main 0+20 correction=1
            ",
            "C2",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn c3_edit_pair_revert_fires() {
        let found = findings_of(
            "
            turn main 0+40
            edit main 3+1 path=a.rs before=x after=y aid=e1
            edit main 8+1 path=a.rs before=y after=x aid=e2
            ",
            "C3",
        );
        assert_eq!(found.len(), 1);
        let finding = &found[0];
        assert_eq!(
            finding.attempt_ids,
            vec!["e1".to_string(), "e2".to_string()]
        );
        assert_eq!(finding.confidence, Confidence::Exact);
        assert_eq!(finding.started_ms, at(3));
        assert_eq!(finding.ended_ms, at(9));
        assert_eq!(finding.lane, "main");
    }

    #[test]
    fn c3_edit_pair_names_the_edits_of_that_path_between() {
        let found = findings_of(
            "
            turn main 0+40
            edit main 3+1 path=a.rs before=x after=y aid=e1
            edit main 6+1 path=a.rs before=y after=z aid=mid
            edit main 8+1 path=b.rs aid=other
            edit main 10+1 path=a.rs before=y after=x aid=e2
            ",
            "C3",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].attempt_ids,
            vec!["e1".to_string(), "mid".to_string(), "e2".to_string()]
        );
    }

    #[test]
    fn c3_edit_pair_silent_when_the_content_moves_on() {
        let found = findings_of(
            "
            turn main 0+40
            edit main 3+1 path=a.rs before=x after=y aid=e1
            edit main 8+1 path=a.rs before=y after=z aid=e2
            ",
            "C3",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn c3_edit_pair_silent_across_paths() {
        let found = findings_of(
            "
            turn main 0+40
            edit main 3+1 path=a.rs before=x after=y aid=e1
            edit main 8+1 path=b.rs before=y after=x aid=e2
            ",
            "C3",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn c3_git_restore_with_paths_fires() {
        let found = findings_of(
            "
            turn main 0+60
            edit main 3+1 path=a.rs aid=old
            bash main 5+1 prog=\"git commit\" aid=c1
            edit main 8+1 path=a.rs aid=e1
            edit main 10+1 path=b.rs aid=e2
            bash main 13+1 prog=\"git restore\" files=a.rs aid=r
            ",
            "C3",
        );
        assert_eq!(found.len(), 1);
        let finding = &found[0];
        assert_eq!(finding.attempt_ids, vec!["e1".to_string(), "r".to_string()]);
        assert_eq!(finding.confidence, Confidence::Exact);
        assert_eq!(finding.count, 1);
        assert_eq!(finding.started_ms, at(8));
        assert_eq!(finding.ended_ms, at(14));
    }

    #[test]
    fn c3_git_restore_without_prior_edits_is_silent() {
        let found = findings_of(
            "
            turn main 0+60
            edit main 3+1 path=b.rs aid=e1
            bash main 13+1 prog=\"git restore\" files=a.rs aid=r
            ",
            "C3",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn c3_pathless_reset_is_inferred() {
        let found = findings_of(
            "
            turn main 0+60
            edit main 3+1 path=a.rs aid=e1
            edit main 6+1 path=b.rs aid=e2
            bash main 9+1 prog=\"git reset\" aid=r
            ",
            "C3",
        );
        assert_eq!(found.len(), 1);
        let finding = &found[0];
        assert_eq!(
            finding.attempt_ids,
            vec!["e1".to_string(), "e2".to_string(), "r".to_string()]
        );
        assert_eq!(finding.confidence, Confidence::Inferred);
        assert_eq!(finding.count, 2);
    }

    #[test]
    fn c3_silent_on_forward_edits() {
        let found = findings_of(
            "
            turn main 0+60
            edit main 3+1 path=a.rs before=x after=y aid=e1
            edit main 6+1 path=a.rs before=y after=z aid=e2
            edit main 9+1 path=b.rs aid=e3
            bash main 12+1 prog=\"git commit\" aid=c1
            ",
            "C3",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn c4_is_never_emitted() {
        let facts = script(
            "
            turn main 0+60 interrupted=1
            edit main 3+1 path=a.rs before=x after=y aid=e1
            edit main 6+1 path=a.rs before=y after=x aid=e2
            bash main 9+1 prog=\"git reset\" aid=r
            turn main 70+10 correction=1
            ",
        )
        .facts(&DevtimeRulesConfig::default());
        let cfg = DevtimeRulesConfig::default();
        assert!(run(&facts, &cfg).findings.iter().all(|f| f.rule_id != "C4"));
        let evaluated = evaluate(&facts, &cfg);
        assert!(evaluated.findings.iter().all(|f| f.rule_id != "C4"));
        assert!(evaluated.findings.iter().any(|f| f.rule_id == "C3"));
    }
}
