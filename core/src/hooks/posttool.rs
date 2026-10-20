//! The PostToolUse outcome, the policy for an unrecognised context, and when parking would only destroy work.

use super::*;

/// Records the OUTCOME of a tool call this daemon already decided about. **Never a second
/// decision** — measured against the installed CLI (2.1.260), a `PostToolUse` response carries no
/// `permissionDecision`, because by the time this fires the tool has already run (or already
/// failed) and there is nothing left to block. Blocking here would be a second barrier, with its
/// own failure surface, on a cooperative path where the call already happened — an expensive
/// warning rather than a protection.
///
/// Named for what it does rather than for the event that triggers it: `posttooluse_decision`
/// would have been the wrong name for the one handler in this file that never returns a
/// `Decision`.
///
/// Wired to BOTH `PostToolUse` and `PostToolUseFailure` (`autopilot.rs`'s `wire_event`), which is
/// why `PostToolUsePayload::event` exists at all — `PostToolUse` alone would let this ledger
/// answer "did it happen?" only for a yes, and stay silent about every no.
pub async fn posttooluse_outcome(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Json(mut payload): Json<PostToolUsePayload>,
) -> StatusCode {
    // Exactly `pretooluse_decision`'s construction, for exactly its reason: a scoped key names its
    // own run and the daemon resolved that name from its own state, so the key decides and the
    // body's claim is dropped. A key naming no run (the control token) leaves the claim as the
    // only identifier there is.
    let run_id = match scope {
        Scope::Run(id) => id,
        _ => payload.run_id,
    };
    payload.run_id = run_id;

    // A command PreToolUse sent through the output filter comes back rewritten; the decision row
    // stores the original, so every consumer below must see that one.
    if let Some(original) =
        crate::execution::output_filter::original_input(&payload.tool_name, &payload.tool_input)
    {
        payload.tool_input = original;
    }

    // First, before the in-flight check below: a finished run's grant must still come back.
    release_admission(
        crate::execution::admission::installed().as_deref(),
        run_id,
        payload.tool_use_id.as_deref(),
    );

    // Validated against runs actually in flight, exactly as `pretooluse_decision` validates it
    // before trusting anything derived from it: the hook's environment sits inside the same
    // cooperative trust model as the token, so the daemon never blindly trusts a run_id it did not
    // itself hand out. A stray outcome naming a finished or unknown run has nothing to complete.
    if !state.run_handles.lock().unwrap().contains_key(&run_id) {
        return StatusCode::OK;
    }

    // Unconditional on `mode`, unlike `pretooluse_decision`'s call to `record_decision`:
    // `record_outcome` is UPDATE-only and matches nothing for a `real`-mode run, which never had a
    // decision row to begin with. Gating on mode here would only duplicate a check the UPDATE's
    // own WHERE clause already makes redundant.
    if let Err(error) = shadow::record_outcome(
        &state.pool,
        run_id,
        &payload.tool_name,
        &payload.tool_input,
        &payload.tool_response,
        &payload.event,
    )
    .await
    {
        tracing::warn!(
            run_id = run_id,
            %error,
            "posttooluse-outcome: failed to record outcome"
        );
    }

    // Always OK, on every path. There is no `permissionDecision` to give and nothing for a caller
    // to retry or appeal — this route only ever records, and a failure to record is this daemon's
    // problem, not the CLI's.
    StatusCode::OK
}

/// Gives back the admission grant a finished tool call held. False when there is no book, no
/// usable `tool_use_id`, or the call held nothing (a light command, or a second report).
pub(crate) fn release_admission(
    book: Option<&crate::execution::admission::AdmissionBook>,
    run_id: i64,
    tool_use_id: Option<&str>,
) -> bool {
    match (book, tool_use_id) {
        (Some(book), Some(id)) if !id.is_empty() => book.release(run_id, id),
        _ => false,
    }
}

/// The reason an orchestrator turn is refused a tool that would act. A constant because the tests
/// assert on it: every other refusal in this branch is also a `deny`, so only the reason tells
/// "the turn had read a stranger's words" apart from "the tool was not ours".
pub const UNTRUSTED_CONTEXT_DENY_REASON: &str =
    "this turn has read third-party content and can no longer act";

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
pub(super) async fn unrecognized_policy_for(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    mode: &str,
) -> crate::governance::classifier::Unrecognized {
    if !crate::execution::runs::runs_unattended(mode) {
        return crate::governance::classifier::Unrecognized::AsksAPerson;
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
        Some(_) => crate::governance::classifier::Unrecognized::MayBeConfined,
        None => crate::governance::classifier::Unrecognized::AsksAPerson,
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
pub(super) async fn a_park_here_would_only_destroy(
    pool: &sqlx::SqlitePool,
    run_id: i64,
    mode: &str,
) -> bool {
    if !crate::execution::runs::runs_unattended(mode) {
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

#[cfg(test)]
mod tests {
    use super::release_admission;
    use crate::execution::admission::{AdmissionBook, Poll};
    use std::time::Duration;

    fn book(capacity: u32) -> AdmissionBook {
        AdmissionBook::new(
            capacity,
            Duration::from_secs(900),
            Duration::from_secs(7200),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn an_outcome_with_a_tool_use_id_releases_its_grant() {
        let book = book(1);
        assert!(matches!(
            book.request(4, "toolu_done", 1),
            Poll::Granted { .. }
        ));
        assert_eq!(book.free(), 0);

        // An outcome without the id, or for another run's call, gives nothing back.
        assert!(!release_admission(Some(&book), 4, None));
        assert!(!release_admission(Some(&book), 4, Some("")));
        assert!(!release_admission(Some(&book), 5, Some("toolu_done")));
        assert!(!release_admission(None, 4, Some("toolu_done")));
        assert_eq!(book.free(), 0);

        assert!(release_admission(Some(&book), 4, Some("toolu_done")));
        assert_eq!(book.free(), 1);
        // The hook can fire twice (PostToolUse and a retry); the second is a no-op.
        assert!(!release_admission(Some(&book), 4, Some("toolu_done")));
        assert_eq!(book.free(), 1);
    }

    #[test]
    fn the_payload_carries_the_tool_use_id_the_hook_echoes() {
        let payload: crate::hooks::PostToolUsePayload = serde_json::from_value(serde_json::json!({
            "run_id": 4,
            "tool_name": "Bash",
            "tool_use_id": "toolu_x",
            "event": "PostToolUse"
        }))
        .unwrap();
        assert_eq!(payload.tool_use_id.as_deref(), Some("toolu_x"));
    }
}
