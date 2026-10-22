//! The HTTP surface of the live fleet tree (spec 2026-10-20 §5).
//!
//! `GET /fleet/live` streams NDJSON; `GET /fleet/nodes/{id}/transcript` pages the full
//! transcript; `POST`/`DELETE /fleet/pins/{root_id}` pin a tree. None of the three is in any
//! scope table of `auth.rs`, so only the control token and an Admin key reach them: a transcript
//! holds the commands and file text of every local session, and a ReadOnly key must not read it.

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use tokio_util::io::ReaderStream;

use super::book::PinError;
use super::hub::{Line, Subscription, TranscriptError};
use super::limits::HEARTBEAT;
use crate::state::AppState;

/// The pipe between the pump and the HTTP body. Small on purpose: a client that cannot keep up
/// should fill its hub channel (and be sent a fresh snapshot) rather than a megabyte of pipe.
const PIPE_BYTES: usize = 64 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/fleet/live", get(get_live))
        .route("/fleet/nodes/{id}/transcript", get(get_transcript))
        .route("/fleet/pins/{root_id}", post(post_pin).delete(delete_pin))
}

async fn get_live(State(state): State<AppState>) -> Response {
    let hub = super::hub::hub(&state);
    let subscription = hub.subscribe().await;
    let (reader, writer) = tokio::io::duplex(PIPE_BYTES);
    tokio::spawn(pump(subscription, writer));
    let mut response = Response::new(Body::from_stream(ReaderStream::new(reader)));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Copies the subscription into the pipe until either side ends. Dropping `subscription` leaves
/// the hub (the last one out stops it); dropping `out` ends the body.
async fn pump(subscription: Subscription, out: tokio::io::DuplexStream) {
    pump_idle(subscription, out, HEARTBEAT).await;
}

/// The wire form of a heartbeat, identical to what the hub publishes.
const HEARTBEAT_LINE: &str = "{\"t\":\"heartbeat\"}\n";

/// `pump` with a liveness floor: when nothing was written for `idle`, the pump writes a
/// heartbeat itself. The hub task runs discover and read passes (each bounded by a 10 s source
/// timeout) in the same loop that publishes its own heartbeat, so a slow adapter could leave the
/// stream silent past the shell's 30 s dead-link limit; this timer does not depend on the hub.
/// The timer restarts after every write, so a healthy hub's heartbeat keeps this one quiet.
async fn pump_idle(
    mut subscription: Subscription,
    mut out: tokio::io::DuplexStream,
    idle: std::time::Duration,
) {
    loop {
        let line: Line = tokio::select! {
            next = subscription.next() => match next {
                Some(line) => line,
                None => break,
            },
            () = tokio::time::sleep(idle) => HEARTBEAT_LINE.into(),
        };
        if out.write_all(line.as_bytes()).await.is_err() {
            break;
        }
    }
}

#[derive(Deserialize)]
struct TranscriptQuery {
    before: Option<String>,
}

fn refusal(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

async fn get_transcript(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> Response {
    let hub = super::hub::hub(&state);
    match hub.transcript(&id, query.before).await {
        Ok(page) => Json(page).into_response(),
        Err(TranscriptError::UnknownNode) => refusal(StatusCode::NOT_FOUND, "unknown_node"),
        Err(TranscriptError::NoTranscript) => refusal(StatusCode::CONFLICT, "no_transcript"),
        Err(TranscriptError::Failed(reason)) => {
            tracing::warn!(node = %id, %reason, "fleet transcript failed");
            refusal(StatusCode::BAD_GATEWAY, "transcript_failed")
        }
    }
}

async fn set_pin(state: AppState, root_id: String, on: bool) -> Response {
    let hub = super::hub::hub(&state);
    match hub.pin(&root_id, on).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(PinError::Unknown) => refusal(StatusCode::NOT_FOUND, "unknown_node"),
        Err(PinError::NotARoot) => refusal(StatusCode::CONFLICT, "not_a_root"),
    }
}

async fn post_pin(State(state): State<AppState>, Path(root_id): Path<String>) -> Response {
    set_pin(state, root_id, true).await
}

async fn delete_pin(State(state): State<AppState>, Path(root_id): Path<String>) -> Response {
    set_pin(state, root_id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::hub::{FleetHub, HubConfig};
    use crate::fleet::model::{TermKind, TranscriptEntry, TranscriptPage};
    use crate::fleet::source::AgentSource;
    use crate::fleet::testkit::{FakeClock, FakeSource, FakeWatch, at, sighting};
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, BufReader};

    struct Fixture {
        state: AppState,
        hub: Arc<FleetHub>,
        _dir: tempfile::TempDir,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let source = FakeSource::new("fake", vec![dir.path().to_path_buf()]);
        let mut root = sighting("claude-code", "a");
        root.has_transcript = true;
        let mut kid = sighting("claude-code", "kid");
        kid.parent = Some(root.key.clone());
        source.set(vec![root, kid]);
        source.set_transcript(TranscriptPage {
            entries: vec![TranscriptEntry {
                kind: TermKind::Tool,
                text: "Read src/a.rs".into(),
                detail: Some("the whole file".into()),
            }],
            before: Some("42".into()),
        });
        let agent: Arc<dyn AgentSource> = source;
        let hub = FleetHub::new(
            vec![agent],
            vec![],
            FakeClock::new(at(1_000)),
            FakeWatch::new(),
            HubConfig::default(),
        );
        let state = crate::channels::testkit::state().await;
        crate::fleet::hub::install(&state.fleet, hub.clone());
        Fixture {
            state,
            hub,
            _dir: dir,
        }
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("a JSON body")
    }

    #[tokio::test]
    async fn the_live_route_answers_ndjson_and_the_hub_runs_while_the_body_is_open() {
        let f = fixture().await;
        let response = get_live(State(f.state.clone())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/x-ndjson"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(f.hub.is_running());

        drop(response); // the client went away
        for _ in 0..50 {
            f.hub.emit_heartbeat();
            if !f.hub.is_running() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!f.hub.is_running(), "the last client leaving stops the hub");
    }

    // Plain `#[tokio::test]` plus `tokio::time::pause()` once the fixture exists: `start_paused`
    // would also pause the clock while `channels::testkit::state()` opens its SQLite pool, and
    // the pool's worker threads need real time to answer.
    #[tokio::test]
    async fn the_pump_writes_one_json_object_per_line_starting_with_a_snapshot() {
        let f = fixture().await;
        tokio::time::pause();
        let subscription = f.hub.subscribe().await;
        let (reader, writer) = tokio::io::duplex(PIPE_BYTES);
        tokio::spawn(pump(subscription, writer));
        let mut lines = BufReader::new(reader).lines();

        let first: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["t"], "snapshot");
        let ids: Vec<_> = first["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["claude-code:a", "claude-code:kid"]);

        f.hub.emit_heartbeat();
        assert_eq!(
            lines.next_line().await.unwrap().unwrap(),
            r#"{"t":"heartbeat"}"#
        );
    }

    #[tokio::test]
    async fn the_pump_heartbeats_on_its_own_when_the_hub_is_silent() {
        let f = fixture().await;
        tokio::time::pause();
        let subscription = f.hub.subscribe().await;
        let (reader, writer) = tokio::io::duplex(PIPE_BYTES);
        // One second idle is far below the hub's own 15 s heartbeat, so a heartbeat here is the
        // pump's.
        tokio::spawn(pump_idle(
            subscription,
            writer,
            std::time::Duration::from_secs(1),
        ));
        let mut lines = BufReader::new(reader).lines();

        let first: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["t"], "snapshot");
        for _ in 0..2 {
            assert_eq!(
                lines.next_line().await.unwrap().unwrap(),
                r#"{"t":"heartbeat"}"#
            );
        }
    }

    #[tokio::test]
    async fn a_reader_that_goes_away_ends_the_pump_and_frees_the_hub() {
        let f = fixture().await;
        tokio::time::pause();
        let subscription = f.hub.subscribe().await;
        let (reader, writer) = tokio::io::duplex(PIPE_BYTES);
        let pumping = tokio::spawn(pump(subscription, writer));
        drop(reader);

        f.hub.emit_heartbeat();
        tokio::time::timeout(std::time::Duration::from_secs(1), pumping)
            .await
            .expect("the pump noticed")
            .unwrap();
        assert!(!f.hub.is_running());
    }

    #[tokio::test]
    async fn the_transcript_route_pages_and_refuses_what_it_cannot_serve() {
        let f = fixture().await;
        let page = get_transcript(
            State(f.state.clone()),
            Path("claude-code:a".into()),
            Query(TranscriptQuery { before: None }),
        )
        .await;
        assert_eq!(page.status(), StatusCode::OK);
        let page = body_json(page).await;
        assert_eq!(page["entries"][0]["text"], "Read src/a.rs");
        assert_eq!(page["entries"][0]["detail"], "the whole file");
        assert_eq!(page["before"], "42");

        let unknown = get_transcript(
            State(f.state.clone()),
            Path("claude-code:zzz".into()),
            Query(TranscriptQuery { before: None }),
        )
        .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(unknown).await,
            serde_json::json!({"error": "unknown_node"})
        );

        let none = get_transcript(
            State(f.state.clone()),
            Path("claude-code:kid".into()),
            Query(TranscriptQuery { before: None }),
        )
        .await;
        assert_eq!(none.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(none).await,
            serde_json::json!({"error": "no_transcript"})
        );
    }

    #[tokio::test]
    async fn pins_answer_204_404_and_409() {
        let f = fixture().await;
        let pin = |id: &str, on: bool| {
            let state = f.state.clone();
            let id = id.to_owned();
            async move {
                if on {
                    post_pin(State(state), Path(id)).await
                } else {
                    delete_pin(State(state), Path(id)).await
                }
            }
        };

        assert_eq!(
            pin("claude-code:a", true).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            pin("claude-code:a", false).await.status(),
            StatusCode::NO_CONTENT
        );
        let unknown = pin("claude-code:ghost", true).await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(unknown).await,
            serde_json::json!({"error": "unknown_node"})
        );
        let child = pin("claude-code:kid", true).await;
        assert_eq!(child.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(child).await,
            serde_json::json!({"error": "not_a_root"})
        );
    }

    #[tokio::test]
    async fn the_router_decodes_an_encoded_node_id_and_routes_each_verb() {
        use tower::ServiceExt as _;
        let f = fixture().await;
        let app = routes().with_state(f.state.clone());

        let transcript = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/fleet/nodes/claude-code%3Aa/transcript?before=7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(transcript.status(), StatusCode::OK);
        assert_eq!(body_json(transcript).await["before"], "42");

        let pinned = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/fleet/pins/claude-code%3Aa")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pinned.status(), StatusCode::NO_CONTENT);

        let unpinned = app
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri("/fleet/pins/claude-code%3Aa")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unpinned.status(), StatusCode::NO_CONTENT);
    }
}
