//! Projection of findings onto spans and attempts.

use super::*;

// --- projection --------------------------------------------------------------------------------------

/// Span kinds that count as work: unclaimed, they are `useful`. `wait_human` and `idle` are not work
/// and stay unannotated.
pub(super) const WORK_KINDS: [&str; 5] = [
    "model",
    "tool",
    "subagent",
    "wait_background",
    "wait_machine",
];

/// One finding competing to annotate a span or an attempt.
pub(super) struct Claim {
    /// Position of the finding in the slice handed to `project`.
    pub(super) index: usize,
    pub(super) rule: &'static Rule,
    /// Position of the rule in `RULES`: the tie-break.
    pub(super) order: usize,
    pub(super) key: String,
}

pub(super) fn waste_rank(waste: Waste) -> u8 {
    match waste {
        Waste::Avoidable => 2,
        Waste::Rework => 1,
        Waste::Useful => 0,
    }
}

/// `avoidable > rework`, then registry order.
pub(super) fn outranks(candidate: &Claim, held: &Claim) -> bool {
    let (a, b) = (
        waste_rank(candidate.rule.waste),
        waste_rank(held.rule.waste),
    );
    a > b || (a == b && candidate.order < held.order)
}

/// The winning claim; between claims that tie completely, the first stands.
pub(super) fn best_claim<'a>(claims: impl Iterator<Item = &'a Claim>) -> Option<&'a Claim> {
    let mut best: Option<&Claim> = None;
    for claim in claims {
        if best.is_none_or(|held| outranks(claim, held)) {
            best = Some(claim);
        }
    }
    best
}

pub(super) fn claims_span(finding: &Finding, span: &SpanFact) -> bool {
    match &finding.policy {
        SpanPolicy::Interval => {
            span.lane == finding.lane
                && span.started_ms >= finding.started_ms
                && span.ended_ms <= finding.ended_ms
        }
        SpanPolicy::Attempts => span
            .attempt_ids
            .iter()
            .any(|id| finding.attempt_ids.contains(id)),
        SpanPolicy::Lane(lane) => &span.lane == lane,
        SpanPolicy::Spans(ids) => ids.contains(&span.id),
        SpanPolicy::Signal => false,
    }
}

/// Puts the findings and verdicts onto the session's spans and attempts.
///
/// - A span goes to the winning finding that claims it (`avoidable` over `rework`, then registry
///   order); a span of work no finding claims is `useful`; `wait_human` and `idle` stay unmarked.
/// - An attempt gets a mark when a non-signal finding names it, when it is an agent attempt (its
///   `verified` defaults to `not_measured`) or when a verdict names it.
///
/// Time totals are meant to be summed from these annotations, never from a finding's `cost_ms`, so
/// overlapping findings never count twice.
pub fn project(
    facts: &SessionFacts,
    findings: &[Finding],
    verdicts: &[Verdict],
) -> (Vec<SpanMark>, Vec<AttemptMarkRow>) {
    let claims: Vec<Claim> = findings
        .iter()
        .enumerate()
        .filter(|(_, finding)| !matches!(finding.policy, SpanPolicy::Signal))
        .filter_map(|(index, finding)| {
            let registered = rule(finding.rule_id)?;
            Some(Claim {
                index,
                rule: registered,
                order: RULES
                    .iter()
                    .position(|r| r.id == registered.id)
                    .unwrap_or(usize::MAX),
                key: finding_key(&facts.session_id, finding),
            })
        })
        .collect();

    let mut span_marks = Vec::new();
    for span in &facts.spans {
        let winner = best_claim(
            claims
                .iter()
                .filter(|claim| claims_span(&findings[claim.index], span)),
        );
        match winner {
            Some(claim) => span_marks.push(SpanMark {
                span_id: span.id,
                waste: Some(claim.rule.waste.as_str().to_string()),
                rule_id: Some(claim.rule.id.to_string()),
                lever: Some(claim.rule.levers[0].to_string()),
                finding_key: Some(claim.key.clone()),
            }),
            None if WORK_KINDS.contains(&span.kind.as_str()) => span_marks.push(SpanMark {
                span_id: span.id,
                waste: Some(Waste::Useful.as_str().to_string()),
                ..Default::default()
            }),
            None => {}
        }
    }

    let merged = merge_verdicts(verdicts.to_vec());
    let verdict_of: HashMap<&str, &Verdict> = merged
        .iter()
        .map(|verdict| (verdict.attempt_id.as_str(), verdict))
        .collect();
    let mut attempt_marks = Vec::new();
    for attempt in &facts.attempts {
        let winner = best_claim(claims.iter().filter(|claim| {
            findings[claim.index]
                .attempt_ids
                .contains(&attempt.attempt_id)
        }));
        let verdict = verdict_of.get(attempt.attempt_id.as_str()).copied();
        if winner.is_none() && verdict.is_none() && attempt.kind != "agent" {
            continue;
        }
        attempt_marks.push(AttemptMarkRow {
            attempt_id: attempt.attempt_id.clone(),
            session_id: facts.session_id.clone(),
            waste: winner.map(|claim| claim.rule.waste.as_str().to_string()),
            rule_id: winner.map(|claim| claim.rule.id.to_string()),
            lever: winner.map(|claim| claim.rule.levers[0].to_string()),
            finding_key: winner.map(|claim| claim.key.clone()),
            verified: verdict
                .map_or(Verified::NotMeasured, |v| v.verified)
                .as_str()
                .to_string(),
            verified_by: verdict.map(|v| v.rule_id.to_string()),
        });
    }
    (span_marks, attempt_marks)
}
