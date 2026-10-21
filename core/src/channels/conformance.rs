//! Contract conformance scenarios (test-only): each one drives the channel contract over HTTP
//! through the fake adapter, over `channels::routes()` behind the real token middleware.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use chrono::Utc;
    use serde_json::{Value, json};
    use sqlx::SqlitePool;

    use crate::channels::ChannelsRuntime;
    use crate::channels::contract::{
        ConversationKey, EventShape, HelloShape, OutPart, decode, fixtures_dir,
    };
    use crate::channels::fake::{FakeAdapter, Reply, register_media, request, stimulate};
    use crate::channels::outbox::{self, Class, NewAction};
    use crate::channels::registry::ChannelConfig;
    use crate::channels::routing;
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
        // Minting a code needs a configured channel (409 otherwise).
        st.channels.presence.set_secrets_present("fake", Vec::new());

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

    // ---- the capability scenarios of the shared fixtures (Anexo C, C.10) -------------------

    /// Every `capabilities/*.json`, by file name.
    fn scenarios() -> Vec<(String, Value)> {
        let dir = fixtures_dir().join("capabilities");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .map(|entry| entry.expect("a directory entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        paths
            .into_iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let bytes = std::fs::read(&path).unwrap();
                let value = serde_json::from_slice(&bytes)
                    .unwrap_or_else(|e| panic!("{name} is not JSON: {e}"));
                (name, value)
            })
            .collect()
    }

    fn needles(want: &Value) -> Vec<&str> {
        want.as_array()
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    /// `actual` satisfies `expected` (C.10.1): a key without `$` is equal (objects are matched key
    /// by key, so extra keys of `actual` are fine), arrays have the same length and are matched
    /// position by position, `$present` / `$absent` name keys, and `$contains`, `$not_contains`
    /// and `$prefix` look at the `text` of the object.
    fn matches(actual: &Value, expected: &Value, at: &str) -> Result<(), String> {
        match expected {
            Value::Object(wanted) => {
                if !actual.is_object() {
                    return Err(format!("{at}: expected an object, got {actual}"));
                }
                for (key, want) in wanted {
                    match key.as_str() {
                        "$present" | "$absent" => {
                            for name in needles(want) {
                                let there = actual.get(name).is_some_and(|v| !v.is_null());
                                if there != (key == "$present") {
                                    return Err(format!("{at}: {key} {name:?} failed in {actual}"));
                                }
                            }
                        }
                        "$contains" | "$not_contains" => {
                            let text = actual["text"]
                                .as_str()
                                .ok_or_else(|| format!("{at}: {key} on a part without text"))?;
                            for needle in needles(want) {
                                if text.contains(needle) != (key == "$contains") {
                                    return Err(format!(
                                        "{at}: {key} {needle:?} failed on {text:?}"
                                    ));
                                }
                            }
                        }
                        "$prefix" => {
                            let text = actual["text"].as_str().unwrap_or_default();
                            let prefix = want.as_str().unwrap_or_default();
                            if !text.starts_with(prefix) {
                                return Err(format!(
                                    "{at}: {text:?} does not start with {prefix:?}"
                                ));
                            }
                        }
                        plain => {
                            let got = actual.get(plain).unwrap_or(&Value::Null);
                            matches(got, want, &format!("{at}/{plain}"))?;
                        }
                    }
                }
                Ok(())
            }
            Value::Array(wanted) => {
                let Some(got) = actual.as_array() else {
                    return Err(format!("{at}: expected an array, got {actual}"));
                };
                if got.len() != wanted.len() {
                    return Err(format!(
                        "{at}: expected {} items, got {}: {actual}",
                        wanted.len(),
                        got.len()
                    ));
                }
                got.iter()
                    .zip(wanted)
                    .enumerate()
                    .try_for_each(|(i, (got, want))| matches(got, want, &format!("{at}/{i}")))
            }
            scalar if scalar == actual => Ok(()),
            scalar => Err(format!("{at}: expected {scalar}, got {actual}")),
        }
    }

    /// What the adapter sees after a step: it asks until the outbox is empty and acknowledges
    /// every action `ok` with `p<id>`, so the next one of a conversation can come out. Actions are
    /// returned by id, the ephemerals as delivered.
    async fn drain(adapter: &FakeAdapter) -> Result<(Vec<Value>, Vec<Value>), String> {
        let mut actions: Vec<Value> = Vec::new();
        let mut ephemeral: Vec<Value> = Vec::new();
        for _ in 0..20 {
            let reply = adapter.poll(Duration::ZERO).await;
            if reply.status != StatusCode::OK {
                return Err(format!("the poll answered {}", reply.status));
            }
            let body = reply.json();
            ephemeral.extend(body["ephemeral"].as_array().cloned().unwrap_or_default());
            let batch = body["actions"].as_array().cloned().unwrap_or_default();
            if batch.is_empty() {
                actions.sort_by_key(|action| action["id"].as_i64());
                return Ok((actions, ephemeral));
            }
            for action in &batch {
                let id = action["id"].as_i64().ok_or("an action without an id")?;
                let acked = adapter.ack_ok(id).await;
                if acked.status != StatusCode::OK {
                    return Err(format!("the ack of {id} answered {}", acked.status));
                }
            }
            actions.extend(batch);
        }
        Err("the outbox never drained".to_string())
    }

    /// `expect.state`: a proposal's status or a capture's answer, read from the test database.
    async fn check_state(pool: &SqlitePool, want: &Value) -> Result<(), String> {
        if let Some(proposal) = want.get("proposal") {
            let id = proposal["id"]
                .as_i64()
                .ok_or("state.proposal without an id")?;
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
                    .bind(id)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| e.to_string())?;
            let wanted = proposal["status"].as_str().unwrap_or_default();
            if status.as_deref() != Some(wanted) {
                return Err(format!("proposal {id} is {status:?}, expected {wanted:?}"));
            }
        }
        if let Some(capture) = want.get("capture") {
            let id = capture["id"]
                .as_i64()
                .ok_or("state.capture without an id")?;
            let answer: Option<String> = sqlx::query_scalar(
                "SELECT n.note_text FROM capture_requests r \
                 JOIN owner_notes n ON n.id = r.note_id WHERE r.job_id = ?",
            )
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;
            let wanted = capture["answer"].as_str().unwrap_or_default();
            if answer.as_deref() != Some(wanted) {
                return Err(format!("capture {id} is {answer:?}, expected {wanted:?}"));
            }
        }
        Ok(())
    }

    /// A state for the scenario: `fake` is the primary, configured, with the scenario's identities,
    /// projects and media, and a runner that keeps a started turn open until the scenario ends it.
    async fn scenario_state(scenario: &Value) -> Result<AppState, String> {
        let mut st = state().await;
        let projects: Vec<(String, String)> = scenario["setup"]["projects"]
            .as_object()
            .map(|map| {
                map.iter()
                    .filter_map(|(project, thread)| {
                        Some((project.clone(), thread.as_str()?.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let config = ChannelConfig {
            projects: projects.into_iter().collect(),
            ..ChannelConfig::default()
        };
        st.channels = Arc::new(ChannelsRuntime::with_configs(HashMap::from([(
            "fake", config,
        )])));
        st.channels.presence.set_secrets_present("fake", Vec::new());
        st.runner = Arc::new(crate::execution::runner::FakeCommandRunner {
            delay: std::sync::Mutex::new(Some(Duration::from_secs(30))),
            ..Default::default()
        });
        for who in scenario["setup"]["identities"]
            .as_array()
            .into_iter()
            .flatten()
        {
            sqlx::query(
                "INSERT INTO channel_identities (channel, sender_ref, conversation_ref, \
                 conversation_kind, home, created_at) VALUES ('fake', ?, ?, 'private', ?, ?)",
            )
            .bind(who["sender_ref"].as_str())
            .bind(who["conversation_ref"].as_str())
            .bind(who["home"].as_bool().unwrap_or(false) as i64)
            .bind("2026-10-20T10:00:00Z")
            .execute(&st.pool)
            .await
            .map_err(|e| e.to_string())?;
        }
        for media in scenario["setup"]["media"].as_array().into_iter().flatten() {
            let id = media["id"].as_str().ok_or("a media without an id")?;
            let mime = media["mime"].as_str().ok_or("a media without a mime")?;
            register_media(&st, "fake", id, media["name"].as_str(), mime).await?;
        }
        sqlx::query("UPDATE channels_global SET primary_channel = 'fake' WHERE id = 1")
            .execute(&st.pool)
            .await
            .map_err(|e| e.to_string())?;
        // The routing row exists before the first step, so nothing set up is announced as news.
        let mut conn = st.pool.acquire().await.map_err(|e| e.to_string())?;
        routing::seed(&mut conn, "fake")
            .await
            .map_err(|e| e.to_string())?;
        drop(conn);
        Ok(st)
    }

    /// Runs one scenario end to end. The `hello` and every `event` also pass the strict decoders.
    async fn run_scenario(scenario: &Value) -> Result<(), String> {
        decode::<HelloShape>(scenario["hello"].to_string().as_bytes())
            .map_err(|e| format!("the hello is not a valid hello: {e:?}"))?;
        let st = scenario_state(scenario).await?;
        let outcome = run_steps(&st, scenario).await;
        end_turns(&st).await;
        outcome
    }

    /// Ends every turn a scenario left in flight. A chat's slot is process-wide (`BUSY_CHATS`) and
    /// the scenarios share conversation refs, so a turn held open by the 30 s fake runner would make
    /// the next scenario's first message queue instead of starting a turn. Aborting the task drops
    /// its slot; the status write inside is a compare-and-set, so a finished turn keeps its status.
    async fn end_turns(st: &AppState) {
        let runs: Vec<i64> = sqlx::query_scalar("SELECT id FROM runs WHERE chat_id LIKE 'fake:%'")
            .fetch_all(&st.pool)
            .await
            .unwrap_or_default();
        for run in runs {
            crate::execution::runs::finalize_termination(st, run, "cancelled").await;
        }
        // Let the aborted tasks drop their slots before the next scenario sends.
        tokio::task::yield_now().await;
    }

    async fn run_steps(st: &AppState, scenario: &Value) -> Result<(), String> {
        let adapter = FakeAdapter::new(st, "fake").await;
        let hello = adapter
            .hello(scenario["hello"]["capabilities"].clone())
            .await;
        if hello.status != StatusCode::OK {
            return Err(format!(
                "the hello answered {}: {}",
                hello.status,
                hello.json()
            ));
        }
        let forbid = &scenario["forbid"];
        let steps = scenario["steps"]
            .as_array()
            .ok_or("a scenario without steps")?;
        for (index, step) in steps.iter().enumerate() {
            let at = |why: String| format!("step {}: {why}", index + 1);
            let mut response = Value::Null;
            if let Some(core) = step.get("core") {
                stimulate(st, core).await.map_err(at)?;
            } else if let Some(event) = step.get("event") {
                decode::<EventShape>(event.to_string().as_bytes())
                    .map_err(|e| at(format!("the event is not a valid event: {e:?}")))?;
                response = adapter.post_event(event).await.json();
            } else {
                return Err(at("neither a core stimulus nor an event".to_string()));
            }
            let (actions, ephemeral) = drain(&adapter).await.map_err(at)?;
            for action in &actions {
                if needles(&forbid["action_types"]).contains(&action["type"].as_str().unwrap_or(""))
                {
                    return Err(at(format!("a forbidden action came out: {action}")));
                }
            }
            for item in &ephemeral {
                if needles(&forbid["ephemeral_types"])
                    .contains(&item["type"].as_str().unwrap_or(""))
                {
                    return Err(at(format!("a forbidden ephemeral came out: {item}")));
                }
            }
            let expect = &step["expect"];
            if let Some(want) = expect.get("response") {
                matches(&response, want, "response").map_err(at)?;
            }
            if let Some(want) = expect.get("actions") {
                matches(&Value::Array(actions), want, "actions").map_err(at)?;
            }
            if let Some(want) = expect.get("ephemeral") {
                matches(&Value::Array(ephemeral), want, "ephemeral").map_err(at)?;
            }
            if let Some(want) = expect.get("state") {
                check_state(&st.pool, want).await.map_err(at)?;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn every_scenario_runs() {
        let mut failures = Vec::new();
        let all = scenarios();
        for (name, scenario) in &all {
            if let Err(why) = run_scenario(scenario).await {
                failures.push(format!("{name}: {why}"));
            }
        }
        assert!(all.len() >= 7, "only {} scenarios found", all.len());
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn the_matchers_read_the_way_the_scenarios_use_them() {
        let action = json!({"type": "send", "conversation": "4242", "buttons": [[]],
            "parts": [{"type": "text", "text": "[nucleos] Build finished"}]});
        let ok = |want: Value| matches(&action, &want, "action");
        assert!(ok(json!({"type": "send", "$present": ["buttons"]})).is_ok());
        assert!(ok(json!({"$absent": ["buttons"]})).is_err());
        assert!(ok(json!({"$absent": ["edit"]})).is_ok());
        assert!(ok(json!({"parts": [{"$prefix": "[nucleos] ", "$contains": ["Build"]}]})).is_ok());
        assert!(ok(json!({"parts": [{"$not_contains": ["Build"]}]})).is_err());
        // Arrays are matched position by position and must be as long.
        assert!(ok(json!({"parts": []})).is_err());
        assert!(ok(json!({"parts": [{"type": "text"}, {"type": "text"}]})).is_err());
        assert!(ok(json!({"conversation": "4243"})).is_err());
    }

    #[test]
    fn the_missing_set_is_exactly_the_six_rows_of_section_6_5_plus_null() {
        let all = scenarios();
        let missing: BTreeSet<Option<String>> = all
            .iter()
            .map(|(_, scenario)| scenario["missing"].as_str().map(str::to_owned))
            .collect();
        let wanted: BTreeSet<Option<String>> = [
            None,
            Some("buttons"),
            Some("edit"),
            Some("threads"),
            Some("typing"),
            Some("reply_to"),
            Some("media_out"),
        ]
        .into_iter()
        .map(|row| row.map(str::to_owned))
        .collect();
        assert_eq!(missing, wanted);
        assert_eq!(all.len(), wanted.len(), "one scenario per row");

        // The hello of each declares exactly that one capability less (`media_out`: the strong form,
        // `image` stays and `file` and `audio` are gone).
        for (name, scenario) in &all {
            let caps = &scenario["hello"]["capabilities"];
            match scenario["missing"].as_str() {
                None => {}
                Some("media_out") => {
                    assert_eq!(caps["media_out"], json!(["image"]), "{name}");
                }
                Some(row) => {
                    assert!(
                        caps.get(row).is_none_or(|v| v == &json!(false)),
                        "{name} still declares {row}"
                    );
                }
            }
        }
    }

    /// An owner paired on `fake:c1` whose adapter said hello with the given `typing` and `edit`
    /// capabilities, and whose first message started a running turn. Returns the turn's run id.
    async fn streaming_turn(typing: bool, edit: bool) -> (AppState, FakeAdapter, i64) {
        let st = state().await;
        let adapter = FakeAdapter::new(&st, "fake").await;
        let hello = adapter
            .hello(json!({"max_text": 4096, "typing": typing, "edit": edit}))
            .await;
        assert_eq!(hello.status, StatusCode::OK);
        seed_owner(&st.pool, "u1", "c1").await;
        let event = FakeAdapter::message_event("e1", "c1", "u1", "hi");
        assert_eq!(
            adapter.post_event(&event).await.status,
            StatusCode::ACCEPTED
        );
        let run_id: i64 = sqlx::query_scalar("SELECT id FROM runs WHERE chat_id = ?")
            .bind("fake:c1")
            .fetch_one(&st.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE runs SET status = 'running' WHERE id = ?")
            .bind(run_id)
            .execute(&st.pool)
            .await
            .unwrap();
        (st, adapter, run_id)
    }

    /// Polls once, acknowledges every delivered action and returns `(type, first text part)` of each.
    async fn deliver(adapter: &FakeAdapter) -> Vec<(String, String)> {
        let body = adapter.poll(Duration::ZERO).await.json();
        let mut seen = Vec::new();
        for action in body["actions"].as_array().expect("an actions array") {
            let kind = action["type"].as_str().unwrap().to_string();
            let text = action["parts"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let acked = adapter.ack_ok(action["id"].as_i64().unwrap()).await;
            assert_eq!(acked.status, StatusCode::OK);
            seen.push((kind, text));
        }
        seen
    }

    /// Puts `text` in the live stream tail of `run_id` and runs one streaming tick.
    async fn stream_tick(st: &AppState, run_id: i64, text: &str) {
        let line = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": text }] },
        });
        let buffer = Arc::new(std::sync::Mutex::new(line.to_string()));
        st.run_tails.lock().unwrap().insert(run_id, buffer);
        crate::channels::streaming::stream_progress(
            st,
            &mut crate::channels::streaming::Throttle::default(),
            std::time::Instant::now(),
            Duration::from_secs(5),
            Utc::now(),
        )
        .await;
    }

    async fn complete_turn(st: &AppState, run_id: i64, stdout: &str) {
        sqlx::query("UPDATE runs SET status = 'completed', stdout = ? WHERE id = ?")
            .bind(stdout)
            .bind(run_id)
            .execute(&st.pool)
            .await
            .unwrap();
        crate::channels::replies::reply_now(st, run_id).await;
    }

    #[tokio::test]
    async fn streaming_with_edit_capability_emits_progress_edits() {
        let (st, adapter, run_id) = streaming_turn(true, true).await;
        let mut wire = deliver(&adapter).await;
        stream_tick(&st, run_id, "halfway there").await;
        wire.extend(deliver(&adapter).await);
        complete_turn(&st, run_id, "All done.").await;
        wire.extend(deliver(&adapter).await);

        let kinds: Vec<&str> = wire.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(kinds, ["send", "edit", "edit", "send"]);
        assert_eq!(
            wire[0].1,
            strings::render(&Text::Working { turn: run_id })
        );
        assert_eq!(
            wire[1].1,
            strings::render(&Text::Progress {
                turn: run_id,
                snippet: "halfway there".into(),
            })
        );
        assert_eq!(wire[2].1, strings::render(&Text::Finished { turn: run_id }));
        assert_eq!(wire[3].1, "All done.");
        let classes: Vec<String> = sqlx::query_scalar(
            "SELECT class FROM channel_outbox WHERE action = 'edit' ORDER BY id",
        )
        .fetch_all(&st.pool)
        .await
        .unwrap();
        assert_eq!(classes, ["progress", "progress"]);
    }

    #[tokio::test]
    async fn streaming_without_edit_capability_emits_no_edit() {
        let (st, adapter, run_id) = streaming_turn(true, false).await;
        let mut wire = deliver(&adapter).await;
        stream_tick(&st, run_id, "halfway there").await;
        wire.extend(deliver(&adapter).await);
        complete_turn(&st, run_id, "All done.").await;
        wire.extend(deliver(&adapter).await);

        let kinds: Vec<&str> = wire.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(kinds, ["send", "send"]);
        assert_eq!(wire[1].1, "All done.");
        assert_eq!(
            count(&st.pool, "SELECT COUNT(*) FROM channel_outbox WHERE action = 'edit'").await,
            0
        );
    }

    #[tokio::test]
    async fn typing_refreshes_only_with_the_capability() {
        // A turn starting announces "typing" on the ephemeral lane of the next poll, once, and only
        // for an adapter that declared the capability.
        let (_st, with, _) = streaming_turn(true, true).await;
        let polled = with.poll(Duration::ZERO).await.json();
        assert_eq!(
            polled["ephemeral"],
            json!([{"type": "typing", "conversation": "c1"}])
        );
        let (_st, without, _) = streaming_turn(false, true).await;
        let polled = without.poll(Duration::ZERO).await.json();
        assert_eq!(polled["ephemeral"], json!([]));
    }
}
