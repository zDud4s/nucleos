//! The daemon side of the shell's embedded browser (`/browser/embedded/*`, contract section B).
//!
//! The shell hosts the agent's webviews; the daemon only brokers. A project becomes usable once the
//! shell has `prepare`d it (the sidecar's fence proxy), built its `agent` environment, and
//! `register`ed the CDP address. From then on the shell's one-second `GET /requests` poll is the
//! heartbeat of EVERY registration: thirty seconds without one and the registration is forgotten
//! and the sidecar told to close the proxy, so a shell that died cannot leave a fence proxy and a
//! CDP connection behind.
//!
//! `browser_open` on a project with a live registration queues an `open` request. The shell creates
//! a webview at `about:blank#<nonce>` and acks with the nonce; the open call then asks the sidecar
//! to attach to that tab, and only after that does the ack return the session id. If the shell never
//! acks within ten seconds the request expires and the caller falls back to the headless path.
//!
//! A `focus` request asks the shell to bring a session's webview forward; it needs no ack, so it is
//! listed once and dropped. The registry also remembers which embedded sessions a person is driving
//! (the owner flag, `set_human`), which `embedded_owner` flips together with the sidecar's detach.
//!
//! Nothing here touches the database: the registry is memory only, like the seat state.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::browser_client::{BrowserClient, BrowserError, Placement, Session};
use crate::state::AppState;

/// How long a registration lives without a `GET /requests` poll.
pub const REGISTRATION_TTL: Duration = Duration::from_secs(30);
/// How long an `open` waits for the shell's ack, and the bound on the sidecar's attach after it.
pub const OPEN_WAIT: Duration = Duration::from_secs(10);
/// How long the shell's ack waits for the attach to settle. Strictly longer than the attach bound
/// ([`OPEN_WAIT`]), so a slow attach is reported by the open call itself and the ack never gives up
/// first, which would leave a sidecar session nobody knows about.
pub const ACK_RESULT_WAIT: Duration = Duration::from_secs(OPEN_WAIT.as_secs() * 3 / 2);
/// How long a `close` request waits to be picked up and acked before it is dropped.
const CLOSE_TTL: Duration = Duration::from_secs(30);
/// How often the watchdog looks for registrations whose shell stopped polling.
const SWEEP_EVERY: Duration = Duration::from_secs(5);
/// Body ceiling for these routes: they carry a project id, an address or a nonce, never content.
pub const BODY_LIMIT: usize = 16 * 1024;

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Open,
    Close,
    /// Bring the session's webview to the front. Carries `session_id`, no url, and needs no ack.
    Focus,
}

/// One entry of `GET /requests`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRequest {
    pub request_id: String,
    pub kind: Kind,
    /// The scope's registry key (`browser::embedded_key`). On the wire it is the project id, or an
    /// explicit `null` for the no-project scope: the shell never sees the internal key.
    #[serde(with = "wire_project")]
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// `PendingRequest.project` on the wire: the no-project key travels as `null`, any other key as the
/// project id it is (contract 3.1, `GET /browser/embedded/requests`).
mod wire_project {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::browser::NO_PROJECT_KEY;

    pub fn serialize<T, S>(key: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        T: AsRef<str>,
        S: Serializer,
    {
        match key.as_ref() {
            NO_PROJECT_KEY => serializer.serialize_none(),
            project => serializer.serialize_some(project),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<String, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Option::<String>::deserialize(deserializer)?
            .unwrap_or_else(|| NO_PROJECT_KEY.to_string()))
    }
}

/// What the ack hands the waiting `open`: the nonce, and where to report the attach result.
struct Acked {
    nonce: String,
    done: oneshot::Sender<Result<String, String>>,
}

struct Request {
    wire: PendingRequest,
    delivered: bool,
    expires: Instant,
    /// Only on `open`: wakes the call that queued it.
    waiter: Option<oneshot::Sender<Acked>>,
}

struct Registration {
    cdp_http: String,
    last_poll: Instant,
}

#[derive(Default)]
struct Inner {
    registrations: HashMap<String, Registration>,
    requests: Vec<Request>,
    /// Embedded session id -> owning project.
    sessions: HashMap<String, String>,
    /// Sidecar session ids a person is driving (the agent is refused from them).
    human: HashSet<String>,
    watching: bool,
}

impl Inner {
    /// Forgets the embedded sessions (and owner flags) of a project whose shell environment is gone:
    /// their tabs died with it, and a stale entry would answer for a session nobody can reach.
    fn forget_sessions_of(&mut self, project: &str) {
        let gone: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, owner)| owner.as_str() == project)
            .map(|(id, _)| id.clone())
            .collect();
        for id in gone {
            self.sessions.remove(&id);
            self.human.remove(&id);
        }
    }
}

struct Shared {
    state: Mutex<Inner>,
    clock: Clock,
    registration_ttl: Duration,
    open_wait: Duration,
    sweep_every: Duration,
}

/// What an ack came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckOutcome {
    /// Unknown, expired or already settled request.
    Gone,
    /// An `open` that the sidecar attached: the session id.
    Opened(String),
    /// A `close` that the shell carried out: the session it closed.
    Closed(String),
    /// The sidecar could not attach (or took too long).
    Failed(String),
}

/// How an open ended.
pub(super) enum OpenEnd {
    Opened(Session),
    /// No session: headless fallback.
    Nothing,
    /// The sidecar made this session but the ack gave up before it could be handed over.
    Discarded(String),
}

/// The shell's registrations, its queued requests and the embedded sessions they produced.
#[derive(Clone)]
pub struct Registry {
    shared: Arc<Shared>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::with_timing(
            Arc::new(Instant::now),
            REGISTRATION_TTL,
            OPEN_WAIT,
            SWEEP_EVERY,
        )
    }

    /// A registry with an injected clock and shortened waits, so expiry is testable without sleeping.
    pub fn with_timing(
        clock: Clock,
        registration_ttl: Duration,
        open_wait: Duration,
        sweep_every: Duration,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::default(),
                clock,
                registration_ttl,
                open_wait,
                sweep_every,
            }),
        }
    }

    /// How long an ack waits for the attach: strictly more than the attach bound itself.
    fn ack_result_wait(&self) -> Duration {
        self.shared.open_wait + self.shared.open_wait / 2
    }

    fn now(&self) -> Instant {
        (self.shared.clock)()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        locked(&self.shared.state)
    }

    /// A live registration exists for the project (polled within the last 30 s).
    pub fn has_live(&self, project: &str) -> bool {
        let now = self.now();
        self.lock().registrations.get(project).is_some_and(|reg| {
            now.saturating_duration_since(reg.last_poll) < self.shared.registration_ttl
        })
    }

    /// Records (or refreshes) a project's registration. Counts as a heartbeat for it.
    pub fn register(&self, project: &str, cdp_http: &str) {
        let now = self.now();
        let mut inner = self.lock();
        // A different CDP address means the shell rebuilt its environment: the old tabs are gone.
        if inner
            .registrations
            .get(project)
            .is_some_and(|reg| reg.cdp_http != cdp_http)
        {
            inner.forget_sessions_of(project);
        }
        inner.registrations.insert(
            project.to_string(),
            Registration {
                cdp_http: cdp_http.to_string(),
                last_poll: now,
            },
        );
    }

    /// `GET /requests`: refreshes every registration's heartbeat and hands out each pending request
    /// once. A request is listed a single time, because a second listing would make the shell build a
    /// second webview for the same open; the ack (or the expiry) settles it.
    pub fn poll(&self) -> Vec<PendingRequest> {
        let now = self.now();
        let mut inner = self.lock();
        for registration in inner.registrations.values_mut() {
            registration.last_poll = now;
        }
        inner.requests.retain(|request| request.expires > now);
        let mut out = Vec::new();
        for request in inner.requests.iter_mut().filter(|r| !r.delivered) {
            request.delivered = true;
            out.push(request.wire.clone());
        }
        // A focus is never acked, so listing it settles it.
        inner
            .requests
            .retain(|request| !(request.wire.kind == Kind::Focus && request.delivered));
        out
    }

    /// Drops registrations whose shell stopped polling, with the requests queued for them, and
    /// returns their projects so the caller can tell the sidecar to forget them.
    pub fn expire(&self) -> Vec<String> {
        let now = self.now();
        let ttl = self.shared.registration_ttl;
        let mut inner = self.lock();
        let dead: Vec<String> = inner
            .registrations
            .iter()
            .filter(|(_, reg)| now.saturating_duration_since(reg.last_poll) >= ttl)
            .map(|(project, _)| project.clone())
            .collect();
        for project in &dead {
            inner.registrations.remove(project);
            inner.forget_sessions_of(project);
            // Dropping an open's waiter wakes it with an error, which sends it to the headless path.
            inner
                .requests
                .retain(|request| &request.wire.project != project);
        }
        dead
    }

    /// Starts the watchdog that forgets silent registrations, unless one is already running. It
    /// stops by itself when no registration is left, and the next `register` starts it again.
    pub fn start_watchdog(&self, client: BrowserClient) {
        {
            let mut inner = self.lock();
            if inner.watching || inner.registrations.is_empty() {
                return;
            }
            inner.watching = true;
        }
        let registry = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(registry.shared.sweep_every).await;
                for project in registry.expire() {
                    tracing::info!(%project, "embedded browser registration lapsed, forgetting it");
                    if let Err(error) = client.embedded_forget(&project).await {
                        tracing::warn!(%project, %error, "the sidecar did not forget an embedded project");
                    }
                }
                // Decided under the same lock that `start_watchdog` takes, so a registration landing
                // now either keeps this loop alive or finds the flag cleared and starts a new one.
                let mut inner = registry.lock();
                if inner.registrations.is_empty() {
                    inner.watching = false;
                    break;
                }
            }
        });
    }

    /// Queues an `open`, waits for the shell's ack and the sidecar's attach, and returns the
    /// sidecar's session. None means "use the headless path": no live registration, no ack in time,
    /// or no attach.
    ///
    /// `placement` is the project's real one (its site grants), the same value `prepare` was given:
    /// the sidecar answers 409 to an attach whose placement differs from the one its proxy runs.
    pub async fn open(
        &self,
        client: &BrowserClient,
        project: &str,
        url: &str,
        placement: &Placement,
    ) -> Option<Session> {
        // The shell prepared the environment with the site list of that moment, often an empty
        // one. Re-preparing with today's list swaps it into the running proxy (same address), so
        // the attach below is not refused as a placement mismatch. A refusal here (a live session
        // under another list) takes the headless path.
        if let Err(error) = client.embedded_prepare(project, placement).await {
            tracing::info!(project, %error, "embedded open: re-prepare refused, using headless");
            return None;
        }
        let project_name = project.to_string();
        let ended = self
            .open_ended(project, url, |cdp_http, nonce, url| async move {
                client
                    .embedded_attach(&project_name, &cdp_http, &nonce, &url, placement)
                    .await
                    .map_err(|error| error.to_string())
            })
            .await;
        match ended {
            OpenEnd::Opened(session) => Some(session),
            OpenEnd::Nothing => None,
            // The sidecar holds a slot for a session the shell was never told about.
            OpenEnd::Discarded(id) => {
                super::embedded_close::release_sidecar_slot(client, &id).await;
                None
            }
        }
    }

    /// [`Registry::open`] with the sidecar call injected.
    pub async fn open_with<F, Fut>(&self, project: &str, url: &str, attach: F) -> Option<Session>
    where
        F: FnOnce(String, String, String) -> Fut,
        Fut: Future<Output = Result<Session, String>>,
    {
        match self.open_ended(project, url, attach).await {
            OpenEnd::Opened(session) => Some(session),
            OpenEnd::Nothing | OpenEnd::Discarded(_) => None,
        }
    }

    /// [`Registry::open_with`] that also says when the attach produced a session nobody received.
    pub(super) async fn open_ended<F, Fut>(&self, project: &str, url: &str, attach: F) -> OpenEnd
    where
        F: FnOnce(String, String, String) -> Fut,
        Fut: Future<Output = Result<Session, String>>,
    {
        if !self.has_live(project) {
            return OpenEnd::Nothing;
        }
        let (waiter, acked) = oneshot::channel();
        let request_id = new_id();
        let cdp_http = {
            let now = self.now();
            let mut inner = self.lock();
            let Some(registration) = inner.registrations.get(project) else {
                return OpenEnd::Nothing;
            };
            let cdp_http = registration.cdp_http.clone();
            inner.requests.push(Request {
                wire: PendingRequest {
                    request_id: request_id.clone(),
                    kind: Kind::Open,
                    project: project.to_string(),
                    url: Some(url.to_string()),
                    session_id: None,
                },
                delivered: false,
                expires: now + self.shared.open_wait,
                waiter: Some(waiter),
            });
            cdp_http
        };
        let acked = match tokio::time::timeout(self.shared.open_wait, acked).await {
            Ok(Ok(acked)) => acked,
            _ => {
                // Expired (or the registration lapsed): withdraw it so a late ack finds it gone.
                self.lock()
                    .requests
                    .retain(|request| request.wire.request_id != request_id);
                return OpenEnd::Nothing;
            }
        };
        let attached = tokio::time::timeout(
            self.shared.open_wait,
            attach(cdp_http, acked.nonce, url.to_string()),
        )
        .await
        .unwrap_or_else(|_| Err("the sidecar did not attach in time".to_string()));
        match attached {
            Ok(session) => {
                self.lock()
                    .sessions
                    .insert(session.id.clone(), project.to_string());
                if acked.done.send(Ok(session.id.clone())).is_err() {
                    // The ack stopped listening: the shell never learns this id, so no tab can be
                    // closed by it. Forget the session here and let the caller free the sidecar.
                    self.session_closed(&session.id);
                    return OpenEnd::Discarded(session.id);
                }
                OpenEnd::Opened(session)
            }
            Err(error) => {
                // The shell is answered `Failed`; a close is keyed by a session id and none exists
                // for this tab, so the shell owns it. A session the sidecar made past its bound is
                // unknown to us and cannot be released.
                let _ = acked.done.send(Err(error));
                OpenEnd::Nothing
            }
        }
    }

    /// `POST /requests/{id}/ack`. For an `open` it blocks until the sidecar has attached.
    pub async fn ack(&self, request_id: &str, nonce: &str) -> AckOutcome {
        let now = self.now();
        let request =
            {
                let mut inner = self.lock();
                let Some(index) = inner.requests.iter().position(|request| {
                    request.wire.request_id == request_id && request.expires > now
                }) else {
                    return AckOutcome::Gone;
                };
                inner.requests.remove(index)
            };
        match request.wire.kind {
            Kind::Close => {
                let session_id = request.wire.session_id.unwrap_or_default();
                self.session_closed(&session_id);
                AckOutcome::Closed(session_id)
            }
            // Focus is dropped when listed, so it can only reach an ack by a stale id.
            Kind::Focus => AckOutcome::Gone,
            Kind::Open => {
                let Some(waiter) = request.waiter else {
                    return AckOutcome::Gone;
                };
                let (done, result) = oneshot::channel();
                let acked = Acked {
                    nonce: nonce.to_string(),
                    done,
                };
                // The open call gave up already: nothing is listening, so nothing was attached.
                if waiter.send(acked).is_err() {
                    return AckOutcome::Gone;
                }
                match tokio::time::timeout(self.ack_result_wait(), result).await {
                    Ok(Ok(Ok(session_id))) => AckOutcome::Opened(session_id),
                    Ok(Ok(Err(error))) => AckOutcome::Failed(error),
                    _ => AckOutcome::Failed("the attach did not finish".to_string()),
                }
            }
        }
    }

    pub fn is_embedded(&self, session_id: &str) -> bool {
        self.lock().sessions.contains_key(session_id)
    }

    /// Queues a `close` for the project that owns the session. False when the session is not embedded.
    pub fn queue_close(&self, session_id: &str) -> bool {
        let now = self.now();
        let mut inner = self.lock();
        let Some(project) = inner.sessions.get(session_id).cloned() else {
            return false;
        };
        let queued = inner.requests.iter().any(|request| {
            request.wire.kind == Kind::Close
                && request.wire.session_id.as_deref() == Some(session_id)
        });
        if !queued {
            inner.requests.push(Request {
                wire: PendingRequest {
                    request_id: new_id(),
                    kind: Kind::Close,
                    project,
                    url: None,
                    session_id: Some(session_id.to_string()),
                },
                delivered: false,
                expires: now + CLOSE_TTL,
                waiter: None,
            });
        }
        true
    }

    /// Queues a `focus` for the project that owns the session. False when the session is not
    /// embedded; an undelivered focus for the same session is not queued twice.
    pub fn queue_focus(&self, session_id: &str) -> bool {
        let now = self.now();
        let mut inner = self.lock();
        let Some(project) = inner.sessions.get(session_id).cloned() else {
            return false;
        };
        let queued = inner.requests.iter().any(|request| {
            request.wire.kind == Kind::Focus
                && !request.delivered
                && request.wire.session_id.as_deref() == Some(session_id)
        });
        if !queued {
            inner.requests.push(Request {
                wire: PendingRequest {
                    request_id: new_id(),
                    kind: Kind::Focus,
                    project,
                    url: None,
                    session_id: Some(session_id.to_string()),
                },
                delivered: false,
                expires: now + CLOSE_TTL,
                waiter: None,
            });
        }
        true
    }

    /// Marks (or unmarks) an embedded session as driven by a person. False when the session is not
    /// embedded.
    pub fn set_human(&self, session_id: &str, human: bool) -> bool {
        let mut inner = self.lock();
        if !inner.sessions.contains_key(session_id) {
            return false;
        }
        if human {
            inner.human.insert(session_id.to_string());
        } else {
            inner.human.remove(session_id);
        }
        true
    }

    /// Test seam: records an embedded session without going through an open.
    #[cfg(test)]
    pub(super) fn adopt_for_test(&self, session_id: &str, project: &str) {
        self.lock()
            .sessions
            .insert(session_id.to_string(), project.to_string());
    }

    pub fn is_human(&self, session_id: &str) -> bool {
        self.lock().human.contains(session_id)
    }

    /// Forgets the session: the tab is gone, whoever closed it.
    pub fn session_closed(&self, session_id: &str) {
        let mut inner = self.lock();
        inner.sessions.remove(session_id);
        inner.human.remove(session_id);
    }
}

fn new_id() -> String {
    let bytes = rand::rng().random::<[u8; 8]>();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("er-{hex}")
}

// ---------------------------------------------------------------------------------------------
// Handlers. Control token only (no scope table lists them); mounted in `http/relay.rs`.
// ---------------------------------------------------------------------------------------------

/// `project` absent or `null` is the no-project scope; any other string is a project id.
#[derive(Debug, Deserialize)]
pub struct PrepareBody {
    #[serde(default)]
    pub project: Option<String>,
}

/// `project` absent or `null` is the no-project scope; any other string is a project id.
#[derive(Debug, Deserialize)]
pub struct RegisterBody {
    #[serde(default)]
    pub project: Option<String>,
    pub cdp_http: String,
}

/// The registry key of a body's `project`: the no-project key for a missing one, the id itself
/// otherwise. An explicit empty string is not the no-project scope, it is a client error, and so is
/// the internal no-project key named explicitly: the no-project scope is reached only by omitting it.
fn scope_key(project: Option<&str>) -> Result<&str, Response> {
    match project {
        None => Ok(crate::browser::embedded_key(crate::browser::NO_PROJECT)),
        Some(project) if project.trim().is_empty() => Err(bad_request("project is empty")),
        Some(crate::browser::NO_PROJECT_KEY) => {
            Err(bad_request("project is the reserved no-project key"))
        }
        Some(project) => Ok(project),
    }
}

#[derive(Debug, Deserialize)]
pub struct AckBody {
    pub nonce: String,
}

pub(super) fn pillar_off() -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "refusal": "pillar_off",
            "detail": "the browser pillar is off",
        })),
    )
        .into_response()
}

pub(super) fn bad_request(detail: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": detail })),
    )
        .into_response()
}

pub(super) fn sidecar_error(error: BrowserError) -> Response {
    let status = match &error {
        BrowserError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
        BrowserError::Unreachable(_) | BrowserError::FenceDown(_) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        BrowserError::BadRequest(_) => StatusCode::BAD_REQUEST,
        BrowserError::NoSuchSession(_) => StatusCode::NOT_FOUND,
        BrowserError::Failed(_) => StatusCode::BAD_GATEWAY,
    };
    (
        status,
        Json(serde_json::json!({ "error": error.to_string() })),
    )
        .into_response()
}

/// The shell's CDP address is dialled by the sidecar, so it must name this machine.
fn is_loopback_http(address: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|prefix| {
            address.strip_prefix(prefix).is_some_and(|port| {
                !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
}

/// The nonce becomes part of a URL fragment and a CDP match, so it stays a plain token.
fn is_plain_token(nonce: &str) -> bool {
    !nonce.is_empty()
        && nonce.len() <= 64
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// `POST /browser/embedded/prepare`.
pub async fn post_prepare(
    State(state): State<AppState>,
    Json(body): Json<PrepareBody>,
) -> Response {
    if !state.browser.enabled {
        return pillar_off();
    }
    let key = match scope_key(body.project.as_deref()) {
        Ok(key) => key,
        Err(refused) => return refused,
    };
    // The same placement `browser_open` attaches with, so the sidecar's proxy and the attach agree:
    // the project's site grants, or the no-project scope's one stable throwaway (plan D5).
    let placement = if body.project.is_none() {
        crate::browser::no_project_placement()
    } else {
        match crate::browser::project_placement(&state.pool, key).await {
            Ok(placement) => placement,
            Err(error) => return sidecar_error(error),
        }
    };
    match state.browser.client.embedded_prepare(key, &placement).await
    {
        Ok(prepared) => Json(prepared).into_response(),
        Err(error) => sidecar_error(error),
    }
}

/// `POST /browser/embedded/register`.
pub async fn post_register(
    State(state): State<AppState>,
    Json(body): Json<RegisterBody>,
) -> Response {
    if !state.browser.enabled {
        return pillar_off();
    }
    let key = match scope_key(body.project.as_deref()) {
        Ok(key) => key,
        Err(refused) => return refused,
    };
    if !is_loopback_http(&body.cdp_http) {
        return bad_request("cdp_http must be a loopback http address");
    }
    state.browser.embedded.register(key, &body.cdp_http);
    state
        .browser
        .embedded
        .start_watchdog(state.browser.client.clone());
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /browser/embedded/requests`. Also the heartbeat of every registration.
pub async fn get_requests(State(state): State<AppState>) -> Response {
    if !state.browser.enabled {
        return pillar_off();
    }
    Json(state.browser.embedded.poll()).into_response()
}

/// `POST /browser/embedded/requests/{id}/ack`.
pub async fn post_ack(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
    Json(body): Json<AckBody>,
) -> Response {
    if !state.browser.enabled {
        return pillar_off();
    }
    if !is_plain_token(&body.nonce) {
        return bad_request("nonce must be 1-64 letters, digits, '-' or '_'");
    }
    match state.browser.embedded.ack(&request_id, &body.nonce).await {
        AckOutcome::Opened(session_id) | AckOutcome::Closed(session_id) => {
            Json(serde_json::json!({ "session_id": session_id })).into_response()
        }
        AckOutcome::Gone => (
            StatusCode::GONE,
            Json(serde_json::json!({ "error": "request expired" })),
        )
            .into_response(),
        AckOutcome::Failed(error) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

/// `POST /browser/embedded/sessions/{id}/closed`. The body is `{}` and is not read.
pub async fn post_closed(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Response {
    if !state.browser.enabled {
        return pillar_off();
    }
    super::embedded_close::tab_closed(&state.pool, &state.browser, &session_id).await;
    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry on a clock the test moves by hand.
    fn registry(open_wait: Duration) -> (Registry, Arc<Mutex<Instant>>) {
        let now = Arc::new(Mutex::new(Instant::now()));
        let handle = now.clone();
        let registry = Registry::with_timing(
            Arc::new(move || *locked(&handle)),
            Duration::from_secs(30),
            open_wait,
            Duration::from_secs(5),
        );
        (registry, now)
    }

    /// A sidecar session as `attach` would return it.
    fn session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            mode: "agent".to_string(),
            requested_url: "https://x.test/a".to_string(),
            final_url: "https://x.test/landed".to_string(),
            title: "Landed".to_string(),
            refusal: None,
            still_loading: false,
            status: 200,
        }
    }

    fn advance(clock: &Arc<Mutex<Instant>>, secs: u64) {
        *locked(clock) += Duration::from_secs(secs);
    }

    #[test]
    fn a_registration_lapses_after_thirty_silent_seconds() {
        let (registry, clock) = registry(OPEN_WAIT);
        assert!(!registry.has_live("p"));
        registry.register("p", "http://127.0.0.1:9222");
        advance(&clock, 29);
        assert!(registry.has_live("p"));
        assert!(registry.expire().is_empty());
        advance(&clock, 2);
        assert!(!registry.has_live("p"));
        assert_eq!(registry.expire(), vec!["p".to_string()]);
        assert!(registry.expire().is_empty(), "forgotten once");
    }

    #[test]
    fn a_poll_is_the_heartbeat_of_every_registration() {
        let (registry, clock) = registry(OPEN_WAIT);
        registry.register("a", "http://127.0.0.1:1");
        registry.register("b", "http://127.0.0.1:2");
        for _ in 0..4 {
            advance(&clock, 20);
            registry.poll();
        }
        assert!(registry.has_live("a") && registry.has_live("b"));
        assert!(registry.expire().is_empty());
    }

    #[test]
    fn close_is_queued_once_listed_once_and_only_for_embedded_sessions() {
        let (registry, clock) = registry(OPEN_WAIT);
        assert!(!registry.queue_close("nope"));
        registry.register("p", "http://127.0.0.1:1");
        registry.lock().sessions.insert("s1".into(), "p".into());
        assert!(registry.is_embedded("s1"));
        assert!(registry.queue_close("s1"));
        assert!(
            registry.queue_close("s1"),
            "a second ask queues nothing new"
        );
        let listed = registry.poll();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, Kind::Close);
        assert_eq!(listed[0].project, "p");
        assert_eq!(listed[0].session_id.as_deref(), Some("s1"));
        assert!(registry.poll().is_empty(), "listed once");
        registry.session_closed("s1");
        assert!(!registry.is_embedded("s1"));
        assert!(!registry.queue_close("s1"));
        // An unclaimed close is dropped after its ttl.
        registry.lock().sessions.insert("s2".into(), "p".into());
        registry.queue_close("s2");
        advance(&clock, 31);
        assert!(registry.poll().is_empty());
    }

    #[test]
    fn focus_is_queued_once_listed_once_and_only_for_embedded_sessions() {
        let (registry, _clock) = registry(OPEN_WAIT);
        assert!(!registry.queue_focus("nope"));
        registry.register("p", "http://127.0.0.1:1");
        registry.lock().sessions.insert("s1".into(), "p".into());
        assert!(registry.queue_focus("s1"));
        assert!(
            registry.queue_focus("s1"),
            "a second ask queues nothing new"
        );
        let listed = registry.poll();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].kind, Kind::Focus);
        assert_eq!(listed[0].project, "p");
        assert_eq!(listed[0].session_id.as_deref(), Some("s1"));
        assert_eq!(listed[0].url, None);
        assert!(registry.poll().is_empty(), "listed once");
        assert!(registry.lock().requests.is_empty(), "gone once listed");
    }

    /// The no-project scope is keyed by `embedded_key(NO_PROJECT)` inside the daemon, but the shell
    /// is told `null`: the listing never leaks the internal key.
    #[tokio::test]
    async fn a_request_queued_for_the_no_project_scope_is_listed_with_a_null_project() {
        use crate::browser::{NO_PROJECT, embedded_key};

        let (registry, _clock) = registry(Duration::from_secs(5));
        registry.register(embedded_key(NO_PROJECT), "http://127.0.0.1:7");
        let opener = registry.clone();
        let open = tokio::spawn(async move {
            opener
                .open_with(
                    embedded_key(NO_PROJECT),
                    "https://x.test/a",
                    |_, _, _| async { Ok(session("sess-1")) },
                )
                .await
        });
        let request = loop {
            if let Some(request) = registry.poll().pop() {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        let listed = serde_json::to_value(&request).unwrap();
        assert_eq!(listed["kind"], "open");
        assert!(
            listed["project"].is_null(),
            "the wire carries null, got {listed}"
        );
        registry.ack(&request.request_id, "n0nce").await;
        open.await.unwrap();
    }

    /// The internal no-project key named explicitly is refused like an empty project: the
    /// no-project scope is reached only by omitting `project`.
    #[test]
    fn an_explicit_no_project_key_is_refused_like_an_empty_project() {
        let refused = scope_key(Some(crate::browser::NO_PROJECT_KEY)).unwrap_err();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            scope_key(Some("")).unwrap_err().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(scope_key(Some("acme")).ok(), Some("acme"));
    }

    #[test]
    fn the_owner_flag_round_trips_and_dies_with_the_session() {
        let (registry, _clock) = registry(OPEN_WAIT);
        assert!(!registry.set_human("nope", true));
        assert!(!registry.is_human("nope"));
        registry.lock().sessions.insert("s1".into(), "p".into());
        assert!(!registry.is_human("s1"));
        assert!(registry.set_human("s1", true));
        assert!(registry.is_human("s1"));
        assert!(registry.set_human("s1", false));
        assert!(!registry.is_human("s1"));
        assert!(registry.set_human("s1", true));
        registry.session_closed("s1");
        assert!(!registry.is_human("s1"));
    }

    #[test]
    fn focus_serializes_as_a_lowercase_kind() {
        assert_eq!(serde_json::to_string(&Kind::Focus).unwrap(), "\"focus\"");
        assert_eq!(
            serde_json::from_str::<Kind>("\"focus\"").unwrap(),
            Kind::Focus
        );
    }

    #[tokio::test]
    async fn open_without_a_live_registration_falls_back() {
        let (registry, _clock) = registry(Duration::from_millis(50));
        let got = registry
            .open_with("p", "https://x.test", |_, _, _| async { Ok(session("s")) })
            .await;
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn open_blocks_until_the_shell_acks_and_the_sidecar_attaches() {
        let (registry, _clock) = registry(Duration::from_secs(5));
        registry.register("p", "http://127.0.0.1:7");
        let opener = registry.clone();
        let open = tokio::spawn(async move {
            opener
                .open_with("p", "https://x.test/a", |cdp, nonce, url| async move {
                    assert_eq!(cdp, "http://127.0.0.1:7");
                    assert_eq!(nonce, "n0nce");
                    assert_eq!(url, "https://x.test/a");
                    Ok(session("sess-1"))
                })
                .await
        });
        let request = loop {
            let mut listed = registry.poll();
            if let Some(request) = listed.pop() {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(request.kind, Kind::Open);
        assert_eq!(request.url.as_deref(), Some("https://x.test/a"));
        let outcome = registry.ack(&request.request_id, "n0nce").await;
        assert_eq!(outcome, AckOutcome::Opened("sess-1".into()));
        assert_eq!(
            open.await.unwrap().map(|s| s.id),
            Some("sess-1".to_string())
        );
        assert!(registry.is_embedded("sess-1"));

        // Closing it round-trips through the same queue.
        assert!(registry.queue_close("sess-1"));
        let close = registry.poll().pop().unwrap();
        assert_eq!(close.kind, Kind::Close);
        let outcome = registry.ack(&close.request_id, "n0nce").await;
        assert_eq!(outcome, AckOutcome::Closed("sess-1".into()));
        assert!(!registry.is_embedded("sess-1"));
        assert_eq!(registry.ack(&close.request_id, "x").await, AckOutcome::Gone);
    }

    #[tokio::test]
    async fn an_unacked_open_expires_and_a_late_ack_is_gone() {
        let (registry, _clock) = registry(Duration::from_millis(60));
        registry.register("p", "http://127.0.0.1:7");
        let opener = registry.clone();
        let open = tokio::spawn(async move {
            opener
                .open_with("p", "https://x.test", |_, _, _| async { Ok(session("s")) })
                .await
        });
        let request = loop {
            if let Some(request) = registry.poll().pop() {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert!(open.await.unwrap().is_none());
        assert_eq!(
            registry.ack(&request.request_id, "n").await,
            AckOutcome::Gone
        );
        assert!(!registry.is_embedded("s"));
    }

    #[tokio::test]
    async fn a_failed_attach_fails_the_ack_and_the_open() {
        let (registry, _clock) = registry(Duration::from_secs(5));
        registry.register("p", "http://127.0.0.1:7");
        let opener = registry.clone();
        let open = tokio::spawn(async move {
            opener
                .open_with("p", "https://x.test", |_, _, _| async {
                    Err::<Session, _>("no such target".to_string())
                })
                .await
        });
        let request = loop {
            if let Some(request) = registry.poll().pop() {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(
            registry.ack(&request.request_id, "n").await,
            AckOutcome::Failed("no such target".into())
        );
        assert!(open.await.unwrap().is_none());
    }

    #[test]
    fn expiring_a_registration_withdraws_its_requests() {
        let (registry, clock) = registry(OPEN_WAIT);
        registry.register("p", "http://127.0.0.1:1");
        registry.lock().sessions.insert("s".into(), "p".into());
        registry.queue_close("s");
        advance(&clock, 31);
        assert_eq!(registry.expire(), vec!["p".to_string()]);
        assert!(registry.lock().requests.is_empty());
    }

    #[test]
    fn only_loopback_addresses_and_plain_nonces_are_accepted() {
        assert!(is_loopback_http("http://127.0.0.1:9222"));
        assert!(is_loopback_http("http://localhost:9222"));
        assert!(!is_loopback_http("http://127.0.0.1"));
        assert!(!is_loopback_http("http://127.0.0.1:9222/json"));
        assert!(!is_loopback_http("http://evil.test:9222"));
        assert!(!is_loopback_http("http://127.0.0.1.evil.test:9222"));
        assert!(is_plain_token("abc-DEF_123"));
        assert!(!is_plain_token(""));
        assert!(!is_plain_token("a#b"));
        assert!(!is_plain_token(&"a".repeat(65)));
    }

    /// The happy path hands the sidecar's whole session back, not just its id, so the row records
    /// what the page really did.
    #[tokio::test]
    async fn an_open_returns_the_sidecar_session_with_its_final_url_and_title() {
        let (registry, _clock) = registry(Duration::from_secs(5));
        registry.register("p", "http://127.0.0.1:7");
        let opener = registry.clone();
        let open = tokio::spawn(async move {
            opener
                .open_with("p", "https://x.test/a", |_, _, _| async {
                    Ok(session("sess-2"))
                })
                .await
        });
        let request = loop {
            if let Some(request) = registry.poll().pop() {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(
            registry.ack(&request.request_id, "n").await,
            AckOutcome::Opened("sess-2".into())
        );
        let got = open.await.unwrap().expect("attached");
        assert_eq!(got.final_url, "https://x.test/landed");
        assert_eq!(got.title, "Landed");
        assert!(registry.is_embedded("sess-2"));
    }

    /// A shell that never acks: the open gives up after the registry's bound (a few milliseconds
    /// here) and leaves nothing behind, so the caller falls back to the headless path.
    #[tokio::test]
    async fn a_shell_that_never_acks_leaves_no_request_or_session_behind() {
        let (registry, _clock) = registry(Duration::from_millis(30));
        registry.register("p", "http://127.0.0.1:7");
        let got = registry
            .open_with("p", "https://x.test", |_, _, _| async {
                Ok(session("never"))
            })
            .await;
        assert!(got.is_none());
        assert!(
            registry.poll().is_empty(),
            "the expired request is withdrawn"
        );
        assert!(!registry.is_embedded("never"));
    }

    #[test]
    fn expiring_a_registration_forgets_its_sessions_and_owner_flags() {
        let (registry, clock) = registry(OPEN_WAIT);
        registry.register("p", "http://127.0.0.1:1");
        registry.register("q", "http://127.0.0.1:2");
        registry.adopt_for_test("sp", "p");
        registry.adopt_for_test("sq", "q");
        assert!(registry.set_human("sp", true));
        advance(&clock, 31);
        registry.register("q", "http://127.0.0.1:2");
        assert_eq!(registry.expire(), vec!["p".to_string()]);
        assert!(!registry.is_embedded("sp") && !registry.is_human("sp"));
        assert!(
            registry.is_embedded("sq"),
            "a live project keeps its sessions"
        );
    }

    #[test]
    fn registering_a_different_cdp_address_forgets_the_old_sessions() {
        let (registry, _clock) = registry(OPEN_WAIT);
        registry.register("p", "http://127.0.0.1:1");
        registry.adopt_for_test("sp", "p");
        registry.adopt_for_test("sq", "q");
        assert!(registry.set_human("sp", true));
        registry.register("p", "http://127.0.0.1:1");
        assert!(
            registry.is_embedded("sp"),
            "the same address is a heartbeat"
        );
        registry.register("p", "http://127.0.0.1:2");
        assert!(!registry.is_embedded("sp") && !registry.is_human("sp"));
        assert!(registry.is_embedded("sq"), "another project is untouched");
    }

    #[test]
    fn the_ack_waits_strictly_longer_than_the_attach_bound() {
        assert!(ACK_RESULT_WAIT > OPEN_WAIT);
        let (registry, _clock) = registry(Duration::from_millis(40));
        assert!(registry.ack_result_wait() > Duration::from_millis(40));
    }

    /// An attach that outlives its bound fails the open and the ack together, in that order: the
    /// ack still hears the attach's own error rather than giving up on it.
    #[tokio::test]
    async fn a_slow_attach_fails_the_ack_with_the_attach_error() {
        let (registry, _clock) = registry(Duration::from_millis(60));
        registry.register("p", "http://127.0.0.1:7");
        let opener = registry.clone();
        let open = tokio::spawn(async move {
            opener
                .open_with("p", "https://x.test", |_, _, _| async {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    Ok(session("slow"))
                })
                .await
        });
        let request = loop {
            if let Some(request) = registry.poll().pop() {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        assert_eq!(
            registry.ack(&request.request_id, "n").await,
            AckOutcome::Failed("the sidecar did not attach in time".into())
        );
        assert!(open.await.unwrap().is_none());
        assert!(!registry.is_embedded("slow"));
    }
}
