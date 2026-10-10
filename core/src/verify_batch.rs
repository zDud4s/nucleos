//! Pure. Which commits one post-merge gate covers (spec 2026-10-05 §6.1).
//! Driven by `verify_postgate_worker`.

/// What the gate knows about one target branch.
pub struct TargetState<'a> {
    /// Current tip of the target.
    pub tip: &'a str,
    /// Last sha a post-merge gate passed on, if any.
    pub last_green: Option<&'a str>,
    /// Last tip a post-merge gate was started for, whatever its outcome.
    pub last_attempted: Option<&'a str>,
    /// A post-merge gate for this project is running right now.
    pub running: bool,
}

/// One first-parent commit of `(last_green, tip]`, oldest first.
pub struct Commit {
    pub sha: String,
    /// The full gate passed on exactly this sha before it was published, or a daemon verification
    /// of its exact tree passed over a green-or-covered base.
    pub covered: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Idle {
    Running,
    UpToDate,
    /// The tip changed but the first-parent list `(last_green, tip]` is empty. A caller must never
    /// reach this by turning a git failure into an empty list: that is an error, not idleness.
    NothingSinceGreen,
}

/// One gate's worth of commits.
#[derive(Debug, PartialEq, Eq)]
pub struct Batch {
    pub tip: String,
    pub base: Option<String>,
    /// Every commit since the last green, oldest first.
    pub commits: Vec<String>,
    /// How many of `commits` are new since the last attempt.
    pub fresh: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Idle(Idle),
    /// The tip already passed the full gate before publish: record it, run nothing.
    MarkCovered {
        sha: String,
    },
    Start(Batch),
}

/// Decide what the post-merge gate does for one target. `since_green` is the
/// first-parent list `(last_green, tip]`, oldest first.
pub fn decide(state: &TargetState<'_>, since_green: &[Commit]) -> Decision {
    if state.running {
        return Decision::Idle(Idle::Running);
    }
    if Some(state.tip) == state.last_attempted || Some(state.tip) == state.last_green {
        return Decision::Idle(Idle::UpToDate);
    }
    if since_green.is_empty() {
        return Decision::Idle(Idle::NothingSinceGreen);
    }
    if let Some(last) = since_green.last()
        && last.sha == state.tip
        && last.covered
    {
        return Decision::MarkCovered {
            sha: state.tip.to_string(),
        };
    }
    let len = since_green.len();
    let fresh = state
        .last_attempted
        .and_then(|a| since_green.iter().position(|c| c.sha == a))
        .map_or(len, |i| len - 1 - i);
    Decision::Start(Batch {
        tip: state.tip.to_string(),
        base: state.last_green.map(str::to_string),
        commits: since_green.iter().map(|c| c.sha.clone()).collect(),
        fresh,
    })
}

/// Why a new batch gate waits instead of starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// The last gate started too recently for `postgate_min_interval_mins`.
    Interval { remaining_secs: u64 },
    /// `postgate_idle_command` answered that the machine is busy.
    Busy,
}

/// Seconds left on the minimum interval between two batch-gate starts, or `None` when nothing
/// holds: the interval is 0 (off), no gate ever started, or the last start is old enough. A
/// negative `since_last_start_secs` (clock skew) counts as 0 s.
pub fn interval_hold(min_interval_mins: u64, since_last_start_secs: Option<i64>) -> Option<u64> {
    if min_interval_mins == 0 {
        return None;
    }
    let since = u64::try_from(since_last_start_secs?).unwrap_or(0);
    let window = min_interval_mins.saturating_mul(60);
    (since < window).then(|| window - since)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commits(shas: &[&str]) -> Vec<Commit> {
        shas.iter()
            .map(|s| Commit {
                sha: (*s).to_string(),
                covered: false,
            })
            .collect()
    }

    fn state<'a>(
        tip: &'a str,
        last_green: Option<&'a str>,
        last_attempted: Option<&'a str>,
        running: bool,
    ) -> TargetState<'a> {
        TargetState {
            tip,
            last_green,
            last_attempted,
            running,
        }
    }

    fn batch(d: Decision) -> Batch {
        match d {
            Decision::Start(b) => b,
            other => panic!("expected Start, got {other:?}"),
        }
    }

    #[test]
    fn a_running_postgate_keeps_the_project_idle() {
        let st = state("c", Some("a"), None, true);
        assert_eq!(
            decide(&st, &commits(&["b", "c"])),
            Decision::Idle(Idle::Running)
        );
    }

    #[test]
    fn a_new_tip_with_nothing_since_green_is_idle() {
        assert_eq!(
            decide(&state("c3", Some("c1"), None, false), &[]),
            Decision::Idle(Idle::NothingSinceGreen)
        );
        assert_eq!(
            decide(&state("c3", None, None, false), &[]),
            Decision::Idle(Idle::NothingSinceGreen)
        );
    }

    #[test]
    fn a_tip_already_attempted_starts_nothing() {
        let list = commits(&["b", "c"]);
        // Tip equals the last attempt.
        assert_eq!(
            decide(&state("c", Some("a"), Some("c"), false), &list),
            Decision::Idle(Idle::UpToDate)
        );
        // Tip equals the last green.
        assert_eq!(
            decide(&state("c", Some("c"), None, false), &[]),
            Decision::Idle(Idle::UpToDate)
        );
    }

    #[test]
    fn a_new_tip_starts_a_batch_from_the_last_green() {
        let b = batch(decide(
            &state("d", Some("a"), None, false),
            &commits(&["b", "c", "d"]),
        ));
        assert_eq!(b.tip, "d");
        assert_eq!(b.base.as_deref(), Some("a"));
        assert_eq!(b.commits, vec!["b", "c", "d"]);
    }

    #[test]
    fn only_commits_after_the_last_attempt_are_fresh() {
        let b = batch(decide(
            &state("e", Some("a"), Some("c"), false),
            &commits(&["b", "c", "d", "e"]),
        ));
        assert_eq!(b.fresh, 2);
        assert_eq!(b.commits.len(), 4);
    }

    #[test]
    fn an_unknown_last_attempt_makes_every_commit_fresh() {
        // Not attempted at all.
        let b = batch(decide(
            &state("d", Some("a"), None, false),
            &commits(&["b", "c", "d"]),
        ));
        assert_eq!(b.fresh, 3);
        // Attempted sha no longer in the list (history rewritten).
        let b = batch(decide(
            &state("d", Some("a"), Some("zzz"), false),
            &commits(&["b", "c", "d"]),
        ));
        assert_eq!(b.fresh, 3);
    }

    #[test]
    fn a_tip_covered_before_publish_is_marked_not_gated() {
        let mut list = commits(&["b", "c"]);
        list[1].covered = true;
        assert_eq!(
            decide(&state("c", Some("a"), None, false), &list),
            Decision::MarkCovered {
                sha: "c".to_string()
            }
        );
        // Covered but not the tip: still a batch.
        let mut list = commits(&["b", "c"]);
        list[0].covered = true;
        assert!(matches!(
            decide(&state("c", Some("a"), None, false), &list),
            Decision::Start(_)
        ));
    }

    #[test]
    fn a_project_never_green_batches_every_commit_given() {
        let b = batch(decide(
            &state("c", None, None, false),
            &commits(&["a", "b", "c"]),
        ));
        assert_eq!(b.base, None);
        assert_eq!(b.commits, vec!["a", "b", "c"]);
        assert_eq!(b.fresh, 3);
    }

    #[test]
    fn an_interval_holds_only_while_the_last_start_is_recent() {
        // 0 minutes switches the rule off, whatever the last start was.
        assert_eq!(interval_hold(0, Some(0)), None);
        assert_eq!(interval_hold(0, Some(5)), None);
        assert_eq!(interval_hold(0, None), None);

        // No earlier start: nothing to wait for.
        assert_eq!(interval_hold(30, None), None);

        // Recent: the remaining seconds, down to the last one.
        assert_eq!(interval_hold(30, Some(0)), Some(1800));
        assert_eq!(interval_hold(30, Some(600)), Some(1200));
        assert_eq!(interval_hold(30, Some(1799)), Some(1));

        // At and past the interval it lets go.
        assert_eq!(interval_hold(30, Some(1800)), None);
        assert_eq!(interval_hold(30, Some(7200)), None);

        // A start in the future (clock step) counts as just now, not as a negative wait.
        assert_eq!(interval_hold(30, Some(-90)), Some(1800));

        assert_eq!(
            Hold::Interval { remaining_secs: 4 },
            Hold::Interval { remaining_secs: 4 }
        );
        assert_ne!(Hold::Busy, Hold::Interval { remaining_secs: 0 });
    }
}
