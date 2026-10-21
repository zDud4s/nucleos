//! Notifications, PII, voice, web and browser seats (`/notifications/*`, `/pii`, `/voice/*`, `/web/*`, `/browser/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        // Owner-only: in no scope table.
        // `read_context` is the run-side door onto the same refs: it sits in no scope table either
        // and is gated per run by `auth::LOADOUT_ROUTES` (the run's `run_loadout` must list it).
        .route(
            "/context/read",
            post(crate::context_refs::post_read_context),
        )
        .route(
            "/context-refs/{owner_kind}/{owner_id}",
            get(crate::context_refs::list_refs).post(crate::context_refs::create_ref),
        )
        .route(
            "/context-refs/{owner_kind}/{owner_id}/{id}",
            axum::routing::put(crate::context_refs::update_ref_note)
                .delete(crate::context_refs::delete_ref),
        )
        // A twenty-minute memo is ~38 MB of 16 kHz PCM, and every route not given its own ceiling
        // inherits axum's 2 MB default — which would reject precisely the long recordings that are
        // least repeatable, and reject them the same way every time. The ceiling is derived from
        // `voice::MAX_CAPTURE_SECONDS` rather than written out, so the two cannot drift apart.
        .route(
            "/voice/capture",
            post(crate::integrations::voice::post_capture).layer(DefaultBodyLimit::max(
                crate::integrations::voice::max_body_bytes(),
            )),
        )
        // The web pillar. `/web/read` needs Admin and `/web/search` does not — see `auth.rs` for
        // why; routing is not where that decision lives, only where these four names appear.
        .route("/web/search", post(crate::integrations::web::post_search))
        .route("/web/read", post(crate::integrations::web::post_read))
        .route("/web/pages", get(crate::integrations::web::list_pages))
        .route("/web/pages/{id}", get(crate::integrations::web::get_page))
        // The browser pillar. Every one of these needs Admin — none is in `auth.rs`'s read-only
        // table, including the two GETs, because the list of hosts a project has logged into is a
        // map of where its owner has accounts. The one exception is the chat-browser door in
        // `auth.rs`: a chat turn's own key reaches the seven agent verbs (open, snapshot, act,
        // look, screenshot, handoff, close) for the sessions its chat or run owns, and nothing else.
        //
        // There is no `/browser/grant`, and its absence is the pillar's central invariant rather
        // than an omission: the site list grows when a person finishes a login and hands the wheel
        // back (spec §5.2), never by asking for a host to be added.
        .route("/browser/open", post(crate::browser::post_open))
        .route("/browser/snapshot", post(crate::browser::post_snapshot))
        .route("/browser/act", post(crate::browser::post_act))
        .route("/browser/screenshot", post(crate::browser::post_screenshot))
        // The agent's picture, beside the person's. Two routes and not one flag, because the two
        // differ in audience and therefore in everything: what is drawn on it, what it costs, and
        // which one a session with a person at the wheel refuses.
        .route("/browser/look", post(crate::browser::post_look))
        .route("/browser/close", post(crate::browser::post_close))
        .route("/browser/revoke", post(crate::browser::post_revoke))
        .route("/browser/readonly", post(crate::browser::post_readonly))
        .route("/browser/forget", post(crate::browser::post_forget))
        // The wheel (spec §4.4). `/handoff` is the agent asking; there is deliberately no route that
        // ACCEPTS — accepting is `POST /proposals/{id}/approve`, the same door every other decision
        // goes through, so the compare-and-set that settles a concurrent approve is also the write
        // that moves the session out of the agent's hands (rule 1).
        //
        // `/keep` is the closest thing to a grant on this surface, and it names no host: it answers
        // yes or no to a chain a browser recorded under a person's own hands.
        .route(
            "/browser/handoff",
            post(crate::browser::wheel::post_handoff),
        )
        // A window a person opens for themselves. It skips the proposal that `/handoff` raises,
        // because the dialogue there defends against an AGENT having chosen the destination and here
        // nobody did — and it refuses outright when nobody is at the machine, which is the check that
        // stops it being a route any run could use to open a browser over the owner's live cookies.
        .route("/browser/window", post(crate::browser::wheel::post_window))
        .route("/browser/return", post(crate::browser::wheel::post_return))
        .route("/browser/keep", post(crate::browser::wheel::post_keep))
        .route("/browser/sessions", get(crate::browser::list_open_sessions))
        .route(
            "/browser/sessions/{id}/live",
            get(crate::browser::live::get_live_checked),
        )
        // In no scope table on purpose: a run's key gets 403 here before the handler, and the handler
        // refuses a control-token holder that carries a run id.
        .route(
            "/browser/sessions/{id}/take",
            post(crate::browser::seat::post_take),
        )
        .route(
            "/browser/sessions/{id}/window",
            post(crate::browser::wheel::post_window_seat),
        )
        .route(
            "/browser/sessions/{id}/input",
            post(crate::browser::seat::post_input).layer(DefaultBodyLimit::max(
                crate::browser::seat::INPUT_BODY_LIMIT,
            )),
        )
        .route(
            "/browser/sessions/{id}/answer",
            post(crate::browser::seat::post_answer).layer(DefaultBodyLimit::max(
                crate::browser::seat::ANSWER_BODY_LIMIT,
            )),
        )
        .route(
            "/browser/sites/{project_id}",
            get(crate::browser::get_sites),
        )
        .route(
            "/browser/writes/{project_id}",
            get(crate::browser::get_writes),
        )
        .route("/voice/config", get(crate::integrations::voice::get_config))
        // One unit of a turn's answer, as audio. Indexed rather than streamed, because the repo's
        // live-turn transport is a poll over a tail buffer and a second transport for the same job
        // would be a second thing to keep correct. Sentences arrive seconds apart; poll is enough.
        .route(
            "/voice/turns/{turn_id}/speech/{index}",
            get(crate::integrations::voice::get_turn_speech),
        )
        .route("/voice/memos", get(crate::integrations::voice::list_memos))
        // Read by hand for prompt tuning, not by the shell — see voice.rs's `list_dictations`.
        .route(
            "/voice/dictations",
            get(crate::integrations::voice::list_dictations),
        )
        .route(
            "/voice/memos/{id}",
            get(crate::integrations::voice::get_memo)
                .delete(crate::integrations::voice::delete_memo),
        )
        .route(
            "/notifications/pending",
            get(crate::integrations::notify::list_pending),
        )
        // Which feed kinds may reach a channel — stored, validated and observed here; RESOLVED in
        // the Telegram sidecar (spec §4, §6), never here. In no scope table in `auth.rs`, so admin
        // by omission (§5.3): the two callers already hold the daemon's full token — the shell from
        // the Credential Manager, the sidecar as `NUCLEOS_DAEMON_TOKEN` — and a read-only key buys
        // no reason to learn which notifications the owner chose to silence.
        .route(
            "/notifications/policy",
            get(get_notify_policy).put(put_notify_policy),
        )
        .route("/notifications/kinds", get(get_notify_kinds))
        // The measurement the shadow pass exists to produce. Without somewhere to read it, the
        // table is write-only and the pass becomes the thing it was designed not to be: data
        // accumulating with nobody able to decide anything from it.
        .route("/pii/observations", get(get_pii_observations))
}

/// What the PII shadow pass has seen, by column and class.
///
/// `none` is a class here and not an absence: it counts the fields that were looked at and found
/// clean, which is the denominator. A tally without it says how often personal data was found and
/// not how often it was looked for, and only the second answers whether a class is worth enforcing.
///
/// The column comes with it because the three are not one population. A `name` in `from_name` is
/// nearly a certainty and a `name` in a subject line is a finding; added together they answer
/// nothing, and the denominator would mix three base rates into one meaningless total.
pub(super) async fn get_pii_observations(
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, StatusCode> {
    let tally = crate::governance::pii_shadow::tally(&state.pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(
        tally
            .into_iter()
            .map(|(column, class, count)| {
                serde_json::json!({"column": column, "class": class, "count": count})
            })
            .collect(),
    ))
}

/// `GET /notifications/policy` — the stored selection policy, verbatim (spec §5.3). A fresh
/// database has no rows, so this answers `{"families": [], "kinds": []}` rather than 404: absence
/// of a rule is the resolution's own "passes" default, not an error.
///
/// With `?channel=<id>` it answers `{channel, primary, default, overrides, resolved}` instead:
/// the machine-wide rows, the channel's own rows, their merge, and whether the channel is the
/// primary one.
pub(super) async fn get_notify_policy(
    State(state): State<AppState>,
    Query(query): Query<PolicyQuery>,
) -> impl IntoResponse {
    let channel = match policy_channel(&query) {
        Ok(channel) => channel,
        Err(response) => return response,
    };

    let Some(channel) = channel else {
        return match notify_policy::load(&state.pool).await {
            Ok(policy) => Json(policy).into_response(),
            Err(error) => {
                tracing::warn!(%error, "notify_policy: reading the stored policy failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
            }
        };
    };

    let read = async {
        let default = notify_policy::load(&state.pool).await?;
        let overrides = crate::channels::routing::channel_overrides(&state.pool, channel).await?;
        let resolved =
            crate::channels::routing::resolved_policy(&state.pool, Some(channel)).await?;
        let primary = crate::channels::access::primary(&state.pool).await? == Some(channel);
        Ok::<_, sqlx::Error>((default, overrides, resolved, primary))
    };
    match read.await {
        Ok((default, overrides, resolved, primary)) => Json(serde_json::json!({
            "channel": channel,
            "primary": primary,
            "default": default,
            "overrides": overrides,
            "resolved": resolved,
        }))
        .into_response(),
        Err(error) => {
            tracing::warn!(%error, "notify_policy: reading the channel policy failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

/// The optional `?channel=<id>` of both policy routes. Absent = the machine-wide default policy,
/// exactly as before channels existed.
#[derive(Debug, Deserialize)]
pub(super) struct PolicyQuery {
    channel: Option<String>,
}

/// A channel id is a lowercase slug: letters, digits, `_` or `-`.
fn is_well_formed_channel(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// `Ok(None)` = no `?channel=`; `Ok(Some(id))` = a compiled channel; `Err` = the 400/404 to send.
fn policy_channel(query: &PolicyQuery) -> Result<Option<&'static str>, axum::response::Response> {
    let Some(raw) = query.channel.as_deref() else {
        return Ok(None);
    };
    if !is_well_formed_channel(raw) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "refusal": "malformed_channel",
                "detail": "the channel id is empty or not lowercase letters, digits, `_` or `-`",
            })),
        )
            .into_response());
    }
    match crate::channels::registry::by_id(raw) {
        Some(manifest) => Ok(Some(manifest.id)),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "refusal": "unknown_channel",
                "detail": format!("no channel {raw:?} is compiled into this build"),
            })),
        )
            .into_response()),
    }
}

/// The refusal name `PUT /notifications/policy` reports for each `ValidationError` variant, kept
/// beside the status/reason split `create_run_status`/`create_run_reason` already use for
/// `CreateRunError`: one place decides what a refusal is CALLED, so the name and the message
/// cannot drift apart.
pub(super) fn notify_policy_refusal_name(error: &notify_policy::ValidationError) -> &'static str {
    match error {
        notify_policy::ValidationError::EmptySelector { .. } => "empty_selector",
        notify_policy::ValidationError::MalformedSelector { .. } => "malformed_selector",
        notify_policy::ValidationError::SelectorTooLong { .. } => "selector_too_long",
        notify_policy::ValidationError::DuplicateSelector { .. } => "duplicate_selector",
        notify_policy::ValidationError::TooManyRules { .. } => "too_many_rules",
    }
}

/// The 400 every validation refusal of `PUT /notifications/policy` sends, with or without a channel.
fn notify_policy_refusal(error: &notify_policy::ValidationError) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        // `detail`, not `message`: the shell's `refusalFrom` reads `body.detail` beside a
        // `refusal` and never looks at `message`, so naming it wrong throws the selector away
        // and leaves the page showing the code twice. Same pair every refusal in this file
        // sends.
        Json(serde_json::json!({
            "refusal": notify_policy_refusal_name(error),
            "detail": error.to_string(),
        })),
    )
        .into_response()
}

/// `PUT /notifications/policy` — replaces the whole policy (spec §5.3). Validates BEFORE writing;
/// `notify_policy::replace` itself never calls `validate`, so a rejected shape and a database
/// error cannot share a code path. A `400` names the refusal and the selector in question, so the
/// shell can point at the offending row instead of saying "invalid" (§7.4). A `500` means the
/// transaction never committed, so the table is exactly as it was.
///
/// With `?channel=<id>` it replaces that channel's override rows instead (an empty policy clears
/// them); the same five refusals apply.
pub(super) async fn put_notify_policy(
    State(state): State<AppState>,
    Query(query): Query<PolicyQuery>,
    Json(policy): Json<notify_policy::Policy>,
) -> impl IntoResponse {
    let channel = match policy_channel(&query) {
        Ok(channel) => channel,
        Err(response) => return response,
    };

    if let Err(error) = notify_policy::validate(&policy) {
        return notify_policy_refusal(&error);
    }

    if let Some(channel) = channel {
        return match crate::channels::routing::put_channel_policy(
            &state.pool,
            channel,
            &policy,
            chrono::Utc::now(),
        )
        .await
        {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(crate::channels::routing::PolicyError::Invalid(error)) => {
                notify_policy_refusal(&error)
            }
            Err(crate::channels::routing::PolicyError::Db(error)) => {
                tracing::warn!(%error, "notify_policy: writing the channel policy failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
            }
        };
    }

    match notify_policy::replace(&state.pool, &policy).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => {
            tracing::warn!(%error, "notify_policy: writing the new policy failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

/// `GET /notifications/kinds` — the kinds THIS machine has written, deduplicated and ordered
/// (spec §4.2, §5.2). Only an observation of the feed's retention window; the union with kinds
/// that only exist as a stored rule is the shell's job (§7.2), not this route's.
pub(super) async fn get_notify_kinds(State(state): State<AppState>) -> impl IntoResponse {
    match notify_policy::observed_kinds(&state.pool).await {
        Ok(kinds) => Json(kinds).into_response(),
        Err(error) => {
            tracing::warn!(%error, "notify_policy: reading observed kinds failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    // `channels::testkit` is `#[cfg(test)]` in the lib, so it does not exist when the bin's tests
    // compile; the `mod http` tests build their state with `http::tests::test_state`. The lib's
    // `fake` channel is `#[cfg(test)]` too, so these tests use the production `telegram` manifest.
    async fn state() -> AppState {
        super::super::tests::test_state().await
    }

    async fn call(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", "Bearer test-token");
        let body = match body {
            Some(body) => {
                request = request.header("content-type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = super::super::build_router(state.clone())
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn empty_policy() -> serde_json::Value {
        serde_json::json!({"families": [], "kinds": []})
    }

    #[tokio::test]
    async fn notify_policy_without_channel_is_unchanged() {
        let state = state().await;
        let (status, body) = call(&state, "GET", "/notifications/policy", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, empty_policy());

        let put = serde_json::json!({
            "families": [{"selector": "job_", "enabled": false}],
            "kinds": []
        });
        let (status, _) = call(&state, "PUT", "/notifications/policy", Some(put.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = call(&state, "GET", "/notifications/policy", None).await;
        assert_eq!(body, put);
    }

    #[tokio::test]
    async fn notify_policy_empty_channel_param_is_400_malformed_channel() {
        let state = state().await;
        for uri in [
            "/notifications/policy?channel=",
            "/notifications/policy?channel=Bad%20Id",
        ] {
            let (status, body) = call(&state, "GET", uri, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["refusal"], "malformed_channel");
            let (status, body) = call(&state, "PUT", uri, Some(empty_policy())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["refusal"], "malformed_channel");
        }
    }

    #[tokio::test]
    async fn notify_policy_unknown_channel_is_404() {
        let state = state().await;
        let (status, body) = call(&state, "GET", "/notifications/policy?channel=nope", None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["refusal"], "unknown_channel");
        let (status, body) = call(
            &state,
            "PUT",
            "/notifications/policy?channel=nope",
            Some(empty_policy()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["refusal"], "unknown_channel");
    }

    #[tokio::test]
    async fn notify_policy_get_with_channel_returns_default_overrides_resolved_primary() {
        let state = state().await;
        notify_policy::replace(
            &state.pool,
            &notify_policy::Policy {
                families: vec![notify_policy::Rule {
                    selector: "job_".into(),
                    enabled: false,
                }],
                kinds: vec![],
            },
        )
        .await
        .unwrap();
        let (status, _) = call(
            &state,
            "PUT",
            "/notifications/policy?channel=telegram",
            Some(serde_json::json!({
                "families": [{"selector": "job_", "enabled": true}],
                "kinds": []
            })),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, body) = call(
            &state,
            "GET",
            "/notifications/policy?channel=telegram",
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["channel"], "telegram");
        assert_eq!(body["primary"], false);
        assert_eq!(
            body["default"]["families"],
            serde_json::json!([{"selector": "job_", "enabled": false}])
        );
        assert_eq!(
            body["overrides"]["families"],
            serde_json::json!([{"selector": "job_", "enabled": true}])
        );
        assert_eq!(
            body["resolved"]["families"],
            serde_json::json!([{"selector": "job_", "enabled": true}])
        );

        // Once the channel is the primary one, `primary` says so.
        sqlx::query("UPDATE channels_global SET primary_channel = 'telegram' WHERE id = 1")
            .execute(&state.pool)
            .await
            .unwrap();
        let (_, body) = call(
            &state,
            "GET",
            "/notifications/policy?channel=telegram",
            None,
        )
        .await;
        assert_eq!(body["primary"], true);
    }

    #[tokio::test]
    async fn notify_policy_put_with_channel_replaces_overrides_204() {
        let state = state().await;
        let uri = "/notifications/policy?channel=telegram";
        let first = serde_json::json!({
            "families": [{"selector": "job_", "enabled": false}],
            "kinds": [{"selector": "job_done", "enabled": true}]
        });
        let (status, _) = call(&state, "PUT", uri, Some(first)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let second = serde_json::json!({
            "families": [{"selector": "email_", "enabled": false}],
            "kinds": []
        });
        let (status, _) = call(&state, "PUT", uri, Some(second.clone())).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = call(&state, "GET", uri, None).await;
        assert_eq!(body["overrides"], second);

        // A refusal names the selector and writes nothing.
        let refused = serde_json::json!({
            "families": [{"selector": "", "enabled": true}],
            "kinds": []
        });
        let (status, body) = call(&state, "PUT", uri, Some(refused)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["refusal"], "empty_selector");
        let (_, body) = call(&state, "GET", uri, None).await;
        assert_eq!(body["overrides"], second);

        // The machine-wide policy was never touched.
        let (_, body) = call(&state, "GET", "/notifications/policy", None).await;
        assert_eq!(body, empty_policy());
    }
}
