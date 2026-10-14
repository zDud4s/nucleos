//! §spec pilar-de-browser
//!
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

pub mod live;
pub mod panel;
pub mod seat;
pub mod wheel;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::browser_client::{BrowserClient, BrowserError, Placement, Session};
use crate::browser_policy::{self, Outcome, Profile, Requester, Surface};
use crate::state::AppState;

mod handlers;

pub use self::handlers::*;

/// What the daemon carries for this pillar. Built once at startup from `~/.nucleos/browser.yaml`.
#[derive(Debug)]
pub struct BrowserRuntime {
    /// Spec §14.2: the pillar ships off and stays off until the fence, the profiles, the handoff and
    /// the tools are green together.
    pub enabled: bool,
    pub client: BrowserClient,
    /// Where a session's mode changes are announced to whoever is watching its live view.
    pub modes: crate::browser::live::ModeChannels,
    /// Who holds the person's seat in each session, kept in memory only (spec browser-volante §4.3).
    pub seats: crate::browser::seat::SeatState,
}

impl BrowserRuntime {
    /// The pillar, off. Named rather than derived, for `WebRuntime::disabled`'s reason: a derived
    /// default would invent a client pointing at nothing, and "off" should be a state somebody chose.
    ///
    /// `#[cfg(test)]` because production always builds a real one from `~/.nucleos/browser.yaml`; this is
    /// what the `test_state()` fixtures hold.
    #[cfg(any(test, feature = "testkit"))]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            client: BrowserClient::new(crate::sidecar::BROWSER_ADDR, String::new()),
            modes: Default::default(),
            seats: Default::default(),
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
    /// Whether an agent may SUBMIT A FORM to this origin, and not merely read it.
    ///
    /// Granted at the login, next to the read grant and separately from it, and never true on an
    /// `idp` row — see [`grant`] for why the identity providers in a login chain are the one place
    /// this must not reach.
    pub writable: bool,
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
    /// Where a person drives once the mode is `human`: `shell` or `window`. `None` until a person
    /// drives; rows from before the column existed are `window`.
    pub seat: Option<String>,
    /// Opened as a window the person can see, with a chat panel on the right (spec
    /// browser-com-painel). Headless sessions are `false`.
    pub visible: bool,
    /// The conversation this session answers to when no run opened it: a window opened from the
    /// shell's Web page "with agent" (spec browser-com-painel-pela-pagina-web §3.2). Wins over
    /// `run_id` when both are set.
    pub owner_chat_id: Option<String>,
    /// Whether the shell could take the seat: a lone open project-profile session. A second open
    /// session on the same profile would share its cookies, so neither qualifies.
    pub shell_eligible: bool,
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

/// What a project's profile may do: the origins it admits, and which of those it may write to.
///
/// One value and one query rather than two of each, for the reason [`Placement`] carries the profile
/// and the sites together: they are one decision, and a caller holding half of it has a browser with
/// logins and an incomplete rule about what may be done with them. Two reads could also disagree —
/// a grant landing between them — and there is no moment at which that disagreement would be seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowed {
    /// Every admitted origin, in the shape `browser_policy::decide` takes.
    pub read: Vec<String>,
    /// The subset that may also be submitted to. A subset in practice and not by construction — the
    /// fence says the same thing about its own two lists, and for the same reason.
    pub write: Vec<String>,
}

/// The origins a project's profile admits, and which of them a form may be submitted to.
pub async fn admitted_origins(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Allowed> {
    let rows = sqlx::query(
        "SELECT origin, writable FROM browser_sites WHERE project_id = ? ORDER BY origin",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let mut allowed = Allowed::default();
    for row in rows {
        let origin: String = row.get("origin");
        if row.get::<i64, _>("writable") != 0 {
            allowed.write.push(origin.clone());
        }
        allowed.read.push(origin);
    }
    Ok(allowed)
}

/// The same list with everything a person needs to decide whether to revoke one.
pub async fn list_sites(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<Site>> {
    let rows = sqlx::query(
        "SELECT origin, kind, granted_at, granted_for, writable FROM browser_sites \
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
            writable: row.get::<i64, _>("writable") != 0,
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
pub async fn grant(
    pool: &SqlitePool,
    project_id: &str,
    chain: &[String],
    writable: bool,
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
        // Writing is granted to the DESTINATION and never to an identity provider, and this is the
        // one line where that is decided. An idp in a login chain is a stepping stone the person did
        // not choose to browse — and the forms on it are login forms, which are precisely the forms
        // an agent must never submit. So the box the person ticked applies to where they landed, and
        // the hosts they passed through stay readable and nothing more.
        let writes = i64::from(writable && kind == "destination");
        // The date and the reason a host was first granted for are NEVER overwritten: rewriting
        // them every time somebody logged in again would rewrite the audit trail of a permission.
        // What the upsert does touch is `writable`, and only on the destination row.
        //
        // # Why the answer at the login is authoritative, including when it narrows
        //
        // The person is looking at the origin they just logged into and answering whether an
        // agent may submit forms there. Treating that as "grant if ticked, leave alone if not"
        // would mean a write permission could be added by an answer and never removed by one,
        // which is how a list of permissions stops matching what anyone believes it says. So an
        // unticked box on the destination takes the grant away — the narrowing direction, which
        // is the one an ambiguous answer should fall in.
        //
        // An idp row is left entirely alone on conflict, because this answer is not about it: the
        // same host can be a stepping stone here and somewhere the person deliberately granted
        // writing over there, and one login must not reach into the other.
        sqlx::query(
            "INSERT INTO browser_sites (project_id, origin, kind, granted_at, granted_for, writable) \
             VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(project_id, origin) DO UPDATE SET \
             writable = CASE WHEN excluded.kind = 'destination' \
                        THEN excluded.writable ELSE browser_sites.writable END",
        )
        .bind(project_id)
        .bind(origin)
        .bind(kind)
        .bind(now)
        .bind(granted_for)
        .bind(writes)
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

/// Take back one origin's WRITE grant, leaving it readable.
///
/// The narrower half of [`revoke`], and it only ever narrows: there is no argument, no
/// `writable: bool`, and therefore no way to spell "grant" with it. That is deliberate, and it is
/// the same invariant `POST /browser/grant` does not exist for — a route that can widen a permission
/// is a route the confused deputy of spec §5.2 could be aimed at, and this one has nothing to aim.
///
/// Widening happens in exactly one place, [`grant`], reached from a person answering for a login
/// they have just performed themselves.
pub async fn make_readonly(
    pool: &SqlitePool,
    project_id: &str,
    origin: &str,
) -> sqlx::Result<bool> {
    // Normalised through the rule that stored it, for revoke's reason: the stored form carries its
    // port explicitly, so a caller passing the shorter spelling of the same origin would otherwise
    // get a silent no-op on the one screen where "nothing happened" and "access withdrawn" look
    // exactly alike.
    let normalised = browser_policy::granted_origins(std::slice::from_ref(&origin.to_string()));
    let Some(origin) = normalised.first() else {
        return Ok(false);
    };
    let result = sqlx::query(
        "UPDATE browser_sites SET writable = 0 \
         WHERE project_id = ? AND origin = ? AND writable = 1",
    )
    .bind(project_id)
    .bind(origin)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// One form submission an agent sent, as this database keeps it.
///
/// See migration 0107 for what is deliberately absent and why: the field NAMES, and never the
/// values. The cost is stated there and accepted — knowing that something was submitted to a reply
/// form does not say what the reply said — and the alternative is a database where every credential
/// an agent ever types comes to rest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Written {
    pub id: i64,
    pub session_id: i64,
    pub origin: String,
    /// The form's action with the query removed, and the method it went with.
    pub action: String,
    pub method: String,
    /// The names of the fields submitted, and how many there were in total. The two disagree when a
    /// form was long enough for the names to be truncated, which is why the count is its own number.
    pub fields: Vec<String>,
    pub field_count: i64,
    /// The act that caused it: the ref from the snapshot, and the verb.
    pub element_ref: String,
    pub verb: String,
    /// The names of any files that went with it. Empty is "none went"; the column is NULL for rows
    /// written before attachments existed, and both read as empty here — the distinction lives in
    /// the database, where migration 0108 explains it, and there is nothing a screen would do
    /// differently with it.
    pub files: Vec<String>,
    pub written_at: String,
}

/// File what an act sent. Called with whatever the sidecar reported, which is only what LEFT.
///
/// Failures are logged and swallowed rather than returned, and that is a decision worth defending:
/// the submission has already happened by the time this runs, so turning a database error into an
/// error for the caller would tell an agent its form was not sent when it was. What it must never do
/// is be silent — a write that reached nobody's record is exactly the state that makes this
/// arrangement unsupervisable — so it goes to the log at error level.
pub async fn record_writes(
    pool: &SqlitePool,
    session_id: i64,
    project_id: Option<&str>,
    writes: &[crate::browser_client::Write],
    now: &str,
) {
    for wrote in writes {
        let fields = serde_json::to_string(&wrote.fields).unwrap_or_else(|_| "[]".to_string());
        // `None` when nothing was attached, and not an empty list. Migration 0108 asks for the
        // distinction: a row that predates attachments and a submission that carried none are
        // different facts, and a column that says "[]" for both loses the only one it could tell.
        let files = if wrote.files.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&wrote.files).unwrap_or_else(|_| "[]".to_string()))
        };
        let outcome = sqlx::query(
            "INSERT INTO browser_writes \
             (session_id, project_id, origin, action, method, fields, field_count, ref, verb, \
              files, written_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(project_id)
        .bind(&wrote.origin)
        .bind(&wrote.action)
        .bind(&wrote.method)
        .bind(&fields)
        .bind(wrote.field_count)
        .bind(&wrote.r#ref)
        .bind(&wrote.verb)
        .bind(&files)
        .bind(now)
        .execute(pool)
        .await;
        if let Err(error) = outcome {
            tracing::error!(
                session = session_id,
                origin = %wrote.origin,
                %error,
                "a form submission left the machine and was not recorded"
            );
        }
    }
}

/// What a project has written, most recent first.
///
/// Read next to the revocation, and that is what makes it supervision rather than decoration: on the
/// screen where the grant comes off, "this origin writes, and here is what it has written".
pub async fn list_writes(
    pool: &SqlitePool,
    project_id: &str,
    limit: i64,
) -> sqlx::Result<Vec<Written>> {
    let rows = sqlx::query(
        "SELECT id, session_id, origin, action, method, fields, field_count, ref, verb, files, \
         written_at FROM browser_writes WHERE project_id = ? ORDER BY written_at DESC, id DESC \
         LIMIT ?",
    )
    .bind(project_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| Written {
            id: row.get("id"),
            session_id: row.get("session_id"),
            origin: row.get("origin"),
            action: row.get("action"),
            method: row.get("method"),
            // A row whose JSON will not parse still reports its count, because the count is the part
            // that says something was submitted at all. Dropping the row would hide a write.
            fields: serde_json::from_str(&row.get::<String, _>("fields")).unwrap_or_default(),
            field_count: row.get("field_count"),
            element_ref: row.get::<Option<String>, _>("ref").unwrap_or_default(),
            verb: row.get::<Option<String>, _>("verb").unwrap_or_default(),
            files: row
                .get::<Option<String>, _>("files")
                .and_then(|said| serde_json::from_str(&said).ok())
                .unwrap_or_default(),
            written_at: row.get("written_at"),
        })
        .collect())
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
    open_with(pool, runtime, ask, false).await
}

/// `open`, optionally as a window the person can see. A visible session runs in the profile the
/// policy chose, like any other (spec browser-com-painel-pela-pagina-web §3.6): the project's
/// profile for a site a person logged into there, a throwaway otherwise. Forcing the project's
/// profile for every visible open admitted nothing the fence would load and, on a project with no
/// site yet, left the fence with an empty list it refuses to start with.
pub async fn open_with(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    ask: Ask<'_>,
    visible: bool,
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
    let decision = browser_policy::decide(url, url, surface, requester, &sites.read);
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

    let opened = if visible {
        runtime.client.open_visible(url, &placement).await
    } else {
        runtime.client.open(url, &placement).await
    };
    let session = match opened {
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
    if profile == Profile::Project && !visible {
        let after =
            browser_policy::decide(url, &session.final_url, surface, requester, &sites.read);
        if matches!(after.outcome, Outcome::Open(Profile::Ephemeral)) {
            let _ = runtime.client.close(&session.id).await;
            let _ = close_row(pool, row_id, after.rule, now).await;
            return reopen_ephemeral(pool, runtime, ask, after.rule).await;
        }
    }

    if visible {
        set_visible(pool, row_id, true)
            .await
            .map_err(|error| BrowserError::Failed(error.to_string()))?;
    }
    let row = finish_session(pool, row_id, &placement, &session)
        .await
        .map_err(|error| BrowserError::Failed(error.to_string()))?;
    Ok(Opened::Session(Box::new(row)))
}

/// Name the conversation a session answers to (spec browser-com-painel-pela-pagina-web §3.2).
pub async fn set_owner_chat(pool: &SqlitePool, id: i64, chat_id: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE browser_sessions SET owner_chat_id = ? WHERE id = ?")
        .bind(chat_id)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Mark a session row as visible (or not).
pub async fn set_visible(pool: &SqlitePool, id: i64, visible: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE browser_sessions SET visible = ? WHERE id = ?")
        .bind(i64::from(visible))
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
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
    // No lists at all, and never read from the database: a throwaway admits every document and
    // writes nowhere, so consulting the project's grants here would only create a way for them
    // to leak into a profile that is deleted at the end of the run.
    let nothing = Allowed::default();
    let placement = placement_for(
        Profile::Ephemeral,
        ask.project_id,
        ask.run_id,
        row_id,
        &nothing,
    );

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
    sites: &Allowed,
) -> Placement {
    match profile {
        // Both lists, always together. A placement carrying the sites and not the write grants would
        // be a fence enforcing half a decision, and the half it dropped is the permissive one.
        Profile::Project => {
            Placement::project(project_id, sites.read.clone()).writing_to(sites.write.clone())
        }
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
                seat, visible, owner_chat_id, \
                (profile_kind = 'project' AND closed_at IS NULL AND \
                 (SELECT COUNT(*) FROM browser_sessions o \
                  WHERE o.profile_id = browser_sessions.profile_id \
                    AND o.profile_kind = 'project' AND o.closed_at IS NULL) = 1) \
                    AS shell_eligible, \
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
        seat: row.get("seat"),
        visible: row.get::<i64, _>("visible") != 0,
        owner_chat_id: row.get("owner_chat_id"),
        shell_eligible: row.get::<i64, _>("shell_eligible") != 0,
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
pub async fn set_mode(
    pool: &SqlitePool,
    modes: &crate::browser::live::ModeChannels,
    id: i64,
    from: &str,
    to: &str,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE browser_sessions SET mode = ? WHERE id = ? AND mode = ? AND closed_at IS NULL",
    )
    .bind(to)
    .bind(id)
    .bind(from)
    .execute(pool)
    .await?;
    let moved = result.rows_affected() == 1;
    if moved {
        // Announced only after the write that made it true, so a watcher never cuts for a change
        // that did not happen.
        modes.publish(
            id,
            crate::browser::live::LiveMode {
                mode: to.to_owned(),
                seat: None,
            },
        );
    }
    Ok(moved)
}

/// [`set_mode`] that also writes the seat, in the same compare-and-set.
///
/// Mode and seat describe one fact (who drives, and where), so they move together or not at all; two
/// writes would leave a window where a `human` row has no seat yet.
pub async fn set_mode_seat(
    pool: &SqlitePool,
    modes: &crate::browser::live::ModeChannels,
    id: i64,
    from: &str,
    to: &str,
    seat: Option<&str>,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE browser_sessions SET mode = ?, seat = ?          WHERE id = ? AND mode = ? AND closed_at IS NULL",
    )
    .bind(to)
    .bind(seat)
    .bind(id)
    .bind(from)
    .execute(pool)
    .await?;
    let moved = result.rows_affected() == 1;
    if moved {
        // Published even when the mode string is unchanged: human/shell -> human/window must cut.
        modes.publish(
            id,
            crate::browser::live::LiveMode {
                mode: to.to_owned(),
                seat: seat.map(str::to_owned),
            },
        );
    }
    Ok(moved)
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
///
/// Only for a session in the project's profile. A throwaway is deleted with its session, so a login
/// made there is one nobody keeps, and granting its hosts would list sites the project's profile
/// was never logged into (spec §5.2).
pub async fn record_chain(pool: &SqlitePool, id: i64, chain: &[String]) -> sqlx::Result<()> {
    // `chain_decided_at` goes back to NULL: a session can be returned twice, and without this the
    // second chain would find its keep question already answered.
    sqlx::query(
        "UPDATE browser_sessions SET chain = ?, chain_decided_at = NULL          WHERE id = ? AND profile_kind = 'project'",
    )
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
    close_with_reason(pool, runtime, id, "closed", now).await
}

/// [`close`] with the reason the row carries, for the paths that close a session for a cause the
/// record has to name (a fence that could not be restored, say).
pub async fn close_with_reason(
    pool: &SqlitePool,
    runtime: &BrowserRuntime,
    id: i64,
    reason: &str,
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
    close_row(pool, id, reason, now)
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
                modes: Default::default(),
                seats: Default::default(),
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

    /// An `AppState` over a temp database and a stubbed sidecar, for the handler tests below.
    fn state_over(pool: sqlx::SqlitePool, browser: BrowserRuntime) -> AppState {
        AppState {
            token: crate::auth::Token("test-token".into()),
            pool,
            telegram_doctrine: None,
            runner: std::sync::Arc::new(crate::execution::runner::FakeCommandRunner::default()),
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
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(browser),
            github: std::sync::Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: std::sync::Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: std::sync::Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: std::sync::Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: std::sync::Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// Spec §4.0: the run that owns a session is the one the daemon says is calling, in the header,
    /// and never one the body names — a body field is a permission the caller grants itself. The
    /// body here claims run 99 and the header says 7; the throwaway profile is named after the run,
    /// so the profile the sidecar was asked for shows which one won.
    #[tokio::test]
    async fn post_open_takes_the_run_from_the_header_not_the_body() {
        let db = TempDb::new().await;
        crate::attention::record_heartbeat(
            &db.pool,
            &crate::attention::AttentionScope::Global,
            chrono::Utc::now(),
        )
        .await
        .expect("owner present");
        let (runtime, seen) = stub_sidecar("https://news.example.net/").await;
        let state = state_over(db.pool.clone(), runtime);

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            crate::daemon_client::RUN_ID_HEADER,
            axum::http::HeaderValue::from_static("7"),
        );
        let response = post_open(
            State(state),
            headers,
            axum::Json(OpenBody {
                project_id: "acme".into(),
                url: "https://news.example.net/".into(),
                run_id: Some(99),
                visible: false,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let sent = placements(&seen);
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0]["profile"]["id"], "r7",
            "the header's run owns the session: {:?}",
            sent[0]
        );
        let stored: Option<i64> = sqlx::query_scalar("SELECT run_id FROM browser_sessions")
            .fetch_one(&db.pool)
            .await
            .expect("the row");
        assert_eq!(stored, Some(7));
        db.close().await;
    }

    /// A visible open follows the policy's profile like any other (spec
    /// browser-com-painel-pela-pagina-web §3.6): a url off the project's list goes to a throwaway,
    /// because the fence of the project's profile admits only the sites a person logged into there.
    #[tokio::test]
    async fn a_visible_open_off_the_list_lands_in_a_throwaway_and_is_marked_visible() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/browse/X-1".into()],
            false,
            NOW,
        )
        .await
        .expect("grant");
        let (runtime, seen) = stub_sidecar("https://news.example.net/").await;

        let opened = open_with(
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
            true,
        )
        .await
        .expect("open");
        let Opened::Session(row) = opened else {
            panic!("a visible open of an allowed surface is a session");
        };

        let sent = placements(&seen);
        assert_eq!(sent[0]["profile"]["kind"], "ephemeral", "{:?}", sent[0]);
        assert_eq!(sent[0]["visible"], true, "{:?}", sent[0]);
        assert!(
            sent[0]["origins"]
                .as_array()
                .is_none_or(|list| list.is_empty()),
            "a throwaway carries none of the project's sites: {:?}",
            sent[0]
        );
        let stored = session_row(&db.pool, row.id)
            .await
            .expect("query")
            .expect("row");
        assert!(stored.visible, "the row remembers the window is visible");
        assert_eq!(stored.profile_kind, "ephemeral");
        db.close().await;
    }

    /// The other half: a site the person logged into in this project opens visible in the
    /// project's profile, with its list, so the agent and the person both have the login.
    #[tokio::test]
    async fn a_visible_open_of_a_listed_site_lands_in_the_project_profile() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/browse/X-1".into()],
            false,
            NOW,
        )
        .await
        .expect("grant");
        let (runtime, seen) = stub_sidecar("https://jira.example.org/browse/X-1").await;

        let opened = open_with(
            &db.pool,
            &runtime,
            Ask {
                project_id: "acme",
                run_id: None,
                url: "https://jira.example.org/browse/X-1",
                surface: Surface::Assistant,
                requester: Requester::Owner,
                now: NOW,
            },
            true,
        )
        .await
        .expect("open");
        let Opened::Session(row) = opened else {
            panic!("a visible open of a listed site is a session");
        };

        let sent = placements(&seen);
        assert_eq!(sent[0]["profile"]["kind"], "project", "{:?}", sent[0]);
        assert_eq!(sent[0]["profile"]["id"], "acme");
        assert_eq!(sent[0]["visible"], true, "{:?}", sent[0]);
        assert_eq!(sent[0]["origins"][0], "https://jira.example.org:443");
        assert_eq!(row.profile_kind, "project");
        db.close().await;
    }

    /// A throwaway is deleted with its session, so a login made in one is a login nobody keeps:
    /// its chain is never recorded, and there is no "keep these?" to answer (spec §5.2).
    #[tokio::test]
    async fn a_throwaway_session_records_no_chain_to_keep() {
        let db = TempDb::new().await;
        let throwaway = insert_session(
            &db.pool,
            None,
            "acme",
            "https://news.example.net/",
            "off-list",
            Profile::Ephemeral,
            NOW,
        )
        .await
        .expect("insert");
        let project = insert_session(
            &db.pool,
            None,
            "acme",
            "https://jira.example.org/",
            "project-site",
            Profile::Project,
            NOW,
        )
        .await
        .expect("insert");
        let chain = vec!["https://jira.example.org/login".to_string()];

        record_chain(&db.pool, throwaway, &chain)
            .await
            .expect("record");
        record_chain(&db.pool, project, &chain)
            .await
            .expect("record");

        assert_eq!(
            take_chain(&db.pool, throwaway, NOW).await.expect("take"),
            None
        );
        assert_eq!(
            take_chain(&db.pool, project, NOW).await.expect("take"),
            Some(chain),
            "the control: a project session's chain is there to answer"
        );
        db.close().await;
    }

    /// The control for the test above: with no `visible` the open is what it always was — the same
    /// profile the policy chose, nothing on the wire saying "window", and a row that says headless.
    #[tokio::test]
    async fn an_open_without_visible_is_headless_as_before() {
        let db = TempDb::new().await;
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
            panic!("an ordinary open is a session");
        };

        let sent = placements(&seen);
        assert_eq!(sent[0]["profile"]["kind"], "ephemeral", "{:?}", sent[0]);
        assert_ne!(sent[0]["visible"], true, "{:?}", sent[0]);
        let stored = session_row(&db.pool, row.id)
            .await
            .expect("query")
            .expect("row");
        assert!(!stored.visible);
        db.close().await;
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
            false,
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
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/".into()],
            false,
            NOW,
        )
        .await
        .expect("first");
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/".into()],
            false,
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
            let granted = grant(&db.pool, "acme", &chain, false, NOW)
                .await
                .expect("grant");
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

    // ---- writing (spec §6.2, extended) ---------------------------------------------------

    /// The one line in `grant` that decides who may be written to, measured on its own.
    ///
    /// Writing reaches the DESTINATION and never an identity provider, and this is not a tidy
    /// distinction: the forms on an identity provider are LOGIN forms, which are precisely the forms
    /// an agent must never submit. A grant that spread across the chain the way the read grant does
    /// would put the one dangerous form in the set on the first login anyone performed.
    #[tokio::test]
    async fn a_write_grant_reaches_the_destination_and_never_an_identity_provider() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &[
                "https://jira.example.org/login".into(),
                "https://accounts.google.com/o/oauth2/auth".into(),
                "https://jira.example.org/browse/X-1".into(),
            ],
            true,
            NOW,
        )
        .await
        .expect("grant");

        let sites = list_sites(&db.pool, "acme").await.expect("sites");
        for site in &sites {
            let expected = site.kind == "destination";
            assert_eq!(
                site.writable, expected,
                "{} is a {} and writable is {}",
                site.origin, site.kind, site.writable
            );
        }
        let allowed = admitted_origins(&db.pool, "acme").await.expect("allowed");
        assert_eq!(allowed.read.len(), 2, "both origins are readable");
        assert_eq!(allowed.write, vec!["https://jira.example.org:443"]);
        db.close().await;
    }

    /// The control that gives the test above its meaning: the same login with the box unticked
    /// grants reading and nothing else. Without this, "writable" could be a column that is always 1.
    #[tokio::test]
    async fn a_login_answered_without_the_box_grants_reading_only() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/browse/X-1".into()],
            false,
            NOW,
        )
        .await
        .expect("grant");

        let allowed = admitted_origins(&db.pool, "acme").await.expect("allowed");
        assert_eq!(allowed.read, vec!["https://jira.example.org:443"]);
        assert!(
            allowed.write.is_empty(),
            "reading a site granted writing to it: {:?}",
            allowed.write
        );
        db.close().await;
    }

    /// A later login answers for the destination, in BOTH directions.
    ///
    /// The narrowing half is the one worth pinning. A permission that an answer can add and no
    /// answer can remove is how a list stops matching what anyone believes it says — so an unticked
    /// box on a host that was writable takes the grant away. The date and the reason it was first
    /// granted for are untouched either way, because those are the audit trail.
    #[tokio::test]
    async fn a_later_login_answers_for_the_destination_in_both_directions() {
        let db = TempDb::new().await;
        let chain = vec!["https://jira.example.org/browse/X-1".to_string()];
        grant(&db.pool, "acme", &chain, true, NOW)
            .await
            .expect("first");
        assert_eq!(
            admitted_origins(&db.pool, "acme")
                .await
                .expect("allowed")
                .write,
            vec!["https://jira.example.org:443"]
        );

        grant(&db.pool, "acme", &chain, false, "2027-01-01T00:00:00Z")
            .await
            .expect("second");

        let allowed = admitted_origins(&db.pool, "acme").await.expect("allowed");
        assert!(
            allowed.write.is_empty(),
            "an unticked box left the write grant standing: {:?}",
            allowed.write
        );
        let sites = list_sites(&db.pool, "acme").await.expect("sites");
        assert_eq!(sites[0].granted_at, NOW, "the audit trail was rewritten");
        db.close().await;
    }

    /// An identity provider is left alone by a login that is not about it.
    ///
    /// The same host can be a stepping stone into one destination and somewhere a person
    /// deliberately granted writing over there. One login reaching into the other would take away a
    /// permission nobody answered about.
    #[tokio::test]
    async fn a_login_through_a_provider_does_not_answer_for_that_provider() {
        let db = TempDb::new().await;
        // Granted deliberately, as a destination, with writing.
        grant(
            &db.pool,
            "acme",
            &["https://accounts.google.com/settings".into()],
            true,
            NOW,
        )
        .await
        .expect("first");

        // And now it appears as a stepping stone in somebody else's login, answered without the box.
        grant(
            &db.pool,
            "acme",
            &[
                "https://jira.example.org/login".into(),
                "https://accounts.google.com/o/oauth2/auth".into(),
                "https://jira.example.org/browse/X-1".into(),
            ],
            false,
            "2027-01-01T00:00:00Z",
        )
        .await
        .expect("second");

        let allowed = admitted_origins(&db.pool, "acme").await.expect("allowed");
        assert_eq!(
            allowed.write,
            vec!["https://accounts.google.com:443"],
            "a login that passed through a host answered for it"
        );
        db.close().await;
    }

    /// Taking the write back without taking the site back — the narrower of the two revocations.
    ///
    /// It exists because they are two permissions and a person may want to end only the larger one:
    /// "this has been useful and I would rather it stopped pressing Send". And it only ever narrows,
    /// which is why there is no boolean on it to pass the other way.
    #[tokio::test]
    async fn an_origin_is_made_readonly_without_losing_the_reading() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/browse/X-1".into()],
            true,
            NOW,
        )
        .await
        .expect("grant");

        // The shorter spelling of the same origin, because that is what a screen sends and the
        // stored form carries the port. A silent no-op here would be indistinguishable from success
        // on the one screen where that matters.
        assert!(
            make_readonly(&db.pool, "acme", "https://jira.example.org")
                .await
                .expect("readonly")
        );

        let allowed = admitted_origins(&db.pool, "acme").await.expect("allowed");
        assert_eq!(allowed.read, vec!["https://jira.example.org:443"]);
        assert!(allowed.write.is_empty());

        // Twice is not an error and is not a lie either: the second answer says nothing changed.
        assert!(
            !make_readonly(&db.pool, "acme", "https://jira.example.org")
                .await
                .expect("readonly")
        );
        db.close().await;
    }

    /// The write list travels with the placement, or the fence has nothing to enforce.
    ///
    /// The half that would fail silently: a placement carrying the sites and not the grants is a
    /// browser that refuses every submission while the database says the permission was given, and
    /// nothing anywhere would report a problem — the agent would simply be told no, correctly, about
    /// a rule nobody wrote.
    #[tokio::test]
    async fn the_write_list_travels_with_the_placement() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/browse/X-1".into()],
            true,
            NOW,
        )
        .await
        .expect("grant");
        let (runtime, seen) = stub_sidecar("https://jira.example.org/browse/X-1").await;

        open(
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

        let sent = placements(&seen);
        assert_eq!(sent[0]["origins"][0], "https://jira.example.org:443");
        assert_eq!(sent[0]["writable"][0], "https://jira.example.org:443");
        db.close().await;
    }

    /// A throwaway carries no write grant, whatever the project has been given.
    ///
    /// The profile has no login in it, so there is nobody for a form to be submitted AS — and the
    /// grants belong to a directory that is deleted at the end of the run. The sidecar's fence
    /// refuses a write list on an ephemeral profile outright, so sending one would not merely be
    /// wrong: it would fail the open.
    #[tokio::test]
    async fn a_throwaway_carries_no_write_grant() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/browse/X-1".into()],
            true,
            NOW,
        )
        .await
        .expect("grant");
        let (runtime, seen) = stub_sidecar("https://news.example.net/").await;

        open(
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

        let sent = placements(&seen);
        assert_eq!(sent[0]["profile"]["kind"], "ephemeral");
        assert!(sent[0].get("writable").is_none(), "{:?}", sent[0]);
        db.close().await;
    }

    /// What the record keeps, and what it refuses to keep.
    ///
    /// The names of the fields and the count, and nowhere in the row a place for a value. This is
    /// the assertion that the price stated in migration 0107 is actually paid: a `password` field is
    /// named and its contents are not in this table at all, because there is no column for them.
    #[tokio::test]
    async fn a_submission_is_recorded_by_its_field_names_and_never_its_values() {
        let db = TempDb::new().await;
        record_writes(
            &db.pool,
            42,
            Some("acme"),
            &[crate::browser_client::Write {
                origin: "https://jira.example.org:443".into(),
                action: "https://jira.example.org/browse/X-1/comment".into(),
                method: "POST".into(),
                fields: vec!["body".into(), "password".into()],
                field_count: 9,
                r#ref: "e7".into(),
                verb: "click".into(),
                files: Vec::new(),
            }],
            NOW,
        )
        .await;

        let written = list_writes(&db.pool, "acme", 100).await.expect("writes");
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].session_id, 42);
        assert_eq!(written[0].origin, "https://jira.example.org:443");
        assert_eq!(written[0].fields, vec!["body", "password"]);
        // Nine were submitted and two names were kept. The two numbers disagree on purpose, and a
        // count that had quietly become "the ones we kept" would be the confident wrongness this
        // whole pillar spent itself removing.
        assert_eq!(written[0].field_count, 9);
        assert_eq!(written[0].element_ref, "e7");
        assert_eq!(written[0].verb, "click");
        db.close().await;
    }

    /// One project does not read another's record. The revocation screen is per project, and so is
    /// the profile whose logins were spent.
    #[tokio::test]
    async fn a_projects_record_is_its_own() {
        let db = TempDb::new().await;
        let wrote = |origin: &str| crate::browser_client::Write {
            origin: origin.into(),
            action: format!("{origin}/x"),
            method: "POST".into(),
            fields: vec!["q".into()],
            field_count: 1,
            r#ref: "e1".into(),
            verb: "click".into(),
            files: Vec::new(),
        };
        record_writes(
            &db.pool,
            1,
            Some("acme"),
            &[wrote("https://a.example:443")],
            NOW,
        )
        .await;
        record_writes(
            &db.pool,
            2,
            Some("other"),
            &[wrote("https://b.example:443")],
            NOW,
        )
        .await;

        let mine = list_writes(&db.pool, "acme", 100).await.expect("writes");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].origin, "https://a.example:443");
        db.close().await;
    }

    /// Forgetting a profile takes the PERMISSIONS and leaves the RECORD.
    ///
    /// The two are different kinds of thing, and this is the same line `browser_sessions` already
    /// sits on: forgetting closes the sessions and does not delete them. A gesture that erased what
    /// an agent had already done under a grant would make "forget this profile" the way to clear the
    /// evidence, which is the opposite of what §10 asks it to be.
    #[tokio::test]
    async fn forgetting_a_profile_keeps_the_record_of_what_was_written() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/browse/X-1".into()],
            true,
            NOW,
        )
        .await
        .expect("grant");
        record_writes(
            &db.pool,
            1,
            Some("acme"),
            &[crate::browser_client::Write {
                origin: "https://jira.example.org:443".into(),
                action: "https://jira.example.org/comment".into(),
                method: "POST".into(),
                fields: vec!["body".into()],
                field_count: 1,
                r#ref: "e1".into(),
                verb: "click".into(),
                files: Vec::new(),
            }],
            NOW,
        )
        .await;
        let (runtime, _) = stub_sidecar("https://jira.example.org/").await;

        forget(&db.pool, &runtime, "acme").await.expect("forget");

        assert!(
            list_sites(&db.pool, "acme")
                .await
                .expect("sites")
                .is_empty(),
            "forgetting left a permission standing"
        );
        assert_eq!(
            list_writes(&db.pool, "acme", 100)
                .await
                .expect("writes")
                .len(),
            1,
            "forgetting erased what had already been done"
        );
        db.close().await;
    }

    /// A listed host opens in the project profile, and the site list travels with it. Without the
    /// list on the wire the sidecar's fence would have nothing to enforce, and would refuse the
    /// session it was opened for.
    #[tokio::test]
    async fn a_listed_host_opens_in_the_project_profile() {
        let db = TempDb::new().await;
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/".into()],
            false,
            NOW,
        )
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
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/".into()],
            false,
            NOW,
        )
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
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/".into()],
            false,
            NOW,
        )
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
            modes: Default::default(),
            seats: Default::default(),
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
        grant(
            &db.pool,
            "acme",
            &["https://jira.example.org/".into()],
            false,
            NOW,
        )
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

    /// Inserts one bare row, the way a database written before the seat column would hold it.
    async fn raw_session(
        pool: &sqlx::SqlitePool,
        profile_kind: &str,
        profile_id: &str,
        closed: bool,
    ) -> i64 {
        let closed_at = closed.then_some(NOW);
        sqlx::query(
            "INSERT INTO browser_sessions                (sidecar_id, run_id, project_id, profile_kind, profile_id, requested_url,                 final_url, rule, mode, opened_at, closed_at)              VALUES ('s', 1, 'acme', ?, ?, 'https://example.org/', 'https://example.org/',                      'project-site', 'agent', ?, ?)",
        )
        .bind(profile_kind)
        .bind(profile_id)
        .bind(NOW)
        .bind(closed_at)
        .execute(pool)
        .await
        .expect("insert")
        .last_insert_rowid()
    }

    /// Rows that existed before the seat column was added are today's behaviour, a real window, and
    /// the migration must say so rather than leave them NULL (which means "no person drives yet").
    #[tokio::test]
    async fn volante_migration_leaves_existing_rows_window() {
        let pool = crate::testdb::pool_migrated_through(166).await;
        raw_session(&pool, "project", "acme", false).await;
        crate::testdb::apply_migrations_after(&pool, 166).await;
        let seat: Option<String> = sqlx::query_scalar("SELECT seat FROM browser_sessions")
            .fetch_one(&pool)
            .await
            .expect("seat");
        assert_eq!(seat.as_deref(), Some("window"));
    }

    /// The row carries the seat and whether the shell could take it: only a lone open project-profile
    /// session qualifies, because a second one on the same profile would share cookies with it.
    #[tokio::test]
    async fn volante_session_json_carries_seat_and_shell_eligible() {
        let db = TempDb::new().await;
        let first = raw_session(&db.pool, "project", "acme", false).await;
        let row = session_row(&db.pool, first).await.unwrap().unwrap();
        assert_eq!(row.seat, None);
        assert!(row.shell_eligible);
        let json = serde_json::to_value(&row).unwrap();
        assert!(json["seat"].is_null());
        assert_eq!(json["shell_eligible"], serde_json::json!(true));

        let second = raw_session(&db.pool, "project", "acme", false).await;
        for id in [first, second] {
            let row = session_row(&db.pool, id).await.unwrap().unwrap();
            assert!(!row.shell_eligible, "two open rows share one profile");
        }

        let throwaway = raw_session(&db.pool, "ephemeral", "run-1", false).await;
        let row = session_row(&db.pool, throwaway).await.unwrap().unwrap();
        assert!(!row.shell_eligible);
        db.close().await;
    }

    /// Mode and seat change in one compare-and-set, and only from the mode the caller expected.
    #[tokio::test]
    async fn volante_set_mode_seat_moves_mode_and_seat_together() {
        let db = TempDb::new().await;
        let id = raw_session(&db.pool, "project", "acme", false).await;
        let modes = crate::browser::live::ModeChannels::default();
        let mut watcher = modes.subscribe(id);

        assert!(
            !set_mode_seat(&db.pool, &modes, id, "human", "agent", None)
                .await
                .unwrap(),
            "wrong expected mode must not move the row"
        );
        assert!(
            set_mode_seat(&db.pool, &modes, id, "agent", "human", Some("shell"))
                .await
                .unwrap()
        );
        let row = session_row(&db.pool, id).await.unwrap().unwrap();
        assert_eq!(row.mode, "human");
        assert_eq!(row.seat.as_deref(), Some("shell"));
        assert!(watcher.has_changed().unwrap());
        assert_eq!(*watcher.borrow_and_update(), "human");

        assert!(
            set_mode_seat(&db.pool, &modes, id, "human", "agent", None)
                .await
                .unwrap()
        );
        let row = session_row(&db.pool, id).await.unwrap().unwrap();
        assert_eq!(row.mode, "agent");
        assert_eq!(row.seat, None);
        db.close().await;
    }
}
