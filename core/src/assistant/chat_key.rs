//! Which key a rooted turn's CLI carries, and when that key is minted.
//!
//! A key is minted only when a CLI process is SPAWNED. A kept process keeps the key it was spawned
//! with — its environment is handed over once — so minting rotates `chat_tokens` and a key minted
//! for a turn that goes down a kept process would retire the very key that process still holds.
//! That was the 401 on turn two: every hook call refused, the hook failing closed, and the
//! conversation unable to use a tool.

/// The environment a turn's CLI is spawned with.
///
/// A turn that may keep a process carries an unstored placeholder key here, and [`rekey`] replaces
/// it at the moment `serve_turn` spawns one; a turn that cannot keep a process is keyed now, since
/// the process it starts is the one that holds the key.
pub(super) async fn turn_env(
    tool_policy: crate::execution::runner::ToolPolicy,
    may_live: bool,
    pool: &sqlx::SqlitePool,
    chat_id: &str,
    run_id: i64,
    control_token: &str,
) -> Vec<(String, String)> {
    // WHICH key this turn carries follows from what it can read, and the two must be decided
    // together or not at all.
    //
    // An orchestrator turn carries the daemon's control token — the key that approves proposals
    // and disengages the kill switch — and the only reason that is safe is that `McpOnly` leaves
    // it no Bash, no Read and no Write to look at its own environment with. A rooted turn has
    // all three. Handing it the same key would rebuild, exactly, the hole `runs.rs` records
    // having already closed once: a run with Bash whose classifier calls `echo
    // $NUCLEOS_DAEMON_TOKEN` a `read-local` action.
    //
    // So it gets a scoped key of its own instead, and loses the power to approve proposals and
    // to disengage the brake. That is the right thing to lose: it is `Origin::Shell` that earned
    // it the tools, which means the person asking is sitting at this machine, in front of the
    // window where both of those are one click away.
    //
    // Scoped to the CONVERSATION and not to this turn, which is the one difference from a
    // `worktree` run and the reason `chat_tokens` exists. A CLI is handed its environment once,
    // at spawn; a key naming this turn is therefore a key the process still presents on turn
    // five, and a process that cannot outlive its turn pays the whole cost of starting again —
    // measured at 5.8s to `init` and 26.4s to a first shell command, against 1.5s and 6.7s down
    // a stdin that is already open. `auth::resolve` reads a `chat:` key against whatever turn of
    // the conversation is running, so the key stays true as the turns change under it, and names
    // nothing at all in between.
    //
    // A process is keyed when it is SPAWNED, not when a turn is written to it: minting rotates
    // `chat_tokens`, so a key minted for a turn that goes down a kept process would retire the
    // very key that process still holds. A turn that may keep a process therefore carries an
    // unstored placeholder here, and `serve_turn` mints the real key (see [`rekey`]) at the
    // moment it spawns one. If any path skipped that, the placeholder matches nothing and the
    // turn is refused.
    let key = match tool_policy {
        crate::execution::runner::ToolPolicy::Unrestricted if may_live => {
            unstored_chat_key(chat_id)
        }
        // A rooted turn that cannot keep a process (a Codex one) can still find the conversation's
        // Claude process kept from an earlier turn: the pin moved from a Claude model to a Codex
        // one, and moving it back would make that process fit again. Minting below retires the key
        // it holds, so it goes first — kept, it would refuse every hook call it made from then on.
        crate::execution::runner::ToolPolicy::Unrestricted => {
            super::evict_live(chat_id);
            fresh_chat_key(pool, chat_id).await
        }
        _ => control_token.to_string(),
    };
    crate::execution::runs::run_env(&key, run_id, None, crate::speed::Capacity::solo())
}

/// Puts a freshly minted key for the conversation into `env`, replacing the one it carries.
///
/// Called by `serve_turn` on the path that starts a process, and only there.
pub(super) async fn rekey(env: &mut Vec<(String, String)>, pool: &sqlx::SqlitePool, chat_id: &str) {
    let key = fresh_chat_key(pool, chat_id).await;
    match env
        .iter_mut()
        .find(|(name, _)| name == "NUCLEOS_DAEMON_TOKEN")
    {
        Some(entry) => entry.1 = key,
        None => env.push(("NUCLEOS_DAEMON_TOKEN".to_string(), key)),
    }
}

/// A conversation-scoped key that was never stored, so nothing can match it.
///
/// Fails closed: a request presenting it is refused rather than ungoverned.
pub(super) fn unstored_chat_key(chat_id: &str) -> String {
    format!("chat:{chat_id}.{}", crate::auth::mint_secret("chat"))
}

/// Mints the conversation's key, rotating the one stored for it.
///
/// Called exactly when a CLI process is about to be spawned with that key in its environment — the
/// previous key is retired, so it must never be minted for a turn written down a process that is
/// already holding the old one.
pub(super) async fn fresh_chat_key(pool: &sqlx::SqlitePool, chat_id: &str) -> String {
    match crate::auth::mint_chat_token(pool, chat_id).await {
        Ok(key) => key,
        Err(error) => {
            tracing::warn!(
                chat_id = %chat_id,
                %error,
                "could not store the conversation's key — this turn's tool calls will be refused"
            );
            // Nothing was stored, so nothing can match, so the turn is refused rather
            // than ungoverned. `runs::mint_run_token` fails in the same direction, and
            // for the same reason: of the two ways to be wrong here, only one of them
            // leaves a run acting with nobody watching.
            unstored_chat_key(chat_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A turn keyed now, because it cannot keep a process, retires the conversation's stored key.
    /// A process kept for that conversation holds the retired key, so it must not outlive the mint.
    #[tokio::test]
    async fn keying_a_turn_that_cannot_keep_a_process_evicts_the_kept_one() {
        let pool = crate::testdb::fresh_pool().await;
        let chat_id = "chat-key-evicts-kept";
        let (messages, _said) = tokio::sync::mpsc::unbounded_channel();
        let (_events, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_stopped, stopped_rx) = tokio::sync::watch::channel(None);
        let kept = super::super::LiveChat::new(
            messages,
            events_rx,
            std::sync::Arc::new(std::sync::Mutex::new(Some("s-1".to_owned()))),
            tokio::spawn(async {}).abort_handle(),
            stopped_rx,
            crate::execution::runner::Permission::Default,
            None,
            String::new(),
            None,
        );
        super::super::relock(&super::super::LIVE_CHATS).insert(chat_id.to_owned(), kept);

        turn_env(
            crate::execution::runner::ToolPolicy::Unrestricted,
            false,
            &pool,
            chat_id,
            1,
            "control",
        )
        .await;

        assert!(
            !super::super::relock(&super::super::LIVE_CHATS).contains_key(chat_id),
            "a kept process outlived the key it holds"
        );
    }
}
