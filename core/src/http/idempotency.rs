//! `Idempotency-Key` for the three routes that start something: `POST /runs`, `POST /jobs` and
//! `POST /assistant/message`.
//!
//! A request that carries the header is remembered, for a while, under `(method, path, key)`. The
//! same key arriving again with the same body gets the ORIGINAL answer back (marked
//! `Idempotency-Replayed: true`) and the handler does not run, so a retry after a dropped
//! connection cannot start a second run, job or turn. A request without the header is untouched.
//!
//! Semantics, all of them deliberate:
//! - Only successful (2xx) answers are remembered. A refusal changed nothing, and the same key
//!   must be able to try again once whatever refused it (the kill switch, a bad project) is fixed.
//! - The same key with a DIFFERENT body is `422`: it is a client bug, not a retry, and answering it
//!   with the first request's result would hide that.
//! - The same key while the first request is still running is `409`; the caller retries shortly
//!   and then gets the replay.
//! - Bounded two ways: entries expire after [`TTL`], and at most [`CAPACITY`] are held (the oldest
//!   goes first). Process memory only — a daemon restart forgets, which is the right scope for a
//!   retry window of minutes.
//! - A body larger than [`MAX_BODY`] or an answer larger than [`MAX_CACHED_RESPONSE`] is not
//!   remembered (the latter still reaches the caller; its key just stays retryable).

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const HEADER: &str = "idempotency-key";
const REPLAYED: &str = "idempotency-replayed";
const MAX_KEY_LEN: usize = 128;
/// How long a key is remembered.
pub(crate) const TTL: Duration = Duration::from_secs(10 * 60);
/// How many keys are held at once.
pub(crate) const CAPACITY: usize = 512;
const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_CACHED_RESPONSE: usize = 1024 * 1024;

#[derive(Clone)]
struct Cached {
    status: StatusCode,
    content_type: Option<HeaderValue>,
    body: Bytes,
}

struct Entry {
    fingerprint: u64,
    created: Instant,
    /// `None` while the first request is still running.
    done: Option<Cached>,
}

enum Begin {
    Fresh,
    Replay(Cached),
    InFlight,
    Mismatch,
}

pub(crate) struct Store {
    ttl: Duration,
    capacity: usize,
    entries: Mutex<HashMap<String, Entry>>,
}

impl Store {
    pub(crate) fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity: capacity.max(1),
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn begin(&self, key: &str, fingerprint: u64, now: Instant) -> Begin {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, e| now.saturating_duration_since(e.created) < self.ttl);
        if let Some(entry) = entries.get(key) {
            if entry.fingerprint != fingerprint {
                return Begin::Mismatch;
            }
            return match &entry.done {
                Some(cached) => Begin::Replay(cached.clone()),
                None => Begin::InFlight,
            };
        }
        while entries.len() >= self.capacity {
            let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, e)| e.created)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            entries.remove(&oldest);
        }
        entries.insert(
            key.to_owned(),
            Entry {
                fingerprint,
                created: now,
                done: None,
            },
        );
        Begin::Fresh
    }

    fn finish(&self, key: &str, cached: Cached) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = entries.get_mut(key) {
            entry.done = Some(cached);
        }
    }

    fn abandon(&self, key: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.get(key).is_some_and(|e| e.done.is_none()) {
            entries.remove(key);
        }
    }
}

fn global() -> &'static Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE.get_or_init(|| Store::new(TTL, CAPACITY))
}

/// The middleware the three routes are wrapped in.
pub(super) async fn guard(req: Request, next: Next) -> Response {
    run(global(), req, next).await
}

fn fingerprint(body: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    hasher.finish()
}

fn refuse(status: StatusCode, text: &str) -> Response {
    (status, text.to_owned()).into_response()
}

async fn run(store: &Store, req: Request, next: Next) -> Response {
    let Some(raw) = req.headers().get(HEADER) else {
        return next.run(req).await;
    };
    let key = match raw.to_str() {
        Ok(k) if !k.is_empty() && k.len() <= MAX_KEY_LEN && k.is_ascii() => k.to_owned(),
        _ => {
            return refuse(
                StatusCode::BAD_REQUEST,
                "Idempotency-Key must be 1-128 ASCII characters",
            );
        }
    };
    let slot = format!("{} {}\0{key}", req.method(), req.uri().path());

    let (parts, body) = req.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_BODY).await else {
        return refuse(StatusCode::PAYLOAD_TOO_LARGE, "the request body is too large");
    };

    match store.begin(&slot, fingerprint(&bytes), Instant::now()) {
        Begin::Mismatch => refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "this Idempotency-Key was already used with a different request body",
        ),
        Begin::InFlight => refuse(
            StatusCode::CONFLICT,
            "a request with this Idempotency-Key is still being processed",
        ),
        Begin::Replay(cached) => replay(cached),
        Begin::Fresh => {
            let response = next
                .run(Request::from_parts(parts, Body::from(bytes)))
                .await;
            if !response.status().is_success() {
                store.abandon(&slot);
                return response;
            }
            let (head, body) = response.into_parts();
            match to_bytes(body, MAX_CACHED_RESPONSE).await {
                Ok(body) => {
                    store.finish(
                        &slot,
                        Cached {
                            status: head.status,
                            content_type: head.headers.get(header::CONTENT_TYPE).cloned(),
                            body: body.clone(),
                        },
                    );
                    Response::from_parts(head, Body::from(body))
                }
                Err(_) => {
                    // Too large to keep, and the body is gone: the key stays retryable.
                    store.abandon(&slot);
                    refuse(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "the answer was too large to record under an Idempotency-Key",
                    )
                }
            }
        }
    }
}

fn replay(cached: Cached) -> Response {
    let mut response = Response::new(Body::from(cached.body));
    *response.status_mut() = cached.status;
    if let Some(ct) = cached.content_type {
        response.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    response
        .headers_mut()
        .insert(REPLAYED, HeaderValue::from_static("true"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::post;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    fn app(store: Arc<Store>, hits: Arc<AtomicUsize>, fail: bool) -> Router {
        Router::new()
            .route(
                "/start",
                post(move || {
                    let hits = hits.clone();
                    async move {
                        let n = hits.fetch_add(1, Ordering::SeqCst) + 1;
                        if fail {
                            (StatusCode::CONFLICT, "refused".to_owned())
                        } else {
                            (StatusCode::OK, format!("{{\"id\":{n}}}"))
                        }
                    }
                }),
            )
            .layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let store = store.clone();
                    async move { run(&store, req, next).await }
                },
            ))
    }

    async fn send(app: &Router, key: Option<&str>, body: &str) -> (StatusCode, String, bool) {
        let mut builder = axum::http::Request::builder().method("POST").uri("/start");
        if let Some(key) = key {
            builder = builder.header("Idempotency-Key", key);
        }
        let res = app
            .clone()
            .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let replayed = res.headers().contains_key(REPLAYED);
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap(), replayed)
    }

    fn fixture(fail: bool) -> (Router, Arc<AtomicUsize>, Arc<Store>) {
        let store = Arc::new(Store::new(TTL, CAPACITY));
        let hits = Arc::new(AtomicUsize::new(0));
        (app(store.clone(), hits.clone(), fail), hits, store)
    }

    #[tokio::test]
    async fn same_key_replays_the_original_answer_without_running_the_handler() {
        let (app, hits, _) = fixture(false);
        let first = send(&app, Some("k1"), "{}").await;
        let second = send(&app, Some("k1"), "{}").await;
        assert_eq!(first, (StatusCode::OK, "{\"id\":1}".to_owned(), false));
        assert_eq!(second, (StatusCode::OK, "{\"id\":1}".to_owned(), true));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn requests_without_a_key_or_with_other_keys_each_run() {
        let (app, hits, _) = fixture(false);
        send(&app, None, "{}").await;
        send(&app, None, "{}").await;
        send(&app, Some("a"), "{}").await;
        send(&app, Some("b"), "{}").await;
        assert_eq!(hits.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn same_key_with_a_different_body_is_unprocessable() {
        let (app, hits, _) = fixture(false);
        send(&app, Some("k"), "{\"a\":1}").await;
        let (status, _, replayed) = send(&app, Some("k"), "{\"a\":2}").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!replayed);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_refusal_is_not_remembered_so_the_key_can_try_again() {
        let (app, hits, _) = fixture(true);
        assert_eq!(send(&app, Some("k"), "{}").await.0, StatusCode::CONFLICT);
        assert_eq!(send(&app, Some("k"), "{}").await.0, StatusCode::CONFLICT);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_malformed_key_is_a_bad_request() {
        let (app, hits, _) = fixture(false);
        let long = "x".repeat(MAX_KEY_LEN + 1);
        assert_eq!(
            send(&app, Some(&long), "{}").await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(send(&app, Some(""), "{}").await.0, StatusCode::BAD_REQUEST);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_key_in_flight_is_a_conflict_and_the_replay_follows_finish() {
        let store = Store::new(TTL, CAPACITY);
        let now = Instant::now();
        assert!(matches!(store.begin("s", 1, now), Begin::Fresh));
        assert!(matches!(store.begin("s", 1, now), Begin::InFlight));
        store.finish(
            "s",
            Cached {
                status: StatusCode::OK,
                content_type: None,
                body: Bytes::from_static(b"x"),
            },
        );
        assert!(matches!(store.begin("s", 1, now), Begin::Replay(_)));
    }

    #[test]
    fn entries_expire_after_the_ttl() {
        let store = Store::new(Duration::from_secs(5), CAPACITY);
        let now = Instant::now();
        assert!(matches!(store.begin("s", 1, now), Begin::Fresh));
        let later = now + Duration::from_secs(6);
        assert!(matches!(store.begin("s", 1, later), Begin::Fresh));
    }

    #[test]
    fn the_store_is_bounded_and_drops_the_oldest_first() {
        let store = Store::new(TTL, 2);
        let t0 = Instant::now();
        store.begin("a", 1, t0);
        store.begin("b", 1, t0 + Duration::from_secs(1));
        store.begin("c", 1, t0 + Duration::from_secs(2));
        let entries = store.entries.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert!(!entries.contains_key("a"));
    }
}
