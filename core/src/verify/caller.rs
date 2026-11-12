//! Who asked for a verification and on what terms: the `Caller` kinds with their queue priority,
//! label and unit timeout, `may_read` for ticket visibility, and the `GateTerms` a daemon gate adds.

use std::time::Duration;

use crate::auth::{ApiTokenLevel, Scope};
use crate::verify::probe;
use crate::verify::runs::{
    self as verify_runs, PRIORITY_AUTONOMOUS, PRIORITY_FIX_REGATE, PRIORITY_INTERACTIVE,
};

/// Who asked, as far as verification cares: the owner, one autonomous run, or the daemon gating
/// a job item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Owner,
    Run(i64),
    /// The daemon's own item gate. `from_scope` never produces it, so no key can claim to be one.
    Job(i64),
    /// The merge queue's gate before publish; `from_scope` never produces it.
    Merge(i64),
    /// The daemon's post-merge gate. `from_scope` never produces it.
    Postgate,
    /// The post-merge gate's recheck of a red sha. `from_scope` never produces it.
    FlakeCheck,
    /// One probe of the post-merge gate's bisection. `from_scope` never produces it.
    Bisect,
    /// The daemon's proof of a red group at the master head. `from_scope` never produces it.
    Probe,
}

/// The post-merge gate runs the whole gate as one unit: run 86 took 1746 s and run 83 was cut at
/// the 1800 s default, so this is about twice the observed time.
pub const POSTGATE_UNIT_TIMEOUT: Duration = Duration::from_secs(3600);

/// What a daemon gate asks of the queue beyond who it is (autonomy-trees spec A7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GateTerms {
    /// The gate re-measures a fix of a red: served before the gate of a new branch.
    pub fix: bool,
    /// A ceiling per unit, in seconds, replacing the executor's configured one; 0 keeps it. Set by
    /// a gate whose last attempt hit the time ceiling.
    pub unit_timeout_secs: u64,
    /// A `test` request that runs only these `(group, test ids)` instead of the diff's scope: the
    /// first step of a fix's re-gate (`plan::plan_recheck`).
    pub recheck: Option<Vec<(String, Vec<String>)>>,
}

impl GateTerms {
    /// The priority the request is queued at: a fix's re-gate outranks the caller's own level, but
    /// never the owner's.
    pub fn priority(&self, caller: Caller) -> i64 {
        let own = caller.priority();
        if self.fix && own > PRIORITY_FIX_REGATE {
            PRIORITY_FIX_REGATE
        } else {
            own
        }
    }

    /// The unit timeout in ms: the terms' own when set, else the caller's.
    pub fn unit_timeout_ms(&self, caller: Caller) -> i64 {
        if self.unit_timeout_secs > 0 {
            i64::try_from(self.unit_timeout_secs.saturating_mul(1000)).unwrap_or(i64::MAX)
        } else {
            caller.unit_timeout_ms()
        }
    }
}

impl Caller {
    /// Every other key is refused: a sidecar, a team run or a narrower API token has no worktree
    /// of its own to verify, and running arbitrary groups is not part of what those keys grant.
    pub fn from_scope(scope: &Scope) -> Option<Caller> {
        match scope {
            Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin) => Some(Caller::Owner),
            Scope::Run(id) => Some(Caller::Run(*id)),
            _ => None,
        }
    }

    /// A person waiting at the keyboard goes ahead of an autonomous run (spec, "Prioridades").
    pub fn priority(self) -> i64 {
        match self {
            Caller::Merge(_) => PRIORITY_AUTONOMOUS,
            Caller::Owner => PRIORITY_INTERACTIVE,
            Caller::Run(_) | Caller::Job(_) => PRIORITY_AUTONOMOUS,
            Caller::Postgate | Caller::FlakeCheck | Caller::Bisect | Caller::Probe => {
                verify_runs::PRIORITY_POSTGATE
            }
        }
    }

    /// How the request row records its caller, and what `may_read` compares against.
    pub fn label(self) -> String {
        match self {
            Caller::Merge(id) => format!("merge:{id}"),
            Caller::Owner => "owner".to_owned(),
            Caller::Run(id) => format!("run:{id}"),
            Caller::Job(id) => format!("job:{id}"),
            Caller::Postgate | Caller::FlakeCheck | Caller::Bisect => "postgate".to_owned(),
            Caller::Probe => "probe".to_owned(),
        }
    }

    /// What a unit's `requested_by` records. Only the post-merge gate is named by caller; every
    /// other caller keeps the scope text, so existing readers see no new values.
    pub fn requested_by(self, scope: &str) -> String {
        match self {
            Caller::Postgate => verify_runs::REQUESTED_BY_POSTGATE.to_owned(),
            Caller::FlakeCheck => verify_runs::REQUESTED_BY_FLAKE_CHECK.to_owned(),
            Caller::Bisect => verify_runs::REQUESTED_BY_BISECT.to_owned(),
            // Named on purpose: `Executor::gate_env` treats a probe like the post-merge gate.
            Caller::Probe => verify_runs::REQUESTED_BY_PROBE.to_owned(),
            _ => scope.to_owned(),
        }
    }

    /// A proof unit is cut off well before its lease (`PROBE_LEASE_SECONDS`) lapses; the
    /// post-merge family gets `POSTGATE_UNIT_TIMEOUT`; every other caller keeps the executor's
    /// configured timeout (0 = "derive it").
    pub fn unit_timeout_ms(self) -> i64 {
        match self {
            Caller::Postgate | Caller::FlakeCheck | Caller::Bisect => {
                i64::try_from(POSTGATE_UNIT_TIMEOUT.as_millis()).unwrap_or(i64::MAX)
            }
            Caller::Probe => i64::try_from(probe::UNIT_TIMEOUT.as_millis()).unwrap_or(i64::MAX),
            _ => 0,
        }
    }
}

/// A run reads only its own tickets: another run's output tail may carry what that run was
/// working on, and a ticket id is a guessable integer.
pub fn may_read(caller: Caller, row_caller: &str) -> bool {
    match caller {
        Caller::Merge(_) => row_caller == caller.label(),
        Caller::Owner => true,
        Caller::Run(_)
        | Caller::Job(_)
        | Caller::Postgate
        | Caller::FlakeCheck
        | Caller::Bisect
        | Caller::Probe => row_caller == caller.label(),
    }
}
