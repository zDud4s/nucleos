//! Session liveness: tells a session that is alive and producing from one that only looks alive.
//!
//! [`classify`] is a pure function from one [`Observation`] of a subject (a daemon run, a tree
//! node, a fleet root) to a [`Veredicto`]. It reads no clock, registry or file: the sources that
//! fill the observation and the sweep that acts on the verdict live elsewhere.
//!
//! It never alarms without evidence: an observation with nothing known about it is `Healthy`.

use chrono::{DateTime, Duration, Utc};

/// What a subject is doing, as far as its observation shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Veredicto {
    Healthy,
    /// Quiet for a legitimate reason. Does not alarm.
    Waiting(Motivo),
    /// Process alive, and no transcript write since it started.
    SilentSinceStart,
    /// Registry status `Waiting` for too long with no reason the daemon knows of, such as a trust
    /// or permission dialog.
    BlockedOnPrompt,
    /// Registry status `Busy` with a transcript that stopped being written.
    Stalled,
    /// No process, while the registry still shows a running state.
    Dead,
}

/// Why a [`Veredicto::Waiting`] subject is legitimately quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motivo {
    RunningChildren,
    OwnerDecision,
    AwaitingApproval,
    ToolInFlight,
}

/// The `status` a subject holds in the session registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectStatus {
    Busy,
    Waiting,
}

/// Everything known about one subject at one moment. `None` means unknown, not "no".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    pub process_alive: Option<bool>,
    pub status: Option<SubjectStatus>,
    pub status_since: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_transcript_write: Option<DateTime<Utc>>,
    /// Carried for the wave 3 sources; not consulted by [`classify`].
    pub last_stream_event: Option<DateTime<Utc>>,
    /// Carried for the wave 3 sources; not consulted by [`classify`].
    pub last_hook_seen: Option<DateTime<Utc>>,
    /// Deadline of the tool call in flight, from the policy clock.
    pub tool_in_flight_until: Option<DateTime<Utc>>,
    pub running_children: i64,
    pub owner_decision_pending: bool,
    pub awaiting_approval: bool,
}

/// How long each symptom must last before it counts. All comparisons are strict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub silent_after: Duration,
    pub blocked_after: Duration,
    pub stalled_after: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            silent_after: Duration::minutes(3),
            blocked_after: Duration::minutes(2),
            stalled_after: Duration::minutes(10),
        }
    }
}

/// Classifies one observation. The first matching rule wins, so the order below is the contract.
pub fn classify(obs: &Observation, limits: &Limits, now: DateTime<Utc>) -> Veredicto {
    // A dead process outranks every reason to wait: nothing is left to wait for.
    if obs.process_alive == Some(false) {
        return if obs.status.is_some() {
            Veredicto::Dead
        } else {
            // No running state left in the registry: a finished process.
            Veredicto::Healthy
        };
    }

    if let Some(motivo) = waiting_reason(obs, now) {
        return Veredicto::Waiting(motivo);
    }

    // Before silence on purpose: a trust dialog also never writes a transcript, and this verdict
    // is the more specific one.
    if obs.status == Some(SubjectStatus::Waiting)
        && obs
            .status_since
            .is_some_and(|since| now - since > limits.blocked_after)
    {
        return Veredicto::BlockedOnPrompt;
    }

    if is_silent_since_start(obs, limits, now) {
        return Veredicto::SilentSinceStart;
    }

    if is_stalled(obs, limits, now) {
        return Veredicto::Stalled;
    }

    Veredicto::Healthy
}

fn waiting_reason(obs: &Observation, now: DateTime<Utc>) -> Option<Motivo> {
    if obs.running_children > 0 {
        Some(Motivo::RunningChildren)
    } else if obs.owner_decision_pending {
        Some(Motivo::OwnerDecision)
    } else if obs.awaiting_approval {
        Some(Motivo::AwaitingApproval)
    } else if obs.tool_in_flight_until.is_some_and(|until| until > now) {
        // Strict: a deadline equal to now has already run out.
        Some(Motivo::ToolInFlight)
    } else {
        None
    }
}

fn is_silent_since_start(obs: &Observation, limits: &Limits, now: DateTime<Utc>) -> bool {
    let Some(started_at) = obs.started_at else {
        return false;
    };
    obs.process_alive == Some(true)
        && now - started_at > limits.silent_after
        && obs
            .last_transcript_write
            .is_none_or(|written| written < started_at)
}

fn is_stalled(obs: &Observation, limits: &Limits, now: DateTime<Utc>) -> bool {
    if obs.status != Some(SubjectStatus::Busy) {
        return false;
    }
    // A never-written transcript is the silence rule's case: an unknown process must not alarm
    // here without evidence.
    let Some(written) = obs.last_transcript_write else {
        return false;
    };
    // A write from before this start says nothing about this start, so the clock runs from
    // whichever came later.
    let active_since = obs
        .started_at
        .map_or(written, |started| written.max(started));
    now - active_since > limits.stalled_after
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, TimeZone, Utc};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 30, 12, 0, 0).unwrap()
    }

    fn minutes_ago(minutes: i64) -> DateTime<Utc> {
        now() - Duration::minutes(minutes)
    }

    fn alive() -> Observation {
        Observation {
            process_alive: Some(true),
            ..Default::default()
        }
    }

    fn verdict(obs: &Observation) -> Veredicto {
        classify(obs, &Limits::default(), now())
    }

    #[test]
    fn running_children_is_waiting_not_stalled() {
        // Every silent and stalled trigger is armed, so only the children can explain Waiting.
        let stale_busy = Observation {
            status: Some(SubjectStatus::Busy),
            started_at: Some(minutes_ago(60)),
            last_transcript_write: Some(minutes_ago(30)),
            running_children: 1,
            ..alive()
        };
        assert_eq!(
            verdict(&stale_busy),
            Veredicto::Waiting(Motivo::RunningChildren)
        );

        let never_wrote = Observation {
            started_at: Some(minutes_ago(60)),
            running_children: 1,
            ..alive()
        };
        assert_eq!(
            verdict(&never_wrote),
            Veredicto::Waiting(Motivo::RunningChildren)
        );

        let waiting_on_children = Observation {
            status: Some(SubjectStatus::Waiting),
            status_since: Some(minutes_ago(30)),
            last_transcript_write: Some(minutes_ago(1)),
            running_children: 2,
            ..alive()
        };
        assert_eq!(
            verdict(&waiting_on_children),
            Veredicto::Waiting(Motivo::RunningChildren)
        );
    }

    #[test]
    fn silent_since_start_after_three_minutes() {
        let silent = Observation {
            started_at: Some(minutes_ago(4)),
            ..alive()
        };
        assert_eq!(verdict(&silent), Veredicto::SilentSinceStart);

        let silent_while_busy = Observation {
            status: Some(SubjectStatus::Busy),
            ..silent.clone()
        };
        assert_eq!(verdict(&silent_while_busy), Veredicto::SilentSinceStart);

        // Any transcript write since the start clears the silence.
        let wrote_once = Observation {
            last_transcript_write: Some(minutes_ago(3)),
            ..silent.clone()
        };
        assert_eq!(verdict(&wrote_once), Veredicto::Healthy);

        // The rule needs a process known to be alive.
        let process_unknown = Observation {
            process_alive: None,
            ..silent
        };
        assert_eq!(verdict(&process_unknown), Veredicto::Healthy);
    }

    #[test]
    fn silent_before_three_minutes_is_healthy() {
        let obs = Observation {
            status: Some(SubjectStatus::Busy),
            started_at: Some(minutes_ago(2)),
            ..alive()
        };
        assert_eq!(verdict(&obs), Veredicto::Healthy);
    }

    #[test]
    fn waiting_status_without_children_is_blocked_on_prompt_after_two_minutes() {
        let blocked = Observation {
            status: Some(SubjectStatus::Waiting),
            status_since: Some(minutes_ago(3)),
            last_transcript_write: Some(minutes_ago(3)),
            ..alive()
        };
        assert_eq!(verdict(&blocked), Veredicto::BlockedOnPrompt);

        let too_recent = Observation {
            status_since: Some(minutes_ago(1)),
            ..blocked.clone()
        };
        assert_eq!(verdict(&too_recent), Veredicto::Healthy);

        // A long-held busy status is not a prompt, whatever status_since says.
        let busy_long_held = Observation {
            status: Some(SubjectStatus::Busy),
            status_since: Some(minutes_ago(30)),
            last_transcript_write: Some(minutes_ago(1)),
            ..alive()
        };
        assert_eq!(verdict(&busy_long_held), Veredicto::Healthy);
    }

    #[test]
    fn busy_without_transcript_write_is_stalled_after_ten_minutes() {
        let stalled = Observation {
            status: Some(SubjectStatus::Busy),
            started_at: Some(minutes_ago(60)),
            last_transcript_write: Some(minutes_ago(11)),
            ..alive()
        };
        assert_eq!(verdict(&stalled), Veredicto::Stalled);

        let wrote_recently = Observation {
            last_transcript_write: Some(minutes_ago(9)),
            ..stalled.clone()
        };
        assert_eq!(verdict(&wrote_recently), Veredicto::Healthy);

        // The same stale transcript under a fresh waiting status is not a stall.
        let not_busy = Observation {
            status: Some(SubjectStatus::Waiting),
            status_since: Some(minutes_ago(1)),
            ..stalled
        };
        assert_eq!(verdict(&not_busy), Veredicto::Healthy);
    }

    #[test]
    fn dead_process_with_running_state_is_dead() {
        let dead = Observation {
            process_alive: Some(false),
            status: Some(SubjectStatus::Busy),
            ..Default::default()
        };
        assert_eq!(verdict(&dead), Veredicto::Dead);

        // Dead comes before every Waiting reason.
        let dead_with_waiting_reasons = Observation {
            status: Some(SubjectStatus::Waiting),
            running_children: 3,
            owner_decision_pending: true,
            awaiting_approval: true,
            tool_in_flight_until: Some(now() + Duration::minutes(5)),
            ..dead.clone()
        };
        assert_eq!(verdict(&dead_with_waiting_reasons), Veredicto::Dead);

        // No running state left in the registry: a finished process, not a dead one.
        let no_running_state = Observation {
            status: None,
            ..dead
        };
        assert_eq!(verdict(&no_running_state), Veredicto::Healthy);
    }

    #[test]
    fn tool_in_flight_inside_policy_clock_is_healthy() {
        // The plan's name says healthy; plan line 84 and spec line 80 say Waiting, asserted here.
        let obs = Observation {
            status: Some(SubjectStatus::Busy),
            started_at: Some(minutes_ago(60)),
            last_transcript_write: Some(minutes_ago(30)),
            tool_in_flight_until: Some(now() + Duration::minutes(5)),
            ..alive()
        };
        assert_eq!(verdict(&obs), Veredicto::Waiting(Motivo::ToolInFlight));
    }

    #[test]
    fn parked_for_owner_decision_is_waiting() {
        // Without the decision this exact observation would be BlockedOnPrompt.
        let obs = Observation {
            status: Some(SubjectStatus::Waiting),
            status_since: Some(minutes_ago(30)),
            last_transcript_write: Some(minutes_ago(30)),
            owner_decision_pending: true,
            ..alive()
        };
        assert_eq!(verdict(&obs), Veredicto::Waiting(Motivo::OwnerDecision));
    }

    #[test]
    fn unknown_observation_is_healthy() {
        assert_eq!(verdict(&Observation::default()), Veredicto::Healthy);

        // Old timestamps alone never alarm while process and status are unknown.
        let only_old_timestamps = Observation {
            status_since: Some(minutes_ago(600)),
            started_at: Some(minutes_ago(600)),
            last_transcript_write: Some(minutes_ago(600)),
            last_stream_event: Some(minutes_ago(600)),
            last_hook_seen: Some(minutes_ago(600)),
            ..Default::default()
        };
        assert_eq!(verdict(&only_old_timestamps), Veredicto::Healthy);
    }

    #[test]
    fn awaiting_approval_is_waiting() {
        // Without the approval this exact observation would be BlockedOnPrompt.
        let obs = Observation {
            status: Some(SubjectStatus::Waiting),
            status_since: Some(minutes_ago(30)),
            last_transcript_write: Some(minutes_ago(30)),
            awaiting_approval: true,
            ..alive()
        };
        assert_eq!(verdict(&obs), Veredicto::Waiting(Motivo::AwaitingApproval));
    }

    #[test]
    fn default_limits_are_three_two_ten_minutes() {
        let limits = Limits::default();
        assert_eq!(limits.silent_after, Duration::minutes(3));
        assert_eq!(limits.blocked_after, Duration::minutes(2));
        assert_eq!(limits.stalled_after, Duration::minutes(10));
    }

    #[test]
    fn tool_past_policy_clock_falls_through_to_stalled() {
        let past_deadline = Observation {
            status: Some(SubjectStatus::Busy),
            started_at: Some(minutes_ago(60)),
            last_transcript_write: Some(minutes_ago(30)),
            tool_in_flight_until: Some(minutes_ago(1)),
            ..alive()
        };
        assert_eq!(verdict(&past_deadline), Veredicto::Stalled);

        // A deadline equal to now has already run out.
        let deadline_is_now = Observation {
            tool_in_flight_until: Some(now()),
            ..past_deadline
        };
        assert_eq!(verdict(&deadline_is_now), Veredicto::Stalled);
    }

    #[test]
    fn limits_are_strict_at_the_boundary() {
        // Non-default values, so a classifier that ignores its Limits argument fails too.
        let limits = Limits {
            silent_after: Duration::minutes(7),
            blocked_after: Duration::minutes(5),
            stalled_after: Duration::minutes(20),
        };
        let one_second = Duration::seconds(1);
        let cases = [
            (
                Observation {
                    started_at: Some(now() - limits.silent_after),
                    ..alive()
                },
                Observation {
                    started_at: Some(now() - limits.silent_after - one_second),
                    ..alive()
                },
                Veredicto::SilentSinceStart,
            ),
            (
                Observation {
                    status: Some(SubjectStatus::Waiting),
                    status_since: Some(now() - limits.blocked_after),
                    last_transcript_write: Some(minutes_ago(1)),
                    ..alive()
                },
                Observation {
                    status: Some(SubjectStatus::Waiting),
                    status_since: Some(now() - limits.blocked_after - one_second),
                    last_transcript_write: Some(minutes_ago(1)),
                    ..alive()
                },
                Veredicto::BlockedOnPrompt,
            ),
            (
                Observation {
                    status: Some(SubjectStatus::Busy),
                    last_transcript_write: Some(now() - limits.stalled_after),
                    ..alive()
                },
                Observation {
                    status: Some(SubjectStatus::Busy),
                    last_transcript_write: Some(now() - limits.stalled_after - one_second),
                    ..alive()
                },
                Veredicto::Stalled,
            ),
        ];
        for (at_limit, past_limit, expected) in cases {
            assert_eq!(classify(&at_limit, &limits, now()), Veredicto::Healthy);
            assert_eq!(classify(&past_limit, &limits, now()), expected);
        }
    }
}
