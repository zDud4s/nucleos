//! The assistant's side of the hook `nucleos-core` calls to start a turn (voice, the browser panel).

use std::sync::Arc;

use crate::assistant_types::Turns;
use crate::execution::runner::Attachment;
use crate::state::AppState;

use super::{Origin, Sent};

/// The real turns: each method is the assistant function of the same name.
struct AssistantTurns;

#[async_trait::async_trait]
impl Turns for AssistantTurns {
    async fn send_or_queue(
        &self,
        state: &AppState,
        chat_id: &str,
        text: &str,
        images: &[Attachment],
        origin: Origin,
    ) -> Result<Sent, String> {
        super::send_or_queue(state, chat_id, text, images, origin).await
    }

    async fn say_now_from(
        &self,
        state: &AppState,
        chat_id: &str,
        text: &str,
        origin: Origin,
    ) -> Result<(), String> {
        super::say_now_from(state, chat_id, text, origin)
            .await
            .map(|_| ())
    }
}

/// Wires the assistant into `state.turns`, so voice and the browser panel can start a turn. Called
/// once by the daemon after it builds its `AppState`, and by any test state that exercises them.
pub fn install_turns(state: &AppState) {
    crate::assistant_types::install_turns(state, Arc::new(AssistantTurns));
}
