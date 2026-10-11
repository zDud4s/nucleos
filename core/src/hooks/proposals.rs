//! Advice on a proposal before it is filed, and telling the council about it.

use super::*;

/// How often the advice looks to see whether the council has settled.
///
/// Fifteen seconds against a deliberation measured in minutes: the poll costs one indexed lookup by
/// primary key, and the alternative — a channel the council signals — would mean `council.rs`
/// knowing that proposals exist, which is a dependency in the wrong direction for a feature that is
/// best-effort by design.
pub(super) const PROPOSAL_ADVICE_POLL: std::time::Duration = std::time::Duration::from_secs(15);

/// How long the advice waits before giving up on a council.
///
/// Longer than any roster's `timeout_seconds` may be (`MAX_COUNCIL_TIMEOUT_SECONDS` is an hour, per
/// SEAT, and a council runs its seats in parallel), so this is a backstop against a driver that
/// died rather than a second clock racing the first one. `council::reconcile` settles an abandoned
/// council at the next daemon startup, so the usual end of a stuck council is a terminal status
/// arriving late, not this ceiling — but a daemon that never restarts would otherwise leave this
/// task alive for the process's whole life.
pub(super) const PROPOSAL_ADVICE_CEILING: std::time::Duration =
    std::time::Duration::from_secs(2 * 60 * 60);

/// The question the council is asked about a refused action.
///
/// Deliberately phrased as "what should the person weigh", not "should this be allowed". The
/// council is not the arbiter here and must not be invited to behave like one — `.ai/decisions.md`
/// fixed that the arbiter of an ambiguity is the human, and a synthesis written as a verdict is one
/// a tired person approves without reading.
pub(super) fn proposal_advice_question(tool_name: &str, reason: &str, tool_input: &str) -> String {
    format!(
        "An autonomous agent was stopped mid-task because it tried to do something the daemon would \
         not let it do unsupervised. A person is going to decide whether to allow it. You are NOT \
         that person and you are not deciding: say what they should weigh.\n\n\
         The tool it reached for: {tool_name}\n\
         Why it was stopped: {reason}\n\
         What it asked for: {tool_input}\n\n\
         What could this do that is not obvious from reading it? What would make it safe, and what \
         would make it a mistake? If it is plainly routine, say so plainly — a long answer to an \
         easy question wastes the reader's attention on the one that is not."
    )
}

/// Puts a council behind a proposal, so the person deciding it has an opinion to read.
///
/// **A NOTE, never a verdict.** `proposals::note` writes a `proposal_events` row whose `from_status`
/// and `to_status` are both the status the proposal already has — it decides nothing, and it exists
/// for exactly this. `transition` is not called here and must not be: `.ai/decisions.md` records
/// that the arbiter of an ambiguity is the human and that the council gates nothing, so a council
/// approving its own advice would contradict a standing decision rather than extend a feature.
///
/// **Best-effort, exactly like `record_refused_action` beside it.** Detached into its own task and
/// returning nothing: the proposal is already written and the run is already parked by the time
/// this starts, and failing to advise must not change what happens to either. Off unless the owner
/// asked — `advises_proposals` is false with no roster, which is the shipped state.
pub(super) fn advise_on_proposal(
    state: &AppState,
    proposal_id: i64,
    tool_name: &str,
    reason: &str,
    tool_input: &str,
) {
    if !state.council.advises_proposals() {
        return;
    }
    let state = state.clone();
    let question = proposal_advice_question(tool_name, reason, tool_input);
    tokio::spawn(async move {
        let council_id = match crate::council::start(&state, &question, None).await {
            Ok(id) => id,
            // Unreachable while `advises_proposals` implies a roster, and silent anyway: "there is
            // no council" is not a failure of the proposal.
            Err(crate::council::StartError::NotConfigured) => return,
            Err(error) => {
                tracing::warn!(
                    proposal_id,
                    %error,
                    "a proposal's council would not start; the proposal stands unadvised"
                );
                return;
            }
        };
        note_the_council_on(
            &state,
            proposal_id,
            &council_id,
            PROPOSAL_ADVICE_POLL,
            PROPOSAL_ADVICE_CEILING,
        )
        .await;
    });
}

/// Waits for one council to settle and writes its synthesis onto a proposal as a note.
///
/// The clock is a parameter rather than the constants above, so the tests can walk both endings —
/// a council that answers and one that never does — without spending the real ceiling on the second
/// one.
///
/// Every ending except "settled with a synthesis" leaves the proposal untouched: a council that
/// errored, one that was cancelled, one whose row vanished, one whose chairman left no transcript,
/// and one that outran the ceiling. The proposal is a thing a person will decide either way, and a
/// note saying the council failed would be a line of noise in the one place attention is scarce.
pub(super) async fn note_the_council_on(
    state: &AppState,
    proposal_id: i64,
    council_id: &str,
    poll: std::time::Duration,
    ceiling: std::time::Duration,
) {
    let deadline = std::time::Instant::now() + ceiling;
    loop {
        match crate::council::get_council_row(&state.pool, council_id).await {
            Ok(Some(row)) if row.is_settled() => {
                let Some(synthesis) = crate::council::synthesis_of(&state.pool, &row).await else {
                    return;
                };
                if let Err(error) = crate::proposals::note(
                    &state.pool,
                    proposal_id,
                    &format!("a council was asked about this action and said:\n\n{synthesis}"),
                )
                .await
                {
                    tracing::warn!(proposal_id, council_id, %error, "could not note a council on a proposal");
                }
                return;
            }
            Ok(Some(_)) => {}
            // Gone: pruned, or deleted. Nothing to wait for.
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(proposal_id, council_id, %error, "could not read a proposal's council");
                return;
            }
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                proposal_id,
                council_id,
                "a proposal's council did not settle within the ceiling; the proposal stands unadvised"
            );
            return;
        }
        tokio::time::sleep(poll).await;
    }
}
