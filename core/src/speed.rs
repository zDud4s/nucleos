//! The speed profile: `normal | fast | thorough`, and what each one buys.
//!
//! Spec: `.ai/specs/2026-09-23-perfil-de-velocidade-design.md`. This is the daemon's half only:
//! the speed's NAME and the CAPACITY it buys (`team_multiplier`, bounded by the ceiling). What a
//! speed means for the process — reasoning effort, the fast path, review, waves — belongs to the
//! workflow, which is a portable bundle the daemon does not read. The daemon hands a session the
//! two facts it owns through [`Capacity::env`], under neutral names, and the workflow decides the
//! rest on its own side.
//!
//! `normal` is today's behaviour, by definition and by test. Speed never creates capacity: every
//! width computed here is bounded by `MAX_PARALLEL_CEILING`, whatever the workflow later does
//! with the number.

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

/// How much wider than the team's own value a speed opens a round. The only row of the speed
/// table the daemon keeps: it is a capacity decision, and capacity is the daemon's to grant.
pub const fn team_multiplier(speed: Speed) -> i64 {
    match speed {
        Speed::Normal | Speed::Thorough => 1,
        Speed::Fast => 2,
    }
}

/// Section 5.2 of the spec. The team's own value is `normal`; `fast` multiplies it, bounded by
/// the ceiling. 1 means "this work is serial" and never rises — raising it would not be faster,
/// it would be wrong — which is reported as `team_serial` when the profile would have raised it.
pub fn team_width(speed: Speed, team_value: i64) -> (i64, Option<&'static str>) {
    let multiplier = team_multiplier(speed);
    let value = team_value.clamp(1, MAX_PARALLEL_CEILING);
    if value == 1 {
        return (1, (multiplier > 1).then_some("team_serial"));
    }
    ((value * multiplier).min(MAX_PARALLEL_CEILING), None)
}

/// The environment variable a launched session reads its speed from, spelled as the workflow
/// spells it (`normal | fast | thorough`).
///
/// Deliberately NOT `NUCLEOS_*`. The workflow is a bundle that runs in projects with no daemon at
/// all, and a name carrying this product's brand would teach it to depend on the product. The
/// daemon writes a neutral contract; whatever reads it need not know who wrote it.
pub const SPEED_VAR: &str = "WORKFLOW_SPEED";

/// The environment variable a launched session reads the daemon's parallel ceiling from.
pub const PARALLEL_CEILING_VAR: &str = "WORKFLOW_PARALLEL_CEILING";

/// What the daemon grants one session it launches: the speed it runs at, and how many sessions
/// wide the daemon itself is prepared to go for it.
///
/// Advisory to the child and never a promise the daemon keeps on the child's word: every place
/// that opens sessions still applies its own ceiling. A workflow that asks for more than it was
/// told gets no more than this, because the number is computed here and checked there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub speed: Speed,
    pub parallel_ceiling: i64,
}

impl Capacity {
    /// A session that is not part of a department: one run, at `normal`. A team of one, which is
    /// exactly what [`team_width`] answers for it — so the two can never disagree.
    pub fn solo() -> Self {
        Self::team(Speed::Normal, 1)
    }

    /// A department's session: its run's speed, and the round width that speed opens.
    pub fn team(speed: Speed, team_value: i64) -> Self {
        Self {
            speed,
            parallel_ceiling: team_width(speed, team_value).0,
        }
    }

    /// The two variables, in the order they are documented.
    pub fn env(self) -> [(String, String); 2] {
        [
            (SPEED_VAR.to_owned(), self.speed.as_str().to_owned()),
            (
                PARALLEL_CEILING_VAR.to_owned(),
                self.parallel_ceiling.to_string(),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEEDS: [Speed; 3] = [Speed::Normal, Speed::Fast, Speed::Thorough];

    /// The most important test of the lot: without it this is a behaviour change dressed as a
    /// new feature. Today a team opens exactly its own value.
    #[test]
    fn normal_is_todays_behaviour() {
        for value in 1..=MAX_PARALLEL_CEILING {
            assert_eq!(team_width(Speed::Normal, value), (value, None));
        }
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
                assert_eq!(Capacity::team(speed, value).parallel_ceiling, width);
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

    /// The contract a launched session reads: neutral names, the workflow's own spelling of the
    /// speed, and the width this module computed — not the team's raw column.
    #[test]
    fn a_session_is_told_its_speed_and_ceiling_under_neutral_names() {
        let pairs = |capacity: Capacity| -> Vec<(String, String)> { capacity.env().to_vec() };
        let expected = |speed: &str, ceiling: &str| {
            vec![
                ("WORKFLOW_SPEED".to_owned(), speed.to_owned()),
                ("WORKFLOW_PARALLEL_CEILING".to_owned(), ceiling.to_owned()),
            ]
        };
        assert_eq!(
            pairs(Capacity::team(Speed::Normal, 2)),
            expected("normal", "2")
        );
        assert_eq!(pairs(Capacity::team(Speed::Fast, 2)), expected("fast", "4"));
        assert_eq!(
            pairs(Capacity::team(Speed::Thorough, 3)),
            expected("thorough", "3")
        );
        assert_eq!(
            pairs(Capacity::team(Speed::Fast, 99)),
            expected("fast", "8")
        );
        assert_eq!(pairs(Capacity::solo()), expected("normal", "1"));
        for (name, _) in Capacity::solo().env() {
            assert!(!name.starts_with("NUCLEOS"), "{name} names the product");
        }
    }
}
