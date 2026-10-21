//! Contract conformance scenarios (test-only): each one drives the channel contract over HTTP
//! through the fake adapter, over `channels::routes()` behind the real token middleware.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use chrono::Utc;
    use serde_json::json;
    use sqlx::SqlitePool;

    use crate::channels::ChannelsRuntime;
    use crate::channels::contract::{ConversationKey, OutPart};
    use crate::channels::fake::{FakeAdapter, Reply, request};
    use crate::channels::outbox::{self, Class, NewAction};
    use crate::channels::registry::ChannelConfig;
    use crate::channels::strings::{self, Text};
    use crate::channels::testkit::state;
    use crate::state::AppState;

    /// The control key of `testkit::state()`.
    const CONTROL_KEY: &str = "test-token";

    fn capabilities() -> serde_json::Value {
        json!({"max_text": 4096})
    }

    /// An adapter that has said `hello`, so the hub knows its capabilities.
    async fn ready(st: &AppState) -> FakeAdapter {
        let adapter = FakeAdapter::new(st, "fake").await;
        let reply = adapter.hello(capabilities()).await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.json(), json!({"contract_version": "1.0.0"}));
        adapter
    }

    /// An owner already paired: `sender` may talk in `conversation`, which is also home.
    async fn seed_owner(pool: &SqlitePool, sender: &str, conversation: &str) {
        sqlx::query(
            "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
             conversation_kind, home, created_at) VALUES ('fake', ?, ?, 'private', 1, ?)",
        )
        .bind(sender)
        .bind(conversation)
        .bind("2026-10-20T10:00:00Z")
        .execute(pool)
        .await
        .unwrap();
    }

    /// Queues a plain `send` for `fake` (what the hub does after a turn) and wakes the channel.
    async fn queue(st: &AppState, conversation: &str, text: &str) -> i64 {
        let mut conn = st.pool.acquire().await.unwrap();
        let id = outbox::enqueue(
            &mut conn,
            &ConversationKey::new("fake", conversation),
            NewAction::Send {
                reply_to: None,
                parts: vec![OutPart::Text {
                    text: text.to_string(),
                }],
                buttons: None,
                silent: false,
            },
            Class::Turn,
            None,
            Utc::now(),
        )
        .await
        .unwrap();
        drop(conn);
        outbox::wake(st, "fake");
        id
    }

    /// The ids of the actions a poll delivered, in delivery order.
    fn action_ids(reply: &Reply) -> Vec<i64> {
        let body = reply.json();
        body["actions"]
            .as_array()
            .expect("a poll answers an actions array")
            .iter()
            .map(|action| action["id"].as_i64().unwrap())
            .collect()
    }

    async fn row_state(pool: &SqlitePool, id: i64) -> String {
        sqlx::query_scalar("SELECT state FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn attempts(pool: &SqlitePool, id: i64) -> i64 {
        sqlx::query_scalar("SELECT attempts FROM channel_outbox WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Makes the lease of a row run out, as if the adapter had gone quiet for a minute.
    async fn expire_lease(pool: &SqlitePool, id: i64) {
        sqlx::query("UPDATE channel_outbox SET lease_until = '2000-01-01T00:00:00Z' WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn count(pool: &SqlitePool, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    async fn full_cycle_hello_event_turn_ack() {
        let st = state().await;
        let adapter = ready(&st).await;
        seed_owner(&st.pool, "u1", "c1").await;

        // The owner writes: the event is accepted and starts a turn on `fake:c1`.
        let event = FakeAdapter::message_event("e1", "c1", "u1", "hi");
        let accepted = adapter.post_event(&event).await;
        assert_eq!(accepted.status, StatusCode::ACCEPTED);
        assert_eq!(accepted.json(), json!({"accepted": true}));
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind("fake:c1")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        assert_eq!(runs, 1);

        // Starting the turn queued its "working" acknowledgement: it is delivered first.
        let run_id: i64 = sqlx::query_scalar("SELECT id FROM runs WHERE chat_id = ?")
            .bind("fake:c1")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        let first = adapter.poll(Duration::ZERO).await;
        assert_eq!(first.status, StatusCode::OK);
        let first_body = first.json();
        assert_eq!(first_body["actions"].as_array().unwrap().len(), 1);
        assert_eq!(first_body["actions"][0]["type"], "send");
        assert_eq!(first_body["actions"][0]["conversation"], "c1");
        assert_eq!(
            first_body["actions"][0]["parts"][0]["text"],
            strings::render(&Text::Working { turn: run_id })
        );
        let ack_id = first_body["actions"][0]["id"].as_i64().unwrap();
        assert_eq!(
            adapter.ack_ok(ack_id).await.json(),
            json!({"state": "done"})
        );

        // The long-poll is already open when the turn's reply is queued, and the wake ends it.
        let (polled, id) = tokio::join!(adapter.poll(Duration::from_secs(5)), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            queue(&st, "c1", "done").await
        });
        assert_eq!(polled.status, StatusCode::OK);
        let body = polled.json();
        assert_eq!(body["actions"].as_array().unwrap().len(), 1);
        assert_eq!(body["actions"][0]["id"], id);
        assert_eq!(body["actions"][0]["type"], "send");
        assert_eq!(body["actions"][0]["conversation"], "c1");
        assert_eq!(body["actions"][0]["parts"][0]["text"], "done");
        assert_eq!(row_state(&st.pool, id).await, "leased");

        // Delivered: the ack carries the platform ref and settles the row.
        let acked = adapter.ack_ok(id).await;
        assert_eq!(acked.status, StatusCode::OK);
        assert_eq!(acked.json(), json!({"state": "done"}));
        let refs: String =
            sqlx::query_scalar("SELECT platform_refs FROM channel_outbox WHERE id = ?")
                .bind(id)
                .fetch_one(&st.pool)
                .await
                .unwrap();
        assert_eq!(refs, format!(r#"["p{id}"]"#));

        let idle = adapter.poll(Duration::ZERO).await;
        assert_eq!(idle.json(), json!({"actions": [], "ephemeral": []}));
    }

    #[tokio::test]
    async fn expired_lease_is_redelivered_and_late_ack_is_ignored() {
        let st = state().await;
        let adapter = ready(&st).await;
        let id = queue(&st, "c1", "one").await;

        assert_eq!(action_ids(&adapter.poll(Duration::ZERO).await), vec![id]);
        // A live lease is never handed out again.
        assert!(action_ids(&adapter.poll(Duration::ZERO).await).is_empty());

        expire_lease(&st.pool, id).await;
        assert_eq!(action_ids(&adapter.poll(Duration::ZERO).await), vec![id]);
        assert_eq!(attempts(&st.pool, id).await, 2);

        // The second delivery holds the lease and may extend it.
        let renewed = adapter.renew(id).await;
        assert_eq!(renewed.status, StatusCode::OK);
        assert!(renewed.json()["lease_until"].is_string());

        let settled = adapter.ack_ok(id).await;
        assert_eq!(settled.json(), json!({"state": "done"}));

        // The acknowledgement of the first delivery arrives late: nothing changes.
        let late = adapter.ack_ok(id).await;
        assert_eq!(late.status, StatusCode::OK);
        assert_eq!(late.json(), json!({"state": "done", "ignored": true}));
        assert_eq!(row_state(&st.pool, id).await, "done");
        assert_eq!(attempts(&st.pool, id).await, 2);

        // And a settled row can no longer be renewed.
        let renew = adapter.renew(id).await;
        assert_eq!(renew.status, StatusCode::CONFLICT);
        assert_eq!(renew.json(), json!({"error": "not_leased"}));
    }

    #[tokio::test]
    async fn acks_out_of_order_across_conversations() {
        let st = state().await;
        let adapter = ready(&st).await;
        let a1 = queue(&st, "ca", "a1").await;
        let b1 = queue(&st, "cb", "b1").await;
        let a2 = queue(&st, "ca", "a2").await;

        // One ordered action per conversation: a2 waits behind a1.
        assert_eq!(
            action_ids(&adapter.poll(Duration::ZERO).await),
            vec![a1, b1]
        );

        // The second conversation finishes first; the first conversation stays blocked.
        assert_eq!(adapter.ack_ok(b1).await.json(), json!({"state": "done"}));
        assert!(action_ids(&adapter.poll(Duration::ZERO).await).is_empty());
        assert_eq!(row_state(&st.pool, a2).await, "pending");

        assert_eq!(adapter.ack_ok(a1).await.json(), json!({"state": "done"}));
        assert_eq!(action_ids(&adapter.poll(Duration::ZERO).await), vec![a2]);
    }

    #[tokio::test]
    async fn retry_blocks_the_conversation_but_not_others() {
        let st = state().await;
        let adapter = ready(&st).await;
        let x1 = queue(&st, "cx", "x1").await;
        let x2 = queue(&st, "cx", "x2").await;
        let y1 = queue(&st, "cy", "y1").await;

        assert_eq!(
            action_ids(&adapter.poll(Duration::ZERO).await),
            vec![x1, y1]
        );

        // x1 fails retryably, not due for a minute; y1 is delivered.
        let failed = adapter
            .ack(
                x1,
                &json!({"failed": {"retryable": true, "retry_after_ms": 60000, "reason": "network"}}),
            )
            .await;
        assert_eq!(failed.json(), json!({"state": "pending"}));
        assert_eq!(adapter.ack_ok(y1).await.json(), json!({"state": "done"}));

        // x2 stays behind x1; the other conversation keeps flowing.
        let y2 = queue(&st, "cy", "y2").await;
        assert_eq!(action_ids(&adapter.poll(Duration::ZERO).await), vec![y2]);
        assert_eq!(row_state(&st.pool, x1).await, "pending");
        assert_eq!(row_state(&st.pool, x2).await, "pending");
        assert_eq!(attempts(&st.pool, x1).await, 1);
    }

    #[tokio::test]
    async fn thread_not_found_falls_back_to_home() {
        let mut st = state().await;
        let config = ChannelConfig {
            projects: [("alpha".to_string(), "thread-7".to_string())].into(),
            ..ChannelConfig::default()
        };
        st.channels = Arc::new(ChannelsRuntime::with_configs(HashMap::from([(
            "fake", config,
        )])));
        seed_owner(&st.pool, "u1", "home").await;
        let adapter = ready(&st).await;
        let id = queue(&st, "thread-7", "hi").await;

        assert_eq!(action_ids(&adapter.poll(Duration::ZERO).await), vec![id]);
        let gone = adapter
            .ack(
                id,
                &json!({"failed": {"retryable": false, "reason": "thread_not_found"}}),
            )
            .await;
        assert_eq!(gone.json(), json!({"state": "failed"}));

        let replacement: Option<i64> =
            sqlx::query_scalar("SELECT replaced_by FROM channel_outbox WHERE id = ?")
                .bind(id)
                .fetch_one(&st.pool)
                .await
                .unwrap();
        let replacement = replacement.expect("the failed original points at its replacement");

        let polled = adapter.poll(Duration::ZERO).await;
        assert_eq!(action_ids(&polled), vec![replacement]);
        let body = polled.json();
        assert_eq!(body["actions"][0]["conversation"], "home");
        let text = body["actions"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("[alpha]"), "{text}");
        assert!(text.ends_with("hi"), "{text}");
    }

    #[tokio::test]
    async fn hello_releases_leased_rows() {
        let st = state().await;
        let adapter = ready(&st).await;
        let id = queue(&st, "c1", "one").await;
        assert_eq!(action_ids(&adapter.poll(Duration::ZERO).await), vec![id]);
        assert_eq!(row_state(&st.pool, id).await, "leased");

        // The adapter restarted: a new hello gives back everything it was holding.
        let again = adapter.hello(capabilities()).await;
        assert_eq!(again.status, StatusCode::OK);
        assert_eq!(row_state(&st.pool, id).await, "pending");

        assert_eq!(action_ids(&adapter.poll(Duration::ZERO).await), vec![id]);
        assert_eq!(attempts(&st.pool, id).await, 2);
    }

    #[tokio::test]
    async fn an_adapter_key_cannot_use_another_channels_routes() {
        let st = state().await;
        let adapter = ready(&st).await;
        for (method, uri) in [
            ("PUT", "/channels/telegram/hello"),
            ("POST", "/channels/telegram/events"),
            ("POST", "/channels/telegram/media"),
            ("GET", "/channels/telegram/media/m_x"),
            ("GET", "/channels/telegram/outbox"),
            ("POST", "/channels/telegram/outbox/1/ack"),
            ("POST", "/channels/telegram/outbox/1/renew"),
        ] {
            let reply = adapter.raw(method, uri, "{}").await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert_eq!(
                reply.json(),
                json!({"error": "channel_mismatch"}),
                "{method} {uri}"
            );
        }
    }

    #[tokio::test]
    async fn and_cannot_use_management_routes() {
        let st = state().await;
        let adapter = ready(&st).await;
        for (method, uri) in [
            ("GET", "/channels"),
            ("PUT", "/channels/primary"),
            ("POST", "/channels/fake/pairing"),
            ("GET", "/channels/fake/pairing"),
            ("DELETE", "/channels/fake/pairing"),
            ("GET", "/channels/fake/identities"),
            ("DELETE", "/channels/fake/identities/u1"),
            ("PUT", "/channels/fake/identities/u1/home"),
        ] {
            let reply = adapter.raw(method, uri, "{}").await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert!(reply.bytes.is_empty(), "{method} {uri} must be a bare 403");
        }
        // Nothing was minted behind the refusals.
        assert_eq!(
            count(&st.pool, "SELECT COUNT(*) FROM channel_pairing_codes").await,
            0
        );
    }

    #[tokio::test]
    async fn the_control_key_cannot_speak_for_a_channel() {
        let st = state().await;
        for (method, uri) in [
            ("PUT", "/channels/fake/hello"),
            ("POST", "/channels/fake/events"),
            ("GET", "/channels/fake/outbox"),
            ("POST", "/channels/fake/outbox/1/ack"),
            ("POST", "/channels/fake/media"),
        ] {
            let reply = request(
                &st,
                CONTROL_KEY,
                method,
                uri,
                Some("application/json"),
                b"{}".to_vec(),
            )
            .await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert!(reply.bytes.is_empty(), "{method} {uri} must be a bare 403");
        }
    }

    #[tokio::test]
    async fn media_roundtrip_and_cross_channel_404() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(dir.path()).unwrap();
        let mut st = state().await;
        st.files_root = Some(root);
        let adapter = FakeAdapter::new(&st, "fake").await;
        let other = FakeAdapter::new(&st, "telegram").await;

        let png = vec![0x89, b'P', b'N', b'G', 0, 1, 2];
        let uploaded = adapter.upload("image/png", png.clone()).await;
        assert_eq!(uploaded.status, StatusCode::OK);
        let mine = uploaded.json()["media_id"].as_str().unwrap().to_string();

        let back = adapter.download(&mine).await;
        assert_eq!(back.status, StatusCode::OK);
        assert_eq!(back.content_type.as_deref(), Some("image/png"));
        assert_eq!(back.bytes, png);

        let uploaded = other.upload("text/plain", b"secret".to_vec()).await;
        assert_eq!(uploaded.status, StatusCode::OK);
        let theirs = uploaded.json()["media_id"].as_str().unwrap().to_string();

        // Another channel's media is a 404 in both directions, never a 403 that would confirm it.
        assert_eq!(
            adapter.download(&theirs).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(other.download(&mine).await.status, StatusCode::NOT_FOUND);
        assert_eq!(other.download(&theirs).await.bytes, b"secret".to_vec());
    }

    #[tokio::test]
    async fn unknown_sender_is_silently_dropped_and_counted() {
        let st = state().await;
        let adapter = ready(&st).await;

        let first = adapter
            .post_event(&FakeAdapter::message_event("s1", "c9", "stranger", "hello"))
            .await;
        assert_eq!(first.status, StatusCode::ACCEPTED);
        assert_eq!(first.json(), json!({"dropped": true}));
        let second = adapter
            .post_event(&FakeAdapter::message_event("s2", "c9", "stranger", "again"))
            .await;
        assert_eq!(second.json(), json!({"dropped": true}));

        assert_eq!(st.channels.presence.drops("fake").unknown_sender, 2);
        // Silent: no turn, nothing to say back.
        assert_eq!(
            count(
                &st.pool,
                "SELECT COUNT(*) FROM runs WHERE chat_id = 'fake:c9'"
            )
            .await,
            0
        );
        assert_eq!(
            count(&st.pool, "SELECT COUNT(*) FROM channel_outbox").await,
            0
        );
        assert!(action_ids(&adapter.poll(Duration::ZERO).await).is_empty());
    }

    #[tokio::test]
    async fn pairing_over_http_end_to_end() {
        let st = state().await;
        let adapter = ready(&st).await;

        // The owner mints a code with the control key.
        let minted = request(
            &st,
            CONTROL_KEY,
            "POST",
            "/channels/fake/pairing",
            None,
            Vec::new(),
        )
        .await;
        assert_eq!(minted.status, StatusCode::CREATED);
        let command = minted.json()["command"].as_str().unwrap().to_string();
        assert!(command.starts_with("/pair "), "{command}");

        let status = |st: &AppState| {
            let st = st.clone();
            async move {
                request(
                    &st,
                    CONTROL_KEY,
                    "GET",
                    "/channels/fake/pairing",
                    None,
                    Vec::new(),
                )
                .await
                .json()
            }
        };
        assert_eq!(status(&st).await["state"], "waiting");

        // Before pairing, the sender is a stranger.
        let before = adapter
            .post_event(&FakeAdapter::message_event("p0", "pc1", "newbie", "hi"))
            .await;
        assert_eq!(before.json(), json!({"dropped": true}));

        // The code arrives as a message through the adapter.
        let paired = adapter
            .post_event(&FakeAdapter::message_event("p1", "pc1", "newbie", &command))
            .await;
        assert_eq!(paired.status, StatusCode::ACCEPTED);
        assert_eq!(paired.json(), json!({"accepted": true}));

        let done = status(&st).await;
        assert_eq!(done["state"], "paired");
        assert_eq!(done["identity"]["sender_ref"], "newbie");
        assert_eq!(done["identity"]["conversation_ref"], "pc1");

        // The confirmation is queued for the conversation.
        let polled = adapter.poll(Duration::ZERO).await;
        let body = polled.json();
        assert_eq!(body["actions"].as_array().unwrap().len(), 1);
        assert_eq!(body["actions"][0]["conversation"], "pc1");
        assert_eq!(
            body["actions"][0]["parts"][0]["text"],
            strings::render(&Text::Paired)
        );

        // From now on the identity is admitted: the message starts a turn instead of being dropped.
        let admitted = adapter
            .post_event(&FakeAdapter::message_event("p2", "pc1", "newbie", "hello"))
            .await;
        assert_eq!(admitted.json(), json!({"accepted": true}));
        assert_eq!(
            count(
                &st.pool,
                "SELECT COUNT(*) FROM runs WHERE chat_id = 'fake:pc1'"
            )
            .await,
            1
        );
    }
}
