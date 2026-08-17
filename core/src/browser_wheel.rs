//! Spec §4.4: the wheel changes hands, and the list of trusted hosts grows.
//!
//! Four moments, and they are separate because the guarantees between them are different:
//!
//! 1. **The agent asks** — it hit a login, a captcha, a consent wall. It does not try (spec §6.2
//!    would refuse it anyway); it asks, and from that instant its actions are refused rather than
//!    queued. The ask raises a proposal, because the daemon runs without a shell and the request has
//!    to survive the window being closed.
//! 2. **The person accepts** — the proposal's status transition is the atomic write that moves the
//!    session out of the agent's hands, and only then does anything happen to a browser.
//! 3. **The person drives** — headful, unfenced (§6.4: they are the one acting), over the PROJECT's
//!    profile even when the agent was in a throwaway (§4.5), with the navigation being recorded.
//! 4. **The person gives it back** — the window closes, and they are shown where they went and asked
//!    whether to keep it. That answer is the only way `browser_sites` ever grows.
//!
//! There is a fifth way in, and it is deliberately not one of the four: `open_window`, where a
//! person opens a window themselves without an agent having asked for anything. It skips step 1 and
//! step 2 entirely — there is no proposal, because the dialogue in those steps defends against an
//! agent choosing a destination, and here nobody did. It joins at step 3 and leaves through step 4
//! like any other, so what a session may GRANT is unchanged; only who may start one is wider. It
//! exists because until it did, a profile could not be prepared, only repaired: the sole way to log
//! in was to wait for the agent to fail at the login first.
//!
//! # Why this module holds no SQL
//!
//! `browser_sessions` has exactly one module that writes to it, and that is `browser.rs`. This one is
//! the machine: which transition is legal, in what order, and what happens when the middle step
//! fails. Splitting them that way keeps the invariant checkable by looking at one file.
//!
//! # The one narrowing of the spec, stated rather than hidden
//!
//! Spec §4.4's diagram draws an arrow from `volante_pedido` back to `agente_conduz` on a refusal.
//! Here a refused wheel **closes the session** instead. Two reasons. The wall that caused the request
//! is still there, so what the agent gets back is a page it cannot use — and §4.5 already says the
//! run goes on without that page. And the sidecar's `Handoff` is one-way by construction: giving the
//! wheel back would need a fourth verb over there and a second place where the two processes can
//! disagree about who is driving, which is the disagreement this whole machine exists to prevent.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::browser::{self, SessionRow, mode};
use crate::browser_client::{BrowserError, Placement};
use crate::browser_policy;
use crate::state::AppState;

/// Everything that can stop a handover, in the grades a caller has to tell apart.
#[derive(Debug)]
pub enum WheelError {
    /// No such session, or it has already closed.
    NoSuchSession,
    /// The session is not in the state this step needs — somebody else already answered, or the
    /// agent is asking for a wheel it has already asked for.
    WrongState(String),
    /// The pillar is off.
    Disabled,
    /// The sidecar said no, or could not be reached.
    Sidecar(BrowserError),
    /// Nobody is at the machine, and this is a window only a person may ask for.
    NoOnePresent,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for WheelError {
    fn from(error: sqlx::Error) -> Self {
        WheelError::Db(error)
    }
}

impl std::fmt::Display for WheelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WheelError::NoSuchSession => write!(f, "no such browsing session"),
            WheelError::WrongState(why) => write!(f, "{why}"),
            WheelError::Disabled => write!(f, "the browser pillar is off"),
            WheelError::Sidecar(error) => write!(f, "{error}"),
            WheelError::NoOnePresent => write!(
                f,
                "a window is opened for somebody to sit at, and nobody is at this machine"
            ),
            WheelError::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

/// A person opens a window of their own, on a project's profile.
///
/// # Why this is not a handover, and has no proposal
///
/// Everything in §4.4 — the proposal, the literal punycode origin, "who asked and how they got
/// there" — exists because an AGENT chose the destination while carrying a stranger's words in its
/// context. That is the confused deputy the dialogue is built against. Here the person typed the
/// address. There is nobody to be confused, so asking them to approve their own request would be
/// ceremony, and ceremony is what teaches people to click through the dialogues that do matter.
///
/// # Why it exists at all
///
/// Spec §5.2 lets a host into a profile's list only when a person logs in, and until this function
/// the ONLY way to log in was for the agent to walk into the wall first: it asks, you accept, you
/// sign in. So a profile could not be prepared, only repaired. Wanting the agent to read your Jira
/// meant waiting for it to fail at Jira. The list still grows by exactly the same mechanism — the
/// window records where it went, and `keep` answers yes or no on the way out — so this widens who
/// may START a session and changes nothing about what a session may GRANT.
///
/// # The presence check is the security control
///
/// Without it this is a route that opens a real window over the profile holding the owner's live
/// cookies, reachable by anything holding the daemon token — which includes every run. `Requester`
/// is not enough on its own here: `browser.rs` uses it to decide a PLACEMENT, and being wrong there
/// costs a throwaway profile. Being wrong here costs a browser nobody is watching, logged in as the
/// owner. So it refuses rather than degrades.
pub async fn open_window(
    state: &AppState,
    project_id: &str,
    url: &str,
) -> Result<SessionRow, WheelError> {
    if !state.browser.enabled {
        return Err(WheelError::Disabled);
    }
    if !crate::attention::owner_is_present(&state.pool, chrono::Utc::now()).await {
        return Err(WheelError::NoOnePresent);
    }

    // The project's profile, never a throwaway — the same reason `accept` re-places a handover
    // (§4.5). A login made in a directory that is deleted afterwards is a login nobody keeps, and
    // this function's whole point is the login.
    let sites = browser::admitted_origins(&state.pool, project_id).await?;
    let placement = Placement::project(project_id, sites);

    let now = chrono::Utc::now().to_rfc3339();
    let row_id = browser::insert_person_window(&state.pool, project_id, url, &now).await?;

    // An empty session id, and the sidecar already understands it: `pool.TakeWheel` guards its
    // "close the agent's browser first" step behind `req.Session != ""`, so with nothing to displace
    // it goes straight to opening a headful browser on the profile. Nothing was added there for this.
    let wheel = match state.browser.client.take_wheel("", url, &placement).await {
        Ok(wheel) => wheel,
        Err(error) => {
            // Spec §4.4a's shape, for the same reason: a launch that failed is reported as a failed
            // delivery rather than cleaned away, so the person sees that their window did not open
            // instead of a click that did nothing.
            let _ =
                browser::set_mode(&state.pool, row_id, mode::HUMAN, mode::DELIVERY_FAILED).await;
            return Err(WheelError::Sidecar(error));
        }
    };

    browser::rebind_sidecar(&state.pool, row_id, &wheel.session, &placement.profile.id).await?;
    live(&state.pool, row_id).await
}

/// The agent asks for the wheel (spec §4.4 rule 3).
///
/// The order is the whole of rule 1. The state moves here FIRST and the sidecar is told second,
/// because this side is the authority on who is driving: if the sidecar call fails, the agent is
/// already being refused by this record, which is the safe direction. Told-then-recorded would leave
/// the opposite window.
pub async fn request(state: &AppState, session_id: i64, reason: &str) -> Result<i64, WheelError> {
    if !state.browser.enabled {
        return Err(WheelError::Disabled);
    }
    let row = live(&state.pool, session_id).await?;
    if row.mode != mode::AGENT {
        return Err(WheelError::WrongState(format!(
            "this session is {}, so there is no wheel for the agent to ask for",
            row.mode
        )));
    }
    if !browser::set_mode(&state.pool, session_id, mode::AGENT, mode::WHEEL_REQUESTED).await? {
        return Err(WheelError::WrongState(
            "somebody already asked for this session's wheel".to_string(),
        ));
    }

    // Second layer, and not fatal. The record above is what refuses the agent's next act; this makes
    // the browser refuse it too, so a crash that leaves the two out of step still fails closed on
    // both sides rather than on neither.
    if let Err(error) = state.browser.client.handoff(&row.sidecar_id, reason).await {
        tracing::warn!(
            session = session_id,
            %error,
            "the sidecar was not told the wheel had been asked for"
        );
    }

    let landed = landing(&row);
    let proposal = crate::proposals::create_wheel_request(
        &state.pool,
        crate::proposals::WheelAsk {
            run_id: row.run_id,
            project_id: row.project_id.as_deref().unwrap_or_default(),
            session_id,
            requested_url: &row.requested_url,
            final_url: &landed,
            // The complete, literal origin — punycode as stored, never prettified. Spec §5.2
            // measure 1: `xn--exemp1o-...` IS the information, and hiding it is the attack.
            origin: &browser_policy::origin_of(&landed).unwrap_or_else(|| landed.clone()),
            reasoning: reason,
        },
    )
    .await?;
    browser::attach_proposal(&state.pool, session_id, proposal).await?;
    Ok(proposal)
}

/// The person accepted. Close the agent's browser, open theirs (spec §4.2).
///
/// `accept` is called with the proposal ALREADY transitioned to approved — that transition is the
/// atomic write of rule 1, and it belongs to the caller so that the same compare-and-set that decides
/// who won a concurrent approve is the one that hands over the wheel.
pub async fn accept(state: &AppState, session_id: i64) -> Result<SessionRow, WheelError> {
    if !state.browser.enabled {
        return Err(WheelError::Disabled);
    }
    let row = live(&state.pool, session_id).await?;
    if row.mode != mode::WHEEL_REQUESTED {
        return Err(WheelError::WrongState(format!(
            "this session is {}, and only a requested wheel can be handed over",
            row.mode
        )));
    }
    // Spec §4.5: the destination is the PROJECT's profile, never the throwaway the agent was in.
    // A login made in a profile that is deleted with the run is a login nobody keeps, and the
    // proposal outlives the run, so it would point at a directory that no longer exists.
    let project = row.project_id.clone().unwrap_or_default();
    let sites = browser::admitted_origins(&state.pool, &project).await?;
    let placement = Placement::project(&project, sites);
    let url = landing(&row);

    let wheel = match state
        .browser
        .client
        .take_wheel(&row.sidecar_id, &url, &placement)
        .await
    {
        Ok(wheel) => wheel,
        Err(error) => {
            // Spec §4.4a. It does NOT go back to the agent: the agent does not recover the wheel
            // because a launch of ours failed. The proposal is updated so the person can see why and
            // decide whether to try again.
            let _ = browser::set_mode(
                &state.pool,
                session_id,
                mode::WHEEL_REQUESTED,
                mode::DELIVERY_FAILED,
            )
            .await;
            if let Some(proposal) = row.proposal_id {
                let _ = crate::proposals::note(
                    &state.pool,
                    proposal,
                    &format!("the window would not open: {error}"),
                )
                .await;
            }
            return Err(WheelError::Sidecar(error));
        }
    };

    // Spec §4.1 allows one browser per profile, so the handover took down whatever else was running
    // in it. Those rows are closed here rather than left to the sweeper: a row saying "open" about a
    // browser that has been stopped is a row the UI would offer to hand over a second time.
    let now = chrono::Utc::now().to_rfc3339();
    for sidecar_id in &wheel.displaced {
        if let Ok(Some(displaced)) = row_by_sidecar_id(&state.pool, sidecar_id).await {
            let _ =
                browser::close_session_row(&state.pool, displaced, "displaced-by-handover", &now)
                    .await;
        }
    }

    browser::rebind_sidecar(
        &state.pool,
        session_id,
        &wheel.session,
        &placement.profile.id,
    )
    .await?;
    if !browser::set_mode(&state.pool, session_id, mode::WHEEL_REQUESTED, mode::HUMAN).await? {
        tracing::warn!(
            session = session_id,
            "the wheel landed on a session that had moved"
        );
    }
    live(&state.pool, session_id).await
}

/// The agent's wheel request was refused. The session closes.
///
/// See the module comment for why this is a close rather than a return to `agente_conduz`.
pub async fn refuse(state: &AppState, session_id: i64) -> Result<(), WheelError> {
    let now = chrono::Utc::now().to_rfc3339();
    match browser::close(&state.pool, &state.browser, session_id, &now).await {
        Ok(_) => Ok(()),
        Err(error) => Err(WheelError::Sidecar(error)),
    }
}

/// The person gives the wheel back. The window closes and the chain comes home.
///
/// Nothing is granted here. What comes back is a list of candidates to be shown, and the granting is
/// a separate answer to a separate question — spec §5.2 is explicit that the concession happens at
/// the return and covers the whole set, so the person has to see the set first.
pub async fn give_back(state: &AppState, session_id: i64) -> Result<Vec<String>, WheelError> {
    let row = live(&state.pool, session_id).await?;
    if row.mode != mode::HUMAN {
        return Err(WheelError::WrongState(format!(
            "this session is {}, so there is no wheel to give back",
            row.mode
        )));
    }
    let returned = state
        .browser
        .client
        .return_wheel(&row.sidecar_id)
        .await
        .map_err(WheelError::Sidecar)?;

    let now = chrono::Utc::now().to_rfc3339();
    browser::record_chain(&state.pool, session_id, &returned.chain).await?;
    // The window is gone, so the session is over. The chain outlives the row on purpose: the person
    // may take a moment to answer, and the answer is about a login that has already happened.
    browser::close_session_row(&state.pool, session_id, "wheel-returned", &now).await?;
    Ok(returned.chain)
}

/// The person's answer to "keep these?" — the only way `browser_sites` grows (spec §5.2, §5.3a).
///
/// It names no host. It answers yes or no to a chain recorded by a browser under the person's own
/// hands, which is why there is no `POST /browser/grant`: a route that took hosts would be a route
/// the confused deputy of §5.2 could aim, and this one has nothing to aim.
pub async fn keep(
    state: &AppState,
    session_id: i64,
    keep: bool,
) -> Result<Vec<String>, WheelError> {
    let Some(row) = browser::session_row(&state.pool, session_id).await? else {
        return Err(WheelError::NoSuchSession);
    };
    let now = chrono::Utc::now().to_rfc3339();
    let Some(chain) = browser::take_chain(&state.pool, session_id, &now).await? else {
        return Err(WheelError::WrongState(
            "there is nothing to keep for this session, or it has already been answered"
                .to_string(),
        ));
    };
    if !keep {
        return Ok(Vec::new());
    }
    let project = row.project_id.clone().unwrap_or_default();
    Ok(browser::grant(&state.pool, &project, &chain, &now).await?)
}

/// Where the session actually is: the landing url when there is one, the requested one otherwise.
///
/// A session refused by the fence has no final url at all, and handing the person an empty string
/// would open a blank window for a request that named a page.
fn landing(row: &SessionRow) -> String {
    if row.final_url.is_empty() {
        row.requested_url.clone()
    } else {
        row.final_url.clone()
    }
}

async fn live(pool: &SqlitePool, id: i64) -> Result<SessionRow, WheelError> {
    match browser::session_row(pool, id).await? {
        Some(row) if row.closed_at.is_none() => Ok(row),
        _ => Err(WheelError::NoSuchSession),
    }
}

async fn row_by_sidecar_id(pool: &SqlitePool, sidecar_id: &str) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM browser_sessions WHERE sidecar_id = ? AND closed_at IS NULL LIMIT 1",
    )
    .bind(sidecar_id)
    .fetch_optional(pool)
    .await
}

// ---------------------------------------------------------------------------------------------
// The handlers.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HandoffBody {
    pub session_id: i64,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct WheelBody {
    pub session_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct KeepBody {
    pub session_id: i64,
    pub keep: bool,
}

#[derive(Debug, Deserialize)]
pub struct WindowBody {
    pub project_id: String,
    pub url: String,
}

/// `POST /browser/window` — a person opens one for themselves.
///
/// No `run_id` field, and its absence is the point: a run cannot ask for this. The refusal is
/// `open_window`'s presence check rather than a missing field, but a field a run could fill would
/// invite exactly the caller this must not have.
pub async fn post_window(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<WindowBody>,
) -> axum::response::Response {
    match open_window(&state, &body.project_id, &body.url).await {
        Ok(row) => axum::Json(row).into_response(),
        Err(error) => wheel_error(error),
    }
}

/// `POST /browser/handoff` — the agent asks for the wheel.
pub async fn post_handoff(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<HandoffBody>,
) -> axum::response::Response {
    match request(&state, body.session_id, &body.reason).await {
        Ok(proposal) => axum::Json(serde_json::json!({ "proposal_id": proposal })).into_response(),
        Err(error) => wheel_error(error),
    }
}

/// `POST /browser/return` — the person hands it back, and is told where they went.
pub async fn post_return(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<WheelBody>,
) -> axum::response::Response {
    match give_back(&state, body.session_id).await {
        Ok(chain) => axum::Json(serde_json::json!({ "chain": chain })).into_response(),
        Err(error) => wheel_error(error),
    }
}

/// `POST /browser/keep` — the answer to the chain.
pub async fn post_keep(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<KeepBody>,
) -> axum::response::Response {
    match keep(&state, body.session_id, body.keep).await {
        Ok(granted) => axum::Json(serde_json::json!({ "granted": granted })).into_response(),
        Err(error) => wheel_error(error),
    }
}

fn wheel_error(error: WheelError) -> axum::response::Response {
    let status = match error {
        WheelError::NoSuchSession => StatusCode::NOT_FOUND,
        // 409, like every other refusal on this surface: nothing about the credentials is wrong, and
        // the machine is simply not in a state where this can be carried out.
        WheelError::WrongState(_) | WheelError::Disabled | WheelError::NoOnePresent => {
            StatusCode::CONFLICT
        }
        WheelError::Sidecar(BrowserError::FenceDown(_)) => StatusCode::SERVICE_UNAVAILABLE,
        WheelError::Sidecar(_) => StatusCode::BAD_GATEWAY,
        WheelError::Db(ref db) => {
            tracing::error!(error = %db, "browser wheel database error");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, error.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::{Ask, Opened};
    use crate::browser_client::BrowserClient;
    use crate::browser_policy::{Requester, Surface};
    use crate::storage::TempDb;

    const NOW: &str = "2026-08-16T10:00:00Z";

    /// A sidecar that answers the six verbs and the two wheel ones, and records every call.
    ///
    /// It answers `/wheel/return` with a chain the test chooses, because the chain is the only thing
    /// a handover produces that outlives it â€” everything else here is a window that closes.
    async fn stub_sidecar(
        chain: Vec<&'static str>,
        take_fails: bool,
    ) -> (
        std::sync::Arc<crate::browser::BrowserRuntime>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use axum::extract::Path;
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let wheel_recorder = seen.clone();
        let verb_recorder = seen.clone();
        let app = axum::Router::new()
            .route(
                "/wheel/{verb}",
                post(move |Path(verb): Path<String>, _body: axum::body::Bytes| {
                    let recorder = wheel_recorder.clone();
                    let chain = chain.clone();
                    async move {
                        recorder.lock().unwrap().push(format!("wheel/{verb}"));
                        if verb == "take" {
                            if take_fails {
                                return (
                                    axum::http::StatusCode::BAD_GATEWAY,
                                    "no display available",
                                )
                                    .into_response();
                            }
                            return axum::Json(serde_json::json!({
                                "session": "h1",
                                "mode": "human",
                                "url": "https://jira.example.org/login",
                            }))
                            .into_response();
                        }
                        axum::Json(serde_json::json!({ "chain": chain })).into_response()
                    }
                }),
            )
            .route(
                "/{verb}",
                post(move |Path(verb): Path<String>, _body: axum::body::Bytes| {
                    let recorder = verb_recorder.clone();
                    async move {
                        recorder.lock().unwrap().push(verb.clone());
                        if verb == "open" {
                            return axum::Json(serde_json::json!({
                                "id": "s1",
                                "mode": "agent",
                                "requested_url": "https://jira.example.org/login",
                                "final_url": "https://jira.example.org/login",
                                "title": "",
                            }))
                            .into_response();
                        }
                        if verb == "handoff" {
                            return axum::Json(serde_json::json!({
                                "session_id": "s1",
                                "mode": "human",
                                "url": "https://jira.example.org/login",
                                "reason": "",
                            }))
                            .into_response();
                        }
                        axum::http::StatusCode::NO_CONTENT.into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            std::sync::Arc::new(crate::browser::BrowserRuntime {
                enabled: true,
                client: BrowserClient::new(&address.to_string(), "tok".into()),
            }),
            seen,
        )
    }

    async fn wheeled(
        chain: Vec<&'static str>,
        take_fails: bool,
    ) -> (
        TempDb,
        AppState,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let db = TempDb::new().await;
        let (browser, seen) = stub_sidecar(chain, take_fails).await;
        let state = AppState {
            token: crate::auth::Token("test-token".into()),
            pool: db.pool.clone(),
            runner: std::sync::Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            local_assistant: None,
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_tails: Default::default(),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser,
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        };
        (db, state, seen)
    }

    /// Opens a throwaway session, which is the ordinary case: the agent hit a login wall, and it hit
    /// it somewhere that is not on the project's list yet.
    async fn a_session(state: &AppState) -> i64 {
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

    /// Spec Â§4.4 rule 3, and the first half of rule 1. Asking raises a proposal â€” the daemon runs
    /// without a shell, so the request has to survive the window being closed â€” and the session
    /// leaves the agent's hands in the same breath.
    #[tokio::test]
    async fn asking_for_the_wheel_raises_a_proposal_and_takes_the_session_off_the_agent() {
        let (db, state, seen) = wheeled(vec![], false).await;
        let session = a_session(&state).await;

        let proposal = request(&state, session, "there is a login here")
            .await
            .expect("request");

        let row = crate::browser::session_row(&db.pool, session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.mode, mode::WHEEL_REQUESTED);
        assert_eq!(row.proposal_id, Some(proposal));

        let raised = crate::proposals::get(&db.pool, proposal)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(raised.kind, "browser-wheel");
        assert_eq!(raised.status, "pending");
        assert_eq!(raised.project_id.as_deref(), Some("acme"));
        assert_eq!(raised.run_id, Some(7));

        // Spec Â§5.2 measure 1: the dialogue is given the complete, literal origin â€” with its port,
        // in the punycode form the policy stores. Prettifying it is the attack.
        let input: serde_json::Value =
            serde_json::from_str(raised.tool_input.as_deref().unwrap()).unwrap();
        assert_eq!(input["origin"], "https://jira.example.org:443");
        assert_eq!(input["session_id"], session);

        // And the sidecar was told, so the browser refuses the agent as well.
        assert!(seen.lock().unwrap().iter().any(|call| call == "handoff"));

        // Asking twice is refused rather than raising a second proposal for one wall.
        assert!(matches!(
            request(&state, session, "again").await,
            Err(WheelError::WrongState(_))
        ));
        db.close().await;
    }

    /// Spec Â§4.5 and Â§4.2 together, which is the whole point of the handover: the agent was in a
    /// throwaway, and the person is sent to the PROJECT's profile â€” the one that is not deleted with
    /// the run, and therefore the only one where a login is worth making.
    #[tokio::test]
    async fn accepting_opens_the_project_profile_and_not_the_throwaway() {
        let (db, state, _) = wheeled(vec![], false).await;
        let session = a_session(&state).await;
        request(&state, session, "login").await.expect("request");

        let before = crate::browser::session_row(&db.pool, session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            before.profile_kind, "ephemeral",
            "the agent was in a throwaway"
        );

        let row = accept(&state, session).await.expect("accept");
        assert_eq!(row.mode, mode::HUMAN);
        assert_eq!(row.profile_kind, "project");
        assert_eq!(row.profile_id, "acme");
        assert_eq!(row.sidecar_id, "h1", "the row follows the person's window");
        db.close().await;
    }

    /// Spec Â§4.4a. The window does not open, and the wheel does NOT go back to the agent â€” a failure
    /// of ours is not a reason to let it drive again. The proposal carries the reason, so the person
    /// can retry or give up.
    #[tokio::test]
    async fn a_window_that_will_not_open_is_a_failed_delivery_and_not_a_return() {
        let (db, state, _) = wheeled(vec![], true).await;
        let session = a_session(&state).await;
        let proposal = request(&state, session, "login").await.expect("request");

        let outcome = accept(&state, session).await;
        assert!(
            matches!(outcome, Err(WheelError::Sidecar(_))),
            "{outcome:?}"
        );

        let row = crate::browser::session_row(&db.pool, session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.mode, mode::DELIVERY_FAILED);

        let notes: Vec<String> = sqlx::query_scalar(
            "SELECT note FROM proposal_events WHERE proposal_id = ? ORDER BY id",
        )
        .bind(proposal)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert!(
            notes.iter().any(|note| note.contains("would not open")),
            "the proposal says nothing about why: {notes:?}"
        );
        db.close().await;
    }

    /// Spec Â§5.2 and Â§5.3a, end to end: the wheel comes back with the chain, and NOTHING is granted
    /// until a person answers. The two steps are separate because the person has to see the set
    /// before agreeing to it.
    #[tokio::test]
    async fn the_chain_is_granted_only_when_the_person_says_so() {
        let (db, state, _) = wheeled(
            vec![
                "https://jira.example.org/login",
                "https://accounts.google.com/o/oauth2/auth",
                "https://jira.example.org/browse/X-1",
            ],
            false,
        )
        .await;
        let session = a_session(&state).await;
        request(&state, session, "login").await.expect("request");
        accept(&state, session).await.expect("accept");

        let chain = give_back(&state, session).await.expect("give back");
        assert_eq!(chain.len(), 3);
        assert!(
            crate::browser::list_sites(&db.pool, "acme")
                .await
                .unwrap()
                .is_empty(),
            "handing the wheel back must not grant anything on its own"
        );

        let granted = keep(&state, session, true).await.expect("keep");
        assert_eq!(
            granted,
            vec![
                "https://jira.example.org:443",
                "https://accounts.google.com:443"
            ]
        );

        // And it can only be answered once: the permission belongs to the moment of the login.
        assert!(matches!(
            keep(&state, session, true).await,
            Err(WheelError::WrongState(_))
        ));
        db.close().await;
    }

    /// The other answer. Saying no grants nothing AND closes the question, so a chain cannot be kept
    /// later by somebody who has forgotten what it was for.
    #[tokio::test]
    async fn saying_no_to_the_chain_grants_nothing_and_closes_the_question() {
        let (db, state, _) = wheeled(vec!["https://jira.example.org/login"], false).await;
        let session = a_session(&state).await;
        request(&state, session, "login").await.expect("request");
        accept(&state, session).await.expect("accept");
        give_back(&state, session).await.expect("give back");

        assert!(keep(&state, session, false).await.expect("keep").is_empty());
        assert!(
            crate::browser::list_sites(&db.pool, "acme")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            keep(&state, session, true).await,
            Err(WheelError::WrongState(_))
        ));
        db.close().await;
    }

    /// The states do not move out of order. Each of these would otherwise be a way to open a window
    /// nobody accepted, or to grant a chain nobody walked.
    #[tokio::test]
    async fn the_states_only_move_in_one_direction() {
        let (db, state, _) = wheeled(vec![], false).await;
        let session = a_session(&state).await;

        // Accepting before anybody asked.
        assert!(matches!(
            accept(&state, session).await,
            Err(WheelError::WrongState(_))
        ));
        // Giving back a wheel nobody has.
        assert!(matches!(
            give_back(&state, session).await,
            Err(WheelError::WrongState(_))
        ));
        // Keeping a chain that was never recorded.
        assert!(matches!(
            keep(&state, session, true).await,
            Err(WheelError::WrongState(_))
        ));
        // And none of it works on a session that does not exist.
        assert!(matches!(
            request(&state, 9999, "x").await,
            Err(WheelError::NoSuchSession)
        ));
        db.close().await;
    }

    /// A person opens a window with no agent involved, and it arrives ready to be given back.
    ///
    /// The assertions to care about are the two that say what this is NOT. No proposal is raised —
    /// the §4.4 dialogue defends against an agent having chosen the destination, and asking somebody
    /// to approve an address they just typed is the ceremony that teaches people to click through
    /// the dialogues that matter. And the profile is the project's, never a throwaway, because the
    /// whole point of this door is a login that outlives the session.
    #[tokio::test]
    async fn a_person_opens_a_window_with_no_agent_and_no_proposal() {
        let (db, state, seen) = wheeled(vec![], false).await;
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Global,
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        let row = open_window(&state, "nucleos", "https://jira.example.org/login")
            .await
            .expect("the window opens");

        assert_eq!(row.mode, mode::HUMAN, "it is the person's from the start");
        assert_eq!(row.profile_kind, "project");
        assert_eq!(row.rule, "person-opened");
        assert_eq!(row.run_id, None, "no run asked for this and none may");
        assert_eq!(
            row.proposal_id, None,
            "a person approving their own address is ceremony, not consent"
        );
        assert!(
            seen.lock().unwrap().iter().any(|verb| verb == "wheel/take"),
            "the sidecar was asked for a headful browser: {:?}",
            seen.lock().unwrap()
        );

        // It leaves by the ordinary door: this is the same `give_back` the handover uses, which is
        // what makes the grant rule identical for both ways in.
        assert!(give_back(&state, row.id).await.is_ok());
        db.close().await;
    }

    /// Nobody at the machine gets no window, and gets no row either.
    ///
    /// This is the security half of `open_window`. Without the check it is a route that opens a real
    /// browser over the profile holding the owner's live cookies, and every run holds the token that
    /// would reach it. The second assertion matters as much as the first: a refusal that still wrote
    /// a row would leave the sessions list advertising a browser to hand over.
    #[tokio::test]
    async fn nobody_present_gets_no_window() {
        let (db, state, seen) = wheeled(vec![], false).await;

        assert!(matches!(
            open_window(&state, "nucleos", "https://jira.example.org/login").await,
            Err(WheelError::NoOnePresent)
        ));
        assert!(
            !seen.lock().unwrap().iter().any(|verb| verb == "wheel/take"),
            "no browser should have been asked for"
        );
        assert!(
            browser::open_sessions(&state.pool)
                .await
                .unwrap()
                .is_empty(),
            "a refusal must not leave a row offering a handover"
        );
        db.close().await;
    }
}
