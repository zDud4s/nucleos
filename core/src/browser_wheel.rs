//! §spec pilar-de-browser
//!
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
    // `.read` and no write list, because this is the PERSON's window: headful, with no fence
    // attached at all (spec §4.1), so a write grant here would be a field nothing consults. The read
    // list travels because the sidecar uses it to place the profile, not to police it.
    let sites = browser::admitted_origins(&state.pool, project_id).await?;
    let placement = Placement::project(project_id, sites.read);

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
            let _ = browser::set_mode(
                &state.pool,
                &state.browser.modes,
                row_id,
                mode::HUMAN,
                mode::DELIVERY_FAILED,
            )
            .await;
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
    if !browser::set_mode(
        &state.pool,
        &state.browser.modes,
        session_id,
        mode::AGENT,
        mode::WHEEL_REQUESTED,
    )
    .await?
    {
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
///
/// This is the seat-less form the tests use; production goes through `accept_with_seat`.
#[cfg(any(test, feature = "testkit"))]
pub async fn accept(state: &AppState, session_id: i64) -> Result<SessionRow, WheelError> {
    accept_with_seat(state, session_id, None)
        .await
        .map(|(row, _)| row)
}

/// Where the person sits once they accept: in the shell, or in a real window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeatChoice {
    Shell,
    Window,
}

/// The approval with the seat spelled out (spec browser-volante §4.1).
///
/// The shell seat is on offer when the row is `shell_eligible`; with no explicit choice it is what
/// the approval picks, and otherwise the person gets a real window as before. An explicit shell
/// choice that is not on offer is refused rather than silently turned into a window. The second
/// element is the `seat_nonce` and is `Some` only for the shell seat.
pub async fn accept_with_seat(
    state: &AppState,
    session_id: i64,
    choice: Option<SeatChoice>,
) -> Result<(SessionRow, Option<String>), WheelError> {
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
    let seat = match choice {
        Some(SeatChoice::Shell) if !row.shell_eligible => {
            return Err(WheelError::WrongState("seat_unavailable".to_string()));
        }
        Some(choice) => choice,
        None if row.shell_eligible => SeatChoice::Shell,
        None => SeatChoice::Window,
    };

    if seat == SeatChoice::Shell {
        // Same browser, no new Chrome: the row moves first, then the sidecar is told the person's
        // stretch begins. A failure here is a failed delivery (spec §4.4a), never a return to the
        // agent.
        if !browser::set_mode_seat(
            &state.pool,
            &state.browser.modes,
            session_id,
            mode::WHEEL_REQUESTED,
            mode::HUMAN,
            Some("shell"),
        )
        .await?
        {
            return Err(WheelError::WrongState(
                "this session's wheel was already handed over".to_string(),
            ));
        }
        if let Err(error) = state.browser.client.begin_person(&row.sidecar_id).await {
            let _ = browser::set_mode(
                &state.pool,
                &state.browser.modes,
                session_id,
                mode::HUMAN,
                mode::DELIVERY_FAILED,
            )
            .await;
            if let Some(proposal) = row.proposal_id {
                let _ = crate::proposals::note(
                    &state.pool,
                    proposal,
                    &format!("the shell seat would not begin: {error}"),
                )
                .await;
            }
            return Err(WheelError::Sidecar(error));
        }
        let nonce = state.browser.seats.issue(session_id);
        let row = live(&state.pool, session_id).await?;
        return Ok((row, Some(nonce)));
    }

    // Spec §4.5: the destination is the PROJECT's profile, never the throwaway the agent was in.
    // A login made in a profile that is deleted with the run is a login nobody keeps, and the
    // proposal outlives the run, so it would point at a directory that no longer exists.
    let project = row.project_id.clone().unwrap_or_default();
    let sites = browser::admitted_origins(&state.pool, &project).await?;
    let placement = Placement::project(&project, sites.read);
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
                &state.browser.modes,
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
    if !browser::set_mode_seat(
        &state.pool,
        &state.browser.modes,
        session_id,
        mode::WHEEL_REQUESTED,
        mode::HUMAN,
        Some("window"),
    )
    .await?
    {
        tracing::warn!(
            session = session_id,
            "the wheel landed on a session that had moved"
        );
    }
    Ok((live(&state.pool, session_id).await?, None))
}

/// The person moves a shell-seat session to a real window. Same mode (`human`), new seat.
///
/// The agent's browser is closed and a headful one opens on the project's profile, exactly as at an
/// approval that chose a window; the shell's nonce is forgotten so the shell can no longer drive.
pub async fn to_window(state: &AppState, session_id: i64) -> Result<SessionRow, WheelError> {
    if !state.browser.enabled {
        return Err(WheelError::Disabled);
    }
    let row = live(&state.pool, session_id).await?;
    if row.mode != mode::HUMAN || row.seat.as_deref() != Some("shell") {
        return Err(WheelError::WrongState(format!(
            "this session is {}, and only a person driving from the shell can open a window",
            row.mode
        )));
    }
    let project = row.project_id.clone().unwrap_or_default();
    let sites = browser::admitted_origins(&state.pool, &project).await?;
    let placement = Placement::project(&project, sites.read);
    let url = landing(&row);

    let wheel = match state
        .browser
        .client
        .take_wheel(&row.sidecar_id, &url, &placement)
        .await
    {
        Ok(wheel) => wheel,
        Err(error) => {
            // The person had the session, so the headless browser is unfenced: ask the sidecar to
            // end the person's stretch before the row is marked failed. Best effort; the error
            // carries no session content.
            if let Err(end_error) = state.browser.client.end_person(&row.sidecar_id).await {
                tracing::warn!(error = %end_error, "could not end the person stretch after a failed window");
            }
            let _ = browser::set_mode(
                &state.pool,
                &state.browser.modes,
                session_id,
                mode::HUMAN,
                mode::DELIVERY_FAILED,
            )
            .await;
            state.browser.seats.forget(session_id);
            return Err(WheelError::Sidecar(error));
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    for sidecar_id in &wheel.displaced {
        if let Ok(Some(displaced)) = row_by_sidecar_id(&state.pool, sidecar_id).await
            && displaced != session_id
        {
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
    browser::set_mode_seat(
        &state.pool,
        &state.browser.modes,
        session_id,
        mode::HUMAN,
        mode::HUMAN,
        Some("window"),
    )
    .await?;
    state.browser.seats.forget(session_id);
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
///
/// `to` says where the session goes. `Some("agent")` from the shell seat hands it back to the run
/// that opened it, but only once the sidecar has confirmed the fence is restored; anything else
/// closes it.
pub async fn give_back(
    state: &AppState,
    session_id: i64,
    to: Option<&str>,
) -> Result<Vec<String>, WheelError> {
    let row = live(&state.pool, session_id).await?;
    if row.mode != mode::HUMAN {
        return Err(WheelError::WrongState(format!(
            "this session is {}, so there is no wheel to give back",
            row.mode
        )));
    }
    if row.seat.as_deref() == Some("shell") {
        return give_back_from_shell(state, &row, to).await;
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

/// Is the run that owns this session still going? Only then is there somebody to hand it back to.
pub(crate) async fn run_is_live(
    pool: &SqlitePool,
    row: &SessionRow,
) -> Result<bool, WheelError> {
    let Some(run_id) = row.run_id else {
        return Ok(false);
    };
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await?;
    Ok(status.as_deref() == Some("running"))
}

/// The person's stretch in the shell is over. There is no window to give back, so the sidecar's
/// `/person/end` stands in for `/wheel/return` and is what brings the chain home.
async fn give_back_from_shell(
    state: &AppState,
    row: &SessionRow,
    to: Option<&str>,
) -> Result<Vec<String>, WheelError> {
    let session_id = row.id;
    let now = chrono::Utc::now().to_rfc3339();
    let to_agent = to == Some("agent") && run_is_live(&state.pool, row).await?;

    let ended = state.browser.client.end_person(&row.sidecar_id).await;
    if to_agent {
        let returned = match ended {
            Ok(returned) => returned,
            Err(error) => {
                // The fence state is unknown, so the session must not go back to the agent.
                let _ = browser::close_with_reason(
                    &state.pool,
                    &state.browser,
                    session_id,
                    "fence-not-restored",
                    &now,
                )
                .await;
                state.browser.seats.forget(session_id);
                return Err(WheelError::Sidecar(error));
            }
        };
        browser::record_chain(&state.pool, session_id, &returned.chain).await?;
        let moved = browser::set_mode_seat(
            &state.pool,
            &state.browser.modes,
            session_id,
            mode::HUMAN,
            mode::AGENT,
            None,
        )
        .await?;
        state.browser.seats.forget(session_id);
        if !moved {
            return Err(WheelError::WrongState(
                "the session changed hands while it was being returned".to_string(),
            ));
        }
        state.browser.seats.mark_returned(session_id);
        return Ok(returned.chain);
    }

    // Close, an absent `to`, or no run alive to receive it: the chain from a best-effort end (empty
    // when the sidecar is gone), and the row closes as a plain return.
    let chain = ended.map(|returned| returned.chain).unwrap_or_default();
    browser::record_chain(&state.pool, session_id, &chain).await?;
    // The sidecar's session is closed here too: the browser is not a window the person owns.
    let _ = browser::close_with_reason(
        &state.pool,
        &state.browser,
        session_id,
        "wheel-returned",
        &now,
    )
    .await;
    state.browser.seats.forget(session_id);
    Ok(chain)
}

/// The person's answer to "keep these?" — the only way `browser_sites` grows (spec §5.2, §5.3a).
///
/// It names no host. It answers yes or no to a chain recorded by a browser under the person's own
/// hands, which is why there is no `POST /browser/grant`: a route that took hosts would be a route
/// the confused deputy of §5.2 could aim, and this one has nothing to aim.
///
/// # Two answers, because reading and writing are two permissions
///
/// `writable` is the second half of the same question, asked at the same moment and about the same
/// chain: may an agent also SUBMIT FORMS where this login landed. It is a separate field rather than
/// a wider `keep` because the two are wanted in different combinations — read the Jira and open no
/// tickets, read the inbox and answer nothing — and it applies only to the destination, never to the
/// identity providers the login passed through (see [`browser::grant`], where that is decided).
///
/// `writable` without `keep` grants nothing at all: there is no writing to a site the profile may
/// not load. The combination is not rejected, because there is nothing to reject — the early return
/// below never reaches the grant.
pub async fn keep(
    state: &AppState,
    session_id: i64,
    keep: bool,
    writable: bool,
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
    Ok(browser::grant(&state.pool, &project, &chain, writable, &now).await?)
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
    /// Where the session goes: `agent` hands it back to its run, anything else (or nothing) closes.
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KeepBody {
    pub session_id: i64,
    pub keep: bool,
    /// May an agent also submit forms where this login landed? Defaults to FALSE, so a caller that
    /// has never heard of writing grants none — the direction an omitted field has to fail in when
    /// the field is a permission.
    #[serde(default)]
    pub writable: bool,
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

/// `POST /browser/sessions/{id}/window` — a person driving from the shell asks for a real window.
///
/// The same credential, run check and presence as `take`: it opens a headful browser on the
/// project's profile, so a run must not be able to ask for it.
pub async fn post_window_seat(
    State(state): State<AppState>,
    axum::Extension(scope): axum::Extension<crate::auth::Scope>,
    headers: axum::http::HeaderMap,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> axum::response::Response {
    use crate::browser_seat::{SeatError, from_a_run, seat_error};

    if from_a_run(&scope, &headers) {
        return seat_error(SeatError::RunRequester);
    }
    if !crate::attention::owner_is_present(&state.pool, chrono::Utc::now()).await {
        return seat_error(SeatError::OwnerAbsent);
    }
    match to_window(&state, id).await {
        Ok(row) => axum::Json(serde_json::json!({ "session": row })).into_response(),
        Err(WheelError::WrongState(_)) => seat_error(SeatError::NotPerson),
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
    match give_back(&state, body.session_id, body.to.as_deref()).await {
        Ok(chain) => axum::Json(serde_json::json!({ "chain": chain })).into_response(),
        Err(error) => wheel_error(error),
    }
}

/// `POST /browser/keep` — the answer to the chain.
pub async fn post_keep(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<KeepBody>,
) -> axum::response::Response {
    match keep(&state, body.session_id, body.keep, body.writable).await {
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

    /// A sidecar that answers the driver's verbs and the two wheel ones, and records every call.
    ///
    /// It answers `/wheel/return` with a chain the test chooses, because the chain is the only thing
    /// a handover produces that outlives it â€” everything else here is a window that closes.
    async fn stub_sidecar(
        chain: Vec<&'static str>,
        take_fails: bool,
        person_fails: bool,
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
        let person_recorder = seen.clone();
        let person_chain = chain.clone();
        let app = axum::Router::new()
            .route(
                "/person/{verb}",
                post(move |Path(verb): Path<String>, _body: axum::body::Bytes| {
                    let recorder = person_recorder.clone();
                    let chain = person_chain.clone();
                    async move {
                        recorder.lock().unwrap().push(format!("person/{verb}"));
                        if verb == "begin" {
                            if person_fails {
                                return (axum::http::StatusCode::BAD_GATEWAY, "no pixels")
                                    .into_response();
                            }
                            return axum::Json(serde_json::json!({})).into_response();
                        }
                        axum::Json(serde_json::json!({ "chain": chain })).into_response()
                    }
                }),
            )
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
                modes: Default::default(),
                seats: Default::default(),
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
        wheeled_with(chain, take_fails, false).await
    }

    async fn wheeled_with(
        chain: Vec<&'static str>,
        take_fails: bool,
        person_fails: bool,
    ) -> (
        TempDb,
        AppState,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let db = TempDb::new().await;
        let (browser, seen) = stub_sidecar(chain, take_fails, person_fails).await;
        let state = AppState {
            token: crate::auth::Token("test-token".into()),
            pool: db.pool.clone(),
            telegram_doctrine: None,
            runner: std::sync::Arc::new(crate::runner::FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: std::sync::Arc::new(crate::assistants::NoAssistants),
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_messages: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            run_tails: Default::default(),
            // No files folder: nothing on the wheel's path reads or writes one.
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::voice::VoiceRuntime::default()),
            browser,
            github: std::sync::Arc::new(crate::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
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

    /// The daemon-side guard on the READ verbs, and the reason it has to be on this side at all.
    ///
    /// **Found by driving a live daemon, not by this suite.** A session was handed off and then
    /// asked for a snapshot, and it answered — with the page. `post_act` and `post_look` refuse once
    /// the session is not the agent's; `post_snapshot` and `post_screenshot` did not.
    ///
    /// Nothing was exploitable, and that is the part worth keeping. The sidecar's `Human` driver
    /// refuses all three, so the second layer held. But its comment says the núcleo "already refuses
    /// them from its own record (spec §4.4 rule 1), so this is the second layer" — and the núcleo
    /// refused two of the three. A first layer that is only believed in cannot do the job it exists
    /// for, which `post_act` states exactly: it holds when the two processes DISAGREE about who is
    /// driving, the state a crash between the request and the handover produces.
    ///
    /// A tree is not pixels, and that is not a defence. A login form's accessibility tree names the
    /// fields and carries their values.
    #[tokio::test]
    async fn the_read_verbs_leave_the_agent_with_the_session() {
        // Spelled out rather than defaulted: `SessionBody` has no `Default`, and giving it one
        // for a test would put a "session 0" into production code.
        fn reading(session_id: i64) -> crate::browser::SessionBody {
            crate::browser::SessionBody {
                session_id,
                changes_only: false,
                text_from: 0,
                controls_from: 0,
                find: String::new(),
            }
        }

        let (db, state, _) = wheeled(vec![], false).await;
        let session = a_session(&state).await;
        request(&state, session, "there is a login here")
            .await
            .expect("request");

        for (verb, response) in [
            (
                "snapshot",
                crate::browser::post_snapshot(
                    axum::extract::State(state.clone()),
                    axum::Json(reading(session)),
                )
                .await,
            ),
            (
                "screenshot",
                crate::browser::post_screenshot(
                    axum::extract::State(state.clone()),
                    axum::Json(reading(session)),
                )
                .await,
            ),
        ] {
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            let text = String::from_utf8_lossy(&body);
            assert!(
                text.contains("refused"),
                "{verb} answered a session that is not the agent's: {text}"
            );
            assert!(
                text.contains("theirs"),
                "{verb} refused without saying whose the screen is: {text}"
            );
        }
        db.close().await;
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

        let chain = give_back(&state, session, None).await.expect("give back");
        assert_eq!(chain.len(), 3);
        assert!(
            crate::browser::list_sites(&db.pool, "acme")
                .await
                .unwrap()
                .is_empty(),
            "handing the wheel back must not grant anything on its own"
        );

        let granted = keep(&state, session, true, false).await.expect("keep");
        assert_eq!(
            granted,
            vec![
                "https://jira.example.org:443",
                "https://accounts.google.com:443"
            ]
        );

        // And it can only be answered once: the permission belongs to the moment of the login.
        assert!(matches!(
            keep(&state, session, true, false).await,
            Err(WheelError::WrongState(_))
        ));
        db.close().await;
    }

    /// The second answer on the same screen: writing, granted where the login landed.
    ///
    /// It rides the same act as the read grant and reaches a strictly smaller set — the destination
    /// only. The identity provider in this chain is the assertion that matters: its forms are login
    /// forms, and those are exactly the forms an agent must never submit.
    #[tokio::test]
    async fn the_login_can_also_grant_writing_and_only_where_it_landed() {
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
        give_back(&state, session, None).await.expect("give back");

        keep(&state, session, true, true).await.expect("keep");

        let allowed = crate::browser::admitted_origins(&db.pool, "acme")
            .await
            .expect("allowed");
        assert_eq!(allowed.read.len(), 2, "both origins are readable");
        assert_eq!(
            allowed.write,
            vec!["https://jira.example.org:443"],
            "writing reached somewhere other than where the login landed"
        );
        db.close().await;
    }

    /// Saying no to the chain while ticking the write box grants nothing at all.
    ///
    /// Not rejected, because there is nothing to reject: there is no writing to a site the profile
    /// may not even load, and the early return in `keep` never reaches the grant. Asserted rather
    /// than reasoned about, because "impossible by construction" is a claim that has to be true of
    /// the code and not only of the paragraph describing it.
    #[tokio::test]
    async fn ticking_the_write_box_while_keeping_nothing_grants_nothing() {
        let (db, state, _) = wheeled(vec!["https://jira.example.org/login"], false).await;
        let session = a_session(&state).await;
        request(&state, session, "login").await.expect("request");
        accept(&state, session).await.expect("accept");
        give_back(&state, session, None).await.expect("give back");

        assert!(
            keep(&state, session, false, true)
                .await
                .expect("keep")
                .is_empty()
        );
        let allowed = crate::browser::admitted_origins(&db.pool, "acme")
            .await
            .expect("allowed");
        assert!(allowed.read.is_empty() && allowed.write.is_empty());
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
        give_back(&state, session, None).await.expect("give back");

        assert!(
            keep(&state, session, false, false)
                .await
                .expect("keep")
                .is_empty()
        );
        assert!(
            crate::browser::list_sites(&db.pool, "acme")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            keep(&state, session, true, false).await,
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
            give_back(&state, session, None).await,
            Err(WheelError::WrongState(_))
        ));
        // Keeping a chain that was never recorded.
        assert!(matches!(
            keep(&state, session, true, false).await,
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
        assert!(give_back(&state, row.id, None).await.is_ok());
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

    /// One open row for project `acme` on sidecar id `s1`, in the given mode and seat.
    async fn volante_row(
        state: &AppState,
        profile_kind: &str,
        row_mode: &str,
        seat: Option<&str>,
    ) -> i64 {
        let profile_id = if profile_kind == "project" {
            "acme"
        } else {
            "run-7"
        };
        sqlx::query(
            "INSERT INTO browser_sessions \
                (sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url, \
                 final_url, rule, mode, seat, opened_at) \
             VALUES ('s1', 7, 'acme', ?, ?, 'https://jira.example.org/login', \
                     'https://jira.example.org/login', 'project-site', ?, ?, \
                     '2026-08-16T10:00:00Z')",
        )
        .bind(profile_kind)
        .bind(profile_id)
        .bind(row_mode)
        .bind(seat)
        .execute(&state.pool)
        .await
        .expect("insert")
        .last_insert_rowid()
    }

    fn volante_saw(seen: &std::sync::Arc<std::sync::Mutex<Vec<String>>>, verb: &str) -> bool {
        seen.lock().unwrap().iter().any(|seen| seen == verb)
    }

    /// Spec 4.1: a lone project-profile session is offered the shell seat, and with no explicit
    /// choice the shell is what the approval picks. No second Chrome is launched for it.
    #[tokio::test]
    async fn volante_approval_on_the_project_profile_goes_shell_with_a_nonce() {
        let (db, state, seen) = wheeled(vec![], false).await;
        let id = volante_row(&state, "project", mode::WHEEL_REQUESTED, None).await;

        let (row, nonce) = accept_with_seat(&state, id, None).await.expect("accept");

        assert_eq!(row.mode, mode::HUMAN);
        assert_eq!(row.seat.as_deref(), Some("shell"));
        assert_eq!(
            row.sidecar_id, "s1",
            "the shell seat keeps the same browser"
        );
        let nonce = nonce.expect("a shell seat comes with a nonce");
        assert!(state.browser.seats.matches(id, &nonce));
        assert!(volante_saw(&seen, "person/begin"));
        assert!(
            !volante_saw(&seen, "wheel/take"),
            "no new window for the shell seat"
        );
        db.close().await;
    }

    /// A throwaway cannot be driven from the shell, so the default is today's real window.
    #[tokio::test]
    async fn volante_approval_on_a_throwaway_goes_window() {
        let (db, state, seen) = wheeled(vec![], false).await;
        let id = volante_row(&state, "ephemeral", mode::WHEEL_REQUESTED, None).await;

        let (row, nonce) = accept_with_seat(&state, id, None).await.expect("accept");

        assert_eq!(row.mode, mode::HUMAN);
        assert_eq!(row.seat.as_deref(), Some("window"));
        assert_eq!(nonce, None);
        assert!(volante_saw(&seen, "wheel/take"));
        assert!(!volante_saw(&seen, "person/begin"));
        db.close().await;
    }

    /// "Open real window" at approval time: the person chose a window although the shell was on
    /// offer.
    #[tokio::test]
    async fn volante_open_real_window_at_approval_goes_window() {
        let (db, state, seen) = wheeled(vec![], false).await;
        let id = volante_row(&state, "project", mode::WHEEL_REQUESTED, None).await;

        let (row, nonce) = accept_with_seat(&state, id, Some(SeatChoice::Window))
            .await
            .expect("accept");

        assert_eq!(row.mode, mode::HUMAN);
        assert_eq!(row.seat.as_deref(), Some("window"));
        assert_eq!(row.sidecar_id, "h1", "the row follows the person's window");
        assert_eq!(nonce, None);
        assert!(volante_saw(&seen, "wheel/take"));
        assert!(!volante_saw(&seen, "person/begin"));
        db.close().await;
    }

    /// The door from the shell seat to a real window: same mode, new seat, and the shell's nonce is
    /// forgotten so the shell can no longer drive.
    #[tokio::test]
    async fn volante_open_real_window_from_human_shell() {
        let (db, state, seen) = wheeled(vec![], false).await;
        let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
        let nonce = state.browser.seats.issue(id);

        let row = to_window(&state, id).await.expect("to window");

        assert_eq!(row.mode, mode::HUMAN);
        assert_eq!(row.seat.as_deref(), Some("window"));
        assert_eq!(row.sidecar_id, "h1");
        assert!(
            !state.browser.seats.matches(id, &nonce),
            "the nonce is forgotten"
        );
        assert!(volante_saw(&seen, "wheel/take"));

        // Only a human/shell session may move; one already in a window may not.
        assert!(matches!(
            to_window(&state, id).await,
            Err(WheelError::WrongState(_))
        ));
        db.close().await;
    }

    /// When the real window cannot open, the person is still on the shell seat, so the sidecar's
    /// fence is restored (best effort `person/end`) rather than left half moved.
    #[tokio::test]
    async fn volante_to_window_failure_restores_the_fence() {
        let (db, state, seen) = wheeled(vec![], true).await;
        let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
        state.browser.seats.issue(id);

        let outcome = to_window(&state, id).await;

        assert!(outcome.is_err(), "{outcome:?}");
        assert!(volante_saw(&seen, "wheel/take"));
        assert!(
            volante_saw(&seen, "person/end"),
            "the fence is restored after the failure: {:?}",
            seen.lock().unwrap()
        );
        db.close().await;
    }

    /// Spec 4.4a for the shell seat: when the person's stretch cannot begin it is a failed
    /// delivery, never a return to the agent.
    #[tokio::test]
    async fn volante_shell_approval_with_failed_begin_person_is_delivery_failed() {
        let (db, state, _) = wheeled_with(vec![], false, true).await;
        let id = volante_row(&state, "project", mode::WHEEL_REQUESTED, None).await;
        let proposal = crate::proposals::create_wheel_request(
            &state.pool,
            crate::proposals::WheelAsk {
                run_id: Some(7),
                project_id: "acme",
                session_id: id,
                requested_url: "https://jira.example.org/login",
                final_url: "https://jira.example.org/login",
                origin: "https://jira.example.org",
                reasoning: "login",
            },
        )
        .await
        .expect("proposal");
        sqlx::query("UPDATE browser_sessions SET proposal_id = ? WHERE id = ?")
            .bind(proposal)
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
        let events_before: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proposal_events WHERE proposal_id = ?")
                .bind(proposal)
                .fetch_one(&state.pool)
                .await
                .unwrap();

        let outcome = accept_with_seat(&state, id, Some(SeatChoice::Shell)).await;
        assert!(
            matches!(outcome, Err(WheelError::Sidecar(_))),
            "{outcome:?}"
        );

        let row = crate::browser::session_row(&state.pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.mode, mode::DELIVERY_FAILED);
        let events_after: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proposal_events WHERE proposal_id = ?")
                .bind(proposal)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(
            events_after > events_before,
            "the proposal carries the reason"
        );
        db.close().await;
    }

    // ---- returning the wheel: `to`, person-driving and wheel_returned (P6) ----------------------

    /// A browser runtime whose sidecar is not there: the port was bound and released, so every call
    /// is refused at the connection.
    async fn volante_dead_runtime() -> std::sync::Arc<crate::browser::BrowserRuntime> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        std::sync::Arc::new(crate::browser::BrowserRuntime {
            enabled: true,
            client: BrowserClient::new(&address.to_string(), "tok".into()),
            modes: Default::default(),
            seats: Default::default(),
        })
    }

    /// A browser runtime whose sidecar answers `/snapshot` with a page and everything else with 204.
    async fn volante_snapshot_runtime() -> std::sync::Arc<crate::browser::BrowserRuntime> {
        use axum::extract::Path;
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let app = axum::Router::new().route(
            "/{verb}",
            post(
                |Path(verb): Path<String>, _body: axum::body::Bytes| async move {
                    if verb == "snapshot" {
                        return axum::Json(serde_json::json!({
                            "session_id": "s1",
                            "url": "https://jira.example.org/login",
                        }))
                        .into_response();
                    }
                    axum::http::StatusCode::NO_CONTENT.into_response()
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        std::sync::Arc::new(crate::browser::BrowserRuntime {
            enabled: true,
            client: BrowserClient::new(&address.to_string(), "tok".into()),
            modes: Default::default(),
            seats: Default::default(),
        })
    }

    /// A minimal `runs` row with the id `volante_row` gives its session (7), in the given status.
    async fn volante_run(state: &AppState, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, prompt, status, created_at) \
             VALUES (7, 'browse', ?, '2026-08-16T10:00:00Z')",
        )
        .bind(status)
        .execute(&state.pool)
        .await
        .expect("insert run");
    }

    async fn volante_closed_reason(state: &AppState, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT closed_reason FROM browser_sessions WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn volante_response_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    /// Returning from the shell seat to the agent: the session stays open and goes back to the agent
    /// with no seat, but only once the sidecar has confirmed the person's stretch is over (the fence
    /// is restored). The chain is recorded and the keep question is open.
    #[tokio::test]
    async fn volante_return_to_agent_moves_the_row_only_after_person_end() {
        let (db, state, seen) = wheeled(vec!["https://jira.example.org/login"], false).await;
        let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
        volante_run(&state, "running").await;
        let nonce = state.browser.seats.issue(id);

        let chain = give_back(&state, id, Some("agent")).await.expect("return");

        assert_eq!(chain, vec!["https://jira.example.org/login".to_string()]);
        assert!(volante_saw(&seen, "person/end"));
        assert!(
            !volante_saw(&seen, "wheel/return"),
            "a shell seat has no window to give back"
        );
        let row = crate::browser::session_row(&state.pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.mode, mode::AGENT);
        assert_eq!(row.seat, None);
        assert_eq!(row.closed_at, None, "the session stays open for the agent");
        assert!(row.chain.is_some(), "the chain is recorded");
        assert_eq!(row.chain_decided_at, None, "the keep question is open");
        assert!(!state.browser.seats.matches(id, &nonce));
        assert!(state.browser.seats.take_returned(id));
        assert!(!state.browser.seats.take_returned(id), "announced once");
        db.close().await;
    }

    /// If the sidecar cannot end the person's stretch, the fence state is unknown, so the session is
    /// closed rather than handed back to the agent.
    #[tokio::test]
    async fn volante_return_to_agent_failed_person_end_closes_fence_not_restored() {
        let (db, mut state, _) = wheeled(vec![], false).await;
        state.browser = volante_dead_runtime().await;
        let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
        volante_run(&state, "running").await;
        let nonce = state.browser.seats.issue(id);

        let outcome = give_back(&state, id, Some("agent")).await;

        assert!(
            matches!(
                outcome,
                Err(WheelError::Sidecar(_)) | Err(WheelError::WrongState(_))
            ),
            "{outcome:?}"
        );
        let row = crate::browser::session_row(&state.pool, id)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(row.mode, mode::AGENT, "never back to the agent");
        assert!(row.closed_at.is_some());
        assert_eq!(
            volante_closed_reason(&state, id).await.as_deref(),
            Some("fence-not-restored")
        );
        assert!(!state.browser.seats.matches(id, &nonce));
        assert!(!state.browser.seats.take_returned(id));
        db.close().await;
    }

    /// `to=agent` with no run alive to receive the session (no run row, or one that is not running)
    /// closes it like a plain return.
    #[tokio::test]
    async fn volante_return_to_agent_without_a_live_run_closes() {
        for finished in [None, Some("failed")] {
            let (db, state, seen) = wheeled(vec!["https://jira.example.org/login"], false).await;
            let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
            if let Some(status) = finished {
                volante_run(&state, status).await;
            }
            let nonce = state.browser.seats.issue(id);

            let chain = give_back(&state, id, Some("agent")).await.expect("return");

            assert_eq!(chain, vec!["https://jira.example.org/login".to_string()]);
            assert!(volante_saw(&seen, "person/end"), "{finished:?}");
            let row = crate::browser::session_row(&state.pool, id)
                .await
                .unwrap()
                .unwrap();
            assert!(row.closed_at.is_some(), "{finished:?}");
            assert_ne!(row.mode, mode::AGENT, "{finished:?}");
            assert!(row.chain.is_some(), "the chain outlives the row");
            assert_eq!(
                volante_closed_reason(&state, id).await.as_deref(),
                Some("wheel-returned")
            );
            assert!(!state.browser.seats.matches(id, &nonce));
            db.close().await;
        }
    }

    /// `to=close`, an absent `to`, and the window seat all keep today's behaviour: the chain comes
    /// home and the row closes as `wheel-returned`. A shell seat gets its chain from `/person/end`.
    #[tokio::test]
    async fn volante_return_close_or_absent_keeps_todays_behaviour() {
        // Window seat, no `to`: the sidecar's wheel return, as before.
        let (db, state, seen) = wheeled(vec!["https://jira.example.org/login"], false).await;
        let id = volante_row(&state, "project", mode::HUMAN, Some("window")).await;
        let chain = give_back(&state, id, None).await.expect("return");
        assert_eq!(chain, vec!["https://jira.example.org/login".to_string()]);
        assert!(volante_saw(&seen, "wheel/return"));
        assert_eq!(
            volante_closed_reason(&state, id).await.as_deref(),
            Some("wheel-returned")
        );
        db.close().await;

        // Shell seat, `to=close` and no `to`, both with a live run: both close.
        for to in [Some("close"), None] {
            let (db, state, seen) = wheeled(vec!["https://jira.example.org/login"], false).await;
            let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
            volante_run(&state, "running").await;
            let chain = give_back(&state, id, to).await.expect("return");
            assert_eq!(chain, vec!["https://jira.example.org/login".to_string()]);
            assert!(volante_saw(&seen, "person/end"), "{to:?}");
            let row = crate::browser::session_row(&state.pool, id)
                .await
                .unwrap()
                .unwrap();
            assert!(row.closed_at.is_some(), "{to:?}");
            assert_eq!(
                volante_closed_reason(&state, id).await.as_deref(),
                Some("wheel-returned"),
                "{to:?}"
            );
            db.close().await;
        }

        // Shell seat, `to=close`, the sidecar cannot end the stretch: the chain is empty and the row
        // still closes as `wheel-returned`.
        let (db, mut state, _) = wheeled(vec![], false).await;
        state.browser = volante_dead_runtime().await;
        let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
        let chain = give_back(&state, id, Some("close")).await.expect("return");
        assert!(chain.is_empty());
        assert_eq!(
            volante_closed_reason(&state, id).await.as_deref(),
            Some("wheel-returned")
        );
        db.close().await;
    }

    /// A session can go to the person twice. Each return records a fresh chain and the keep question
    /// is asked again, even though the first one was answered.
    #[tokio::test]
    async fn volante_a_second_return_reopens_the_keep_question() {
        let (db, state, _) = wheeled(vec!["https://jira.example.org/login"], false).await;
        let id = volante_row(&state, "project", mode::HUMAN, Some("shell")).await;
        volante_run(&state, "running").await;

        give_back(&state, id, Some("agent")).await.expect("first");
        keep(&state, id, false, false).await.expect("first answer");
        assert!(
            matches!(
                keep(&state, id, true, false).await,
                Err(WheelError::WrongState(_))
            ),
            "answered once, not twice"
        );

        assert!(
            crate::browser::set_mode_seat(
                &state.pool,
                &state.browser.modes,
                id,
                mode::AGENT,
                mode::HUMAN,
                Some("shell"),
            )
            .await
            .unwrap()
        );
        give_back(&state, id, Some("agent")).await.expect("second");

        let granted = keep(&state, id, true, false)
            .await
            .expect("the second chain can be answered");
        assert!(!granted.is_empty(), "the second answer grants the chain");
        db.close().await;
    }

    /// While a person drives (either seat) the agent's refusal says `person-driving`; a requested
    /// wheel still says `wheel-requested`.
    #[tokio::test]
    async fn volante_person_driving_is_the_consequence_while_a_person_drives() {
        let (db, state, _) = wheeled(vec![], false).await;
        for (row_mode, seat, consequence) in [
            (mode::HUMAN, Some("shell"), "person-driving"),
            (mode::HUMAN, Some("window"), "person-driving"),
            (mode::WHEEL_REQUESTED, None, "wheel-requested"),
        ] {
            sqlx::query(
                "UPDATE browser_sessions SET closed_at = '2026-08-16T10:01:00Z' \
                 WHERE closed_at IS NULL",
            )
            .execute(&state.pool)
            .await
            .unwrap();
            let id = volante_row(&state, "project", row_mode, seat).await;

            let snapshot = volante_response_json(
                crate::browser::post_snapshot(
                    axum::extract::State(state.clone()),
                    axum::Json(crate::browser::SessionBody {
                        session_id: id,
                        changes_only: false,
                        text_from: 0,
                        controls_from: 0,
                        find: String::new(),
                    }),
                )
                .await,
            )
            .await;
            assert_eq!(snapshot["outcome"], "refused", "{row_mode} {seat:?}");
            assert_eq!(
                snapshot["refusal"]["consequence"], consequence,
                "snapshot, {row_mode} {seat:?}"
            );

            let act = volante_response_json(
                crate::browser::post_act(
                    axum::extract::State(state.clone()),
                    axum::Json(crate::browser::ActBody {
                        session_id: id,
                        kind: "click".into(),
                        element_ref: "e1".into(),
                        text: String::new(),
                        filename: String::new(),
                    }),
                )
                .await,
            )
            .await;
            assert_eq!(
                act["refusal"]["consequence"], consequence,
                "act, {row_mode} {seat:?}"
            );
        }
        db.close().await;
    }

    /// After the wheel comes back the agent's next snapshot carries `wheel_returned: true`, once,
    /// and the answer after that does not.
    #[tokio::test]
    async fn volante_wheel_returned_appears_once() {
        let (db, mut state, _) = wheeled(vec![], false).await;
        state.browser = volante_snapshot_runtime().await;
        let id = volante_row(&state, "project", mode::AGENT, None).await;
        let ask = |state: AppState| async move {
            volante_response_json(
                crate::browser::post_snapshot(
                    axum::extract::State(state),
                    axum::Json(crate::browser::SessionBody {
                        session_id: id,
                        changes_only: false,
                        text_from: 0,
                        controls_from: 0,
                        find: String::new(),
                    }),
                )
                .await,
            )
            .await
        };

        let before = ask(state.clone()).await;
        assert!(before.get("wheel_returned").is_none(), "{before}");

        state.browser.seats.mark_returned(id);
        let first = ask(state.clone()).await;
        assert_eq!(first["wheel_returned"], true, "{first}");
        assert_eq!(first["session_id"], "s1", "the snapshot itself is intact");

        let second = ask(state.clone()).await;
        assert!(second.get("wheel_returned").is_none(), "{second}");
        db.close().await;
    }
}
