use axum::extract::State;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

use crate::auth::Scope;
use crate::classifier;
use crate::runs::finalize_termination;
use crate::shadow;
use crate::state::AppState;

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

#[derive(Serialize, Deserialize)]
pub struct Decision {
    // "allow" | "deny" | "pending_approval" (spec §3.3). To the CLI, both "deny" and
    // "pending_approval" are just "block" (the hook script maps them, Step 6); the core treats them
    // differently — see below.
    pub decision: String,
    pub reason: String,
}

#[derive(Deserialize)]
pub struct SessionGitPayload {
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Value,
    /// Where the session is standing. Not necessarily a repository root — see `git_exec::toplevel`.
    pub cwd: String,
}

/// The decision for a session nobody launched: a person's own editor, in a worktree, with no run
/// behind it.
///
/// **This exists because the pillar was governing the wrong half of its own purpose.** The queue is
/// there to order git operations *between sessions*, and the sessions doing the most work are the
/// ones a person opens by hand. Those carry no `NUCLEOS_RUN_ID`, so `ask_daemon.py` expressed no
/// opinion and git ran directly — measured, not theorised: an editor session was asked to
/// `git merge master` and it merged, with no hook, no proposal and no row. Every guarantee the queue
/// offers is a guarantee about requests that reach the queue.
///
/// **It answers with `deny` or with nothing, never with `allow`**, and that is what makes it safe to
/// add where the old code chose silence. The comment it replaces was right about its own case:
/// registered repo-wide, an `allow` emitted by a daemon that does not know what the person is doing
/// would auto-approve their tools. A refusal grants nothing.
///
/// **Only what the queue can actually perform is refused** — the same six parsers that decide it for
/// runs, not the classifier's much wider `pending_approval` net. That distinction is the difference
/// between a gate and a wall: the classifier sends everything not provably read-only for approval, so
/// refusing on its verdict would stop a session at its second command. And a spelling the queue
/// declines (`git merge --squash`, `git branch -D`) must keep working directly, or it becomes
/// impossible rather than governed.
///
/// The refusal is not a redirect: the operation is admitted here, in the daemon, and the session is
/// told the ticket. The alternative was to answer "ask the queue yourself", which would have meant
/// telling every editor session how to obtain the control token — handing out the master key to
/// avoid one round trip.
pub async fn session_git_decision(
    State(state): State<AppState>,
    Json(payload): Json<SessionGitPayload>,
) -> Json<Decision> {
    let no_opinion = || {
        Json(Decision {
            decision: "allow".to_owned(),
            reason: "not an operation this queue performs".to_owned(),
        })
    };

    if !matches!(payload.tool_name.as_str(), "Bash" | "PowerShell") {
        return no_opinion();
    }
    let Some(command) = payload.tool_input.get("command").and_then(Value::as_str) else {
        return no_opinion();
    };

    // One deadline for the whole decision. A hook runs in front of every tool call, so this path
    // spends git subprocesses on a person's keystrokes — which is why the caller filters first and
    // only asks about commands that could possibly be queueable.
    let deadline = std::time::Instant::now() + crate::git_exec::OPERATION_TIMEOUT;
    let Ok(root) = crate::git_exec::toplevel(Path::new(&payload.cwd), deadline).await else {
        // Standing outside a working tree, so no git command from here reaches a repository this
        // queue serves. Silence rather than refusal: this hook is registered for one repository and
        // a session that has wandered out of it is not the case being governed.
        return no_opinion();
    };
    let Ok(branch) = crate::git_exec::current_branch(&root, deadline).await else {
        return no_opinion();
    };

    // Per SEGMENT, not per command. Every parser below matches its whole token list as an exact
    // shape, so a shell operator in front of the git call made the list longer and the match fail —
    // and a failed match here is an ALLOW. `cd repo && git merge master` was permitted by the route
    // whose entire purpose is to refuse it, measured against the running daemon. The strictness of
    // the parsers is not what was wrong and is not touched; they are simply asked about each command
    // in the line rather than about the line.
    let Some(op) = crate::vcs::shell_segments(command)
        .into_iter()
        .find_map(|segment| {
            crate::vcs::merge_from_command(segment, &branch)
                .or_else(|| crate::vcs::push_from_command(segment, &branch))
                .or_else(|| crate::vcs::tag_from_command(segment, &branch))
                .or_else(|| crate::vcs::fetch_from_command(segment))
                .or_else(|| crate::vcs::branch_delete_from_command(segment))
                .or_else(|| crate::vcs::rebase_from_command(segment, &branch))
        })
    else {
        // Declined by the queue is not the same sentence as fine to run by hand, and reading them
        // as one left `git push --force` passing. A spelling that still writes something other
        // sessions share is refused with NOTHING queued — there is nothing to queue, because the
        // queue cannot perform that spelling either. Per segment, for the reason above.
        return match crate::vcs::shell_segments(command)
            .into_iter()
            .find_map(crate::vcs::unqueueable_but_shared)
        {
            Some(reason) => Json(deny_with(&reason)),
            None => no_opinion(),
        };
    };

    // From here the command IS one the queue performs, so every remaining failure refuses rather
    // than falls through. The direction is deliberately the opposite of `runs::queueable_operation`,
    // and the two are right for opposite reasons: there, a person has already approved an action and
    // being unable to queue it must not strand them holding it; here, nobody has approved anything,
    // and falling through would hand back the very bypass this function closes.
    let project_id = match crate::vcs::project_for_worktree(&state.pool, &root, deadline).await {
        Ok(project_id) => project_id,
        Err(reason) => {
            return Json(deny_with(&format!(
                "{} goes through the queue, and {reason}",
                op.kind()
            )));
        }
    };
    let repo = match crate::vcs::resolve_repo(&state.pool, &project_id).await {
        Ok(repo) => repo,
        Err(error) => {
            tracing::warn!(
                project_id,
                ?error,
                "session-git: could not resolve the repository"
            );
            return Json(deny_with(&format!(
                "{} goes through the queue, and {project_id}'s repository could not be resolved",
                op.kind()
            )));
        }
    };

    match crate::vcs::submit(&state.pool, &repo, &op, crate::vcs::Origin::Shell).await {
        Ok(id) => Json(Decision {
            decision: "deny".to_owned(),
            reason: format!(
                "queued as vcs request #{id} ({}) — this repository takes one git operation at a \
                 time, across every session in every worktree, so it is performed by the queue \
                 rather than here. Do not run it again and do not run it another way: watch \
                 GET /vcs/requests/{id}/wait, and expect your worktree to be moved under you when \
                 it lands.",
                op.kind()
            ),
        }),
        Err(error) => {
            tracing::error!(?error, "session-git: could not admit the request");
            Json(deny_with(&format!(
                "{} goes through the queue, and admitting it failed",
                op.kind()
            )))
        }
    }
}

/// A refusal that never becomes an approval, however its caller fails.
fn deny_with(reason: &str) -> Decision {
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
enum ProjectRules {
    /// What the project declared — and an empty pair when there was no project to ask, or when the
    /// call is one no rule could reach. Both of those are the whole truth rather than a stand-in.
    Declared(crate::project_policy::ShellRules),
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
/// drop glue. `ShellRules` holds two `Vec`s, so it has drop glue, so the borrow is a local and
/// `declared()` stops compiling with `E0515: cannot return value referencing temporary value`. The
/// `static` has one address with the program's lifetime and nothing to promote.
static NO_SHELL_RULES: crate::project_policy::ShellRules = crate::project_policy::ShellRules {
    allow: Vec::new(),
    deny: Vec::new(),
};

impl ProjectRules {
    /// Nothing declared and nothing failed: a run with no project, or a tool call whose verdict the
    /// rules could not change anyway. Distinct from `Unreadable` in exactly the way that matters —
    /// there is nothing here that needs repairing afterwards.
    fn none() -> Self {
        Self::Declared(crate::project_policy::ShellRules::default())
    }

    fn declared(&self) -> &crate::project_policy::ShellRules {
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
    fn were_read(&self) -> bool {
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
    fn downgrade_if_unreadable(
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
async fn shell_rules_of(state: &AppState, project_id: Option<&str>) -> ProjectRules {
    let Some(project_id) = project_id else {
        return ProjectRules::none();
    };
    match crate::project_policy::shell_rules(&state.pool, project_id).await {
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

/// The GitHub policy this decision is taken under: the machine default from `.ai/github.yaml` with
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
async fn github_policy_of<'a>(
    state: &'a AppState,
    project_id: Option<&str>,
) -> std::borrow::Cow<'a, crate::github::Policy> {
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

pub async fn pretooluse_decision(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(mut payload): Json<PreToolUsePayload>,
) -> Json<Decision> {
    // `run_id` arrives in the body, which makes it a claim the caller makes about itself, and every
    // branch below reads `mode` from it. A scoped key names its own run, and the daemon resolved
    // that name from its own state rather than from anything the caller chose — so the key decides
    // and the claim is dropped. A run cannot borrow another run's rules, or spend another run's
    // denial allowance to have it stopped, because it has no way to say which run it is.
    //
    // Dropped rather than compared-and-refused, which is what this was. The guarantee is the same
    // one, kept by construction instead of by inspection; the difference is that a stale claim is
    // now irrelevant instead of fatal. A CLI kept alive across turns is handed its environment once,
    // at spawn, and `ask_daemon.py` echoes `NUCLEOS_RUN_ID` out of it — so from the second turn on,
    // a comparison would refuse every tool call the conversation made.
    //
    // A key that names no run — the control token an orchestrator turn carries, which it can hold
    // because `ToolPolicy::McpOnly` leaves it no way to read its own environment — leaves the claim
    // as the only identifier there is. Hence the fallback, and hence no `deny` here: the claim is
    // still the truth for exactly the callers that cannot usefully lie about it.
    let run_id = match scope {
        Scope::Run(id) => id,
        _ => payload.run_id,
    };
    // Corrected IN the payload, and not merely beside it. Every branch below is handed `&payload`
    // and several read the id back out of it — `rooted_decision` checks the read-untrusted barrier
    // with it, `assistant_decision` both checks and SETS it — so leaving the claim in place would
    // fix the id in this function and leave it stale in the ones that decide with it.
    //
    // Which is the hole a kept process opens, and it is not the one the key closed: a CLI is handed
    // its environment once, so from the second turn on `ask_daemon.py` echoes the FIRST turn's id
    // for the rest of the conversation. A turn that read a stranger's text is marked on its own row;
    // a barrier reading the first turn's row finds nothing there, and the ordering rule holds on
    // turn one and is walked around on every turn after it.
    payload.run_id = run_id;

    // Validate run_id against runs actually in flight before trusting anything derived from it (spec
    // §3.4 — the hook's environment sits inside the same cooperative trust model as the token, so the
    // core never blindly trusts what the hook sends).
    let is_in_flight = state.run_handles.lock().unwrap().contains_key(&run_id);

    // `mode` is resolved for EVERY request, in flight or not, because it decides WHICH set of rules
    // applies — and a run that has left `run_handles` is exactly when defaulting to `real` is most
    // dangerous: the classifier permits `Read` there, so an email triage run would be handed the one
    // tool the pillar exists to keep away from a stranger's text. `cwd` stays behind the in-flight
    // check: it only feeds the classifier's path sensitivity, which is meaningful for a run that is
    // actually executing.
    //
    // `project_id` sits with `mode` and NOT with `cwd`, for the same sentence: a project's two shell
    // lists say what may run at all, not where a path may point, and a run that has left
    // `run_handles` is precisely when losing that project's `deny` would cost the most. One more
    // column on a query that already ran, so the reach costs nothing.
    //
    // `permission_mode` rides along for the same sentence, and it is the SNAPSHOT the turn started
    // with rather than what the conversation's selector says now: moving the menu while a turn is
    // running must not change the rules underneath it. NULL means no CLI turn wrote one — every
    // autopilot run, and every turn the local brain answered — and reads as `Auto`, which is what a
    // rooted conversation did before the column existed. One more column on a query that already
    // ran, so the reach costs nothing.
    let (cwd, mode, project_id, permission) =
        match sqlx::query_as::<_, (Option<String>, String, Option<String>, Option<String>)>(
            "SELECT cwd, mode, project_id, permission_mode FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await
        {
            Ok(Some((cwd, mode, project_id, permission))) => (
                is_in_flight.then_some(cwd).flatten(),
                mode,
                project_id,
                permission.as_deref().map_or(
                    crate::chats::PermissionMode::Auto,
                    crate::chats::PermissionMode::from_wire,
                ),
            ),
            Ok(None) => (
                None,
                "real".to_owned(),
                None,
                crate::chats::PermissionMode::Auto,
            ),
            // `mode` decides WHICH set of rules applies, so an unreadable one cannot resolve to the
            // most permissive of them. `Ok(None)` above can safely default to `real` because the row is
            // genuinely absent — there is no run whose rules we are guessing at. An `Err` is different:
            // the run may well be a triage or shadow run whose barrier we would be stepping over, and
            // the pool this reads through is shared with feed appends and run-status writes, so
            // SQLITE_BUSY under contention is an ordinary event rather than a theoretical one.
            Err(error) => {
                tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: failed to resolve the run's mode — failing closed"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not resolve the run's mode — failing closed".to_owned(),
                });
            }
        };

    // Barrier 2 of spec §5.5. A triage run is launched with no tools at all (barrier 1), so a tool
    // call arriving here means barrier 1 is not in force — which is the entire reason this branch
    // exists. There is no allowlist and no read-only exception: the run's whole job is to read text
    // a stranger wrote and answer with a verdict, and every tool is a way for that text to act.
    //
    // It returns before the classifier, so it never terminates the run and never mints a proposal.
    if mode == crate::email::TRIAGE_MODE {
        // Only a run that is actually executing can have reached a tool through barrier 1, and that
        // is the alarming case. The startup verification deliberately probes this branch with a
        // throwaway row that is NOT in flight, so warning on both would fire a false alarm on every
        // single boot — and an alarm that cries wolf at startup is one nobody reads when it matters.
        if is_in_flight {
            tracing::warn!(
                run_id = run_id,
                tool = %payload.tool_name,
                "pretooluse-decision: a triage run attempted a tool — barrier 1 is not in force"
            );
        } else {
            tracing::debug!(
                run_id = run_id,
                tool = %payload.tool_name,
                "pretooluse-decision: denied a tool for a triage run that is not in flight"
            );
        }
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "email triage runs have no tools".to_owned(),
        });
    }

    // Orchestrator (assistant) turns are constrained to the NucleOS MCP tools by their tool policy
    // (`ToolPolicy::McpOnly`) and delegate all real work to governed runs, so they must NOT go
    // through the autopilot classifier — doing so would terminate the turn and mint action-approval proposals it
    // can never satisfy (a resume expects a worktree run). Allow the sanctioned MCP tools, block
    // everything else, and never create a proposal or terminate the turn.
    if mode == "assistant" {
        // ...unless the turn is ROOTED: one continuing a session had in the IDE, spoken to from the
        // machine, in a directory that registers this hook. That turn was launched
        // `ToolPolicy::Unrestricted` precisely so it can touch the code the conversation is about,
        // and the branch below would deny every one of those calls — leaving it holding tools it can
        // never use, which is worse than not having them.
        if let Some(root) = rooted_turn(&state, run_id).await {
            return rooted_decision(&state, &payload, &root, project_id.as_deref(), permission)
                .await;
        }
        return assistant_decision(&state, &payload).await;
    }

    // A council seat reads in order to answer a question, and does nothing else. Same shape as the
    // branch above and a strictly narrower list: `mcp_tools::COUNCIL_TOOLS` carries no `Acts` at
    // all, so there is no ordering rule to apply and nothing a seat can do that the owner would
    // have to undo.
    //
    // Like the orchestrator's, this returns BEFORE the classifier — a council run has no worktree
    // and no proposal to resume into, so a `pending_approval` here would terminate the seat and
    // mint an approval nothing could ever satisfy.
    if mode == crate::council::COUNCIL_MODE {
        return council_decision(&state, &payload).await;
    }

    // A department is in the same position as a seat, and for the question of WHICH tools this
    // branch is the SECOND layer rather than the first. `team.rs` launches with `cwd: None`, so no
    // `.claude/settings.json` of the owner's resolves and this hook may never fire at all — which
    // is why `auth::TEAM_ROUTES` is the barrier that has to hold alone there, and does.
    //
    // For the other question — having read, may it still act — this branch is not a second layer at
    // all. It is the ONLY one on the cloud path. `auth::permits` is a pure function over
    // `(Scope, Method, path)` and cannot know what a turn has read, and `permitted_after_untrusted`
    // has exactly one caller in this codebase (`local_agent.rs`), which is the Ollama path.
    //
    // The two sentences that used to stand here said `TEAM_TOOLS` carried no `Acts` and that there
    // was nothing for the ordering rule to bite on. Both were true when written and stopped being
    // true when the alçada landed: `propose_action` and `propose_teammate` are `Acts` today, and
    // `propose_action`'s own grading exists precisely so this door shuts.
    if mode == crate::team::TEAM_MODE {
        return team_decision(&state, &payload).await;
    }

    // The run's own project, off the row that already named it — and asked for only when the answer
    // could matter. `classifier::reads_shell_rules` is the same list the classifier's own branch
    // uses, so the filter cannot drift away from what it filters; see `shell_rules_of` for why a
    // read nobody would consult is still worth not taking.
    //
    // The GitHub policy comes from the same place under the same gate, and the gate fits it exactly:
    // `classify` consults `policy` only in `classify_segment`, which is reached only for the two
    // shell tools `reads_shell_rules` names. A `Read` cannot be a `gh` line.
    let (rules, policy) = if classifier::reads_shell_rules(&payload.tool_name) {
        (
            shell_rules_of(&state, project_id.as_deref()).await,
            github_policy_of(&state, project_id.as_deref()).await,
        )
    } else {
        (
            ProjectRules::none(),
            std::borrow::Cow::Borrowed(&state.github.policy),
        )
    };
    let classification = rules.downgrade_if_unreadable(classifier::classify(
        &payload.tool_name,
        &payload.tool_input,
        cwd.as_deref().map(Path::new),
        policy.as_ref(),
        rules.declared(),
        unrecognized_policy_for(&state.pool, run_id, &mode).await,
    ));
    tracing::info!(
        tool_name = %payload.tool_name,
        decision = %classification.decision.decision,
        action_class = classification.action_class,
        reason = %classification.reason,
        "pretooluse-decision: classified action"
    );

    if mode == "shadow" {
        // Gated on the run being in flight, which is what the mode lookup above used to guarantee
        // implicitly. A scoreboard is a record of decisions taken over live runs; a stray call
        // naming a finished run is not one of those.
        //
        // The digest is the EFFECTIVE policy's — the project's, where there is one — because the row
        // records a decision and this is the policy the decision was taken under. Recording the
        // machine's label beside a verdict a project's declaration produced would put the wrong
        // configuration on the evidence, which is the one thing this column exists to prevent.
        if is_in_flight
            && let Err(error) = shadow::record_decision(
                &state.pool,
                run_id,
                &payload.tool_name,
                &payload.tool_input,
                &classification,
                policy.digest(),
            )
            .await
        {
            tracing::warn!(
                run_id = run_id,
                %error,
                "pretooluse-decision: failed to record shadow decision"
            );
        }

        let read_only = matches!(payload.tool_name.as_str(), "Read" | "Grep" | "Glob")
            || (payload.tool_name == "Bash" && classification.action_class == "read-local");
        return if read_only {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "shadow mode permits this read-only tool".to_owned(),
            })
        } else {
            Json(Decision {
                decision: "deny".to_owned(),
                reason: "shadow mode blocks tools that are not read-only".to_owned(),
            })
        };
    }

    if mode == "worktree"
        && is_in_flight
        && let Err(error) = shadow::record_decision(
            &state.pool,
            run_id,
            &payload.tool_name,
            &payload.tool_input,
            &classification,
            policy.digest(),
        )
        .await
    {
        tracing::warn!(
            run_id = run_id,
            %error,
            "pretooluse-decision: failed to record shadow decision"
        );
    }

    // Class-scoped authorization (spec §8.4 step 6): once a human has approved an action, every
    // later action of that CLASS is allowed for the rest of the resume run, overriding the
    // pending_approval. Only a pending_approval is ever lifted — a `deny` (destructive) never
    // reaches this check, so a grant can never launder a denied action.
    //
    // The class is the key because neither of the alternatives is a boundary a human would
    // recognise: `tool_name` is "Bash" for every shell action, so an approved `git push` authorized
    // whatever this run tried next, while the exact input put the identical question a second time
    // for the next push. The class is what the human actually agreed to.
    if classification.decision.decision == "pending_approval" && is_in_flight {
        // Already taken over by the queue (migration 0054). Answered BEFORE the grant lookup and
        // before the pause below, because both would be wrong here: there is no grant to consume —
        // the approval deliberately minted none — and pausing would fetch a person to approve a
        // merge that is already queued, whose approval would queue it a second time.
        //
        // A `deny` rather than a pause, because the run is being told where its work went, not asked
        // to wait. Counted against the ordinary prober allowance, and that is deliberate: a run told
        // in its resume prompt and again in this reason that the merge is queued and must not be
        // retried, which does it three times regardless, is not obeying. One that reads either
        // message spends none of it.
        match crate::proposals::matching_queued_request(
            &state.pool,
            run_id,
            &payload.tool_name,
            &payload.tool_input.to_string(),
        )
        .await
        {
            Ok(Some(request_id)) => {
                tracing::info!(
                    run_id = run_id,
                    request_id,
                    "pretooluse-decision: the action is already queued — refusing the retry"
                );
                count_denial_and_stop_a_prober(&state, run_id, &payload.tool_name).await;
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: format!(
                        "this action was handed to the daemon's git queue as request {request_id} \
                         when it was approved, and will be carried out there — do not attempt it \
                         again"
                    ),
                });
            }
            Ok(None) => {}
            // Falling through to the grant lookup is the safe direction: the worst that follows is
            // a pause and a question for a person, which is what happened before any of this.
            Err(error) => {
                tracing::warn!(
                    run_id = run_id,
                    %error,
                    "pretooluse-decision: could not tell whether this action is already queued"
                );
            }
        }

        // **Not while the project's refusals are unreadable.** A grant is an authorization derived
        // from a decision about a command, and it outlives that decision by the whole length of the
        // resume run; while the list that decided it cannot be read, there is no way to know the
        // decision still stands. Without this gate `downgrade_if_unreadable` is defeated for the
        // rest of the run by one earlier approval: it hands the grant lookup a `pending_approval`
        // carrying an allow-only class, and `read-local` covers `ls`, `cat`, `git status` and
        // `cargo test` between them. Measured — an outage, one grant of class `read-local`, and
        // `ls -la` came back `allow` under a project that denies `ls`.
        //
        // `matching_queued_request` above is deliberately NOT gated. It answers a run that is
        // retrying something the queue already took over, which is a fact about a request that
        // exists and is true whatever the project's list says — and its answer is a refusal.
        if rules.were_read() {
            match crate::proposals::grant_covers_class(
                &state.pool,
                run_id,
                classification.action_class,
            )
            .await
            {
                Ok(true) => {
                    tracing::info!(
                        run_id = run_id,
                        tool = %payload.tool_name,
                        action_class = classification.action_class,
                        "pretooluse-decision: a grant covers this action class — authorizing the action"
                    );
                    let _ = crate::feed::append(
                        &state.pool,
                        None,
                        "action_authorized",
                        &format!(
                            "authorized approved {} action for run {}",
                            payload.tool_name, run_id
                        ),
                        Some(run_id),
                    )
                    .await;
                    return Json(Decision {
                        decision: "allow".to_owned(),
                        reason: format!(
                            "approved authorization for the {} action class",
                            classification.action_class
                        ),
                    });
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        run_id = run_id,
                        %error,
                        "pretooluse-decision: grant lookup failed; falling back to the classifier decision"
                    );
                }
            }
        }
    }

    // A denied action is answered and then counted. Answering was all this used to do, which left
    // the run free to try the next spelling immediately — and against an unbounded number of
    // attempts a lexical classifier is a puzzle with a solution rather than a boundary. The two
    // verdicts were also the wrong way round in cost: `pending_approval` stopped the run and
    // fetched a human, while `deny` — the harsher judgement — cost the run nothing at all.
    if classification.decision.decision == "deny" && is_in_flight {
        count_denial_and_stop_a_prober(&state, run_id, &payload.tool_name).await;
    }

    // **An unrecognized tool is refused, not parked, and the two are not the same verdict.**
    //
    // Parking exists to put a decision in front of a person: writes outside the workspace, changes
    // to the files that ARE the policy, a git operation with consequences. Those keep parking, and
    // must — the whole design is that nobody but the owner authorises them.
    //
    // `unrecognized-tool` is not that, and the hyphen is load-bearing. Until 2026-08-28 both this
    // and the shell path's unrecognized COMMANDS shared one label, and keying on it here refused
    // `git branch -D`, `cargo fix` and `gh run list` -- real decisions -- while meaning to refuse
    // only tool names. Two job-node tests caught it, and the classifier now names the two apart.
    //
    // A tool nobody has reasoned about is a capability nobody has bounded, which is a gap in
    // `classifier.rs` rather than a question about the work. Parked, it stopped an autonomous run
    // dead and minted a proposal saying, in effect,
    // "somebody please decide about WebSearch" — a question no owner asleep at 4am was going to
    // answer, and one that has the same answer every time. Measured 2026-08-27: two overnight
    // attempts died exactly here, hours of work each, having asked for a tool once.
    //
    // Refused, the run is told no and carries on with the tools it has, which is what a person
    // would have replied. **Deliberately not counted by `count_denial_and_stop_a_prober`**: that
    // counter is the guard against searching a grammar for a destructive command that gets through,
    // and reaching for an absent tool is not that search. `runner::DEFAULT_MAX_TURNS` is what bounds
    // a run that will not take no for an answer.
    //
    // Scoped to the unattended modes by the one definition of that word (`runs::runs_unattended`),
    // and not applied more widely, because the premise is literally "nobody is awake". A `real`-mode
    // run has its owner at the window, and for them the pause is exactly right: they can approve it
    // in ten seconds.
    if classification.decision.decision == "pending_approval"
        && classification.action_class == "unrecognized-tool"
        && crate::runs::runs_unattended(&mode)
    {
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: format!(
                "{} is not available to an autonomous run. Nobody is awake to approve it, so this                  is a refusal and not a pause: do not retry it, and do the work with the tools you                  have.",
                payload.tool_name
            ),
        });
    }

    // **A park that cannot be answered is a refusal that also destroys an item, so it is spelled as
    // a refusal.** See `a_park_here_would_only_destroy` for the whole argument; the short of it is
    // that a job node's park never becomes an `action-approval` — it becomes a `skipped-item` — so
    // the command is refused either way and the only question is whether the item's work survives
    // the asking. Job 22 on 2026-08-30 is the measurement: it asked about one path outside its
    // worktree, was correctly refused, and lost the whole item plus everything it had already
    // written for it.
    //
    // Placed AFTER the two `deny` branches above and before the pause below, so it can only ever
    // convert a `pending_approval`, never soften a `deny`.
    if classification.decision.decision == "pending_approval"
        && is_in_flight
        && a_park_here_would_only_destroy(&state.pool, run_id, &mode).await
    {
        tracing::info!(
            run_id,
            tool_name = %payload.tool_name,
            action_class = classification.action_class,
            "pretooluse-decision: refused rather than parked — a park here would only end the item"
        );
        record_refused_action(
            &state,
            &payload,
            &payload.tool_name,
            None,
            &classification.reason,
        )
        .await;
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: format!("{} {A_PARK_HERE_WOULD_END_THE_ITEM}", classification.reason),
        });
    }

    if classification.decision.decision == "pending_approval" {
        // Only for a genuinely in-flight run_id — an unknown/stale one must not terminate anything.
        if is_in_flight {
            // In its own task, on purpose. Terminating the run kills the CLI whose hook script owns
            // the connection this handler is answering, and that script gives up after 5s anyway
            // (`ask_daemon.py`'s `timeout=5`) — so the request can disappear mid-handler, and a
            // dropped request drops the handler future exactly the way `abort()` does. Awaiting the
            // JoinHandle keeps the response as synchronous as before; dropping a JoinHandle only
            // detaches its task, so the pause still gets recorded when the request goes away.
            let _ = tokio::spawn(pause_for_approval(
                state.clone(),
                run_id,
                payload.tool_name.clone(),
                payload.tool_input.to_string(),
                classification.reason.clone(),
            ))
            .await;
        } else {
            tracing::warn!(
                "pretooluse-decision: pending_approval for unknown/finished run_id {} — not terminating",
                run_id
            );
        }
    }

    Json(classification.decision)
}

/// The reason an orchestrator turn is refused a tool that would act. A constant because the tests
/// assert on it: every other refusal in this branch is also a `deny`, so only the reason tells
/// "the turn had read a stranger's words" apart from "the tool was not ours".
pub const UNTRUSTED_CONTEXT_DENY_REASON: &str =
    "this turn has read third-party content and can no longer act";

/// The reason an errand's turn is refused a tool that would act.
///
/// Its own constant beside `UNTRUSTED_CONTEXT_DENY_REASON` and not a reuse of it, because the two
/// say different things to the model and only one of them is true here. "You have read third-party
/// content" is a statement about this turn that a clean first message would find simply false, and
/// a model told something false about itself will try to work around it. This one is a standing
/// rule it cannot satisfy by behaving differently, which is what stops it trying.
pub const ERRAND_MAY_NOT_ACT: &str =
    "an errand does not act on its own; this was written down for a person to decide on";

/// The orchestrator turn's decision, and the only barrier standing between a mail body and the
/// daemon's controls.
///
/// Everything else in this file gets a second look from the classifier. This branch does not, by
/// design — a turn holds no worktree and cannot satisfy an approval — so an unconditional allow
/// here is genuinely unconditional. What made that dangerous is the tool set: `get_email` returns a
/// stranger's body verbatim into the same context that reaches `approve_proposal`, `set_kill` and
/// `create_run`, and the turn carries the control token, so a body asking for a proposal to be
/// approved was read by the one agent able to approve it.
///
/// The rule is an ordering rule rather than a policy on any single tool: read what you like, and
/// act while nothing third-party has entered the turn — but not both, and not in that order. It is
/// deliberately not a hard split of the tool set, because reading mail from a phone is the feature,
/// and the ordering costs the owner one extra message rather than the tool.
/// What an unrecognised command gets for THIS run: a person, or a chance to prove it is confined.
///
/// Both halves of `classifier::Unrecognized`'s conjunction are answered here, and neither is
/// guessed:
///
/// - **the owner asked for it** — `jobs.rule_name IS NULL`. The column has meant this since jobs
///   existed (`NewJob::rule_name`: "`None` for a job nobody scheduled"), and nothing had ever read
///   it to decide anything. A run with no `job_id` at all is not covered: a standalone `POST /runs`
///   IS requested work, but the daemon does not record who created a run — `runs.origin` exists and
///   is NULL on all 292 rows — so there is no signal to read, and inventing one from the absence of
///   a `job_id` would hand the same widening to every run a schedule starts.
/// - **nobody is awake** — `runs_unattended`, the one definition of that word, so this cannot drift
///   from the three policies that already read it.
///
/// Fails closed in every direction: an error, a missing row, an unknown run all give
/// `AsksAPerson`, which is what every caller got before this function existed.
async fn unrecognized_policy_for(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    mode: &str,
) -> crate::classifier::Unrecognized {
    if !crate::runs::runs_unattended(mode) {
        return crate::classifier::Unrecognized::AsksAPerson;
    }
    let asked_for: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM runs JOIN jobs ON jobs.id = runs.job_id
         WHERE runs.id = ? AND jobs.rule_name IS NULL",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    match asked_for {
        Some(_) => crate::classifier::Unrecognized::MayBeConfined,
        None => crate::classifier::Unrecognized::AsksAPerson,
    }
}

/// The sentence a node gets instead of being put down for asking.
pub const A_PARK_HERE_WOULD_END_THE_ITEM: &str = "this is unattended work you were asked to finish, so there is nobody to approve this and \
     nothing is waiting on an answer. It is refused, it has been written down for the owner to \
     read, and the item is still yours: do not retry it, do the rest of the work.";

/// Whether parking THIS run would destroy its item rather than ask anybody anything.
///
/// **The premise, and it is what makes this a refusal rather than a loosening: in a job, a park is
/// already a refusal.** `pause_for_approval` sends a node that owns an item — and every stage in
/// `NODES_THAT_GIVE_UP` — down `skip_the_item`, which mints a `skipped-item` record and never a
/// `action-approval`. Nothing is ever approved and nothing ever resumes. The command does not run
/// either way, so no command this daemon blocks today becomes runnable; the only thing that changes
/// is whether the item's work survives the asking.
///
/// Measured: 16 items across 7 jobs have been skipped, and 257 of the 278 refusals ever recorded
/// were `unrecognized` — a class that means the classifier had no opinion, not that anything was
/// dangerous.
///
/// The **plan** node is deliberately excluded, by being neither an item-owner nor in
/// `NODES_THAT_GIVE_UP`. Its park is the one that is a real question: it produces the queue, so
/// there is nothing partial to preserve, and `pause_for_approval` gives it a real
/// `action-approval` a person can answer and resume. Refusing there would throw away the one case
/// where asking works.
///
/// Requested work only, on the same signal `unrecognized_policy_for` reads, and for the same reason:
/// a job a schedule started was never agreed to, and the strict road is the right one for it.
///
/// Fails closed everywhere: any error, any missing row, anything not a job node answers `false` and
/// the run parks exactly as it did before this existed.
async fn a_park_here_would_only_destroy(pool: &sqlx::SqlitePool, run_id: i64, mode: &str) -> bool {
    if !crate::runs::runs_unattended(mode) {
        return false;
    }
    // Read-only, and it mirrors `put_the_item_down`'s WHERE clause plus the `NODES_THAT_GIVE_UP`
    // arm beside it. Two copies of one condition, which is a thing this file distrusts — so
    // `a_refusal_and_a_skip_agree_about_which_nodes_they_cover` pins them against each other.
    let answer: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM runs
         JOIN jobs ON jobs.id = runs.job_id
         WHERE runs.id = ?
           AND jobs.rule_name IS NULL
           AND (EXISTS (SELECT 1 FROM job_items
                        WHERE job_items.job_id = jobs.id
                          AND job_items.run_id = runs.id
                          AND job_items.status = 'running')
                OR runs.stage IN ('review', 'replan'))",
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    answer.is_some()
}

/// The conversation a run belongs to, or `None` for a run that is nobody's turn.
async fn chat_of_run(pool: &sqlx::SqlitePool, run_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>("SELECT chat_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()
}

/// The directory a turn is rooted in, or `None` when it is an ordinary orchestrator turn.
///
/// Read from the CHAT and not from the run, because it is a property of the conversation: every
/// turn of a continued conversation runs in the same place, and a run column would be a second copy
/// free to disagree with the one `assistant.rs` launches from.
async fn rooted_turn(state: &AppState, run_id: i64) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT c.cwd FROM runs r JOIN chats c ON c.chat_id = r.chat_id WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .flatten()
}

/// How long a tool call waits for somebody to answer for it.
///
/// Forty-five seconds, and the number is not a preference. The CLI holds the hook call open while
/// this waits and kills it at its own ceiling — sixty seconds unless the settings entry says
/// otherwise, and the entries already written into people's projects do not. Raising that would
/// help only the projects wired after the change and leave every existing one being cut off
/// mid-question, so the window is chosen to fit the ceiling that is actually out there.
///
/// Long enough for somebody looking at the window to read a command and decide; short enough that
/// stepping away costs one refused tool call rather than a conversation that hangs.
pub(crate) const ASK_WINDOW: std::time::Duration = std::time::Duration::from_secs(45);

/// One tool call a conversation is waiting to be allowed.
///
/// In memory and nowhere else, deliberately. A proposal is a durable row because the run it belongs
/// to is not being watched; this exists only while a hook call is blocked on it, and a daemon that
/// restarts has killed the turn that was asking. A question outliving the turn that asked it is a
/// question about nothing.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Ask {
    pub id: String,
    pub chat_id: String,
    pub run_id: i64,
    pub tool: String,
    /// The one argument that says what this is about, or `None` when none of them does.
    ///
    /// Deliberately not the whole input, for the reason `runner::ToolCall` gives about its own: a
    /// `Write` carries the file it is writing, and a window that printed that argument would print
    /// the file.
    pub detail: Option<String>,
}

struct Pending {
    ask: Ask,
    /// Handed to whoever answers, and taken when they do — so a second answer finds nothing rather
    /// than overwriting the first.
    answer: Option<tokio::sync::oneshot::Sender<bool>>,
    /// Handed to the hook call that waits, and taken for the same reason.
    heard: Option<tokio::sync::oneshot::Receiver<bool>>,
}

static ASKS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Pending>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Records that this turn is waiting to be allowed something, and returns the question's name.
pub(crate) fn ask_about(chat_id: &str, run_id: i64, tool: &str, detail: Option<String>) -> String {
    let id = crate::auth::generate_uuid_v4();
    let (answer, heard) = tokio::sync::oneshot::channel();
    ASKS.lock().unwrap().insert(
        id.clone(),
        Pending {
            ask: Ask {
                id: id.clone(),
                chat_id: chat_id.to_owned(),
                run_id,
                tool: tool.to_owned(),
                detail,
            },
            answer: Some(answer),
            heard: Some(heard),
        },
    );
    id
}

/// What this conversation is waiting to be allowed, which is nearly always nothing or one thing.
pub fn asks_for(chat_id: &str) -> Vec<Ask> {
    ASKS.lock()
        .unwrap()
        .values()
        .filter(|pending| pending.ask.chat_id == chat_id)
        .map(|pending| pending.ask.clone())
        .collect()
}

/// Answers one question. `false` when there was nothing to answer, which is a race a person loses
/// harmlessly: the turn moved on, or somebody answered a moment sooner.
pub fn answer_ask(id: &str, allow: bool) -> bool {
    let answer = ASKS
        .lock()
        .unwrap()
        .get_mut(id)
        .and_then(|pending| pending.answer.take());
    match answer {
        Some(answer) => answer.send(allow).is_ok(),
        None => false,
    }
}

/// Waits for this RUN's question to be answered. `None` means refuse, whatever the reason.
///
/// Keyed on the run and not on the question's name, which is what keeps it honest without a second
/// check: a run has at most one tool call in flight, because the hook that asks is synchronous and
/// the CLI is sitting on it. So "this run's ask" names exactly one thing, and no run can name
/// another's.
///
/// The question is taken down either way. A turn whose call was refused has moved on, and a
/// question still standing in the window would be about something that is no longer happening.
pub(crate) async fn wait_for_run(run_id: i64, window: std::time::Duration) -> Option<bool> {
    let (id, heard) = {
        let mut asks = ASKS.lock().unwrap();
        let (id, pending) = asks
            .iter_mut()
            .find(|(_, pending)| pending.ask.run_id == run_id)?;
        (id.clone(), pending.heard.take()?)
    };
    let answer = tokio::time::timeout(window, heard)
        .await
        .ok()
        .and_then(|heard| heard.ok());
    ASKS.lock().unwrap().remove(&id);
    answer
}

/// What the owner is told when a rooted turn asks for something that would need approving.
pub(crate) const ROOTED_APPROVAL_DENY_REASON: &str = "this needs approving, and a conversation is not where that happens — do it in the window, or \
     say what you want and let it start a run";

/// A rooted turn's tool call: the NucleOS tools as ever, and the machine through the classifier.
///
/// **The MCP tools keep their own door.** Anything `mcp__nucleos__*` goes to `assistant_decision`
/// unchanged, so the untrusted-read marking and the barrier that follows it are one implementation
/// and not two.
///
/// **A built-in goes through the classifier**, with the run's ROOT as its workspace — which is what
/// makes `writes outside the run's workspace are denied` mean something here: the conversation may
/// touch the project it is about and not the rest of the disk.
///
/// **`pending_approval` is answered with a refusal, not with a parked proposal.** This is the whole
/// reason the orchestrator branch existed: parking mints a proposal that expects a worktree run to
/// resume into, and a chat turn has none, so the turn would die owing an approval nobody can grant.
/// Refusing is not a lesser version of that — it is the right answer HERE. Elevation requires
/// `Origin::Shell`, which means the owner is sitting at this window; parking exists for work nobody
/// is watching, and the useful reply to somebody who is watching is to say so and let them answer.
///
/// The turn survives either way, which is the property the orchestrator branch was protecting: the
/// hook never terminates a conversation and never leaves a proposal behind it.
/// What `bypass` says when it lowers a refusal to a question.
///
/// BOTH reasons are rewritten where this is used, not one. `Classification` carries the sentence
/// twice — on the `Decision` and again at the top — and `downgrade_if_unreadable` already rewrites
/// both when it makes this same move. Leaving the old one would label the question in the window
/// "destructive deletion commands are denied", which is the sentence of a refusal that has just
/// stopped existing.
const BYPASS_STILL_ASKS: &str =
    "this conversation does not ask about anything else, and asks about this: it deletes";

/// Which of the classifier's `allow`s survive this rung.
///
/// The ladder is the CLI's own, and what separates its steps is not what the classifier DECIDED but
/// which of its allows a person is willing to have run unasked. The classifier already answers
/// `allow` for `Edit` and `Write` inside the workspace — they share the `read-local` branch with the
/// reads — so a rung that left the verdict alone would be `auto` under another name.
///
/// **`action_class` and NOT `only_reads`, and this is the biggest practical decision in the file.**
/// `only_reads` is a list of five TOOLS with `Bash` deliberately outside it, so a `manual` built on
/// its negation asks about every shell command a turn makes — `ls`, `git status`, `cargo check` —
/// five to fifteen questions a turn, each one blocking its own hook call against a 45-second window.
/// That is not what the CLI's own unelevated rung does, and it does not need building: this
/// classifier already answers `read-local` for a recognised non-mutating shell command. The two
/// questions are different and `only_reads`' own doc says so — it asks whether a TOOL can write
/// whatever it is handed, which is the right question for the third-party-text barrier; the class
/// asks whether THIS action changes anything, which is the right question here.
///
/// `WRITE_TOOLS` is subtracted from `manual` for the mirror-image reason: `read-local` covers
/// ordinary in-workspace writes too, and those are the next rung up, not this one.
fn allowed_at(
    permission: crate::chats::PermissionMode,
    action_class: &str,
    tool_name: &str,
) -> bool {
    match permission {
        crate::chats::PermissionMode::Manual => {
            action_class == "read-local" && !crate::classifier::writes_files(tool_name)
        }
        crate::chats::PermissionMode::AcceptEdits => action_class == "read-local",
        // Everything the classifier was willing to allow. `plan` is here and not one rung down
        // because its restraint comes from the CLI's own flag: a planning turn still reaches for
        // tools, and those calls arrive here and are governed exactly as `auto`'s are.
        crate::chats::PermissionMode::Plan
        | crate::chats::PermissionMode::Auto
        | crate::chats::PermissionMode::Bypass => true,
    }
}

async fn rooted_decision(
    state: &AppState,
    payload: &PreToolUsePayload,
    root: &str,
    project_id: Option<&str>,
    permission: crate::chats::PermissionMode,
) -> Json<Decision> {
    // Corrected in the handler before this is called, so it is the turn that is actually running
    // rather than the one a living process was spawned for.
    let run_id = payload.run_id;
    if payload.tool_name.starts_with("mcp__") {
        return assistant_decision(state, payload).await;
    }

    // The project comes down from the caller, off the same `runs` row that gave it `mode` — NOT out
    // of `vcs::project_for_worktree(root)`, which would be the obvious way to get it from `root` and
    // is the wrong one here. That function spawns a `git rev-parse` and canonicalises every roster
    // row on each call, and a hook runs in front of every single tool call; the comment on
    // `session_git_decision` makes the same point about its neighbour — this path would be spending
    // git subprocesses on a person's keystrokes. The row already knows.
    //
    // Filtered like the sibling call, and for the same reason: an unreadable list parks everything,
    // so a read no tool call could consult is a read whose failure costs more than the read itself.
    // The GitHub policy rides the same gate, as it does there.
    let (rules, policy) = if crate::classifier::reads_shell_rules(&payload.tool_name) {
        (
            shell_rules_of(state, project_id).await,
            github_policy_of(state, project_id).await,
        )
    } else {
        (
            ProjectRules::none(),
            std::borrow::Cow::Borrowed(&state.github.policy),
        )
    };
    let mut classification = crate::classifier::classify(
        &payload.tool_name,
        &payload.tool_input,
        Some(Path::new(root)),
        // The project's declared operations reach a conversation too. A rooted turn is the owner
        // working on the project the turn is about, and what that project may do on its own remote
        // does not change because somebody is watching.
        policy.as_ref(),
        // A project's `deny` means "never run this here", and a conversation is not an exemption:
        // the owner sitting in front of this turn is the person who wrote the list down.
        rules.declared(),
        // A rooted turn is a conversation. Somebody asked for it AND is sitting in front of it, so
        // a park costs them ten seconds and buys the strict reading.
        crate::classifier::Unrecognized::AsksAPerson,
    );

    // **`bypass` moves the VERDICT. It does not move the path.**
    //
    // Lowering the answer here and letting control fall through is not a style choice against
    // jumping to `ask_about` from the `deny` return below. That jump would step over the
    // third-party-text barrier a few lines down — for precisely the family this mode lowers — so a
    // turn that had read a stranger's words and then reached for `rm -rf` would be ASKED about
    // instead of refused, and a person could say yes. Falling through goes past the barrier by
    // construction rather than by somebody remembering to.
    //
    // What lowers and what does not comes to one sentence: this mode lowers a refusal about a
    // command's SHAPE, and never one about where the command POINTS, what somebody DECLARED, or a
    // fact about the TURN. So `destructive` — the two blind text tests — becomes a question, and
    // `destructive-outside`, `outside-workspace` and `project-denied` stay refusals. A class this
    // match has never heard of stays a refusal too: whoever adds a fourth family of `deny` should
    // not have to know this code exists for it to fail in the safe direction.
    //
    // The other half is the rung's whole purpose: what the rules would have ASKED about, this mode
    // runs. That is what somebody chose when they chose it — no second opinion, no latency, no
    // model bill — and it is still not silent about the one thing it lowers.
    if permission == crate::chats::PermissionMode::Bypass {
        // Read out first: matching on the struct borrows it for every arm, and two of them write
        // to it.
        let verdict = classification.decision.decision.clone();
        match (verdict.as_str(), classification.action_class) {
            ("deny", "destructive") => {
                classification.decision.decision = "pending_approval".to_owned();
                classification.decision.reason = BYPASS_STILL_ASKS.to_owned();
                classification.reason = BYPASS_STILL_ASKS.to_owned();
            }
            ("pending_approval", _) => {
                classification.decision.decision = "allow".to_owned();
            }
            _ => {}
        }
    }

    // AFTER the mode, and never before it. This turns an `allow` into a `pending_approval` when the
    // project's own lists could not be read, and no rung of this ladder lifts that: an unreadable
    // `deny` list is the "somebody may have declared this" case, which is exactly what `bypass`
    // above refuses to lower. Run first, its downgrade would be promoted straight back to `allow`
    // by the arm above — handing bypass the free pass that
    // `rules_that_cannot_be_read_cost_an_approval_and_never_an_allow` exists to refuse.
    let classification = rules.downgrade_if_unreadable(classification);

    if classification.decision.decision == "deny" {
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: classification.reason,
        });
    }

    // The read-untrusted barrier, extended to the tools this turn now has.
    //
    // Without it the barrier would hold on the MCP side and be walked around on the other: a turn
    // that read a mail body could not `approve_proposal`, and could run `Bash`.
    //
    // Keyed on `classifier::only_reads` and NOT on `action_class == "read-local"`, which was the
    // first attempt and was wrong in the direction that matters: that class is about approval, and
    // it covers ordinary in-workspace writes too, so `Write` sailed through the barrier. An
    // allow-list of tools that cannot change anything, rather than a deny-list of the classes that
    // can — because the second hands every future class through by default.
    if !crate::classifier::only_reads(&payload.tool_name) {
        match crate::runs::read_untrusted_context(&state.pool, payload.run_id).await {
            Ok(false) => {}
            Ok(true) => {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool = %payload.tool_name,
                    "pretooluse-decision: refused a rooted turn's action after it read third-party content"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: UNTRUSTED_CONTEXT_DENY_REASON.to_owned(),
                });
            }
            Err(error) => {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool = %payload.tool_name,
                    %error,
                    "pretooluse-decision: could not tell whether the rooted turn has read third-party content — failing closed"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not tell whether this turn has read third-party content"
                        .to_owned(),
                });
            }
        }
    }

    // The rung decides which allows survive; everything else falls through to the question below.
    if classification.decision.decision == "allow"
        && allowed_at(permission, classification.action_class, &payload.tool_name)
    {
        return Json(Decision {
            decision: "allow".to_owned(),
            reason: classification.reason,
        });
    }

    // Asked about, rather than refused. `ROOTED_APPROVAL_DENY_REASON` said "do it in the window" and
    // there was nowhere in the window to do it — which made a coding conversation stop at the first
    // action the classifier did not recognise as read-only.
    //
    // `asking` and not a verdict, because the hook has five seconds and a person does not. The fast
    // path stays fast; the waiting happens on a second call the hook makes only when it hears this.
    // A script too old to know the word fails closed on an unrecognised verdict, which is the same
    // refusal it gave before.
    //
    // `detail_of` and not the whole input, for the reason it exists: a `Write` carries the file it
    // is writing, and a window that printed that argument would print the file.
    if let Some(chat_id) = chat_of_run(&state.pool, run_id).await {
        crate::hooks::ask_about(
            &chat_id,
            run_id,
            &payload.tool_name,
            crate::runner::detail_of(&payload.tool_input),
        );
        return Json(Decision {
            decision: "asking".to_owned(),
            reason: classification.reason,
        });
    }

    // No conversation to ask — which is not a state a rooted turn can be in, since being rooted is a
    // property of its chat. Refused the way it always was rather than allowed on a technicality.
    Json(Decision {
        decision: "deny".to_owned(),
        reason: ROOTED_APPROVAL_DENY_REASON.to_owned(),
    })
}

async fn assistant_decision(state: &AppState, payload: &PreToolUsePayload) -> Json<Decision> {
    // Whole segment, not a prefix. MCP tool names are `mcp__<server>__<tool>`, so a server called
    // `nucleos__x` produced `mcp__nucleos__x__...`, which passed a prefix test and inherited the
    // orchestrator's unconditional allow — in the one mode that skips the classifier, the proposals
    // and the termination entirely.
    let Some(tool) = payload
        .tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|tool| !tool.contains("__"))
    else {
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "the orchestrator is restricted to NucleOS tools".to_owned(),
        });
    };

    // The `get_run`-names-a-triage-run rule lives in `mcp_tools::effect_of_call` rather than here,
    // because a second dispatcher needed the same answer and got a different one from the bare
    // table. One implementation is the only way two callers cannot disagree.
    //
    // The errand is passed rather than read out of `tool_input`, so a call cannot say whose folder
    // it is asking about; `errand_of_run` answers `None` for every run that has none and for every
    // resolution that fails, which `effect_of_call` reads as "cannot say" and classifies as a
    // stranger's words.
    let errand = errand_of_run(&state.pool, payload.run_id).await;
    let effect =
        crate::mcp_tools::effect_of_call(&state.pool, tool, &payload.tool_input, errand).await;

    match effect {
        crate::mcp_tools::ToolEffect::ReadsUntrusted => {
            // Marked BEFORE the tool is allowed, and the failure to mark refuses the read. The
            // alternative is a turn that has a stranger's words in it and no record of having read
            // them, which is the state every refusal below depends on not existing.
            if let Err(error) =
                crate::runs::mark_untrusted_context(&state.pool, payload.run_id).await
            {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool,
                    %error,
                    "pretooluse-decision: could not mark the turn as having read third-party content — refusing the read"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not record that this turn has read third-party content"
                        .to_owned(),
                });
            }
            // And WHICH stranger, which is a different question with a different failure policy.
            // The mark above fails closed because the barrier reads it; this is read only by a
            // person deciding whether to finish a refused action by hand, so failing closed here
            // would give a convenience a veto over every read the daemon does. Logged and carried
            // on: the cost is a refusal that has to say "not recorded", which is worse than knowing
            // and better than a browsing session that cannot open a page because a log line would
            // not write.
            if let Err(error) = crate::runs::record_untrusted_read(
                &state.pool,
                payload.run_id,
                tool,
                Some(&payload.tool_input.to_string()),
            )
            .await
            {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool,
                    %error,
                    "pretooluse-decision: the turn is marked but what it read could not be written down"
                );
            }
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "orchestrator NucleOS tool".to_owned(),
            })
        }
        crate::mcp_tools::ToolEffect::Acts => {
            // An errand never acts on its own, and this is asked BEFORE the barrier because it is a
            // different rule: the barrier is about what the turn has READ, and this is about whose
            // work it is. An errand is a Telegram topic. Its first message is a turn that has read
            // nothing, so the barrier would allow it — and the acting tools are the daemon's
            // controls and, one day, an email nobody can unsend.
            //
            // What this buys is that the errand's box is safe to widen. Until now the box was safe
            // because it happened to contain nothing that acts, and that property leaves the moment
            // somebody adds a tool. This one does not.
            if errand.is_some() {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool,
                    "pretooluse-decision: an errand may not act without a person"
                );
                // The reason the errand is GIVEN, not the untrusted-context one this used to
                // record. They are different statements and only one of them is true here: a clean
                // errand has read nobody's words, and the file's own argument for keeping the two
                // constants apart — "a model told something false about itself will try to work
                // around it" — applies to the record a person reads as much as to the reply.
                record_refused_action(state, payload, tool, errand, ERRAND_MAY_NOT_ACT).await;
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: ERRAND_MAY_NOT_ACT.to_owned(),
                });
            }
            match crate::runs::read_untrusted_context(&state.pool, payload.run_id).await {
                Ok(false) => Json(Decision {
                    decision: "allow".to_owned(),
                    reason: "orchestrator NucleOS tool".to_owned(),
                }),
                Ok(true) => {
                    // Warned, not merely refused. The owner asking their own bot to do two things
                    // in one message reaches this line, and so does a mail body that talked it into
                    // the second one; the two are indistinguishable from here, and only one of them
                    // is worth looking at a log for.
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        "pretooluse-decision: refused an action in a turn that has read third-party content"
                    );
                    // The errand this arm already resolved, rather than a second walk: the two
                    // could disagree only by being asked at different moments, and a record naming
                    // a different errand than the one the barrier judged is worse than an unnamed
                    // one.
                    record_refused_action(
                        state,
                        payload,
                        tool,
                        errand,
                        UNTRUSTED_CONTEXT_DENY_REASON,
                    )
                    .await;
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: UNTRUSTED_CONTEXT_DENY_REASON.to_owned(),
                    })
                }
                Err(error) => {
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        %error,
                        "pretooluse-decision: could not tell whether the turn has read third-party content — failing closed"
                    );
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: "could not tell whether this turn has read third-party content"
                            .to_owned(),
                    })
                }
            }
        }
        // `WritesOwn` sits with `ReadsOwn` and not with `Acts`, which is the entire reason the
        // variant exists. An errand reads the web first and writes down what it found afterwards, so
        // its every turn is already past the barrier by the time it records anything; refusing the
        // write here would mean an errand that never records anything at all. It reaches no network,
        // starts no work and lifts no approval, and the file it writes is marked with what the
        // writing turn had read — so the words do not launder by passing through the disk.
        crate::mcp_tools::ToolEffect::ReadsOwn | crate::mcp_tools::ToolEffect::WritesOwn => {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "orchestrator NucleOS tool".to_owned(),
            })
        }
    }
}

/// Writes down an action the barrier just refused, so somebody finds out it was wanted.
///
/// **Best-effort, and deliberately after the decision is already made.** Every failure here is
/// swallowed: the refusal is the security property and this is the courtesy beside it, so a full
/// disk or a lost race must never be able to turn a `deny` into anything else. The one failure that
/// is expected rather than exceptional is the unique violation — a run that reaches a second time
/// already has its row — and it is not worth a warning, which is why the log line says how many
/// rather than complaining.
///
/// The reasoning is the same sentence the model was given, because the person reading this in the
/// morning is answering a different question from the model's: not "may I", but "should I do this
/// myself". The tool input travels with it for the same reason — an errand asking to email a dealer
/// is a decision nobody can take from the tool name alone.
async fn record_refused_action(
    state: &AppState,
    payload: &PreToolUsePayload,
    tool: &str,
    errand: Option<i64>,
    reason: &str,
) {
    let session_id =
        sqlx::query_scalar::<_, Option<String>>("SELECT session_id FROM runs WHERE id = ?")
            .bind(payload.run_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
            .flatten();

    // Read here and copied onto the row, rather than joined at display time: `runs` rows are pruned
    // on their own schedule and this record is meant to outlive the turn. An error is `None` for the
    // same reason the write was best-effort — a person reading "not recorded" still has the action
    // in front of them, where a refusal that failed to be written down at all leaves them nothing.
    let read_from = crate::runs::untrusted_reads_json(&state.pool, payload.run_id)
        .await
        .unwrap_or_default();

    match crate::proposals::create_refused_action(
        &state.pool,
        payload.run_id,
        session_id.as_deref(),
        errand,
        tool,
        reason,
        Some(&payload.tool_input.to_string()),
        read_from.as_deref(),
    )
    .await
    {
        Ok(proposal_id) => tracing::info!(
            run_id = payload.run_id,
            proposal_id,
            tool,
            "pretooluse-decision: the refused action was written down for a person to read"
        ),
        Err(error) => tracing::debug!(
            run_id = payload.run_id,
            tool,
            %error,
            "pretooluse-decision: this run already has a refused action on record, or it could not be written"
        ),
    }
}

/// The errand behind the run making this call, or `None`.
///
/// A run records the chat it answers in `chat_id`, and an errand's `chat_key` IS that same string —
/// `errands::resolve` is keyed by it. Nothing guesses an errand from the shape of a chat key; the
/// row is the answer, and its absence is the common one, because almost no run is an errand's.
///
/// Every failure — a run with no chat, a chat with no errand, a database that will not answer —
/// answers `None`, which `effect_of_call` treats as "cannot say" and classifies as a stranger's
/// words. That is the fail-closed direction: the alternative would let an unresolvable errand make
/// an unrecorded file read as the owner's own notes.
async fn errand_of_run(pool: &sqlx::SqlitePool, run_id: i64) -> Option<i64> {
    let chat_id = sqlx::query_scalar::<_, Option<String>>("SELECT chat_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .flatten()?;
    crate::errands::resolve(pool, &chat_id)
        .await
        .ok()
        .flatten()
        .map(|errand| errand.id)
}

/// What a council seat may call: the named list, and nothing else.
///
/// An ALLOW-list, written out, rather than "deny the `Acts` ones". The two are the same today and
/// stop being the same the moment somebody adds a tool to the MCP server: a deny-list hands a
/// council every future tool by default, and this hands it none of them until somebody decides. The
/// direction matters more here than for the orchestrator, because a council is up to eight agents
/// launched by one sentence rather than one turn a person is watching.
///
/// Almost pure. The list is a membership test that cannot fail; the one stateful question is which
/// run a `get_run` names, and that one fails closed.
async fn council_decision(state: &AppState, payload: &PreToolUsePayload) -> Json<Decision> {
    // Whole segment, not a prefix, for the reason `assistant_decision` records: an MCP server named
    // `nucleos__x` produces `mcp__nucleos__x__…`, which passes a prefix test.
    let tool = payload
        .tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|tool| !tool.contains("__"))
        .filter(|tool| crate::mcp_tools::COUNCIL_TOOLS.contains(tool));

    if let Some(tool) = tool {
        // `get_run` reads any run by id, and run ids are sequential integers — so a seat could
        // read the row next to its own and find a sibling's answer before writing its own. Phase 1
        // is supposed to be N independent answers, and one seat that waited would be answering
        // with the others' work in front of it.
        //
        // The same shape as the orchestrator's `get_run` check above, and for a related reason:
        // that one asks whether the named run holds a stranger's words, this one whether it holds
        // a peer's. Both fail closed, because "I could not tell" is not "no".
        if tool == "get_run" && get_run_names_a_council_run(state, &payload.tool_input).await {
            tracing::debug!(
                run_id = payload.run_id,
                "pretooluse-decision: refused a council seat a look at another seat's run"
            );
            return Json(Decision {
                decision: "deny".to_owned(),
                reason: "a council seat may not read another seat's run".to_owned(),
            });
        }
        return Json(Decision {
            decision: "allow".to_owned(),
            reason: "council seats may read NucleOS state".to_owned(),
        });
    }

    // Not warned about. A seat reaching for `create_run` is a model being a model, not a symptom of
    // anything — where an orchestrator refused an action after reading mail is a line somebody
    // should read. A log level is a claim about who should look at it.
    tracing::debug!(
        run_id = payload.run_id,
        tool = %payload.tool_name,
        "pretooluse-decision: refused a tool a council seat may not call"
    );
    Json(Decision {
        decision: "deny".to_owned(),
        reason: "a council seat may only read NucleOS state".to_owned(),
    })
}

/// What a team agent may call: the named list, and — once it has read a stranger's words — the
/// reads alone.
///
/// **Two questions, and this branch used to answer only the first.** Which tools a department may
/// reach is `TEAM_TOOLS`, and `auth::TEAM_ROUTES` refuses the rest without anybody's cooperation.
/// Whether it may still ACT, having read, was answered nowhere on the cloud path: this function
/// allowed every name on the list unconditionally, and the doc that stood here said `TEAM_TOOLS`
/// carried no `Acts` — true when it was written, false since the alçada landed.
///
/// **`TEAM_TOOLS` is six untrusted reads and two asks**, which is what makes the shape below so
/// small. `get_email`, `get_email_queue`, `list_files`, `read_team_file`, `web_read` and
/// `web_search` all carry somebody else's words into the turn; `propose_action` and
/// `propose_teammate` are the alçada. A department's whole day is: read the world, then ask — and
/// the rule is that the asking comes first or not at all.
///
/// **The marking is this function's too, and that half was missing as well.** A refusal that
/// consults a flag nothing ever sets is a refusal that never fires, and `runs.read_untrusted` was
/// written for a cloud team node by nothing at all: the orchestrator's marking arm lives past the
/// `mode` branch that dispatches here, so a department returns before ever reaching it, and
/// `team.rs` marks on the LOCAL path only, off `local_agent`'s taint atomic. Adding the refusal
/// without the marking would have looked exactly like a fix and behaved exactly like none.
///
/// `effect_of_call` rather than the bare `tool_effect`, even though the two are identical for every
/// name on today's list — that function short-circuits anything which is not `ReadsOwn`, and no
/// team tool is. It is asked anyway so that the day somebody puts `get_run` on a department's list,
/// this branch gets the argument-aware answer the orchestrator gets rather than the bare table's.
/// One implementation is the only way two callers cannot disagree, which is a lesson this file
/// records having already paid for once.
///
/// Whole segment and not a prefix, for the reason `assistant_decision` records: an MCP server named
/// `nucleos__x` produces `mcp__nucleos__x__…`, which passes a prefix test and is not this server.
async fn team_decision(state: &AppState, payload: &PreToolUsePayload) -> Json<Decision> {
    let permitted = payload
        .tool_name
        .strip_prefix("mcp__nucleos__")
        .filter(|tool| !tool.contains("__"))
        .filter(|tool| crate::mcp_tools::TEAM_TOOLS.contains(tool));

    let Some(tool) = permitted else {
        // Debug and not warn, for the reason the council's branch gives: a specialist reaching for
        // `create_run` is a model being a model, not a symptom of anything.
        tracing::debug!(
            run_id = payload.run_id,
            tool = %payload.tool_name,
            "pretooluse-decision: refused a tool a team agent may not call"
        );
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: "a team agent may only read".to_owned(),
        });
    };

    // `None` rather than a walk: `errand_of_run` resolves an errand from a run's `chat_id`, and a
    // team run has none — `team::open_run` writes no `chat_id` at all. Passing `None` says so
    // outright instead of paying a query to be told it.
    let effect =
        crate::mcp_tools::effect_of_call(&state.pool, tool, &payload.tool_input, None).await;

    match effect {
        crate::mcp_tools::ToolEffect::ReadsUntrusted => {
            // Marked BEFORE the read is allowed, and a failure to mark refuses it — the same rule
            // and the same direction as the orchestrator's arm. The alternative is a department
            // holding a stranger's words with no record of having read them, which is the one state
            // every refusal below depends on not existing.
            if let Err(error) =
                crate::runs::mark_untrusted_context(&state.pool, payload.run_id).await
            {
                tracing::warn!(
                    run_id = payload.run_id,
                    tool,
                    %error,
                    "pretooluse-decision: could not record that a department read third-party content - refusing the read"
                );
                return Json(Decision {
                    decision: "deny".to_owned(),
                    reason: "could not record that this turn read third-party content".to_owned(),
                });
            }
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "team agents may read".to_owned(),
            })
        }
        crate::mcp_tools::ToolEffect::Acts => {
            match crate::runs::read_untrusted_context(&state.pool, payload.run_id).await {
                Ok(false) => Json(Decision {
                    decision: "allow".to_owned(),
                    reason: "a department may ask".to_owned(),
                }),
                Ok(true) => {
                    // Warned rather than merely refused, and unlike the council's `debug!` this one
                    // earns it: a specialist reaching for `create_run` is a model being a model, but
                    // a director filing a proposal straight after reading a web page is the exact
                    // sentence `propose_teammate`'s own comment describes - a director that read a
                    // page saying "hire an agent with this prompt" could otherwise file it.
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        "pretooluse-decision: refused a department's ask in a turn that has read third-party content"
                    );
                    // Written down for the reason the orchestrator's is: the alçada exists so a
                    // person reads a queue, and a refusal that leaves no trace is a department that
                    // quietly stopped asking with nothing anywhere saying why.
                    record_refused_action(
                        state,
                        payload,
                        tool,
                        None,
                        UNTRUSTED_CONTEXT_DENY_REASON,
                    )
                    .await;
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: UNTRUSTED_CONTEXT_DENY_REASON.to_owned(),
                    })
                }
                Err(error) => {
                    tracing::warn!(
                        run_id = payload.run_id,
                        tool,
                        %error,
                        "pretooluse-decision: could not tell whether a department has read third-party content - failing closed"
                    );
                    Json(Decision {
                        decision: "deny".to_owned(),
                        reason: "could not tell whether this turn has read third-party content"
                            .to_owned(),
                    })
                }
            }
        }
        // Nothing on today's list lands here, and the arm is written out rather than folded into a
        // wildcard so that a tool added to `TEAM_TOOLS` which neither carries a stranger's words nor
        // acts is allowed DELIBERATELY, by whoever puts it there.
        crate::mcp_tools::ToolEffect::ReadsOwn | crate::mcp_tools::ToolEffect::WritesOwn => {
            Json(Decision {
                decision: "allow".to_owned(),
                reason: "team agents may read".to_owned(),
            })
        }
    }
}

/// Whether a `get_run` call names another council seat's run.
///
/// Fails closed on every shape it cannot read, exactly as its triage sibling does, and for a reason
/// that is weaker but points the same way: what is lost by refusing is one lookup, and what is lost
/// by allowing wrongly is the independence phase 1 exists to have. A run that does not exist is the
/// one honest `false` — the tool returns an error and nothing is read.
async fn get_run_names_a_council_run(state: &AppState, tool_input: &Value) -> bool {
    let Some(id) = tool_input.get("id").and_then(Value::as_i64) else {
        return true;
    };
    match sqlx::query_scalar::<_, String>("SELECT mode FROM runs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await
    {
        Ok(Some(mode)) => mode == crate::council::COUNCIL_MODE,
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(
                run_id = id,
                %error,
                "pretooluse-decision: could not resolve the mode of the run a seat asked for — refusing it"
            );
            true
        }
    }
}

/// How many denied actions a run may attempt before it is stopped.
///
/// Not one: a run that reaches for something destructive, is told no, and takes a different route
/// is behaving exactly as the gate intends, and killing it there would turn the gate into a
/// tripwire. Not unbounded either, which is what it was. Three leaves room for an honest mistake
/// and a retry, and far too little to search a grammar with.
const DENIAL_LIMIT: i64 = 3;

/// Records a denied attempt and, once a run has spent its allowance, stops it.
///
/// Terminated to `failed` rather than `awaiting_approval`: a denied action is destructive by
/// classification, and the pause path exists to make an action approvable. Offering a human an
/// "approve" button here would launder precisely the verdict that is supposed to be final — the
/// class-scoped grant deliberately only ever lifts a `pending_approval`.
async fn count_denial_and_stop_a_prober(state: &AppState, run_id: i64, tool_name: &str) {
    let denials: i64 = match sqlx::query_scalar(
        "UPDATE runs SET denials = denials + 1 WHERE id = ? RETURNING denials",
    )
    .bind(run_id)
    .fetch_one(&state.pool)
    .await
    {
        Ok(count) => count,
        // The action is still denied — that part never depended on this write. Only the allowance
        // does, and the pool this shares with feed appends and status writes makes SQLITE_BUSY an
        // ordinary event here. Sustained probing is many calls, of which lost counts are a
        // minority; treating one lost count as a reason to kill the run would make a busy database
        // indistinguishable from an attack.
        Err(error) => {
            tracing::warn!(run_id, %error, "could not count a denied action against the run");
            return;
        }
    };

    tracing::warn!(
        run_id,
        tool = %tool_name,
        denials,
        "pretooluse-decision: denied action {denials}/{DENIAL_LIMIT} for this run"
    );
    if denials < DENIAL_LIMIT {
        return;
    }

    // Spawned and then awaited, for the same reason `pause_for_approval` is: terminating the run
    // kills the CLI whose hook script owns the connection this handler is answering, so the request
    // can vanish mid-handler and take an inline continuation with it.
    let state = state.clone();
    let tool_name = tool_name.to_owned();
    let _ = tokio::spawn(async move {
        if !finalize_termination(&state, run_id, "failed").await {
            return;
        }
        let _ = crate::feed::append(
            &state.pool,
            None,
            "run_stopped_probing",
            &format!("run {run_id} was stopped after {denials} denied actions (last: {tool_name})"),
            Some(run_id),
        )
        .await;
    })
    .await;
}

/// The whole `pending_approval` act: terminate the run, then record the proposal that makes the
/// pause actionable. These two belong together — a run parked in `awaiting_approval` with no
/// proposal can be neither approved nor rejected, and it goes on holding one of the project's
/// concurrency slots, permanently: the sweep spares `awaiting_approval`, and startup recovery only
/// reconciles rows left `running`. Hence the caller runs this as a detachable task rather than
/// inline in a request that may not survive its own side effects.
async fn pause_for_approval(
    state: AppState,
    run_id: i64,
    tool_name: String,
    tool_input: String,
    reason: String,
) {
    // Active termination (spec §8.4 steps 2–3): drive the run to `awaiting_approval` via the same
    // atomic-handle-removal arbiter cancellation uses (`finalize_termination`, Chunk 2 Task 4).
    if !finalize_termination(&state, run_id, "awaiting_approval").await {
        return;
    }

    let (session_id, project_id, job_id, stage) =
        sqlx::query_as::<_, (Option<String>, Option<String>, Option<i64>, Option<String>)>(
            "SELECT session_id, project_id, job_id, stage FROM runs WHERE id = ?",
        )
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or((None, None, None, None));

    // A node of a job takes the other road entirely, and three shapes of node take three roads.
    //
    // An **item's** node is put down and the job carries on to the next item. A **review** node owns
    // no item and does not need one: its verdict is advisory — §5.5 gives ship/no-ship to the gate —
    // and everything it was going to read is already written, gated and checkpointed. A **replan**
    // node owns none either, and ending it hands the job to `stop_after_replan`, which stops it
    // `Stopped` rather than `Failed` precisely so the rounds that DID run stay worth looking at.
    //
    // The rule underneath all three: **a node that asks already has an ending written for it**, and
    // the job's own handler picks it. What parking adds is not safety — it is a job that reads live,
    // holds a concurrency slot and does nothing, with no sign anywhere until a person goes looking.
    //
    // Measured, not reasoned into. Chunk 2 put review and plan on one road with a single sentence,
    // and four jobs falsified it in a day: 12 parked its review on `git reflog`, 13 parked its own
    // on a `for` loop, and 14 and 16 both parked their REPLAN nodes after every item had passed.
    //
    // The **plan** node is the one that still parks, and the argument for it is the one Chunk 2
    // made: the queue is what it produces, so there is nothing partial to preserve and nothing to
    // carry on to. Ending it would fail a job that a single answer turns into a night's work, to
    // save a slot that is now bounded by a ceiling rather than blocking the project.
    //
    // The mark is what decides for an item, and it is also step (2) of the skip — see
    // `skip_the_item`. Doing it here rather than inside keeps "did this run own an item?" and "put
    // it down" as one write: asking first and marking after would be a race with the same
    // cancel/reconcile the mark is already guarded against.
    //
    // Read HERE and not before the termination above, deliberately: `job_id` does not change when a
    // run ends, and moving the read earlier would put an extra query on the hot path of every
    // ordinary run's hook, which is nearly all of them.
    let road = match job_id {
        Some(job_id) if put_the_item_down(&state.pool, job_id, run_id).await => {
            Some((job_id, true))
        }
        Some(job_id) if NODES_THAT_GIVE_UP.contains(&stage.as_deref().unwrap_or_default()) => {
            Some((job_id, false))
        }
        _ => None,
    };
    if let Some((job_id, had_an_item)) = road {
        skip_the_item(
            state,
            SkippedItem {
                run_id,
                job_id,
                session_id,
                project_id,
                tool_name,
                tool_input,
                reason,
                had_an_item,
            },
        )
        .await;
        return;
    }

    if let Err(error) = crate::proposals::create_action_approval(
        &state.pool,
        run_id,
        session_id.as_deref(),
        project_id.as_deref(),
        &tool_name,
        &reason,
        Some(&tool_input),
    )
    .await
    {
        tracing::warn!(
            run_id,
            %error,
            "pretooluse-decision: failed to record action-approval proposal"
        );
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "proposal_record_failed",
            &format!("failed to record action-approval proposal: {error}"),
            Some(run_id),
        )
        .await;

        // Warning alone left the run parked in `awaiting_approval` with nothing to approve or
        // reject, holding one of the project's concurrency slots for good: the sweep spares that
        // status on purpose, because a run with a PENDING proposal is resumable.
        // `reconcile_stranded_approvals` is what tells the two apart — but only at startup, so the
        // project ran one slot narrower until someone restarted the daemon.
        //
        // Undo the pause instead. `interrupted` is the status startup recovery already uses for
        // exactly this shape, so a run that ends here reads the same either way, and the slot is
        // free immediately. Guarded on the status this function set, so a cancel that arrived in
        // the meantime keeps the last word.
        let rolled_back = sqlx::query(
            "UPDATE runs SET status = 'interrupted', completed_at = ?
             WHERE id = ? AND status = 'awaiting_approval'",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(run_id)
        .execute(&state.pool)
        .await;
        match rolled_back {
            Ok(result) if result.rows_affected() == 1 => tracing::warn!(
                run_id,
                "pretooluse-decision: rolled the unapprovable pause back to interrupted"
            ),
            Ok(_) => {}
            Err(rollback_error) => tracing::error!(
                run_id,
                %rollback_error,
                "pretooluse-decision: could not roll back an unapprovable pause — this project is blocked until restart"
            ),
        }
    }
}

/// A job's node asked for a decision, so the job puts the item down and carries on without it.
///
/// The other half of `pause_for_approval`, and it exists because parking is the wrong answer for a
/// job. A run belongs to a person who is going to come back to it; a job is the thing that was
/// supposed to work while nobody was watching, and stopping it dead on the first unrecognised shell
/// command is what "work through the night" met in practice — measured on 2026-08-07, where a
/// two-item job in a four-file repository parked seven times.
///
/// **The order of the three writes below is the whole safety argument, and it is not arbitrary.**
///
/// 1. The run is already terminated by the caller. That `.await` is the one `core/AGENTS.md` names
///    as biting hardest: it kills the CLI whose hook script owns the connection being answered, so
///    everything after it runs on borrowed time.
/// 2. **Mark the item.** First, and before anything that can fail or block — done by the caller in
///    `put_the_item_down`, because whether the mark landed is also what decides that this road is
///    the right one at all. An item left `running` in a job nobody is driving is a job that answers
///    `Wait` for ever — `next_step` sees a live node, there is no live node, and no later pass
///    rescues it. Every other loss here is recoverable; that one is not.
/// 3. **Take the pause off the run.** The caller parked it to ask; the asking is over. A run left
///    `awaiting_approval` parks the whole job through `job::node_awaiting_approval`, which is the
///    same stop by another door — the item reads `skipped` and the job waits on it anyway.
///    Recoverable, unlike (2): `reconcile_stranded_approvals` writes the same `interrupted` at the
///    next startup, which is why it goes second and not first.
/// 4. Revert the tree, then record the proposal. Both may fail, and neither failure is allowed to
///    take the mark with it: an item skipped without a proposal is work nobody will be reminded of,
///    which is bad and survivable, where an item stuck `running` is a dead job.
struct SkippedItem {
    run_id: i64,
    job_id: i64,
    session_id: Option<String>,
    project_id: Option<String>,
    tool_name: String,
    tool_input: String,
    reason: String,
    /// Whether this run owned an item whose half-written edits have to be undone.
    ///
    /// False for a review node, the other thing that takes this road, and the difference is not
    /// cosmetic. A review node owns no item, so `footing_for_run` would answer for whichever item
    /// ran last — and reverting to that footing would throw away the checkpoint of the job's final
    /// item, which is the one thing the review existed to look at. A node that changes nothing by
    /// design has nothing to revert.
    had_an_item: bool,
}

/// The job stages that give their node up rather than parking the job on it.
///
/// Both own no item, and both already have an ending written for the case where their run does not
/// come back: `ingest_replan`'s failure arm hands the job to `stop_after_replan` (`Stopped`, with
/// the rounds that ran still on the branch), and `load_view` reads any terminal review as `Done`
/// ("a review that failed is still a review that happened").
///
/// `plan` is deliberately absent. Its ending exists too — `Outcome::Failed` — but it is the only one
/// that throws away a whole job to save a slot: nothing has been done yet, so there is no partial
/// work to preserve, and a single answer turns that same job into a night's work.
const NODES_THAT_GIVE_UP: [&str; 2] = ["review", "replan"];

/// Step (2) of the skip, and the question that decides whether there is a skip at all: mark this
/// run's item `skipped`, and say whether there was one.
///
/// Scoped to the item this run owns, so a job whose other items are in flight is untouched. A
/// `false` answer has two shapes and the caller treats them alike, because the right move is the
/// same for both — park and ask:
///
/// - **No item exists.** A plan or review node. There is nothing to put down, and skipping a job's
///   plan would leave it with no queue to carry on with.
/// - **The item is no longer `running`.** A cancel or a reconcile got there first and its verdict is
///   the newer one; overwriting it would be this handler talking over a decision already made.
///
/// A database error also answers `false`, which parks the run rather than skipping an item that may
/// still be `running`. Loud, and recoverable by hand — where a job stalled on a live item is not.
async fn put_the_item_down(pool: &sqlx::SqlitePool, job_id: i64, run_id: i64) -> bool {
    let marked = sqlx::query(
        "UPDATE job_items SET status = ? WHERE job_id = ? AND run_id = ? AND status = 'running'",
    )
    .bind(crate::job::STATUS_SKIPPED)
    .bind(job_id)
    .bind(run_id)
    .execute(pool)
    .await;
    match marked {
        Ok(result) if result.rows_affected() == 1 => true,
        Ok(_) => {
            tracing::info!(
                run_id,
                job_id,
                "pretooluse-decision: no running job item for this run — parking it instead"
            );
            false
        }
        Err(error) => {
            tracing::error!(
                run_id,
                job_id,
                %error,
                "pretooluse-decision: could not mark a job item skipped — parking the run instead"
            );
            false
        }
    }
}

async fn skip_the_item(state: AppState, item: SkippedItem) {
    let SkippedItem {
        run_id,
        job_id,
        session_id,
        project_id,
        tool_name,
        tool_input,
        reason,
        had_an_item,
    } = item;

    // (3) Take the pause off the RUN. The caller terminated it to `awaiting_approval`, because at
    // that point the answer was still "ask a person". It is not any more — the answer was "skip it",
    // and it has already been given.
    //
    // Without this the job stops anyway, one step later and for a different reason:
    // `job::node_awaiting_approval` asks whether ANY run of the job is `awaiting_approval` and parks
    // the whole job when one is, and `node_in_flight` counts that status as a node still to wait
    // for. The item would read `skipped` while the job sat on a node nobody would ever answer —
    // measured on 2026-08-08, job 3: item 0 `skipped`, job `awaiting_approval`, nothing pending.
    //
    // `interrupted` and not something new: it is what `reconcile_stranded_approvals` writes at
    // startup for precisely this row (an `awaiting_approval` run with no `action-approval` proposal),
    // and what `pause_for_approval` rolls back to when a pause turns out not to be one. Writing it
    // here is the same verdict without waiting for a restart.
    //
    // AFTER the item's mark, never before, for two reasons. If this write is lost the damage is
    // recoverable — that same startup reconciler writes it — where an item left `running` is a job
    // that answers `Wait` for ever and no pass rescues. And the window between the two writes is
    // only safe in this order: `job::reconcile_nodes` folds back every item that is still `running`
    // whose run has left flight, and would read this one as `failed` and stop the chain. Marked
    // first, the item is no longer `running` and that query cannot see it at all.
    //
    // Guarded on the status the caller set, so a cancel that landed in between keeps the last word.
    let unpaused = sqlx::query(
        "UPDATE runs SET status = 'interrupted', completed_at = ?
         WHERE id = ? AND status = 'awaiting_approval'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(run_id)
    .execute(&state.pool)
    .await;
    if let Err(error) = unpaused {
        tracing::error!(
            run_id,
            job_id,
            %error,
            "pretooluse-decision: could not lift the pause off a skipped item's run — the job will park until restart"
        );
    }

    // (4a) Put the tree back. The item wrote whatever it wrote before it asked, and the next item
    // must not build on a half-done change nobody approved.
    //
    // Only for an item. A review node reaches this function too and must NOT come through here:
    // it owns no item, so `footing_for_run` would answer for whichever one ran last, and reverting
    // to that footing would discard the checkpoint of the job's final item — the very work the
    // review was there to read. A node that changes nothing by design has nothing to put back.
    if had_an_item {
        // One call and not two. The tree and the sha are a pair — reverting a checkout to a footing
        // taken from a different one is worse than reverting nothing — and asking for them together
        // is what keeps them from being resolved off different keys, which is how a node working in
        // its item's own tree came to have the job's reverted instead.
        match crate::job::revert_target(&state.pool, job_id, run_id).await {
            Some((worktree, sha)) => {
                if let Err(error) = crate::worktree::revert_to(&worktree, &sha).await {
                    tracing::warn!(run_id, job_id, %error, "pretooluse-decision: could not revert a skipped item");
                }
            }
            None => tracing::warn!(
                run_id,
                job_id,
                "pretooluse-decision: no worktree or no footing to revert a skipped item to"
            ),
        }
    }

    // (4b) The proposal. A different `kind` from an action approval, and `wip.rs` counts only the
    // other one: this is work NOT YET DONE waiting on a decision, where the WIP limit exists to cap
    // work already done waiting to be looked at. Conflating them closes the autonomy this change
    // just opened, at the third skipped item of the night.
    if let Err(error) = crate::proposals::create_skipped_item(
        &state.pool,
        run_id,
        session_id.as_deref(),
        project_id.as_deref(),
        &tool_name,
        &reason,
        Some(&tool_input),
    )
    .await
    {
        tracing::warn!(run_id, job_id, %error, "pretooluse-decision: failed to record a skipped-item proposal");
        let _ = crate::feed::append(
            &state.pool,
            project_id.as_deref(),
            "proposal_record_failed",
            &format!("failed to record skipped-item proposal: {error}"),
            Some(run_id),
        )
        .await;
        // Deliberately NOT rolled back, where the action-approval path above rolls its pause back.
        // There the run is stuck `awaiting_approval` with nothing to approve, holding an index that
        // blocks the project. Here the item is `skipped`, the job moves on, and what is lost is the
        // reminder — a worse outcome than having it, and a far better one than a job that stalls.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::proposals;
    use crate::runner::FakeCommandRunner;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        AppState {
            token: Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            workflow_library: None,
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// The real middleware, not a stand-in that inserts the extension directly: the handler now
    /// reads a `Scope` that only `require_token` puts there, so a test router without it would pass
    /// while every hook call in production returned 500.
    fn test_router(state: AppState) -> Router {
        Router::new()
            .route("/hooks/pretooluse-decision", post(pretooluse_decision))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::auth::require_token,
            ))
            .with_state(state)
    }

    /// As the control token, which is what these tests were written against — they are about what
    /// the classifier decides, not about who is allowed to ask. `decide_as` is for the latter.
    async fn decide(app: &Router, body: &str) -> Decision {
        decide_as(app, "test-token", body).await
    }

    async fn decide_as(app: &Router, bearer: &str, body: &str) -> Decision {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/hooks/pretooluse-decision")
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {bearer}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Insert a `runs` row and spawn a long-sleeping task whose abort handle is registered under
    /// the row's id, making that run look in-flight to `pretooluse_decision` the same way a real
    /// governed run does. `project_id`, `cwd`, and `session_id` are `None` for tests that don't
    /// care about them; `created_at` is a fixed placeholder since no test ever asserts on it.
    async fn in_flight_run(
        state: &AppState,
        mode: &str,
        project_id: Option<&str>,
        cwd: Option<&str>,
        session_id: Option<&str>,
    ) -> i64 {
        let run_id = sqlx::query(
            "INSERT INTO runs (prompt, status, mode, project_id, cwd, session_id, created_at)
             VALUES ('x', 'running', ?, ?, ?, ?, '2026-07-17T00:00:00Z')",
        )
        .bind(mode)
        .bind(project_id)
        .bind(cwd)
        .bind(session_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let task =
            tokio::spawn(async { tokio::time::sleep(std::time::Duration::from_secs(60)).await });
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(run_id, task.abort_handle());

        run_id
    }

    /// A live implement node of a live job: the job row, one `running` item, the run that owns it,
    /// and a worktree row for the job so the revert has somewhere to point.
    ///
    /// Returns `(job_id, run_id)`.
    async fn in_flight_job_node(state: &AppState) -> (i64, i64) {
        let job_id = sqlx::query(
            "INSERT INTO jobs
             (project_id, project_root, prompt, status, max_items, gate_each, review, created_at)
             VALUES ('proj', 'C:\\work\\repo', 'advance the backlog', 'implementing', 5, 1, 1,
                     '2026-08-07T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid();

        let run_id = in_flight_run(
            state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        sqlx::query("UPDATE runs SET job_id = ?, stage = 'implement' WHERE id = ?")
            .bind(job_id)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO job_items (job_id, ordinal, description, status, run_id)
             VALUES (?, 0, 'the first thing', 'running', ?)",
        )
        .bind(job_id)
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch, created_at)
             VALUES ('job', ?, 'proj', 'C:\\work\\repo', 'C:\\work\\wt', 'nucleos/job-x',
                     '2026-08-07T00:00:00Z')",
        )
        .bind(job_id)
        .execute(&state.pool)
        .await
        .unwrap();

        (job_id, run_id)
    }

    /// Which runs earn the confinement widening, and which do not — the whole conjunction, one
    /// case per way of failing it.
    ///
    /// The last two are the ones worth having: this function is the only thing standing between
    /// "the owner asked for this" and "an unrecognised command may run", so every way of NOT being
    /// that has to come back `AsksAPerson` rather than fall through to it.
    #[tokio::test]
    async fn only_an_unattended_node_of_a_job_the_owner_asked_for_may_be_confined() {
        use crate::classifier::Unrecognized;
        let state = test_state().await;

        // Asked for (`rule_name IS NULL`) and unattended: the case the whole thing exists for.
        let (job_id, run_id) = in_flight_job_node(&state).await;
        assert_eq!(
            unrecognized_policy_for(&state.pool, run_id, "worktree").await,
            Unrecognized::MayBeConfined
        );

        // Same run, same job, watched by a person: a park costs them ten seconds.
        assert_eq!(
            unrecognized_policy_for(&state.pool, run_id, "real").await,
            Unrecognized::AsksAPerson
        );

        // Same run, same mode, but now the job came from a schedule. Nobody agreed to this work.
        sqlx::query("UPDATE jobs SET rule_name = 'nightly' WHERE id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            unrecognized_policy_for(&state.pool, run_id, "worktree").await,
            Unrecognized::AsksAPerson
        );

        // A run belonging to no job at all. It may well have been requested — a standalone
        // `POST /runs` is — but the daemon records no origin for a run, so there is nothing to
        // read and the strict answer is the only honest one.
        let orphan = in_flight_run(&state, "worktree", Some("proj"), None, None).await;
        assert_eq!(
            unrecognized_policy_for(&state.pool, orphan, "worktree").await,
            Unrecognized::AsksAPerson
        );

        // A run id that names nothing.
        assert_eq!(
            unrecognized_policy_for(&state.pool, 999_999, "worktree").await,
            Unrecognized::AsksAPerson
        );
    }

    /// The other road out of the same door, and the one the owner asked for: the call is refused,
    /// the item is NOT put down, and the work the node has already done stays where it is.
    #[tokio::test]
    async fn a_requested_jobs_node_is_refused_rather_than_put_down() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "frobnicate --hard"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");
        assert!(
            decision.reason.contains("the item is still yours"),
            "the node was not told it may carry on: {}",
            decision.reason
        );

        let item: String =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? AND run_id = ?")
                .bind(job_id)
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(item, "running", "the item was put down for asking");

        let run_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(run_status, "running", "the run was terminated for asking");

        // The owner still gets the decision, as a record rather than as a question.
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(kinds, vec!["refused-action"]);
    }

    /// The plan node keeps parking, and this is the test that stops the refusal spreading to it.
    ///
    /// Its park is the only one in a job that is a real question: it produces the queue, so there
    /// is nothing partial to lose, and the proposal it mints can be answered and resumed.
    #[tokio::test]
    async fn a_requested_jobs_plan_node_still_parks_because_its_park_can_be_answered() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        // What makes it a plan node: it owns no running item, and its stage says so.
        sqlx::query("UPDATE job_items SET status = 'passed', run_id = NULL WHERE job_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET stage = 'plan' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "frobnicate --hard"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            kinds,
            vec!["action-approval"],
            "a plan node's park has to stay something a person can answer"
        );
    }

    /// The two copies of one condition, pinned against each other.
    ///
    /// `a_park_here_would_only_destroy` spells out in SQL what `pause_for_approval` decides in Rust
    /// with `put_the_item_down` and `NODES_THAT_GIVE_UP`. Two statements of one rule drift, and the
    /// drift would be silent and one-directional — the predicate saying "refuse" for a node the
    /// other road would have parked answerably, or the reverse. So both are asked, per shape.
    #[tokio::test]
    async fn a_refusal_and_a_skip_agree_about_which_nodes_they_cover() {
        for (stage, owns_an_item, ends_the_node) in [
            ("implement", true, true),
            ("review", false, true),
            ("replan", false, true),
            // The exception, and the only one.
            ("plan", false, false),
            // A stage that gives up owns no item; one that owns an item is covered whatever its
            // stage says, which is why the first row and this one must disagree.
            ("implement", false, false),
        ] {
            let state = test_state().await;
            let (job_id, run_id) = in_flight_job_node(&state).await;
            if !owns_an_item {
                sqlx::query(
                    "UPDATE job_items SET status = 'passed', run_id = NULL WHERE job_id = ?",
                )
                .bind(job_id)
                .execute(&state.pool)
                .await
                .unwrap();
            }
            sqlx::query("UPDATE runs SET stage = ? WHERE id = ?")
                .bind(stage)
                .bind(run_id)
                .execute(&state.pool)
                .await
                .unwrap();

            assert_eq!(
                a_park_here_would_only_destroy(&state.pool, run_id, "worktree").await,
                ends_the_node,
                "stage {stage}, owns_an_item {owns_an_item}: the predicate and the skip road \
                 disagree about this shape"
            );
            // And the other half of the conjunction, on the same row: scheduled work keeps the
            // skip road whatever its shape.
            nobody_asked_for_this_job(&state.pool, job_id).await;
            assert!(
                !a_park_here_would_only_destroy(&state.pool, run_id, "worktree").await,
                "stage {stage}: work nobody asked for was given the refusal road"
            );
        }
    }

    /// Make this job one a SCHEDULE started rather than one the owner asked for.
    ///
    /// The four tests below cover the road a park takes when it ENDS a node, and since 2026-08-30
    /// that road belongs to proactive work: a job the owner asked for is refused the call and keeps
    /// its item (`a_park_here_would_only_destroy`). The road did not go away and neither did its
    /// tests — they say which work it applies to now, in one line each.
    async fn nobody_asked_for_this_job(pool: &sqlx::SqlitePool, job_id: i64) {
        sqlx::query("UPDATE jobs SET rule_name = 'nightly' WHERE id = ?")
            .bind(job_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// A `runs` row without an abort handle: the run exists and its mode is on record, but nothing
    /// is executing under it. Every barrier that reads `mode` has to hold here too, because this is
    /// the state a run passes through on its way out — and the state a forged request would claim.
    async fn out_of_flight_run(state: &AppState, mode: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, created_at)
             VALUES ('x', 'completed', ?, '2026-07-28T00:00:00Z')",
        )
        .bind(mode)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Mints and stores a run's own key, returning what its CLI would find in the environment.
    async fn key_for(state: &AppState, run_id: i64) -> String {
        let (token, secret) = crate::auth::mint_run_token(run_id);
        sqlx::query("UPDATE runs SET token = ? WHERE id = ?")
            .bind(&secret)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        token
    }

    /// A rooted turn that needs approving is ASKED about, rather than refused outright.
    ///
    /// This is the wall a coding conversation hits. The classifier sends everything not provably
    /// read-only for approval, and a chat turn cannot park a proposal — one expects a worktree run
    /// to resume into and a conversation has none — so the answer was a refusal telling the person
    /// to go and do it somewhere else. There is nowhere else: it is their window, they are watching
    /// it, and the useful reply to somebody watching is a question.
    ///
    /// `asking` and not a verdict, because the hook has five seconds and a person does not. The
    /// fast path stays fast and the waiting happens on a second call.
    #[tokio::test]
    async fn a_rooted_turn_that_needs_approving_is_asked_about_rather_than_refused() {
        let state = test_state().await;
        let root = tempfile::TempDir::new().unwrap();
        let (chat_id, run_id) = rooted_conversation(&state, &root).await;
        let key = crate::auth::mint_chat_token(&state.pool, &chat_id)
            .await
            .unwrap();
        let app = test_router(state.clone());

        let decision = decide_as(
            &app,
            &key,
            &format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"npm publish"}}}}"#
            ),
        )
        .await;

        assert_eq!(decision.decision, "asking");
        let waiting = asks_for(&chat_id);
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].tool, "Bash");
        assert_eq!(waiting[0].detail.as_deref(), Some("npm publish"));
        answer_ask(&waiting[0].id, false);
    }

    /// Saying yes lets the call through; saying no refuses it. The waiting call is what the hook is
    /// sitting on, and its answer is what the CLI is finally told.
    #[tokio::test]
    async fn what_the_person_says_is_what_the_tool_call_is_told() {
        for (allowed, expected) in [(true, "allow"), (false, "deny")] {
            let state = test_state().await;
            let root = tempfile::TempDir::new().unwrap();
            let (chat_id, run_id) = rooted_conversation(&state, &root).await;

            let id = ask_about(&chat_id, run_id, "Bash", Some("npm publish".to_owned()));
            let waiting =
                tokio::spawn(async move { wait_for_run(run_id, Duration::from_secs(5)).await });

            // The window answering, a moment later, as a person does.
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(answer_ask(&id, allowed));

            let answer = waiting.await.unwrap();
            assert_eq!(
                answer.map(|yes| if yes { "allow" } else { "deny" }),
                Some(expected)
            );
        }
    }

    /// An ask nobody answers is refused, not left open.
    ///
    /// The CLI is holding a hook call while this waits, and the model is holding a turn behind that.
    /// Fail closed and say why: a person who stepped away gets a refused tool call, not a
    /// conversation that hangs until something else times it out.
    #[tokio::test]
    async fn an_ask_nobody_answers_is_refused() {
        let state = test_state().await;
        let root = tempfile::TempDir::new().unwrap();
        let (chat_id, run_id) = rooted_conversation(&state, &root).await;

        let _id = ask_about(&chat_id, run_id, "Bash", None);
        let answer = wait_for_run(run_id, Duration::from_millis(30)).await;

        assert_eq!(answer, None);
        // And it is gone, rather than sitting in the window as a question about a turn that has
        // already moved on.
        assert!(asks_for(&chat_id).is_empty());
    }

    /// A run waits on ITS OWN ask and can reach no other, which is what stops one turn answering
    /// for another — or consuming the question somebody else is being asked.
    #[tokio::test]
    async fn a_run_waits_on_its_own_ask_and_reaches_no_other() {
        let state = test_state().await;
        let root = tempfile::TempDir::new().unwrap();
        let (chat_id, run_id) = rooted_conversation(&state, &root).await;

        let id = ask_about(&chat_id, run_id, "Bash", None);

        // A different run, waiting: there is nothing of its own to wait for.
        assert_eq!(
            wait_for_run(run_id + 1, Duration::from_millis(30)).await,
            None
        );
        // And the question that was not theirs is still standing.
        assert_eq!(asks_for(&chat_id).len(), 1);
        answer_ask(&id, false);
    }

    /// A conversation with a directory and a running turn, which is what `rooted_decision` needs.
    async fn rooted_conversation(state: &AppState, root: &tempfile::TempDir) -> (String, i64) {
        let chat_id = format!("asking-{}", crate::auth::generate_uuid_v4());
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at, cwd)
             VALUES (?, 'cloud', '2026-01-01T00:00:00Z', ?)",
        )
        .bind(&chat_id)
        .bind(root.path().to_str().unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
        // An id of its own, and not the one this pool would hand out. Every test has its own
        // in-memory database, so every one of them would call its first run `1` — while the ask
        // registry is a single process-wide map, exactly as it is in the daemon. Real run ids are
        // unique because there is one database; here they have to be made so.
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(90_000);
        let run_id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, mode, chat_id, created_at)
             VALUES (?, 'x', 'running', 'assistant', ?, '2026-07-17T00:00:00Z')",
        )
        .bind(run_id)
        .bind(&chat_id)
        .execute(&state.pool)
        .await
        .unwrap();
        (chat_id, run_id)
    }

    /// A living conversation's turn is judged on what IT read, not on what its first turn read.
    ///
    /// This is the hole a kept process opens, and it is not the one the key closed. `ask_daemon.py`
    /// echoes `NUCLEOS_RUN_ID` out of an environment fixed at spawn, so from the second turn on the
    /// claim in the body names the FIRST turn — for the rest of the conversation. The handler
    /// resolves the real turn from the key; every branch it hands the payload to was still reading
    /// the claim.
    ///
    /// What that costs is the ordering rule: read what you like, act while nothing third-party has
    /// entered the turn, but not both and not in that order. A turn that read a stranger's text is
    /// marked on ITS row, and a barrier checking the first turn's row finds nothing there — so the
    /// rule holds on turn one and is walked around on every turn after it, in the exact
    /// conversations that were given Bash, Read and Write.
    #[tokio::test]
    async fn a_living_conversations_turn_is_judged_on_what_it_read_not_on_what_its_first_turn_did()
    {
        let state = test_state().await;
        let root = tempfile::TempDir::new().unwrap();
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at, cwd)
             VALUES ('live-chat', 'cloud', '2026-01-01T00:00:00Z', ?)",
        )
        .bind(root.path().to_str().unwrap())
        .execute(&state.pool)
        .await
        .unwrap();

        // The turn the process was started for, long finished — and clean.
        let first = turn_of(&state, "live-chat").await;
        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();

        // The turn being answered right now, down the same living process, which has read a
        // stranger's text.
        let now = turn_of(&state, "live-chat").await;
        crate::runs::mark_untrusted_context(&state.pool, now)
            .await
            .unwrap();

        let key = crate::auth::mint_chat_token(&state.pool, "live-chat")
            .await
            .unwrap();
        let app = test_router(state.clone());

        // The process still echoes the id it was spawned with, because that is the only id it has.
        let decision = decide_as(
            &app,
            &key,
            &format!(
                r#"{{"run_id":{first},"tool_name":"Bash","tool_input":{{"command":"echo hi"}}}}"#
            ),
        )
        .await;

        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, UNTRUSTED_CONTEXT_DENY_REASON);
    }

    /// One running turn of a conversation, the shape a chat turn's row actually has.
    async fn turn_of(state: &AppState, chat_id: &str) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('x', 'running', 'assistant', ?, '2026-07-17T00:00:00Z')",
        )
        .bind(chat_id)
        .execute(&state.pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// `run_id` comes from the request body, so it is a claim the caller makes about itself. Every
    /// branch in this handler reads `mode` from that id, so a run able to name another run's id
    /// picks which rules it is judged by — a `shadow` run could ask under a `worktree` run's id and
    /// be handed the worktree ruleset — and could spend another run's denial allowance to have it
    /// stopped.
    ///
    /// So the key decides and the claim is ignored, rather than the two being compared and a
    /// mismatch refused. The guarantee is the same one, kept by construction instead of by
    /// inspection; what changes is that a stale claim is now merely irrelevant rather than fatal,
    /// which is what a CLI kept alive across turns needs — it is handed its environment once, at
    /// spawn, and would echo the first turn's id for the rest of the conversation.
    #[tokio::test]
    async fn the_gate_judges_a_turn_by_its_key_and_not_by_the_id_it_claims() {
        let state = test_state().await;
        // A `worktree` run borrowing a `shadow` run's id, because that is the direction with
        // something to gain: shadow's whole premise is that it records what it WOULD have done and
        // is never stopped for it, so a real run judged by shadow's rules would be a run that can
        // probe the classifier forever.
        let mine = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let other = in_flight_run(&state, "shadow", None, None, None).await;
        let my_key = key_for(&state, mine).await;
        let app = test_router(state.clone());

        // Naming the shadow run does not buy shadow's ruleset: the answer is the one this run's own
        // mode earns, whichever id it wrote down.
        let claimed = decide_as(
            &app,
            &my_key,
            &format!(r#"{{"run_id":{other},"tool_name":"Read","tool_input":{{}}}}"#),
        )
        .await;
        let own = decide_as(
            &app,
            &my_key,
            &format!(r#"{{"run_id":{mine},"tool_name":"Read","tool_input":{{}}}}"#),
        )
        .await;
        assert_eq!(claimed.decision, own.decision);
        assert_eq!(own.decision, "allow");

        // And the denials it spends are its own — counted against the run that actually made them,
        // not against the one it named. Otherwise borrowing an id would be a way to stop any run in
        // the house from behind a key that governs one.
        let probe = format!(
            r#"{{"run_id":{other},"tool_name":"Bash","tool_input":{{"command":"rm -rf /"}}}}"#
        );
        for _ in 0..DENIAL_LIMIT {
            assert_eq!(decide_as(&app, &my_key, &probe).await.decision, "deny");
        }
        let handles = state.run_handles.lock().unwrap();
        assert!(
            !handles.contains_key(&mine),
            "the prober's own run is stopped"
        );
        assert!(
            handles.contains_key(&other),
            "the run whose id it borrowed is untouched"
        );
    }

    /// A `deny` used to cost the run nothing, so a lexical classifier could be searched: try a
    /// spelling, get told no, try the next, forever, with nothing counting the attempts and nothing
    /// watching. The allowance leaves room for an honest mistake and stops a search.
    #[tokio::test]
    async fn a_run_that_keeps_reaching_for_denied_actions_is_stopped() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"rm -rf /"}}}}"#
        );

        for attempt in 1..DENIAL_LIMIT {
            assert_eq!(decide(&app, &body).await.decision, "deny");
            assert!(
                state.run_handles.lock().unwrap().contains_key(&run_id),
                "attempt {attempt} is within the allowance and must not stop the run"
            );
        }

        // The one that spends it. The action is still denied — being stopped is on top of the
        // refusal, never instead of it.
        assert_eq!(decide(&app, &body).await.decision, "deny");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        // `failed`, not `awaiting_approval`: a denied action must never acquire an approve button.
        assert_eq!(status, "failed");

        let proposals = proposals::list_pending(&state.pool).await.unwrap();
        assert!(
            proposals.is_empty(),
            "stopping a prober must not mint something a human can approve"
        );
    }

    /// A run that tries again the action the queue took over is TOLD so, not paused.
    ///
    /// The loop this closes: the approval queues the merge and mints no grant, so the resumed run's
    /// retry used to be an ordinary `pending_approval` — pausing the run and putting a second
    /// proposal in front of a person, whose approval would queue the same merge a second time. The
    /// prompt asks the run not to retry; this is what happens when it does anyway.
    ///
    /// Three things are asserted because getting any one of them wrong reopens the loop: the verdict
    /// names the request, the run is still in flight, and nothing was minted for a person to read.
    #[tokio::test]
    async fn an_action_the_queue_already_has_is_refused_rather_than_asked_about_again() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());
        let input = r#"{"command":"git merge feature/x"}"#;

        sqlx::query(
            "INSERT INTO action_grants
             (run_id, tool_name, tool_input, proposal_id, created_at, consumed_at, queued_request_id)
             VALUES (?, 'Bash', ?, 1, '2026-01-01T00:00:00Z', NULL, 77)",
        )
        .bind(run_id)
        .bind(input)
        .execute(&state.pool)
        .await
        .unwrap();

        let body = format!(r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{input}}}"#);
        let decision = decide(&app, &body).await;

        assert_eq!(decision.decision, "deny");
        assert!(
            decision.reason.contains("request 77"),
            "the run has to be told WHICH request has its work: {}",
            decision.reason
        );
        assert!(
            state.run_handles.lock().unwrap().contains_key(&run_id),
            "a refusal that names the queue must not also stop the run"
        );
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty(),
            "asking a person again about a merge already queued would queue it twice"
        );
    }

    /// The refusal is COUNTED, and that is the half that keeps the answer from being free.
    ///
    /// Without it the verdict would be the one thing in the system that neither pauses the run nor
    /// spends anything: a run that ignores both the resume prompt and the reason string could retry
    /// for ever, burning tokens against a merge that is already on its way. The test that asserts
    /// the run survives ONE refusal cannot see that — it passes whether or not anything is counted.
    #[tokio::test]
    async fn a_run_that_keeps_retrying_a_queued_action_is_stopped() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());
        let input = r#"{"command":"git merge feature/x"}"#;

        sqlx::query(
            "INSERT INTO action_grants
             (run_id, tool_name, tool_input, proposal_id, created_at, consumed_at, queued_request_id)
             VALUES (?, 'Bash', ?, 1, '2026-01-01T00:00:00Z', NULL, 77)",
        )
        .bind(run_id)
        .bind(input)
        .execute(&state.pool)
        .await
        .unwrap();

        let body = format!(r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{input}}}"#);
        for _ in 0..DENIAL_LIMIT {
            assert_eq!(decide(&app, &body).await.decision, "deny");
        }

        assert!(
            !state.run_handles.lock().unwrap().contains_key(&run_id),
            "a run told {DENIAL_LIMIT} times where its work went, that asks again, is not obeying"
        );
    }

    /// The allowance is per run, so one run spending it does not shorten another's.
    #[tokio::test]
    async fn each_run_gets_its_own_allowance() {
        let state = test_state().await;
        let spender = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;
        let bystander = in_flight_run(&state, "worktree", Some("proj-2"), None, None).await;
        let app = test_router(state.clone());

        for _ in 0..DENIAL_LIMIT {
            let body = format!(
                r#"{{"run_id":{spender},"tool_name":"Bash","tool_input":{{"command":"rm -rf /"}}}}"#
            );
            decide(&app, &body).await;
        }

        assert!(!state.run_handles.lock().unwrap().contains_key(&spender));
        assert!(state.run_handles.lock().unwrap().contains_key(&bystander));
        let untouched: i64 = sqlx::query_scalar("SELECT denials FROM runs WHERE id = ?")
            .bind(bystander)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(untouched, 0);
    }

    /// Shadow mode denies everything that is not read-only — that is its whole job, not evidence of
    /// a run probing the gate. It returns before the classifier's verdict is ever counted.
    #[tokio::test]
    async fn a_shadow_runs_ordinary_refusals_do_not_spend_an_allowance() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "shadow", Some("proj-1"), None, None).await;
        let app = test_router(state.clone());

        for _ in 0..DENIAL_LIMIT + 2 {
            let body = format!(
                r#"{{"run_id":{run_id},"tool_name":"Edit","tool_input":{{"file_path":"a.txt"}}}}"#
            );
            assert_eq!(decide(&app, &body).await.decision, "deny");
        }

        assert!(
            state.run_handles.lock().unwrap().contains_key(&run_id),
            "a shadow run must survive doing exactly what shadow mode expects of it"
        );
    }

    /// The single most important assertion in this pillar: a triage run gets NO tool, of any kind.
    /// Barrier 1 means the CLI should never offer one — this is what happens if it does.
    #[tokio::test]
    async fn a_triage_run_is_denied_every_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let app = test_router(state);

        for (tool, input) in [
            ("Bash", serde_json::json!({"command": "ls -la"})),
            ("Read", serde_json::json!({"file_path": "/etc/passwd"})),
            (
                "Edit",
                serde_json::json!({"file_path": "a", "new_string": "b"}),
            ),
            (
                "Write",
                serde_json::json!({"file_path": "a", "content": "b"}),
            ),
            ("Grep", serde_json::json!({"pattern": "secret"})),
            ("Glob", serde_json::json!({"pattern": "**/*.env"})),
            ("mcp__nucleos__create_run", serde_json::json!({})),
            (
                "mcp__claude_ai_Google_Drive__create_file",
                serde_json::json!({}),
            ),
        ] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": tool,
                "tool_input": input,
            })
            .to_string();
            let decision = decide(&app, &body).await;
            assert_eq!(decision.decision, "deny", "{tool} must be denied");
            assert_eq!(decision.reason, "email triage runs have no tools");
        }
    }

    /// `ls -la` is the case that proves the fallthrough was real: the classifier calls it
    /// read-local and ALLOWS it, so before `mode` was resolved for out-of-flight runs, a triage run
    /// that had left `run_handles` was handed a shell.
    #[tokio::test]
    async fn a_triage_run_stays_denied_once_it_leaves_the_handle_map() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, crate::email::TRIAGE_MODE).await;
        let app = test_router(state);

        let body = serde_json::json!({
            "run_id": run_id,
            "tool_name": "Bash",
            "tool_input": {"command": "ls -la"},
        })
        .to_string();
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "email triage runs have no tools");
    }

    #[tokio::test]
    async fn an_out_of_flight_orchestrator_turn_is_denied_rather_than_classified() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, "assistant").await;
        let app = test_router(state);

        let body = serde_json::json!({
            "run_id": run_id,
            "tool_name": "Read",
            "tool_input": {"file_path": "/etc/passwd"},
        })
        .to_string();
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(
            decision.reason,
            "the orchestrator is restricted to NucleOS tools"
        );
    }

    /// The no-regression half of the lift: resolving `mode` outside the in-flight check must not
    /// have narrowed what a shadow run may do.
    #[tokio::test]
    async fn an_out_of_flight_shadow_run_still_allows_read_only_tools() {
        let state = test_state().await;
        let run_id = out_of_flight_run(&state, "shadow").await;
        let app = test_router(state);

        for tool in ["Read", "Grep", "Glob"] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": tool,
                "tool_input": {"file_path": "src/main.rs", "pattern": "fn"},
            })
            .to_string();
            let decision = decide(&app, &body).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// The scoreboard records decisions taken over live runs. A call naming a run that is no longer
    /// executing is not one, and counting it would quietly inflate the promotion gate's evidence.
    #[tokio::test]
    async fn an_out_of_flight_run_records_no_shadow_decision() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let shadow_id = out_of_flight_run(&state, "shadow").await;
        let worktree_id = out_of_flight_run(&state, "worktree").await;
        let app = test_router(state);

        for run_id in [shadow_id, worktree_id] {
            let body = serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "src/main.rs"},
            })
            .to_string();
            decide(&app, &body).await;
        }

        let recorded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM shadow_decisions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(recorded, 0);
    }

    #[tokio::test]
    async fn denies_rm_rf() {
        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"rm -rf /tmp/x"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "destructive deletion commands are denied");
    }

    #[tokio::test]
    async fn allows_safe_read_command() {
        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"ls -la"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "allow");
        assert_eq!(decision.reason, "recognized non-mutating shell command");
    }

    /// The hook's pending path, and the classifier's verdict that reaches it, asserted together so
    /// the two cannot drift apart. `echo hi` stood here until `classifier.rs` learned that saying
    /// something is not doing something; the command changed, the path under test did not.
    #[tokio::test]
    async fn pends_unrecognized_command() {
        let tool_input = serde_json::json!({"command": "frobnicate --hard"});
        let classification = classifier::classify(
            "Bash",
            &tool_input,
            None,
            &crate::github::Policy::empty(),
            &crate::project_policy::ShellRules::default(),
            classifier::Unrecognized::AsksAPerson,
        );
        assert_eq!(classification.action_class, "unrecognized");

        let app = test_router(test_state().await);
        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"frobnicate --hard"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");
    }

    /// **The night this whole chunk exists to stop losing.** A run of a project that declared
    /// `bash scripts/gates.sh` runs it; the identical command, from a run belonging to no project,
    /// parks at 2am waiting for somebody who is asleep.
    ///
    /// Both runs are in flight and differ in exactly one column — `runs.project_id` — so nothing
    /// but the project's own list can account for the two answers.
    #[tokio::test]
    async fn a_prefix_the_project_declared_runs_where_a_run_with_no_project_parks() {
        let state = test_state().await;
        crate::project_policy::declare_shell_rule(
            &state.pool,
            "alpha",
            "bash scripts/gates.sh",
            crate::project_policy::Verdict::Allow,
            None,
        )
        .await
        .unwrap();

        let declared = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        let no_project = in_flight_run(&state, "real", None, None, None).await;
        let app = test_router(state.clone());

        let asking = |run_id: i64| {
            format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"bash scripts/gates.sh core"}}}}"#
            )
        };

        let decision = decide(&app, &asking(declared)).await;
        assert_eq!(decision.decision, "allow");
        assert_eq!(
            decision.reason,
            "a prefix this project declared runs without asking"
        );

        let decision = decide(&app, &asking(no_project)).await;
        assert_eq!(decision.decision, "pending_approval");
    }

    /// The GitHub half of the same wiring: the policy a decision is taken under is the project's,
    /// not the daemon's.
    ///
    /// `test_state` ships `GithubRuntime::default()`, whose policy is autonomous in NOTHING — so the
    /// `allow` below cannot be coming from `.ai/github.yaml`, and the two runs differ in exactly one
    /// column again. Hand `classify` `state.github.policy` here instead of the layered one and this
    /// is the test that says so; the constructor could be perfect and the feature would still be
    /// unreachable, which is the failure this chunk has already fixed twice.
    ///
    /// The command carries no refused flag on purpose. `gh run list --limit 5` would park whatever
    /// the project declared, and a test that could not tell that apart from a broken call site would
    /// be no test at all.
    #[tokio::test]
    async fn an_operation_the_project_declared_reaches_github_where_a_run_with_no_project_parks() {
        let state = test_state().await;
        crate::project_policy::declare_github_op(&state.pool, "alpha", "run_list")
            .await
            .unwrap();

        let declared = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        let no_project = in_flight_run(&state, "real", None, None, None).await;
        let app = test_router(state.clone());

        let asking = |run_id: i64| {
            format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"gh run list -R owner/name"}}}}"#
            )
        };

        let decision = decide(&app, &asking(declared)).await;
        assert_eq!(decision.decision, "allow");
        assert_eq!(
            decision.reason,
            "structural GitHub reads on an autonomy list are allowed"
        );

        let decision = decide(&app, &asking(no_project)).await;
        assert_eq!(decision.decision, "pending_approval");

        // A project that declared nothing is not a project that declared this: same daemon, same
        // command, and the answer goes back to what it was before the row existed.
        let other = in_flight_run(&state, "real", Some("beta"), None, None).await;
        let decision = decide(&app, &asking(other)).await;
        assert_eq!(decision.decision, "pending_approval");
    }

    /// A declared read is still bound by the flags. The project said `run_status`; the map names
    /// `gh run view --log-failed` `run_status` too, until its guard reads `REFUSED_READ_FLAGS` — and
    /// what comes back is a failed step's log, which is a stranger's words.
    ///
    /// Here rather than only in `github.rs` because this is the door an agent actually types into,
    /// and the question is whether a project's declaration reaches it carrying its bounds.
    #[tokio::test]
    async fn a_declared_read_that_asks_for_a_strangers_words_parks_anyway() {
        let state = test_state().await;
        crate::project_policy::declare_github_op(&state.pool, "alpha", "run_status")
            .await
            .unwrap();

        let run_id = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        let app = test_router(state.clone());

        let asking = |command: &str| {
            format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"{command}"}}}}"#
            )
        };

        let decision = decide(&app, &asking("gh run view 1 -R owner/name")).await;
        assert_eq!(decision.decision, "allow");

        for refused in [
            "gh run view --log 1 -R owner/name",
            "gh run view --log-failed 1 -R owner/name",
        ] {
            let decision = decide(&app, &asking(refused)).await;
            assert_eq!(
                decision.decision, "pending_approval",
                "{refused:?} returns log text and asks a person"
            );
        }
    }

    /// The refusal direction. `ls -la` is a command the compiled list allows outright, so a project
    /// that denies `ls` is overriding a permission rather than filling a gap — the half of this
    /// feature that can only ever take something away.
    ///
    /// The last three lines are why `project_id` is NOT gated behind `is_in_flight` the way `cwd`
    /// is: the refusal has to hold for a run that has left `run_handles` too, which is the moment
    /// losing a project's `deny` would cost the most.
    #[tokio::test]
    async fn a_projects_refusal_overrides_a_compiled_permission() {
        let state = test_state().await;
        crate::project_policy::declare_shell_rule(
            &state.pool,
            "alpha",
            "ls",
            crate::project_policy::Verdict::Deny,
            None,
        )
        .await
        .unwrap();

        let refusing = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        let silent = in_flight_run(&state, "real", Some("beta"), None, None).await;
        let app = test_router(state.clone());

        let asking = |run_id: i64| {
            format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"ls -la"}}}}"#
            )
        };

        let decision = decide(&app, &asking(refusing)).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "this project denies this command");

        // A project that declared nothing is not a project that forbade everything.
        let decision = decide(&app, &asking(silent)).await;
        assert_eq!(decision.decision, "allow");

        state.run_handles.lock().unwrap().remove(&refusing);
        let decision = decide(&app, &asking(refusing)).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "this project denies this command");
    }

    /// A `deny` list nobody can read must not be spent as a free pass.
    ///
    /// The read is forced to fail by dropping the table out from under it, which is the only honest
    /// way to produce that error from outside `project_policy`: `shell_rules` returns `Err` for a
    /// database that will not answer, and a pool shared with feed appends and run-status writes
    /// makes SQLITE_BUSY an ordinary event rather than a theoretical one.
    ///
    /// `ls -la` is an `allow` the compiled list gives on its own, and this project may well have
    /// denied `ls` — nobody can say, so it costs an approval prompt. `rm -rf` is a compiled `deny`,
    /// and the downgrade has to leave it exactly where it is: turning a refusal into a prompt is
    /// offering somebody the chance to approve the very thing that was refused.
    #[tokio::test]
    async fn rules_that_cannot_be_read_cost_an_approval_and_never_an_allow() {
        let state = test_state().await;
        let asking_allow = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        let asking_deny = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        sqlx::query("DROP TABLE project_shell_rules")
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &format!(
                r#"{{"run_id":{asking_allow},"tool_name":"Bash","tool_input":{{"command":"ls -la"}}}}"#
            ),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let decision = decide(
            &app,
            &format!(
                r#"{{"run_id":{asking_deny},"tool_name":"Bash","tool_input":{{"command":"rm -rf /tmp/x"}}}}"#
            ),
        )
        .await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "destructive deletion commands are denied");
    }

    /// **An approval taken out earlier is not a key to a refusal nobody can read.**
    ///
    /// `downgrade_if_unreadable` is the first thing in this codebase to answer `pending_approval`
    /// carrying an allow-only class, and that is what puts `read-local` in front of the grant
    /// lookup at all — every other producer of that class answers `allow`, and the grant table is
    /// only consulted for a `pending_approval`. Ungated, one earlier approval would buy the rest of
    /// the run every `ls`, `cat`, `git status` and `cargo test` it liked, for as long as the table
    /// stayed unreadable, and the downgrade's own reason string would be false on its face.
    ///
    /// The grant is minted BEFORE the table goes, which is the only way this is reachable at all:
    /// `resume_approved_run` records no class when its own read fails, so one unbroken outage mints
    /// nothing. An intermittent `SQLITE_BUSY` — which this file already calls an ordinary event
    /// rather than a theoretical one — is exactly the pattern that arrives here.
    ///
    /// The last assertion is what makes this about the gate rather than about an expired grant: the
    /// grant is still live at the moment the command is refused, and still covers the class.
    #[tokio::test]
    async fn a_grant_does_not_survive_rules_that_cannot_be_read() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        proposals::grant_action(&state.pool, run_id, "Bash", Some("read-local"), 1)
            .await
            .unwrap();
        let app = test_router(state.clone());
        let asking = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"ls -la"}}}}"#
        );

        // While the table answers, the command simply runs and the grant is never reached.
        assert_eq!(decide(&app, &asking).await.decision, "allow");

        sqlx::query("DROP TABLE project_shell_rules")
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(decide(&app, &asking).await.decision, "pending_approval");
        assert!(
            proposals::grant_covers_class(&state.pool, run_id, "read-local")
                .await
                .unwrap(),
            "the grant was live and covered the class — the gate is what refused, not an expiry"
        );
    }

    /// **An outage on a table these calls never consult must not stop them.**
    ///
    /// `classify` reads the rules in exactly one place, `classify_shell_command`, and every branch
    /// above it returns first — so for a `Read`, a `Grep`, a `Glob` or a subagent the project's two
    /// lists cannot change the verdict at all. Loading them anyway made the FAILURE of that read
    /// rewrite the answer: a transient `SQLITE_BUSY` parked a run on its next file read, which is
    /// this chunk's own autonomy loss arriving through a different door. It fails closed, so it was
    /// never a safety hole; it is a night lost the same way.
    ///
    /// The filter is `classifier::reads_shell_rules`, which is the same list the classifier's own
    /// branch is written from, so this cannot become a call that skips a load the classifier then
    /// needs.
    #[tokio::test]
    async fn an_outage_on_the_rules_never_parks_a_call_they_could_not_reach() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", Some("alpha"), None, None).await;
        sqlx::query("DROP TABLE project_shell_rules")
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        for (tool, input) in [
            ("Read", serde_json::json!({"file_path": "src/main.rs"})),
            ("Grep", serde_json::json!({"pattern": "fn main"})),
            ("Glob", serde_json::json!({"pattern": "**/*.rs"})),
            ("Task", serde_json::json!({"prompt": "look something up"})),
        ] {
            let decision = decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": tool,
                    "tool_input": input,
                })
                .to_string(),
            )
            .await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// A conversation is not an exemption from what the project wrote down. The rooted branch
    /// returns before the sibling classifier call, so it needs its own wiring and its own test —
    /// severing it (passing `None` for the project) left every other test in this file green.
    ///
    /// The pair is the assertion: the same command, in the same kind of turn, refused for the
    /// project that denied it and allowed for the one that did not.
    #[tokio::test]
    async fn a_rooted_turn_is_bound_by_its_projects_refusals() {
        let state = test_state().await;
        crate::project_policy::declare_shell_rule(
            &state.pool,
            "alpha",
            "ls",
            crate::project_policy::Verdict::Deny,
            None,
        )
        .await
        .unwrap();

        let refusing = rooted_turn_run(&state, "C:/Projects/nucleos").await;
        sqlx::query("UPDATE runs SET project_id = 'alpha' WHERE id = ?")
            .bind(refusing)
            .execute(&state.pool)
            .await
            .unwrap();
        let silent = rooted_turn_run(&state, "C:/Projects/nucleos").await;
        let app = test_router(state.clone());

        let asking = |run_id: i64| {
            format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"ls -la"}}}}"#
            )
        };

        let decision = decide(&app, &asking(refusing)).await;
        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, "this project denies this command");

        assert_eq!(decide(&app, &asking(silent)).await.decision, "allow");
    }

    /// The GitHub half of the same sentence, on the same branch, and it was pinned by NOTHING until
    /// this test.
    ///
    /// `rooted_decision` returns before the sibling classifier call, so it carries its own copy of
    /// the wiring — and reverting its `policy` argument to `state.github.policy` left this whole file
    /// green at `96 passed; 0 failed`. The twin above exists because severing the SHELL rules left
    /// every other test green; the identical hole sat open beside it for the policy, opened by the
    /// very commit that wrote the paragraph justifying the wiring. A constructor can be perfect and
    /// the feature still unreachable, and that is the failure this chunk has now fixed three times.
    ///
    /// The pair is the assertion, as it is above. `test_state` ships `GithubRuntime::default()`,
    /// autonomous in nothing, so the `allow` can only be the project's own row — and the turn naming
    /// no project asks a person about the identical line.
    ///
    /// `asking` and not `pending_approval`, because this branch never parks: a conversation has
    /// somebody sitting in front of it, so an unrecognised line goes back to them as a question.
    #[tokio::test]
    async fn a_rooted_turn_reaches_github_through_its_projects_declared_operations() {
        let state = test_state().await;
        crate::project_policy::declare_github_op(&state.pool, "alpha", "run_list")
            .await
            .unwrap();

        let declared = rooted_turn_run(&state, "C:/Projects/nucleos").await;
        sqlx::query("UPDATE runs SET project_id = 'alpha' WHERE id = ?")
            .bind(declared)
            .execute(&state.pool)
            .await
            .unwrap();
        let no_project = rooted_turn_run(&state, "C:/Projects/nucleos").await;
        let app = test_router(state.clone());

        let asking = |run_id: i64| {
            format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"gh run list -R owner/name"}}}}"#
            )
        };

        let decision = decide(&app, &asking(declared)).await;
        assert_eq!(decision.decision, "allow");
        assert_eq!(
            decision.reason,
            "structural GitHub reads on an autonomy list are allowed"
        );

        assert_eq!(decide(&app, &asking(no_project)).await.decision, "asking");

        // And the flags still bind here. A rooted turn is the owner working on their own project,
        // not an exemption from what `--log-failed` brings back.
        let refused = format!(
            r#"{{"run_id":{declared},"tool_name":"Bash","tool_input":{{"command":"gh run view --log-failed 1 -R owner/name"}}}}"#
        );
        assert_eq!(decide(&app, &refused).await.decision, "asking");
    }

    #[tokio::test]
    async fn git_push_pends_approval_and_terminates_the_run() {
        let state = test_state().await;

        // Stand up a fake in-flight run: a runs row plus a live task whose abort handle is registered
        // under the same id (that's the `run_id` the hook will send).
        let run_id = in_flight_run(&state, "real", None, None, None).await;

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"git push origin main"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");
        assert_eq!(
            decision.reason,
            "push, merge, deploy, publish, and tag actions require approval"
        );

        // The run was actively terminated into awaiting_approval, and its handle removed (spec §8.4).
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    /// The trap in spec §7's cancellation sweep, made a test rather than a comment. `pause_for_approval`
    /// drives the run through the same `finalize_termination` a cancel uses, but the run it produces
    /// **resumes**: sweeping its queued requests would cancel the very merge it paused to have approved,
    /// and the human would then approve a request that no longer exists.
    ///
    /// It has to be driven through the handler rather than asserted on the predicate, because a
    /// predicate test cannot see a wrong argument at the call site — which is the only place this can
    /// actually go wrong.
    #[tokio::test]
    async fn a_run_paused_for_approval_keeps_the_merge_it_asked_for() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", None, None, None).await;
        let request = crate::vcs::submit(
            &state.pool,
            &crate::vcs::ResolvedRepo::synthetic("proj-1", "C:/repo", "proj-1"),
            &crate::vcs::Op::Merge {
                source: "feat/x".into(),
                target: "master".into(),
            },
            crate::vcs::Origin::Run(run_id),
        )
        .await
        .unwrap();

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"git push origin main"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        let request_status: String =
            sqlx::query_scalar("SELECT status FROM vcs_requests WHERE id = ?")
                .bind(request)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            request_status, "awaiting_approval",
            "the run resumes, so the merge it is pausing to have approved must still be there"
        );
    }

    #[tokio::test]
    async fn edit_to_autopilot_config_pends_approval_and_terminates_the_run() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", None, Some("C:\\work\\repo"), None).await;

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Edit","tool_input":{{"file_path":".ai/autopilot.yaml"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "pending_approval");
        assert_eq!(
            decision.reason,
            "changes to autopilot governance files require approval"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    /// Terminating the run kills the CLI whose hook script owns the very connection this handler is
    /// serving, and that script gives up after 5s anyway (`ask_daemon.py`'s `timeout=5`). Either way
    /// the request can vanish mid-handler, and a dropped request drops the handler future exactly the
    /// way `abort()` does — so everything sequenced after the termination is lost.
    ///
    /// The loss is unrecoverable, not merely untidy: a run parked in `awaiting_approval` with no
    /// proposal can be neither approved nor rejected, and it holds one of the project's concurrency
    /// slots for as long as it sits there — the sweep spares that status, because a run with a
    /// pending proposal is resumable. Startup recovery does not help either: it only reconciles rows
    /// left `running`.
    #[tokio::test]
    async fn a_dropped_hook_request_still_records_the_approval_proposal() {
        use std::future::Future;

        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", Some("proj-1"), None, None).await;

        let mut handler = Box::pin(pretooluse_decision(
            State(state.clone()),
            // What the middleware would have inserted for this run's own key.
            Extension(Scope::Run(run_id)),
            Json(PreToolUsePayload {
                run_id,
                tool_name: "Bash".to_owned(),
                tool_input: serde_json::json!({"command": "git push origin main"}),
            }),
        ));

        // Drive the handler by hand so the request can be dropped at a chosen point: the instant the
        // irreversible half is done. Removing the abort handle is that point of no return — it is the
        // arbiter that decides this call owns the termination, and it runs before any recording work.
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        let mut terminated = false;
        for _ in 0..10_000 {
            assert!(
                handler.as_mut().poll(&mut context).is_pending(),
                "the handler ran to completion before the request could be dropped"
            );
            if !state.run_handles.lock().unwrap().contains_key(&run_id) {
                terminated = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(terminated, "the handler never terminated the run");
        drop(handler);

        for _ in 0..100 {
            if !proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a paused run with no proposal is stuck forever and blocks its project"
        );
        assert_eq!(pending[0].run_id, Some(run_id));
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
    }

    #[tokio::test]
    async fn cwd_dependent_delete_outside_workspace_denies_through_the_handler() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "real", None, Some("C:\\work\\repo"), None).await;

        let app = test_router(state.clone());
        let body = format!(
            r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"rm ../outside/x"}}}}"#
        );
        let decision = decide(&app, &body).await;
        assert_eq!(decision.decision, "deny");
        // The class this delete answers to is the one about the boundary, not the one about `rm
        // -rf`'s shape — which is what this test was always demonstrating and what the reason now
        // says out loud.
        assert_eq!(
            decision.reason,
            "deletions that reach outside the workspace are denied"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_allows_and_records_an_ordinary_edit() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let tool_input = serde_json::json!({"file_path": "src/ordinary.rs"});
        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": tool_input
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "allow");

        let row: (String, String, String) = sqlx::query_as(
            "SELECT decision, action_class, tool_input FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Edit'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "allow");
        assert_eq!(row.1, "read-local");
        assert_eq!(row.2, tool_input.to_string());

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_denies_and_records_a_destructive_delete_without_terminating() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf target"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let row: (String, String) = sqlx::query_as(
            "SELECT decision, action_class FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Bash'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "deny");
        assert_eq!(row.1, "destructive");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn worktree_pends_and_terminates_on_git_push_and_records_it() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let row: (String, String) = sqlx::query_as(
            "SELECT decision, action_class FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Bash'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "pending_approval");
        assert_eq!(row.1, "push-merge-deploy");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        assert!(!state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn shadow_read_only_brake_records_would_decisions_without_terminating() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "shadow", None, Some("C:\\work\\repo"), None).await;
        let app = test_router(state.clone());

        let edit_input = serde_json::json!({"file_path": "src/ordinary.rs"});
        let edit = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": edit_input
            })
            .to_string(),
        )
        .await;
        assert_eq!(edit.decision, "deny");

        let row: (String, String, String) = sqlx::query_as(
            "SELECT decision, action_class, tool_input FROM shadow_decisions
             WHERE run_id = ? AND tool_name = 'Edit'",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "allow");
        assert_eq!(row.1, "read-local");
        assert_eq!(row.2, edit_input.to_string());

        let read = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "src/lib.rs"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(read.decision, "allow");

        let push = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(push.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(status, "awaiting_approval");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
    }

    #[tokio::test]
    async fn git_push_pause_creates_a_pending_action_approval_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        let proposal = &pending[0];
        assert_eq!(proposal.run_id, Some(run_id));
        assert_eq!(proposal.tool_name.as_deref(), Some("Bash"));
        assert_eq!(proposal.session_id.as_deref(), Some("sess-x"));
        assert_eq!(proposal.project_id.as_deref(), Some("proj"));
        assert_eq!(proposal.status, "pending");
        assert!(!proposal.reasoning.is_empty());
    }

    /// A job's node takes the other road out of the same door.
    ///
    /// The item is put down, the job is left alone to carry on, and the record is a `skipped-item`
    /// rather than an `action-approval` — which is what stops `list_pending` offering it as
    /// something to approve, since approving it would resume nothing.
    #[tokio::test]
    async fn a_jobs_node_skips_its_item_instead_of_parking_the_job() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        nobody_asked_for_this_job(&state.pool, job_id).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let item: (String, i64) =
            sqlx::query_as("SELECT status, ordinal FROM job_items WHERE job_id = ? AND run_id = ?")
                .bind(job_id)
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(item.0, "skipped", "the item must not be left running");

        let job_status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            job_status, "implementing",
            "the job itself is untouched — it has a queue to get on with"
        );

        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(kinds, vec!["skipped-item"]);
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty(),
            "a skipped item is a note, not something to approve — approving it resumes nothing"
        );

        // And the half the first version of this test did not ask about, which is what let the
        // whole thing stop anyway. `job::node_awaiting_approval` parks a job when ANY of its runs
        // is `awaiting_approval`, so a skipped item whose run keeps that status trades one stop for
        // another: the item reads `skipped` and the job waits on it for ever.
        let run_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            run_status, "interrupted",
            "the run must not keep asking after the answer was given"
        );
        let still_parked: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM runs WHERE job_id = ? AND status = 'awaiting_approval' LIMIT 1",
        )
        .bind(job_id)
        .fetch_optional(&state.pool)
        .await
        .unwrap();
        assert!(
            still_parked.is_none(),
            "this is the query `job::node_awaiting_approval` runs; a hit here parks the whole job"
        );
    }

    /// And the counterpart, which is the one that would go wrong quietly: a run with no `job_id`
    /// keeps the behaviour it has always had. This is the test that fails if the branch above is
    /// ever widened past the condition it was written for.
    #[tokio::test]
    async fn a_run_that_belongs_to_no_job_still_parks_and_asks() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(kinds, vec!["action-approval"]);
    }

    /// The 2026-08-27 decision, and the failure that produced it. Two overnight runs asked for a
    /// tool this file has never reasoned about, were parked, and spent the night holding a
    /// concurrency slot while a proposal nobody could answer sat in the queue. Hours of work each.
    ///
    /// The run has to still be running afterwards — that is the entire property — so the status is
    /// asserted as well as the verdict.
    #[tokio::test]
    async fn an_unattended_run_is_refused_an_unrecognized_tool_instead_of_being_parked() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-x"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "WebSearch",
                "tool_input": {"query": "anything"}
            })
            .to_string(),
        )
        .await;

        assert_eq!(decision.decision, "deny");
        assert!(
            decision.reason.contains("WebSearch"),
            "the refusal must name the tool so the model knows what to stop reaching for: {}",
            decision.reason
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status, "running",
            "the run must carry on with the tools it has"
        );
        let proposals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proposals")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            proposals, 0,
            "nobody is awake to answer a proposal about a tool name"
        );
    }

    /// The narrowing, asserted. Only `unrecognized` became a refusal; an action a person genuinely
    /// has to decide about still parks and still asks. Without this test the change above reads as
    /// "autonomous runs stopped asking", which is the opposite of what was decided.
    #[tokio::test]
    async fn an_unattended_run_still_parks_for_an_action_a_person_must_decide() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-y"),
        )
        .await;
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");
    }

    /// The narrowing's sharper edge, and the one two job-node tests found first. An unrecognized
    /// COMMAND is not an unrecognized TOOL: `git branch -D`, `cargo fix`, `gh run list` and a bare
    /// shell loop all classify `pending_approval`, and every one of them is an action a person has
    /// to decide about. Refusing those would have quietly narrowed what the owner governs, which is
    /// the opposite of what was asked for.
    #[tokio::test]
    async fn an_unattended_run_still_parks_for_a_command_nobody_has_reasoned_about() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-w"),
        )
        .await;
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git branch -D feature"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status, "awaiting_approval",
            "deleting a branch is a decision, and the owner is the one who takes it"
        );
    }

    /// The other half of the same decision: a subagent is no longer an unrecognized tool, so it is
    /// neither parked nor refused. `classifier.rs` holds the reasoning; this asserts the hook agrees,
    /// because the classifier being right about `Agent` buys nothing if the run still stops here.
    #[tokio::test]
    async fn an_unattended_run_may_start_a_subagent() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-z"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Agent",
                "tool_input": {"prompt": "read the spec and report"}
            })
            .to_string(),
        )
        .await;

        assert_eq!(decision.decision, "allow");
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
    }

    /// The node of a job that owns no item: the plan, which runs before the queue exists.
    ///
    /// Found by dogfooding the change above on 2026-08-07 — the plan node asked to run `find`, took
    /// the job road because its run has a `job_id`, matched no item, and returned before writing any
    /// proposal. The run sat in `awaiting_approval` with nothing to approve, which is precisely the
    /// state `pause_for_approval`'s rollback exists to prevent, and which holds one of the
    /// project's concurrency slots until a restart notices.
    ///
    /// The queue is what the plan produces, so there is nothing to skip and nothing to carry on to.
    /// It parks and asks, like any other run — and it is the ONLY node that still does. A review
    /// node owns no item either and stopped taking this road; see the test below for why.
    #[tokio::test]
    async fn a_jobs_plan_node_owns_no_item_so_it_parks_and_asks() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        // What makes it a plan node: the queue does not exist yet.
        sqlx::query("DELETE FROM job_items WHERE job_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET stage = 'plan' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a parked node with no proposal can be neither approved nor rejected, and blocks the \
             project until the daemon restarts"
        );
        assert_eq!(pending[0].kind, "action-approval");
        assert_eq!(pending[0].run_id, Some(run_id));
    }

    /// A review node abandons the review rather than parking the job on it.
    ///
    /// It owns no item, so it used to take the plan node's road and park — and the cost of that is
    /// the whole night, not one opinion. Its verdict is advisory (§5.5 gives ship/no-ship to the
    /// gate), it runs after every item is written, gated and checkpointed, and it changes nothing
    /// itself. So the job loses a review and finishes, which is the trade the right way round.
    ///
    /// Measured twice before it was changed: job 12 on 2026-08-08 parked its review node on
    /// `git reflog` and job 13 parked its own on a `for` loop, each with all the real work already
    /// done. Both sat until a person cancelled them.
    ///
    /// The run must end TERMINAL, and that is the whole mechanism — `load_view` already reads any
    /// finished review as `ReviewState::Done` ("a review that failed is still a review that
    /// happened"), so the round closes with no change to the state machine at all.
    /// The replan node too, and for the same reason with a different ending behind it.
    ///
    /// Its run ending non-successfully hands the job to `stop_after_replan`, which stops it
    /// `Stopped` and not `Failed` — precisely so the rounds that already ran stay worth looking at.
    /// That ending existed before this; all that was missing is the node reaching it.
    ///
    /// Measured on 2026-08-08: jobs 14 and 16 both passed every item, closed their round, spawned
    /// their replan node — and both parked it, one on a `for` loop and one on `find`. Two jobs that
    /// had done all their work sat holding a slot each.
    #[tokio::test]
    async fn a_jobs_replan_node_gives_up_instead_of_parking_the_job() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        nobody_asked_for_this_job(&state.pool, job_id).await;
        sqlx::query("UPDATE job_items SET status = 'passed', run_id = NULL WHERE job_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET stage = 'replan' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                // A shell loop, which the classifier reads as a program rather than a line and so
                // does not recognise. NOT `find`, which it allows — an allowed command never
                // reaches this road, and the run would read terminal for the wrong reason.
                "tool_input": {"command": "for f in *.py; do cat \"$f\"; done"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(
            status, "awaiting_approval",
            "a parked replan leaves a job that finished all its work reading live and doing nothing"
        );
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(kinds, vec!["skipped-item"]);
        // The rounds that ran are what `stop_after_replan` keeps, so nothing may be reverted here.
        let item_statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert!(item_statuses.iter().all(|status| status == "passed"));
    }

    #[tokio::test]
    async fn a_jobs_review_node_gives_up_the_review_instead_of_parking_the_job() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        nobody_asked_for_this_job(&state.pool, job_id).await;
        // What makes it a review node: it owns no running item, and its stage says so.
        sqlx::query("UPDATE job_items SET status = 'passed', run_id = NULL WHERE job_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET stage = 'review' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "for f in *.py; do cat \"$f\"; done"}
            })
            .to_string(),
        )
        .await;

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_ne!(
            status, "awaiting_approval",
            "a review left parked stops the job it was only ever going to comment on"
        );

        // Nothing pending: `list_pending` filters `skipped-item` out, because it is a note rather
        // than a queue — work NOT done waiting on a decision, not work done waiting to be read.
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            kinds,
            vec!["skipped-item"],
            "what it asked for is still on the record for the morning"
        );

        // And the items it was going to read are untouched. A review node owns no item, so a revert
        // here would target whichever ran last and throw away that item's checkpoint.
        let item_statuses: Vec<String> =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? ORDER BY ordinal")
                .bind(job_id)
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert!(item_statuses.iter().all(|status| status == "passed"));
    }

    /// The write that must not be lost: an item left `running` in a job nobody drives makes
    /// `next_step` answer `Wait` for ever, and no later pass rescues it.
    ///
    /// Exercised by taking away everything the skip could stumble on — no worktree row, so no
    /// revert is possible — and demanding the mark survive anyway.
    #[tokio::test]
    async fn the_item_is_marked_even_when_nothing_else_about_the_skip_can_happen() {
        let state = test_state().await;
        let (job_id, run_id) = in_flight_job_node(&state).await;
        nobody_asked_for_this_job(&state.pool, job_id).await;
        sqlx::query("DELETE FROM worktrees WHERE owner_kind = 'job' AND owner_id = ?")
            .bind(job_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "git push origin main"}
            })
            .to_string(),
        )
        .await;

        let status: String =
            sqlx::query_scalar("SELECT status FROM job_items WHERE job_id = ? AND run_id = ?")
                .bind(job_id)
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            status, "skipped",
            "a job with no worktree still must not be left holding a running item"
        );
    }

    /// An in-flight turn of a conversation ROOTED in `root` — one continuing a session had in the
    /// IDE. Returns the run id.
    ///
    /// Writes NO `permission_mode`, deliberately: every caller of this therefore also pins that a
    /// run whose column is NULL is governed as `auto`, which is what a rooted conversation did
    /// before the column existed and what an older row still holds.
    async fn rooted_turn_run(state: &AppState, root: &str) -> i64 {
        let chat_id = crate::chats::create(
            &state.pool,
            crate::chats::Brain::Cloud,
            Some(&crate::sessions::had_in(root, "had-in-the-ide")),
        )
        .await
        .unwrap();
        let run_id = in_flight_run(state, "assistant", None, None, None).await;
        sqlx::query("UPDATE runs SET chat_id = ? WHERE id = ?")
            .bind(&chat_id)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        run_id
    }

    /// The same turn, started on a named rung.
    ///
    /// The run's own column and not the chat's, which is the whole point of there being two: the
    /// hook reads what this turn STARTED with, so that moving the selector mid-turn cannot change
    /// the rules underneath a turn already running.
    async fn rooted_turn_on(
        state: &AppState,
        root: &str,
        permission: crate::chats::PermissionMode,
    ) -> i64 {
        let run_id = rooted_turn_run(state, root).await;
        sqlx::query("UPDATE runs SET permission_mode = ? WHERE id = ?")
            .bind(permission.as_str())
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        run_id
    }

    /// The four probes the ladder is measured with, each one a different `action_class`.
    fn probe(run_id: i64, tool: &str, input: serde_json::Value) -> String {
        serde_json::json!({ "run_id": run_id, "tool_name": tool, "tool_input": input }).to_string()
    }

    /// `manual` asks before anything changes, and before nothing else.
    ///
    /// The second half is the load-bearing one and it is why this rung is built on `action_class`
    /// rather than on `only_reads`: that list holds five TOOLS and `Bash` is deliberately outside
    /// it, so a rung built on its negation asks about `ls`, `git status` and `cargo check` too —
    /// five to fifteen questions a turn, each blocking its own hook call. `git status` coming back
    /// `allow` is what says this rung was not built that way.
    #[tokio::test]
    async fn manual_asks_before_an_edit_and_never_before_a_read_or_a_status() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Manual,
        )
        .await;

        assert_eq!(
            decide(
                &app,
                &probe(run_id, "Read", serde_json::json!({"file_path": "a.rs"}))
            )
            .await
            .decision,
            "allow",
            "reading changes nothing"
        );
        assert_eq!(
            decide(
                &app,
                &probe(run_id, "Bash", serde_json::json!({"command": "git status"}))
            )
            .await
            .decision,
            "allow",
            "a recognised non-mutating command changes nothing either — this is the half that \
             stops the rung being rebuilt on `only_reads`"
        );
        assert_eq!(
            decide(
                &app,
                &probe(
                    run_id,
                    "Write",
                    serde_json::json!({"file_path": "C:/Projects/nucleos/a.rs", "content": "x"})
                )
            )
            .await
            .decision,
            "asking",
            "an edit is exactly what this rung promises to be asked about"
        );
    }

    /// `accept_edits` adds the edits and stops there.
    ///
    /// The pair is the test. The classifier already answers `allow` for `Edit` and `Write` inside
    /// the workspace — they share the `read-local` branch with the reads — so a rung that left the
    /// verdict alone would be `auto` wearing another name, and only the SECOND assertion catches
    /// that. `git add -A` is `allow`/`vcs-local`: a class `auto` runs and this rung does not.
    #[tokio::test]
    async fn accept_edits_adds_the_edits_and_still_asks_about_the_class_above_them() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let editing = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::AcceptEdits,
        )
        .await;
        let auto = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Auto,
        )
        .await;

        let write = |run_id| {
            probe(
                run_id,
                "Write",
                serde_json::json!({"file_path": "C:/Projects/nucleos/a.rs", "content": "x"}),
            )
        };
        let staging = |run_id| probe(run_id, "Bash", serde_json::json!({"command": "git add -A"}));

        assert_eq!(decide(&app, &write(editing)).await.decision, "allow");
        assert_eq!(
            decide(&app, &staging(editing)).await.decision,
            "asking",
            "a class beyond the edits must still be asked about, or this rung is `auto`"
        );
        assert_eq!(
            decide(&app, &staging(auto)).await.decision,
            "allow",
            "and `auto` must run the very thing the rung above asked about, or the pair proves \
             nothing"
        );
    }

    /// `bypass` runs what the rules would have asked about. That is the whole of what it buys.
    #[tokio::test]
    async fn bypass_runs_what_the_rules_would_have_asked_about() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let asking = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Auto,
        )
        .await;
        let running = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Bypass,
        )
        .await;

        let unrecognised = |run_id| {
            probe(
                run_id,
                "Bash",
                serde_json::json!({"command": "npm install"}),
            )
        };

        assert_eq!(decide(&app, &unrecognised(asking)).await.decision, "asking");
        assert_eq!(decide(&app, &unrecognised(running)).await.decision, "allow");
    }

    /// The one thing `bypass` asks about, and it is not a technicality.
    ///
    /// `rm -rf target` is the case this whole mode was argued from: 11.8 GB of a stale build
    /// directory that somebody goes to a terminal to delete by hand, which is what the asking window
    /// exists to end. The scope is wider than the friendly example, and that is stated rather than
    /// discovered — ANY `rm -rf` inside the workspace becomes a question here, `rm -rf ./core/src`
    /// included.
    #[tokio::test]
    async fn bypass_still_asks_about_a_delete_inside_the_workspace() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Bypass,
        )
        .await;

        for command in ["rm -rf target", "rm -rf ./core/src"] {
            let decision = decide(
                &app,
                &probe(run_id, "Bash", serde_json::json!({"command": command})),
            )
            .await;
            assert_eq!(decision.decision, "asking", "{command}");
            assert_eq!(
                decision.reason, BYPASS_STILL_ASKS,
                "{command}: the question must not be labelled with the refusal it replaced"
            );
        }
    }

    /// What `bypass` never lowers: where a command POINTS, and what somebody DECLARED.
    ///
    /// `rm -rf /` is the sharp one. It matches `"rm -rf"` in the blind phrase list too, so a split
    /// of the destructive family that kept the code's original disjunct order would have labelled it
    /// `destructive` and let this mode turn it into a question somebody can say yes to. It comes
    /// back `destructive-outside` because the target is asked about first.
    ///
    /// `destructive-outside` is also a class the lowering `match` does not name, so this is the
    /// assertion that an unnamed `deny` family falls through to a refusal rather than to a widening
    /// — whoever adds a fourth one does not have to know this code exists for it to fail safely.
    #[tokio::test]
    async fn bypass_never_lowers_a_refusal_about_where_a_command_points_or_what_a_project_declared()
    {
        let state = test_state().await;
        crate::project_policy::declare_shell_rule(
            &state.pool,
            "alpha",
            "npm ci",
            crate::project_policy::Verdict::Deny,
            None,
        )
        .await
        .unwrap();
        let run_id = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Bypass,
        )
        .await;
        sqlx::query("UPDATE runs SET project_id = 'alpha' WHERE id = ?")
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        let app = test_router(state.clone());

        for (label, tool, input) in [
            (
                "a delete that reaches outside the workspace",
                "Bash",
                serde_json::json!({"command": "rm -rf /"}),
            ),
            (
                "a write outside the workspace",
                "Write",
                serde_json::json!({"file_path": "C:/Windows/System32/x", "content": "x"}),
            ),
            (
                "a command this project denied",
                "Bash",
                serde_json::json!({"command": "npm ci"}),
            ),
        ] {
            assert_eq!(
                decide(&app, &probe(run_id, tool, input)).await.decision,
                "deny",
                "{label}"
            );
        }
    }

    /// The single assertion that separates the correct implementation from the tempting one.
    ///
    /// Lowering the verdict and letting control fall through passes the third-party-text barrier by
    /// construction. Jumping from the `deny` return straight to `ask_about` — which reads like the
    /// same change and is shorter — steps OVER that barrier, for precisely the family this mode
    /// lowers: a turn holding a stranger's words would be asked about `rm -rf` instead of refused,
    /// and a person could say yes.
    #[tokio::test]
    async fn bypass_over_a_turn_that_read_third_party_text_is_refused_and_not_asked() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Bypass,
        )
        .await;
        crate::runs::mark_untrusted_context(&state.pool, run_id)
            .await
            .unwrap();

        let decision = decide(
            &app,
            &probe(
                run_id,
                "Bash",
                serde_json::json!({"command": "rm -rf target"}),
            ),
        )
        .await;

        assert_eq!(decision.decision, "deny");
        assert_eq!(decision.reason, UNTRUSTED_CONTEXT_DENY_REASON);
    }

    /// A refusal stays a refusal on every rung below `bypass` — including `accept_edits`, the one
    /// most likely to be written as a union of allowed sets laid over the `deny` return.
    #[tokio::test]
    async fn the_rungs_below_bypass_leave_a_refusal_a_refusal() {
        let state = test_state().await;
        let app = test_router(state.clone());

        for permission in [
            crate::chats::PermissionMode::Manual,
            crate::chats::PermissionMode::AcceptEdits,
            crate::chats::PermissionMode::Plan,
            crate::chats::PermissionMode::Auto,
        ] {
            let run_id = rooted_turn_on(&state, "C:/Projects/nucleos", permission).await;
            assert_eq!(
                decide(
                    &app,
                    &probe(
                        run_id,
                        "Bash",
                        serde_json::json!({"command": "rm -rf target"})
                    )
                )
                .await
                .decision,
                "deny",
                "{permission:?}"
            );
        }
    }

    /// The reason the run carries a column of its own.
    ///
    /// The hook reads the turn's SNAPSHOT, not the conversation's current setting. Without that,
    /// moving the selector while a turn is running would change the rules underneath a turn already
    /// running — and the hook reads this once per tool call, minutes after the turn began.
    #[tokio::test]
    async fn the_hook_reads_the_turns_snapshot_and_not_the_conversations_current_setting() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_on(
            &state,
            "C:/Projects/nucleos",
            crate::chats::PermissionMode::Manual,
        )
        .await;

        // Somebody moves the selector to the widest rung while the turn is mid-answer.
        let chat_id: String = sqlx::query_scalar("SELECT chat_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        crate::chats::set_permission_mode(
            &state.pool,
            &chat_id,
            crate::chats::PermissionMode::Bypass,
        )
        .await
        .unwrap();

        assert_eq!(
            decide(
                &app,
                &probe(
                    run_id,
                    "Write",
                    serde_json::json!({"file_path": "C:/Projects/nucleos/a.rs", "content": "x"})
                )
            )
            .await
            .decision,
            "asking",
            "this turn started on `manual` and finishes on it"
        );
    }

    /// The whole point of the rooted branch: an ordinary orchestrator turn is denied a `Read`, and
    /// this one is not. Asserted as a PAIR, because the interesting claim is the difference — either
    /// alone would still pass if the branch stopped being reached.
    #[tokio::test]
    async fn only_a_rooted_turn_may_read_the_machine() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let plain = in_flight_run(&state, "assistant", None, None, None).await;
        let rooted = rooted_turn_run(&state, "C:/Projects/nucleos").await;

        let read = |run_id: i64| {
            format!(
                r#"{{"run_id":{run_id},"tool_name":"Read","tool_input":{{"file_path":"a.rs"}}}}"#
            )
        };
        assert_eq!(decide(&app, &read(plain)).await.decision, "deny");
        assert_eq!(decide(&app, &read(rooted)).await.decision, "allow");
    }

    /// The NucleOS tools keep the door they always had, so there is one implementation of the
    /// untrusted-read marking and not two.
    #[tokio::test]
    async fn a_rooted_turn_still_reaches_the_nucleos_tools_the_same_way() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_run(&state, "C:/Projects/nucleos").await;

        let decision = decide(
            &app,
            &format!(
                r#"{{"run_id":{run_id},"tool_name":"mcp__nucleos__list_runs","tool_input":{{}}}}"#
            ),
        )
        .await;
        assert_eq!(decision.decision, "allow");
    }

    /// A conversation may touch the project it is about, and not the rest of the disk. The
    /// classifier already enforces this — what this fixes is that it is now given a workspace to
    /// enforce it against.
    #[tokio::test]
    async fn a_rooted_turn_may_not_write_outside_the_project_it_continues() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_run(&state, "C:/Projects/nucleos").await;

        let decision = decide(
            &app,
            &format!(
                r#"{{"run_id":{run_id},"tool_name":"Write","tool_input":{{"file_path":"C:/Windows/System32/drivers/etc/hosts","content":"x"}}}}"#
            ),
        )
        .await;
        assert_eq!(decision.decision, "deny");
    }

    /// ASKED about, and still NOT parked. The two are separate facts and both matter.
    ///
    /// Parking mints a proposal that expects a worktree run to resume into, and a conversation has
    /// none — the turn would die owing an approval nobody can grant. That has not changed and is
    /// asserted below.
    ///
    /// What changed is the other half. A rooted turn requires `Origin::Shell`, so the owner IS at
    /// the window while this is being decided — and the honest thing to do with somebody who is
    /// watching is ask them. It used to be refused with a sentence telling them to do it somewhere
    /// else, and there was nowhere else, which stopped a coding conversation at the first action
    /// the classifier did not recognise as read-only.
    #[tokio::test]
    async fn a_rooted_turn_is_asked_about_rather_than_parked_when_something_needs_approving() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_run(&state, "C:/Projects/nucleos").await;

        let decision = decide(
            &app,
            &format!(
                r#"{{"run_id":{run_id},"tool_name":"Bash","tool_input":{{"command":"frobnicate --hard"}}}}"#
            ),
        )
        .await;

        assert_eq!(decision.decision, "asking");
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status, "running",
            "the conversation was terminated by a question"
        );
        let proposals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM proposals")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(proposals, 0, "a question left a proposal nobody can resume");
    }

    /// The barrier follows the tools. Without this the rule would hold on the MCP side and be walked
    /// around on the other: no `approve_proposal` after reading mail, but `Bash` all you like.
    #[tokio::test]
    async fn a_rooted_turn_that_read_third_party_text_may_still_read_and_may_do_nothing_else() {
        let state = test_state().await;
        let app = test_router(state.clone());
        let run_id = rooted_turn_run(&state, "C:/Projects/nucleos").await;
        crate::runs::mark_untrusted_context(&state.pool, run_id)
            .await
            .unwrap();

        let reading = decide(
            &app,
            &format!(
                r#"{{"run_id":{run_id},"tool_name":"Read","tool_input":{{"file_path":"a.rs"}}}}"#
            ),
        )
        .await;
        assert_eq!(
            reading.decision, "allow",
            "reading this machine is still allowed"
        );

        let writing = decide(
            &app,
            &format!(
                r#"{{"run_id":{run_id},"tool_name":"Write","tool_input":{{"file_path":"C:/Projects/nucleos/a.rs","content":"x"}}}}"#
            ),
        )
        .await;
        assert_eq!(writing.decision, "deny");
        assert_eq!(writing.reason, UNTRUSTED_CONTEXT_DENY_REASON);
    }

    #[tokio::test]
    async fn assistant_turn_allows_nucleos_mcp_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "mcp__nucleos__list_projects",
                "tool_input": {}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "allow");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn assistant_turn_denies_non_mcp_tool_and_creates_no_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "ToolSearch",
                "tool_input": {}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Every acting tool this server has, refused to a department one by one — bar the one it holds
    /// on purpose.
    ///
    /// Enumerated from `TOOL_EFFECTS` rather than listed, so a tool classified `Acts` in future is
    /// covered the day it is added instead of the day somebody remembers this test. The second
    /// layer only — `auth::TEAM_ROUTES` refuses these without anybody's cooperation, and this hook
    /// may never fire at all, since a team run launches with no working directory to resolve a
    /// `.claude/settings.json` from.
    #[tokio::test]
    async fn a_team_agent_is_refused_every_acting_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::team::TEAM_MODE, None, None, None).await;
        let app = test_router(state.clone());

        let mut acting = 0;
        for tool in crate::mcp_tools::every_tool_name() {
            if crate::mcp_tools::tool_effect(&tool) != crate::mcp_tools::ToolEffect::Acts {
                continue;
            }
            // The single exception, named rather than filtered by a predicate so a second one has
            // to be typed out by whoever adds it. Calling `propose_action` performs nothing — it
            // records a request the core carries out later if a human agrees — and its `Acts`
            // grading exists so the taint rule closes it once the turn has read a stranger's words.
            if tool == "propose_action" || tool == "propose_teammate" {
                continue;
            }
            acting += 1;
            let decision = orchestrator_tool(&app, run_id, &tool, serde_json::json!({})).await;
            assert_eq!(decision.decision, "deny", "a department reached {tool}");
        }
        assert!(acting >= 7, "only {acting} acting tools were exercised");

        // And the reads it exists to do are allowed, so the loop above is refusing the actions
        // rather than the whole server.
        //
        // **A fresh run per tool, and the reason is the rule this file now enforces.** Six of the
        // eight names below are `ReadsUntrusted`, so calling one MARKS the turn — and `get_email`
        // sorts before `propose_action`. Sharing one run would have this loop assert that a
        // department may ask having already read, which is the opposite of what `team_decision`
        // decides, and it would do so as an accident of alphabetical order rather than as anything
        // anybody chose. What this loop means is "each of these is allowed to a department that has
        // not read yet", and one run each is what says that.
        for tool in crate::mcp_tools::TEAM_TOOLS {
            let clean = in_flight_run(&state, crate::team::TEAM_MODE, None, None, None).await;
            let decision = orchestrator_tool(&app, clean, tool, serde_json::json!({})).await;
            assert_eq!(
                decision.decision, "allow",
                "a department was refused {tool}"
            );
        }
    }

    /// A department that has read a stranger's words may still read, and may no longer ask.
    ///
    /// This is the rule `propose_action`'s own grading was written for — *"a specialist that has
    /// read a web page or a colleague's file loses it for the rest of the turn, which is exactly
    /// the door that must close"* — and until this test the door was open on the cloud path.
    /// `team_decision` allowed every name in `TEAM_TOOLS` unconditionally, and the two comments
    /// above it asserted the premise that justified doing so: *"`TEAM_TOOLS` carries no `Acts`"*.
    /// Both sentences were true when written and stopped being true when the alçada landed.
    ///
    /// The local box did enforce it, which is why nothing looked broken: `local_agent.rs` is the
    /// ONE caller of `permitted_after_untrusted` in this codebase, so the rule held for an Ollama
    /// specialist and not for the Claude CLI one that runs by default.
    ///
    /// Both halves are asserted on purpose. A barrier that refused everything would also pass the
    /// first loop, and a department whose reads shut down after one web page cannot do the job it
    /// exists for — the rule is "having read, act no more", not "having read, stop".
    #[tokio::test]
    async fn a_team_agent_that_read_a_strangers_words_may_still_read_and_may_no_longer_ask() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::team::TEAM_MODE, None, None, None).await;
        crate::runs::mark_untrusted_context(&state.pool, run_id)
            .await
            .unwrap();
        let app = test_router(state.clone());

        // Enumerated from `TEAM_TOOLS` filtered by effect, not from a list typed here, so a third
        // acting tool added to a department's box is covered the day it arrives.
        let mut acting = 0;
        for tool in crate::mcp_tools::TEAM_TOOLS {
            if crate::mcp_tools::tool_effect(tool) != crate::mcp_tools::ToolEffect::Acts {
                continue;
            }
            acting += 1;
            let decision = orchestrator_tool(&app, run_id, tool, serde_json::json!({})).await;
            assert_eq!(
                decision.decision, "deny",
                "a department that had read a stranger's words still reached {tool}"
            );
        }
        assert_eq!(
            acting, 2,
            "a department holds exactly two acting tools; if that changed, this test is measuring              something other than what it was written for"
        );

        for tool in crate::mcp_tools::TEAM_TOOLS {
            if crate::mcp_tools::tool_effect(tool) == crate::mcp_tools::ToolEffect::Acts {
                continue;
            }
            let decision = orchestrator_tool(&app, run_id, tool, serde_json::json!({})).await;
            assert_eq!(
                decision.decision, "allow",
                "a department that had read one page was refused {tool}, and reading is its job"
            );
        }
    }

    /// The refusals that are not about `Acts` at all: a tool of another server whose name passes a
    /// prefix test, and a NucleOS tool a department is simply not offered.
    #[tokio::test]
    async fn a_team_agent_is_refused_a_lookalike_server_and_an_unoffered_read() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::team::TEAM_MODE, None, None, None).await;
        let app = test_router(state.clone());

        for tool_name in [
            // `nucleos__x` is a different server, and `mcp__nucleos__x__list_files` is what it
            // produces — which passes a prefix test and must not pass this one.
            "mcp__nucleos__x__list_files",
            "mcp__other__list_files",
            "Bash",
            "Write",
            // Classified `ReadsOwn`, on no `Acts` list, and still not a department's business.
            "mcp__nucleos__list_projects",
            "mcp__nucleos__get_budget",
        ] {
            let decision = decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": tool_name,
                    "tool_input": {}
                })
                .to_string(),
            )
            .await;
            assert_eq!(
                decision.decision, "deny",
                "a department reached {tool_name}"
            );
        }
    }

    /// An assistant run answering an errand's topic: the errand row, and a run whose `chat_id` is
    /// the key that row is registered under.
    ///
    /// Nothing on the run says "errand" — the join is the chat key and only the chat key, which is
    /// the walk `errand_of_run` has to make on every single tool call an errand's turn produces.
    ///
    /// Returns `(errand_id, run_id)`.
    async fn errand_bound_run(state: &AppState, chat_key: &str) -> (i64, i64) {
        let errand_id = crate::errands::create(&state.pool, "carros", chat_key)
            .await
            .unwrap();
        let run_id = in_flight_run(state, "assistant", None, None, None).await;
        sqlx::query("UPDATE runs SET chat_id = ? WHERE id = ?")
            .bind(chat_key)
            .bind(run_id)
            .execute(&state.pool)
            .await
            .unwrap();
        (errand_id, run_id)
    }

    /// `WritesOwn` on the far side of the barrier, which is the whole reason the variant exists.
    ///
    /// The order here is the order every errand actually runs in: read its own notes, read the open
    /// web, write down what it found. The web read drops the barrier — `create_run` at the end
    /// proves the barrier is genuinely down and not merely untested — and the write still has to go
    /// through, because an errand that researches and can never record is not an errand.
    ///
    /// If `errand_files_write` were ever moved to `Acts`, or the `WritesOwn` arm folded into the
    /// `Acts` arm, this test is what says so: the third assertion flips and nothing else does.
    #[tokio::test]
    async fn an_errand_still_writes_its_own_notes_after_reading_the_web() {
        let state = test_state().await;
        let (errand_id, run_id) = errand_bound_run(&state, "-1002003004:7").await;
        let app = test_router(state.clone());

        // A file this errand recorded, written by a turn that had read nothing third-party: the one
        // shape `errand_file_read_effect` lets back as own.
        crate::errands::record_artifact(&state.pool, errand_id, "notas.md", false, None)
            .await
            .unwrap();
        let own = orchestrator_tool(
            &app,
            run_id,
            "errand_files_read",
            serde_json::json!({"path": "notas.md"}),
        )
        .await;
        assert_eq!(own.decision, "allow");
        assert!(
            !crate::runs::read_untrusted_context(&state.pool, run_id)
                .await
                .unwrap(),
            "reading back a file this errand itself wrote clean is not reading a stranger"
        );

        let web = orchestrator_tool(
            &app,
            run_id,
            "web_read",
            serde_json::json!({"url": "https://stand.example/anuncio"}),
        )
        .await;
        assert_eq!(
            web.decision, "allow",
            "reading the web is what an errand is for"
        );
        assert!(
            crate::runs::read_untrusted_context(&state.pool, run_id)
                .await
                .unwrap(),
            "the turn now has a stranger's words in it and must be on record as having them"
        );

        let write = orchestrator_tool(
            &app,
            run_id,
            "errand_files_write",
            serde_json::json!({"path": "notas.md", "content": "1998 Golf, 3200 EUR"}),
        )
        .await;
        assert_eq!(
            write.decision, "allow",
            "an errand that cannot record what it found is not an errand"
        );

        let act = orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "x"}),
        )
        .await;
        assert_eq!(
            act.decision, "deny",
            "the allow above must be WritesOwn passing the barrier, not a barrier that never closed"
        );
    }

    /// A run with no chat has no errand, and `errand_of_run` says so rather than guessing.
    ///
    /// The path asked for is a real one, recorded clean under a real errand — so if this walk ever
    /// answered `Some` for a run that names no chat, the read would come back `ReadsOwn` and the
    /// turn would stay unmarked. That is the laundering direction: one errand's marks vouching for
    /// a turn that was never that errand's. `None` is read as "cannot say", and cannot-say is a
    /// stranger.
    #[tokio::test]
    async fn a_run_with_no_chat_borrows_no_errands_marks() {
        let state = test_state().await;
        let errand_id = crate::errands::create(&state.pool, "carros", "-1002003004:7")
            .await
            .unwrap();
        crate::errands::record_artifact(&state.pool, errand_id, "notas.md", false, None)
            .await
            .unwrap();

        // Same mode, same tool, same path as the test above — the only difference is the missing
        // `chat_id`, so that is the only thing the different outcome can be attributed to.
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let read = orchestrator_tool(
            &app,
            run_id,
            "errand_files_read",
            serde_json::json!({"path": "notas.md"}),
        )
        .await;
        assert_eq!(read.decision, "allow", "the read itself is not the refusal");
        assert!(
            crate::runs::read_untrusted_context(&state.pool, run_id)
                .await
                .unwrap(),
            "a file classified against an errand this run does not belong to must count as a stranger's words"
        );
    }

    /// The barrier gets a door, and the door is a person.
    ///
    /// §6 refuses an action once a turn has read a stranger's words, and for an errand that is
    /// every turn that did any research — which is all of them. Until now the refusal was the end
    /// of the line: the model was stopped and the owner never learned what it had wanted to do. So
    /// the errand that spends an afternoon finding the right car cannot tell anyone it found it.
    ///
    /// The refusal does not move. What changes is that it is written down where somebody can read
    /// it, decide, and act — or ask the errand again in a fresh turn, which starts clean and may
    /// act. That is the whole of piece 5: the refusal keeps a record instead of a silence.
    #[tokio::test]
    async fn an_action_refused_after_the_web_is_written_down_for_a_person() {
        let state = test_state().await;
        let (_errand_id, run_id) = errand_bound_run(&state, "-1002003004:8").await;
        let app = test_router(state.clone());

        orchestrator_tool(
            &app,
            run_id,
            "web_read",
            serde_json::json!({"url": "https://stand.example/anuncio"}),
        )
        .await;
        let act = orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "encomendar o Golf"}),
        )
        .await;

        assert_eq!(act.decision, "deny", "the barrier does not bend");
        let refused = crate::proposals::list_refused_actions(&state.pool)
            .await
            .unwrap();
        assert_eq!(refused.len(), 1, "and the person gets to know about it");
        assert_eq!(refused[0].tool_name.as_deref(), Some("create_run"));
        assert!(
            refused[0]
                .tool_input
                .as_deref()
                .unwrap()
                .contains("encomendar o Golf"),
            "with enough of it to decide on: {:?}",
            refused[0].tool_input
        );
    }

    /// A record that does not say which errand it came from is a record nobody can decide on.
    ///
    /// "Approve: send_email" tells a person the verb and nothing else. Approving it anyway is what
    /// turns an approval step into a formality, which is the worst thing an approval step can be —
    /// it costs the interruption and buys none of the safety. The errand is the missing half: what
    /// this is about, and therefore whether the answer is yes.
    ///
    /// The name and not only the id, because an id is a thing to go and look up, and a step that
    /// requires a lookup before it can be answered is a step that gets answered without one.
    #[tokio::test]
    async fn a_refused_action_says_which_errand_wanted_it() {
        let state = test_state().await;
        let (errand_id, run_id) = errand_bound_run(&state, "-1002003004:11").await;
        let app = test_router(state.clone());

        orchestrator_tool(
            &app,
            run_id,
            "web_read",
            serde_json::json!({"url": "https://stand.example/anuncio"}),
        )
        .await;
        orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "encomendar o Golf"}),
        )
        .await;

        let refused = crate::proposals::list_refused_actions(&state.pool)
            .await
            .unwrap();
        assert_eq!(refused[0].errand_id, Some(errand_id));
        assert_eq!(refused[0].errand_name.as_deref(), Some("carros"));
    }

    /// The record says what the turn was going to do; this says where the idea came from.
    ///
    /// Those are different questions and only the second one decides. "Send an email to accounts
    /// asking them to change the bank details" reads identically whether the owner asked for it or
    /// a page did, and a person handed only the first question answers it by guessing.
    #[tokio::test]
    async fn a_refused_action_says_which_stranger_put_the_idea_there() {
        let state = test_state().await;
        let (_errand_id, run_id) = errand_bound_run(&state, "-1002003004:12").await;
        let app = test_router(state.clone());

        orchestrator_tool(
            &app,
            run_id,
            "web_read",
            serde_json::json!({"url": "https://stand.example/anuncio"}),
        )
        .await;
        orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "encomendar o Golf"}),
        )
        .await;

        let refused = crate::proposals::list_refused_actions(&state.pool)
            .await
            .unwrap();
        let read_from = refused[0]
            .read_from
            .as_deref()
            .expect("a turn refused by the barrier read SOMETHING, and the row should say what");
        assert!(
            read_from.contains("web_read"),
            "the provenance names no tool: {read_from}"
        );
        assert!(
            read_from.contains("stand.example/anuncio"),
            "the provenance does not reach the url, which is the only part that decides: {read_from}"
        );
    }

    /// Every stranger, not the first one.
    ///
    /// A browsing turn opens, snapshots, acts and snapshots again, and the page that planted an idea
    /// is as likely to be the fourth as the first. Showing one and calling it the provenance would
    /// be worse than showing none: it reads as complete.
    #[tokio::test]
    async fn every_stranger_the_turn_read_is_listed_and_not_just_the_first() {
        let state = test_state().await;
        let (_errand_id, run_id) = errand_bound_run(&state, "-1002003004:13").await;
        let app = test_router(state.clone());

        for url in [
            "https://stand.example/primeiro",
            "https://outro.example/segundo",
        ] {
            orchestrator_tool(&app, run_id, "web_read", serde_json::json!({"url": url})).await;
        }
        orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "x"}),
        )
        .await;

        let refused = crate::proposals::list_refused_actions(&state.pool)
            .await
            .unwrap();
        let read_from = refused[0].read_from.as_deref().unwrap_or_default();
        assert!(
            read_from.contains("primeiro") && read_from.contains("segundo"),
            "one of the two reads is missing: {read_from}"
        );
    }

    /// An absent provenance is a legitimate answer, and the case that proves it is the OTHER
    /// refusal this kind carries.
    ///
    /// `ERRAND_MAY_NOT_ACT` fires on whose work it is, not on what was read — an errand's first
    /// message has read nothing at all. A reader that treats `None` as "contaminated by something
    /// we failed to log" would show a warning about a turn that was clean.
    #[tokio::test]
    async fn an_errand_refused_for_whose_work_it_is_carries_no_provenance() {
        let state = test_state().await;
        let (_errand_id, run_id) = errand_bound_run(&state, "-1002003004:14").await;
        let app = test_router(state.clone());

        let act = orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "x"}),
        )
        .await;

        assert_eq!(act.decision, "deny");
        assert_eq!(act.reason, ERRAND_MAY_NOT_ACT, "a different rule fired");
        let refused = crate::proposals::list_refused_actions(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            refused[0].read_from, None,
            "this turn read nothing, and the row must not imply it did"
        );
    }

    /// The guard. If this fails, piece 5 has put a human step in front of everything that worked
    /// before it — which is the failure mode of every approval mechanism ever added to anything.
    ///
    /// On a run that is NOT an errand's, which is the population this protects. An ordinary
    /// orchestrator turn asked to start a run starts it, exactly as before errands existed.
    #[tokio::test]
    async fn an_action_in_an_ordinary_clean_turn_still_goes_straight_through() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let act = orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "x"}),
        )
        .await;

        assert_eq!(act.decision, "allow");
        assert!(
            crate::proposals::list_refused_actions(&state.pool)
                .await
                .unwrap()
                .is_empty(),
            "nothing was refused, so there is nothing to ask anybody about"
        );
    }

    /// An errand never acts on its own, whatever is in its box and whatever it has read.
    ///
    /// Until now the refusal came from the injection barrier, which is a rule about ORDER: read a
    /// stranger's words and the acting tools shut. That made the errand's box safe by coincidence —
    /// nothing in it acts, so the barrier never had to bite — and it left one gap that the day
    /// somebody widens the box walks straight into: a turn that has read nothing yet is clean, and
    /// a clean turn acts unconditionally. The first message in a topic is exactly that turn.
    ///
    /// So the rule here is about WHOSE work it is rather than what it has read. An errand is a
    /// Telegram topic; the acting tools are the daemon's controls and, one day, an email nobody can
    /// unsend. A person is between them, always, and the record from piece 5 is how they hear about
    /// it. That is what makes the box safe to widen — the safety stops depending on the box being
    /// empty of actions.
    #[tokio::test]
    async fn an_errand_cannot_act_even_in_a_turn_that_has_read_nothing() {
        let state = test_state().await;
        let (errand_id, run_id) = errand_bound_run(&state, "-1002003004:12").await;
        let app = test_router(state.clone());

        assert!(
            !crate::runs::read_untrusted_context(&state.pool, run_id)
                .await
                .unwrap(),
            "this turn has read nothing, which is the whole point of the test"
        );
        let act = orchestrator_tool(
            &app,
            run_id,
            "create_run",
            serde_json::json!({"project_id": "proj", "prompt": "encomendar o Golf"}),
        )
        .await;

        assert_eq!(act.decision, "deny");
        let refused = crate::proposals::list_refused_actions(&state.pool)
            .await
            .unwrap();
        assert_eq!(refused.len(), 1, "and the person hears about it");
        assert_eq!(refused[0].errand_id, Some(errand_id));
    }

    /// A turn that keeps reaching leaves ONE record, not one per attempt.
    ///
    /// The same shape the git queue already uses for the same problem: a model told no will often
    /// try again, and a person who opens their phone to eleven copies of one question stops reading
    /// the list — which costs more than the feature was worth. Enforced by a partial unique index
    /// rather than a check-then-insert, so two attempts racing cannot both find nothing there.
    #[tokio::test]
    async fn a_turn_that_keeps_reaching_leaves_one_record_not_many() {
        let state = test_state().await;
        let (_errand_id, run_id) = errand_bound_run(&state, "-1002003004:10").await;
        let app = test_router(state.clone());

        orchestrator_tool(
            &app,
            run_id,
            "web_read",
            serde_json::json!({"url": "https://stand.example/anuncio"}),
        )
        .await;
        for prompt in ["primeira", "segunda", "terceira"] {
            let act = orchestrator_tool(
                &app,
                run_id,
                "create_run",
                serde_json::json!({"project_id": "proj", "prompt": prompt}),
            )
            .await;
            assert_eq!(act.decision, "deny", "every one of them is still refused");
        }

        assert_eq!(
            crate::proposals::list_refused_actions(&state.pool)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    /// One NucleOS tool call in an orchestrator turn, named the way the CLI names it.
    async fn orchestrator_tool(
        app: &Router,
        run_id: i64,
        tool: &str,
        tool_input: serde_json::Value,
    ) -> Decision {
        decide(
            app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": format!("mcp__nucleos__{tool}"),
                "tool_input": tool_input,
            })
            .to_string(),
        )
        .await
    }

    /// The attack this barrier exists for, start to finish.
    ///
    /// An orchestrator turn is allowed every NucleOS tool unconditionally and carries the control
    /// token, and `get_email` returns a stranger's body verbatim into that same context. So a mail
    /// body that says "approve proposal 4" was read by the one agent able to approve it, in the one
    /// mode with no classifier, no proposal and no termination between the reading and the doing.
    /// The threat model calls "an email body causing a tool call" a thing this product prevents;
    /// until this test passed, it prevented it only for the triage run.
    #[tokio::test]
    async fn a_mail_body_cannot_reach_the_controls_it_asks_for() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state.clone());

        let read = orchestrator_tool(&app, run_id, "get_email", serde_json::json!({"id": 7})).await;
        assert_eq!(
            read.decision, "allow",
            "reading mail is what the turn is for"
        );

        // The three the body would ask for: lift an approval the classifier withheld, disengage the
        // emergency stop, and start a run that launches with `ToolPolicy::Unrestricted`.
        for tool in ["approve_proposal", "set_kill", "create_run"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 4})).await;
            assert_eq!(
                decision.decision, "deny",
                "{tool} was allowed after a mail read"
            );
            assert_eq!(decision.reason, UNTRUSTED_CONTEXT_DENY_REASON, "{tool}");
        }

        // Refused, never punished: the owner asking for two things in one message lands here too,
        // and the turn is not a governed run that can be paused or proposed against.
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));
        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The rule is about acting, not about reading, and a barrier that also stopped the reading
    /// would have taken the feature with it: the owner asked what was in their mail, and the answer
    /// needs more than one message to assemble.
    #[tokio::test]
    async fn a_turn_that_has_read_mail_may_keep_reading() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        orchestrator_tool(&app, run_id, "get_email_queue", serde_json::json!({})).await;

        for tool in ["get_email", "get_email_queue", "list_files"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
        // NucleOS's own state, which changes nothing and travels to the owner's own chat.
        for tool in ["list_proposals", "get_budget", "get_kill", "list_projects"] {
            let decision = orchestrator_tool(&app, run_id, tool, serde_json::json!({})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// The ordinary case has to keep working, or the barrier is just an outage: a turn that has read
    /// nothing third-party is the one the owner uses to approve and to work the kill switch.
    #[tokio::test]
    async fn a_turn_that_has_read_no_mail_still_acts() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        for tool in ["approve_proposal", "set_kill", "create_run", "cancel_run"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
    }

    /// Marked on the run, not on the process. A flag held in a module-level set would be shared by
    /// every turn the daemon is running, so one chat asking about its mail would quietly disarm the
    /// controls in another — and a test suite whose in-memory databases all start numbering at 1
    /// would not notice, because it would look like the barrier working.
    #[tokio::test]
    async fn one_turn_reading_mail_does_not_disarm_the_next() {
        let state = test_state().await;
        let reader = in_flight_run(&state, "assistant", None, None, None).await;
        let other = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        orchestrator_tool(&app, reader, "get_email", serde_json::json!({"id": 1})).await;

        assert_eq!(
            orchestrator_tool(
                &app,
                reader,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "deny"
        );
        assert_eq!(
            orchestrator_tool(
                &app,
                other,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "allow",
            "a turn that read nothing must not inherit another turn's refusal"
        );
    }

    /// `get_run` reads a run's own output, which is NucleOS's account of the owner's work — except
    /// for a triage run, where it is a model's answer over mail a stranger wrote. The parse that
    /// bounds a verdict to a class and 200 stripped characters runs after the raw stream is stored,
    /// so this tool is the one way that text gets back out unbounded.
    #[tokio::test]
    async fn reading_a_triage_run_counts_as_reading_the_mail_it_triaged() {
        let state = test_state().await;
        let turn = in_flight_run(&state, "assistant", None, None, None).await;
        let triage = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let worktree = in_flight_run(&state, "worktree", None, None, None).await;
        let app = test_router(state);

        // A worktree run's output is the owner's own work and leaves the turn able to act.
        orchestrator_tool(&app, turn, "get_run", serde_json::json!({"id": worktree})).await;
        assert_eq!(
            orchestrator_tool(
                &app,
                turn,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "allow"
        );

        orchestrator_tool(&app, turn, "get_run", serde_json::json!({"id": triage})).await;
        assert_eq!(
            orchestrator_tool(
                &app,
                turn,
                "set_kill",
                serde_json::json!({"engaged": false})
            )
            .await
            .decision,
            "deny",
            "a triage run's stdout is a stranger's words at one remove"
        );
    }

    /// The question being answered is whether third-party text is about to enter the turn, and "I
    /// cannot tell which run you mean" is not "no". An id that is missing or is not a number would
    /// otherwise be the cheapest way to read a triage run without being counted as having done so.
    #[tokio::test]
    async fn a_get_run_naming_nothing_readable_is_treated_as_third_party_text() {
        for tool_input in [
            serde_json::json!({}),
            serde_json::json!({"id": "12"}),
            serde_json::json!({"id": null}),
        ] {
            let state = test_state().await;
            let turn = in_flight_run(&state, "assistant", None, None, None).await;
            let app = test_router(state);

            orchestrator_tool(&app, turn, "get_run", tool_input.clone()).await;

            assert_eq!(
                orchestrator_tool(
                    &app,
                    turn,
                    "set_kill",
                    serde_json::json!({"engaged": false})
                )
                .await
                .decision,
                "deny",
                "{tool_input}"
            );
        }
    }

    /// A name this server does not expose reaches the same unconditional allow as one it does, so it
    /// has to land on the fail-closed side of the rule rather than on neither side of it.
    #[tokio::test]
    async fn an_unrecognised_nucleos_tool_is_refused_after_mail_is_read() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "assistant", None, None, None).await;
        let app = test_router(state);

        assert_eq!(
            orchestrator_tool(&app, run_id, "send_email", serde_json::json!({}))
                .await
                .decision,
            "allow",
            "an unknown name behaves as it did before a turn has read anything"
        );

        orchestrator_tool(&app, run_id, "get_email", serde_json::json!({"id": 1})).await;

        assert_eq!(
            orchestrator_tool(&app, run_id, "send_email", serde_json::json!({}))
                .await
                .decision,
            "deny"
        );
    }

    #[tokio::test]
    async fn self_governing_edit_pause_creates_a_proposal_with_edit_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(
            &state,
            "worktree",
            Some("proj"),
            Some("C:\\work\\repo"),
            Some("sess-e"),
        )
        .await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": {"file_path": ".ai/autopilot.yaml"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tool_name.as_deref(), Some("Edit"));
        assert_eq!(pending[0].run_id, Some(run_id));
    }

    /// What the human actually agreed to is a KIND of action, not one spelling of it. Approving
    /// `git push origin main` and then parking the resume on `git push origin other` asked the same
    /// question twice about the same decision — and every re-ask is a chance to answer it wearily.
    /// The grant therefore covers its class for the rest of the run rather than a single call.
    #[tokio::test]
    async fn a_grant_authorizes_every_action_of_its_class_for_the_rest_of_the_run() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        proposals::grant_action(&state.pool, run_id, "Bash", Some("push-merge-deploy"), 1)
            .await
            .unwrap();
        let app = test_router(state.clone());

        // Two DIFFERENT pushes: same class, different input. The second is the one the old
        // single-use, input-matched grant sent back for a second approval.
        for command in ["git push origin main", "git push origin other"] {
            let decision = decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": "Bash",
                    "tool_input": {"command": command}
                })
                .to_string(),
            )
            .await;
            assert_eq!(decision.decision, "allow", "{command}");

            // Never parks, and is never terminated on the way to parking: an authorized action
            // that still killed the run would be an allow in name only.
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(status, "running", "{command}");
            assert!(
                state.run_handles.lock().unwrap().contains_key(&run_id),
                "{command}"
            );
        }
    }

    /// The other half of the class rule: covering a class for the rest of the run is only safe if
    /// the class is a real boundary. A push approval must not reach an edit to the file that
    /// governs what this run is allowed to do at all.
    #[tokio::test]
    async fn a_grant_does_not_authorize_a_different_action_class() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        proposals::grant_action(&state.pool, run_id, "Bash", Some("push-merge-deploy"), 1)
            .await
            .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Edit",
                "tool_input": {"file_path": ".ai/autopilot.yaml"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "awaiting_approval");

        // The grant survives whole: an action it does not cover must neither spend it nor be
        // recorded as having used it.
        let grant = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT action_class, consumed_at FROM action_grants WHERE run_id = ?",
        )
        .bind(run_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            grant,
            (Some("push-merge-deploy".to_owned()), None),
            "the grant must survive an action class it does not authorize"
        );
    }

    #[tokio::test]
    async fn a_database_error_resolving_the_mode_denies() {
        // `mode` decides WHICH rules apply, so failing to read it is not a reason to pick the most
        // permissive one. This is the `Err` twin of the `Ok(None)` case: an email-triage run whose
        // mode read fails must not be handed `Read`, the one tool the pillar exists to keep away
        // from a stranger's text. A gate that cannot be read is not permission.
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::email::TRIAGE_MODE, None, None, None).await;
        let app = test_router(state.clone());

        // A closed pool makes every query error — the cheapest faithful stand-in for the SQLITE_BUSY
        // this handler shares a pool with feed appends and run-status writes to earn.
        state.pool.close().await;

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Read",
                "tool_input": {"file_path": "README.md"}
            })
            .to_string(),
        )
        .await;

        assert_eq!(decision.decision, "deny");
    }

    #[tokio::test]
    async fn deny_still_denies_even_with_a_matching_grant() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "worktree", None, Some("C:\\work\\repo"), None).await;
        // The grant names the very class of the action attempted below, so this proves `deny`
        // outranks a grant that covers it rather than merely one that failed to match. No approval
        // flow can mint such a grant — `deny` never becomes a proposal — which is exactly why the
        // check has to hold against one conjured directly in the table.
        proposals::grant_action(&state.pool, run_id, "Bash", Some("destructive"), 1)
            .await
            .unwrap();
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            &serde_json::json!({
                "run_id": run_id,
                "tool_name": "Bash",
                "tool_input": {"command": "rm -rf target"}
            })
            .to_string(),
        )
        .await;
        assert_eq!(decision.decision, "deny");

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        assert!(state.run_handles.lock().unwrap().contains_key(&run_id));

        let consumed_at: Option<String> =
            sqlx::query_scalar("SELECT consumed_at FROM action_grants WHERE run_id = ?")
                .bind(run_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(consumed_at.is_none());
    }

    /// A seat's whole job is to read this machine's state and answer, so the reads have to work or
    /// the tools are decoration.
    #[tokio::test]
    async fn a_council_turn_allows_a_reads_own_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        // A `worktree` run, so `get_run` below names something that is not a peer.
        let other = in_flight_run(&state, "worktree", None, None, None).await;
        let app = test_router(state);

        for tool in ["list_projects", "list_proposals", "get_budget", "get_kill"] {
            let decision = orchestrator_tool(&app, run_id, tool, serde_json::json!({})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }
        assert_eq!(
            orchestrator_tool(&app, run_id, "get_run", serde_json::json!({"id": other}))
                .await
                .decision,
            "allow"
        );
    }

    /// Phase 1 is N INDEPENDENT answers, and run ids are sequential integers — so without this a
    /// seat could read the row next to its own and find a sibling's answer before writing its own.
    /// The seat that waited would then be answering with the others' work in front of it, and the
    /// ranking that follows would be measuring the wait.
    #[tokio::test]
    async fn a_council_seat_cannot_read_another_seats_run() {
        let state = test_state().await;
        let seat = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        let sibling = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        let app = test_router(state);

        assert_eq!(
            orchestrator_tool(&app, seat, "get_run", serde_json::json!({"id": sibling}))
                .await
                .decision,
            "deny"
        );
        // Its own row is a council run too, and the same rule refuses it. Nothing is lost: a seat
        // knows what it was asked, and the row holds nothing the seat did not write.
        assert_eq!(
            orchestrator_tool(&app, seat, "get_run", serde_json::json!({"id": seat}))
                .await
                .decision,
            "deny"
        );
        // Fails closed on a shape it cannot read, like its triage sibling: an absent id, and an id
        // that is not a number, are both "I could not tell" rather than "no".
        assert_eq!(
            orchestrator_tool(&app, seat, "get_run", serde_json::json!({}))
                .await
                .decision,
            "deny"
        );
        assert_eq!(
            orchestrator_tool(&app, seat, "get_run", serde_json::json!({"id": "nine"}))
                .await
                .decision,
            "deny"
        );
    }

    /// The half that makes a seat worth asking. Half the questions somebody puts to a council are
    /// about what arrived, and a seat that cannot read mail answers those from what it half-recalls.
    ///
    /// Safe here in a way it is not for the orchestrator, and for a structural reason rather than a
    /// hopeful one: the taint rule exists to stop a stranger's words from reaching a tool that ACTS,
    /// and no tool a seat may call acts.
    #[tokio::test]
    async fn a_council_turn_allows_a_reads_untrusted_tool() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        let app = test_router(state.clone());

        for tool in ["get_email_queue", "get_email", "list_files"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "allow", "{tool}");
        }

        // And having read them changes nothing afterwards, because there was never anything to
        // withdraw: a seat could not act before the mail and cannot act after it.
        assert_eq!(
            orchestrator_tool(&app, run_id, "create_run", serde_json::json!({}))
                .await
                .decision,
            "deny"
        );
    }

    /// The brake itself. Every `Acts` tool refused, the run left alone, and no proposal minted —
    /// a council has no worktree to resume into, so an approval for one could never be satisfied.
    #[tokio::test]
    async fn a_council_turn_denies_an_acts_tool_and_creates_no_proposal() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, crate::council::COUNCIL_MODE, None, None, None).await;
        let app = test_router(state.clone());

        for tool in [
            "create_run",
            "create_job",
            "approve_proposal",
            "reject_proposal",
            "cancel_run",
            "set_kill",
            "triage_email",
            "vcs_request",
            // Not an action, and refused all the same: `web_search` and `web_read` reach off this
            // machine, and a roster of local seats holding either would stop being a local council.
            "web_search",
            "web_read",
            // The read-back half of `vcs_request`. A seat that cannot queue an operation has
            // nothing of its own to read back.
            "vcs_ticket",
        ] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_eq!(decision.decision, "deny", "{tool}");
        }

        // Tools outside this server too: a seat is not an ordinary run and does not get Bash by
        // falling through to the classifier.
        for tool in ["Bash", "Write", "Read", "mcp__other__anything"] {
            let decision = decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": tool,
                    "tool_input": {"command": "git push"}
                })
                .to_string(),
            )
            .await;
            assert_eq!(decision.decision, "deny", "{tool}");
        }

        // The prefix trap `assistant_decision` records: a server called `nucleos__x` would produce
        // this name, and a prefix test would have inherited the council's allow.
        assert_eq!(
            decide(
                &app,
                &serde_json::json!({
                    "run_id": run_id,
                    "tool_name": "mcp__nucleos__x__get_budget",
                    "tool_input": {}
                })
                .to_string(),
            )
            .await
            .decision,
            "deny"
        );

        assert!(
            proposals::list_pending(&state.pool)
                .await
                .unwrap()
                .is_empty()
        );
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running", "a refusal must not terminate the seat");
    }

    /// The property the mode branches are written to preserve: a mode nobody has written a branch
    /// for does not inherit an unconditional allow.
    ///
    /// It falls through to the classifier, where an MCP tool name is `unrecognized` and therefore
    /// `pending_approval` — which is a stop, not a grant. Asserted as "not allow" rather than as the
    /// exact verdict, because the value of this test is the direction and pinning the spelling would
    /// make a later refinement of the classifier read as a regression here.
    #[tokio::test]
    async fn an_unknown_mode_gets_no_acts_by_default() {
        let state = test_state().await;
        let run_id = in_flight_run(&state, "a-mode-invented-later", None, None, None).await;
        let app = test_router(state);

        for tool in ["create_run", "set_kill", "vcs_request"] {
            let decision =
                orchestrator_tool(&app, run_id, tool, serde_json::json!({"id": 1})).await;
            assert_ne!(decision.decision, "allow", "{tool}");
        }
    }

    #[tokio::test]
    async fn no_proposal_created_when_run_is_not_in_flight() {
        let state = test_state().await;
        let app = test_router(state.clone());

        let decision = decide(
            &app,
            r#"{"run_id":0,"tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#,
        )
        .await;
        assert_eq!(decision.decision, "pending_approval");

        let pending = proposals::list_pending(&state.pool).await.unwrap();
        assert!(pending.is_empty());
    }

    /// A repository on the roster, and the path a session would be standing in.
    async fn rostered_repo(state: &AppState, prefix: &str) -> tempfile::TempDir {
        let dir = crate::git_exec::tests::space_free_tempdir(prefix);
        crate::git_exec::tests::initialize_repo(dir.path());
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'shadow', ?)",
        )
        .bind("p")
        .bind(dir.path().to_string_lossy().into_owned())
        .execute(&state.pool)
        .await
        .unwrap();
        dir
    }

    async fn session_decision(state: &AppState, command: &str, cwd: &Path) -> Decision {
        session_git_decision(
            State(state.clone()),
            Json(SessionGitPayload {
                tool_name: "Bash".to_owned(),
                tool_input: serde_json::json!({ "command": command }),
                cwd: cwd.to_string_lossy().into_owned(),
            }),
        )
        .await
        .0
    }

    async fn queued_rows(state: &AppState) -> Vec<(String, String)> {
        sqlx::query_as("SELECT op, origin FROM vcs_requests ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap()
    }

    /// **The whole point, and the behaviour that was missing.** A session nobody launched asked for
    /// a merge and got it, because the hook had no opinion without a run id.
    #[tokio::test]
    async fn an_editor_sessions_merge_is_queued_and_the_command_refused() {
        let state = test_state().await;
        let repo = rostered_repo(&state, "hook-session-merge").await;

        let decision = session_decision(&state, "git merge feature", repo.path()).await;

        assert_eq!(decision.decision, "deny", "{}", decision.reason);
        assert!(
            decision.reason.contains("queued as vcs request #1"),
            "the refusal has to name the ticket, or the session has nothing to watch: {}",
            decision.reason
        );
        assert_eq!(
            queued_rows(&state).await,
            vec![("merge".to_owned(), "shell".to_owned())],
            "an editor session is `shell`: a person's agent redirected here, not a person acting"
        );
    }

    /// The asymmetry that makes it safe to speak at all. Nothing this function does may end in an
    /// approval — the old silence was chosen because an `allow` from a daemon that cannot see what
    /// the person is doing would auto-approve their own tools.
    #[tokio::test]
    async fn a_session_decision_is_never_an_approval() {
        let state = test_state().await;
        let repo = rostered_repo(&state, "hook-session-never-allow").await;

        for command in [
            "git merge feature",
            "git push origin",
            "git tag v1",
            "git fetch origin",
            "git branch -d stale",
            "git rebase main",
        ] {
            let decision = session_decision(&state, command, repo.path()).await;
            assert_eq!(decision.decision, "deny", "{command}: {}", decision.reason);
        }
    }

    /// The worst spelling of the verb this queue most exists for, and it used to pass.
    ///
    /// "Declined by the queue" and "fine to run by hand" were read as one sentence. That reading is
    /// right for `--squash`, which is a different operation touching only the caller's index, and
    /// wrong for `--force`, which is the same operation in a worse spelling. Measured against the
    /// live gate before the fix: all three of these came back `allow`.
    ///
    /// Refused, and NOTHING queued — the queue cannot perform these spellings either, so there is
    /// nothing to admit. The refusal names the spelling it does know.
    #[tokio::test]
    async fn a_spelling_that_still_writes_what_others_share_is_refused_without_queueing() {
        let state = test_state().await;
        let repo = rostered_repo(&state, "hook-session-shared").await;

        for command in [
            "git push --force origin master",
            "git push",
            "git pull origin master",
            "git branch -D stale",
            "cd somewhere && git push --force origin master",
        ] {
            let decision = session_decision(&state, command, repo.path()).await;
            assert_eq!(decision.decision, "deny", "{command}: {}", decision.reason);
        }

        // And a MENTION is not a command. This refused its own commit message once: segments split
        // on newlines, so a line of prose naming the spelling looked exactly like one being run.
        for narrated in [
            "git commit -m \"git push --force was allowed\"",
            "echo remember to git push later",
            "grep -rn \"git pull\" docs",
        ] {
            let decision = session_decision(&state, narrated, repo.path()).await;
            assert_eq!(
                decision.decision, "allow",
                "a mention is not a command: {narrated} -> {}",
                decision.reason
            );
        }
        assert!(
            queued_rows(&state).await.is_empty(),
            "a refusal is not an admission: there is nothing the queue could perform here"
        );
    }

    /// The difference between a gate and a wall. The classifier sends everything not provably
    /// read-only for approval; refusing on THAT would stop a session at its second command. And a
    /// spelling that touches only the caller's own index has to keep working directly, or it becomes
    /// impossible rather than governed — `--squash` does not merge, it stages.
    #[tokio::test]
    async fn what_the_queue_will_not_perform_is_left_alone_rather_than_made_impossible() {
        let state = test_state().await;
        let repo = rostered_repo(&state, "hook-session-declined").await;

        for command in [
            "cargo test",
            "git status",
            "git merge --squash feature",
            "git merge --abort",
            "git log --oneline",
        ] {
            let decision = session_decision(&state, command, repo.path()).await;
            assert_eq!(decision.decision, "allow", "{command}: {}", decision.reason);
        }
        assert!(
            queued_rows(&state).await.is_empty(),
            "no opinion must also mean no row"
        );
    }

    /// The direction this function fails, and it is the opposite of `runs::queueable_operation`'s.
    /// There, a person has already approved an action and being unable to queue it must not strand
    /// them holding it. Here nobody has approved anything, so falling through would hand back
    /// exactly the bypass this exists to close.
    #[tokio::test]
    async fn a_repository_no_project_claims_is_refused_rather_than_waved_through() {
        let state = test_state().await;
        let dir = crate::git_exec::tests::space_free_tempdir("hook-session-unclaimed");
        crate::git_exec::tests::initialize_repo(dir.path());

        let decision = session_decision(&state, "git merge feature", dir.path()).await;

        assert_eq!(decision.decision, "deny", "{}", decision.reason);
        assert!(
            decision.reason.contains("no project on the roster"),
            "the refusal has to say which precondition failed: {}",
            decision.reason
        );
        assert!(queued_rows(&state).await.is_empty());
    }

    /// A session standing outside any repository is not the case being governed, and refusing there
    /// would block tools in every directory a person wanders into.
    ///
    /// The system temp directory, NOT `space_free_tempdir` — which builds inside the checkout on
    /// purpose, and is therefore inside a git repository. Writing this test the other way asserted
    /// nothing about being outside a working tree and failed by finding this very repo, which is a
    /// better outcome than the version that would have passed for the wrong reason.
    #[tokio::test]
    async fn a_session_outside_a_working_tree_gets_no_opinion() {
        let state = test_state().await;
        let dir = tempfile::tempdir().expect("create a tempdir outside any repository");
        assert!(
            !dir.path().join(".git").exists(),
            "the premise of this test is that nothing here is a repository"
        );

        let decision = session_decision(&state, "git merge feature", dir.path()).await;

        assert_eq!(decision.decision, "allow", "{}", decision.reason);
    }

    /// `cwd` is wherever the person was standing, which is a subdirectory more often than not.
    /// `current_branch` and `repo_key` both refuse a non-root path, and reading that refusal as
    /// "nothing to govern" is the bypass `git_exec::toplevel` exists to close.
    #[tokio::test]
    async fn a_session_standing_in_a_subdirectory_is_governed_just_the_same() {
        let state = test_state().await;
        let repo = rostered_repo(&state, "hook-session-subdir").await;
        let inside = repo.path().join("core");
        std::fs::create_dir_all(&inside).unwrap();

        let decision = session_decision(&state, "git merge feature", &inside).await;

        assert_eq!(decision.decision, "deny", "{}", decision.reason);
        assert_eq!(queued_rows(&state).await.len(), 1);
    }
}
