//! A linked child's final reply, delivered to its parent conversation.
//!
//! A browser agent is a conversation of its own (`chats::links`). When one of its turns completes,
//! what it answered travels back to the conversation that asked, as an ordinary relay: through
//! `relay::admit` and every brake it has, then `send_relayed_or_queue`. A refused delivery opens no
//! turn; it becomes a `browser_reply` notice the parent's next turn sees once, and every delivery
//! that was attempted leaves a `chat_link_deliveries` row for the panel.

/// What became of a linked child's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The parent was free: the reply opened this turn there.
    Delivered { turn_id: i64 },
    /// The parent was busy: the reply waits in its queue, carrying its relay id.
    Queued,
    /// The relay gate (or the child turn's unreadable origin) refused it; `reason` is the slug.
    Refused { reason: String },
    /// The child has no active link with a parent conversation.
    NotLinked,
    /// The reply was empty or only whitespace.
    Empty,
}

/// Delivers `reply`, the final answer of the child turn `child_run_id`, to the child's parent.
// RED stub
pub async fn deliver_reply(
    state: &crate::state::AppState,
    child_chat_id: &str,
    child_run_id: i64,
    reply: &str,
) -> DeliveryOutcome {
    let _ = (state, child_chat_id, child_run_id, reply);
    todo!("RED stub")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::Origin;
    use crate::execution::runner::FakeCommandRunner;
    use crate::state::AppState;
    use sqlx::SqlitePool;
    use std::sync::Arc;
    use std::time::Duration;

    async fn test_state() -> AppState {
        AppState {
            startup_config: Default::default(),
            token: crate::auth::Token("t".into()),
            pool: crate::testdb::fresh_pool().await,
            channels: Default::default(),
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            utility_model: None,
            assistants: Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            fleet: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// The owner is at the machine now: the one row `attention::owner_is_present` reads.
    async fn owner_present(pool: &SqlitePool) {
        crate::attention::record_heartbeat(
            pool,
            &crate::attention::AttentionScope::Global,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    }

    /// A live cloud conversation with a fresh id (slots are process-global, so ids must not repeat
    /// across tests), titled when `title` is given.
    async fn a_conversation(pool: &SqlitePool, title: Option<&str>) -> String {
        let chat_id = crate::chats::create(pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        crate::chats::rename(pool, &chat_id, title).await.unwrap();
        chat_id
    }

    /// A finished turn a person typed in `chat_id`.
    async fn typed_turn(pool: &SqlitePool, chat_id: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, origin, created_at)
             VALUES ('find me a hotel', 'completed', 'assistant', ?, 'shell', ?) RETURNING id",
        )
        .bind(chat_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A finished turn in `chat_id` born from `relay_id`, as `send_relayed_message` would have
    /// written it once the relay landed.
    async fn relayed_turn(pool: &SqlitePool, chat_id: &str, relay_id: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, from_relay_id, origin, created_at)
             VALUES ('relayed', 'completed', 'assistant', ?, ?, 'shell', ?) RETURNING id",
        )
        .bind(chat_id)
        .bind(relay_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A mother `M` whose typed turn handed a task to its linked child `F`: chain `[M, F]`. Returns
    /// `(mother, child, link_id, child_run_id)`.
    async fn a_linked_pair(pool: &SqlitePool, child_title: Option<&str>) -> (String, String, i64, i64) {
        let mother = a_conversation(pool, Some("Trip planning")).await;
        let child = a_conversation(pool, child_title).await;
        let link = crate::chats::links::open(pool, &child, Some(&mother), "browser", None)
            .await
            .expect("link the child to its mother");
        let mother_turn = typed_turn(pool, &mother).await;
        let relay_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO chat_relays (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
             VALUES (?, ?, ?, 'find me a hotel', 1, ?) RETURNING id",
        )
        .bind(&mother)
        .bind(&child)
        .bind(mother_turn)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(pool)
        .await
        .unwrap();
        let child_run = relayed_turn(pool, &child, relay_id).await;
        (mother, child, link.id, child_run)
    }

    /// `(outcome, reason, run_id)` of every delivery row of `link_id`, oldest first.
    async fn deliveries(pool: &SqlitePool, link_id: i64) -> Vec<(String, Option<String>, i64)> {
        sqlx::query_as(
            "SELECT outcome, reason, run_id FROM chat_link_deliveries WHERE link_id = ? ORDER BY id",
        )
        .bind(link_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn runs_in(pool: &SqlitePool, chat_id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn browser_reply_notices(pool: &SqlitePool, chat_id: &str) -> Vec<String> {
        crate::chats::notices::for_chat(pool, chat_id)
            .await
            .unwrap()
            .into_iter()
            .filter(|notice| notice.kind == crate::chats::notices::KIND_BROWSER_REPLY)
            .map(|notice| notice.body)
            .collect()
    }

    async fn settled(pool: &SqlitePool, id: i64) {
        for _ in 0..500 {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
            if status != "running" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("turn {id} never settled");
    }

    /// The child's answer opens a turn in the mother, as a relay the child's turn sent: the chain
    /// stays reconstructible and the mother's prompt says who is speaking.
    #[tokio::test]
    async fn a_finished_turn_of_a_linked_child_is_delivered_to_its_parent_as_a_relay() {
        let state = test_state().await;
        owner_present(&state.pool).await;
        let (mother, child, link_id, child_run) = a_linked_pair(&state.pool, Some("Hotels")).await;

        let outcome = deliver_reply(&state, &child, child_run, "Found three hotels.").await;

        let turn_id = match outcome {
            DeliveryOutcome::Delivered { turn_id } => turn_id,
            other => panic!("the reply should have opened a turn in the mother: {other:?}"),
        };
        let (relay_id, sending_run_id, body): (i64, i64, String) = sqlx::query_as(
            "SELECT id, sending_run_id, body FROM chat_relays WHERE from_chat_id = ? AND to_chat_id = ?",
        )
        .bind(&child)
        .bind(&mother)
        .fetch_one(&state.pool)
        .await
        .expect("a chat_relays row child -> parent");
        assert_eq!(sending_run_id, child_run, "the relay is sent by the child's turn");
        assert_eq!(body, "[browser agent · Hotels] Found three hotels.");
        let (chat_id, from_relay_id, prompt): (String, Option<i64>, String) =
            sqlx::query_as("SELECT chat_id, from_relay_id, prompt FROM runs WHERE id = ?")
                .bind(turn_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(chat_id, mother);
        assert_eq!(from_relay_id, Some(relay_id), "the mother's turn records its relay");
        assert_eq!(prompt, "[browser agent · Hotels] Found three hotels.");
        assert_eq!(
            deliveries(&state.pool, link_id).await,
            vec![("delivered".to_string(), None, child_run)]
        );
        settled(&state.pool, turn_id).await;
    }

    /// A mother mid-turn does not lose the answer: it waits in her queue with its relay id, and
    /// the drain sends it once she is free.
    #[tokio::test]
    async fn a_reply_to_a_busy_parent_is_queued_and_the_drain_sends_it() {
        let state = test_state().await;
        owner_present(&state.pool).await;
        let (mother, child, link_id, child_run) = a_linked_pair(&state.pool, None).await;
        let held = crate::assistant::take_the_slot_for_testing(&mother);

        let outcome = deliver_reply(&state, &child, child_run, "Booked.").await;

        assert_eq!(outcome, DeliveryOutcome::Queued);
        let (text, relay_id): (String, Option<i64>) =
            sqlx::query_as("SELECT text, relay_id FROM chat_queue WHERE chat_id = ?")
                .bind(&mother)
                .fetch_one(&state.pool)
                .await
                .expect("the reply waits in the mother's queue");
        assert_eq!(text, "[browser agent · browser agent] Booked.");
        let relay_id = relay_id.expect("the queued reply carries its relay");
        assert_eq!(
            deliveries(&state.pool, link_id).await,
            vec![("queued".to_string(), None, child_run)]
        );

        drop(held);
        crate::assistant::send::drain_queued(&state, &mother).await;

        let (turn_id, prompt): (i64, String) = sqlx::query_as(
            "SELECT id, prompt FROM runs WHERE chat_id = ? AND from_relay_id = ?",
        )
        .bind(&mother)
        .bind(relay_id)
        .fetch_one(&state.pool)
        .await
        .expect("the drain sent the waiting reply");
        assert_eq!(prompt, "[browser agent · browser agent] Booked.");
        settled(&state.pool, turn_id).await;
    }

    /// Nobody at the machine: the gate refuses, the mother gets a notice and no turn is spent.
    #[tokio::test]
    async fn a_refused_reply_becomes_a_notice_and_opens_no_turn() {
        let state = test_state().await;
        // No heartbeat: `owner_is_present` fails closed to "away".
        let (mother, child, link_id, child_run) = a_linked_pair(&state.pool, Some("Hotels")).await;

        let outcome = deliver_reply(&state, &child, child_run, "Found three hotels.").await;

        assert_eq!(
            outcome,
            DeliveryOutcome::Refused {
                reason: "owner_away".to_string()
            }
        );
        assert_eq!(runs_in(&state.pool, &mother).await, 1, "only the typed turn");
        let notices = browser_reply_notices(&state.pool, &mother).await;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].starts_with("Not delivered (owner_away)"),
            "{notices:?}"
        );
        assert!(notices[0].contains("Found three hotels."), "{notices:?}");
        assert_eq!(
            deliveries(&state.pool, link_id).await,
            vec![(
                "refused".to_string(),
                Some("owner_away".to_string()),
                child_run
            )]
        );
    }

    /// Spec 4.4's ceiling, walked in full: the task `[M, F]`, the first reply `[M, F, M]`, the
    /// mother's follow-up `[M, F, M, F]`, and the second reply is one hop too deep.
    #[tokio::test]
    async fn the_second_reply_of_a_linked_pair_is_too_deep_and_falls_to_a_notice() {
        let state = test_state().await;
        owner_present(&state.pool).await;
        let (mother, child, link_id, first_child_run) =
            a_linked_pair(&state.pool, Some("Flights")).await;

        let first = deliver_reply(&state, &child, first_child_run, "Two flights found.").await;
        let mother_turn = match first {
            DeliveryOutcome::Delivered { turn_id } => turn_id,
            other => panic!("the first reply should be delivered: {other:?}"),
        };
        settled(&state.pool, mother_turn).await;

        let follow_up = crate::relay::admit(
            &state.pool,
            &mother,
            &child,
            mother_turn,
            Origin::Shell,
            "and the return flight?",
            chrono::Utc::now(),
        )
        .await
        .expect("a linked mother may write back to her child");
        let second_child_run = relayed_turn(&state.pool, &child, follow_up).await;

        let second = deliver_reply(&state, &child, second_child_run, "Return booked.").await;

        assert_eq!(
            second,
            DeliveryOutcome::Refused {
                reason: "relay_too_deep".to_string()
            }
        );
        assert_eq!(
            runs_in(&state.pool, &mother).await,
            2,
            "the typed turn and the first reply only"
        );
        let notices = browser_reply_notices(&state.pool, &mother).await;
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].starts_with("Not delivered (relay_too_deep)"),
            "{notices:?}"
        );
        assert_eq!(
            deliveries(&state.pool, link_id).await,
            vec![
                ("delivered".to_string(), None, first_child_run),
                (
                    "refused".to_string(),
                    Some("relay_too_deep".to_string()),
                    second_child_run
                ),
            ]
        );
    }

    /// The reply was written by a relayed turn, so the mother it wakes keeps `McpOnly` even with
    /// a folder and a wired hook: a relay is handed the shell only on the first hop.
    #[tokio::test]
    async fn a_parent_woken_by_the_reply_runs_mcp_only() {
        let state = test_state().await;
        owner_present(&state.pool).await;
        let (_mother, child, _link_id, child_run) = a_linked_pair(&state.pool, None).await;

        let outcome = deliver_reply(&state, &child, child_run, "Done.").await;

        let turn_id = match outcome {
            DeliveryOutcome::Delivered { turn_id } => turn_id,
            other => panic!("the reply should have opened a turn in the mother: {other:?}"),
        };
        let relay_id: Option<i64> = sqlx::query_scalar("SELECT from_relay_id FROM runs WHERE id = ?")
            .bind(turn_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let handover = crate::relay::handover_of(&state.pool, relay_id).await;
        assert_eq!(handover, crate::relay::Handover::Withheld);
        assert_eq!(
            crate::assistant::tool_policy_for(Some("."), Origin::Shell, true, handover),
            crate::execution::runner::ToolPolicy::McpOnly
        );
        settled(&state.pool, turn_id).await;
    }

    /// Nothing to say, or nobody to say it to: no relay, no notice, no delivery row.
    #[tokio::test]
    async fn an_empty_reply_and_an_unlinked_child_deliver_nothing() {
        let state = test_state().await;
        owner_present(&state.pool).await;
        let (mother, child, link_id, child_run) = a_linked_pair(&state.pool, None).await;

        assert_eq!(
            deliver_reply(&state, &child, child_run, "").await,
            DeliveryOutcome::Empty
        );
        assert_eq!(
            deliver_reply(&state, &child, child_run, "  \n\t ").await,
            DeliveryOutcome::Empty
        );

        // A conversation that was never linked.
        let loner = a_conversation(&state.pool, None).await;
        let loner_run = typed_turn(&state.pool, &loner).await;
        assert_eq!(
            deliver_reply(&state, &loner, loner_run, "hello").await,
            DeliveryOutcome::NotLinked
        );

        // A panel-only link has no parent to deliver to.
        crate::chats::links::open(&state.pool, &child, None, "browser", None)
            .await
            .expect("relink to the panel only");
        assert_eq!(
            deliver_reply(&state, &child, child_run, "hello").await,
            DeliveryOutcome::NotLinked
        );

        let relays: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_relays WHERE from_chat_id IN (?, ?)")
            .bind(&child)
            .bind(&loner)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(relays, 0);
        assert_eq!(runs_in(&state.pool, &mother).await, 1, "only the typed turn");
        assert!(browser_reply_notices(&state.pool, &mother).await.is_empty());
        assert!(deliveries(&state.pool, link_id).await.is_empty());
        let all_deliveries: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_link_deliveries")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(all_deliveries, 0);
    }
}
