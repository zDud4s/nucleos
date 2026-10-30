//! stub: run resume after a stop

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::assistant_types::{
        install_turns, upsert_session, Origin, RecordingTurns, Sent, Turns, TURN_IN_PROGRESS,
    };
    use crate::chats::{self, Brain};
    use crate::execution::runner::Attachment;
    use crate::governance::autopilot::{classifier_hook_is_wired, wire_classifier_hook};
    use crate::sessions::had_in;
    use crate::state::{
        AppState, ROOTED_CHAT_PROGRESS_TIMEOUT_MULTIPLIER, ROOTED_CHAT_RUN_TIMEOUT_MULTIPLIER,
    };

    /// A directory a turn can be rooted in: the classifier hook is wired there.
    fn wired_dir() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        wire_classifier_hook(dir.path()).expect("wire the hook");
        assert!(classifier_hook_is_wired(dir.path()));
        dir
    }

    fn path_of(dir: &TempDir) -> &str {
        dir.path().to_str().expect("a UTF-8 temp path")
    }

    /// A chat picked up from `session_id`, had in `cwd`.
    async fn chat_from(state: &AppState, cwd: &str, session_id: &str) -> String {
        chats::create(&state.pool, Brain::Cloud, Some(&had_in(cwd, session_id)))
            .await
            .expect("create the chat")
    }

    /// `(chat_id, cwd)` of every chat picked up from `session_id`.
    async fn chats_from(state: &AppState, session_id: &str) -> Vec<(String, Option<String>)> {
        sqlx::query_as("SELECT chat_id, cwd FROM chats WHERE ide_session_id = ?")
            .bind(session_id)
            .fetch_all(&state.pool)
            .await
            .expect("read the chats")
    }

    #[tokio::test]
    async fn resume_launches_a_headless_turn_never_an_interactive_one() {
        let state = crate::channels::testkit::state().await;
        let recorder = RecordingTurns::install(&state);
        let dir = wired_dir();
        chat_from(&state, path_of(&dir), "another-session").await;
        let chat = chat_from(&state, path_of(&dir), "session-1").await;

        let resumed = resume(&state, "session-1", None).await.expect("resumed");

        assert_eq!(resumed.chat_id, chat);
        assert!(matches!(resumed.sent, Sent::Turn(_)), "{:?}", resumed.sent);
        assert!(!RESUME_PROMPT.trim().is_empty());
        assert_eq!(
            recorder.calls(),
            vec![(chat, RESUME_PROMPT.to_string(), Origin::Shell)]
        );
    }

    #[tokio::test]
    async fn resume_of_unknown_session_is_an_error() {
        let state = crate::channels::testkit::state().await;
        let recorder = RecordingTurns::install(&state);

        assert_eq!(
            resume(&state, "no-such-session", None).await,
            Err(ResumeError::UnknownSession)
        );
        assert!(recorder.calls().is_empty());
    }

    /// The state's own timeouts, shortened so a value taken from the defaults would not pass.
    #[tokio::test]
    async fn resume_uses_the_rooted_chat_timeouts() {
        let mut state = crate::channels::testkit::state().await;
        state.progress_timeout = Duration::from_secs(7);
        state.run_timeout = Duration::from_secs(11);
        RecordingTurns::install(&state);
        let dir = wired_dir();
        chat_from(&state, path_of(&dir), "session-1").await;

        let resumed = resume(&state, "session-1", None).await.expect("resumed");

        assert_eq!(
            resumed.silence,
            state.progress_timeout * ROOTED_CHAT_PROGRESS_TIMEOUT_MULTIPLIER
        );
        assert_eq!(
            resumed.ceiling,
            state.run_timeout * ROOTED_CHAT_RUN_TIMEOUT_MULTIPLIER
        );
    }

    /// A conversation given a new session is still found by it, not only by the one it came from.
    #[tokio::test]
    async fn resume_finds_the_chat_through_assistant_sessions() {
        let state = crate::channels::testkit::state().await;
        let recorder = RecordingTurns::install(&state);
        let dir = wired_dir();
        chat_from(&state, path_of(&dir), "another-session").await;
        let chat = chat_from(&state, path_of(&dir), "origin-session").await;
        upsert_session(
            &state.pool,
            &chat,
            "rotated-session",
            "2026-10-30T10:00:00+00:00",
        )
        .await
        .expect("record the session");

        let resumed = resume(&state, "rotated-session", None)
            .await
            .expect("resumed");

        assert_eq!(resumed.chat_id, chat);
        assert_eq!(
            recorder.calls(),
            vec![(chat, RESUME_PROMPT.to_string(), Origin::Shell)]
        );
    }

    #[tokio::test]
    async fn resume_adopts_an_external_session_given_its_cwd() {
        let state = crate::channels::testkit::state().await;
        let recorder = RecordingTurns::install(&state);
        let dir = wired_dir();

        let resumed = resume(&state, "external-session", Some(path_of(&dir)))
            .await
            .expect("resumed");

        assert_eq!(
            chats_from(&state, "external-session").await,
            vec![(resumed.chat_id.clone(), Some(path_of(&dir).to_string()))]
        );
        assert_eq!(
            recorder.calls(),
            vec![(resumed.chat_id, RESUME_PROMPT.to_string(), Origin::Shell)]
        );
    }

    /// Without a directory the turn would run tool-less. The `cwd` argument cannot make up for it:
    /// it only adopts a session that has no chat yet.
    #[tokio::test]
    async fn resume_refuses_a_chat_with_no_cwd() {
        let state = crate::channels::testkit::state().await;
        let recorder = RecordingTurns::install(&state);
        let dir = wired_dir();
        let chat = chats::create(&state.pool, Brain::Cloud, None)
            .await
            .expect("create the chat");
        sqlx::query("UPDATE chats SET ide_session_id = ? WHERE chat_id = ?")
            .bind("session-1")
            .bind(&chat)
            .execute(&state.pool)
            .await
            .expect("name the session");

        for cwd in [None, Some(path_of(&dir))] {
            let refused = resume(&state, "session-1", cwd).await;
            assert!(matches!(refused, Err(ResumeError::NotRooted(_))), "{refused:?}");
        }
        assert!(recorder.calls().is_empty());
    }

    /// Both ways in: a chat that already lives in the directory, and an adoption, which must leave
    /// no chat behind.
    #[tokio::test]
    async fn resume_refuses_a_directory_without_the_hook() {
        let state = crate::channels::testkit::state().await;
        let recorder = RecordingTurns::install(&state);
        let dir = TempDir::new().expect("tempdir");
        chat_from(&state, path_of(&dir), "session-1").await;

        let known = resume(&state, "session-1", None).await;
        assert!(matches!(known, Err(ResumeError::NotRooted(_))), "{known:?}");

        let adopted = resume(&state, "external-session", Some(path_of(&dir))).await;
        assert!(matches!(adopted, Err(ResumeError::NotRooted(_))), "{adopted:?}");
        assert!(chats_from(&state, "external-session").await.is_empty());

        assert!(recorder.calls().is_empty());
    }

    #[tokio::test]
    async fn resume_with_no_turns_wired_says_so() {
        let state = crate::channels::testkit::state().await;
        let dir = wired_dir();
        chat_from(&state, path_of(&dir), "session-1").await;

        assert_eq!(
            resume(&state, "session-1", None).await,
            Err(ResumeError::NotWired)
        );
    }

    /// Refuses every turn, the way the assistant does when the chat is already answering one.
    struct RefusingTurns;

    #[async_trait::async_trait]
    impl Turns for RefusingTurns {
        async fn send_or_queue(
            &self,
            _state: &AppState,
            _chat_id: &str,
            _text: &str,
            _images: &[Attachment],
            _origin: Origin,
        ) -> Result<Sent, String> {
            Err(TURN_IN_PROGRESS.to_string())
        }

        async fn say_now_from(
            &self,
            _state: &AppState,
            _chat_id: &str,
            _text: &str,
            _origin: Origin,
        ) -> Result<(), String> {
            Err(TURN_IN_PROGRESS.to_string())
        }
    }

    #[tokio::test]
    async fn resume_passes_a_turn_refusal_through() {
        let state = crate::channels::testkit::state().await;
        install_turns(&state, Arc::new(RefusingTurns));
        let dir = wired_dir();
        chat_from(&state, path_of(&dir), "session-1").await;

        assert_eq!(
            resume(&state, "session-1", None).await,
            Err(ResumeError::Refused(TURN_IN_PROGRESS.to_string()))
        );
    }
}
