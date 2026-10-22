//! A fake adapter used by the contract scenarios (test-only): it speaks the channel contract over
//! HTTP, through `channels::routes()` behind the real token middleware, with its own channel key.
//! It keeps no state of its own: every method is one request, and the caller decides the order.

use std::time::Duration;

use axum::body::Body;
use axum::http::{StatusCode, header};
use chrono::Utc;
use serde_json::Value;

use crate::auth::Service;
use crate::channels::contract::{ConversationKey, OutPart};
use crate::channels::outbox::{self, Class, NewAction};
use crate::channels::{replies, routing, streaming};
use crate::state::AppState;

/// What one request answered.
pub struct Reply {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

impl Reply {
    /// The body as JSON; `Null` when it is empty or not JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.bytes).unwrap_or(serde_json::Value::Null)
    }
}

/// One request through the channel routes and the real token middleware, as whoever holds `key`.
pub async fn request(
    state: &AppState,
    key: &str,
    method: &str,
    uri: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> Reply {
    use tower::ServiceExt;
    let app = crate::channels::routes()
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_token,
        ))
        .with_state(state.clone());
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {key}"));
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    let response = app
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Reply {
        status,
        content_type,
        bytes: bytes.to_vec(),
    }
}

/// An adapter of one channel, holding that channel's own key.
pub struct FakeAdapter {
    state: AppState,
    channel: &'static str,
    key: String,
}

impl FakeAdapter {
    /// Mints the channel's key (the supervisor does this at launch) and returns the adapter.
    pub async fn new(state: &AppState, channel: &'static str) -> Self {
        let key = crate::auth::mint_service_token(&state.pool, Service::Channel(channel))
            .await
            .unwrap();
        Self {
            state: state.clone(),
            channel,
            key,
        }
    }

    /// A message event of the contract (`type: message`, one text part).
    pub fn message_event(
        event_id: &str,
        conversation: &str,
        sender: &str,
        text: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "event_id": event_id,
            "conversation": {"ref": conversation, "kind": "private"},
            "sender": {"ref": sender},
            "type": "message",
            "message": {"ref": "m1", "parts": [{"type": "text", "text": text}]},
        })
    }

    fn path(&self, suffix: &str) -> String {
        format!("/channels/{}/{suffix}", self.channel)
    }

    async fn json_call(&self, method: &str, uri: &str, body: &serde_json::Value) -> Reply {
        request(
            &self.state,
            &self.key,
            method,
            uri,
            Some("application/json"),
            body.to_string().into_bytes(),
        )
        .await
    }

    /// Any request with the adapter's key, for the routes it must NOT reach.
    pub async fn raw(&self, method: &str, uri: &str, body: &str) -> Reply {
        request(
            &self.state,
            &self.key,
            method,
            uri,
            Some("application/json"),
            body.as_bytes().to_vec(),
        )
        .await
    }

    /// `PUT hello` with the given capabilities.
    pub async fn hello(&self, capabilities: serde_json::Value) -> Reply {
        let body = serde_json::json!({
            "kind": self.channel,
            "version": "1.0.0",
            "capabilities": capabilities,
        });
        self.json_call("PUT", &self.path("hello"), &body).await
    }

    /// `POST events`.
    pub async fn post_event(&self, event: &serde_json::Value) -> Reply {
        self.json_call("POST", &self.path("events"), event).await
    }

    /// `GET outbox?wait=<seconds>`: with a non-zero wait it parks until something is due.
    pub async fn poll(&self, wait: Duration) -> Reply {
        let uri = format!("{}?wait={}", self.path("outbox"), wait.as_secs());
        request(&self.state, &self.key, "GET", &uri, None, Vec::new()).await
    }

    /// `POST outbox/{id}/ack` with an arbitrary body.
    pub async fn ack(&self, id: i64, body: &serde_json::Value) -> Reply {
        self.json_call("POST", &self.path(&format!("outbox/{id}/ack")), body)
            .await
    }

    /// The acknowledgement a well-behaved adapter sends after delivering: `ok`, with the platform
    /// ref `p<id>`.
    pub async fn ack_ok(&self, id: i64) -> Reply {
        let body = serde_json::json!({"ok": true, "platform_refs": [format!("p{id}")]});
        self.ack(id, &body).await
    }

    /// `POST outbox/{id}/renew`.
    pub async fn renew(&self, id: i64) -> Reply {
        request(
            &self.state,
            &self.key,
            "POST",
            &self.path(&format!("outbox/{id}/renew")),
            None,
            Vec::new(),
        )
        .await
    }

    /// `POST media`: raw bytes, the `Content-Type` is the mime.
    pub async fn upload(&self, mime: &str, bytes: Vec<u8>) -> Reply {
        request(
            &self.state,
            &self.key,
            "POST",
            &self.path("media"),
            Some(mime),
            bytes,
        )
        .await
    }

    /// `GET media/{media_id}`.
    pub async fn download(&self, media_id: &str) -> Reply {
        request(
            &self.state,
            &self.key,
            "GET",
            &self.path(&format!("media/{media_id}")),
            None,
            Vec::new(),
        )
        .await
    }
}

// ---- the `core` stimuli of the capability scenarios (Anexo C, C.10.1) --------------------------

/// What a stimulus did: `Err` names why it could not be applied.
pub type Stimulated = Result<(), String>;

fn db(error: sqlx::Error) -> String {
    format!("database error: {error}")
}

fn text_of<'a>(stimulus: &'a Value, name: &str) -> Result<&'a str, String> {
    stimulus[name]
        .as_str()
        .ok_or_else(|| format!("the stimulus has no text field {name:?}: {stimulus}"))
}

fn number_of(stimulus: &Value, name: &str) -> Result<i64, String> {
    stimulus[name]
        .as_i64()
        .ok_or_else(|| format!("the stimulus has no number field {name:?}: {stimulus}"))
}

/// One routing pass over the feed and the proposals, as the routing loop would run after a signal.
async fn announce(state: &AppState) -> Stimulated {
    let wanted = routing::Wanted {
        feed: true,
        proposals: true,
        ..routing::Wanted::default()
    };
    routing::route_once(state, Utc::now(), wanted)
        .await
        .map(|_| ())
        .map_err(db)
}

/// Registers a media blob the scenario can name in `send_parts` (only the row, never the bytes:
/// degradation reads the display name, the adapter would read the file).
pub async fn register_media(
    state: &AppState,
    channel: &str,
    id: &str,
    name: Option<&str>,
    mime: &str,
) -> Stimulated {
    sqlx::query(
        "INSERT INTO channel_media (id, channel, path, mime, name, bytes, created_at) \
         VALUES (?, ?, 'channels/fake/scenario', ?, ?, 1, ?)",
    )
    .bind(id)
    .bind(channel)
    .bind(mime)
    .bind(name)
    .bind(Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await
    .map(|_| ())
    .map_err(db)
}

/// A pending approval with the id the scenario names. It is a paused run's `action-approval`,
/// the only kind the chat announces with buttons, so approving it from the chat really resumes
/// the run (through the state's fake runner).
async fn proposal_pending(state: &AppState, id: i64, title: &str) -> Stimulated {
    let pool = &state.pool;
    let now = Utc::now().to_rfc3339();
    let run: i64 = sqlx::query_scalar(
        "INSERT INTO runs (project_id, cwd, prompt, status, session_id, mode, created_at) \
         VALUES ('proj', 'C:/worktrees/proj/paused', 'x', 'awaiting_approval', 'sess-a', \
         'worktree', ?) RETURNING id",
    )
    .bind(&now)
    .fetch_one(pool)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO worktrees (owner_kind, owner_id, project_id, project_root, path, branch, \
         created_at) VALUES ('run', ?, 'proj', 'C:/repos/proj', ?, ?, ?)",
    )
    .bind(run)
    .bind(format!("C:/worktrees/proj/run-{run}"))
    .bind(format!("nucleos/run-{run}"))
    .bind(&now)
    .execute(pool)
    .await
    .map_err(db)?;
    let made = crate::proposals::create_action_approval(
        pool,
        run,
        Some("sess-a"),
        Some("proj"),
        "Bash",
        title,
        Some(r#"{"command":"git push origin main"}"#),
    )
    .await
    .map_err(db)?;
    if made != id {
        sqlx::query("UPDATE proposals SET id = ? WHERE id = ?")
            .bind(id)
            .bind(made)
            .execute(pool)
            .await
            .map_err(db)?;
        sqlx::query("UPDATE proposal_events SET proposal_id = ? WHERE proposal_id = ?")
            .bind(id)
            .bind(made)
            .execute(pool)
            .await
            .map_err(db)?;
    }
    announce(state).await
}

/// An open capture request for job `job`, announced through the feed like the distiller's.
async fn capture_requested(state: &AppState, job: i64, summary: &str) -> Stimulated {
    sqlx::query(
        "INSERT OR IGNORE INTO jobs (id, project_id, project_root, status, max_items, created_at)
            VALUES (?, 'p', '/tmp', 'failed', 3, 't')",
    )
    .bind(job)
    .execute(&state.pool)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, \
         deadline, created_at) VALUES (?, 'p', '[]', ?, 'open', '2099-01-01T00:00:00Z', ?)",
    )
    .bind(job)
    .bind(summary)
    .bind(Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await
    .map_err(db)?;
    nucleos_base::feed::append(
        &state.pool,
        None,
        crate::capture::FEED_KIND,
        summary,
        None,
        Some(&nucleos_base::feed::Subject::Job(job)),
    )
    .await
    .map_err(db)?;
    announce(state).await
}

/// Ends, with `stdout`, the newest turn of a `fake` conversation and answers it.
async fn turn_finished(state: &AppState, stdout: &str) -> Stimulated {
    let run: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM runs WHERE chat_id LIKE 'fake:%' ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(db)?;
    let run = run.ok_or("no turn of a fake conversation is open")?;
    sqlx::query("UPDATE runs SET status = 'completed', stdout = ? WHERE id = ?")
        .bind(stdout)
        .bind(run)
        .execute(&state.pool)
        .await
        .map_err(db)?;
    replies::reply_now(state, run).await;
    Ok(())
}

/// Gives the newest turn of a `fake` conversation `text` as its live output so far and runs one
/// streaming tick, as the replies loop would: the ack gets a progress edit when the channel can edit.
async fn stream_text(state: &AppState, text: &str) -> Stimulated {
    let run: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM runs WHERE chat_id LIKE 'fake:%' ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(db)?;
    let run = run.ok_or("no turn of a fake conversation is open")?;
    let line = serde_json::json!({
        "type": "assistant",
        "message": { "content": [{ "type": "text", "text": text }] },
    });
    let tail = std::sync::Arc::new(std::sync::Mutex::new(line.to_string()));
    state
        .run_tails
        .lock()
        .map_err(|e| e.to_string())?
        .insert(run, tail);
    // A fresh throttle per tick: the scenario drives the cadence, not the wall clock.
    streaming::stream_progress(
        state,
        &mut streaming::Throttle::default(),
        tokio::time::Instant::now(),
        Duration::ZERO,
        Utc::now(),
    )
    .await;
    Ok(())
}

/// Queues a `send` of `parts` the common way, where degradation happens at lease time.
async fn send_parts(state: &AppState, conversation: &str, parts: &Value) -> Stimulated {
    let parts: Vec<OutPart> = serde_json::from_value(parts.clone())
        .map_err(|error| format!("the parts are not outbound parts: {error}"))?;
    let mut conn = state.pool.acquire().await.map_err(db)?;
    outbox::enqueue(
        &mut conn,
        &ConversationKey::new("fake", conversation),
        NewAction::Send {
            reply_to: None,
            parts,
            buttons: None,
            silent: false,
        },
        Class::Turn,
        None,
        Utc::now(),
    )
    .await
    .map_err(db)?;
    drop(conn);
    outbox::wake(state, "fake");
    Ok(())
}

/// Applies one `core` stimulus of a scenario to the test state, discriminated by `do`.
pub async fn stimulate(state: &AppState, stimulus: &Value) -> Stimulated {
    match stimulus["do"].as_str() {
        Some("proposal_pending") => {
            let id = number_of(stimulus, "proposal_id")?;
            proposal_pending(state, id, text_of(stimulus, "title")?).await
        }
        Some("feed_row") => {
            nucleos_base::feed::append(
                &state.pool,
                stimulus["project_id"].as_str(),
                text_of(stimulus, "kind")?,
                text_of(stimulus, "text")?,
                None,
                None,
            )
            .await
            .map_err(db)?;
            announce(state).await
        }
        Some("capture_requested") => {
            let job = number_of(stimulus, "capture")?;
            capture_requested(state, job, text_of(stimulus, "summary")?).await
        }
        Some("turn_finished") => turn_finished(state, text_of(stimulus, "stdout")?).await,
        Some("stream_text") => stream_text(state, text_of(stimulus, "text")?).await,
        Some("send_parts") => {
            send_parts(
                state,
                text_of(stimulus, "conversation")?,
                &stimulus["parts"],
            )
            .await
        }
        other => Err(format!("unknown stimulus {other:?}")),
    }
}
