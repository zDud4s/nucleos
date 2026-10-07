//! Family B: repeated and avoidable failures (B1 to B7): precision, project and environment knowledge,
//! estimation, machine and permission failures. Rules read the session's facts and never write a row.
//!
//! Every rule here is a pair: a failed attempt X, carrying one class of the closed error vocabulary
//! (`devtime_parse::ERROR_CLASSES`), and the attempt Y that followed it in the same lane. The classes
//! are decided at parse time from `vocab.error_signatures` in the config; this file only reads the
//! stored token, so it holds no threshold and no phrase list of its own. Findings carry attempt ids,
//! times and counts, never any text of the session.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{
    AttemptFact, Confidence, Finding, Outcome, RuleOutput, SessionFacts, SpanPolicy, a_fail,
    duration, next_in_lane, passed,
};

/// The closed error-class tokens this family reads (all members of `devtime_parse::ERROR_CLASSES`).
const EDIT_NOT_FOUND: &str = "edit_not_found";
const HOOK_BLOCK: &str = "hook_block";
const WRONG_SHELL: &str = "wrong_shell";
const TIMEOUT: &str = "timeout";
const EXIT_75: &str = "exit_75";
const PERMISSION_DENIED: &str = "permission_denied";

pub fn run(facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
    let mut findings = Vec::new();
    b1_to_b6(facts, &mut findings);
    b7(facts, &mut findings);
    RuleOutput {
        findings,
        verdicts: Vec::new(),
    }
}

/// The paths an attempt touched: its recorded edits first, then its written files.
fn paths_of(a: &AttemptFact) -> Vec<&str> {
    let mut paths: Vec<&str> = a.edits.iter().map(|edit| edit.path.as_str()).collect();
    paths.extend(a.files.iter().map(String::as_str));
    paths
}

/// A finding over `[x.started, y.started]` on x's lane: the time between a failure and its retry.
fn between(rule_id: &'static str, x: &AttemptFact, y: &AttemptFact) -> Finding {
    let ended_ms = y.started_ms.max(x.started_ms);
    Finding {
        rule_id,
        lane: x.lane.clone(),
        started_ms: x.started_ms,
        ended_ms,
        cost_ms: ended_ms - x.started_ms,
        attempt_ids: vec![x.attempt_id.clone(), y.attempt_id.clone()],
        confidence: Confidence::Exact,
        count: 1,
        policy: SpanPolicy::Interval,
    }
}

/// A finding that claims only the failed attempt itself, with no retry to name.
fn alone(rule_id: &'static str, x: &AttemptFact) -> Finding {
    Finding {
        rule_id,
        lane: x.lane.clone(),
        started_ms: x.started_ms,
        ended_ms: x.done_ms.max(x.started_ms),
        cost_ms: duration(x),
        attempt_ids: vec![x.attempt_id.clone()],
        confidence: Confidence::Exact,
        count: 1,
        policy: SpanPolicy::Attempts,
    }
}

/// `between` when there is a retry, `alone` when there is not.
fn between_or_alone(rule_id: &'static str, x: &AttemptFact, y: Option<&AttemptFact>) -> Finding {
    match y {
        Some(y) => between(rule_id, x, y),
        None => alone(rule_id, x),
    }
}

/// A finding that names the failed attempt and its retry but claims only the failed attempt's spans:
/// the retry is the work the failure should have been, so its time is not waste.
fn claims_only_first(
    facts: &SessionFacts,
    rule_id: &'static str,
    x: &AttemptFact,
    y: &AttemptFact,
) -> Finding {
    let span_ids: Vec<i64> = facts
        .spans
        .iter()
        .filter(|span| span.attempt_ids.contains(&x.attempt_id))
        .map(|span| span.id)
        .collect();
    Finding {
        rule_id,
        lane: x.lane.clone(),
        started_ms: x.started_ms,
        ended_ms: x.done_ms.max(x.started_ms),
        cost_ms: duration(x),
        attempt_ids: vec![x.attempt_id.clone(), y.attempt_id.clone()],
        confidence: Confidence::Exact,
        count: 1,
        policy: SpanPolicy::Spans(span_ids),
    }
}

fn b1_to_b6(facts: &SessionFacts, findings: &mut Vec<Finding>) {
    for x in &facts.attempts {
        let Some(class) = x.error_class.as_deref() else {
            continue;
        };
        match class {
            // B1: an edit that did not find its text, then the same path edited successfully.
            EDIT_NOT_FOUND if x.is_edit => {
                let x_paths = paths_of(x);
                let y = if x_paths.is_empty() {
                    None
                } else {
                    next_in_lane(facts, x.idx, |a| {
                        a.is_edit
                            && a.outcome == Outcome::Ok
                            && paths_of(a).iter().any(|path| x_paths.contains(path))
                    })
                };
                findings.push(between_or_alone("B1", x, y));
            }
            // B2: a hook refused the call and the same tool was tried again.
            HOOK_BLOCK => {
                let y = next_in_lane(facts, x.idx, |a| {
                    a.tool_name == x.tool_name && (!x.is_shell || a.cmd_program == x.cmd_program)
                });
                if let Some(y) = y {
                    findings.push(between("B2", x, y));
                }
            }
            // B3: the wrong shell, then a shell call that worked. A row written by parser v1 never
            // carries this class, so such a session stays silent.
            WRONG_SHELL => {
                let y = next_in_lane(facts, x.idx, |a| a.is_shell && a.outcome == Outcome::Ok);
                findings.push(between_or_alone("B3", x, y));
            }
            // B4: a timeout, then the same command with a longer timeout or in the background.
            TIMEOUT => {
                let Some(hash) = x.cmd_hash.as_deref() else {
                    continue;
                };
                let x_timeout = x.timeout_ms.unwrap_or(0);
                let y = next_in_lane(facts, x.idx, |a| {
                    a.cmd_hash.as_deref() == Some(hash)
                        && (a.timeout_ms.unwrap_or(0) > x_timeout || a.background)
                });
                if let Some(y) = y {
                    findings.push(claims_only_first(facts, "B4", x, y));
                }
            }
            // B5: the machine was busy (exit 75), and the same command ran again.
            EXIT_75 => {
                let Some(hash) = x.cmd_hash.as_deref() else {
                    continue;
                };
                let y = next_in_lane(facts, x.idx, |a| a.cmd_hash.as_deref() == Some(hash));
                if let Some(y) = y {
                    findings.push(between("B5", x, y));
                }
            }
            // B6: a permission was denied and something else was tried instead. The same call again
            // is not an alternative, and a shell denial is answered by a shell alternative.
            PERMISSION_DENIED => {
                let y = next_in_lane(facts, x.idx, |a| {
                    (a.tool_name != x.tool_name || a.cmd_hash != x.cmd_hash)
                        && a.is_shell == x.is_shell
                });
                if let Some(y) = y {
                    findings.push(between("B6", x, y));
                }
            }
            _ => {}
        }
    }
}

/// A run of one program's calls in one lane, from its first failure.
struct Run<'a> {
    program: &'a str,
    fails: Vec<&'a AttemptFact>,
}

/// B7: the same program failing under several different commands before one works, with no edit in
/// between (only reads and searches may sit between the calls).
fn b7(facts: &SessionFacts, findings: &mut Vec<Finding>) {
    let mut runs: BTreeMap<String, Run> = BTreeMap::new();
    for a in &facts.attempts {
        if a.is_read || a.is_search {
            continue;
        }
        let program = if a.is_shell {
            a.cmd_program.as_deref()
        } else {
            None
        };
        let Some(program) = program else {
            runs.remove(&a.lane);
            continue;
        };
        let same_program = runs.get(&a.lane).is_some_and(|run| run.program == program);
        if !same_program || a.mutating {
            runs.remove(&a.lane);
        }
        if a.mutating {
            continue;
        }
        if a_fail(a) {
            runs.entry(a.lane.clone())
                .or_insert_with(|| Run {
                    program,
                    fails: Vec::new(),
                })
                .fails
                .push(a);
        } else if passed(a) {
            if let Some(run) = runs.remove(&a.lane)
                && let Some(finding) = finish_run(&run, a)
            {
                findings.push(finding);
            }
        } else {
            // A timeout, an interruption or a launch: the streak is not a plain retry loop.
            runs.remove(&a.lane);
        }
    }
}

fn finish_run(run: &Run, pass: &AttemptFact) -> Option<Finding> {
    let first = run.fails.first()?;
    let distinct: BTreeSet<&str> = run
        .fails
        .iter()
        .filter_map(|fail| fail.cmd_hash.as_deref())
        .collect();
    if distinct.len() < 2 {
        return None;
    }
    let ended_ms = pass.started_ms.max(first.started_ms);
    let mut attempt_ids: Vec<String> = run.fails.iter().map(|f| f.attempt_id.clone()).collect();
    attempt_ids.push(pass.attempt_id.clone());
    Some(Finding {
        rule_id: "B7",
        lane: first.lane.clone(),
        started_ms: first.started_ms,
        ended_ms,
        cost_ms: ended_ms - first.started_ms,
        attempt_ids,
        confidence: Confidence::Exact,
        count: run.fails.len() as i64,
        policy: SpanPolicy::Interval,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules_fixture::{at, script};

    fn found(text: &str) -> Vec<Finding> {
        let facts = script(text).facts(&DevtimeRulesConfig::default());
        run(&facts, &DevtimeRulesConfig::default()).findings
    }

    fn ids(finding: &Finding) -> Vec<&str> {
        finding.attempt_ids.iter().map(String::as_str).collect()
    }

    // --- B1 ---

    #[test]
    fn b1_fires_when_an_edit_misses_and_the_same_path_is_edited_ok() {
        let findings = found(
            "
            turn main 0
            edit main 2+1 path=a.rs out=error err=edit_not_found aid=x
            edit main 6+1 path=a.rs aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "B1");
        assert_eq!(ids(f), vec!["x", "y"]);
        assert_eq!(f.started_ms, at(2));
        assert_eq!(f.ended_ms, at(6));
        assert_eq!(f.confidence, Confidence::Exact);
        assert!(matches!(f.policy, SpanPolicy::Interval));
    }

    #[test]
    fn b1_without_a_retry_claims_the_failed_attempt_alone() {
        let findings = found(
            "
            turn main 0
            edit main 2+1 path=a.rs out=error err=edit_not_found aid=x
            edit main 6+1 path=other.rs aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(ids(&findings[0]), vec!["x"]);
        assert!(matches!(findings[0].policy, SpanPolicy::Attempts));
    }

    #[test]
    fn b1_silent_on_a_tool_error() {
        let findings = found(
            "
            turn main 0
            edit main 2+1 path=a.rs out=error err=tool_error aid=x
            edit main 6+1 path=a.rs aid=y
            ",
        );
        assert!(findings.is_empty());
    }

    // --- B2 ---

    #[test]
    fn b2_fires_when_a_blocked_command_is_tried_again() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"git commit\" out=error err=hook_block aid=x
            bash main 6+1 prog=\"git commit\" aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "B2");
        assert_eq!(ids(f), vec!["x", "y"]);
        assert_eq!(f.started_ms, at(2));
        assert_eq!(f.ended_ms, at(6));
    }

    #[test]
    fn b2_silent_without_a_new_attempt() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"git commit\" out=error err=hook_block aid=x
            ",
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn b2_silent_when_the_next_shell_call_is_another_program() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"git commit\" out=error err=hook_block aid=x
            bash main 6+1 prog=\"git status\" aid=y
            ",
        );
        assert!(findings.is_empty());
    }

    // --- B3 ---

    #[test]
    fn b3_fires_when_the_wrong_shell_is_followed_by_a_working_one() {
        let findings = found(
            "
            turn main 0
            ps main 2+1 prog=\"ls -la\" out=error err=wrong_shell aid=x
            ps main 6+1 prog=\"Get-ChildItem\" aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "B3");
        assert_eq!(ids(f), vec!["x", "y"]);
        assert_eq!(f.ended_ms, at(6));
    }

    #[test]
    fn b3_without_a_working_call_claims_the_attempt_alone() {
        let findings = found(
            "
            turn main 0
            ps main 2+1 prog=\"ls -la\" out=error err=wrong_shell aid=x
            ",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(ids(&findings[0]), vec!["x"]);
    }

    #[test]
    fn b3_silent_on_a_plain_nonzero_exit() {
        let findings = found(
            "
            turn main 0
            ps main 2+1 prog=\"ls -la\" out=error exit=1 aid=x
            ps main 6+1 prog=\"Get-ChildItem\" aid=y
            ",
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn b3_silent_on_parser_v1_rows() {
        // Parser v1 never stores the class, so the failure reads as a plain nonzero exit.
        let findings = found(
            "
            turn main 0
            ps main 2+1 prog=\"ls -la\" out=error exit=1 pv=1 aid=x
            ps main 6+1 prog=\"Get-ChildItem\" pv=1 aid=y
            ",
        );
        assert!(findings.is_empty());
    }

    // --- B4 ---

    #[test]
    fn b4_fires_when_a_timeout_is_rerun_with_a_longer_timeout() {
        let findings = found(
            "
            turn main 0
            bash main 2+120 prog=\"cargo test\" timeout=120000 out=error err=timeout aid=x
            bash main 130+5 prog=\"cargo test\" timeout=600000 aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "B4");
        assert_eq!(ids(f), vec!["x", "y"]);
        assert_eq!(f.started_ms, at(2));
        assert_eq!(f.ended_ms, at(122));
        assert!(matches!(f.policy, SpanPolicy::Spans(_)));
    }

    #[test]
    fn b4_fires_when_the_rerun_goes_to_the_background() {
        let findings = found(
            "
            turn main 0
            bash main 2+120 prog=\"cargo test\" timeout=120000 out=error err=timeout aid=x
            bash main 130+0 prog=\"cargo test\" bg=1 bg_end=200 bg_status=completed aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "B4");
    }

    #[test]
    fn b4_silent_when_the_rerun_keeps_the_same_timeout() {
        let findings = found(
            "
            turn main 0
            bash main 2+120 prog=\"cargo test\" timeout=120000 out=error err=timeout aid=x
            bash main 130+5 prog=\"cargo test\" timeout=120000 aid=y
            ",
        );
        assert!(findings.is_empty());
    }

    // --- B5 ---

    #[test]
    fn b5_fires_when_an_exit_75_command_runs_again() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"cargo build\" out=error exit=75 err=exit_75 aid=x
            bash main 9+3 prog=\"cargo build\" aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "B5");
        assert_eq!(ids(f), vec!["x", "y"]);
        assert_eq!(f.started_ms, at(2));
        assert_eq!(f.ended_ms, at(9));
    }

    #[test]
    fn b5_silent_when_the_next_command_differs() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"cargo build\" out=error exit=75 err=exit_75 aid=x
            bash main 9+3 prog=\"cargo test\" aid=y
            ",
        );
        assert!(findings.is_empty());
    }

    // --- B6 ---

    #[test]
    fn b6_fires_when_a_denied_command_is_replaced_by_another() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"rm\" out=error err=permission_denied aid=x
            bash main 6+1 prog=\"trash\" aid=y
            ",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "B6");
        assert_eq!(ids(f), vec!["x", "y"]);
        assert_eq!(f.ended_ms, at(6));
    }

    #[test]
    fn b6_silent_when_the_same_command_comes_back_or_the_session_ends() {
        let same = found(
            "
            turn main 0
            bash main 2+1 prog=\"rm\" out=error err=permission_denied aid=x
            bash main 6+1 prog=\"rm\" aid=y
            ",
        );
        assert!(same.is_empty());
        let ended = found(
            "
            turn main 0
            bash main 2+1 prog=\"rm\" out=error err=permission_denied aid=x
            ",
        );
        assert!(ended.is_empty());
    }

    // --- B7 ---

    #[test]
    fn b7_fires_when_one_program_fails_under_two_commands_then_passes() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"gh pr\" hash=h1 out=error exit=1 aid=f1
            read main 4+1 path=a.rs aid=r1
            bash main 6+1 prog=\"gh pr\" hash=h2 out=error exit=1 aid=f2
            bash main 9+1 prog=\"gh pr\" hash=h3 aid=ok
            ",
        );
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.rule_id, "B7");
        assert_eq!(ids(f), vec!["f1", "f2", "ok"]);
        assert_eq!(f.started_ms, at(2));
        assert_eq!(f.ended_ms, at(9));
        assert_eq!(f.count, 2);
    }

    #[test]
    fn b7_silent_when_the_same_command_fails_then_passes() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"gh pr\" hash=h1 out=error exit=1 aid=f1
            bash main 6+1 prog=\"gh pr\" hash=h1 aid=ok
            ",
        );
        assert!(findings.is_empty());
    }

    #[test]
    fn b7_silent_when_an_edit_sits_between_the_failures() {
        let findings = found(
            "
            turn main 0
            bash main 2+1 prog=\"gh pr\" hash=h1 out=error exit=1 aid=f1
            edit main 4+1 path=a.rs aid=e1
            bash main 6+1 prog=\"gh pr\" hash=h2 out=error exit=1 aid=f2
            bash main 9+1 prog=\"gh pr\" hash=h3 aid=ok
            ",
        );
        assert!(findings.is_empty());
    }
}
