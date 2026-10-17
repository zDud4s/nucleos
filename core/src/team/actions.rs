//! Team action proposals, execution, and refusal handling.

use super::*;

/// One thing a department asked the core to do.
#[derive(Debug, Clone, PartialEq, serde::Serialize, sqlx::FromRow)]
pub struct TeamAction {
    pub id: i64,
    pub team_run_id: String,
    /// The item that asked, or NULL when the director did.
    pub ordinal: Option<i64>,
    pub kind: String,
    pub payload: String,
    pub why: String,
    pub proposal_id: Option<i64>,
    pub state: String,
    pub error: Option<String>,
    pub created_at: String,
    pub executed_at: Option<String>,
}

/// Why an action could not even be asked for. Every variant is a refusal handed back to the AGENT,
/// mid-turn, while it can still do something about it.
#[derive(Debug)]
pub enum ActionError {
    /// Not a `Scope::TeamRun` at all.
    NotADepartment,
    /// The run named by the key is gone.
    NoSuchRun,
    /// `kind` is not in `GRANTABLE_ACTIONS`. Nobody can ask for this, granted or not.
    Unknown(String),
    /// This team has no grant for it.
    Ungranted(String),
    /// The payload is not a well-formed request of that kind, with the fault named.
    Malformed(String),
    /// The team already holds as many undecided actions as it may.
    QueueFull(String),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ActionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotADepartment => {
                formatter.write_str("only a department asks the core to act for it")
            }
            Self::NoSuchRun => formatter.write_str("no such team run"),
            Self::Unknown(kind) => write!(formatter, "nobody can do `{kind}`"),
            Self::Ungranted(kind) => {
                write!(formatter, "this department may not do `{kind}`")
            }
            Self::Malformed(why) | Self::QueueFull(why) => formatter.write_str(why),
            Self::Db(error) => write!(formatter, "database error: {error}"),
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ProposeActionRequest {
    pub kind: String,
    pub payload: serde_json::Value,
    pub why: String,
}

/// What the agent is told. **A sentence, not a status code**, because the reader is a model that has
/// to decide what to do next: "filed for approval as #41" and "this department may not send email"
/// lead to different paragraphs, and a 403 leads to a retry.
#[derive(Debug, serde::Serialize)]
pub struct ProposeActionResponse {
    pub id: i64,
    pub proposal_id: Option<i64>,
    pub outcome: String,
}

/// PURE: whether this payload is a well-formed request of this kind, and its canonical form.
///
/// **Validated when it is WRITTEN and never when it is executed.** A `send_email` with no recipient
/// has to be refused to the agent, which is still mid-turn and can still fix it — not to a human
/// three hours later, who can fix nothing and whose only options are to approve something broken or
/// throw away work already paid for.
///
/// Returns the JSON to store, re-serialised from the fields this understands rather than passed
/// through. A payload carrying extra keys stores without them, so what a human reads when approving
/// is exactly what the executor will act on.
pub(super) fn validate_payload(kind: &str, payload: &serde_json::Value) -> Result<String, String> {
    // Trimmed, for the fields where surrounding space is a typo: an address, a subject, a path, a
    // timezone name.
    let text = |field: &str| -> Result<String, String> {
        payload
            .get(field)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("a `{kind}` needs a non-empty `{field}`"))
    };
    // NOT trimmed, for the fields where it is the content: a document's trailing newline is part of
    // the document, and a body's leading blank line may be deliberate. Still required to have
    // something in it — an empty file and an empty message are both requests worth refusing — but
    // what is stored is what was written.
    let body = |field: &str| -> Result<String, String> {
        payload
            .get(field)
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("a `{kind}` needs a non-empty `{field}`"))
    };

    let canonical = match kind {
        "send_email" => {
            let (to, subject, body) = (text("to")?, text("subject")?, body("body")?);
            // The house's own rule, borrowed rather than restated: a header ends at the first line
            // break, so a `\n` in `to` or `subject` does not corrupt a message — it ends that header
            // and starts one nobody approved. Asking `mailsend` means a department and the owner's
            // own send button cannot come to disagree about what an address is.
            crate::integrations::mailsend::validate(&to, &subject).map_err(str::to_owned)?;
            serde_json::json!({ "to": to, "subject": subject, "body": body })
        }
        "file_document" => {
            let (path, content) = (text("path")?, body("content")?);
            // Shape only, here. Whether the path stays inside the files root is decided by
            // `files::resolve_within` at execution, against a root this function does not have —
            // and that is the check that matters, so this one only refuses the obviously wrong.
            if path.starts_with('/') || path.starts_with('\\') || path.contains("..") {
                return Err(
                    "a `file_document` path is relative and stays inside the folder".into(),
                );
            }
            serde_json::json!({ "path": path, "content": content })
        }
        "calendar_event" => {
            let title = text("title")?;
            let starts_at_local = text("starts_at_local")?;
            let tz = text("tz")?;
            let minutes = payload
                .get("duration_minutes")
                .and_then(serde_json::Value::as_i64)
                .filter(|minutes| crate::integrations::calendar::duration_in_bounds(*minutes))
                .ok_or(
                    "a `calendar_event` needs a `duration_minutes` above zero and at most 31 days",
                )?;
            // Both parsed HERE and stored as strings, so an unparseable date is the agent's problem
            // and not a `failed` row a human approved in good faith.
            chrono::NaiveDateTime::parse_from_str(
                &starts_at_local,
                crate::integrations::calendar::LOCAL_FORMAT,
            )
            .map_err(|_| {
                format!(
                    "`starts_at_local` is written {}, e.g. 2026-08-17T09:30:00",
                    crate::integrations::calendar::LOCAL_FORMAT
                )
            })?;
            tz.parse::<chrono_tz::Tz>()
                .map_err(|_| format!("`{tz}` is not a timezone name, e.g. Europe/Lisbon"))?;
            serde_json::json!({
                "title": title,
                "starts_at_local": starts_at_local,
                "duration_minutes": minutes,
                "tz": tz,
            })
        }
        // Unreachable in production: `propose_action` checks `GRANTABLE_ACTIONS` first. Kept total
        // rather than `unreachable!` so that adding a kind to the constant and forgetting this
        // function is a refusal, not a panic in a background loop.
        _ => return Err(format!("nobody can do `{kind}`")),
    };
    Ok(canonical.to_string())
}

/// A department asks the core to do something. **It does not happen here.**
///
/// This is the whole of the design's decision #2, and the reason `TEAM_TOOLS` stays read-only
/// forever: the agent declares an intention and gets a sentence back, the turn carries on, nothing
/// blocks and nothing is resumed. `proposals::create_calendar_event` had already written the shape
/// — *"The agent never writes the event itself. This row is the whole mechanism"* — and this is that
/// mechanism with a second table in front of it.
///
/// **Which run is asking comes from the key, not from the body**, exactly as `post_read_file` insists
/// one route over. A department is the one scope in the house that names its caller.
///
/// The order of the five refusals is not arbitrary. The constant is asked before the table, so a row
/// somebody put in `team_grants` by hand cannot grant an action the house does not have. The payload
/// is checked before the ceiling, so a malformed request is a fault the agent can fix rather than
/// one that eats a slot in the queue.
pub async fn propose_action(
    state: &AppState,
    scope: &crate::auth::Scope,
    headers: &axum::http::HeaderMap,
    request: &ProposeActionRequest,
) -> Result<ProposeActionResponse, ActionError> {
    let crate::auth::Scope::TeamRun(team_run_id) = scope else {
        return Err(ActionError::NotADepartment);
    };

    if !GRANTABLE_ACTIONS.contains(&request.kind.as_str()) {
        return Err(ActionError::Unknown(request.kind.clone()));
    }

    let team_id: String = sqlx::query_scalar("SELECT team_id FROM team_runs WHERE id = ?")
        .bind(team_run_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(ActionError::NoSuchRun)?;

    let mode: Option<String> =
        sqlx::query_scalar("SELECT mode FROM team_grants WHERE team_id = ? AND kind = ?")
            .bind(&team_id)
            .bind(&request.kind)
            .fetch_optional(&state.pool)
            .await?;
    // Default deny. The absence of a row is the refusal — there is no `mode = 'deny'` to disagree
    // with it.
    let mode = mode.ok_or_else(|| ActionError::Ungranted(request.kind.clone()))?;

    let payload =
        validate_payload(&request.kind, &request.payload).map_err(ActionError::Malformed)?;
    let why = request.why.trim();
    if why.is_empty() {
        return Err(ActionError::Malformed(
            "say why, in one line — it is what the person deciding will read".to_owned(),
        ));
    }

    // Only `propose` actions occupy the queue: an `allow` action is decided already and executes on
    // the next pass, so counting it would let a fast-clearing kind block a slow one.
    if mode == "propose" {
        let ceiling: i64 = sqlx::query_scalar("SELECT max_open_actions FROM teams WHERE id = ?")
            .bind(&team_id)
            .fetch_optional(&state.pool)
            .await?
            .unwrap_or(DEFAULT_MAX_OPEN_ACTIONS);
        let open = open_actions_of(&state.pool, &team_id).await?;
        if open >= ceiling {
            return Err(ActionError::QueueFull(format!(
                "this department already has {open} action(s) waiting for approval, and may hold \
                 {ceiling}; the person deciding has not got to them yet"
            )));
        }
    }

    // Attribution, not authority: every node of a run may ask, and this only records which did.
    // `None` for the director and for a node the run does not recognise — see `Caller`.
    //
    // **Read before the transaction opens, and that ordering is load-bearing.** A read taken while
    // a transaction is in flight is served by a SECOND pooled connection, and against a
    // `sqlite::memory:` database a second connection is a second, empty database — so this
    // silently resolved to `Unknown` and every action was recorded as nobody's. Every read this
    // function makes happens before `begin`; everything after it is writes.
    let ordinal = match calling_node(&state.pool, team_run_id, headers).await {
        Caller::Specialist(ordinal) => Some(ordinal),
        Caller::Director | Caller::Unknown => None,
    };

    let now = chrono::Utc::now().to_rfc3339();
    let mut transaction = state.pool.begin().await?;
    // The proposal FIRST, so its id can go on the action row. One transaction and not two writes:
    // an action with no proposal is one nobody will ever decide, and a proposal with no action is a
    // button that approves nothing. `ingest_director` learned this at cost in the pillar before —
    // saving only the last write leaves the earlier ones unsaved.
    let proposal_id = if mode == "propose" {
        Some(
            crate::proposals::create_team_action_in_transaction(
                &mut transaction,
                &request.kind,
                why,
                &payload,
                &now,
            )
            .await?,
        )
    } else {
        None
    };
    let id = sqlx::query(
        "INSERT INTO team_actions
           (team_run_id, ordinal, kind, payload, why, proposal_id, state, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?)",
    )
    .bind(team_run_id)
    .bind(ordinal)
    .bind(&request.kind)
    .bind(&payload)
    .bind(why)
    .bind(proposal_id)
    .bind(&now)
    .execute(&mut *transaction)
    .await?
    .last_insert_rowid();
    transaction.commit().await?;

    let outcome = match proposal_id {
        Some(proposal_id) => format!(
            "filed for approval as #{proposal_id}. It will happen if the person says yes; carry on \
             without waiting."
        ),
        None => "queued — this department may do that without asking, and the core will do it \
                 shortly."
            .to_owned(),
    };
    Ok(ProposeActionResponse {
        id,
        proposal_id,
        outcome,
    })
}

/// How many of this team's actions are still waiting for somebody to decide.
///
/// Counted across the team's RUNS and not within one, because the ceiling is the owner's attention
/// and the owner has one queue. It clears the moment somebody decides — the same self-limiting
/// property `wip.rs` has, in the axis `wip.rs` cannot reach: that one counts
/// `proposals WHERE project_id = ?`, and a department has no project.
async fn open_actions_of(pool: &sqlx::SqlitePool, team_id: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*)
           FROM team_actions
           JOIN team_runs ON team_runs.id = team_actions.team_run_id
           JOIN proposals ON proposals.id = team_actions.proposal_id
          WHERE team_runs.team_id = ?
            AND team_actions.state = 'pending'
            AND proposals.status = 'pending'",
    )
    .bind(team_id)
    .fetch_one(pool)
    .await
}

/// Everything decided and not yet done, oldest first.
///
/// `proposal_id IS NULL` is an `allow` action, which was decided when it was written. Otherwise the
/// proposal has to read `approved`: `rejected` is handled by `refuse_action`, and `pending` means
/// nobody has answered.
const DUE_ACTIONS_SQL: &str =
    "SELECT id, team_run_id, ordinal, kind, payload, why, proposal_id, state,
                                      error, created_at, executed_at
                                 FROM team_actions
                                WHERE state = 'pending'
                                  AND (proposal_id IS NULL
                                       OR proposal_id IN (SELECT id FROM proposals
                                                           WHERE status = 'approved'))
                                ORDER BY id";

/// Does what the department asked and a human agreed to, one action at a time.
///
/// **In the tick and not in the approve handler**, for the two reasons this house always gives: an
/// HTTP handler that sends an email holds the connection open while a slow SMTP thinks, and a daemon
/// restarted in the middle of that loses the action with no trace. A `pending` row survives a
/// restart and is reconcilable by construction; an `await` inside a handler is neither. It is the
/// same decision that makes a team run driven by the database rather than by a `JoinHandle`.
///
/// Claimed by compare-and-swap BEFORE the work, so two passes overlapping cannot send one email
/// twice. `ingest_director` uses the identical pattern for the identical reason.
pub async fn execute_due_actions(state: &AppState) {
    let due: Vec<TeamAction> = match sqlx::query_as(DUE_ACTIONS_SQL).fetch_all(&state.pool).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "could not read the team actions waiting to be done");
            return;
        }
    };

    for action in due {
        // The claim. A row moved to `working` by another pass is skipped here, and the pass that
        // moved it owns it — an email that may already have gone must not be sent again.
        let claimed = sqlx::query(
            "UPDATE team_actions SET state = 'working' WHERE id = ? AND state = 'pending'",
        )
        .bind(action.id)
        .execute(&state.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .unwrap_or(false);
        if !claimed {
            continue;
        }

        let outcome = perform(state, &action).await;
        let (final_state, error) = match &outcome {
            Ok(()) => ("done", None),
            Err(why) => ("failed", Some(why.clone())),
        };
        if let Err(error) = sqlx::query(
            "UPDATE team_actions SET state = ?, error = ?, executed_at = ? WHERE id = ?",
        )
        .bind(final_state)
        .bind(error.as_deref())
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(action.id)
        .execute(&state.pool)
        .await
        {
            // The action HAPPENED and the record says `working`. Loud, because the next pass will
            // not retry it — `working` matches no claim — and a human reading the row needs to know
            // why it is stuck there.
            tracing::error!(
                action = action.id,
                %error,
                "a team action was performed and its outcome could not be recorded"
            );
            continue;
        }

        // The feed is where a person reads what the daemon did while nobody was looking, and both
        // endings belong in it: a `done` nobody sees is an email that went out unannounced.
        let _ = crate::feed::append(
            &state.pool,
            None,
            "team_action",
            &match &outcome {
                Ok(()) => format!("a department's `{}` was carried out", action.kind),
                Err(why) => format!("a department's `{}` failed: {why}", action.kind),
            },
            None,
            Some(&crate::feed::Subject::TeamRun(action.team_run_id.clone())),
        )
        .await;
    }
}

/// The four lines that actually touch the world. Everything above decides whether to reach here.
///
/// Each arm delegates: `team.rs` knows what was asked and who may ask it, and knows nothing about
/// how a message is submitted or where a folder lives. The `Err` string is what a person reads
/// beside a `failed` row the morning after, so it says what did not happen rather than which
/// function returned what.
async fn perform(state: &AppState, action: &TeamAction) -> Result<(), String> {
    let payload: serde_json::Value = serde_json::from_str(&action.payload)
        .map_err(|error| format!("the stored request could not be read back: {error}"))?;
    let text = |field: &str| -> String {
        payload
            .get(field)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    };

    match action.kind.as_str() {
        "send_email" => crate::integrations::mailsend::send(
            state,
            &crate::integrations::mailsend::SendRequest {
                to: text("to"),
                subject: text("subject"),
                body: text("body"),
            },
        )
        .await
        .map_err(|failure| failure.to_string()),

        "file_document" => {
            // The one action that leaves the sandbox the pillar built: it writes into the owner's
            // files root rather than into the run's own folder, which is the whole point — a
            // delivery nobody opens has not been delivered. Contained by `files::resolve_within`
            // against that root, the same check the Files tab makes, and by nothing else.
            let root = files_root(state)?;
            let target = crate::files::resolve_within(&root, &text("path"))
                .map_err(|_| "that path is not inside the files folder".to_owned())?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("the folder could not be made: {error}"))?;
            }
            crate::storage::write_atomic(&target, text("content").as_bytes())
                .map_err(|error| format!("the file could not be written: {error}"))
        }

        "calendar_event" => {
            // Parsed again rather than trusted, though `validate_payload` already parsed it: the row
            // has been sitting in a table since, and this is the last moment before it becomes an
            // event somebody's week is arranged around.
            let starts_at_local = chrono::NaiveDateTime::parse_from_str(
                &text("starts_at_local"),
                crate::integrations::calendar::LOCAL_FORMAT,
            )
            .map_err(|_| "the stored start time is not a date and time".to_owned())?;
            let tz: chrono_tz::Tz = text("tz")
                .parse()
                .map_err(|_| "the stored timezone is not one".to_owned())?;
            let minutes = payload
                .get("duration_minutes")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            if !crate::integrations::calendar::duration_in_bounds(minutes) {
                return Err("the stored duration is not one the calendar accepts".to_owned());
            }
            crate::integrations::calendar::insert_event(
                &state.pool,
                &text("title"),
                starts_at_local,
                minutes,
                tz,
                // The source column says WHO put it there, which is what a person scanning their
                // week wants to know about an entry they do not remember making.
                "team",
                None,
                None,
            )
            .await
            .map(|_| ())
            .map_err(|error| format!("the event could not be written: {error}"))
        }

        other => Err(format!("nobody can do `{other}`")),
    }
}

/// A rejected proposal closes its action.
///
/// `failed` with `error = 'rejected'` and not a `rejected` state of its own. A third state would say
/// the same thing `proposals.status` already says, in a second place, and the two would eventually
/// disagree — while `state` here answers only one question: is there anything left to do about this
/// row? There is not.
pub async fn refuse_action(pool: &sqlx::SqlitePool, proposal_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE team_actions SET state = 'failed', error = 'rejected', executed_at = ?
          WHERE proposal_id = ? AND state = 'pending'",
    )
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(proposal_id)
    .execute(pool)
    .await
    .map(|_| ())
}
