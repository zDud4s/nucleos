//! The assistant half of moving a chat to another machine: the parts of `chats::transfer` that have
//! to stop or restart a turn, and so cannot sit below the assistant (`nucleos-core` cannot see it).

use crate::chats::transfer::{Refusal, When, freeze_at, touch_at, unix_now};
use crate::chats::transfer_package;

/// The questions the hook store holds for the chat right now. The store lives in this crate, so the
/// lower `chats` code takes them as a parameter.
pub fn parked_now(chat_id: &str) -> Vec<transfer_package::ParkedQuestion> {
    crate::hooks::asks_for(chat_id)
        .into_iter()
        .map(|ask| transfer_package::ParkedQuestion {
            tool: ask.tool,
            detail: ask.detail,
        })
        .collect()
}

/// What `begin` did.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Begun {
    /// The turn `now` cut, if one was running.
    pub cut_turn: Option<i64>,
    /// Messages waiting, which the package carries.
    pub queued: usize,
}

/// How long one wait for the turn to end lasts before the freeze is confirmed alive again.
const WAIT_SLICE: std::time::Duration = std::time::Duration::from_secs(60);

/// Freezes the chat, then deals with a turn in flight: `AfterTurn` waits for it (a turn that never
/// ends keeps the freeze alive, and an `abort` meanwhile ends the wait with a refusal), `Now` cuts
/// it. Returns when no turn is running here and nothing new can start. Any refusal after the freeze
/// leaves it standing for the caller to `abort`, except a lost freeze, which is already gone.
pub async fn begin(
    state: &crate::state::AppState,
    chat_id: &str,
    when: When,
    transfer_id: &str,
) -> Result<Begun, Refusal> {
    freeze_at(&state.pool, chat_id, transfer_id, unix_now()).await?;
    // A retry for this transfer sees the same freeze. Its remembered cut and questions are the
    // only record left after a cut, so they must survive until package or abort resolves it.
    // State of another transfer (an expired freeze never aborted) is dropped.
    if transfer_package::claim(chat_id, transfer_id) {
        transfer_package::remember(chat_id, false, parked_now(chat_id));
    }
    let mut cut_turn = None;
    // A cut forgets the questions a turn had parked; the package still carries them.
    match when {
        When::Now => {
            cut_turn = super::cut_turn_for_move(state, chat_id).await;
            transfer_package::remember(chat_id, cut_turn.is_some(), parked_now(chat_id));
        }
        When::AfterTurn => {
            while !super::wait_for_turn_end(chat_id, WAIT_SLICE).await {
                if !touch_at(&state.pool, chat_id, transfer_id, unix_now()).await? {
                    return Err(Refusal::NotFrozenForThis);
                }
            }
        }
    }
    if !touch_at(&state.pool, chat_id, transfer_id, unix_now()).await? {
        return Err(Refusal::NotFrozenForThis);
    }
    let queued = crate::chats::queued(&state.pool, chat_id).await?.len();
    Ok(Begun { cut_turn, queued })
}

/// Lifts the freeze and lets the queue drain again, which is what `abort` and a dropped
/// connection mean: the chat goes on working here.
pub async fn abort_and_resume(
    state: &crate::state::AppState,
    chat_id: &str,
    transfer_id: &str,
) -> sqlx::Result<bool> {
    let lifted = crate::chats::transfer::abort(&state.pool, chat_id, transfer_id).await?;
    transfer_package::forget(chat_id);
    if lifted {
        super::resume_queue_after_freeze(state, chat_id);
    }
    Ok(lifted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn begin_with_a_new_transfer_id_drops_state_from_an_expired_transfer() {
        let state = crate::channels::testkit::state().await;
        let chat = "telegram:chat-move-expired";
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(chat)
            .bind("2026-01-01T00:00:00Z")
            .execute(&state.pool)
            .await
            .unwrap();
        // What an earlier transfer left behind when its freeze expired without an abort.
        assert!(transfer_package::claim(chat, "t-old"));
        transfer_package::remember(
            chat,
            true,
            vec![transfer_package::ParkedQuestion {
                tool: "Bash".to_owned(),
                detail: Some("stale question".to_owned()),
            }],
        );

        begin(&state, chat, When::AfterTurn, "t-new").await.unwrap();

        let store = crate::execution::git_exec::testkit::space_free_tempdir("nucleos-move-stale-");
        let package = transfer_package::build(&state.pool, chat, "t-new", store.path(), vec![])
            .await
            .unwrap();
        assert!(!package.interrupted, "the old transfer's cut is forgotten");
        assert!(package.parked.is_empty(), "{:?}", package.parked);
        transfer_package::forget(chat);
    }

    /// A chat that arrives without its transcript has no session, so the first send takes the
    /// rotation path, which reads the imported exchanges back and replays them in front of the
    /// new message. No handover is needed for that.
    #[tokio::test]
    async fn the_first_send_after_a_transcriptless_import_replays_the_imported_exchanges() {
        let state = crate::channels::testkit::state().await;
        let chat = "imported-no-transcript";
        let row = |n: u32, untrusted: i64| {
            serde_json::json!({
                "prompt": format!("question {n}"), "stdout": format!("answer {n}"), "stderr": null,
                "status": "completed", "exit_code": 0, "cost_usd": 0.1, "answered_by": null,
                "session_id": "gone", "created_at": format!("2026-02-01T00:00:{n:02}.000Z"),
                "completed_at": format!("2026-02-01T00:00:{n:02}.500Z"), "model": null,
                "cache_ttl": null, "context_fill": null, "tools_used": null, "thought": null,
                "thought_tokens": null, "compacted": 0, "origin": null,
                "read_untrusted": untrusted, "input_tokens": null, "output_tokens": null,
                "cache_read_tokens": null, "cache_creation_tokens": null, "num_turns": null,
            })
        };
        // Exchange 2 read third-party text: the barrier floors the replay above it.
        let package: transfer_package::Package = serde_json::from_value(serde_json::json!({
            "transfer_id": "t-replay", "chat_id": chat,
            "chat": {"title": null, "brain": "cloud", "created_at": "2026-01-01T00:00:00Z",
                     "model": null, "effort": null, "plan_only": false},
            "session": null, "code": null, "queue": [], "parked": [], "interrupted": false,
            "history": [row(1, 0), row(2, 1), row(3, 0), row(4, 0)],
        }))
        .unwrap();
        transfer_package::import(
            &state.pool,
            &package,
            std::path::Path::new("unused"),
        )
        .await
        .unwrap();

        assert_eq!(crate::assistant::get_session(&state.pool, chat).await.unwrap(), None);
        let (handoff, older, tail) = super::compaction::replay_window(&state.pool, chat)
            .await
            .unwrap();
        assert!(handoff.is_none() && older.is_empty());
        assert_eq!(
            tail,
            vec![
                ("question 3".to_owned(), "answer 3".to_owned()),
                ("question 4".to_owned(), "answer 4".to_owned()),
            ]
        );
        let sent = super::send::replayed_after(None, &tail, "and now?");
        let at = |needle: &str| sent.find(needle).unwrap_or_else(|| panic!("{needle} in {sent}"));
        assert!(at("you: question 3") < at("you: question 4") && at("you: question 4") < at("and now?"));
        assert!(!sent.contains("question 2"), "the untrusted barrier floors the replay");
    }
}
