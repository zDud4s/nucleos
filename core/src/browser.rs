//! The browser pillar's domain: profiles, the site lists, and the sessions opened in them.
//!
//! The only module that touches `browser_profiles`, `browser_sites` and `browser_sessions`. It owns
//! no decision — `browser_policy` is pure and decides — and it owns no transport, because
//! `browser_client` does. What lives here is everything in between: reading the list the decision
//! needs, recording what was opened and under which rule, and growing the list when a person logs in.
//!
//! # The list grows in exactly one way
//!
//! [`grant`] is the only function in this repository that writes to `browser_sites`, and it is
//! reached from one place: a person handing the wheel back after a login (spec §5.2). There is no
//! path into it from a YAML file, from an agent, or from a default. That is the invariant the whole
//! pillar rests on, and it is small enough to keep by inspection — which is why it is worth keeping
//! that way rather than adding a second writer that "only" adds a host it already knows about.
//!
//! # Why an open can happen twice
//!
//! The profile has to be chosen BEFORE the page runs (spec §5.4), and the destination after a
//! redirect is not knowable before it. So the first open is decided on the requested url alone. If
//! the session lands somewhere the project does not admit, the fence has already refused the
//! document — nothing ran — and this module reopens the same request in a throwaway. That is spec
//! §5.4's "the url is handed to an ephemeral session", and it is a downgrade rather than a failure.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::browser_client::{BrowserClient, BrowserError, Placement, Session};
use crate::browser_policy::{self, Outcome, Profile, Requester, Surface};
use crate::state::AppState;

/// What the daemon carries for this pillar. Built once at startup from `.ai/browser.yaml`.
#[derive(Debug)]
pub struct BrowserRuntime {
    /// Spec §14.2: the pillar ships off and stays off until the fence, the profiles, the handoff and
    /// the tools are green together.
    pub enabled: bool,
    pub client: BrowserClient,
}

impl BrowserRuntime {
    /// The pillar, off. Named rather than derived, for `WebRuntime::disabled`'s reason: a derived
    /// default would invent a client pointing at nothing, and "off" should be a state somebody chose.
    ///
    /// `#[cfg(test)]` because production always builds a real one from `.ai/browser.yaml`; this is
    /// what the `test_state()` fixtures hold.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            client: BrowserClient::new(crate::sidecar::BROWSER_ADDR, String::new()),
        }
    }
}

/// One origin a project's profile admits, as it is stored.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Site {
    pub origin: String,
    /// `destination` — somewhere the person chose to log in — or `idp`, a host their login passed
    /// through. Kept apart so a person revoking access can see which is which (spec §5.3a).
    pub kind: String,
    pub granted_at: String,
    /// The destination whose login brought this origin in, or `None` when this is the destination.
    pub granted_for: Option<String>,
}

/// One browsing session as the núcleo remembers it.
#[derive(Debug, Clone, Serialize)]
pub struct SessionRow {
    pub id: i64,
    pub sidecar_id: String,
    pub run_id: Option<i64>,
    /// The project this session was opened FOR — not necessarily the one whose profile it runs in.
    /// A throwaway belongs to no project, but the session that used it was still asked for by one,
    /// and spec §4.5's handover needs to know which.
    pub project_id: Option<String>,
    pub profile_kind: String,
    pub profile_id: String,
    pub requested_url: String,
    pub final_url: String,
    pub rule: String,
    /// `agent`, `wheel-requested`, `human` or `delivery-failed` — spec §4.4's state machine.
    pub mode: String,
    pub refusal: Option<String>,
    /// The proposal that asked for the wheel, once one exists (spec §4.4 rule 3).
    pub proposal_id: Option<i64>,
    /// The navigation the person's window recorded, as JSON, once the wheel has come back.
    pub chain: Option<String>,
    /// When the person answered "keep these?" — either way. `None` with a `chain` present means the
    /// question is still open, which is exactly what the UI needs to know to ask it.
    pub chain_decided_at: Option<String>,
    pub opened_at: String,
    pub closed_at: Option<String>,
}

/// The states of spec §4.4, spelled once.
pub mod mode {
    /// The agent drives: headless, fenced, consequence-free.
    pub const AGENT: &str = "agent";
    /// The agent has asked for the wheel. Its actions are refused from HERE, not from the window
    /// opening (spec §4.4 rule 1).
    pub const WHEEL_REQUESTED: &str = "wheel-requested";
    /// A person is driving a real window.
    pub const HUMAN: &str = "human";
    /// They accepted and the headful browser would not start (spec §4.4a). Not a way back to AGENT.
    pub const DELIVERY_FAILED: &str = "delivery-failed";
}

/// What an open produced.
#[derive(Debug)]
pub enum Opened {
    /// A session exists. It may still carry a refusal — the fence stopped the navigation and the
    /// session is empty — which is a value the agent reads, not an error (spec §6.2).
    Session(Box<SessionRow>),
    /// No session, and the reason is one nobody can browse their way around: the pillar is off, the
    /// reach is undesigned, or there is nobody present to take the wheel.
    Refused { rule: String, recoverable: bool },
}

/// The origins a project's profile admits, in the shape `browser_policy::decide` takes.
pub async fn admitted_origins(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<String>> {
    let rows = sqlx::query("SELECT origin FROM browser_sites WHERE project_id = ? ORDER BY origin")
        .bind(project_id)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>("origin"))
        .collect())
}

/// The same list with everything a person needs to decide whether to revoke one.
pub async fn list_sites(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<Site>> {
    let rows = sqlx::query(
        "SELECT origin, kind, granted_at, granted_for FROM browser_sites \
         WHERE project_id = ? ORDER BY kind DESC, origin",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Site {
            origin: row.get("origin"),
            kind: row.get("kind"),
            granted_at: row.get("granted_at"),
            granted_for: row.get("granted_for"),
        })
        .collect())
}

/// Grant a login's whole chain to a project's profile, as one set.
///
/// This is spec §5.2 and §5.3a, and it is the only writer of `browser_sites`. Two things about the
/// shape are load-bearing:
///
/// - **The chain is granted together or not at all.** A login crosses hosts — the destination, then
///   an identity provider, then back — and granting them one at a time would mean a login abandoned
///   halfway had already produced a permanent permission. One transaction, one human act.
/// - **The destination is whichever host the chain ended on**, and the rest are identity providers.
///   Not a guess: the person was driving, and where they ended up is where they logged in.
///
/// `chain` is the navigation the headful window recorded. `browser_policy::granted_origins`
/// normalises it and drops anything that is not an https origin, so what reaches the table has
/// already been through the same rules the decision uses.
// Unreachable until the handoff lands (spec §4.4): the wheel coming back is the one act that calls
// this. Named on the function rather than on the module, so it is the only thing here that may be
// unreached — and so this line has to be deleted by the phase that adds the caller.
#[allow(dead_code)]
pub async fn grant(
    pool: &SqlitePool,
    project_id: &str,
    chain: &[String],
    now: &str,
) -> sqlx::Result<Vec<String>> {
    let origins = browser_policy::granted_origins(chain);
    // The destination is the last admissible step of the chain, and NOT the last element of
    // `origins`. A real login returns to where it started — jira, google, jira — and `granted_origins`
    // deduplicates by first occurrence, so the deduplicated list ends on the identity provider. Taking
    // it from there would file the person's actual destination as an IdP and the IdP as the
    // destination, which is exactly backwards on the screen where they later revoke one.
    let reversed = chain.iter().rev().cloned().collect::<Vec<_>>();
    let Some(destination) = browser_policy::granted_origins(&reversed)
        .into_iter()
        .next()
    else {
        return Ok(Vec::new());
    };

    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO browser_profiles (project_id, created_at, last_used_at) VALUES (?, ?, ?) \
         ON CONFLICT(project_id) DO UPDATE SET last_used_at = excluded.last_used_at",
    )
    .bind(project_id)
    .bind(now)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    for origin in &origins {
        let (kind, granted_for) = if origin == &destination {
            ("destination", None)
        } else {
            ("idp", Some(destination.as_str()))
        };
        // ON CONFLICT DO NOTHING rather than UPDATE: a host granted earlier keeps the date and the
        // reason it was granted for. Overwriting them would rewrite the audit trail of a permission
        // every time somebody logged in again.
        sqlx::query(
            "INSERT INTO browser_sites (project_id, origin, kind, granted_at, granted_for) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT(project_id, origin) DO NOTHING",
        )
        .bind(project_id)
        .bind(origin)
        .bind(kind)
        .bind(now)
        .bind(granted_for)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(origins)
}

/// Withdraw one origin from a project's profile.
///
/// Only the row. The cookies for that host stay in the profile on disk until the profile itself is
/// discarded, and saying otherwise would be a promise this function cannot keep — what it does keep
/// is that no document from that origin loads in the profile again.
pub async fn revoke(pool: &SqlitePool, project_id: &str, origin: &str) -> sqlx::Result<bool> {
    // Normalised through the same rule that stored it. An origin arrives here from a screen, and the
    // stored form carries its port explicitly (`https://host:443`) because the port is part of the
    // identity — so a caller passing the shorter spelling of the same origin would otherwise get a
    // silent no-op, on the one screen where "nothing happened" and "access withdrawn" look alike.
    let normalised = browser_policy::granted_origins(std::slice::from_ref(&origin.to_string()));
    let Some(origin) = normalised.first() else {
        return Ok(false);
    };
    let result = sqlx::query("DELETE FROM browser_sites WHERE project_id = ? AND origin = ?")
        .bind(project_id)
        .bind(origin)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Everything about one request to open a session, apart from where it will be sent.
///
/// A struct rather than six arguments, and the grouping is the point: `surface` and `requester` are
/// two questions that must be asked together (`browser_policy::decide`), and a signature long enough
/// to want a struct is also long enough for two same-typed arguments to be swapped without the
/// compiler noticing.
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    pub project_id: &'a str,
    /// The run this session belongs to, when there is one. It names the throwaway profile, so every
    /// session of a run shares one browser and one directory to delete at the end of it.
    pub run_id: Option<i64>,
    pub url: &'a str,
    pub surface: Surface,
    pub requester: Requester,
    pub now: &'a str,
}

/// Open a session on a url, in whichever profile the policy chooses.
pub async fn open(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    ask: Ask<'_>,
) -> Result<Opened, BrowserError> {
    let Ask {
        project_id,
        run_id,
        url,
        surface,
        requester,
        now,
    } = ask;
    if !runtime.enabled {
        return Ok(Opened::Refused {
            rule: "pillar-disabled".to_string(),
            recoverable: true,
        });
    }

    let sites = admitted_origins(pool, project_id)
        .await
        .map_err(|error| BrowserError::Failed(error.to_string()))?;

    // Decided on the requested url alone, because the profile has to be chosen before the page runs
    // and the destination after a redirect is not knowable yet. The redirect case is handled below,
    // after the fence has already refused the document.
    let decision = browser_policy::decide(url, url, surface, requester, &sites);
    let profile = match decision.outcome {
        Outcome::Open(profile) => profile,
        Outcome::Refused { recoverable } => {
            return Ok(Opened::Refused {
                rule: decision.rule.to_string(),
                recoverable,
            });
        }
    };

    let row_id = insert_session(pool, run_id, project_id, url, decision.rule, profile, now)
        .await
        .map_err(|error| BrowserError::Failed(error.to_string()))?;
    let placement = placement_for(profile, project_id, run_id, row_id, &sites);

    let session = match runtime.client.open(url, &placement).await {
        Ok(session) => session,
        Err(error) => {
            // The row exists and no session does. Closing it here rather than leaving it to the
            // sweeper keeps "open" in this table meaning "open over there".
            let _ = close_row(pool, row_id, "open-failed", now).await;
            return Err(error);
        }
    };

    // The redirect trap, closed after the fact because it cannot be closed before it. If the request
    // was placed in the project profile and landed off the list, the fence refused the document —
    // nothing from that host ran in the profile — and the url is handed to a throwaway instead.
    if profile == Profile::Project {
        let after = browser_policy::decide(url, &session.final_url, surface, requester, &sites);
        if matches!(after.outcome, Outcome::Open(Profile::Ephemeral)) {
            let _ = runtime.client.close(&session.id).await;
            let _ = close_row(pool, row_id, after.rule, now).await;
            return reopen_ephemeral(pool, runtime, ask, after.rule).await;
        }
    }

    let row = finish_session(pool, row_id, &placement, &session)
        .await
        .map_err(|error| BrowserError::Failed(error.to_string()))?;
    Ok(Opened::Session(Box::new(row)))
}

/// The second half of the redirect downgrade: the same request, in a profile with nothing to lose.
///
/// It reopens on the REQUESTED url rather than the final one. Jumping straight to the destination
/// would skip whatever the redirect does — a session hand-off, a cookie, a consent step — and
/// produce a page the person would not recognise from having followed the link themselves.
async fn reopen_ephemeral(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    ask: Ask<'_>,
    rule: &'static str,
) -> Result<Opened, BrowserError> {
    let row_id = insert_session(
        pool,
        ask.run_id,
        ask.project_id,
        ask.url,
        rule,
        Profile::Ephemeral,
        ask.now,
    )
    .await
    .map_err(|error| BrowserError::Failed(error.to_string()))?;
    let placement = placement_for(Profile::Ephemeral, ask.project_id, ask.run_id, row_id, &[]);

    let session = match runtime.client.open(ask.url, &placement).await {
        Ok(session) => session,
        Err(error) => {
            let _ = close_row(pool, row_id, "open-failed", ask.now).await;
            return Err(error);
        }
    };
    let row = finish_session(pool, row_id, &placement, &session)
        .await
        .map_err(|error| BrowserError::Failed(error.to_string()))?;
    Ok(Opened::Session(Box::new(row)))
}

/// Which profile goes on the wire.
///
/// An ephemeral profile is named after the run when there is one, so every session of a run shares
/// one throwaway browser and one directory to delete at the end of it. When there is no run — an
/// assistant turn on its own — it is named after this session row, which is unique by construction
/// and traceable back to the row that opened it.
fn placement_for(
    profile: Profile,
    project_id: &str,
    run_id: Option<i64>,
    row_id: i64,
    sites: &[String],
) -> Placement {
    match profile {
        Profile::Project => Placement::project(project_id, sites.to_vec()),
        Profile::Ephemeral => match run_id {
            Some(run) => Placement::ephemeral(&format!("r{run}")),
            None => Placement::ephemeral(&format!("s{row_id}")),
        },
    }
}

/// Insert the row BEFORE the sidecar is called.
///
/// Two reasons, and the second is the one that matters. It gives the ephemeral profile a unique name
/// for the runless case — and it means a daemon that dies mid-open leaves a row that says so, rather
/// than a browser nothing in this database has ever heard of.
async fn insert_session(
    pool: &SqlitePool,
    run_id: Option<i64>,
    project_id: &str,
    url: &str,
    rule: &str,
    profile: Profile,
    now: &str,
) -> sqlx::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO browser_sessions \
           (sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url, final_url, \
            rule, mode, opened_at) \
         VALUES ('', ?, ?, ?, '', ?, '', ?, 'agent', ?)",
    )
    .bind(run_id)
    // The project the session was opened FOR, whichever profile it lands in. Spec §4.5 needs it for
    // the throwaway case above all: a wheel request from a throwaway is a request to establish a
    // session in the project, and without this the handover would have no project to establish it in.
    .bind(project_id)
    .bind(profile.as_str())
    .bind(url)
    .bind(rule)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Fill in what only exists after the sidecar answered.
///
/// The profile id comes from the PLACEMENT and not from the session: the sidecar does not report a
/// profile back, and it should not have to. The núcleo decided where this runs, so the núcleo is
/// what records it — a sidecar that answered with a different profile than the one it was sent would
/// be a sidecar to stop trusting, not a source to read from.
async fn finish_session(
    pool: &SqlitePool,
    row_id: i64,
    placement: &Placement,
    session: &Session,
) -> sqlx::Result<SessionRow> {
    sqlx::query(
        "UPDATE browser_sessions SET sidecar_id = ?, final_url = ?, profile_id = ?, refusal = ?, \
         mode = ? WHERE id = ?",
    )
    .bind(&session.id)
    .bind(&session.final_url)
    .bind(&placement.profile.id)
    .bind(session.refusal.as_ref().map(|refusal| &refusal.consequence))
    .bind(&session.mode)
    .bind(row_id)
    .execute(pool)
    .await?;
    session_row(pool, row_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn session_row(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<SessionRow>> {
    let row = sqlx::query(
        "SELECT id, sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url, \
                final_url, rule, mode, refusal, proposal_id, chain, chain_decided_at, \
                opened_at, closed_at \
         FROM browser_sessions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| SessionRow {
        id: row.get("id"),
        sidecar_id: row.get("sidecar_id"),
        run_id: row.get("run_id"),
        project_id: row.get("project_id"),
        profile_kind: row.get("profile_kind"),
        profile_id: row.get("profile_id"),
        requested_url: row.get("requested_url"),
        final_url: row.get("final_url"),
        rule: row.get("rule"),
        mode: row.get("mode"),
        refusal: row.get("refusal"),
        proposal_id: row.get("proposal_id"),
        chain: row.get("chain"),
        chain_decided_at: row.get("chain_decided_at"),
        opened_at: row.get("opened_at"),
        closed_at: row.get("closed_at"),
    }))
}

// ---------------------------------------------------------------------------------------------
// The wheel's writes. The state machine that drives them is `browser_wheel`, which owns no SQL —
// these stay here so `browser_sessions` keeps having exactly one module that touches it.
// ---------------------------------------------------------------------------------------------

/// Move a session between the states of spec §4.4, and only from the state the caller expected.
///
/// Compare-and-set, and this is the atomic write rule 1 rests on: "a transição para fora de
/// `agente_conduz` é a mesma escrita atómica que regista a aceitação". A read-then-write would leave
/// a window in which two callers both believed they had the wheel — and one of them would be an
/// agent clicking on a page a person had just been handed.
pub async fn set_mode(pool: &SqlitePool, id: i64, from: &str, to: &str) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE browser_sessions SET mode = ? WHERE id = ? AND mode = ? AND closed_at IS NULL",
    )
    .bind(to)
    .bind(id)
    .bind(from)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Point a session at the proposal that asked for its wheel.
pub async fn attach_proposal(pool: &SqlitePool, id: i64, proposal_id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE browser_sessions SET proposal_id = ? WHERE id = ?")
        .bind(proposal_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Replace the sidecar's id for a session, which the handover changes.
///
/// The headful window is a different browser and a different session over there; here it is the same
/// row, because it is the same request by the same run for the same page. Keeping one row is what
/// lets the UI show a handover as something that happened TO a session rather than as two unrelated
/// ones.
pub async fn rebind_sidecar(
    pool: &SqlitePool,
    id: i64,
    sidecar_id: &str,
    profile_id: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE browser_sessions SET sidecar_id = ?, profile_kind = 'project', \
         profile_id = ? WHERE id = ?",
    )
    .bind(sidecar_id)
    .bind(profile_id)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Open a row for a window a PERSON asked for, in `human` from the first instant.
///
/// Every other row in this table starts in `agent` and may become `human` by the handover of spec
/// §4.4. This one never was the agent's, and the difference is not bookkeeping: the whole of §4.4 —
/// the proposal, the punycode origin, the "who asked and how they got there" — exists because an
/// AGENT chose the destination while holding a stranger's words in its context. Here the person
/// typed it. There is no deputy to confuse, so there is nothing to approve, and a row that started
/// in `agent` would have to be walked through a state machine whose reason for existing is absent.
///
/// `rule` says `person-opened` for the same reason the others say why they landed where they did:
/// the placement here is not the outcome of `browser_policy::decide`, and a row claiming a rule that
/// never ran would be the kind of record that reads as evidence and is not.
pub async fn insert_person_window(
    pool: &SqlitePool,
    project_id: &str,
    url: &str,
    now: &str,
) -> sqlx::Result<i64> {
    let result = sqlx::query(
        "INSERT INTO browser_sessions \
           (sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url, final_url, \
            rule, mode, opened_at) \
         VALUES ('', NULL, ?, 'project', '', ?, '', 'person-opened', 'human', ?)",
    )
    .bind(project_id)
    .bind(url)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.last_insert_rowid())
}

/// Store the navigation a person's window recorded, unanswered.
pub async fn record_chain(pool: &SqlitePool, id: i64, chain: &[String]) -> sqlx::Result<()> {
    sqlx::query("UPDATE browser_sessions SET chain = ? WHERE id = ?")
        .bind(serde_json::to_string(chain).unwrap_or_else(|_| "[]".to_string()))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Take the recorded chain, once, and stamp the moment it was answered.
///
/// Once, because the permission belongs to the moment of the login (spec §5.2). A chain that could
/// be answered twice could be answered a week later by somebody who had forgotten what it was for,
/// and the answer grants a permanent right to load a host inside the profile holding the logins.
/// `chain_decided_at IS NULL` in the WHERE is what makes that impossible rather than unlikely.
pub async fn take_chain(
    pool: &SqlitePool,
    id: i64,
    now: &str,
) -> sqlx::Result<Option<Vec<String>>> {
    let row = sqlx::query(
        "UPDATE browser_sessions SET chain_decided_at = ? \
         WHERE id = ? AND chain IS NOT NULL AND chain_decided_at IS NULL \
         RETURNING chain",
    )
    .bind(now)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let chain: String = row.get("chain");
    Ok(Some(
        serde_json::from_str(&chain).unwrap_or_else(|_| Vec::new()),
    ))
}

/// Forget a project's profile: the directory, the site list, and the record that it existed.
///
/// Spec §10's "Esquecer", and the counterweight the design needs rather than a convenience. The site
/// list grows only by a person finishing a login (§5.2), so it grows for ever unless there is a way
/// back — and what it grows by is a permanent right to load a host inside a profile holding live
/// session cookies. What is given has to be removable, in the same place.
///
/// The order is deliberate and the failure is not fatal. The directory goes first, because that is
/// where the cookies are and deleting the rows while the cookies stayed would be the dangerous half
/// of the job done last. If the sidecar cannot be reached the rows still go: what remains then is a
/// directory nobody has a list for, which the next `Admit` sweep and the ceiling both account for,
/// and which no session can be placed into because the decision reads this table.
pub async fn forget(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    project_id: &str,
) -> sqlx::Result<Vec<String>> {
    let placement = Placement::project(project_id, Vec::new());
    let now = chrono::Utc::now().to_rfc3339();
    let stopped = match runtime.client.forget(&placement).await {
        Ok(stopped) => stopped,
        Err(error) => {
            tracing::warn!(project = project_id, %error, "the profile directory was not deleted");
            Vec::new()
        }
    };
    for sidecar_id in &stopped {
        let _ = sqlx::query(
            "UPDATE browser_sessions SET closed_at = ?, closed_reason = 'profile-forgotten' \
             WHERE sidecar_id = ? AND closed_at IS NULL",
        )
        .bind(&now)
        .bind(sidecar_id)
        .execute(pool)
        .await;
    }

    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM browser_sites WHERE project_id = ?")
        .bind(project_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM browser_profiles WHERE project_id = ?")
        .bind(project_id)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(stopped)
}

/// Close a session's row from outside this module's own paths — the wheel's failures need it.
pub async fn close_session_row(
    pool: &SqlitePool,
    id: i64,
    reason: &str,
    now: &str,
) -> sqlx::Result<()> {
    close_row(pool, id, reason, now).await
}

/// The open sessions, oldest first.
pub async fn open_sessions(pool: &SqlitePool) -> sqlx::Result<Vec<SessionRow>> {
    let rows =
        sqlx::query("SELECT id FROM browser_sessions WHERE closed_at IS NULL ORDER BY opened_at")
            .fetch_all(pool)
            .await?;
    let mut sessions = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(session) = session_row(pool, row.get::<i64, _>("id")).await? {
            sessions.push(session);
        }
    }
    Ok(sessions)
}

/// Close one session, over there and here.
pub async fn close(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    id: i64,
    now: &str,
) -> Result<bool, BrowserError> {
    let Some(row) = session_row(pool, id)
        .await
        .map_err(|error| BrowserError::Failed(error.to_string()))?
    else {
        return Ok(false);
    };
    if row.closed_at.is_some() {
        return Ok(false);
    }
    // The sidecar first, and its failure is not fatal here. A session it has already forgotten —
    // because it restarted — must still be closed in this table, or it stays open for ever.
    let sidecar = runtime.client.close(&row.sidecar_id).await;
    close_row(pool, id, "closed", now)
        .await
        .map_err(|error| BrowserError::Failed(error.to_string()))?;
    match sidecar {
        Ok(()) | Err(BrowserError::NoSuchSession(_)) => Ok(true),
        Err(error) => Err(error),
    }
}

/// Retire every open session, because the process holding them went away.
///
/// Spec §9.1: the sidecar restarting costs every live session and nothing durable. What would be
/// lost without this is not data but truth — rows saying "open" about a browser that no longer
/// exists, which is worse than saying nothing.
pub async fn retire_open_sessions(pool: &SqlitePool, now: &str) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE browser_sessions SET closed_at = ?, closed_reason = 'sidecar-restarted' \
         WHERE closed_at IS NULL",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

async fn close_row(pool: &SqlitePool, id: i64, reason: &str, now: &str) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE browser_sessions SET closed_at = ?, closed_reason = ? \
         WHERE id = ? AND closed_at IS NULL",
    )
    .bind(now)
    .bind(reason)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// The handlers. `/browser/*`, mounted in `http.rs`.
//
// None of them takes a `requester` or a `surface` from the request body. Both are derived here, for
// `web.rs`'s reason and one more: the shell and an agent authenticate with the same token, so a
// field on the wire saying which one is calling would be a permission the caller grants itself —
// and the permission in question is whether a page may run inside the profile that holds the
// owner's logins.
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OpenBody {
    pub project_id: String,
    pub url: String,
    #[serde(default)]
    pub run_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SessionBody {
    pub session_id: i64,
    /// Only meaningful to `/browser/snapshot`; harmless on the others, which is why they share a
    /// body rather than growing a second one that differs by a single optional field.
    #[serde(default)]
    pub changes_only: bool,
    /// Where to resume a page's prose, for a snapshot that came back truncated. Same story: only
    /// `/browser/snapshot` reads it.
    #[serde(default)]
    pub text_from: i64,
    /// The same, for the actionable elements, which are bounded apart from the prose.
    #[serde(default)]
    pub controls_from: i64,
    /// Keep only what says this. Also snapshot-only, and the one field here that changes what is
    /// READ rather than how much of it: a search answers a question about a page without the page
    /// being carried to answer it.
    #[serde(default)]
    pub find: String,
}

#[derive(Debug, Deserialize)]
pub struct ActBody {
    pub session_id: i64,
    pub kind: String,
    #[serde(rename = "ref")]
    pub element_ref: String,
    #[serde(default)]
    pub text: String,
}

/// `POST /browser/open`.
pub async fn post_open(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<OpenBody>,
) -> axum::response::Response {
    let now = chrono::Utc::now();
    let ask = Ask {
        project_id: &body.project_id,
        run_id: body.run_id,
        url: &body.url,
        // The assistant, always, in this version. Spec §6.0b: the autonomous path is a seam and not
        // a road, and the seam is `browser_policy`'s refusal rather than a branch here. When a
        // pillar does reach the browser, it will arrive through its own caller and name itself.
        surface: Surface::Assistant,
        requester: requester_now(&state, now).await,
        now: &now.to_rfc3339(),
    };
    match open(&state.pool, &state.browser, ask).await {
        Ok(Opened::Session(row)) => axum::Json(row).into_response(),
        Ok(Opened::Refused { rule, recoverable }) => (
            // 409 rather than 403: nothing about the credentials is wrong. The request cannot be
            // carried out in the state the machine is in, and `recoverable` says whether that state
            // is one the person can change.
            StatusCode::CONFLICT,
            axum::Json(serde_json::json!({
                "refused": rule,
                "recoverable": recoverable,
            })),
        )
            .into_response(),
        Err(error) => browser_error(error),
    }
}

/// `POST /browser/snapshot`.
pub async fn post_snapshot(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<SessionBody>,
) -> axum::response::Response {
    let Some(row) = live_session(&state, body.session_id).await else {
        return gone();
    };
    match state
        .browser
        .client
        .snapshot(
            &row.sidecar_id,
            body.changes_only,
            body.text_from,
            body.controls_from,
            &body.find,
        )
        .await
    {
        Ok(snapshot) => axum::Json(snapshot).into_response(),
        Err(error) => browser_error(error),
    }
}

/// `POST /browser/act`.
///
/// A refusal by the fence comes back as 200 carrying `outcome: "refused"`, all the way from the
/// sidecar. Mapping it to a 4xx here would undo the one property the whole surface is arranged
/// around (spec §6.2).
pub async fn post_act(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<ActBody>,
) -> axum::response::Response {
    let Some(row) = live_session(&state, body.session_id).await else {
        return gone();
    };
    // Spec §4.4 rule 1, on this side. From the moment the wheel is ASKED for — not from the window
    // opening — the agent's actions are refused and not queued. The sidecar refuses them too, and
    // neither layer is redundant: this one holds when the two processes disagree about who is
    // driving, which is the state a crash between the request and the handover produces.
    //
    // A refusal and not an error, in the shape the agent already knows how to read (§6.2).
    if row.mode != mode::AGENT {
        return axum::Json(serde_json::json!({
            "outcome": "refused",
            "refusal": {
                "consequence": "wheel-requested",
                "detail": format!("this session is {}, so it is not the agent's to act on", row.mode),
            },
        }))
        .into_response();
    }
    match state
        .browser
        .client
        .act(&row.sidecar_id, &body.kind, &body.element_ref, &body.text)
        .await
    {
        Ok(result) => {
            if result.refused() {
                // Worth a line in the daemon's log, and only a line: it is an ordinary answer, not a
                // fault. What it buys is that a person reading back a run can see the fence acted,
                // rather than inferring it from a page that did not change.
                tracing::info!(
                    session = row.id,
                    consequence = result
                        .refusal
                        .as_ref()
                        .map(|refusal| refusal.consequence.as_str())
                        .unwrap_or_default(),
                    "the fence refused an action"
                );
            }
            axum::Json(result).into_response()
        }
        Err(error) => browser_error(error),
    }
}

/// `POST /browser/screenshot` — pixels, for a person to look at.
///
/// It answers to the shell and not to the agent. Spec §3.5 records why: `filter_outgoing` in
/// `mcp_tools.rs` redacts text and has never had an image branch, so a screenshot of the owner's
/// authenticated session handed to a model would leave the machine without passing the redaction
/// every other answer goes through.
pub async fn post_screenshot(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<SessionBody>,
) -> axum::response::Response {
    let Some(row) = live_session(&state, body.session_id).await else {
        return gone();
    };
    match state.browser.client.screenshot(&row.sidecar_id).await {
        Ok(image) => ([(axum::http::header::CONTENT_TYPE, "image/png")], image).into_response(),
        Err(error) => browser_error(error),
    }
}

/// `POST /browser/close`.
pub async fn post_close(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<SessionBody>,
) -> axum::response::Response {
    let now = chrono::Utc::now().to_rfc3339();
    match close(&state.pool, &state.browser, body.session_id, &now).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => gone(),
        Err(error) => browser_error(error),
    }
}

/// `GET /browser/sessions` — what is open right now.
pub async fn list_open_sessions(State(state): State<AppState>) -> axum::response::Response {
    match open_sessions(&state.pool).await {
        Ok(sessions) => axum::Json(sessions).into_response(),
        Err(error) => db_error(error),
    }
}

/// `GET /browser/sites/{project_id}` — where this project has logged in.
///
/// Admin, like everything else here and unlike `GET /web/pages` beside it. The list of hosts a
/// person has accounts on is a map of their working life, and a read-only key exists to be handed
/// to something less trusted than the shell.
pub async fn get_sites(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    match list_sites(&state.pool, &project_id).await {
        Ok(sites) => axum::Json(sites).into_response(),
        Err(error) => db_error(error),
    }
}

#[derive(Debug, Deserialize)]
pub struct RevokeBody {
    pub project_id: String,
    pub origin: String,
}

/// `POST /browser/revoke` — withdraw one origin from a project's profile.
///
/// There is no matching grant route, and its absence is the invariant: the list grows when a person
/// finishes a login and hands the wheel back (spec §5.2), which is a different act in a different
/// place. A `POST /browser/grant` would be a way to add a host by asking, and everything above rests
/// on there not being one.
pub async fn post_revoke(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<RevokeBody>,
) -> axum::response::Response {
    match revoke(&state.pool, &body.project_id, &body.origin).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "no such site").into_response(),
        Err(error) => db_error(error),
    }
}

#[derive(Debug, Deserialize)]
pub struct ForgetBody {
    pub project_id: String,
}

/// `POST /browser/forget` — delete a project's profile and everything granted to it.
///
/// The only route on this surface that destroys something a person made, and the reason it exists is
/// spec §10's: "o que se dá tem de se poder tirar, no mesmo sítio". A revocation screen with no way
/// to take back the whole profile would leave the cookies behind after the list said they were gone.
pub async fn post_forget(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<ForgetBody>,
) -> axum::response::Response {
    match forget(&state.pool, &state.browser, &body.project_id).await {
        Ok(stopped) => axum::Json(serde_json::json!({ "stopped": stopped.len() })).into_response(),
        Err(error) => db_error(error),
    }
}

/// Presence, read here and never taken from the caller.
async fn requester_now(state: &AppState, now: chrono::DateTime<chrono::Utc>) -> Requester {
    if crate::attention::owner_is_present(&state.pool, now).await {
        Requester::Owner
    } else {
        Requester::Autonomous
    }
}

/// The session row, if it is still open. A closed one is not addressable: the browser behind it is
/// gone, and answering from the row would describe a page that no longer exists.
async fn live_session(state: &AppState, id: i64) -> Option<SessionRow> {
    match session_row(&state.pool, id).await {
        Ok(Some(row)) if row.closed_at.is_none() => Some(row),
        _ => None,
    }
}

fn gone() -> axum::response::Response {
    (StatusCode::NOT_FOUND, "no such browsing session").into_response()
}

fn db_error(error: sqlx::Error) -> axum::response::Response {
    tracing::error!(%error, "browser database error");
    (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
}

/// The sidecar's failures, in the grades a caller has to tell apart.
///
/// `FenceDown` is 503 and not 500 for the reason spec §6.2a gives: nothing is broken, browsing is
/// simply not available, and a 500 reads as a crash and invites the retry loop that would run
/// against an unfenced browser.
fn browser_error(error: BrowserError) -> axum::response::Response {
    let status = match error {
        BrowserError::Unreachable(_) => StatusCode::BAD_GATEWAY,
        BrowserError::FenceDown(_) => StatusCode::SERVICE_UNAVAILABLE,
        BrowserError::NoSuchSession(_) => StatusCode::NOT_FOUND,
        BrowserError::BadRequest(_) => StatusCode::INTERNAL_SERVER_ERROR,
        BrowserError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
        BrowserError::Failed(_) => StatusCode::BAD_GATEWAY,
    };
    (status, error.to_string()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::TempDb;

    const NOW: &str = "2026-08-16T10:00:00Z";

    /// A sidecar that answers `/open` with a landing url the test chooses, and records every
    /// placement it was sent.
    ///
    /// The placement is the whole point of the fixture. Everything this module does converges on one
    /// question — which profile was this session put in — and the only place that becomes observable
    /// is the body that crosses the wire.
    async fn stub_sidecar(
        final_url: &'static str,
    ) -> (
        BrowserRuntime,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use axum::extract::Path;
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let app = axum::Router::new().route(
            "/{verb}",
            post(
                move |Path(verb): Path<String>, axum::Json(body): axum::Json<serde_json::Value>| {
                    let recorder = recorder.clone();
                    async move {
                        recorder
                            .lock()
                            .unwrap()
                            .push(serde_json::json!({"verb": verb, "body": body}));
                        if verb == "open" {
                            let requested = body["url"].as_str().unwrap_or_default().to_string();
                            return axum::Json(serde_json::json!({
                                "id": "s1",
                                "mode": "agent",
                                "requested_url": requested,
                                "final_url": final_url,
                                "title": "",
                            }))
                            .into_response();
                        }
                        axum::http::StatusCode::NO_CONTENT.into_response()
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            BrowserRuntime {
                enabled: true,
                client: BrowserClient::new(&address.to_string(), "tok".into()),
            },
            seen,
        )
    }

    fn placements(
        seen: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) -> Vec<serde_json::Value> {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|call| call["verb"] == "open")
            .map(|call| call["body"]["placement"].clone())
            .collect()
    }

    /// Spec §5.3a, and the shape that made the first version of `grant` wrong: a real login RETURNS
    /// to where it started, so the chain names the destination twice. The set is granted together,
    /// and which member is the destination decides what a person sees on the revocation screen.
    #[tokio::test]
    async fn a_login_chain_is_granted_as_one_set_with_the_destination_named() {
        let db = TempDb::new().await;
        let granted = grant(
            &db.pool,
            "acme",
            &[
                "https://jira.example.org/login".into(),
                "https://accounts.google.com/o/oauth2/auth".into(),
                "https://jira.example.org/browse/X-1".into(),
            ],
            NOW,
        )
        .await
        .expect("grant");

        // The stored form carries the port, because the port is part of the identity (spec §5.3)
        // — and it is the form the Go fence produces too, which is what makes the two lists one.
        assert_eq!(
            granted,
            vec![
                "https://jira.example.org:443",
                "https://accounts.google.com:443"
            ]
        );
        let sites = list_sites(&db.pool, "acme").await.expect("sites");
        let jira = sites
            .iter()
            .find(|site| site.origin == "https://jira.example.org:443")
            .expect("the destination is stored");
        assert_eq!(jira.kind, "destination", "the login was FOR jira");
        assert_eq!(jira.granted_for, None);
        let google = sites
            .iter()
            .find(|site| site.origin == "https://accounts.google.com:443")
            .expect("the idp is stored");
        assert_eq!(google.kind, "idp");
        assert_eq!(
            google.granted_for.as_deref(),
            Some("https://jira.example.org:443")
        );
        db.close().await;
    }

    /// Granting twice does not rewrite when a permission was granted. The date and the reason are
    /// the audit trail of a human act, and overwriting them on every later login would erase it.
    #[tokio::test]
    async fn a_second_login_does_not_rewrite_the_first_grant() {
        let db = TempDb::new().await;
        grant(&db.pool, "acme", &["https://jira.example.org/".into()], NOW)
            .await
            .expect("first");
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/".into()],
            "2027-01-01T00:00:00Z",
        )
        .await
        .expect("second");

        let sites = list_sites(&db.pool, "acme").await.expect("sites");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].granted_at, NOW);
        db.close().await;
    }

    /// Nothing that is not an https origin reaches the table. `granted_origins` is what filters, and
    /// this pins that `grant` does not route around it — an `http` step in a chain is the one that
    /// would otherwise put a plaintext host inside a profile holding live session cookies.
    #[tokio::test]
    async fn a_chain_of_nothing_grants_nothing() {
        let db = TempDb::new().await;
        for chain in [
            vec![],
            vec!["not a url".to_string()],
            vec!["http://jira.example.org/".to_string()],
        ] {
            let granted = grant(&db.pool, "acme", &chain, NOW).await.expect("grant");
            assert!(granted.is_empty(), "{chain:?} granted {granted:?}");
        }
        assert!(
            list_sites(&db.pool, "acme")
                .await
                .expect("sites")
                .is_empty()
        );
        db.close().await;
    }

    /// A listed host opens in the project profile, and the site list travels with it. Without the
    /// list on the wire the sidecar's fence would have nothing to enforce, and would refuse the
    /// session it was opened for.
    #[tokio::test]
    async fn a_listed_host_opens_in_the_project_profile() {
        let db = TempDb::new().await;
        grant(&db.pool, "acme", &["https://jira.example.org/".into()], NOW)
            .await
            .expect("grant");
        let (runtime, seen) = stub_sidecar("https://jira.example.org/browse/X-1").await;

        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: Some(7),
                url: "https://jira.example.org/browse/X-1",
                surface: Surface::Assistant,
                requester: Requester::Owner,
                now: NOW,
            },
        )
        .await
        .expect("open");

        let Opened::Session(row) = opened else {
            panic!("a listed host must open");
        };
        assert_eq!(row.profile_kind, "project");
        assert_eq!(row.rule, browser_policy::RULE_PROJECT_SITE);
        assert_eq!(row.project_id.as_deref(), Some("acme"));
        assert_eq!(row.sidecar_id, "s1");

        let sent = placements(&seen);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["profile"]["kind"], "project");
        assert_eq!(sent[0]["origins"][0], "https://jira.example.org:443");
        db.close().await;
    }

    /// The control, and the ordinary case: anything not on the list is a throwaway. Not a refusal —
    /// a downgrade (spec §5.4) — and the throwaway carries no site list at all.
    #[tokio::test]
    async fn an_unlisted_host_opens_in_a_throwaway() {
        let db = TempDb::new().await;
        grant(&db.pool, "acme", &["https://jira.example.org/".into()], NOW)
            .await
            .expect("grant");
        let (runtime, seen) = stub_sidecar("https://news.example.net/").await;

        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: Some(7),
                url: "https://news.example.net/",
                surface: Surface::Assistant,
                requester: Requester::Owner,
                now: NOW,
            },
        )
        .await
        .expect("open");

        let Opened::Session(row) = opened else {
            panic!("an unlisted host is downgraded, not refused");
        };
        assert_eq!(row.profile_kind, "ephemeral");
        assert_eq!(row.rule, browser_policy::RULE_OFF_LIST);
        // The PROFILE belongs to no project; the session was still opened for one, and spec §4.5's
        // handover reads exactly this field to know where to establish the login.
        assert_eq!(row.project_id.as_deref(), Some("acme"));
        assert_eq!(row.profile_id, "r7", "one throwaway per run");

        let sent = placements(&seen);
        assert_eq!(sent[0]["profile"]["kind"], "ephemeral");
        assert!(sent[0].get("origins").is_none(), "{}", sent[0]);
        db.close().await;
    }

    /// Spec §5.4's redirect trap, and the reason an open can happen twice.
    ///
    /// A listed host with an open redirect must not launder a stranger's page into the profile that
    /// holds the logins. The fence refuses the document there, so nothing runs; this asserts that the
    /// núcleo then reopens the SAME request in a throwaway rather than reporting a failure — and
    /// that the second placement carries no site list, which is what makes the retry meaningful.
    #[tokio::test]
    async fn a_redirect_off_the_list_is_reopened_in_a_throwaway() {
        let db = TempDb::new().await;
        grant(&db.pool, "acme", &["https://jira.example.org/".into()], NOW)
            .await
            .expect("grant");
        let (runtime, seen) = stub_sidecar("https://evil.example.net/").await;

        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: Some(7),
                url: "https://jira.example.org/redirect?to=evil",
                surface: Surface::Assistant,
                requester: Requester::Owner,
                now: NOW,
            },
        )
        .await
        .expect("open");

        let Opened::Session(row) = opened else {
            panic!("the downgrade produces a session");
        };
        assert_eq!(row.profile_kind, "ephemeral");
        assert_eq!(row.rule, browser_policy::RULE_REDIRECTED_OUT);

        let sent = placements(&seen);
        assert_eq!(sent.len(), 2, "one open in each profile");
        assert_eq!(sent[0]["profile"]["kind"], "project");
        assert_eq!(sent[1]["profile"]["kind"], "ephemeral");

        // The retry goes to the url that was ASKED for, not the one it landed on. Jumping to the
        // destination would skip whatever the redirect does on the way.
        let urls = {
            let calls = seen.lock().unwrap();
            calls
                .iter()
                .filter(|call| call["verb"] == "open")
                .map(|call| call["body"]["url"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(urls[1], urls[0]);

        // And the project session is closed rather than left open next to the one that replaced it.
        let open_now = open_sessions(&db.pool).await.expect("open sessions");
        assert_eq!(open_now.len(), 1);
        assert_eq!(open_now[0].profile_kind, "ephemeral");
        db.close().await;
    }

    /// Spec §6.0b. The autonomous path is a seam and not a road: it is refused structurally, and the
    /// refusal says so rather than looking like something the person can fix by opening the shell.
    #[tokio::test]
    async fn an_autonomous_surface_is_refused_before_anything_is_opened() {
        let db = TempDb::new().await;
        let (runtime, seen) = stub_sidecar("https://example.org/").await;

        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: Some(7),
                url: "https://example.org/",
                surface: Surface::Autonomous,
                requester: Requester::Owner,
                now: NOW,
            },
        )
        .await
        .expect("a refusal is not an error");

        match opened {
            Opened::Refused { rule, recoverable } => {
                assert_eq!(rule, browser_policy::RULE_REACH_UNDESIGNED);
                assert!(
                    !recoverable,
                    "nothing the person does today opens this path"
                );
            }
            Opened::Session(_) => panic!("an autonomous surface must not open a session"),
        }
        assert!(
            placements(&seen).is_empty(),
            "the sidecar was called anyway"
        );
        assert!(open_sessions(&db.pool).await.expect("sessions").is_empty());
        db.close().await;
    }

    /// The pillar ships off (spec §14.2), and off means the sidecar is never called — not that the
    /// call fails somewhere further in.
    #[tokio::test]
    async fn a_disabled_pillar_opens_nothing() {
        let db = TempDb::new().await;
        let (mut runtime, seen) = stub_sidecar("https://example.org/").await;
        runtime.enabled = false;

        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: None,
                url: "https://example.org/",
                surface: Surface::Assistant,
                requester: Requester::Owner,
                now: NOW,
            },
        )
        .await
        .expect("disabled is an answer");
        assert!(matches!(opened, Opened::Refused { .. }));
        assert!(placements(&seen).is_empty());
        db.close().await;
    }

    /// A session with no run still needs a profile name nobody else will use. It takes the row's own
    /// id, which is unique by construction and leads back to the row that opened it.
    #[tokio::test]
    async fn a_session_outside_a_run_names_its_own_throwaway() {
        let db = TempDb::new().await;
        let (runtime, _) = stub_sidecar("https://example.org/").await;

        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: None,
                url: "https://example.org/",
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
        assert_eq!(row.profile_id, format!("s{}", row.id));
        db.close().await;
    }

    /// Spec §9.1: the sidecar restarting costs every live session. What must not survive is a row
    /// here saying "open" about a browser that no longer exists — which is worse than saying nothing,
    /// because the UI would offer to take the wheel of it.
    #[tokio::test]
    async fn a_sidecar_restart_retires_the_sessions_it_took_with_it() {
        let db = TempDb::new().await;
        let (runtime, _) = stub_sidecar("https://example.org/").await;
        open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: Some(1),
                url: "https://example.org/",
                surface: Surface::Assistant,
                requester: Requester::Owner,
                now: NOW,
            },
        )
        .await
        .expect("open");
        assert_eq!(open_sessions(&db.pool).await.expect("open").len(), 1);

        let retired = retire_open_sessions(&db.pool, "2026-08-16T11:00:00Z")
            .await
            .expect("retire");
        assert_eq!(retired, 1);
        assert!(open_sessions(&db.pool).await.expect("open").is_empty());

        // And retiring again is a no-op, so a restart loop does not rewrite the history of sessions
        // that were already accounted for.
        assert_eq!(
            retire_open_sessions(&db.pool, "2026-08-16T12:00:00Z")
                .await
                .expect("retire"),
            0
        );
        db.close().await;
    }

    /// Closing a session the sidecar has already forgotten still closes it here. Otherwise a restart
    /// between the open and the close leaves a row open for ever, and the ceiling it counts against
    /// never comes back.
    #[tokio::test]
    async fn closing_a_session_the_sidecar_lost_still_closes_the_row() {
        let db = TempDb::new().await;
        let (runtime, _) = stub_sidecar("https://example.org/").await;
        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: Some(1),
                url: "https://example.org/",
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

        // A client pointing at nothing: the same shape as a sidecar that restarted between calls.
        let gone = BrowserRuntime {
            enabled: true,
            client: BrowserClient::new("127.0.0.1:1", "tok".into()),
        };
        let _ = close(&db.pool, &gone, row.id, "2026-08-16T11:00:00Z").await;
        let after = session_row(&db.pool, row.id)
            .await
            .expect("row")
            .expect("row");
        assert!(
            after.closed_at.is_some(),
            "the row must not stay open when the browser is gone"
        );
        db.close().await;
    }

    /// Revoking removes the row, and the next open no longer places that host in the project
    /// profile. The two halves are one property: a revocation that left the decision unchanged would
    /// be a button that does nothing.
    #[tokio::test]
    async fn revoking_a_site_moves_it_back_to_a_throwaway() {
        let db = TempDb::new().await;
        grant(&db.pool, "acme", &["https://jira.example.org/".into()], NOW)
            .await
            .expect("grant");
        let (runtime, _) = stub_sidecar("https://jira.example.org/").await;

        assert!(
            revoke(&db.pool, "acme", "https://jira.example.org")
                .await
                .expect("revoke")
        );
        assert!(
            !revoke(&db.pool, "acme", "https://jira.example.org")
                .await
                .expect("revoke twice")
        );

        let opened = open(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: Some(2),
                url: "https://jira.example.org/",
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
        assert_eq!(row.profile_kind, "ephemeral");
        db.close().await;
    }
}
