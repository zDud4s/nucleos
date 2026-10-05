//! `GET /browser/sessions/{id}/live`: the sidecar's record stream, proxied to the shell for
//! agent-mode sessions only and cut at a record boundary the moment the mode leaves `agent`
//! (spec §3.2, browser-ao-vivo).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::watch;
use tokio_util::io::ReaderStream;

use crate::browser::{self, BrowserRuntime, mode};
use crate::state::AppState;

/// The record kind that ends a stream; every other kind is a frame, passed through untouched.
const END: u8 = b'E';

/// A record is kind (1 byte) + body length (u32, big-endian) + body. Past this the stream is not
/// one this module understands, and it is ended rather than buffered.
const MAX_BODY: usize = 16 * 1024 * 1024;
const HEADER: usize = 5;

/// One record of the stream. The body is opaque: a frame is never decoded, logged or stored here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub kind: u8,
    pub body: Vec<u8>,
}

impl Record {
    fn to_wire(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + self.body.len());
        out.push(self.kind);
        out.extend_from_slice(&(self.body.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.body);
        out
    }
}

fn end_record(reason: &str) -> Record {
    Record {
        kind: END,
        body: serde_json::to_vec(&serde_json::json!({ "reason": reason })).unwrap_or_default(),
    }
}

/// Reassembles records from chunks cut anywhere, the header's middle included. Only whole records
/// ever come out; what has half arrived stays inside until the rest does.
#[derive(Debug, Default)]
pub struct RecordReader {
    buffer: Vec<u8>,
    /// A header announced a body past `MAX_BODY`: nothing after it can be trusted.
    broken: bool,
}

impl RecordReader {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Record> {
        if self.broken {
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        let mut records = Vec::new();
        let mut at = 0;
        while self.buffer.len() - at >= HEADER {
            let length = u32::from_be_bytes([
                self.buffer[at + 1],
                self.buffer[at + 2],
                self.buffer[at + 3],
                self.buffer[at + 4],
            ]) as usize;
            if length > MAX_BODY {
                self.broken = true;
                self.buffer.clear();
                return records;
            }
            if self.buffer.len() - at < HEADER + length {
                break;
            }
            records.push(Record {
                kind: self.buffer[at],
                body: self.buffer[at + HEADER..at + HEADER + length].to_vec(),
            });
            at += HEADER + length;
        }
        self.buffer.drain(..at);
        records
    }

    fn is_broken(&self) -> bool {
        self.broken
    }
}

/// Where a session's mode changes are announced to whoever is watching it. An entry exists only
/// while somebody watches: `publish` to a session nobody subscribed to creates nothing, and
/// `release` forgets an entry once its last receiver is gone.
#[derive(Debug, Default)]
pub struct ModeChannels(pub Mutex<HashMap<i64, watch::Sender<String>>>);

impl ModeChannels {
    pub fn subscribe(&self, id: i64) -> watch::Receiver<String> {
        let mut channels = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        channels
            .entry(id)
            .or_insert_with(|| watch::channel(mode::AGENT.to_owned()).0)
            .subscribe()
    }

    pub fn publish(&self, id: i64, to: &str) {
        let channels = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sender) = channels.get(&id) {
            sender.send_replace(to.to_owned());
        }
    }

    pub fn release(&self, id: i64) {
        let mut channels = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if channels
            .get(&id)
            .is_some_and(|sender| sender.receiver_count() == 0)
        {
            channels.remove(&id);
        }
    }
}

/// A subscription that gives itself back, whichever way the request ends.
struct LiveGuard {
    runtime: Arc<BrowserRuntime>,
    id: i64,
    rx: Option<watch::Receiver<String>>,
}

impl LiveGuard {
    fn new(runtime: Arc<BrowserRuntime>, id: i64) -> Self {
        let rx = Some(runtime.modes.subscribe(id));
        Self { runtime, id, rx }
    }

    fn rx(&mut self) -> &mut watch::Receiver<String> {
        self.rx.as_mut().expect("the receiver lives until drop")
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        // The receiver first, so `release` sees the count without it.
        self.rx = None;
        self.runtime.modes.release(self.id);
    }
}

pub async fn get_live(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    start(&state, id, async {}).await
}

/// `after_read` runs between the row being read as `agent` and the sidecar being asked: a test
/// hook for the window a check-then-act proxy loses.
async fn start(
    state: &AppState,
    id: i64,
    after_read: impl Future<Output = ()> + Send + 'static,
) -> Response {
    if !state.browser.enabled {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": "pillar_off",
                "detail": "the browser pillar is off",
            })),
        )
            .into_response();
    }
    // Subscribed BEFORE the row is read, so a mode change landing in between is seen by the pump.
    let guard = LiveGuard::new(state.browser.clone(), id);
    let Some(row) = browser::live_session(state, id).await else {
        return browser::gone();
    };
    if row.mode != mode::AGENT {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": "not_agent",
                "detail": format!(
                    "this session is {}, so what is on its screen is theirs",
                    row.mode
                ),
            })),
        )
            .into_response();
    }
    after_read.await;
    let upstream = match state.browser.client.watch(&row.sidecar_id).await {
        Ok(response) => response,
        Err(error) => return browser::browser_error(error),
    };
    let (reader, writer) = tokio::io::duplex(256 * 1024);
    tokio::spawn(pump(upstream, writer, guard));
    let mut response = Response::new(Body::from_stream(ReaderStream::new(reader)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response
}

/// Forwards whole records until the mode leaves `agent`, the sidecar ends, or the client goes.
/// Frames pass through untouched; the only record this writes itself is the end.
async fn pump(
    mut upstream: reqwest::Response,
    mut out: tokio::io::DuplexStream,
    mut guard: LiveGuard,
) {
    use tokio::io::AsyncWriteExt as _;
    let mut reader = RecordReader::default();
    let mut ended = false;
    'outer: loop {
        let chunk = tokio::select! {
            biased;
            changed = guard.rx().changed() => {
                if changed.is_err() || *guard.rx().borrow() != mode::AGENT {
                    let _ = out.write_all(&end_record("wheel").to_wire()).await;
                    ended = true;
                    break 'outer;
                }
                continue;
            }
            chunk = upstream.chunk() => chunk,
        };
        match chunk {
            Ok(Some(bytes)) => {
                for record in reader.push(&bytes) {
                    if *guard.rx().borrow() != mode::AGENT {
                        let _ = out.write_all(&end_record("wheel").to_wire()).await;
                        ended = true;
                        break 'outer;
                    }
                    if out.write_all(&record.to_wire()).await.is_err() {
                        // The client is gone; dropping `upstream` closes the sidecar's request.
                        return;
                    }
                    if record.kind == END {
                        ended = true;
                        break 'outer;
                    }
                }
                if reader.is_broken() {
                    break 'outer;
                }
            }
            Ok(None) | Err(_) => break 'outer,
        }
    }
    if !ended {
        let _ = out.write_all(&end_record("gone").to_wire()).await;
    }
    let _ = out.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{Ask, Opened};
    use crate::browser_client::BrowserClient;
    use crate::browser_policy::{Requester, Surface};
    use crate::state::AppState;
    use crate::storage::TempDb;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt as _;

    const NOW: &str = "2026-08-16T10:00:00Z";

    /// What the stub sidecar's `/watch` does with its stream.
    #[derive(Clone, Copy)]
    enum Script {
        /// A whole `F` record every 20 ms, until the reader goes away.
        Frames,
        /// One whole frame, then half of a second one, then silence for good.
        HalfThenStall,
        /// One whole frame, then half of a second one, then the stream ends.
        HalfThenEof,
    }

    /// One record as the sidecar writes it: kind byte, u32 big-endian length, body.
    fn wire(kind: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![kind];
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    /// Parses a collected proxy body with the module's own reader, and proves the body was made of
    /// whole records and nothing else: re-encoding what was read must give back every byte.
    fn records_of(bytes: &[u8]) -> Vec<Record> {
        let records = RecordReader::default().push(bytes);
        let again: Vec<u8> = records
            .iter()
            .flat_map(|record| wire(record.kind, &record.body))
            .collect();
        assert_eq!(
            again, bytes,
            "the proxied body must end on a record boundary, with no partial record in it"
        );
        records
    }

    fn end_reason(record: &Record) -> String {
        assert_eq!(record.kind, b'E', "expected an end record");
        let parsed: serde_json::Value = serde_json::from_slice(&record.body).expect("end json");
        parsed["reason"].as_str().expect("reason").to_owned()
    }

    /// A sidecar that answers `/open` and streams `/watch` by `script`; the counter says how many
    /// times `/watch` was asked for.
    async fn stub_sidecar(
        script: Script,
    ) -> (Arc<crate::browser::BrowserRuntime>, Arc<AtomicUsize>) {
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let watches = Arc::new(AtomicUsize::new(0));
        let counter = watches.clone();
        let app = axum::Router::new()
            .route(
                "/open",
                post(|_body: axum::body::Bytes| async {
                    axum::Json(serde_json::json!({
                        "id": "s1",
                        "mode": "agent",
                        "requested_url": "https://jira.example.org/login",
                        "final_url": "https://jira.example.org/login",
                        "title": "",
                    }))
                    .into_response()
                }),
            )
            .route(
                "/watch",
                post(move |_body: axum::body::Bytes| {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        let (reader, mut writer) = tokio::io::duplex(64 * 1024);
                        tokio::spawn(async move {
                            match script {
                                Script::Frames => {
                                    let mut n = 0u32;
                                    loop {
                                        let body = format!("frame-{n}");
                                        if writer
                                            .write_all(&wire(b'F', body.as_bytes()))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                        n += 1;
                                        tokio::time::sleep(Duration::from_millis(20)).await;
                                    }
                                }
                                Script::HalfThenStall | Script::HalfThenEof => {
                                    let whole = wire(b'F', b"whole-frame");
                                    let second = wire(b'F', b"0123456789");
                                    // The header and three bytes of ten: a record cut in the middle.
                                    let half = &second[..5 + 3];
                                    if writer.write_all(&whole).await.is_err()
                                        || writer.write_all(half).await.is_err()
                                    {
                                        return;
                                    }
                                    let _ = writer.flush().await;
                                    if matches!(script, Script::HalfThenStall) {
                                        std::future::pending::<()>().await;
                                    }
                                    // Dropping the writer is the end of the stream.
                                }
                            }
                        });
                        axum::response::Response::builder()
                            .header("content-type", "application/octet-stream")
                            .body(axum::body::Body::from_stream(
                                tokio_util::io::ReaderStream::new(reader),
                            ))
                            .unwrap()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            Arc::new(crate::browser::BrowserRuntime {
                enabled: true,
                client: BrowserClient::new(&address.to_string(), "tok".into()),
                modes: Default::default(),
            }),
            watches,
        )
    }

    async fn live(script: Script, enabled: bool) -> (TempDb, AppState, Arc<AtomicUsize>) {
        let db = TempDb::new().await;
        let (browser, watches) = stub_sidecar(script).await;
        let browser = if enabled {
            browser
        } else {
            Arc::new(crate::browser::BrowserRuntime::disabled())
        };
        let state = AppState {
            token: crate::auth::Token("test-token".into()),
            pool: db.pool.clone(),
            telegram_doctrine: None,
            runner: Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: Arc::new(crate::assistants::NoAssistants),
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::voice::VoiceRuntime::default()),
            browser,
            github: Arc::new(crate::github::GithubRuntime::default()),
            web: Arc::new(crate::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::calendar::CalendarRuntime::default()),
            council: Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };
        (db, state, watches)
    }

    /// An agent-mode session, opened through the stub sidecar.
    async fn an_agent_session(state: &AppState) -> i64 {
        let opened = crate::browser::open(
            &state.pool,
            &state.browser,
            Ask {
                project_id: "acme",
                run_id: Some(7),
                url: "https://jira.example.org/login",
                surface: Surface::Assistant,
                requester: Requester::Owner,
                now: NOW,
            },
        )
        .await
        .expect("open");
        let Opened::Session(row) = opened else {
            panic!("must open");
        };
        row.id
    }

    /// Moves the session out of `agent` the way the wheel does, which publishes on the channel.
    async fn leave_agent(state: &AppState, id: i64) {
        let moved = crate::browser::set_mode(
            &state.pool,
            &state.browser.modes,
            id,
            crate::browser::mode::AGENT,
            crate::browser::mode::WHEEL_REQUESTED,
        )
        .await
        .expect("set_mode");
        assert!(moved, "the session was in agent mode");
    }

    async fn collect(response: axum::response::Response) -> Vec<u8> {
        tokio::time::timeout(
            Duration::from_secs(10),
            axum::body::to_bytes(response.into_body(), usize::MAX),
        )
        .await
        .expect("the proxy must close the stream")
        .expect("body")
        .to_vec()
    }

    #[tokio::test]
    async fn live_refuses_a_session_that_is_not_agent() {
        let (_db, state, watches) = live(Script::Frames, true).await;
        let id = an_agent_session(&state).await;
        leave_agent(&state, id).await;

        let response = start(&state, id, async {}).await;

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
        assert_eq!(body["refusal"], "not_agent");
        assert!(
            body["detail"].as_str().unwrap().contains("wheel-requested"),
            "the detail names the mode: {body}"
        );
        assert_eq!(
            watches.load(Ordering::SeqCst),
            0,
            "a refused session must never reach the sidecar's /watch"
        );
    }

    #[tokio::test]
    async fn live_answers_404_for_an_unknown_session() {
        let (_db, state, watches) = live(Script::Frames, true).await;

        let response = get_live(
            axum::extract::State(state.clone()),
            axum::extract::Path(9999),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(watches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn live_refuses_when_the_pillar_is_off() {
        let (_db, state, watches) = live(Script::Frames, false).await;

        let response = start(&state, 1, async {}).await;

        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).expect("a JSON refusal");
        assert_eq!(watches.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn live_cuts_with_end_wheel_when_the_mode_leaves_agent() {
        let (_db, state, watches) = live(Script::Frames, true).await;
        let id = an_agent_session(&state).await;

        let response = get_live(axum::extract::State(state.clone()), axum::extract::Path(id)).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
        assert_eq!(watches.load(Ordering::SeqCst), 1);

        let flipper = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            leave_agent(&flipper, id).await;
        });
        let records = records_of(&collect(response).await);

        assert!(
            records.len() >= 2,
            "some frames, then the end: {}",
            records.len()
        );
        let (last, frames) = records.split_last().unwrap();
        assert_eq!(end_reason(last), "wheel");
        assert!(
            frames
                .iter()
                .all(|record| record.kind == b'F' && record.body.starts_with(b"frame-")),
            "everything before the end is a frame, untouched"
        );
    }

    #[tokio::test]
    async fn live_sees_a_mode_change_that_lands_after_the_row_was_read() {
        let (_db, state, _watches) = live(Script::Frames, true).await;
        let id = an_agent_session(&state).await;

        // The hook runs between the row being read as `agent` and the sidecar being asked: the
        // window a check-then-act proxy loses.
        let hook_state = state.clone();
        let response = start(&state, id, async move {
            leave_agent(&hook_state, id).await;
        })
        .await;

        let records = records_of(&collect(response).await);
        assert_eq!(records.len(), 1, "not one frame may be forwarded");
        assert_eq!(end_reason(&records[0]), "wheel");
    }

    #[tokio::test]
    async fn live_cut_mid_frame_ends_on_a_record_boundary() {
        let (_db, state, _watches) = live(Script::HalfThenStall, true).await;
        let id = an_agent_session(&state).await;

        let response = get_live(axum::extract::State(state.clone()), axum::extract::Path(id)).await;
        let flipper = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            leave_agent(&flipper, id).await;
        });
        let records = records_of(&collect(response).await);

        assert_eq!(records.len(), 2, "the whole frame, then the end");
        assert_eq!(records[0].kind, b'F');
        assert_eq!(records[0].body, b"whole-frame");
        assert_eq!(end_reason(&records[1]), "wheel");
    }

    #[tokio::test]
    async fn live_sidecar_eof_mid_record_drops_it_and_ends_gone() {
        let (_db, state, _watches) = live(Script::HalfThenEof, true).await;
        let id = an_agent_session(&state).await;

        let response = get_live(axum::extract::State(state.clone()), axum::extract::Path(id)).await;
        let records = records_of(&collect(response).await);

        assert_eq!(records.len(), 2, "the whole frame, then the end");
        assert_eq!(records[0].body, b"whole-frame");
        assert_eq!(end_reason(&records[1]), "gone");
    }

    #[test]
    fn live_record_reader_reassembles_records_split_across_chunks() {
        let mut stream = wire(b'F', b"first-jpeg");
        stream.extend(wire(b'F', b""));
        stream.extend(wire(b'E', br#"{"reason":"closed"}"#));

        // One byte at a time: every split point, the header's included, is exercised.
        let mut reader = RecordReader::default();
        let mut slow = Vec::new();
        for byte in &stream {
            slow.extend(reader.push(std::slice::from_ref(byte)));
        }
        // And all at once.
        let fast = RecordReader::default().push(&stream);

        for records in [&slow, &fast] {
            assert_eq!(records.len(), 3);
            assert_eq!(
                (records[0].kind, records[0].body.as_slice()),
                (b'F', &b"first-jpeg"[..])
            );
            assert_eq!(
                (records[1].kind, records[1].body.as_slice()),
                (b'F', &b""[..])
            );
            assert_eq!(end_reason(&records[2]), "closed");
        }

        // A record that has only half arrived yields nothing, and keeps what it has.
        let mut reader = RecordReader::default();
        let whole = wire(b'F', b"abcdef");
        assert!(reader.push(&whole[..7]).is_empty());
        let rest = reader.push(&whole[7..]);
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].body, b"abcdef");
    }

    #[test]
    fn live_registry_forgets_a_session_nobody_watches() {
        let modes = ModeChannels::default();

        // Publishing to a session nobody subscribed to creates nothing.
        modes.publish(7, "human");
        assert!(modes.0.lock().unwrap().is_empty());

        let first = modes.subscribe(7);
        let second = modes.subscribe(7);
        modes.publish(7, "wheel-requested");
        assert_eq!(*first.borrow(), "wheel-requested");
        assert_eq!(*second.borrow(), "wheel-requested");

        // One watcher left: the entry stays.
        drop(first);
        modes.release(7);
        assert_eq!(modes.0.lock().unwrap().len(), 1);

        // The last one gone: the entry goes with it.
        drop(second);
        modes.release(7);
        assert!(
            modes.0.lock().unwrap().is_empty(),
            "a registry that never forgets grows with every session ever watched"
        );
    }
}
