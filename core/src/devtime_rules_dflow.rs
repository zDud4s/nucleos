//! Family D, flow group: duration, parallelism, reference and model rules (D4 to D9, D12). Rules read the
//! session's facts and the thresholds in `DevtimeRulesConfig`; they never write a row.
//!
//! Every threshold and vocabulary (seconds, repeat counts, token budgets, model strength) comes from the
//! config. The only literals here are the closed tokens the parser itself emits (lane `main`, attempt
//! kind `agent`, span kind `model`, error classes `timeout` and `exit_75`, background statuses).
//! A finding carries ids, times and counts only, never any text of the session.

use std::collections::{HashMap, HashSet};

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{
    AttemptFact, Confidence, Finding, MessageFact, Outcome, RuleOutput, SessionFacts, SpanFact,
    SpanPolicy, Verdict, Verified, a_fail, duration, edits_between, passed,
};

/// The lane of the main conversation, as the lane builder names it.
const MAIN_LANE: &str = "main";
/// An attempt `kind` that launches a subagent.
const KIND_AGENT: &str = "agent";
/// A span `kind` that is the model thinking or writing.
const SPAN_MODEL: &str = "model";
/// Error classes whose repeat belongs to B4 and B5, not to D7.
const CLASS_TIMEOUT: &str = "timeout";
const CLASS_EXIT_75: &str = "exit_75";
/// Background notification statuses that mean the work did not finish well.
const BG_FAILED: &str = "failed";
const BG_KILLED: &str = "killed";

pub fn run(facts: &SessionFacts, cfg: &DevtimeRulesConfig) -> RuleOutput {
    let mut out = RuleOutput::default();
    d4(facts, &mut out);
    d5(facts, cfg, &mut out);
    d6(facts, cfg, &mut out);
    d7(facts, &mut out);
    d8(facts, cfg, &mut out);
    d9(facts, cfg, &mut out);
    d12(facts, cfg, &mut out);
    out
}

// --- helpers -----------------------------------------------------------------------------------------

/// Whole seconds from the config as milliseconds, saturating.
fn secs_to_ms(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX).saturating_mul(1000)
}

/// The attempts a message carries, in start order (the facts are sorted that way).
fn message_attempts<'a>(facts: &'a SessionFacts, message: &MessageFact) -> Vec<&'a AttemptFact> {
    facts
        .attempts
        .iter()
        .filter(|a| {
            a.lane == message.lane && a.message_id.as_deref() == Some(message.message_id.as_str())
        })
        .collect()
}

/// `(earliest start, latest done)` of a non-empty group of attempts.
fn bounds(attempts: &[&AttemptFact]) -> (i64, i64) {
    let mut start = i64::MAX;
    let mut end = i64::MIN;
    for a in attempts {
        start = start.min(a.started_ms);
        end = end.max(a.done_ms);
    }
    (start, end.max(start))
}

/// How long a group of attempts took, never negative.
fn group_duration(attempts: &[&AttemptFact]) -> i64 {
    let (start, end) = bounds(attempts);
    (end - start).max(0)
}

fn ids_of(attempts: &[&AttemptFact]) -> Vec<String> {
    attempts.iter().map(|a| a.attempt_id.clone()).collect()
}

/// How long a span lasted, never negative.
fn span_len(span: &SpanFact) -> i64 {
    (span.ended_ms - span.started_ms).max(0)
}

/// The main lane's tool-using messages, in time order.
fn main_tool_messages(facts: &SessionFacts) -> Vec<&MessageFact> {
    let mut messages: Vec<&MessageFact> = facts
        .messages
        .iter()
        .filter(|m| m.lane == MAIN_LANE && m.has_tool_use)
        .collect();
    messages.sort_by(|a, b| {
        (a.first_ms, a.message_id.as_str()).cmp(&(b.first_ms, b.message_id.as_str()))
    });
    messages
}

/// A shell call that runs a recognized build, test or lint command.
fn is_verif(a: &AttemptFact) -> bool {
    a.is_shell && a.cmd_class.is_some()
}

// --- D4: a read-only round that did not need the previous one ----------------------------------------

fn d4(facts: &SessionFacts, out: &mut RuleOutput) {
    let messages = main_tool_messages(facts);
    for pair in messages.windows(2) {
        let first = message_attempts(facts, pair[0]);
        let second = message_attempts(facts, pair[1]);
        if first.is_empty() || second.is_empty() {
            continue;
        }
        let read_only_known = first
            .iter()
            .chain(second.iter())
            .all(|a| (a.is_read || a.is_search) && a.refs_known);
        if !read_only_known {
            continue;
        }
        let produced: HashSet<&str> = first
            .iter()
            .flat_map(|a| a.refs_out.iter().map(String::as_str))
            .collect();
        let dependent = second
            .iter()
            .any(|a| a.refs_in.iter().any(|r| produced.contains(r.as_str())));
        if dependent {
            continue;
        }
        let (started_ms, ended_ms) = bounds(&second);
        out.findings.push(Finding {
            rule_id: "D4",
            lane: MAIN_LANE.to_string(),
            started_ms,
            ended_ms,
            cost_ms: group_duration(&first).min(group_duration(&second)),
            attempt_ids: ids_of(&second),
            confidence: Confidence::Inferred,
            count: 1,
            policy: SpanPolicy::Attempts,
        });
    }
}

// --- D5: a long foreground agent whose result the next step did not use ------------------------------

fn d5(facts: &SessionFacts, cfg: &DevtimeRulesConfig, out: &mut RuleOutput) {
    let limit = secs_to_ms(cfg.thresholds.d5_foreground_agent_seconds);
    let messages = main_tool_messages(facts);
    for agent in &facts.attempts {
        if !(agent.is_main && agent.kind == KIND_AGENT && !agent.background) {
            continue;
        }
        if duration(agent) <= limit || !agent.refs_known {
            continue;
        }
        // The next tool-using main message that starts after the agent finished and carries attempts.
        let mut next: Vec<&AttemptFact> = Vec::new();
        for message in &messages {
            if message.first_ms < agent.done_ms {
                continue;
            }
            let attempts = message_attempts(facts, message);
            if !attempts.is_empty() {
                next = attempts;
                break;
            }
        }
        if next.is_empty() || !next.iter().all(|a| a.refs_known) {
            continue;
        }
        let used = next
            .iter()
            .any(|a| a.refs_in.iter().any(|r| agent.refs_out.contains(r)));
        if used {
            continue;
        }
        out.findings.push(Finding {
            rule_id: "D5",
            lane: agent.lane.clone(),
            started_ms: agent.started_ms,
            ended_ms: agent.done_ms,
            cost_ms: duration(agent).min(group_duration(&next)),
            attempt_ids: vec![agent.attempt_id.clone()],
            confidence: Confidence::Inferred,
            count: 1,
            policy: SpanPolicy::Attempts,
        });
    }
}

// --- D6: a long foreground command ------------------------------------------------------------------

fn d6(facts: &SessionFacts, cfg: &DevtimeRulesConfig, out: &mut RuleOutput) {
    let limit = secs_to_ms(cfg.thresholds.d6_foreground_command_seconds);
    for a in &facts.attempts {
        if !(a.is_main && a.is_shell && !a.background) {
            continue;
        }
        if a.outcome == Outcome::Interrupted || a.error_class.as_deref() == Some(CLASS_TIMEOUT) {
            continue;
        }
        let length = duration(a);
        if length <= limit {
            continue;
        }
        out.findings.push(Finding {
            rule_id: "D6",
            lane: a.lane.clone(),
            started_ms: a.started_ms,
            ended_ms: a.done_ms,
            cost_ms: length,
            attempt_ids: vec![a.attempt_id.clone()],
            confidence: Confidence::Inferred,
            count: 1,
            policy: SpanPolicy::Attempts,
        });
    }
}

// --- D7: the same check run again with nothing changed -----------------------------------------------

fn d7(facts: &SessionFacts, out: &mut RuleOutput) {
    let mut last: HashMap<(&str, &str), &AttemptFact> = HashMap::new();
    for b in &facts.attempts {
        if !is_verif(b) {
            continue;
        }
        let Some(hash) = b.cmd_hash.as_deref() else {
            continue;
        };
        let key = (b.lane.as_str(), hash);
        if let Some(a) = last.get(&key).copied() {
            let timed = a.error_class.as_deref() == Some(CLASS_TIMEOUT)
                || a.error_class.as_deref() == Some(CLASS_EXIT_75);
            // fail then pass is A8's (a flaky check), a timeout or exit 75 is B4/B5's.
            let flaky = a_fail(a) && passed(b);
            // A run that overlaps the previous one is parallel work, not a repeat.
            let sequential = b.started_ms >= a.done_ms;
            let edited = edits_between(facts, a.done_ms, b.started_ms)
                .next()
                .is_some();
            if !timed && !flaky && sequential && !edited {
                out.findings.push(Finding {
                    rule_id: "D7",
                    lane: b.lane.clone(),
                    started_ms: b.started_ms,
                    ended_ms: b.done_ms,
                    cost_ms: duration(b),
                    attempt_ids: vec![b.attempt_id.clone()],
                    confidence: Confidence::Exact,
                    count: 1,
                    policy: SpanPolicy::Attempts,
                });
            }
        }
        last.insert(key, b);
    }
}

// --- D8: waiting by hand -----------------------------------------------------------------------------

fn d8(facts: &SessionFacts, cfg: &DevtimeRulesConfig, out: &mut RuleOutput) {
    // (i) every sleep.
    for s in &facts.attempts {
        if s.is_shell && s.is_sleep {
            out.findings.push(Finding {
                rule_id: "D8",
                lane: s.lane.clone(),
                started_ms: s.started_ms,
                ended_ms: s.done_ms,
                cost_ms: duration(s),
                attempt_ids: vec![s.attempt_id.clone()],
                confidence: Confidence::Exact,
                count: 1,
                policy: SpanPolicy::Attempts,
            });
        }
    }

    // (ii) a command polled again and again between sleeps. A chain needs at least one repeat.
    let repeats = usize::try_from(cfg.thresholds.d8_poll_repeats)
        .unwrap_or(usize::MAX)
        .max(2);
    let mut lanes: Vec<&str> = Vec::new();
    for a in &facts.attempts {
        if !lanes.contains(&a.lane.as_str()) {
            lanes.push(a.lane.as_str());
        }
    }
    for lane in lanes {
        let mut chain: Vec<&AttemptFact> = Vec::new();
        for a in facts.attempts.iter().filter(|a| a.lane == lane) {
            if a.is_shell && a.is_sleep {
                continue;
            }
            let pollable = a.is_shell && !is_verif(a) && a.cmd_hash.is_some();
            if pollable {
                let same = chain
                    .first()
                    .is_some_and(|first| first.cmd_hash == a.cmd_hash);
                if !same {
                    flush_poll_chain(&chain, repeats, out);
                    chain.clear();
                }
                chain.push(a);
                continue;
            }
            if !a.is_shell && !a.is_edit {
                // A read or a search between polls does not break the wait.
                continue;
            }
            flush_poll_chain(&chain, repeats, out);
            chain.clear();
        }
        flush_poll_chain(&chain, repeats, out);
    }
}

/// Emits the finding of one poll chain when it is long enough: the repeats after the first call.
fn flush_poll_chain(chain: &[&AttemptFact], repeats: usize, out: &mut RuleOutput) {
    if chain.len() < repeats || chain.len() < 2 {
        return;
    }
    let rest = &chain[1..];
    let (started_ms, ended_ms) = bounds(rest);
    let cost_ms: i64 = rest.iter().map(|a| duration(a)).sum();
    out.findings.push(Finding {
        rule_id: "D8",
        lane: rest[0].lane.clone(),
        started_ms,
        ended_ms,
        cost_ms,
        attempt_ids: ids_of(rest),
        confidence: Confidence::Inferred,
        count: i64::try_from(rest.len()).unwrap_or(i64::MAX),
        policy: SpanPolicy::Attempts,
    });
}

// --- D9: the model slowed down by a long context -----------------------------------------------------

fn d9(facts: &SessionFacts, cfg: &DevtimeRulesConfig, out: &mut RuleOutput) {
    let budget = cfg.thresholds.d9_context_tokens;
    let model_spans: Vec<&SpanFact> = facts
        .spans
        .iter()
        .filter(|s| s.lane == MAIN_LANE && s.kind == SPAN_MODEL)
        .collect();
    let is_over = |s: &SpanFact| s.context_tokens.is_some_and(|tokens| tokens > budget);
    let heavy: Vec<&SpanFact> = model_spans.iter().copied().filter(|s| is_over(s)).collect();
    if heavy.is_empty() {
        return;
    }
    let mut baseline: Vec<i64> = model_spans
        .iter()
        .copied()
        .filter(|s| !is_over(s))
        .map(span_len)
        .collect();
    if baseline.is_empty() {
        baseline = model_spans.iter().copied().map(span_len).collect();
    }
    baseline.sort_unstable();
    let mid = baseline.len() / 2;
    let median = if baseline.len() % 2 == 1 {
        baseline[mid]
    } else {
        (baseline[mid - 1] + baseline[mid]) / 2
    };
    let cost_ms: i64 = heavy.iter().map(|s| (span_len(s) - median).max(0)).sum();
    let started_ms = heavy.iter().map(|s| s.started_ms).min().unwrap_or(0);
    let ended_ms = heavy
        .iter()
        .map(|s| s.ended_ms)
        .max()
        .unwrap_or(started_ms)
        .max(started_ms);
    out.findings.push(Finding {
        rule_id: "D9",
        lane: MAIN_LANE.to_string(),
        started_ms,
        ended_ms,
        cost_ms,
        attempt_ids: Vec::new(),
        confidence: Confidence::Inferred,
        count: i64::try_from(heavy.len()).unwrap_or(i64::MAX),
        policy: SpanPolicy::Spans(heavy.iter().map(|s| s.id).collect()),
    });
}

// --- D12: a stronger model fixed what a weaker one failed (hypothesis) --------------------------------

/// The position of the first strength token the model name contains, case-insensitively. A model
/// that names none has no rank and never compares.
fn model_rank(model: &str, strength: &[String]) -> Option<usize> {
    let lower = model.to_lowercase();
    strength.iter().position(|token| {
        let token = token.trim().to_lowercase();
        !token.is_empty() && lower.contains(&token)
    })
}

fn agent_failed(a: &AttemptFact) -> bool {
    let bg_bad = a.background
        && (a.bg_status.as_deref() == Some(BG_FAILED) || a.bg_status.as_deref() == Some(BG_KILLED));
    a.outcome == Outcome::Error || bg_bad
}

fn d12(facts: &SessionFacts, cfg: &DevtimeRulesConfig, out: &mut RuleOutput) {
    let strength = &cfg.vocab.model_strength;
    for x in &facts.attempts {
        if x.kind != KIND_AGENT || x.agent_type.is_none() || !agent_failed(x) {
            continue;
        }
        let Some(rank_x) = x.model.as_deref().and_then(|m| model_rank(m, strength)) else {
            continue;
        };
        // Only the very next agent of the same type counts: a second failure ends the story.
        let Some(y) = facts
            .attempts
            .iter()
            .find(|y| y.idx > x.idx && y.kind == KIND_AGENT && y.agent_type == x.agent_type)
        else {
            continue;
        };
        if y.outcome != Outcome::Ok {
            continue;
        }
        let Some(rank_y) = y.model.as_deref().and_then(|m| model_rank(m, strength)) else {
            continue;
        };
        if rank_y <= rank_x {
            continue;
        }
        // The finding claims the failed agent's spans only; the retry's work is useful.
        let claimed: Vec<i64> = facts
            .spans
            .iter()
            .filter(|s| s.attempt_ids.iter().any(|id| id == &x.attempt_id))
            .map(|s| s.id)
            .collect();
        out.findings.push(Finding {
            rule_id: "D12",
            lane: x.lane.clone(),
            started_ms: x.started_ms,
            ended_ms: x.done_ms,
            cost_ms: duration(x),
            attempt_ids: vec![x.attempt_id.clone(), y.attempt_id.clone()],
            confidence: Confidence::Inferred,
            count: 1,
            policy: SpanPolicy::Spans(claimed),
        });
        out.verdicts.push(Verdict {
            attempt_id: x.attempt_id.clone(),
            verified: Verified::Failed,
            rule_id: "D12",
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules_fixture::{at, script};

    fn run_script(text: &str) -> RuleOutput {
        let cfg = DevtimeRulesConfig::default();
        let facts = script(text).facts(&cfg);
        run(&facts, &cfg)
    }

    fn of<'a>(out: &'a RuleOutput, rule: &str) -> Vec<&'a Finding> {
        out.findings.iter().filter(|f| f.rule_id == rule).collect()
    }

    fn ids(finding: &Finding) -> Vec<&str> {
        finding.attempt_ids.iter().map(String::as_str).collect()
    }

    // --- D4 ---

    #[test]
    fn d4_fires_on_independent_read_rounds() {
        let out = run_script(
            r#"
            turn main 0
            read main 10+1 path=a.rs aid=r1 refs_out=x
            read main 20+3 path=b.rs aid=r2 refs_in=y refs_out=z
            "#,
        );
        let found = of(&out, "D4");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["r2"]);
        assert_eq!(found[0].cost_ms, 1000);
        assert_eq!(found[0].confidence, Confidence::Inferred);
        assert!(matches!(found[0].policy, SpanPolicy::Attempts));
    }

    #[test]
    fn d4_silent_when_the_second_read_uses_the_first_result() {
        let out = run_script(
            r#"
            turn main 0
            read main 10+1 path=a.rs aid=r1 refs_out=x
            read main 20+1 path=b.rs aid=r2 refs_in=x
            "#,
        );
        assert!(of(&out, "D4").is_empty());
    }

    #[test]
    fn d4_silent_when_a_round_edits() {
        let out = run_script(
            r#"
            turn main 0
            read main 10+1 path=a.rs aid=r1 refs_out=x
            edit main 20+1 path=a.rs aid=e1
            "#,
        );
        assert!(of(&out, "D4").is_empty());
    }

    #[test]
    fn d4_silent_on_parser_v1() {
        let out = run_script(
            r#"
            turn main 0
            read main 10+1 path=a.rs aid=r1 pv=1
            read main 20+1 path=b.rs aid=r2 pv=1
            "#,
        );
        assert!(of(&out, "D4").is_empty());
    }

    // --- D5 ---

    #[test]
    fn d5_fires_on_a_long_agent_nobody_used() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+180 type=wf-executor id=ag1 aid=ag refs_out=x
            read  main 195+1 path=b.rs aid=r1 refs_in=y
            "#,
        );
        let found = of(&out, "D5");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["ag"]);
        assert_eq!(found[0].cost_ms, 1000);
        assert_eq!(found[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn d5_silent_when_the_next_read_names_the_agent_result() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+180 type=wf-executor id=ag1 aid=ag refs_out=x
            read  main 195+1 path=b.rs aid=r1 refs_in=x
            "#,
        );
        assert!(of(&out, "D5").is_empty());
    }

    #[test]
    fn d5_silent_on_a_short_agent() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+60 type=wf-executor id=ag1 aid=ag refs_out=x
            read  main 85+1 path=b.rs aid=r1 refs_in=y
            "#,
        );
        assert!(of(&out, "D5").is_empty());
    }

    // --- D6 ---

    #[test]
    fn d6_fires_on_a_long_foreground_command() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+150 prog="cargo test" aid=slow
            "#,
        );
        let found = of(&out, "D6");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["slow"]);
        assert_eq!(found[0].cost_ms, 150_000);
        assert_eq!(found[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn d6_silent_in_background_or_when_short() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10 bg=1 bg_end=170 bg_status=completed prog="cargo test" aid=bgt
            bash main 200+90 prog="cargo build" aid=short
            "#,
        );
        assert!(of(&out, "D6").is_empty());
    }

    #[test]
    fn d6_silent_on_a_timeout() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+150 prog="cargo test" out=error err=timeout aid=slow
            "#,
        );
        assert!(of(&out, "D6").is_empty());
    }

    // --- D7 ---

    #[test]
    fn d7_fires_on_a_rerun_with_no_edit() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+5 prog="cargo test" aid=a
            bash main 20+5 prog="cargo test" aid=b
            "#,
        );
        let found = of(&out, "D7");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["b"]);
        assert_eq!(found[0].confidence, Confidence::Exact);
        assert_eq!(found[0].cost_ms, 5000);
    }

    #[test]
    fn d7_silent_when_an_edit_sits_between() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+5 prog="cargo test" aid=a
            edit main 17+1 path=a.rs aid=e
            bash main 20+5 prog="cargo test" aid=b
            "#,
        );
        assert!(of(&out, "D7").is_empty());
    }

    #[test]
    fn d7_excludes_flaky_pair() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+5 prog="cargo test" out=error exit=101 aid=a
            bash main 20+5 prog="cargo test" aid=b
            "#,
        );
        assert!(of(&out, "D7").is_empty());
    }

    #[test]
    fn d7_excludes_a_timed_out_first_run() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+5 prog="cargo test" out=error err=timeout aid=a
            bash main 20+5 prog="cargo test" aid=b
            "#,
        );
        assert!(of(&out, "D7").is_empty());
    }

    // --- D8 ---

    #[test]
    fn d8_sleep_fires() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+30 prog=sleep aid=zz
            "#,
        );
        let found = of(&out, "D8");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["zz"]);
        assert_eq!(found[0].confidence, Confidence::Exact);
        assert_eq!(found[0].cost_ms, 30_000);
    }

    #[test]
    fn d8_polls_fire_inferred() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+2 prog="gh run view" aid=p1
            bash main 15+1 prog=sleep aid=s1
            bash main 20+2 prog="gh run view" aid=p2
            bash main 25+1 prog=sleep aid=s2
            bash main 30+2 prog="gh run view" aid=p3
            "#,
        );
        let polls: Vec<&Finding> = of(&out, "D8")
            .into_iter()
            .filter(|f| f.confidence == Confidence::Inferred)
            .collect();
        assert_eq!(polls.len(), 1);
        assert_eq!(ids(polls[0]), vec!["p2", "p3"]);
        assert_eq!(polls[0].count, 2);
    }

    #[test]
    fn d8_silent_on_two_status_calls() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+1 prog="git status" aid=g1
            bash main 20+1 prog="git status" aid=g2
            "#,
        );
        assert!(of(&out, "D8").is_empty());
    }

    #[test]
    fn d8_polls_break_on_an_edit() {
        let out = run_script(
            r#"
            turn main 0
            bash main 10+1 prog="gh run view" aid=p1
            edit main 15+1 path=a.rs aid=e1
            bash main 20+1 prog="gh run view" aid=p2
            edit main 25+1 path=a.rs aid=e2
            bash main 30+1 prog="gh run view" aid=p3
            "#,
        );
        assert!(of(&out, "D8").is_empty());
    }

    // --- D9 ---

    fn model_span(id: i64, start_s: i64, secs: i64, context: i64) -> SpanFact {
        SpanFact {
            id,
            lane: MAIN_LANE.to_string(),
            kind: SPAN_MODEL.to_string(),
            started_ms: at(start_s),
            ended_ms: at(start_s + secs),
            attempt_ids: Vec::new(),
            context_tokens: Some(context),
        }
    }

    fn d9_run(spans: Vec<SpanFact>) -> RuleOutput {
        let facts = SessionFacts {
            spans,
            ..SessionFacts::default()
        };
        run(&facts, &DevtimeRulesConfig::default())
    }

    #[test]
    fn d9_fires_on_a_long_context_span() {
        let out = d9_run(vec![
            model_span(1, 0, 10, 1000),
            model_span(2, 20, 10, 2000),
            model_span(3, 40, 10, 3000),
            model_span(4, 60, 30, 300_000),
        ]);
        let found = of(&out, "D9");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cost_ms, 20_000);
        assert_eq!(found[0].count, 1);
        assert_eq!(found[0].confidence, Confidence::Inferred);
        match &found[0].policy {
            SpanPolicy::Spans(span_ids) => assert_eq!(span_ids, &vec![4]),
            other => panic!("expected Spans, got {other:?}"),
        }
    }

    #[test]
    fn d9_silent_when_every_span_is_under_the_limit() {
        let out = d9_run(vec![
            model_span(1, 0, 10, 1000),
            model_span(2, 20, 30, 249_000),
        ]);
        assert!(of(&out, "D9").is_empty());
    }

    #[test]
    fn d9_uses_all_spans_when_none_is_under_the_limit() {
        let out = d9_run(vec![
            model_span(1, 0, 10, 300_000),
            model_span(2, 20, 30, 400_000),
        ]);
        let found = of(&out, "D9");
        assert_eq!(found.len(), 1);
        // The median of {10 s, 30 s} is 20 s: only the 30 s span exceeds it, by 10 s.
        assert_eq!(found[0].cost_ms, 10_000);
        assert_eq!(found[0].count, 2);
    }

    // --- D12 ---

    #[test]
    fn d12_fires_when_a_stronger_model_fixes_the_failure() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+20 type=wf-executor id=x1 model=sonnet out=error aid=x
            agent main 40+20 type=wf-executor id=x2 model=opus aid=y
            "#,
        );
        let found = of(&out, "D12");
        assert_eq!(found.len(), 1);
        assert_eq!(ids(found[0]), vec!["x", "y"]);
        assert_eq!(found[0].confidence, Confidence::Inferred);
        assert_eq!(out.verdicts.len(), 1);
        assert_eq!(out.verdicts[0].attempt_id, "x");
        assert_eq!(out.verdicts[0].verified, Verified::Failed);
        assert_eq!(out.verdicts[0].rule_id, "D12");
    }

    #[test]
    fn d12_silent_on_the_same_model() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+20 type=wf-executor id=x1 model=haiku out=error aid=x
            agent main 40+20 type=wf-executor id=x2 model=haiku aid=y
            "#,
        );
        assert!(of(&out, "D12").is_empty());
        assert!(out.verdicts.is_empty());
    }

    #[test]
    fn d12_silent_when_the_retry_is_weaker() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+20 type=wf-executor id=x1 model=opus out=error aid=x
            agent main 40+20 type=wf-executor id=x2 model=sonnet aid=y
            "#,
        );
        assert!(of(&out, "D12").is_empty());
    }

    #[test]
    fn d12_silent_on_a_different_agent_type() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+20 type=wf-executor id=x1 model=sonnet out=error aid=x
            agent main 40+20 type=wf-reviewer id=x2 model=opus aid=y
            "#,
        );
        assert!(of(&out, "D12").is_empty());
    }

    #[test]
    fn d12_silent_on_unknown_models() {
        let out = run_script(
            r#"
            turn main 0
            agent main 10+20 type=wf-executor id=x1 model=mystery-1 out=error aid=x
            agent main 40+20 type=wf-executor id=x2 model=opus aid=y
            "#,
        );
        assert!(of(&out, "D12").is_empty());
    }
}
