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
    transfer_package::forget(chat_id);
    let mut cut_turn = None;
    // A cut forgets the questions a turn had parked; the package still carries them.
    transfer_package::remember(chat_id, false, parked_now(chat_id));
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
