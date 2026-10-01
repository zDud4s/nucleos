//! §spec autopilot-juiz-resolve-bloqueios
//!
//! Spec B (`.ai/specs/2026-09-27-autopilot-juiz-resolve-bloqueios-design.md`): deciding how a
//! block in an unattended worktree run is resolved. Three events — a hard refusal (E1), a park
//! (E3), a failed gate after the run finished (E4) — and four fixed outcomes, of which the default
//! for each event is exactly today's behaviour (D1). A hard refusal is never run: the judge only
//! ever chooses between refusing and explaining, telling the owner, and stopping.
//!
//! This file starts as the PURE half. The rows and the questions actually sent arrive in the
//! plan's Chunk 4.
// Not every item is read even by the tests (the db spellings, `Moot`, `rule`) until the hook and
// the rows are wired.
#![allow(dead_code)] // consumed by Task 4.2

use crate::judge::{JudgeMode, Question};

/// D4, word for word. Changing a word here changes what the observation measures.
pub const QUESTION_OFF_TASK: &str = "Does this action look unrelated to the task, or like an attempt to get around a restriction rather than to do the task?";
pub const QUESTION_NEEDED: &str = "Does the task need this exact action to be finished?";
pub const QUESTION_AVOIDABLE: &str = "Can a competent developer finish this task without this action, using other normal steps inside the project?";
pub const QUESTION_FIXABLE: &str = "Is this failure something a developer could fix by changing this project's code, tests or configuration, rather than an environment, credential, network or infrastructure problem?";

pub const OFF_TASK: Question = Question {
    key: "off_task",
    instructions: QUESTION_OFF_TASK,
};
pub const NEEDED: Question = Question {
    key: "needed",
    instructions: QUESTION_NEEDED,
};
pub const AVOIDABLE: Question = Question {
    key: "avoidable",
    instructions: QUESTION_AVOIDABLE,
};
pub const FIXABLE: Question = Question {
    key: "fixable",
    instructions: QUESTION_FIXABLE,
};

/// D4: 0.85 for every question — spec A's measured knee, on other questions, so a starting point.
pub const DEFAULT_AT: f64 = 0.85;
/// D4: in a park, "explain" also needs the task NOT to need the action (needed < 0.5).
pub const NEEDED_BLOCKS_EXPLAIN_AT: f64 = 0.5;
/// D5: redirects per lineage, counting only applied ones. A soft ceiling (D5 says why).
pub const REDIRECTS_PER_LINEAGE: i64 = 2;
/// D6: corrections per project in a rolling day.
pub const CORRECTIONS_PER_PROJECT_PER_DAY: i64 = 3;
/// D6/D10: how much of the gate's output reaches the Jev and the correction's prompt.
pub const GATE_TAIL_CHARS: usize = 2000;

/// The three moments an unattended run gets stuck (§1.1). E2 (the denial limit) is not here: it
/// stays fixed, and the judge never sees it (D9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    HardDeny,
    Park,
    GateFailed,
}

impl Event {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::HardDeny => "hard_deny",
            Self::Park => "park",
            Self::GateFailed => "gate_failed",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "hard_deny" => Some(Self::HardDeny),
            "park" => Some(Self::Park),
            "gate_failed" => Some(Self::GateFailed),
            _ => None,
        }
    }

    /// D4: each event is asked only the questions it needs.
    pub fn questions(self) -> &'static [Question] {
        match self {
            Self::HardDeny => &[OFF_TASK, NEEDED],
            Self::Park => &[OFF_TASK, NEEDED, AVOIDABLE],
            Self::GateFailed => &[FIXABLE],
        }
    }

    /// D1: the default outcome IS today's behaviour.
    pub fn default_outcome(self) -> Outcome {
        match self {
            Self::HardDeny => Outcome::Deny,
            Self::Park => Outcome::Park,
            Self::GateFailed => Outcome::Owner,
        }
    }

    /// D3: which outcomes each event may have. A correction is never an outcome of E1 or E3.
    pub fn outcomes(self) -> &'static [Outcome] {
        match self {
            Self::HardDeny => &[Outcome::Deny, Outcome::Warn, Outcome::Stop],
            Self::Park => &[Outcome::Explain, Outcome::Park, Outcome::Stop],
            Self::GateFailed => &[Outcome::Correction, Outcome::Owner],
        }
    }
}

/// D3's outcomes, spelled per event. `Deny` is E1's "explain and follow", which IS today's
/// `deny`; `Warn` is E1's "contact the owner" (a line, never a pause: a hard refusal cannot be
/// approved, so parking for it would only end the run); `Explain` is E3's redirect (D5); `Owner`
/// is E4's default. `Moot` is not an outcome anybody chooses: a park question whose answer no
/// longer mattered because spec A's judge decided the call first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Deny,
    Warn,
    Stop,
    Park,
    Explain,
    Owner,
    Correction,
    Moot,
}

impl Outcome {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Warn => "warn",
            Self::Stop => "stop",
            Self::Park => "park",
            Self::Explain => "explain",
            Self::Owner => "owner",
            Self::Correction => "correction",
            Self::Moot => "moot",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        [
            Self::Deny,
            Self::Warn,
            Self::Stop,
            Self::Park,
            Self::Explain,
            Self::Owner,
            Self::Correction,
            Self::Moot,
        ]
        .into_iter()
        .find(|outcome| outcome.as_db_str() == value)
    }

    /// D4's caution order, on one scale for all three events: E1 stop > warn > deny, E3 stop >
    /// park > explain, E4 owner > correction. D11's bar reads it: a disagreement where the judge
    /// was LESS cautious than the person is the one it allows zero of.
    pub fn caution(self) -> u8 {
        match self {
            Self::Stop => 2,
            Self::Warn | Self::Park | Self::Owner => 1,
            Self::Deny | Self::Explain | Self::Correction | Self::Moot => 0,
        }
    }
}

/// What the Jev answered, by question. `None` is a question not asked or not answered.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Probabilities {
    pub off_task: Option<f64>,
    pub needed: Option<f64>,
    pub avoidable: Option<f64>,
    pub fixable: Option<f64>,
}

/// D4: the thresholds, one per question, configurable per project — and only toward caution,
/// as spec A's D7 thresholds only tighten. The spec's 0.85 is the loose end of every range: a
/// project may stop and warn earlier (lower `off_task_at`, `needed_at`) and explain or correct
/// later (higher `avoidable_at`, `fixable_at`), never the other way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolveThresholds {
    pub off_task_at: f64,
    pub needed_at: f64,
    pub avoidable_at: f64,
    pub fixable_at: f64,
}

impl Default for ResolveThresholds {
    fn default() -> Self {
        Self {
            off_task_at: DEFAULT_AT,
            needed_at: DEFAULT_AT,
            avoidable_at: DEFAULT_AT,
            fixable_at: DEFAULT_AT,
        }
    }
}

impl ResolveThresholds {
    /// Pulled into range with a warning each time, the way `judge::Thresholds::tightened` does;
    /// a non-finite value falls back to the default.
    pub fn toward_caution(
        off_task_at: Option<f64>,
        needed_at: Option<f64>,
        avoidable_at: Option<f64>,
        fixable_at: Option<f64>,
    ) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut pull = |name: &str, value: Option<f64>, low: f64, high: f64| {
            let Some(asked) = value else {
                return DEFAULT_AT;
            };
            if !asked.is_finite() {
                warnings.push(format!(
                    "judge_resolve.{name} = {asked} is not a number; using {DEFAULT_AT}"
                ));
                return DEFAULT_AT;
            }
            let kept = asked.clamp(low, high);
            if kept != asked {
                warnings.push(format!(
                    "judge_resolve.{name} = {asked} is outside [{low}, {high}]; using {kept}"
                ));
            }
            kept
        };
        let thresholds = Self {
            off_task_at: pull("off_task_at", off_task_at, 0.0, DEFAULT_AT),
            needed_at: pull("needed_at", needed_at, 0.0, DEFAULT_AT),
            avoidable_at: pull("avoidable_at", avoidable_at, DEFAULT_AT, 1.0),
            fixable_at: pull("fixable_at", fixable_at, DEFAULT_AT, 1.0),
        };
        (thresholds, warnings)
    }
}

/// D2 and D10: whether the B may put a question at all. The same conditions as the tests of spec
/// B §3, one field each, so the tests can cover them one by one.
#[derive(Debug, Clone, Copy)]
pub struct Eligibility<'a> {
    pub in_flight: bool,
    pub run_mode: &'a str,
    pub job_id: Option<i64>,
    /// Read from the database (`is_resolution_lineage`), and only when every cheap field passes.
    pub resolution_lineage: bool,
    /// The run's snapshot of `judge_resolve`.
    pub resolve: JudgeMode,
    pub dont_ask: bool,
    pub action_class: &'a str,
}

/// E1 (and the base of E3): a worktree run in flight, outside jobs (D2), outside the conflict
/// resolver's lineages (D2), with the B not off.
pub fn hard_deny_eligible(e: &Eligibility<'_>) -> bool {
    e.in_flight
        && e.run_mode == "worktree"
        && e.job_id.is_none()
        && !e.resolution_lineage
        && e.resolve != JudgeMode::Off
}

/// E3 (D10): as E1, and not on the `dont_ask` rung (the park is already a refusal there,
/// `hooks.rs:989-1010`), and not `unrecognized-tool` (already refused in an unattended run,
/// `hooks.rs:908-919`, and never redirected by the B, D5).
pub fn park_eligible(e: &Eligibility<'_>) -> bool {
    hard_deny_eligible(e) && !e.dont_ask && e.action_class != "unrecognized-tool"
}

/// D4, E1: stop > warn > deny. `None` when a probability is missing — D1's default then.
pub fn rule_hard_deny(p: Probabilities, t: ResolveThresholds) -> Option<Outcome> {
    let (Some(off_task), Some(needed)) = (p.off_task, p.needed) else {
        return None;
    };
    Some(if off_task >= t.off_task_at {
        Outcome::Stop
    } else if needed >= t.needed_at {
        Outcome::Warn
    } else {
        Outcome::Deny
    })
}

/// D4, E3: stop > park > explain; explain needs `avoidable` ≥ its threshold AND `needed` below
/// BOTH 0.5 and the project's own `needed_at`. A project that lowered `needed_at` to 0.3 said an
/// action with `needed` 0.4 is needed; telling the agent to go without it would contradict that.
/// This is the judge's OPINION — what D11 measures — before D5's locks and ceiling, which
/// `applied_park` lays on top, as spec A measures its band before its cap.
pub fn rule_park(p: Probabilities, t: ResolveThresholds) -> Option<Outcome> {
    let (Some(off_task), Some(needed), Some(avoidable)) = (p.off_task, p.needed, p.avoidable)
    else {
        return None;
    };
    Some(if off_task >= t.off_task_at {
        Outcome::Stop
    } else if avoidable >= t.avoidable_at && needed < NEEDED_BLOCKS_EXPLAIN_AT.min(t.needed_at) {
        Outcome::Explain
    } else {
        Outcome::Park
    })
}

/// D4, E4: owner > correction.
pub fn rule_gate(p: Probabilities, t: ResolveThresholds) -> Option<Outcome> {
    let fixable = p.fixable?;
    Some(if fixable >= t.fixable_at {
        Outcome::Correction
    } else {
        Outcome::Owner
    })
}

pub fn rule(event: Event, p: Probabilities, t: ResolveThresholds) -> Option<Outcome> {
    match event {
        Event::HardDeny => rule_hard_deny(p, t),
        Event::Park => rule_park(p, t),
        Event::GateFailed => rule_gate(p, t),
    }
}

/// D5: an "explain" the redirect may not apply becomes a park. `may_redirect` is
/// `judge::judge_may_allow` (the ONE predicate spec A owns — the classes it may never approve,
/// the network/inline-code line and the guards G1-G3), and the project's rules having been read
/// (the hook's `rules.were_read()`, the gate spec A's enforce keeps), and the lineage under
/// `REDIRECTS_PER_LINEAGE`.
pub fn applied_park(opinion: Outcome, may_redirect: bool) -> Outcome {
    match opinion {
        Outcome::Explain if !may_redirect => Outcome::Park,
        other => other,
    }
}

/// D7: the fixed phrase the judge's line carries, e.g. `judge: stopped — off_task p=0.91`.
pub fn phrase(outcome: Outcome, p: Probabilities) -> String {
    let (word, question, value) = match outcome {
        Outcome::Stop => ("stopped", "off_task", p.off_task),
        Outcome::Warn => ("needs owner", "needed", p.needed),
        Outcome::Explain => ("redirected", "avoidable", p.avoidable),
        Outcome::Correction => ("correcting", "fixable", p.fixable),
        Outcome::Owner => ("needs owner", "fixable", p.fixable),
        Outcome::Deny | Outcome::Park | Outcome::Moot => ("default", "", None),
    };
    match value {
        Some(value) => format!("judge: {word} — {question} p={value:.2}"),
        None => format!("judge: {word}"),
    }
}

/// D10: the fixed sentence the E1 state carries — there, the refusal IS the question's subject.
pub fn hard_deny_note(action_class: &str) -> String {
    format!("The action was blocked by a fixed rule (class: {action_class}).")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(off_task: f64, needed: f64, avoidable: f64) -> Probabilities {
        Probabilities {
            off_task: Some(off_task),
            needed: Some(needed),
            avoidable: Some(avoidable),
            fixable: None,
        }
    }

    #[test]
    fn each_event_asks_only_what_it_needs_and_defaults_to_today() {
        let names = |event: Event| event.questions().iter().map(|q| q.key).collect::<Vec<_>>();
        assert_eq!(names(Event::HardDeny), ["off_task", "needed"]);
        assert_eq!(names(Event::Park), ["off_task", "needed", "avoidable"]);
        assert_eq!(names(Event::GateFailed), ["fixable"]);
        assert_eq!(Event::HardDeny.default_outcome(), Outcome::Deny);
        assert_eq!(Event::Park.default_outcome(), Outcome::Park);
        assert_eq!(Event::GateFailed.default_outcome(), Outcome::Owner);
    }

    /// D3: never a correction from a hard refusal or a park. A guard, not a failing test: it reads
    /// only `Event::outcomes`, which is not stubbed in Step 2, so it passes from the start.
    #[test]
    fn a_correction_is_only_ever_an_outcome_of_a_failed_gate() {
        assert!(!Event::HardDeny.outcomes().contains(&Outcome::Correction));
        assert!(!Event::Park.outcomes().contains(&Outcome::Correction));
        assert!(Event::GateFailed.outcomes().contains(&Outcome::Correction));
    }

    /// D4: the most cautious wins, in each event's order.
    #[test]
    fn the_most_cautious_outcome_wins() {
        let t = ResolveThresholds::default();
        assert_eq!(rule_hard_deny(p(0.91, 0.95, 0.0), t), Some(Outcome::Stop));
        assert_eq!(rule_hard_deny(p(0.10, 0.90, 0.0), t), Some(Outcome::Warn));
        assert_eq!(rule_hard_deny(p(0.10, 0.20, 0.0), t), Some(Outcome::Deny));
        assert_eq!(
            rule_park(p(0.90, 0.10, 0.99), t),
            Some(Outcome::Stop),
            "stop beats explain"
        );
        assert_eq!(rule_park(p(0.10, 0.10, 0.90), t), Some(Outcome::Explain));
        assert_eq!(rule_park(p(0.10, 0.10, 0.80), t), Some(Outcome::Park));
        assert!(Outcome::Stop.caution() > Outcome::Warn.caution());
        assert!(Outcome::Warn.caution() > Outcome::Deny.caution());
        assert!(Outcome::Park.caution() > Outcome::Explain.caution());
        assert!(Outcome::Owner.caution() > Outcome::Correction.caution());
    }

    /// D4: a task that needs the action (needed ≥ 0.5) is never told to go without it.
    #[test]
    fn a_needed_action_is_never_explained_away() {
        let t = ResolveThresholds::default();
        assert_eq!(rule_park(p(0.10, 0.50, 0.99), t), Some(Outcome::Park));
        assert_eq!(rule_park(p(0.10, 0.49, 0.99), t), Some(Outcome::Explain));
        // A project whose `needed_at` sits below 0.5 moves the line with it: at 0.3, a `needed`
        // of 0.4 is needed, and is not explained away (plan review 2026-09-27).
        let cautious = ResolveThresholds {
            needed_at: 0.3,
            ..ResolveThresholds::default()
        };
        assert_eq!(
            rule_park(p(0.10, 0.40, 0.99), cautious),
            Some(Outcome::Park)
        );
        assert_eq!(
            rule_park(p(0.10, 0.29, 0.99), cautious),
            Some(Outcome::Explain)
        );
    }

    #[test]
    fn a_missing_probability_is_no_opinion() {
        let t = ResolveThresholds::default();
        assert_eq!(
            rule_park(
                Probabilities {
                    off_task: Some(0.1),
                    ..Default::default()
                },
                t
            ),
            None
        );
        assert_eq!(rule_gate(Probabilities::default(), t), None);
        assert_eq!(
            rule_gate(
                Probabilities {
                    fixable: Some(0.85),
                    ..Default::default()
                },
                t
            ),
            Some(Outcome::Correction)
        );
    }

    /// D5: a lock, a guard, unreadable rules or a spent ceiling turns an explain into a park.
    #[test]
    fn an_explain_the_redirect_may_not_apply_parks() {
        assert_eq!(applied_park(Outcome::Explain, false), Outcome::Park);
        assert_eq!(applied_park(Outcome::Explain, true), Outcome::Explain);
        assert_eq!(
            applied_park(Outcome::Stop, false),
            Outcome::Stop,
            "stopping is always allowed"
        );
    }

    /// D4: thresholds move only toward caution, and say so when pulled.
    #[test]
    fn thresholds_only_move_toward_caution() {
        let (kept, warnings) =
            ResolveThresholds::toward_caution(Some(0.7), Some(0.8), Some(0.9), Some(0.95));
        assert_eq!(
            (
                kept.off_task_at,
                kept.needed_at,
                kept.avoidable_at,
                kept.fixable_at
            ),
            (0.7, 0.8, 0.9, 0.95)
        );
        assert!(warnings.is_empty());
        let (pulled, warnings) =
            ResolveThresholds::toward_caution(Some(0.95), Some(0.99), Some(0.5), Some(f64::NAN));
        assert_eq!(
            (
                pulled.off_task_at,
                pulled.needed_at,
                pulled.avoidable_at,
                pulled.fixable_at
            ),
            (0.85, 0.85, 0.85, 0.85)
        );
        assert_eq!(warnings.len(), 4);
    }

    /// D10/S11: each condition, one at a time, turns the B off for the call.
    #[test]
    fn every_condition_of_eligibility_counts_on_its_own() {
        let base = Eligibility {
            in_flight: true,
            run_mode: "worktree",
            job_id: None,
            resolution_lineage: false,
            resolve: JudgeMode::Observe,
            dont_ask: false,
            action_class: "unrecognized",
        };
        assert!(hard_deny_eligible(&base) && park_eligible(&base));
        assert!(!park_eligible(&Eligibility {
            in_flight: false,
            ..base
        }));
        assert!(!park_eligible(&Eligibility {
            run_mode: "shadow",
            ..base
        }));
        assert!(!park_eligible(&Eligibility {
            job_id: Some(3),
            ..base
        }));
        assert!(!park_eligible(&Eligibility {
            resolution_lineage: true,
            ..base
        }));
        assert!(!park_eligible(&Eligibility {
            resolve: JudgeMode::Off,
            ..base
        }));
        assert!(!park_eligible(&Eligibility {
            dont_ask: true,
            ..base
        }));
        assert!(!park_eligible(&Eligibility {
            action_class: "unrecognized-tool",
            ..base
        }));
        assert!(hard_deny_eligible(&Eligibility {
            dont_ask: true,
            action_class: "destructive",
            ..base
        }));
    }

    #[test]
    fn the_judges_phrase_names_the_question_and_the_number() {
        assert_eq!(
            phrase(Outcome::Stop, p(0.912, 0.0, 0.0)),
            "judge: stopped — off_task p=0.91"
        );
        assert_eq!(
            hard_deny_note("destructive"),
            "The action was blocked by a fixed rule (class: destructive)."
        );
    }
}
