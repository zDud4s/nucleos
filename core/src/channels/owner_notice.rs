//! Tells the owner, through the primary channel, that work is waiting on them or has ended while
//! they were away: a question parked in `ask_about_call`, or a long turn that finished.
//!
//! The text never carries tool input, a command or a file's contents: only the tool's name and a
//! link into the app, the same restraint as `Ask.detail`. A failure to notify is a warning and
//! nothing more; the question and the turn go on as if this module did not exist.

use std::time::Duration;

use chrono::Utc;

use crate::channels::outbox::{self, Class, NewAction};
use crate::channels::{access, contract};
use crate::state::AppState;

/// How long a question stays parked before the owner is told. Long enough that somebody in front
/// of the screen answers first, and that `auto` has already given its verdict.
pub const ASK_NOTICE_DELAY: Duration = Duration::from_secs(60);

/// A turn that ran at least this long tells the owner when it ends.
pub const LONG_TURN_NOTICE: Duration = Duration::from_secs(10 * 60);

/// The longest tool name that goes out; a name is the model's choice, so it is capped.
const TOOL_NAME_MAX: usize = 64;

/// Enqueues one notification to the primary channel's home conversation. `false` when there is no
/// destination (no primary, or it has no home): the owner then sees it in the app only.
pub async fn notify_owner(state: &AppState, text: String) -> sqlx::Result<bool> {
    let Some(primary) = access::primary(&state.pool).await? else {
        return Ok(false);
    };
    let Some(home) = access::home_conversation(&state.pool, primary).await? else {
        return Ok(false);
    };
    let mut tx = state.pool.begin().await?;
    outbox::enqueue(
        &mut tx,
        &contract::ConversationKey::new(primary, home),
        NewAction::Send {
            reply_to: None,
            parts: vec![contract::OutPart::Text { text }],
            buttons: None,
            silent: false,
        },
        Class::Notification,
        None,
        Utc::now(),
    )
    .await?;
    tx.commit().await?;
    outbox::wake(state, primary);
    Ok(true)
}

/// `{public_origin}/chats/{chat_id}` when the remote gateway is set up, `None` otherwise.
fn chat_link(state: &AppState, chat_id: &str) -> Option<String> {
    let root = state.machine_config_root.as_deref()?;
    let config = crate::remote::RemoteConfig::load(&root.join(crate::remote::REMOTE_FILE));
    if !config.enabled || config.validate().is_err() {
        return None;
    }
    Some(format!(
        "{}/chats/{chat_id}",
        config.public_origin.trim_end_matches('/')
    ))
}

fn with_link(state: &AppState, chat_id: &str, text: String) -> String {
    match chat_link(state, chat_id) {
        Some(link) => format!("{text}\n{link}"),
        None => text,
    }
}

async fn notify_or_warn(state: &AppState, text: String) {
    if let Err(error) = notify_owner(state, text).await {
        tracing::warn!(%error, "could not tell the owner through the primary channel");
    }
}

/// After `ASK_NOTICE_DELAY`, tells the owner about the question `ask_id` if it still stands. Run
/// from a task of its own: the hook that asked has 5 s to answer.
pub fn spawn_ask_notice(state: AppState, chat_id: String, ask_id: String, tool: String) {
    tokio::spawn(async move {
        tokio::time::sleep(ASK_NOTICE_DELAY).await;
        notice_if_open(&state, &chat_id, &ask_id, &tool).await;
    });
}

/// The part of `spawn_ask_notice` after the wait.
async fn notice_if_open(state: &AppState, chat_id: &str, ask_id: &str, tool: &str) {
    if !crate::hooks::ask_is_open(ask_id) {
        return;
    }
    let tool: String = tool.chars().take(TOOL_NAME_MAX).collect();
    let text = with_link(
        state,
        chat_id,
        format!("NucleOS is waiting for your answer: the {tool} tool needs permission."),
    );
    notify_or_warn(state, text).await;
}

/// Called when a turn ends. A turn that ran for `LONG_TURN_NOTICE` or more tells the owner, unless
/// it came in through a channel: that conversation already carries the reply.
pub async fn long_turn_ended(
    state: &AppState,
    chat_id: &str,
    elapsed: Duration,
    from_channel: bool,
) {
    if from_channel || elapsed < LONG_TURN_NOTICE {
        return;
    }
    let text = with_link(state, chat_id, "A long turn has finished.".to_owned());
    notify_or_warn(state, text).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::testkit;

    const HOME: &str = "-100";

    /// A state whose primary is `telegram` with a home conversation, and a gateway at
    /// `https://box.example.com` unless `home` is false (then no home is registered).
    async fn rig(home: bool) -> (AppState, tempfile::TempDir) {
        let mut state = testkit::state().await;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(crate::remote::REMOTE_FILE),
            "enabled: true\npublic_origin: \"https://box.example.com\"\n",
        )
        .unwrap();
        state.machine_config_root = Some(tmp.path().to_path_buf());
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram'")
            .execute(&state.pool)
            .await
            .unwrap();
        if home {
            sqlx::query(
                "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
                 conversation_kind, home, created_at) \
                 VALUES ('telegram', '42', ?, 'group', 1, 't')",
            )
            .bind(HOME)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        (state, tmp)
    }

    /// Every outbox row as (conversation, class, text), oldest first.
    async fn rows(state: &AppState) -> Vec<(String, String, String)> {
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT conversation, class, payload FROM channel_outbox ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(conversation, class, payload)| {
                let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
                let text = v["parts"][0]["text"].as_str().unwrap().to_owned();
                (conversation, class, text)
            })
            .collect()
    }

    #[tokio::test]
    async fn an_open_question_reaches_the_owner_after_the_delay() {
        let (state, _tmp) = rig(true).await;
        let ask_id = crate::hooks::ask_about("notice-chat-1", 910_001, "Bash", None);

        notice_if_open(&state, "notice-chat-1", &ask_id, "Bash").await;

        let rows = rows(&state).await;
        assert_eq!(rows.len(), 1);
        let (conversation, class, text) = &rows[0];
        assert_eq!(
            (conversation.as_str(), class.as_str()),
            (HOME, "notification")
        );
        assert!(text.contains("Bash"), "{text}");
        assert!(
            text.ends_with("https://box.example.com/chats/notice-chat-1"),
            "{text}"
        );
        crate::hooks::forget_asks_for_chat("notice-chat-1");
    }

    #[tokio::test]
    async fn a_question_answered_in_time_sends_nothing() {
        let (state, _tmp) = rig(true).await;
        let ask_id = crate::hooks::ask_about("notice-chat-2", 910_002, "Bash", None);
        crate::hooks::forget_asks_for_chat("notice-chat-2");

        notice_if_open(&state, "notice-chat-2", &ask_id, "Bash").await;

        assert!(rows(&state).await.is_empty());
    }

    #[tokio::test]
    async fn no_home_conversation_sends_nothing_and_does_not_fail() {
        let (state, _tmp) = rig(false).await;
        assert!(!notify_owner(&state, "hello".to_owned()).await.unwrap());
        let ask_id = crate::hooks::ask_about("notice-chat-3", 910_003, "Bash", None);

        notice_if_open(&state, "notice-chat-3", &ask_id, "Bash").await;

        assert!(rows(&state).await.is_empty());
        crate::hooks::forget_asks_for_chat("notice-chat-3");
    }

    #[tokio::test]
    async fn without_a_gateway_the_message_goes_out_without_a_link() {
        let (mut state, _tmp) = rig(true).await;
        state.machine_config_root = None;
        let ask_id = crate::hooks::ask_about("notice-chat-4", 910_004, "Edit", None);

        notice_if_open(&state, "notice-chat-4", &ask_id, "Edit").await;

        let rows = rows(&state).await;
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].2.contains("http"), "{}", rows[0].2);
        crate::hooks::forget_asks_for_chat("notice-chat-4");
    }

    #[tokio::test]
    async fn a_long_turn_that_ends_tells_the_owner() {
        let (state, _tmp) = rig(true).await;

        long_turn_ended(
            &state,
            "c",
            LONG_TURN_NOTICE - Duration::from_secs(1),
            false,
        )
        .await;
        assert!(rows(&state).await.is_empty(), "a short turn says nothing");

        long_turn_ended(&state, "c", LONG_TURN_NOTICE, false).await;
        let rows = rows(&state).await;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].2.ends_with("https://box.example.com/chats/c"));
    }

    #[tokio::test]
    async fn a_channel_turn_does_not_tell_twice() {
        let (state, _tmp) = rig(true).await;

        long_turn_ended(&state, "c", LONG_TURN_NOTICE * 2, true).await;

        assert!(rows(&state).await.is_empty());
    }
}
