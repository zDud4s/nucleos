//! The decisions of `nucleos-core --supervise`, the Windows task's restart loop.
//!
//! Task Scheduler's `RestartOnFailure` was measured inert here (an exit 7 did not relaunch in 200 s),
//! so the logon task runs the daemon's own binary in a supervising mode that launches the daemon as
//! a child and relaunches it after a failure. Everything that DECIDES lives here as pure functions
//! over exit codes and durations; spawning, waiting and sleeping belong to the daemon's `main`.
//!
//! The rule that matters: only a real failure is restarted. A deliberate stop must stay stopped, and
//! the tools that stop the daemon on purpose leave their own codes (`Stop-Process` exits -1,
//! `taskkill /F` exits 1, Ctrl+C or a logoff exits `STATUS_CONTROL_C_EXIT`).

use std::time::Duration;

/// The flag that selects the supervising mode.
pub const SUPERVISE_FLAG: &str = "--supervise";

/// The exit code of a daemon whose port is already taken, i.e. another daemon is up. Distinct from
/// the panic code (101) that an unchecked `bind().unwrap()` produced, so a second instance is not
/// mistaken for a crash.
pub const EXIT_PORT_BUSY: i32 = 20;

/// The exit code of a daemon that cannot use the system credential store (for instance, the vault
/// refuses the daemon token). A failure like any other, so it is relaunched within the budget; the
/// code only keeps it apart from the plain `1`, which means "somebody stopped it".
pub const EXIT_CREDENTIAL_STORE: i32 = 78;

/// `STATUS_CONTROL_C_EXIT` (0xC000013A) as the signed code `ExitStatus::code` reports.
pub const EXIT_CONTROL_C: i32 = 0xC000_013A_u32 as i32;

/// How the supervisor reads one exit of the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// Exit 0: shutdown request, update drain.
    Clean,
    /// Somebody stopped it on purpose: `Stop-Process` (-1), `taskkill /F` (1), Ctrl+C, logoff.
    Deliberate,
    /// Another daemon already owns the port.
    PortBusy,
    /// Anything else, including a panic (101) and a process the OS ended without a code.
    Failure,
}

/// Why the supervisor stopped on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The daemon left cleanly or was stopped on purpose.
    Stopped,
    /// Another daemon owns the port.
    PortBusy,
}

/// What to do after the daemon exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Leave; the restart is not wanted.
    Stop(StopReason),
    /// Relaunch the daemon after this pause.
    Restart { after: Duration },
    /// Too many failures inside the window: log it and leave.
    GiveUp,
}

/// The restart budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The pause before the first restart; it doubles with every further one.
    pub base_backoff: Duration,
    /// The longest pause.
    pub max_backoff: Duration,
    /// How many restarts the window allows; the next failure gives up.
    pub max_restarts: u32,
    /// How far back restarts count.
    pub window: Duration,
    /// A run at least this long was healthy: the history and the backoff start over.
    pub healthy_after: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            base_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
            max_restarts: 5,
            window: Duration::from_secs(10 * 60),
            healthy_after: Duration::from_secs(5 * 60),
        }
    }
}

/// PURE: whether the arguments ask for the supervising mode (`--supervise` as its own word).
pub fn wants_supervisor(args: &[String]) -> bool {
    args.iter().skip(1).any(|arg| arg == SUPERVISE_FLAG)
}

/// PURE: classifies an exit code. `None` is a process the OS ended without a code.
pub fn classify_exit(code: Option<i32>) -> ExitKind {
    match code {
        Some(0) => ExitKind::Clean,
        Some(1 | -1 | EXIT_CONTROL_C) => ExitKind::Deliberate,
        Some(EXIT_PORT_BUSY) => ExitKind::PortBusy,
        _ => ExitKind::Failure,
    }
}

/// PURE: whether an exit with this code is worth a relaunch at all (before the budget is looked at).
pub fn should_restart(code: Option<i32>) -> bool {
    classify_exit(code) == ExitKind::Failure
}

/// PURE: the pause before restart number `restarts_in_window` (0 is the first restart).
pub fn backoff(policy: &Policy, restarts_in_window: u32) -> Duration {
    let factor = 1u32.checked_shl(restarts_in_window).unwrap_or(u32::MAX);
    policy
        .base_backoff
        .checked_mul(factor)
        .map_or(policy.max_backoff, |pause| pause.min(policy.max_backoff))
}

/// The restart history of one supervisor. Times are offsets from the supervisor's own start, so
/// the logic never reads a clock.
#[derive(Debug, Clone)]
pub struct Tracker {
    policy: Policy,
    restarts: Vec<Duration>,
}

impl Tracker {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            restarts: Vec::new(),
        }
    }

    /// How many restarts currently count against the budget.
    pub fn restarts_counted(&self) -> usize {
        self.restarts.len()
    }

    /// Records one exit of the daemon, started at `started` and ended at `ended`, and decides.
    pub fn on_exit(&mut self, code: Option<i32>, started: Duration, ended: Duration) -> Decision {
        match classify_exit(code) {
            ExitKind::Clean | ExitKind::Deliberate => return Decision::Stop(StopReason::Stopped),
            ExitKind::PortBusy => return Decision::Stop(StopReason::PortBusy),
            ExitKind::Failure => {}
        }
        if ended.saturating_sub(started) >= self.policy.healthy_after {
            self.restarts.clear();
        }
        let window = self.policy.window;
        self.restarts
            .retain(|at| ended.saturating_sub(*at) < window);
        if self.restarts.len() >= self.policy.max_restarts as usize {
            return Decision::GiveUp;
        }
        let after = backoff(&self.policy, self.restarts.len() as u32);
        self.restarts.push(ended);
        Decision::Restart { after }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn supervise_is_chosen_only_by_the_exact_flag() {
        assert!(wants_supervisor(&words("nucleos-core.exe --supervise")));
        assert!(wants_supervisor(&words(
            "nucleos-core.exe --foo --supervise"
        )));
        assert!(!wants_supervisor(&words("nucleos-core.exe")));
        assert!(!wants_supervisor(&words("nucleos-core.exe --supervised")));
        assert!(!wants_supervisor(&words("nucleos-core.exe --no-supervise")));
        // The program name is not an argument: a path that happens to read like the flag is not one.
        assert!(!wants_supervisor(&words("--supervise")));
        assert!(!wants_supervisor(&[]));
    }

    #[test]
    fn a_clean_exit_is_not_restarted() {
        assert_eq!(classify_exit(Some(0)), ExitKind::Clean);
        assert!(!should_restart(Some(0)));
    }

    /// `Stop-Process` exits -1 and `taskkill /F` exits 1; `--settings-reset` and the installer
    /// rely on both staying stopped. Ctrl+C and a logoff end the process with
    /// `STATUS_CONTROL_C_EXIT`.
    #[test]
    fn the_exits_the_stopping_tools_leave_are_deliberate() {
        for code in [1, -1, EXIT_CONTROL_C] {
            assert_eq!(classify_exit(Some(code)), ExitKind::Deliberate, "{code}");
            assert!(!should_restart(Some(code)), "{code}");
        }
        assert_eq!(EXIT_CONTROL_C, -1_073_741_510);
    }

    #[test]
    fn a_taken_port_is_a_second_instance_and_not_restarted() {
        assert_eq!(classify_exit(Some(EXIT_PORT_BUSY)), ExitKind::PortBusy);
        assert!(!should_restart(Some(EXIT_PORT_BUSY)));
        // A panic must not look like a taken port.
        assert_ne!(EXIT_PORT_BUSY, 101);
    }

    #[test]
    fn a_panic_a_plain_error_and_a_missing_code_are_failures() {
        for code in [Some(101), Some(7), Some(2), Some(3), None] {
            assert_eq!(classify_exit(code), ExitKind::Failure, "{code:?}");
            assert!(should_restart(code), "{code:?}");
        }
    }

    #[test]
    fn a_credential_store_failure_is_restarted_and_is_not_the_deliberate_one() {
        assert_ne!(EXIT_CREDENTIAL_STORE, 1);
        assert_eq!(
            classify_exit(Some(EXIT_CREDENTIAL_STORE)),
            ExitKind::Failure
        );
        assert!(should_restart(Some(EXIT_CREDENTIAL_STORE)));
    }

    #[test]
    fn the_backoff_doubles_and_stops_at_the_cap() {
        let policy = Policy {
            base_backoff: secs(2),
            max_backoff: secs(20),
            ..Policy::default()
        };
        let pauses: Vec<Duration> = (0..6).map(|n| backoff(&policy, n)).collect();
        assert_eq!(
            pauses,
            vec![secs(2), secs(4), secs(8), secs(16), secs(20), secs(20)]
        );
        // A count far past the cap must not overflow.
        assert_eq!(backoff(&policy, 200), secs(20));
    }

    #[test]
    fn a_failure_restarts_with_a_growing_pause() {
        let policy = Policy {
            base_backoff: secs(2),
            max_backoff: secs(60),
            max_restarts: 5,
            window: secs(600),
            healthy_after: secs(300),
        };
        let mut tracker = Tracker::new(policy);
        assert_eq!(
            tracker.on_exit(Some(7), secs(0), secs(1)),
            Decision::Restart { after: secs(2) }
        );
        assert_eq!(
            tracker.on_exit(Some(7), secs(4), secs(5)),
            Decision::Restart { after: secs(4) }
        );
        assert_eq!(tracker.restarts_counted(), 2);
    }

    #[test]
    fn clean_deliberate_and_port_busy_exits_stop_the_supervisor_without_spending_budget() {
        let mut tracker = Tracker::new(Policy::default());
        for code in [0, 1, -1, EXIT_CONTROL_C] {
            assert_eq!(
                tracker.on_exit(Some(code), secs(0), secs(1)),
                Decision::Stop(StopReason::Stopped),
                "{code}"
            );
        }
        assert_eq!(
            tracker.on_exit(Some(EXIT_PORT_BUSY), secs(0), secs(1)),
            Decision::Stop(StopReason::PortBusy)
        );
        assert_eq!(tracker.restarts_counted(), 0);
    }

    #[test]
    fn the_budget_runs_out_inside_the_window() {
        let policy = Policy {
            max_restarts: 3,
            window: secs(600),
            healthy_after: secs(300),
            ..Policy::default()
        };
        let mut tracker = Tracker::new(policy);
        for n in 0..3 {
            let at = secs(10 * n);
            assert!(
                matches!(
                    tracker.on_exit(Some(101), at, at + secs(1)),
                    Decision::Restart { .. }
                ),
                "restart {n} is within the budget"
            );
        }
        assert_eq!(
            tracker.on_exit(Some(101), secs(40), secs(41)),
            Decision::GiveUp
        );
    }

    #[test]
    fn restarts_older_than_the_window_stop_counting() {
        let policy = Policy {
            max_restarts: 2,
            window: secs(100),
            healthy_after: secs(10_000),
            ..Policy::default()
        };
        let mut tracker = Tracker::new(policy);
        assert!(matches!(
            tracker.on_exit(Some(101), secs(0), secs(1)),
            Decision::Restart { .. }
        ));
        assert!(matches!(
            tracker.on_exit(Some(101), secs(5), secs(6)),
            Decision::Restart { .. }
        ));
        // 500 s later the two restarts are outside the 100 s window: the budget is whole again.
        assert!(matches!(
            tracker.on_exit(Some(101), secs(500), secs(501)),
            Decision::Restart { .. }
        ));
        assert_eq!(tracker.restarts_counted(), 1);
    }

    /// A daemon that ran healthy for `healthy_after` clears the history, so a failure after a long
    /// good run restarts with the first, short pause rather than the next one.
    #[test]
    fn a_healthy_run_resets_the_budget_and_the_backoff() {
        let policy = Policy {
            base_backoff: secs(2),
            max_backoff: secs(60),
            max_restarts: 2,
            window: secs(100_000),
            healthy_after: secs(300),
        };
        let mut tracker = Tracker::new(policy);
        for n in 0..2 {
            let at = secs(10 * n);
            assert!(matches!(
                tracker.on_exit(Some(101), at, at + secs(1)),
                Decision::Restart { .. }
            ));
        }
        // The budget is spent, but this run lasted 400 s before it failed.
        assert_eq!(
            tracker.on_exit(Some(101), secs(100), secs(500)),
            Decision::Restart { after: secs(2) }
        );
        assert_eq!(tracker.restarts_counted(), 1);
    }

    #[test]
    fn a_run_just_short_of_healthy_does_not_reset() {
        let policy = Policy {
            max_restarts: 1,
            window: secs(100_000),
            healthy_after: secs(300),
            ..Policy::default()
        };
        let mut tracker = Tracker::new(policy);
        assert!(matches!(
            tracker.on_exit(Some(101), secs(0), secs(1)),
            Decision::Restart { .. }
        ));
        assert_eq!(
            tracker.on_exit(Some(101), secs(10), secs(309)),
            Decision::GiveUp
        );
    }

    #[test]
    fn the_default_policy_is_the_documented_budget() {
        let policy = Policy::default();
        assert_eq!(policy.base_backoff, secs(2));
        assert_eq!(policy.max_backoff, secs(60));
        assert_eq!(policy.max_restarts, 5);
        assert_eq!(policy.window, secs(600));
        assert_eq!(policy.healthy_after, secs(300));
    }

    #[test]
    fn a_run_of_exactly_healthy_after_resets_the_history() {
        let policy = Policy {
            max_restarts: 1,
            ..Policy::default()
        };
        let mut tracker = Tracker::new(policy);
        assert!(matches!(
            tracker.on_exit(Some(101), secs(0), secs(1)),
            Decision::Restart { .. }
        ));
        // Exactly 300 s: the budget of one is spent, yet the history starts over.
        assert_eq!(
            tracker.on_exit(Some(101), secs(10), secs(310)),
            Decision::Restart { after: secs(2) }
        );
        assert_eq!(tracker.restarts_counted(), 1);
    }

    #[test]
    fn a_restart_exactly_one_window_old_no_longer_counts() {
        let policy = Policy {
            max_restarts: 1,
            ..Policy::default()
        };
        let mut tracker = Tracker::new(policy);
        assert!(matches!(
            tracker.on_exit(Some(101), secs(0), secs(1)),
            Decision::Restart { .. }
        ));
        // 599 s old: still inside the 600 s window.
        let mut still_counts = tracker.clone();
        assert_eq!(
            still_counts.on_exit(Some(101), secs(599), secs(600)),
            Decision::GiveUp
        );
        // Exactly 600 s old: outside.
        assert!(matches!(
            tracker.on_exit(Some(101), secs(600), secs(601)),
            Decision::Restart { .. }
        ));
        assert_eq!(tracker.restarts_counted(), 1);
    }
}
