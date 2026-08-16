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

// Nothing calls this yet: the storage and the domain land before the handlers that mount them.
//
// INSTRUCTION, not description — the same one `browser_policy.rs` and `browser_client.rs` carry:
// DELETE THIS LINE when the `/browser/*` routes exist. All three suppressions go together, and if
// any is still here afterwards, that module has grown something nothing reaches.
#![cfg_attr(not(test), allow(dead_code))]

use serde::Serialize;
use sqlx::{Row, SqlitePool};

use crate::browser_client::{BrowserClient, BrowserError, Placement, Session};
use crate::browser_policy::{self, Outcome, Profile, Requester, Surface};

/// What the daemon carries for this pillar. Built once at startup from `.ai/browser.yaml`.
#[derive(Debug)]
pub struct BrowserRuntime {
    /// Spec §14.2: the pillar ships off and stays off until the fence, the profiles, the handoff and
    /// the tools are green together.
    pub enabled: bool,
    pub client: BrowserClient,
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
    pub project_id: Option<String>,
    pub profile_kind: String,
    pub profile_id: String,
    pub requested_url: String,
    pub final_url: String,
    pub rule: String,
    pub mode: String,
    pub refusal: Option<String>,
    pub opened_at: String,
    pub closed_at: Option<String>,
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
    .bind(if profile == Profile::Project {
        Some(project_id)
    } else {
        None
    })
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
                final_url, rule, mode, refusal, opened_at, closed_at \
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
        opened_at: row.get("opened_at"),
        closed_at: row.get("closed_at"),
    }))
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
        assert_eq!(row.project_id, None, "a throwaway belongs to no project");
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
