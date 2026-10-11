//! The hook payloads and what a decision is read against: the run snapshot, the project's shell and GitHub rules, and the hook's time budget.

use super::*;

#[derive(Deserialize)]
pub struct PreToolUsePayload {
    // The run this decision is for (spec §3.3). `run_id == runs.id` — injected into the CLI's
    // environment as NUCLEOS_RUN_ID (Step 5) and echoed back by the hook script; there is no second
    // identifier.
    pub run_id: i64,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
}

/// What arrives with a `PostToolUse`/`PostToolUseFailure` report: the OUTCOME of a call this
/// daemon already decided about (spec-adjacent to `PreToolUsePayload`, but never a second
/// decision).
#[derive(Deserialize)]
pub struct PostToolUsePayload {
    // Same claim-vs-key relationship `posttooluse_outcome` resolves for `PreToolUsePayload::run_id`
    // above: a scoped key names its own run and this claim is dropped; a key naming no run leaves
    // the claim as the only identifier there is.
    pub run_id: i64,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    // The field `PreToolUsePayload` has no reason to carry, and the entire reason this payload
    // exists: measured against the installed CLI (2.1.260), the CLI's own embedded hook
    // documentation marks `tool_response` a `PostToolUse`-only field. It is what turns a barrier
    // that only judges INTENT into a ledger that also knows the RESULT.
    #[serde(default)]
    pub tool_response: Value,
    // The CLI's `hook_event_name`, echoed back so a success (`PostToolUse`) and a failure
    // (`PostToolUseFailure`) are told apart once they reach `shadow::record_outcome`, which stores
    // it verbatim in `outcome_event`. No `#[serde(default)]`: every real invocation of this hook
    // carries it, and a payload that does not is exactly the malformed input this route should
    // refuse to guess about.
    pub event: String,
}

#[derive(Serialize, Deserialize)]
pub struct Decision {
    // "allow" | "deny" | "pending_approval" (spec §3.3). To the CLI, both "deny" and
    // "pending_approval" are just "block" (the hook script maps them, Step 6); the core treats them
    // differently — see below.
    pub decision: String,
    pub reason: String,
}

/// Spec B: what the hook already read about the run, as the resolver's request needs it. A struct
/// and not positional arguments: three of them are `i64`-ish and in a row, and swapped at the call
/// they would compile and record the wrong lineage.
pub(super) struct RunSnapshot<'a> {
    pub(super) run_id: i64,
    pub(super) lineage_root: i64,
    pub(super) shadow_decision_id: Option<i64>,
    pub(super) project_id: Option<&'a str>,
    pub(super) cwd: &'a str,
}

/// Spec B: the block this call is, as the resolver is asked about it.
pub(super) fn resolve_asked(
    state: &AppState,
    event: crate::judge::resolve::Event,
    run: &RunSnapshot<'_>,
    payload: &PreToolUsePayload,
    classification: &classifier::Classification,
) -> crate::judge::resolve::Asked {
    crate::judge::resolve::Asked {
        run_id: run.run_id,
        lineage_root_id: run.lineage_root,
        event,
        event_ref: run.shadow_decision_id,
        project_id: run.project_id.map(str::to_owned),
        machine_root: state.machine_config_root.clone(),
        subject: crate::judge::resolve::Subject::Call {
            tool_name: payload.tool_name.clone(),
            tool_input: payload.tool_input.clone(),
            cwd: run.cwd.to_owned(),
            action_class: classification.action_class,
        },
    }
}

/// A refusal that never becomes an approval, however its caller fails.
pub(super) fn deny_with(reason: &str) -> Decision {
    Decision {
        decision: "deny".to_owned(),
        reason: reason.to_owned(),
    }
}

/// A run's project shell rules as they reached the decision — the case where they could not be read
/// included.
///
/// That second case is why this is a type and not a bare `ShellRules`. `classify` takes its rules by
/// reference and has no vocabulary for "I could not read them": handed an empty pair it answers
/// exactly as it would for a project that declared nothing, and for a project that declared a
/// `deny` that is the wrong answer in the one direction that costs something.
pub(super) enum ProjectRules {
    /// What the project declared — and an empty pair when there was no project to ask, or when the
    /// call is one no rule could reach. Both of those are the whole truth rather than a stand-in.
    Declared(crate::project::policy::ShellRules),
    /// The table would not answer. Carries NO lists, because a state that held both "unreadable"
    /// and a set of rules would be a state nothing can act on — and the struct this replaces could
    /// represent it.
    Unreadable,
}

/// The empty pair `declared()` hands back when there is nothing to hand back.
///
/// A `static` because that method returns a reference; `Vec::new()` is a `const fn`, so this costs
/// no allocation and no initialisation.
///
/// **And a `const` will not do, which is the half that is easy to try and undo.** A `const` is
/// inlined as a value at each use site, so `&NO_SHELL_RULES` borrows a temporary; that temporary
/// lives for `'static` only if rvalue static promotion applies, and promotion refuses any type with
/// drop glue. `ShellRules` holds three `Vec`s, so it has drop glue, so the borrow is a local and
/// `declared()` stops compiling with `E0515: cannot return value referencing temporary value`. The
/// `static` has one address with the program's lifetime and nothing to promote.
///
/// It is also why `ShellRules::deny_writes` is a `Vec` of pairs and not a map keyed by tool: a
/// `HashMap::new()` is not a `const fn`, so a shape chosen for the read side would have taken this
/// `static` away and the `E0515` above with it.
pub(super) static NO_SHELL_RULES: crate::project::policy::ShellRules = crate::project::policy::ShellRules {
    allow: Vec::new(),
    deny: Vec::new(),
    deny_writes: Vec::new(),
};

impl ProjectRules {
    /// Nothing declared and nothing failed: a run with no project, or a tool call whose verdict the
    /// rules could not change anyway. Distinct from `Unreadable` in exactly the way that matters —
    /// there is nothing here that needs repairing afterwards.
    pub(super) fn none() -> Self {
        Self::Declared(crate::project::policy::ShellRules::default())
    }

    pub(super) fn declared(&self) -> &crate::project::policy::ShellRules {
        match self {
            Self::Declared(rules) => rules,
            // Classified under an empty pair and repaired afterwards: `downgrade_if_unreadable` is
            // the other half of this answer, and neither half is right on its own.
            Self::Unreadable => &NO_SHELL_RULES,
        }
    }

    /// Whether the project's refusals are actually in hand.
    ///
    /// Read by the grant lookup as well as by the downgrade, which is why it is a question and not
    /// a private field test: an authorization taken out earlier is not a key to a refusal nobody
    /// can read.
    pub(super) fn were_read(&self) -> bool {
        matches!(self, Self::Declared(_))
    }

    /// Repairs the one answer an empty stand-in could have got wrong.
    ///
    /// An unreadable list is an unreadable `deny`, and a refusal nobody can read must not be spent
    /// as a free pass — so a command that came back `allow` costs an approval prompt instead. The
    /// direction is `shell_rules`' own: it returns a `Result` precisely so that this cannot be read
    /// as "the project denied nothing".
    ///
    /// **Only the `allow` is touched, and that is what stops this doing harm of its own.** A `deny`
    /// downgraded to `pending_approval` would turn a refusal into something a person can approve,
    /// which is the very defect that put a project's `deny` ahead of the approval list one layer
    /// down. An existing `pending_approval` is already the answer this gives.
    ///
    /// **The class is left exactly as the classifier named it, and that is only safe because the
    /// grant lookup is gated on `were_read`.** A class is a fact about the COMMAND — `read-local`,
    /// `vcs-local` — where this is a fact about a database read, so a class invented here would put
    /// a row on the scoreboard naming an error rather than an action. But preserving the real class
    /// does not by itself avoid the problem it avoids for an invented one; it only narrows it from
    /// "everything" to "everything of that class", and `read-local` is the widest class there is.
    ///
    /// This method is the first thing in the codebase to emit a `pending_approval` carrying an
    /// allow-only class, which is what puts `read-local` and its siblings within reach of the grant
    /// table at all: every other producer of those classes answers `allow`, and the grant check
    /// only runs for a `pending_approval`. Measured before the gate went in — an outage, one prior
    /// approval of class `read-local`, and `ls -la` came back
    /// `("allow", "approved authorization for the read-local action class")` under a project that
    /// denies `ls`. The gate at the grant lookup is what makes the paragraph above true rather than
    /// nearly true; the two belong together and neither is correct alone.
    pub(super) fn downgrade_if_unreadable(
        &self,
        classification: classifier::Classification,
    ) -> classifier::Classification {
        if self.were_read() || classification.decision.decision != "allow" {
            return classification;
        }
        let reason = "this project's shell rules could not be read, and a refusal nobody can read \
                      is not a permission"
            .to_owned();
        classifier::Classification {
            decision: Decision {
                decision: "pending_approval".to_owned(),
                reason: reason.clone(),
            },
            action_class: classification.action_class,
            reason,
        }
    }
}

/// The project's two shell lists, read at decision time and never cached: a prefix declared a minute
/// ago has to bind the command attempted now, and a cached refusal is one that goes on being lifted
/// for as long as the cache lives.
///
/// **No project is not a failed read.** A run whose row names no project has no project `deny` that
/// could be lost, so an empty pair is the whole truth there rather than a stand-in for something
/// missing — which is why only the `Err` arm answers `Unreadable`.
///
/// **Asked only about a call whose verdict the rules could reach**, which is `classifier` question
/// `reads_shell_rules` and the caller's job rather than this function's. Not for the cost of the
/// read — it is an index scan of one project's prefixes, nothing like the git subprocesses
/// `session_git_decision` warns about — but for the cost of its FAILURE: `Unreadable` turns every
/// `allow` into an approval prompt, and a transient `SQLITE_BUSY` that parked a run on its next
/// `Read` would be this chunk's own autonomy loss arriving through a different door.
pub(super) async fn shell_rules_of(state: &AppState, project_id: Option<&str>) -> ProjectRules {
    let Some(project_id) = project_id else {
        return ProjectRules::none();
    };
    match crate::project::policy::shell_rules(&state.pool, project_id).await {
        Ok(declared) => ProjectRules::Declared(declared),
        Err(error) => {
            tracing::warn!(
                project_id,
                %error,
                "pretooluse-decision: could not read this project's shell rules — an allow costs an approval instead"
            );
            ProjectRules::Unreadable
        }
    }
}

/// The GitHub policy this decision is taken under: the machine default from `~/.nucleos/github.yaml` with
/// the project's declared operations laid over it, read at decision time and never cached.
///
/// The sentence `shell_rules_of` makes, about the other table, and §4.4 of
/// `.ai/specs/2026-09-03-alcada-por-projecto-design.md` is where it was decided for both. The run's
/// project comes off the row that already named it — one more column on a query that already ran —
/// and never off `vcs::project_for_worktree`, which spawns a `git rev-parse` in front of a person's
/// keystrokes.
///
/// **A failed read costs autonomy and can cost nothing else, which is why the SWALLOWING reader is
/// the right one here and there is no `ProjectRules::Unreadable` shape to go with it.**
/// `project_policy::github_ops` turns an unreadable table into an empty `Vec`; an empty overlay is
/// the machine default, because `for_project` adds rather than replaces. So the worst a database
/// hiccup can do is withhold operations the project declared — fewer things running unasked, never
/// more, and nothing that was written down is lost in a direction that grants. The shell rules run
/// the other way: an unreadable list is an unreadable `deny`, and losing THAT would be an allow, so
/// that side needs the `Result` reader and the downgrade. `project_policy` carries both halves for
/// exactly this reason and this is the consumer the swallowing one was written for.
///
/// Borrowed when there is nothing to lay over. A run with no project has no rows, so the machine
/// default is the whole answer and not a stand-in for one.
pub(super) async fn github_policy_of<'a>(
    state: &'a AppState,
    project_id: Option<&str>,
) -> std::borrow::Cow<'a, crate::integrations::github::Policy> {
    match project_id {
        Some(project_id) => std::borrow::Cow::Owned(
            state
                .github
                .policy_for_project(&state.pool, project_id)
                .await,
        ),
        None => std::borrow::Cow::Borrowed(&state.github.policy),
    }
}

/// What `core/hooks/ask_daemon.py` gives the daemon for the whole call (its `urlopen(..., timeout=5)`
/// on `/hooks/pretooluse-decision`); past it the hook refuses, "failing closed", and whatever the
/// daemon decided afterwards is never delivered. Keep in step with that file.
pub(super) const HOOK_BUDGET: Duration = Duration::from_secs(5);

/// What still has to happen after the judge returns: pausing for approval, recording, answering.
pub(super) const AFTER_JUDGE_MARGIN: Duration = Duration::from_secs(1);

/// Under this much remaining budget the judge is not asked to rule at all: it only observes
/// (detached) and the classifier decides. A wait that short buys no answer worth the risk.
pub(super) const JUDGE_FLOOR: Duration = Duration::from_millis(500);

/// How long the judge may be waited for, given when the hook's request began: `JUDGE_DEADLINE` at
/// most, less whatever the git probes before it already spent, and `None` below `JUDGE_FLOOR`.
pub(super) fn judge_wait(started: Instant) -> Option<Duration> {
    let remaining = HOOK_BUDGET
        .saturating_sub(started.elapsed())
        .saturating_sub(AFTER_JUDGE_MARGIN);
    (remaining >= JUDGE_FLOOR).then(|| remaining.min(crate::judge::JUDGE_DEADLINE))
}
