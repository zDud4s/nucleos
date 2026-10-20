//! A fake adapter used by the contract scenarios (test-only): it speaks the channel contract over
//! HTTP, through `channels::routes()` behind the real token middleware, with its own channel key.
//! It keeps no state of its own: every method is one request, and the caller decides the order.

use std::time::Duration;

use axum::body::Body;
use axum::http::{StatusCode, header};

use crate::auth::Service;
use crate::state::AppState;

/// What one request answered.
pub(crate) struct Reply {
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
pub(crate) async fn request(
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
pub(crate) struct FakeAdapter {
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
