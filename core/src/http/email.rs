//! Mail, contacts and calendar (`/email/*`, `/contacts/*`, `/calendar/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/email/cursor", get(get_email_cursor))
        .route("/email/triage", post(post_email_triage))
        .route("/email/queue", get(get_email_queue))
        // Admin-only by construction: absent from BOTH scope tables in `auth.rs`, which is where
        // the reason is written down.
        .route(
            "/email/send",
            post(crate::integrations::mailsend::post_email_send),
        )
        // The one route whose legitimate payload outgrows axum's 2 MB default. A full batch is
        // 200 messages of up to 32 KiB of body each (the sidecar's own `MaxPerBatch` and
        // `MaxBodyBytes`), so ~6.4 MB of text before subjects, headers and JSON escaping — the
        // ordinary shape of a first sync, not an attack. Rejecting it at the transport was
        // self-inflicted deadlock rather than a limit: the 413 stopped the cursor advancing, so
        // the sidecar re-sent the identical batch every five minutes for as long as the mailbox
        // stayed that busy, and `MAX_MESSAGES_PER_BATCH` never ran because the body never reached
        // the handler. Sized with headroom over the sidecar's ceiling and applied only here, so
        // every other route keeps the tighter default.
        .route(
            "/email/incoming",
            post(post_email_incoming).layer(DefaultBodyLimit::max(EMAIL_BATCH_BODY_LIMIT)),
        )
        // Static segments win over `{id}` in matchit, so the three routes above stay reachable.
        .route("/email/{id}", get(get_email))
        .route(
            "/email/{id}/attachments/{position}",
            get(get_email_attachment),
        )
        // Static `/attachments` and the parameterised `/attachments/{position}` coexist; matchit
        // prefers the literal, so the bulk routes never shadow a single one.
        .route("/email/{id}/attachments", get(get_email_attachments))
        .route(
            "/email/{id}/attachments/save-all",
            post(post_email_attachments_save_all),
        )
        .route(
            "/email/{id}/attachments/{position}/save",
            post(post_email_attachment_save),
        )
        .route("/email/{id}/requeue", post(post_email_requeue))
        // The address travels in the body rather than the path: an email address is not a safe path
        // segment, and encoding one into a route only to decode it again buys nothing here.
        .route("/contacts", get(get_contacts))
        .route("/contacts/verdict", post(post_sender_verdict))
        .route("/contacts/unmerge", post(post_contact_unmerge))
        .route("/contacts/merges", get(get_contact_merges))
        // The literal `/calendar/busy` and `/calendar/config` coexist with no `{id}` sibling at
        // that depth, so no shadowing question arises here — unlike `/runs/awaiting-approval`.
        .route(
            "/calendar/events",
            get(crate::integrations::calendar::list_events)
                .post(crate::integrations::calendar::create_event),
        )
        .route(
            "/calendar/events/{id}",
            axum::routing::delete(crate::integrations::calendar::delete_event),
        )
        .route(
            "/calendar/events/{id}/cancel",
            post(crate::integrations::calendar::cancel_occurrence),
        )
        .route(
            "/calendar/events/{id}/move",
            post(crate::integrations::calendar::move_occurrence),
        )
        .route(
            "/calendar/busy",
            get(crate::integrations::calendar::get_busy),
        )
        .route(
            "/calendar/config",
            get(crate::integrations::calendar::get_config),
        )
}

#[derive(Deserialize)]
pub(super) struct MailboxQuery {
    pub(super) mailbox: String,
}

/// The sidecar's delivery envelope (spec §4.3).
#[derive(Deserialize)]
pub(super) struct EmailIncomingRequest {
    pub(super) mailbox: String,
    pub(super) uidvalidity: i64,
    /// The highest uid the sidecar LOOKED AT, which is what lets the cursor move past a message it
    /// could not read. Not the same as the highest uid delivered.
    pub(super) max_uid_examined: i64,
    /// Absent, `null` and `""` all mean inbound — see `absent_or_null_is_empty` above for why the
    /// empty case has to be spelled out: Go writes an unset string field as `""` rather than
    /// omitting it, and `daemon.Batch.Direction` has no `omitempty`.
    #[serde(default)]
    pub(super) direction: Option<String>,
    #[serde(default, deserialize_with = "absent_or_null_is_empty")]
    pub(super) skipped: Vec<crate::integrations::email::SkippedMessage>,
    #[serde(default, deserialize_with = "absent_or_null_is_empty")]
    pub(super) messages: Vec<crate::integrations::email::IncomingMessage>,
}

/// Paging belongs to the sidecar; a batch this large means it stopped doing its job, and the
/// núcleo should say so rather than quietly ingest whatever arrives.
pub(super) const MAX_MESSAGES_PER_BATCH: usize = 200;

/// Body ceiling for `/email/incoming`, in bytes.
///
/// `MAX_MESSAGES_PER_BATCH` x the sidecar's 32 KiB per-body cap is ~6.4 MB of text; doubling it
/// covers subjects, addresses, headers and JSON escaping without turning the route into an
/// unbounded sink. It is a backstop, not the real limit — the count check in the handler is.
pub(super) const EMAIL_BATCH_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// How much of the mailbox the queue hands back. Generous because a first sync pulls a week at
/// once, and a list that silently stops at its limit is indistinguishable from mail that never
/// arrived — which is the exact confusion this pillar has already cost once.
pub(super) const EMAIL_QUEUE_LIMIT: i64 = 200;

pub(super) async fn get_email_cursor(
    State(state): State<AppState>,
    Query(query): Query<MailboxQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let cursor = crate::integrations::email::get_cursor(&state.pool, &query.mailbox)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(match cursor {
        Some(cursor) => serde_json::json!({
            "uidvalidity": cursor.uidvalidity,
            "last_uid": cursor.last_uid,
        }),
        None => serde_json::Value::Null,
    }))
}

pub(super) async fn post_email_incoming(
    State(state): State<AppState>,
    Json(body): Json<EmailIncomingRequest>,
) -> Result<Json<crate::integrations::email::IngestOutcome>, StatusCode> {
    if body.messages.len() > MAX_MESSAGES_PER_BATCH {
        return Err(StatusCode::BAD_REQUEST);
    }
    let direction = match body.direction.as_deref().map(str::trim) {
        None | Some("") => crate::integrations::contacts::MessageDirection::Inbound,
        Some(value) if value.eq_ignore_ascii_case("inbound") => {
            crate::integrations::contacts::MessageDirection::Inbound
        }
        Some(value) if value.eq_ignore_ascii_case("outbound") => {
            crate::integrations::contacts::MessageDirection::Outbound
        }
        // Defaulting would store the user's sent bodies as inbound; refusal keeps the cursor in
        // place so the retry makes the mailbox complain instead of quietly mis-filing them.
        Some(_) => return Err(StatusCode::BAD_REQUEST),
    };
    // Ingestion is one transaction over untrusted content and it moves the cursor. A client that
    // disconnects mid-request must not be able to leave that half-done.
    let pool = state.pool.clone();
    let retain_bodies_days = state.email.retain_bodies_days;
    // Resolved here and never accepted from the body, for the same reason the direction is not: a
    // sidecar that could name the owner could tell the núcleo that a stranger's message was the
    // owner's own, and `outbound_ever` is what `priority.rs` reads to lower a brake.
    let owner_address = state.email.username.clone();
    uncancellable(async move {
        crate::integrations::email::ingest_batch(
            &pool,
            direction,
            &body.mailbox,
            body.uidvalidity,
            body.max_uid_examined,
            &body.skipped,
            &body.messages,
            &owner_address,
            retain_bodies_days,
            chrono::Utc::now(),
        )
        .await
        .map(Json)
        // Logged, not discarded. `|_|` here made a failed ingest into a 500 with an empty body and
        // no line anywhere: the sidecar reported `daemon returned 500:` on every cycle, the cursor
        // stayed put, and the mailbox went eight days unread with nothing in the log to say why.
        // The message names the mailbox and the batch, because the two things worth knowing next
        // are which mailbox stalled and whether it is one message or the whole batch that cannot
        // land. It never names what a message SAYS — see `redact.rs`.
        .map_err(|error| {
            tracing::warn!(
                mailbox = %body.mailbox,
                batch = body.messages.len(),
                %error,
                "email ingest failed — the cursor stays put and the sidecar will retry"
            );
            StatusCode::INTERNAL_SERVER_ERROR
        })
    })
    .await?
}

/// Triage what is waiting, now.
///
/// Reading the mailbox happens on its own because it is free; spending a run does not. This is the
/// request — so it launches a batch immediately rather than raising a flag and waiting up to a
/// minute for the loop's own tick to notice.
///
/// It answers with what it started, not with verdicts: a run takes minutes, and holding an HTTP
/// request open for it would only make the caller's timeout the deadline for the mail.
pub(super) async fn post_email_triage(
    State(state): State<AppState>,
) -> Result<Json<crate::governance::triage::TriageOutcome>, StatusCode> {
    if !state.email.armed.load(std::sync::atomic::Ordering::Relaxed) {
        // Not an error the caller can fix by retrying: the pillar is off, or its barrier failed
        // verification at startup and it is deliberately staying off.
        return Ok(Json(crate::governance::triage::TriageOutcome {
            queued: 0,
            run_id: None,
            reason: Some("the email pillar is not armed".to_string()),
        }));
    }

    // Launching writes a claim across the batch's rows; a client that disconnects must not leave
    // that half-written.
    let state = state.clone();
    uncancellable(async move {
        let mut loop_state = crate::governance::triage::LoopState::default();
        crate::governance::triage::triage_now(&state, &mut loop_state, chrono::Utc::now()).await
    })
    .await
    .map(Json)
}

/// How many correspondents the roster returns. Enough to find anyone; short enough to draw.
pub(super) const CONTACTS_LIMIT: i64 = 200;

/// Who writes to you, busiest first, with each one's standing decision.
///
/// `contacts.rs` has recorded this since it existed — every inbound message updates a count and a
/// last-seen, and `priority.rs` reads it to tell a stranger from someone you know. None of it was
/// readable: the module still carries `#[allow(dead_code)]` on three fields "consumed by the later
/// contact display surface", and this is that surface for the part that is actually finished.
///
/// Deliberately NOT the merged view. `propose_merges` and `merge` exist and nothing in production
/// calls either, so two addresses belonging to one person are still two rows — and reporting them
/// as one would be reporting a judgement nobody has made.
pub(super) async fn get_contacts(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::integrations::contacts::Correspondent>>, StatusCode> {
    crate::integrations::contacts::roster(&state.pool, CONTACTS_LIMIT)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading the contact roster failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Suggestions that two addresses are one person, waiting on an answer.
///
/// Its own route rather than a slice of `/proposals`, because the two kinds share a table and
/// nothing else — see `contacts::pending_merges`. It also puts the question where the context is:
/// deciding whether two addresses are the same person is a thing you do while looking at your
/// correspondents, not while looking at a queue of paused runs.
pub(super) async fn get_contact_merges(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::integrations::contacts::MergeSuggestion>>, StatusCode> {
    crate::integrations::contacts::pending_merges(&state.pool)
        .await
        .map(Json)
        .map_err(|error| {
            tracing::warn!(%error, "reading pending contact merges failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Deserialize)]
pub(super) struct UnmergeRequest {
    pub(super) address: String,
}

/// Splits one address back out into a person of its own.
///
/// The undo for an approved merge, and the reason approving one is a safe thing to offer: the join
/// is a pointer move, so undoing it restores exactly what was there — counters included, since a
/// merge never moved them. Without this route the merge was still exact and still undoable in
/// principle, and unreachable in practice.
///
/// Answers 204 whether or not the address was merged with anything. Splitting an address that is
/// already alone is the state the caller asked for, and a caller that had to tell those apart would
/// be handling somebody else's bookkeeping.
pub(super) async fn post_contact_unmerge(
    State(state): State<AppState>,
    Json(body): Json<UnmergeRequest>,
) -> Result<StatusCode, StatusCode> {
    let known: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM contact_addresses WHERE address = ?")
            .bind(crate::integrations::contacts::normalize_address(
                &body.address,
            ))
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if known.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    crate::integrations::contacts::unmerge(&state.pool, &body.address)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| {
            tracing::warn!(%error, "splitting a contact failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Deserialize)]
pub(super) struct SenderVerdictRequest {
    pub(super) address: String,
    /// `pin`, `mute`, or `null` to withdraw the decision entirely.
    pub(super) verdict: Option<String>,
}

/// Records what a person has decided about a sender, once, for all their future mail.
///
/// This is the only writer of `contact_overrides` outside a test. The table has been read by
/// `priority::adjust` since it existed — a pin outranks the model and a mute outranks it the other
/// way — so until now the highest-authority rule in triage was one nothing could set.
///
/// An unknown verdict is refused rather than stored. `priority::adjust` falls through for anything
/// it does not recognise, which means a typo would be accepted, saved, and then do nothing at all
/// for as long as it sat there; a 400 now is the only moment that mistake is visible.
///
/// An address nobody has written from is a 404 for a related reason: a contact exists because mail
/// arrived, and inventing one here would let a mistyped address become a permanent row that never
/// matches anything and never explains why.
pub(super) async fn post_sender_verdict(
    State(state): State<AppState>,
    Json(body): Json<SenderVerdictRequest>,
) -> Result<StatusCode, StatusCode> {
    if let Some(verdict) = body.verdict.as_deref()
        && !crate::governance::priority::is_known_verdict(verdict)
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    match crate::integrations::contacts::set_verdict(
        &state.pool,
        &body.address,
        body.verdict.as_deref(),
    )
    .await
    {
        Ok(crate::integrations::contacts::VerdictOutcome::Applied) => Ok(StatusCode::NO_CONTENT),
        Ok(crate::integrations::contacts::VerdictOutcome::UnknownAddress) => {
            Err(StatusCode::NOT_FOUND)
        }
        Err(error) => {
            tracing::warn!(%error, "recording a sender verdict failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// One attachment, described. The bytes are not here and are not stored (migration 0021).
#[derive(serde::Serialize, sqlx::FromRow)]
pub(super) struct EmailAttachment {
    pub(super) position: i64,
    pub(super) filename: Option<String>,
    pub(super) mime_type: Option<String>,
    pub(super) size_bytes: i64,
}

/// One message in full.
///
/// Separate from the queue's row because the body is the expensive and sensitive half: a mailbox
/// list has no business carrying a mailbox's worth of third-party text, and this way opening a
/// message is the moment that text is read out of the database, not a side effect of drawing a list.
#[derive(serde::Serialize, sqlx::FromRow)]
pub(super) struct EmailDetail {
    pub(super) id: i64,
    pub(super) from_addr: String,
    pub(super) from_name: Option<String>,
    pub(super) subject: Option<String>,
    pub(super) received_at: String,
    pub(super) triage_class: Option<String>,
    pub(super) triage_summary: Option<String>,
    pub(super) triaged_at: Option<String>,
    /// What the model answered; NULL when the row was never triaged.
    pub(super) model_class: Option<String>,
    /// Which rule decided the stored class; NULL when none fired or the row was never triaged.
    pub(super) priority_rule: Option<String>,
    /// NULL once retention has pruned it (§7.2), which is a state the reader must show rather than
    /// mistake for an empty message.
    pub(super) body_text: Option<String>,
    pub(super) has_attachments: i64,
    /// The standing human decision about this sender — `pin`, `mute`, or none — the same field the
    /// queue row carries. The message page is where that decision is changed, and a control that
    /// cannot show which way it is set can only be pressed blind.
    pub(super) sender_verdict: Option<String>,
}

#[derive(serde::Serialize)]
pub(super) struct EmailDetailResponse {
    #[serde(flatten)]
    pub(super) message: EmailDetail,
    pub(super) attachments: Vec<EmailAttachment>,
}

/// One message, body included — what "open the mail" reads.
pub(super) async fn get_email(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<EmailDetailResponse>, StatusCode> {
    let mut message: EmailDetail = sqlx::query_as(
        "SELECT id, from_addr, from_name, subject, received_at, triage_class, triage_summary,
                triaged_at, model_class, priority_rule, body_text, has_attachments,
                NULL AS sender_verdict
           FROM emails WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Matched through `contacts::normalize_address`, for the reason `get_email_queue` gives: one
    // definition of "the same sender", applied in Rust, rather than a second one written in SQL.
    message.sender_verdict = sqlx::query_scalar(
        "SELECT overrides.verdict
           FROM contact_overrides AS overrides
           JOIN contact_addresses AS addresses
             ON addresses.contact_id = overrides.contact_id
          WHERE addresses.address = ?",
    )
    .bind(crate::integrations::contacts::normalize_address(
        &message.from_addr,
    ))
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let attachments: Vec<EmailAttachment> = sqlx::query_as(
        "SELECT position, filename, mime_type, size_bytes
           FROM email_attachments WHERE email_id = ? ORDER BY position",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(EmailDetailResponse {
        message,
        attachments,
    }))
}

/// How long the núcleo waits for the sidecar to go and get a file. Generous because the sidecar
/// opens a fresh TLS connection and may be fetching 25 MB over someone's home connection.
pub(super) const ATTACHMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// One attachment's bytes, fetched from the mailbox at the moment they are asked for.
///
/// Nothing is cached on the way through. The file exists in exactly one place — the mailbox it
/// arrived in — and keeping a copy would mean a stranger's executable sitting on disk because
/// someone once clicked a filename.
pub(super) async fn get_email_attachment(
    State(state): State<AppState>,
    Path((id, position)): Path<(i64, i64)>,
) -> Result<axum::response::Response, StatusCode> {
    let (bytes, filename) = fetch_attachment(&state, id, position).await?;

    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                content_disposition(&filename),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// One attachment as the sidecar hands it over in bulk.
#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct BulkAttachment {
    pub(super) position: i64,
    #[serde(default)]
    pub(super) filename: Option<String>,
    #[serde(default)]
    pub(super) mime_type: Option<String>,
    pub(super) size_bytes: i64,
    pub(super) content_base64: String,
    /// True when `content_base64` is only the first part of the file (`size_bytes` is the true
    /// size). Absent from older sidecars, which means complete.
    #[serde(default)]
    pub(super) truncated: bool,
}

/// Every attachment of one message, read in a single pass over the mailbox.
///
/// One route rather than the caller looping, because each single fetch downloads the WHOLE message:
/// a loop over eight attachments pulled the same eight files eight times, over eight connections.
pub(super) async fn get_email_attachments(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<BulkAttachment>>, StatusCode> {
    fetch_all_attachments(&state, id).await.map(Json)
}

pub(super) async fn fetch_all_attachments(
    state: &AppState,
    id: i64,
) -> Result<Vec<BulkAttachment>, StatusCode> {
    let row: Option<(i64, i64, String)> =
        sqlx::query_as("SELECT uid, uidvalidity, mailbox FROM emails WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (uid, uidvalidity, mailbox) = row.ok_or(StatusCode::NOT_FOUND)?;
    let token = sidecar_bearer(state, &mailbox)?;

    let client = reqwest::Client::builder()
        .timeout(ATTACHMENT_TIMEOUT)
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response = client
        .get(format!(
            "http://{}{}",
            crate::sidecar::EMAIL_FETCH_ADDR,
            attachment_query("/attachments", uid, uidvalidity, None)
        ))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if !response.status().is_success() {
        return Err(StatusCode::BAD_GATEWAY);
    }
    response
        .json::<Vec<BulkAttachment>>()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)
}

#[derive(serde::Serialize)]
pub(super) struct SaveAllOutcome {
    pub(super) folder: String,
    /// The names actually stored, in the order the message carries them.
    pub(super) filenames: Vec<String>,
}

/// Files every attachment of one message into a folder.
///
/// Partial success is not a state this reports: the write happens after all the bytes are in hand,
/// so the failure that matters — the mailbox being unreachable — happens before anything lands.
/// What can still fail per file is the disk, and a folder holding three of eight files with no word
/// about the other five is worse than a refusal.
pub(super) async fn post_email_attachments_save_all(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<SaveAttachmentRequest>,
) -> Result<Json<SaveAllOutcome>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();
    let attachments = fetch_all_attachments(&state, id).await?;
    // A cut-off file would be filed as if it were whole. Refuse before anything is written, in the
    // same spirit as the no-partial-success rule above.
    if attachments.iter().any(|a| a.truncated) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    let decoded: Vec<(String, Vec<u8>)> = attachments
        .into_iter()
        .map(|attachment| {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&attachment.content_base64)
                .map_err(|_| StatusCode::BAD_GATEWAY)?;
            Ok((attachment.filename.unwrap_or_default(), bytes))
        })
        .collect::<Result<_, StatusCode>>()?;

    let saved = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            let mut filenames = Vec::with_capacity(decoded.len());
            for (filename, bytes) in &decoded {
                filenames.push(crate::files::write_file(
                    &root,
                    &body.folder,
                    filename,
                    bytes,
                )?);
            }
            Ok::<_, crate::files::PathError>(SaveAllOutcome {
                folder: body.folder,
                filenames,
            })
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    saved.map(Json).map_err(folder_status)
}

/// The path and query for one sidecar attachment request. It names the UIDVALIDITY the message was
/// stored under, so the sidecar can refuse a uid that a reset has since given to another message.
fn attachment_query(path: &str, uid: i64, uidvalidity: i64, position: Option<i64>) -> String {
    match position {
        Some(position) => format!("{path}?uid={uid}&uidvalidity={uidvalidity}&position={position}"),
        None => format!("{path}?uid={uid}&uidvalidity={uidvalidity}"),
    }
}

/// The key to present to the email sidecar for a message stored under `mailbox`.
///
/// The sidecar's OWN key, never the control token (the process parses MIME written by strangers;
/// see `mailsend::send`). It also only ever selects the configured mailbox, so a message stored
/// under any other one cannot be fetched by uid and is reported missing rather than mis-served.
fn sidecar_bearer<'a>(state: &'a AppState, mailbox: &str) -> Result<&'a str, StatusCode> {
    if mailbox != state.email.mailbox {
        return Err(StatusCode::NOT_FOUND);
    }
    state
        .email
        .sidecar_token
        .as_deref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

/// Asks the sidecar for one attachment's bytes, and reports the name it was described under.
///
/// Shared by the route that hands a file to a person and the one that files it into a folder,
/// because those must never diverge on WHICH file they mean.
pub(super) async fn fetch_attachment(
    state: &AppState,
    id: i64,
    position: i64,
) -> Result<(Vec<u8>, String), StatusCode> {
    let row: Option<(i64, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT emails.uid, emails.uidvalidity, emails.mailbox, email_attachments.filename
           FROM email_attachments
           JOIN emails ON emails.id = email_attachments.email_id
          WHERE email_attachments.email_id = ? AND email_attachments.position = ?",
    )
    .bind(id)
    .bind(position)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (uid, uidvalidity, mailbox, filename) = row.ok_or(StatusCode::NOT_FOUND)?;
    let token = sidecar_bearer(state, &mailbox)?;

    let client = reqwest::Client::builder()
        .timeout(ATTACHMENT_TIMEOUT)
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response = client
        .get(format!(
            "http://{}{}",
            crate::sidecar::EMAIL_FETCH_ADDR,
            attachment_query("/attachment", uid, uidvalidity, Some(position))
        ))
        .bearer_auth(token)
        .send()
        .await
        // The sidecar not answering is not the same as the file not existing, and telling a person
        // "not found" when the truth is "nothing went to look" sends them hunting in the mailbox.
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    if !response.status().is_success() {
        return Err(if response.status() == reqwest::StatusCode::NOT_FOUND {
            // The stored description and the live message disagree: the mail was deleted or
            // replaced since it was read.
            StatusCode::NOT_FOUND
        } else if response.status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
            // The attachment is over the sidecar's cap and it refused to hand over a cut-off file.
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_GATEWAY
        });
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    Ok((bytes.to_vec(), filename.unwrap_or_default()))
}

#[derive(Deserialize)]
pub(super) struct SaveAttachmentRequest {
    /// Which folder under the root. Empty means the root itself.
    #[serde(default)]
    pub(super) folder: String,
}

/// Fetches an attachment and files it into the folder — the one path where a stranger's bytes are
/// written to this disk, and it happens because a person asked for it by name.
pub(super) async fn post_email_attachment_save(
    State(state): State<AppState>,
    Path((id, position)): Path<(i64, i64)>,
    Json(body): Json<SaveAttachmentRequest>,
) -> Result<Json<SavedFile>, StatusCode> {
    let root = files_root(&state)?.to_path_buf();
    let (bytes, filename) = fetch_attachment(&state, id, position).await?;

    // Blocking file I/O off the async runtime, and uncancellable: a client that disconnects
    // mid-write must not leave half a file behind under a name that says it is whole.
    let saved = uncancellable(async move {
        tokio::task::spawn_blocking(move || {
            crate::files::write_file(&root, &body.folder, &filename, &bytes).map(|stored| {
                SavedFile {
                    filename: stored,
                    folder: body.folder,
                }
            })
        })
        .await
    })
    .await?
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    saved.map(Json).map_err(folder_status)
}

/// The mail the pillar knows about: what is waiting, and what it most recently said.
#[derive(Deserialize)]
pub(super) struct EmailQueueQuery {
    pub(super) q: Option<String>,
}

pub(super) async fn get_email_queue(
    State(state): State<AppState>,
    Query(query): Query<EmailQueueQuery>,
) -> Result<Json<Vec<crate::integrations::email::QueuedEmail>>, StatusCode> {
    // Newest arrival first — the order a mailbox is read in. Deliberately NOT by triage time: a
    // verdict landing now would otherwise drag a week-old message to the top, and a list that
    // reorders itself while you read it is one you lose your place in. Waiting mail is marked
    // rather than floated for the same reason; the count and the button live above the list.
    //
    // This list is the mail that came in. The user's own sent mail is held for what it says about a
    // correspondent, not read back to them; filtering it also stops sent mail consuming
    // `EMAIL_QUEUE_LIMIT` slots.
    //
    // Sorting `received_at` as text is a chronological sort because the sidecar normalises the
    // server's INTERNALDATE to UTC (`...Z`), so every value shares one offset. `id` breaks ties
    // within a second, which a bulk delivery produces routinely.
    //
    // `failed` sorts with the rest rather than being hidden: it is the class most likely to be
    // requeued, so it is the one that must stay findable.
    // Searching narrows this list rather than being a list of its own, so the ordering, the limit
    // and the `inbound` filter above are stated once and hold either way. 0058 indexes only what
    // survives triage — sender, subject, and the locally-written summary — so a search for a word
    // that was only ever in a body finds nothing, which is the correct answer once the body is gone
    // rather than a gap in the index.
    let search = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());

    // The FTS expression is derived from the trimmed text inside `queue_search`.
    let queue = crate::integrations::email::queue_search(&state.pool, EMAIL_QUEUE_LIMIT, search)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(queue))
}

pub(super) async fn post_email_requeue(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, StatusCode> {
    use crate::integrations::email::RequeueError;
    let pool = state.pool.clone();
    uncancellable(async move { crate::integrations::email::requeue(&pool, id).await })
        .await?
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(|error| match error {
            RequeueError::UnknownEmail => StatusCode::NOT_FOUND,
            RequeueError::BodyPurged | RequeueError::ClaimedByRun(_) => StatusCode::CONFLICT,
        })
}

#[cfg(test)]
mod attachment_request_tests {
    use super::*;

    #[test]
    fn the_attachment_request_carries_the_uidvalidity() {
        assert_eq!(
            attachment_query("/attachment", 5, 77, Some(2)),
            "/attachment?uid=5&uidvalidity=77&position=2"
        );
        assert_eq!(
            attachment_query("/attachments", 5, 77, None),
            "/attachments?uid=5&uidvalidity=77"
        );
    }
}
