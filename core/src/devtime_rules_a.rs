//! Family A: verification and rework loops (A1 to A8) and the three-valued verification verdicts. Rules read
//! the session's facts and the thresholds in `DevtimeRulesConfig`; they never write a row.
//!
//! - A1/A2: a test (A1, by command hash) or a build (A2, by program) fails, the tree is edited, the same
//!   check passes. An unresolved cycle of two or more failures with an edit inside is reported as inferred.
//! - A3: a lint failure that a later lint run fixed; no edit is required (the fix may be a formatter).
//! - A4: an implementer agent reported success and the next main-lane check fails. Also the verdict
//!   (`passed`, `failed`, `not_measured`) of every such agent.
//! - A5: a review round (an implementer runs again after a reviewer).
//! - A6: an agent failed and the same agent type was launched again.
//! - A7: a subagent ended badly (failed, killed, or stopped on a tool call with no final result).
//! - A8: a check failed and the very next run of the same command passed with no edit in between (flaky).
//!
//! Findings carry only ids, hashes and closed tokens, never message text. Every vocabulary (command
//! classes, roles, programs) is resolved upstream into the facts; the only literals here are the stored
//! closed tokens of the transcript (`agent`, `failed`, `killed`).

use std::collections::BTreeMap;

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{
    AttemptFact, Confidence, Finding, Outcome, Role, RuleOutput, SessionFacts, SpanPolicy, Verdict,
    Verified, a_fail, duration, edits_between, passed,
};
use crate::devtime_rules_cmd::CmdClass;

pub fn run(facts: &SessionFacts, _cfg: &DevtimeRulesConfig) -> RuleOutput {
    let mut out = RuleOutput::default();
    out.findings.extend(a1(facts));
    out.findings.extend(a2(facts));
    out.findings.extend(a3(facts));
    let a4_out = a4(facts);
    out.findings.extend(a4_out.findings);
    out.verdicts.extend(a4_out.verdicts);
    out.findings.extend(a5(facts));
    let a6_out = a6(facts);
    out.findings.extend(a6_out.findings);
    out.verdicts.extend(a6_out.verdicts);
    out.findings.extend(a7(facts));
    out.findings.extend(a8(facts));
    out
}

// --- shared small helpers ---------------------------------------------------------------------------

/// A shell call that the command lists classified as a check (test, build or lint).
fn is_verif(a: &AttemptFact) -> bool {
    a.is_shell && a.cmd_class.is_some()
}

/// An agent attempt that ended badly: a failed foreground call, or a background one whose notification
/// said `failed` or `killed`. A foreground interrupt is not one (that is C1's).
fn failed_agent(a: &AttemptFact) -> bool {
    a.outcome == Outcome::Error || matches!(a.bg_status.as_deref(), Some("failed") | Some("killed"))
}

/// A finding that claims the spans of its lane inside `[start, end]`; `end` is never before `start`.
fn interval_finding(
    rule_id: &'static str,
    lane: &str,
    start: i64,
    end: i64,
    attempt_ids: Vec<String>,
    confidence: Confidence,
    count: i64,
) -> Finding {
    let end = end.max(start);
    Finding {
        rule_id,
        lane: lane.to_string(),
        started_ms: start,
        ended_ms: end,
        cost_ms: end - start,
        attempt_ids,
        confidence,
        count,
        policy: SpanPolicy::Interval,
    }
}

/// The ids of the spans that carry this attempt. Used where a finding names several attempts but must
/// claim the time of one of them only.
fn span_ids_of(facts: &SessionFacts, attempt_id: &str) -> Vec<i64> {
    facts
        .spans
        .iter()
        .filter(|span| span.attempt_ids.iter().any(|id| id == attempt_id))
        .map(|span| span.id)
        .collect()
}

fn key_hash(a: &AttemptFact) -> Option<String> {
    a.cmd_hash.clone()
}

fn key_program(a: &AttemptFact) -> Option<String> {
    a.cmd_program.clone()
}

/// Check runs of the given classes, grouped by `(lane, key)`, each group in session order.
fn grouped<'a>(
    facts: &'a SessionFacts,
    classes: &[CmdClass],
    key: fn(&AttemptFact) -> Option<String>,
) -> BTreeMap<(String, String), Vec<&'a AttemptFact>> {
    let mut groups: BTreeMap<(String, String), Vec<&'a AttemptFact>> = BTreeMap::new();
    for a in &facts.attempts {
        if !is_verif(a) {
            continue;
        }
        let Some(class) = a.cmd_class else {
            continue;
        };
        if !classes.contains(&class) {
            continue;
        }
        let Some(group_key) = key(a) else {
            continue;
        };
        groups
            .entry((a.lane.clone(), group_key))
            .or_default()
            .push(a);
    }
    groups
}

fn sort_findings(found: &mut [Finding]) {
    found.sort_by(|a, b| {
        a.started_ms
            .cmp(&b.started_ms)
            .then_with(|| a.attempt_ids.cmp(&b.attempt_ids))
    });
}

// --- A1 / A2: fail, edit, pass ----------------------------------------------------------------------

/// The shared shape of A1 and A2. A failing run opens a cycle, later failures stay in it, and the first
/// pass closes it. The cycle is a finding when the tree was edited between the first failure and the
/// pass. A cycle still open at the end with at least two failures and an edit inside is inferred.
fn fail_edit_pass(
    facts: &SessionFacts,
    rule_id: &'static str,
    class: CmdClass,
    key: fn(&AttemptFact) -> Option<String>,
) -> Vec<Finding> {
    let mut found: Vec<Finding> = Vec::new();
    let groups = grouped(facts, &[class], key);
    for runs in groups.values() {
        let mut cycle: Vec<&AttemptFact> = Vec::new();
        for &run in runs.iter() {
            if a_fail(run) {
                cycle.push(run);
            } else if passed(run) && !cycle.is_empty() {
                let first = cycle[0];
                if edits_between(facts, first.done_ms, run.started_ms)
                    .next()
                    .is_some()
                {
                    let mut ids: Vec<String> = cycle.iter().map(|f| f.attempt_id.clone()).collect();
                    ids.push(run.attempt_id.clone());
                    found.push(interval_finding(
                        rule_id,
                        &first.lane,
                        first.done_ms,
                        run.started_ms,
                        ids,
                        Confidence::Exact,
                        cycle.len() as i64,
                    ));
                }
                cycle.clear();
            }
        }
        if cycle.len() >= 2 {
            let first = cycle[0];
            let last = cycle[cycle.len() - 1];
            if edits_between(facts, first.done_ms, last.started_ms)
                .next()
                .is_some()
            {
                let ids: Vec<String> = cycle.iter().map(|f| f.attempt_id.clone()).collect();
                found.push(interval_finding(
                    rule_id,
                    &first.lane,
                    first.done_ms,
                    last.started_ms,
                    ids,
                    Confidence::Inferred,
                    cycle.len() as i64,
                ));
            }
        }
    }
    sort_findings(&mut found);
    found
}

/// A1: a test failed, the tree was edited, the same command (same hash) passed.
fn a1(facts: &SessionFacts) -> Vec<Finding> {
    fail_edit_pass(facts, "A1", CmdClass::Test, key_hash)
}

/// A2: a build failed, the tree was edited, the same program passed.
fn a2(facts: &SessionFacts) -> Vec<Finding> {
    fail_edit_pass(facts, "A2", CmdClass::Build, key_program)
}

// --- A3: lint failed, then fixed --------------------------------------------------------------------

/// A3: a lint run failed and a later one passed. The pass is the next run with the same hash; when none
/// exists, the first passing run of the same program at or after a run with a different hash. No edit is
/// required, because the fix may be the formatter itself.
fn a3(facts: &SessionFacts) -> Vec<Finding> {
    let lints: Vec<&AttemptFact> = facts
        .attempts
        .iter()
        .filter(|a| is_verif(a) && a.cmd_class == Some(CmdClass::Lint))
        .collect();
    let mut found: Vec<Finding> = Vec::new();
    let mut claimed: Vec<String> = Vec::new();
    for (pos, &fail) in lints.iter().enumerate() {
        if !a_fail(fail) {
            continue;
        }
        let rest: Vec<&AttemptFact> = lints
            .iter()
            .skip(pos + 1)
            .copied()
            .filter(|a| a.lane == fail.lane)
            .collect();
        let same_hash = rest
            .iter()
            .copied()
            .find(|a| passed(a) && a.cmd_hash == fail.cmd_hash);
        let fix = match same_hash {
            Some(pass) => Some(pass),
            None => {
                let other = rest
                    .iter()
                    .position(|a| a.cmd_program == fail.cmd_program && a.cmd_hash != fail.cmd_hash);
                match other {
                    Some(from) => rest[from..]
                        .iter()
                        .copied()
                        .find(|a| passed(a) && a.cmd_program == fail.cmd_program),
                    None => None,
                }
            }
        };
        let Some(pass) = fix else {
            continue;
        };
        // Several failures that end in one pass are one loop: the first failure claims it.
        if claimed.contains(&pass.attempt_id) {
            continue;
        }
        claimed.push(pass.attempt_id.clone());
        found.push(interval_finding(
            "A3",
            &fail.lane,
            fail.done_ms,
            pass.started_ms,
            vec![fail.attempt_id.clone(), pass.attempt_id.clone()],
            Confidence::Exact,
            1,
        ));
    }
    sort_findings(&mut found);
    found
}

// --- A4: implementer said done, the next check fails ------------------------------------------------

/// A4 and the verdict of every implementer agent that finished well: the first main-lane test or build
/// after it decides `passed` or `failed`; no such check means `not_measured`.
fn a4(facts: &SessionFacts) -> RuleOutput {
    let mut out = RuleOutput::default();
    for x in facts.attempts.iter().filter(|a| {
        a.kind == "agent"
            && a.role == Some(Role::Implementer)
            && a.agent_type.is_some()
            && a.outcome == Outcome::Ok
    }) {
        let check = facts.attempts.iter().find(|v| {
            v.is_main
                && is_verif(v)
                && matches!(v.cmd_class, Some(CmdClass::Test) | Some(CmdClass::Build))
                && v.started_ms >= x.done_ms
        });
        let verified = match check {
            None => Verified::NotMeasured,
            Some(v) if passed(v) => Verified::Passed,
            Some(v) if a_fail(v) => {
                out.findings.push(a4_finding(facts, x, v));
                Verified::Failed
            }
            Some(_) => Verified::NotMeasured,
        };
        out.verdicts.push(Verdict {
            attempt_id: x.attempt_id.clone(),
            verified,
            rule_id: "A4",
        });
    }
    out
}

/// The finding for an implementer `x` whose next check `v` failed: the time until the same command passed
/// again, or the check's own span when it never did.
fn a4_finding(facts: &SessionFacts, x: &AttemptFact, v: &AttemptFact) -> Finding {
    let confidence = if edits_between(facts, x.done_ms, v.started_ms).any(|a| a.is_main) {
        Confidence::Inferred
    } else {
        Confidence::Exact
    };
    let ids = vec![x.attempt_id.clone(), v.attempt_id.clone()];
    let next_pass = if v.cmd_hash.is_some() {
        facts.attempts.iter().find(|p| {
            p.is_main
                && p.is_shell
                && p.cmd_hash == v.cmd_hash
                && p.started_ms > v.started_ms
                && passed(p)
        })
    } else {
        None
    };
    match next_pass {
        Some(pass) => interval_finding(
            "A4",
            &v.lane,
            v.done_ms,
            pass.started_ms,
            ids,
            confidence,
            1,
        ),
        None => Finding {
            rule_id: "A4",
            lane: v.lane.clone(),
            started_ms: v.started_ms,
            ended_ms: v.done_ms.max(v.started_ms),
            cost_ms: duration(v),
            attempt_ids: ids,
            confidence,
            count: 1,
            // Only the check's time is claimed, not the implementer's span that also sits in `attempt_ids`.
            policy: SpanPolicy::Spans(span_ids_of(facts, &v.attempt_id)),
        },
    }
}

// --- A5: review rounds ------------------------------------------------------------------------------

/// A5: in the main lane, an implementer that runs again after a reviewer is one more round. `count` is the
/// number of rounds so far.
fn a5(facts: &SessionFacts) -> Vec<Finding> {
    let agents: Vec<&AttemptFact> = facts
        .attempts
        .iter()
        .filter(|a| a.is_main && a.kind == "agent")
        .collect();
    let mut found: Vec<Finding> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut rounds: i64 = 0;
    for &reviewer in agents.iter().filter(|a| a.role == Some(Role::Reviewer)) {
        let again = agents
            .iter()
            .copied()
            .find(|a| a.role == Some(Role::Implementer) && a.started_ms > reviewer.done_ms);
        let Some(implementer) = again else {
            continue;
        };
        // Two reviewers before one implementer are one round.
        if seen.contains(&implementer.attempt_id) {
            continue;
        }
        seen.push(implementer.attempt_id.clone());
        rounds += 1;
        found.push(Finding {
            rule_id: "A5",
            lane: implementer.lane.clone(),
            started_ms: implementer.started_ms,
            ended_ms: implementer.done_ms.max(implementer.started_ms),
            cost_ms: duration(implementer),
            attempt_ids: vec![implementer.attempt_id.clone()],
            confidence: Confidence::Exact,
            count: rounds,
            policy: SpanPolicy::Attempts,
        });
    }
    found
}

// --- A6: relaunch after an error --------------------------------------------------------------------

/// A6: an agent failed and the same agent type was launched again later in the lane. The failed agent's
/// verdict is `failed`.
fn a6(facts: &SessionFacts) -> RuleOutput {
    let mut out = RuleOutput::default();
    for x in facts
        .attempts
        .iter()
        .filter(|a| a.kind == "agent" && failed_agent(a))
    {
        let Some(agent_type) = x.agent_type.as_deref() else {
            continue;
        };
        let relaunch = facts.attempts.iter().find(|y| {
            y.kind == "agent"
                && y.lane == x.lane
                && y.idx > x.idx
                && y.started_ms >= x.done_ms
                && y.agent_type.as_deref() == Some(agent_type)
        });
        let Some(y) = relaunch else {
            continue;
        };
        out.findings.push(Finding {
            rule_id: "A6",
            lane: x.lane.clone(),
            started_ms: x.started_ms,
            ended_ms: x.done_ms.max(x.started_ms),
            cost_ms: duration(x),
            attempt_ids: vec![x.attempt_id.clone(), y.attempt_id.clone()],
            confidence: Confidence::Exact,
            count: 1,
            // The failed agent's time only: the relaunch is the redo, not the waste.
            policy: SpanPolicy::Spans(span_ids_of(facts, &x.attempt_id)),
        });
        out.verdicts.push(Verdict {
            attempt_id: x.attempt_id.clone(),
            verified: Verified::Failed,
            rule_id: "A6",
        });
    }
    out
}

// --- A7: subagent ended badly -----------------------------------------------------------------------

/// Whether the session holds anything on this lane.
fn lane_present(facts: &SessionFacts, lane: &str) -> bool {
    facts.spans.iter().any(|s| s.lane == lane)
        || facts.messages.iter().any(|m| m.lane == lane)
        || facts.attempts.iter().any(|a| a.lane == lane)
}

/// Whether the last message of the lane (by `last_ms`) still carried a tool call: the subagent stopped
/// without a final text.
fn ends_on_a_tool_call(facts: &SessionFacts, lane: &str) -> bool {
    facts
        .messages
        .iter()
        .filter(|m| m.lane == lane)
        .max_by_key(|m| m.last_ms)
        .is_some_and(|m| m.has_tool_use)
}

/// A7: an agent that failed or was killed (exact), or one that returned ok while its own lane ends on a
/// tool call with no final result (inferred).
fn a7(facts: &SessionFacts) -> Vec<Finding> {
    let mut found: Vec<Finding> = Vec::new();
    for x in facts.attempts.iter().filter(|a| a.kind == "agent") {
        let lane_name: Option<String> = x.agent_id.as_ref().map(|id| format!("agent:{id}"));
        let own_lane: Option<String> = match lane_name {
            Some(name) if lane_present(facts, &name) => Some(name),
            _ => None,
        };
        let confidence = if failed_agent(x) {
            Confidence::Exact
        } else if x.outcome == Outcome::Ok
            && own_lane
                .as_deref()
                .is_some_and(|lane| ends_on_a_tool_call(facts, lane))
        {
            Confidence::Inferred
        } else {
            continue;
        };
        let (policy, cost_ms) = match &own_lane {
            Some(lane) => {
                let total: i64 = facts
                    .spans
                    .iter()
                    .filter(|s| &s.lane == lane)
                    .map(|s| (s.ended_ms - s.started_ms).max(0))
                    .sum();
                let cost = if total > 0 { total } else { duration(x) };
                (SpanPolicy::Lane(lane.clone()), cost)
            }
            None => (SpanPolicy::Attempts, duration(x)),
        };
        found.push(Finding {
            rule_id: "A7",
            lane: x.lane.clone(),
            started_ms: x.started_ms,
            ended_ms: x.done_ms.max(x.started_ms),
            cost_ms,
            attempt_ids: vec![x.attempt_id.clone()],
            confidence,
            count: 1,
            policy,
        });
    }
    found
}

// --- A8: flaky --------------------------------------------------------------------------------------

/// A8: a test or build failed and the very next run of the same command passed with no edit in between.
fn a8(facts: &SessionFacts) -> Vec<Finding> {
    let mut found: Vec<Finding> = Vec::new();
    let groups = grouped(facts, &[CmdClass::Test, CmdClass::Build], key_hash);
    for runs in groups.values() {
        for pair in runs.windows(2) {
            let fail = pair[0];
            let pass = pair[1];
            if a_fail(fail)
                && passed(pass)
                && edits_between(facts, fail.done_ms, pass.started_ms)
                    .next()
                    .is_none()
            {
                found.push(interval_finding(
                    "A8",
                    &fail.lane,
                    fail.started_ms,
                    pass.started_ms,
                    vec![fail.attempt_id.clone(), pass.attempt_id.clone()],
                    Confidence::Exact,
                    1,
                ));
            }
        }
    }
    sort_findings(&mut found);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules_fixture::{at, script};

    fn out(src: &str) -> RuleOutput {
        let cfg = DevtimeRulesConfig::default();
        let facts = script(src).facts(&cfg);
        run(&facts, &cfg)
    }

    fn of<'a>(output: &'a RuleOutput, rule_id: &str) -> Vec<&'a Finding> {
        output
            .findings
            .iter()
            .filter(|f| f.rule_id == rule_id)
            .collect()
    }

    fn verdict(output: &RuleOutput, attempt_id: &str, rule_id: &str) -> Option<Verified> {
        output
            .verdicts
            .iter()
            .find(|v| v.attempt_id == attempt_id && v.rule_id == rule_id)
            .map(|v| v.verified)
    }

    fn ids(finding: &Finding) -> Vec<&str> {
        finding.attempt_ids.iter().map(|s| s.as_str()).collect()
    }

    // --- A1 ---

    #[test]
    fn a1_fires_after_fail_edit_green() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo test" out=error exit=101 aid=red
            edit main 20+1 path=core/src/a.rs aid=fix
            bash main 40+2 prog="cargo test" aid=green
        "#);
        let found = of(&o, "A1");
        assert_eq!(found.len(), 1);
        let f = found[0];
        assert_eq!(ids(f), vec!["red", "green"]);
        assert_eq!(f.started_ms, at(12));
        assert_eq!(f.ended_ms, at(40));
        assert_eq!(f.cost_ms, 28_000);
        assert_eq!(f.confidence, Confidence::Exact);
        assert_eq!(f.count, 1);
        assert_eq!(f.lane, "main");
        assert!(matches!(f.policy, SpanPolicy::Interval));
        assert!(of(&o, "A8").is_empty());
    }

    #[test]
    fn a1_silent_without_edits() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo test" out=error exit=101 aid=red
            bash main 40+2 prog="cargo test" aid=green
        "#);
        assert!(of(&o, "A1").is_empty());
        // Fail then pass with no edit is the flaky rule's.
        assert_eq!(of(&o, "A8").len(), 1);
    }

    #[test]
    fn a1_silent_on_a_different_hash() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo test" hash=h1 out=error exit=101 aid=red
            edit main 20+1 path=core/src/a.rs aid=fix
            bash main 40+2 prog="cargo test" hash=h2 aid=green
        "#);
        assert!(of(&o, "A1").is_empty());
    }

    #[test]
    fn a1_unresolved_cycle_is_inferred() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo test" out=error exit=101 aid=red1
            edit main 20+1 path=core/src/a.rs aid=fix
            bash main 30+2 prog="cargo test" out=error exit=101 aid=red2
        "#);
        let found = of(&o, "A1");
        assert_eq!(found.len(), 1);
        let f = found[0];
        assert_eq!(f.confidence, Confidence::Inferred);
        assert_eq!(ids(f), vec!["red1", "red2"]);
        assert_eq!(f.started_ms, at(12));
        assert_eq!(f.ended_ms, at(30));
        assert_eq!(f.count, 2);
    }

    #[test]
    fn a1_silent_on_a_single_unresolved_failure() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo test" out=error exit=101 aid=red1
            edit main 20+1 path=core/src/a.rs aid=fix
        "#);
        assert!(of(&o, "A1").is_empty());
    }

    // --- A2 ---

    #[test]
    fn a2_fires_after_build_fail_edit_pass() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo build" out=error exit=101 aid=red
            edit main 20+1 path=core/src/a.rs aid=fix
            bash main 30+2 prog="cargo build" aid=green
        "#);
        let found = of(&o, "A2");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["red", "green"]);
        assert_eq!(found[0].started_ms, at(12));
        assert_eq!(found[0].ended_ms, at(30));
        assert_eq!(found[0].confidence, Confidence::Exact);
        assert!(of(&o, "A1").is_empty());
    }

    #[test]
    fn a2_silent_without_edits() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo build" out=error exit=101 aid=red
            bash main 30+2 prog="cargo build" aid=green
        "#);
        assert!(of(&o, "A2").is_empty());
    }

    // --- A3 ---

    #[test]
    fn a3_fires_when_the_same_hash_passes_later() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo fmt" hash=h2 out=error exit=1 aid=f
            bash main 20+2 prog="cargo fmt" hash=h3 aid=other
            bash main 30+2 prog="cargo fmt" hash=h2 aid=p
        "#);
        let found = of(&o, "A3");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["f", "p"]);
        assert_eq!(found[0].started_ms, at(12));
        assert_eq!(found[0].ended_ms, at(30));
        assert_eq!(found[0].confidence, Confidence::Exact);
    }

    #[test]
    fn a3_fires_on_a_different_invocation_of_the_program() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo fmt" hash=h2 out=error exit=1 aid=f
            bash main 20+2 prog="cargo fmt" hash=h3 aid=g
        "#);
        let found = of(&o, "A3");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["f", "g"]);
    }

    #[test]
    fn a3_silent_on_a_lint_pass_without_a_prior_fail() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo fmt" hash=h2 aid=p1
            bash main 30+2 prog="cargo fmt" hash=h3 aid=p2
        "#);
        assert!(of(&o, "A3").is_empty());
    }

    #[test]
    fn a3_silent_when_the_failure_is_never_fixed() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo fmt" hash=h2 out=error exit=1 aid=f
        "#);
        assert!(of(&o, "A3").is_empty());
    }

    // --- A4 ---

    #[test]
    fn a4_fires_when_next_check_fails() {
        let o = out(r#"
            turn main 0
            agent main 10+90 type=general-purpose id=ag1 aid=x
            bash main 110+5 prog="cargo test" out=error exit=101 aid=v
        "#);
        let found = of(&o, "A4");
        assert_eq!(found.len(), 1);
        let f = found[0];
        assert_eq!(ids(f), vec!["x", "v"]);
        assert_eq!(f.confidence, Confidence::Exact);
        assert_eq!(f.started_ms, at(110));
        assert_eq!(f.ended_ms, at(115));
        match &f.policy {
            SpanPolicy::Spans(span_ids) => assert!(!span_ids.is_empty()),
            other => panic!("expected the check's own spans, got {other:?}"),
        }
        assert_eq!(verdict(&o, "x", "A4"), Some(Verified::Failed));
    }

    #[test]
    fn a4_interval_ends_at_the_next_pass() {
        let o = out(r#"
            turn main 0
            agent main 10+90 type=general-purpose id=ag1 aid=x
            bash main 110+5 prog="cargo test" out=error exit=101 aid=v
            edit main 120+1 path=core/src/a.rs aid=fix
            bash main 130+5 prog="cargo test" aid=green
        "#);
        let found = of(&o, "A4");
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0].policy, SpanPolicy::Interval));
        assert_eq!(found[0].started_ms, at(115));
        assert_eq!(found[0].ended_ms, at(130));
        assert_eq!(found[0].confidence, Confidence::Exact);
    }

    #[test]
    fn a4_passed_verdict_when_next_check_passes() {
        let o = out(r#"
            turn main 0
            agent main 10+90 type=general-purpose id=ag1 aid=x
            bash main 110+5 prog="cargo test" aid=v
        "#);
        assert!(of(&o, "A4").is_empty());
        assert_eq!(verdict(&o, "x", "A4"), Some(Verified::Passed));
    }

    #[test]
    fn a4_not_measured_without_a_check() {
        let o = out(r#"
            turn main 0
            agent main 10+90 type=general-purpose id=ag1 aid=x
        "#);
        assert!(of(&o, "A4").is_empty());
        assert_eq!(verdict(&o, "x", "A4"), Some(Verified::NotMeasured));
    }

    #[test]
    fn a4_inferred_when_controller_edited_between() {
        let o = out(r#"
            turn main 0
            agent main 10+90 type=general-purpose id=ag1 aid=x
            edit main 105+1 path=core/src/a.rs aid=ctl
            bash main 110+5 prog="cargo test" out=error exit=101 aid=v
        "#);
        let found = of(&o, "A4");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn a4_skips_an_agent_without_a_type() {
        let o = out(r#"
            turn main 0
            agent main 10+90 id=ag1 aid=x
            bash main 110+5 prog="cargo test" out=error exit=101 aid=v
        "#);
        assert!(of(&o, "A4").is_empty());
        assert_eq!(verdict(&o, "x", "A4"), None);
    }

    // --- A5 ---

    #[test]
    fn a5_fires_and_counts_rounds() {
        let o = out(r#"
            turn main 0
            agent main 10+20 type=wf-executor id=i1 aid=i1
            agent main 40+10 type=wf-reviewer id=r1 aid=r1
            agent main 60+20 type=wf-executor id=i2 aid=i2
            agent main 90+10 type=wf-reviewer id=r2 aid=r2
            agent main 110+10 type=wf-executor id=i3 aid=i3
        "#);
        let found = of(&o, "A5");
        assert_eq!(found.len(), 2);
        assert_eq!(ids(found[0]), vec!["i2"]);
        assert_eq!(found[0].count, 1);
        assert_eq!(ids(found[1]), vec!["i3"]);
        assert_eq!(found[1].count, 2);
        assert_eq!(found[0].confidence, Confidence::Exact);
        assert!(matches!(found[0].policy, SpanPolicy::Attempts));
    }

    #[test]
    fn a5_silent_when_no_implementer_follows_the_reviewer() {
        let o = out(r#"
            turn main 0
            agent main 10+20 type=wf-executor id=i1 aid=i1
            agent main 40+10 type=wf-reviewer id=r1 aid=r1
        "#);
        assert!(of(&o, "A5").is_empty());
    }

    // --- A6 ---

    #[test]
    fn a6_fires_on_relaunch_after_error() {
        let o = out(r#"
            turn main 0
            agent main 10+20 type=wf-executor id=ag1 out=error aid=x
            agent main 40+20 type=wf-executor id=ag2 aid=y
        "#);
        let found = of(&o, "A6");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["x", "y"]);
        assert_eq!(found[0].confidence, Confidence::Exact);
        assert!(matches!(found[0].policy, SpanPolicy::Spans(_)));
        assert_eq!(verdict(&o, "x", "A6"), Some(Verified::Failed));
    }

    #[test]
    fn a6_silent_when_a_different_agent_type_follows() {
        let o = out(r#"
            turn main 0
            agent main 10+20 type=wf-executor id=ag1 out=error aid=x
            agent main 40+20 type=wf-planner id=ag2 aid=y
        "#);
        assert!(of(&o, "A6").is_empty());
        assert_eq!(verdict(&o, "x", "A6"), None);
    }

    // --- A7 ---

    #[test]
    fn a7_fires_on_bg_failed() {
        let o = out(r#"
            turn main 0
            agent main 10+0 bg=1 bg_end=40 bg_status=failed type=wf-executor id=ag1 aid=x
        "#);
        let found = of(&o, "A7");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["x"]);
        assert_eq!(found[0].confidence, Confidence::Exact);
        assert!(matches!(found[0].policy, SpanPolicy::Attempts));
    }

    #[test]
    fn a7_inferred_on_no_final_result() {
        let o = out(r#"
            turn main 0
            agent main 10+30 type=wf-executor id=ag1 aid=x
            msg agent:ag1 12+1 id=sm1 tools=0
            msg agent:ag1 20+1 id=sm2 tools=1
            read agent:ag1 21+1 path=core/src/a.rs msg=sm2 aid=r1
        "#);
        let found = of(&o, "A7");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["x"]);
        assert_eq!(found[0].confidence, Confidence::Inferred);
        match &found[0].policy {
            SpanPolicy::Lane(lane) => assert_eq!(lane, "agent:ag1"),
            other => panic!("expected the subagent lane, got {other:?}"),
        }
    }

    #[test]
    fn a7_silent_on_final_text() {
        let o = out(r#"
            turn main 0
            agent main 10+30 type=wf-executor id=ag1 aid=x
            msg agent:ag1 12+1 id=sm1 tools=1
            read agent:ag1 13+1 path=core/src/a.rs msg=sm1 aid=r1
            msg agent:ag1 25+1 id=sm3 tools=0
        "#);
        assert!(of(&o, "A7").is_empty());
    }

    // --- A8 ---

    #[test]
    fn a8_fires_on_fail_then_pass_without_edit() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo test" out=error exit=101 aid=f
            bash main 30+2 prog="cargo test" aid=p
        "#);
        let found = of(&o, "A8");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["f", "p"]);
        assert_eq!(found[0].started_ms, at(10));
        assert_eq!(found[0].ended_ms, at(30));
        assert_eq!(found[0].confidence, Confidence::Exact);
        assert!(of(&o, "A1").is_empty());
    }

    #[test]
    fn a8_silent_when_an_edit_sits_between() {
        let o = out(r#"
            turn main 0
            bash main 10+2 prog="cargo test" out=error exit=101 aid=f
            edit main 20+1 path=core/src/a.rs aid=fix
            bash main 30+2 prog="cargo test" aid=p
        "#);
        assert!(of(&o, "A8").is_empty());
        assert_eq!(of(&o, "A1").len(), 1);
    }
}
