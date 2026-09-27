//! The speed profile: `normal | fast | thorough`, and what each one buys.
//!
//! Spec: `.ai/specs/2026-09-23-perfil-de-velocidade-design.md`. The controller reads the same
//! table from `.ai/workflow/speed.yaml`, which is gitignored — so it cannot be embedded here, and
//! a fresh clone must still build. The table is therefore compiled in, and
//! `the_compiled_table_is_the_workflow_table` holds the two equal wherever that file exists.
//!
//! `normal` is today's behaviour, by definition and by test. Speed buys concurrency and cheaper
//! models; it never buys less verification, and it never creates capacity: every width it
//! computes is bounded by `MAX_PARALLEL_CEILING`.

use crate::team::MAX_PARALLEL_CEILING;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Speed {
    #[default]
    Normal,
    Fast,
    Thorough,
}

impl Speed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Fast => "fast",
            Self::Thorough => "thorough",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "fast" => Some(Self::Fast),
            "thorough" => Some(Self::Thorough),
            _ => None,
        }
    }

    /// A stored column. NULL is `normal`. Every value was validated on the way in, so an unknown
    /// one is a damaged row — resolved `normal` and said out loud, never guessed.
    pub fn from_column(value: Option<&str>) -> Self {
        match value {
            None => Self::Normal,
            Some(stored) => Self::parse(stored).unwrap_or_else(|| {
                tracing::warn!(
                    stored,
                    "a stored speed this daemon does not know; resolving normal"
                );
                Self::Normal
            }),
        }
    }
}

/// One row of the table in `.ai/workflow/speed.yaml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// The codex-safe subset: `low | medium | high | xhigh`.
    pub reasoning_effort: &'static str,
    pub fast_path_forbidden: bool,
    pub review_always: bool,
    pub wave: bool,
    pub team_multiplier: i64,
}

pub const fn profile(speed: Speed) -> Profile {
    match speed {
        Speed::Normal => Profile {
            reasoning_effort: "medium",
            fast_path_forbidden: false,
            review_always: false,
            wave: false,
            team_multiplier: 1,
        },
        Speed::Fast => Profile {
            reasoning_effort: "low",
            fast_path_forbidden: false,
            review_always: false,
            wave: true,
            team_multiplier: 2,
        },
        Speed::Thorough => Profile {
            reasoning_effort: "high",
            fast_path_forbidden: true,
            review_always: true,
            wave: false,
            team_multiplier: 1,
        },
    }
}

/// What a speed buys for one task. Same field names as the Python `SpeedDecision`, because
/// the parity test compares the two field by field.
#[derive(Debug, Clone, PartialEq, Eq)]
// No reader outside tests yet: the controller reads it by parity, and in plan 3 the daemon.
#[cfg_attr(not(test), allow(dead_code))]
pub struct SpeedDecision {
    pub workers: i64,
    pub reasoning_effort: &'static str,
    pub fast_path_allowed: bool,
    pub review_required: bool,
    pub team_max_parallel: Option<i64>,
    pub degraded: Vec<&'static str>,
}

// No reader outside tests yet: the controller reads it by parity, and in plan 3 the daemon.
#[cfg_attr(not(test), allow(dead_code))]
pub struct Task<'a> {
    pub size: &'a str,
    pub elevated: bool,
    pub tdd_able: bool,
    pub team_max_parallel: Option<i64>,
    pub max_workers: i64,
}

/// Section 5.2 of the spec. The team's own value is `normal`; `fast` multiplies it, bounded by
/// the ceiling. 1 means "this work is serial" and never rises — raising it would not be faster,
/// it would be wrong — which is reported as `team_serial` when the profile would have raised it.
pub fn team_width(speed: Speed, team_value: i64) -> (i64, Option<&'static str>) {
    let multiplier = profile(speed).team_multiplier;
    let value = team_value.clamp(1, MAX_PARALLEL_CEILING);
    if value == 1 {
        return (1, (multiplier > 1).then_some("team_serial"));
    }
    ((value * multiplier).min(MAX_PARALLEL_CEILING), None)
}

// No reader outside tests yet: the controller reads it by parity, and in plan 3 the daemon.
#[cfg_attr(not(test), allow(dead_code))]
pub fn resolve(speed: Speed, task: &Task) -> SpeedDecision {
    let profile = profile(speed);
    let mut degraded = Vec::new();
    let medium_or_large = matches!(task.size, "medium" | "large");

    let workers = if !profile.wave {
        1
    } else if medium_or_large && task.tdd_able {
        task.max_workers.max(1)
    } else {
        // Waves exist only for medium/large TDD-able work (spec, section 4.1).
        degraded.push("task_not_decomposable");
        1
    };

    let team_max_parallel = task.team_max_parallel.map(|value| {
        let (width, reason) = team_width(speed, value);
        degraded.extend(reason);
        width
    });

    SpeedDecision {
        workers,
        reasoning_effort: profile.reasoning_effort,
        fast_path_allowed: !profile.fast_path_forbidden,
        // Rule 6 is the floor under every profile; `thorough` only adds to it.
        review_required: profile.review_always || task.elevated || medium_or_large,
        team_max_parallel,
        degraded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const SPEEDS: [Speed; 3] = [Speed::Normal, Speed::Fast, Speed::Thorough];

    fn task(size: &str) -> Task<'_> {
        Task {
            size,
            elevated: false,
            tdd_able: false,
            team_max_parallel: None,
            max_workers: 2,
        }
    }

    /// The most important test of the lot: without it this is a behaviour change dressed as a
    /// new feature. Today there is no wave, the execute phase runs at `medium`, the fast path is
    /// the heuristic's call, review is rule 6, and a team opens exactly its own value.
    #[test]
    fn normal_is_todays_behaviour() {
        for value in 1..=MAX_PARALLEL_CEILING {
            assert_eq!(team_width(Speed::Normal, value), (value, None));
        }
        let decision = resolve(Speed::Normal, &task("large"));
        assert_eq!(decision.workers, 1);
        assert_eq!(decision.reasoning_effort, "medium");
        assert!(decision.fast_path_allowed);
        assert!(decision.degraded.is_empty());
    }

    /// Invariant (II): no profile opens more than the ceiling, whatever the column says.
    #[test]
    fn no_speed_opens_past_the_ceiling() {
        for speed in SPEEDS {
            for value in [1, 2, 5, 8, 99] {
                let (width, _) = team_width(speed, value);
                assert!(
                    (1..=MAX_PARALLEL_CEILING).contains(&width),
                    "{speed:?} {value} -> {width}"
                );
            }
        }
    }

    #[test]
    fn a_serial_team_is_never_raised() {
        assert_eq!(team_width(Speed::Fast, 1), (1, Some("team_serial")));
        assert_eq!(team_width(Speed::Thorough, 1), (1, None));
    }

    #[test]
    fn speed_round_trips_through_its_column() {
        for speed in SPEEDS {
            assert_eq!(Speed::from_column(Some(speed.as_str())), speed);
        }
        assert_eq!(Speed::from_column(None), Speed::Normal);
        assert_eq!(Speed::from_column(Some("fsat")), Speed::Normal);
    }

    // ---- parity with the workflow side ----
    //
    // Read at test time from the checkout this binary was COMPILED in (`CARGO_MANIFEST_DIR`).
    // `.ai/` is gitignored, so on a fresh clone the files are absent and these two tests say so
    // and pass — the same soft-skip `seed_worktree.py --check` takes where seeding is not its job.

    fn workflow_file(name: &str) -> Option<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../.ai/workflow")
            .join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(_) => {
                eprintln!(
                    "skipped: {} is absent (gitignored; fresh clone)",
                    path.display()
                );
                None
            }
        }
    }

    #[derive(serde::Deserialize)]
    struct FileTable {
        default: String,
        team_ceiling: i64,
        profiles: BTreeMap<String, FileProfile>,
    }

    #[derive(serde::Deserialize)]
    struct FileProfile {
        reasoning_effort: String,
        fast_path: String,
        review: String,
        wave: bool,
        team_multiplier: i64,
    }

    #[test]
    fn the_compiled_table_is_the_workflow_table() {
        let Some(text) = workflow_file("speed.yaml") else {
            return;
        };
        let table: FileTable = serde_yaml::from_str(&text).unwrap();
        assert_eq!(table.default, "normal");
        assert_eq!(table.team_ceiling, MAX_PARALLEL_CEILING);
        assert_eq!(table.profiles.len(), SPEEDS.len());
        for speed in SPEEDS {
            let file = &table.profiles[speed.as_str()];
            let compiled = profile(speed);
            assert_eq!(
                file.reasoning_effort, compiled.reasoning_effort,
                "{speed:?}"
            );
            assert_eq!(
                file.fast_path == "forbidden",
                compiled.fast_path_forbidden,
                "{speed:?}"
            );
            assert_eq!(file.review == "always", compiled.review_always, "{speed:?}");
            assert_eq!(file.wave, compiled.wave, "{speed:?}");
            assert_eq!(file.team_multiplier, compiled.team_multiplier, "{speed:?}");
        }
    }

    #[derive(serde::Deserialize)]
    struct Cases {
        cases: Vec<Case>,
    }

    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        input: CaseInput,
        expected: CaseExpected,
    }

    #[derive(serde::Deserialize)]
    struct CaseInput {
        speed: String,
        size: String,
        elevated: bool,
        tdd_able: bool,
        team_max_parallel: Option<i64>,
        max_workers: i64,
    }

    #[derive(serde::Deserialize)]
    struct CaseExpected {
        workers: i64,
        reasoning_effort: String,
        fast_path_allowed: bool,
        review_required: bool,
        team_max_parallel: Option<i64>,
        degraded: Vec<String>,
    }

    #[test]
    fn every_parity_case_resolves_as_the_workflow_expects() {
        let Some(text) = workflow_file("speed-cases.yaml") else {
            return;
        };
        let cases: Cases = serde_yaml::from_str(&text).unwrap();
        assert!(!cases.cases.is_empty());
        for case in cases.cases {
            let speed = Speed::parse(&case.input.speed).unwrap();
            let decision = resolve(
                speed,
                &Task {
                    size: &case.input.size,
                    elevated: case.input.elevated,
                    tdd_able: case.input.tdd_able,
                    team_max_parallel: case.input.team_max_parallel,
                    max_workers: case.input.max_workers,
                },
            );
            let expected = &case.expected;
            assert_eq!(decision.workers, expected.workers, "{}", case.name);
            assert_eq!(
                decision.reasoning_effort, expected.reasoning_effort,
                "{}",
                case.name
            );
            assert_eq!(
                decision.fast_path_allowed, expected.fast_path_allowed,
                "{}",
                case.name
            );
            assert_eq!(
                decision.review_required, expected.review_required,
                "{}",
                case.name
            );
            assert_eq!(
                decision.team_max_parallel, expected.team_max_parallel,
                "{}",
                case.name
            );
            assert_eq!(decision.degraded, expected.degraded, "{}", case.name);
        }
    }
}
