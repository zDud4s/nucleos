//! Deciding for a run rooted in a conversation: its turn, what is allowed there, and `rooted_decision`.

use super::*;

/// The conversation a run belongs to, or `None` for a run that is nobody's turn.
pub(super) async fn chat_of_run(pool: &sqlx::SqlitePool, run_id: i64) -> Option<String> {
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
pub(super) async fn rooted_turn(state: &AppState, run_id: i64) -> Option<String> {
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

/// What `bypass` says when it lowers a refusal to a question.
///
/// BOTH reasons are rewritten where this is used, not one. `Classification` carries the sentence
/// twice — on the `Decision` and again at the top — and `downgrade_if_unreadable` already rewrites
/// both when it makes this same move. Leaving the old one would label the question in the window
/// "destructive deletion commands are denied", which is the sentence of a refusal that has just
/// stopped existing.
pub(super) const BYPASS_STILL_ASKS: &str =
    "this conversation does not ask about anything else, and asks about this: it deletes";

/// The clause a refusal on `dont_ask` carries, after the classifier's own sentence.
///
/// A constant and not a literal at the return, for the reason `BYPASS_STILL_ASKS` above is one: the
/// test that pins the rung's position asserts on this text, and two copies of a sentence that must
/// match is how a reason drifts away from the test that guards it. The leading em dash is part of
/// it — this is APPENDED to a reason, never used alone.
pub(super) const DONT_ASK_CLAUSE: &str = " — and this conversation asks nobody";

/// Spec B D12, word for word: a person already answered this exact action for this task.
pub const A_PERSON_DECLINED_THIS: &str = "A person already declined this exact action for this task. It was not run. Do not try it again. Carry on with the task another way if there is one, or finish what you can and say what is missing in your final message.";

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
///
/// **`dont_ask` is here with `auto` and that is the whole of its share of this function.** This is
/// consulted only where the classifier has ALREADY said `allow`, so the only thing a rung can do
/// here is keep an allow or drop it, and `dont_ask` keeps every one of them — it is `auto`'s
/// permission exactly, not a narrower one. What makes the rung different is not in this function at
/// all: it is what happens to the calls that are NOT allows, which `auto` turns into a question and
/// `dont_ask` turns into a refusal, one screen down in `rooted_decision`.
pub(super) fn allowed_at(
    permission: crate::chats::PermissionMode,
    action_class: &str,
    tool_name: &str,
) -> bool {
    match permission {
        crate::chats::PermissionMode::Manual => {
            action_class == "read-local" && !crate::governance::classifier::writes_files(tool_name)
        }
        crate::chats::PermissionMode::AcceptEdits => action_class == "read-local",
        // Everything the classifier was willing to allow. `plan` is here and not one rung down
        // because its restraint comes from the CLI's own flag: a planning turn still reaches for
        // tools, and those calls arrive here and are governed exactly as `auto`'s are.
        crate::chats::PermissionMode::Plan
        | crate::chats::PermissionMode::Auto
        | crate::chats::PermissionMode::Bypass
        | crate::chats::PermissionMode::DontAsk => true,
    }
}

pub(super) async fn rooted_decision(
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
        // An opted-in ambient tool falls through to the classifier, the read_untrusted barrier and
        // the rung, exactly like a built-in tool. Anything under the `mcp__nucleos__` prefix keeps
        // the strict NucleOS-only path.
        let ambient = !payload.tool_name.starts_with("mcp__nucleos__")
            && crate::chats::ambient_mcp_for_run(&state.pool, run_id).await;
        if !ambient {
            return assistant_decision(state, payload).await;
        }
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
    // The GitHub policy rides its own gate, as it does there, and for the reason given there.
    let rules = if crate::governance::classifier::reads_shell_rules(&payload.tool_name) {
        shell_rules_of(state, project_id).await
    } else {
        ProjectRules::none()
    };
    let policy = if crate::governance::classifier::reads_github_policy(&payload.tool_name) {
        github_policy_of(state, project_id).await
    } else {
        std::borrow::Cow::Borrowed(&state.github.policy)
    };
    let mut classification = crate::governance::classifier::classify(
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
        crate::governance::classifier::Unrecognized::AsksAPerson,
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
    //
    // `read_untrusted_since` and not `read_untrusted_context`: a live task's calls arrive under its
    // launcher's id, and the process it runs in was fed by every later turn of the chat too, so
    // those turns count against it.
    if !crate::governance::classifier::only_reads(&payload.tool_name) {
        match crate::execution::runs::read_untrusted_since(&state.pool, payload.run_id).await {
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
    // A verdict already given, this turn, about this exact call. Answered straight rather than
    // asked again: the question would be registered and resolved in the same breath, which shows a
    // person a flicker of something already decided.
    //
    // Below the barrier and never above it, like everything else on this path — a turn that has
    // since read a stranger's words does not get to spend a verdict it earned before it did.
    let key = judge_key(&payload.tool_name, &payload.tool_input);
    if permission == crate::chats::PermissionMode::Auto && judged_this_turn(run_id, &key) {
        return Json(Decision {
            decision: "allow".to_owned(),
            reason: "already judged to follow from what was asked, earlier in this turn".to_owned(),
        });
    }

    // **A conversation that asks nobody.** Everything below this line asks a person; `dont_ask`
    // is the rung that has no person, so it stops here and answers instead.
    //
    // The POSITION is the feature, and each of the three boundaries around it is load-bearing:
    //
    // - **After the `deny` return** and after the third-party-text barrier, so this can never
    //   SOFTEN a refusal. A call those two turned down is already gone by the time control reaches
    //   here, and a future edit that moves this block above them would quietly convert their
    //   refusals into this one's — the same verdict with a weaker reason, and the barrier's warning
    //   never logged.
    // - **After `allowed_at`**, so every allow still runs. This rung's promise is that it permits
    //   PRECISELY what `auto` permits; moved above that block it would refuse the reads, the
    //   in-workspace writes and the recognised commands as well, which is not a stricter version of
    //   this feature but a different and useless one.
    // - **Before `ask_about`**, which is the point of the whole change. No `Ask` is ever registered,
    //   so nothing waits out the 45-second window for an answer nobody is there to give, and
    //   `spawn_judge` — reached only from inside the block below — never fires, so the rung costs no
    //   model call either.
    //
    // What it does NOT do: end the run. This refuses ONE tool call and returns, exactly as a
    // classifier `deny` does on every other rung; the turn goes on and the model may do something
    // else. Nothing here mints a proposal, stops a prober or terminates a conversation, and nothing
    // here should start.
    //
    // The reason is the classifier's own, with a clause appended rather than replaced: a refusal
    // that said only "this conversation asks nobody" would tell the model the rung and not the
    // fact, and the model's next attempt would be a guess.
    if permission == crate::chats::PermissionMode::DontAsk {
        return Json(Decision {
            decision: "deny".to_owned(),
            reason: format!("{}{DONT_ASK_CLAUSE}", classification.reason),
        });
    }

    if let Some(chat_id) = chat_of_run(&state.pool, run_id).await {
        let ask_id = crate::hooks::ask_about(
            &chat_id,
            run_id,
            &payload.tool_name,
            crate::execution::runner::detail_of(&payload.tool_input),
        );
        // **`auto` only.** Not `manual`, where a rung called Manual whose answer comes from a model
        // is a rung that lies; not `accept_edits`, one line above for the same reason; not `bypass`,
        // which is the rung of asking nobody; and not `plan`, whose restraint already comes from the
        // CLI's own flag and which is governed here without a second opinion. One mode has a judge,
        // and the reason each of the other four exists is to decide without one. `dont_ask` is not
        // among the four because it never arrives — it returned above, before the ask was
        // registered, which is what makes it the rung that summons no judge at all.
        if permission == crate::chats::PermissionMode::Auto {
            spawn_judge(
                state,
                ask_id,
                run_id,
                project_id.map(str::to_owned),
                root.to_owned(),
                payload.tool_name.clone(),
                payload.tool_input.clone(),
                key,
            );
        }
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
