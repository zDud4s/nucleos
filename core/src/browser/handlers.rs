//! HTTP handlers, request bodies, and error mappers for the browser pillar.

use super::*;

// ---------------------------------------------------------------------------------------------
// The handlers. `/browser/*`, mounted in `http.rs`.
//
// None of them takes a `requester` or a `surface` from the request body. Both are derived here, for
// `web.rs`'s reason and one more: the shell and an agent authenticate with the same token, so a
// field on the wire saying which one is calling would be a permission the caller grants itself —
// and the permission in question is whether a page may run inside the profile that holds the
// owner's logins.
// ---------------------------------------------------------------------------------------------

/// The project a `POST /browser/open` names, if any.
///
/// On the wire it is exactly an optional string (`serde(transparent)`): absent or `null` is
/// `None`. It is a newtype rather than a bare `Option<String>` only so a caller that does name a
/// project can still write `project_id: "acme".into()`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct RequestedProject(pub Option<String>);

impl RequestedProject {
    /// The named project, borrowed; `None` when the caller named none.
    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

impl From<&str> for RequestedProject {
    fn from(project: &str) -> Self {
        Self(Some(project.to_owned()))
    }
}

impl From<String> for RequestedProject {
    fn from(project: String) -> Self {
        Self(Some(project))
    }
}

impl From<Option<String>> for RequestedProject {
    fn from(project: Option<String>) -> Self {
        Self(project)
    }
}

#[derive(Debug, Deserialize)]
pub struct OpenBody {
    /// Absent or null: the calling turn's stamped project, else no project ([`resolve_scope`]).
    #[serde(default)]
    pub project_id: RequestedProject,
    pub url: String,
    /// Still deserialised so an older caller does not fail, but IGNORED: the owning run comes from
    /// the daemon-set header only (spec §4.0).
    #[serde(default)]
    pub run_id: Option<i64>,
    /// Open a window the person can see, with a chat panel on the right.
    #[serde(default)]
    pub visible: bool,
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
    /// Upload's second argument: what the file is called. A NAME, judged as one by the sidecar
    /// before anything touches a disk — the daemon does not resolve it, because resolving is
    /// deciding and there is nothing here for it to decide against.
    #[serde(default)]
    pub filename: String,
}

/// The scope a browser open runs under.
///
/// An explicit id is kept as given (and validated downstream exactly as before). An absent one
/// resolves to the calling run's stamped `runs.project_id`; a run with no project, no run at all,
/// or a lookup that fails is [`NO_PROJECT`].
pub(crate) async fn resolve_scope(
    pool: &sqlx::SqlitePool,
    run_id: Option<i64>,
    requested: Option<&str>,
) -> String {
    if let Some(requested) = requested {
        return requested.to_owned();
    }
    let Some(run_id) = run_id else {
        return NO_PROJECT.to_owned();
    };
    let stamped: Result<Option<Option<String>>, sqlx::Error> =
        sqlx::query_scalar("SELECT project_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await;
    match stamped {
        Ok(Some(Some(project))) if !project.is_empty() => project,
        Ok(_) => NO_PROJECT.to_owned(),
        Err(error) => {
            tracing::warn!(%error, run = run_id, "could not read the calling run's project; browsing in no project");
            NO_PROJECT.to_owned()
        }
    }
}

/// `POST /browser/open`.
pub async fn post_open(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    chat: Option<axum::Extension<crate::auth::ChatBrowser>>,
    axum::Json(body): axum::Json<OpenBody>,
) -> axum::response::Response {
    let now = chrono::Utc::now();
    // The chat turn the auth door admitted, else the header only (spec §4.0): a `run_id` in the
    // body is a permission the caller would grant itself, so the body field is deserialised for
    // compatibility and never read.
    let run_id = chat
        .as_ref()
        .map(|chat| chat.run_id)
        .or_else(|| crate::door::sending_run_id_of(&headers));
    // No project named: the calling turn's own project, or no project at all.
    let project_id = resolve_scope(&state.pool, run_id, body.project_id.as_deref()).await;
    let ask = Ask {
        project_id: &project_id,
        run_id,
        url: &body.url,
        // The assistant, always, in this version. Spec §6.0b: the autonomous path is a seam and not
        // a road, and the seam is `browser_policy`'s refusal rather than a branch here. When a
        // pillar does reach the browser, it will arrive through its own caller and name itself.
        surface: Surface::Assistant,
        requester: requester_now(&state, now).await,
        now: &now.to_rfc3339(),
    };
    match open_with(&state.pool, &state.browser, ask, body.visible).await {
        Ok(Opened::Session(mut row)) => {
            if let Some(chat) = chat.as_ref() {
                // On failure `owner_of` still falls back to the run's own chat.
                match crate::browser::set_owner_chat(&state.pool, row.id, &chat.chat_id).await {
                    Ok(()) => row.owner_chat_id = Some(chat.chat_id.clone()),
                    Err(error) => {
                        tracing::warn!(%error, session = row.id, "could not record the chat that owns a browser session");
                    }
                }
            }
            if row.visible {
                crate::browser::panel::open_on(&state, &row, &body.url).await;
            }
            axum::Json(row).into_response()
        }
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
    // A read is still a read of THEIR screen. The tree is not pixels, but a login form's tree names
    // the fields and carries their values, so "it is only the accessibility tree" is not a reason to
    // let it through while somebody else is driving.
    //
    // This guard was missing, and the way it was missing is the interesting part: the sidecar's
    // `Human` driver refuses `Snapshot`, `Act` and `Screenshot`, and says in its own comment that
    // "the núcleo already refuses them from its own record (spec §4.4 rule 1), so this is the second
    // layer". The núcleo refused two of the three. Nothing was exploitable — the second layer held —
    // but the sentence vouching for the first one was false, which is precisely the state
    // [`post_act`] warns about: the daemon-side guard exists for the case where the two processes
    // DISAGREE about who is driving, and a layer that is only believed in cannot do that.
    if row.mode != mode::AGENT {
        return not_the_agents(&row, "what is on its screen is theirs");
    }
    if let Some(refusal) = owner_has_control(&state, &row, "what is on its screen is theirs") {
        return refusal;
    }
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
        Ok(snapshot) => {
            let mut value = serde_json::to_value(&snapshot).unwrap_or_default();
            // Told once, to the first read after the person handed the wheel back: the page may have
            // changed under the agent while it was not driving.
            if state.browser.seats.take_returned(row.id)
                && let Some(object) = value.as_object_mut()
            {
                object.insert("wheel_returned".into(), serde_json::Value::Bool(true));
            }
            tell_panel_once(&state, row.id, &mut value);
            axum::Json(value).into_response()
        }
        Err(error) => browser_error(error),
    }
}

/// What the person typed in the panel and the note they left with the wheel, each told once to the
/// agent's next read (spec browser-com-painel §4.3).
pub(super) fn tell_panel_once(state: &AppState, id: i64, value: &mut serde_json::Value) {
    let messages = state.browser.seats.take_panel_messages(id);
    let note = state.browser.seats.take_returned_note(id);
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if !messages.is_empty() {
        object.insert("panel_messages".into(), serde_json::json!(messages));
    }
    if let Some(note) = note {
        object.insert("wheel_note".into(), serde_json::Value::String(note));
    }
}

/// The refusal an agent gets for a session that is not in its hands.
///
/// `person-driving` while a person has the wheel (either seat), `wheel-requested` for the mode in
/// which the wheel has only been asked for. The shape is a fence refusal, so an agent reads it with
/// the vocabulary it already has.
pub(super) fn not_the_agents(row: &SessionRow, what: &str) -> axum::response::Response {
    let consequence = if row.mode == mode::HUMAN {
        "person-driving"
    } else {
        "wheel-requested"
    };
    axum::Json(serde_json::json!({
        "outcome": "refused",
        "refusal": {
            "consequence": consequence,
            "detail": format!("this session is {}, so {what}", row.mode),
        },
    }))
    .into_response()
}

/// The refusal an agent gets while the owner holds an embedded session by hand (the shell's own
/// flag, not the wheel mode). `None` when the session is not embedded or the owner has let go.
pub(super) fn owner_has_control(
    state: &AppState,
    row: &SessionRow,
    what: &str,
) -> Option<axum::response::Response> {
    if !state.browser.embedded.is_human(&row.sidecar_id) {
        return None;
    }
    Some(
        axum::Json(serde_json::json!({
            "outcome": "refused",
            "refusal": {
                "consequence": "owner_has_control",
                "detail": format!("the owner is using this session by hand, so {what}"),
            },
        }))
        .into_response(),
    )
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
        return not_the_agents(&row, "it is not the agent's to act on");
    }
    if let Some(refusal) = owner_has_control(&state, &row, "it is not the agent's to act on") {
        return refusal;
    }
    match state
        .browser
        .client
        .act(
            &row.sidecar_id,
            &body.kind,
            &body.element_ref,
            &body.text,
            &body.filename,
        )
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
            // Before the answer goes back, and that ordering is the point. What the sidecar reports
            // here is what LEFT the machine as the person, so the record of it must not depend on
            // the caller reading the reply, on the run surviving, or on anything else happening
            // afterwards. The agent works alone inside its grant; this is the whole of what makes
            // that supervisable later.
            if !result.writes.is_empty() {
                let now = chrono::Utc::now().to_rfc3339();
                record_writes(
                    &state.pool,
                    row.id,
                    row.project_id.as_deref(),
                    &result.writes,
                    &now,
                )
                .await;
            }
            if row.visible && result.outcome == "done" {
                let target = if body.element_ref.is_empty() {
                    result.url.as_str()
                } else {
                    body.element_ref.as_str()
                };
                crate::browser::panel::panel_push(
                    &state,
                    &row.sidecar_id,
                    serde_json::json!({
                        "v": 1,
                        "kind": "message",
                        "role": "action",
                        "text": format!("browser: {} {}", body.kind, target).trim_end(),
                        "ts": chrono::Utc::now().to_rfc3339(),
                    }),
                )
                .await;
            }
            let mut value = serde_json::to_value(&result).unwrap_or_default();
            if state.browser.seats.take_returned(row.id)
                && let Some(object) = value.as_object_mut()
            {
                object.insert("wheel_returned".into(), serde_json::Value::Bool(true));
            }
            tell_panel_once(&state, row.id, &mut value);
            axum::Json(value).into_response()
        }
        Err(error) => browser_error(error),
    }
}

/// `POST /browser/look` — the annotated picture, for the agent.
///
/// Refused while a person has the wheel, and this is the sharpest of the wheel refusals. The reason
/// the wheel is handed over is almost always a login, so what is on that screen is a password field
/// with somebody's fingers on it — and a look is precisely the verb that would carry it to a model.
/// The sidecar's `Human` driver refuses it too; neither layer is redundant, for the reason
/// [`post_act`] gives about the two processes disagreeing over who is driving.
///
/// The refusal is shaped like a fence refusal rather than an error, so an agent reads it with the
/// vocabulary it already has.
pub async fn post_look(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<SessionBody>,
) -> axum::response::Response {
    let Some(row) = live_session(&state, body.session_id).await else {
        return gone();
    };
    if row.mode != mode::AGENT {
        return not_the_agents(&row, "what is on its screen is theirs");
    }
    if let Some(refusal) = owner_has_control(&state, &row, "what is on its screen is theirs") {
        return refusal;
    }
    match state.browser.client.look(&row.sidecar_id).await {
        Ok(result) => axum::Json(result).into_response(),
        Err(error) => browser_error(error),
    }
}

/// `POST /browser/screenshot` — pixels, for a person to look at.
///
/// It answers the SHELL, and `/browser/look` above answers the agent. The two are separate routes
/// rather than one with a flag because they differ in everything that follows from the audience: this
/// one is a full-page PNG of whatever is there, unlabelled and unbounded, and it is read by a window
/// a person is looking at.
///
/// This comment used to say the agent could not be shown pixels at all, because `filter_outgoing` in
/// `mcp_tools.rs` redacts text and has no image branch. That reason has not gone away — it is now a
/// price paid on purpose and written down at the branch itself, where somebody deciding whether to
/// widen it will actually be standing. What keeps it bounded is that a look is viewport-only, drawn
/// on a page the fence admitted, and refused outright once a person has the wheel.
pub async fn post_screenshot(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<SessionBody>,
) -> axum::response::Response {
    let Some(row) = live_session(&state, body.session_id).await else {
        return gone();
    };
    // The same refusal [`post_look`] carries, and for a stronger version of the same reason: this
    // one returns raw pixels of whatever is on the screen. The sidecar's `Human` driver singles it
    // out — "the layer that matters most: the page in front of the person during a handover is a
    // login form, with a password half-typed into it."
    if row.mode != mode::AGENT {
        return not_the_agents(&row, "what is on its screen is theirs");
    }
    if let Some(refusal) = owner_has_control(&state, &row, "what is on its screen is theirs") {
        return refusal;
    }
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
pub struct ReadonlyBody {
    pub project_id: String,
    pub origin: String,
}

/// `POST /browser/readonly` — take back one origin's write grant, leaving it readable.
///
/// It carries no boolean, and that absence is the invariant. A body of `{origin, writable}` would be
/// a route that can WIDEN a permission, which is the thing this surface deliberately does not have:
/// the list grows in one place, when a person finishes a login and answers for the chain they just
/// walked (spec §5.2). This one only ever narrows, so there is nothing here for a confused deputy to
/// aim.
pub async fn post_readonly(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<ReadonlyBody>,
) -> axum::response::Response {
    match make_readonly(&state.pool, &body.project_id, &body.origin).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            "no such site, or it was already read-only",
        )
            .into_response(),
        Err(error) => db_error(error),
    }
}

/// `GET /browser/writes/{project_id}` — what agents have submitted in this project's profile.
///
/// Admin scope, like everything else on this surface, and for a sharper reason than the rest: these
/// rows say which forms a person's own logged-in identity was used to submit, which is a more
/// intimate readout than the list of hosts beside it.
pub async fn get_writes(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> axum::response::Response {
    match list_writes(&state.pool, &project_id, 100).await {
        Ok(writes) => axum::Json(writes).into_response(),
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
pub(super) async fn requester_now(
    state: &AppState,
    now: chrono::DateTime<chrono::Utc>,
) -> Requester {
    if crate::attention::owner_is_present(&state.pool, now).await {
        Requester::Owner
    } else {
        Requester::Autonomous
    }
}

/// The session row, if it is still open. A closed one is not addressable: the browser behind it is
/// gone, and answering from the row would describe a page that no longer exists.
pub async fn live_session(state: &AppState, id: i64) -> Option<SessionRow> {
    match session_row(&state.pool, id).await {
        Ok(Some(row)) if row.closed_at.is_none() => Some(row),
        _ => None,
    }
}

pub fn gone() -> axum::response::Response {
    (StatusCode::NOT_FOUND, "no such browsing session").into_response()
}

pub(super) fn db_error(error: sqlx::Error) -> axum::response::Response {
    tracing::error!(%error, "browser database error");
    (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
}

/// The sidecar's failures, in the grades a caller has to tell apart.
///
/// `FenceDown` is 503 and not 500 for the reason spec §6.2a gives: nothing is broken, browsing is
/// simply not available, and a 500 reads as a crash and invites the retry loop that would run
/// against an unfenced browser.
pub fn browser_error(error: BrowserError) -> axum::response::Response {
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

    async fn run_stamped(pool: &sqlx::SqlitePool, project: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, project_id, created_at)
             VALUES ('p', 'running', 'worktree', ?, '2026-10-22T10:00:00Z') RETURNING id",
        )
        .bind(project)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn an_absent_project_id_resolves_to_the_calling_turns_stamped_project() {
        let pool = crate::testdb::fresh_pool().await;
        let run = run_stamped(&pool, Some("acme")).await;
        assert_eq!(resolve_scope(&pool, Some(run), None).await, "acme");
    }

    #[tokio::test]
    async fn an_absent_project_id_with_no_stamped_project_is_the_no_project_scope() {
        let pool = crate::testdb::fresh_pool().await;
        let run = run_stamped(&pool, None).await;
        assert_eq!(resolve_scope(&pool, Some(run), None).await, NO_PROJECT);
        assert_eq!(resolve_scope(&pool, None, None).await, NO_PROJECT);
    }

    #[tokio::test]
    async fn an_explicit_project_id_is_kept_as_before() {
        let pool = crate::testdb::fresh_pool().await;
        let run = run_stamped(&pool, Some("acme")).await;
        assert_eq!(
            resolve_scope(&pool, Some(run), Some("other")).await,
            "other"
        );
    }
}
