//! Sending a message and following its turn (`/assistant/message`, `/assistant/turns/*`, `/assistant/{turn_id}/live*`, relay and forward).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/assistant/message", post(post_assistant_message))
        // Answering a question a turn is being held on. Under `assistant` and not under `hooks`
        // because this is the person speaking, not the CLI: the hook's own half carries a run's key
        // and lives beside the gate.
        .route("/assistant/asks/{ask_id}", post(post_ask_answer))
        // What one turn's tools actually answered. Its own route, and a segment deeper than the
        // turn, because the answers are large and the transcript is polled: see `ToolCall::result`
        // for why they are stripped from the turn list and fetched only when somebody opens one.
        .route("/assistant/turns/{turn_id}/tools", get(get_turn_tools))
        // Stop on a chat turn: an interrupt that keeps the process and the session, falling back to
        // the kill `/runs/{id}/cancel` performs. A route of the chat's own so that route stays as it
        // is for Kill and for every other kind of run.
        .route("/assistant/turns/{turn_id}/stop", post(post_stop_turn))
        // Finding a sentence rather than a conversation. The window's own palette matches titles,
        // which is the right first answer and a useless second one: what people come back for is
        // something that was SAID, and a title is a summary written by a model.
        .route("/assistant/search", get(search_assistant))
        // `{chat_id}` is the DESTINATION, matching every sibling route above: this acts on the
        // conversation named in the URL, the same way `/seen` marks it read and `/title` renames
        // it — here, the act is handing it a message. The sender is never in the URL or the body;
        // `relay_send_to_chat` reads it off `RUN_ID_HEADER`, which only the run itself can have set
        // (`daemon_client::send_to_chat`'s own doc says why that is trustworthy), so a caller
        // cannot name a conversation to relay FROM any more than `POST /assistant/message` lets one
        // name who is typing.
        .route("/assistant/chats/{chat_id}/relay", post(relay_send_to_chat))
        // Send now: text written into the turn that is running. See `assistant::say_now`.
        .route("/assistant/chats/{chat_id}/say-now", post(post_say_now))
        // The same hop, asked for by the person instead of by the model. A route of its own and not
        // a flag on the one above, because the two differ in the one thing that matters: where the
        // sending turn's identity comes from. `/relay` reads it off a header this process wrote
        // into a run's environment; this reads it off a body, because a person at a window is not
        // a run and has no such header to send.
        .route("/assistant/chats/{chat_id}/forward", post(forward_to_chat))
        // The path a relayed turn travelled, asked for rather than carried. A segment deeper than
        // the transcript because it is about ONE turn, and off the transcript because it is read
        // when somebody hovers — not every second and a half, which is what the transcript costs.
        .route(
            "/assistant/chats/{chat_id}/turns/{turn_id}/chain",
            get(get_relay_chain),
        )
        // A turn in flight, as words. The literal is a segment deeper than `{turn_id}` above, so
        // the two cannot shadow each other whatever a turn id looks like.
        .route("/assistant/{turn_id}/live", get(get_assistant_live))
        // The same state as a push stream. A literal deeper than `/live`, so it cannot shadow it.
        .route(
            "/assistant/{turn_id}/live/stream",
            get(get_assistant_live_stream),
        )
}

#[derive(Deserialize)]
pub(super) struct AssistantMessageRequest {
    pub(super) chat_id: String,
    pub(super) text: String,
    /// Which client is asking, so the daemon routes the turn without inferring it from the shape of
    /// `chat_id`. Absent means the shell, which is what every caller written before this field
    /// existed means too.
    #[serde(default)]
    pub(super) origin: Option<String>,
    /// Whether this caller would rather wait than be refused while a turn is already running.
    ///
    /// Opted into, never assumed. The Telegram sidecar gives up on a turn after a timeout and would
    /// rather be told no than be answered ten minutes late into a conversation that has moved on;
    /// the window in front of a person would rather keep the words. Absent means refuse, which is
    /// what every caller written before this field existed expects.
    #[serde(default)]
    pub(super) wait_if_busy: bool,
    /// The pictures attached to this message. Empty for every caller that attaches none.
    #[serde(default)]
    pub(super) images: Vec<ImageIn>,
}

/// One picture as the window sends it: base64, with the sender's claim about what it is.
#[derive(Deserialize)]
pub(super) struct ImageIn {
    pub(super) media_type: String,
    pub(super) data: String,
}

pub(super) async fn post_assistant_message(
    State(state): State<AppState>,
    Json(body): Json<AssistantMessageRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Uncancellable for the same reason `create_run` is: `send_message` writes the turn's `running`
    // row and only then spawns the task that will finish it. A client that disconnects mid-request
    // drops this future exactly the way `abort()` drops a run's, and a drop landing between those
    // two leaves a `running` assistant row with no task and no abort handle — `/assistant/{id}`
    // reports it running forever and `/cancel` answers 404. The Telegram sidecar is the caller, and
    // it gives up on a turn after a timeout, so the disconnect is routine rather than theoretical.
    let wait = body.wait_if_busy;
    let outcome = uncancellable(async move {
        let origin = crate::assistant::Origin::from_wire(body.origin.as_deref());
        // The window's shape becomes the runner's here, at the edge, so nothing inside the daemon
        // has to know what a request body looks like.
        let images: Vec<crate::execution::runner::Attachment> = body
            .images
            .into_iter()
            .map(|image| crate::execution::runner::Attachment {
                media_type: image.media_type,
                data: image.data,
            })
            .collect();
        match wait {
            true => {
                crate::assistant::send_or_queue(&state, &body.chat_id, &body.text, &images, origin)
                    .await
            }
            false => crate::assistant::send_message_with(
                &state,
                &body.chat_id,
                &body.text,
                &images,
                origin,
            )
            .await
            .map(crate::assistant::Sent::Turn),
        }
    })
    .await
    .map_err(|status| refusal(status, "internal"))?;

    match outcome {
        Ok(crate::assistant::Sent::Turn(turn_id)) => {
            Ok(Json(serde_json::json!({ "turn_id": turn_id })))
        }
        // Not a turn id and not a refusal: the words were kept and will be sent without anybody
        // pressing anything again. `queued` rather than a null id, because a caller has something
        // different to do about each and a null says neither.
        Ok(crate::assistant::Sent::Queued) => Ok(Json(serde_json::json!({ "queued": true }))),
        // Clears by waiting, which is what makes it the one refusal here that needs no gesture from
        // anybody — and what makes it dangerous to confuse with the one below.
        Err(msg) if msg == crate::assistant::TURN_IN_PROGRESS => {
            Err(refusal(StatusCode::CONFLICT, "turn_in_progress"))
        }
        // Not a 500: nothing broke. The conversation asked to be answered on this machine and this
        // machine has nothing that can — a fact about how it is configured, which the caller can
        // act on by choosing the other model. A 500 would send them looking for a crash.
        Err(msg) if msg == crate::assistant::NO_LOCAL_MODEL => {
            Err(refusal(StatusCode::SERVICE_UNAVAILABLE, "no_local_model"))
        }
        // The hosted route's twin of the arm above, 503 for the same reason and with a slug of
        // its own: both say "this machine cannot answer that", and what to DO about each is
        // different — one is fixed by installing a model, this one by naming one in the config.
        Err(msg) if msg == crate::assistant::NO_HOSTED_MODEL => {
            Err(refusal(StatusCode::SERVICE_UNAVAILABLE, "no_hosted_model"))
        }
        // Half-configured rather than unconfigured: a hosted model IS named, and the key needed
        // to reach it is not stored. `assistants::Refusal` keeps these two apart deliberately —
        // telling somebody "no model configured" when the model is configured points them at
        // the wrong half of the setup — and folding them together here would undo that at the
        // one place a person actually reads it.
        Err(msg) if msg == crate::assistants::HOSTED_KEY_MISSING => Err(refusal(
            StatusCode::SERVICE_UNAVAILABLE,
            "no_openrouter_key",
        )),
        // The pictures themselves were refused before anything was spent: too many, too big, not
        // an image the model reads, or a model that reads none. Something the sender changes, so
        // a 422 that names which, never a 500 that sends them looking for a crash.
        Err(msg) if picture_refusal(&msg).is_some() => Err(refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            picture_refusal(&msg).unwrap_or("picture_refused"),
        )),
        Err(_) => Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")),
    }
}

/// The slug for each of `assistant::send`'s picture refusals, `None` for anything else.
fn picture_refusal(msg: &str) -> Option<&'static str> {
    use crate::assistant::{IMAGE_TOO_LARGE, IMAGE_UNREADABLE, IMAGES_NEED_CLOUD, TOO_MANY_IMAGES};
    match msg {
        _ if msg == TOO_MANY_IMAGES => Some("too_many_pictures"),
        _ if msg == IMAGE_TOO_LARGE => Some("picture_too_large"),
        _ if msg == IMAGE_UNREADABLE => Some("picture_unreadable"),
        _ if msg == IMAGES_NEED_CLOUD => Some("pictures_need_cloud"),
        _ => None,
    }
}

/// The body `POST /assistant/chats/{chat_id}/relay` takes — nothing more than what a model chose
/// when it called `send_to_chat`. Everything else `relay::admit` needs to decide (who is asking,
/// and from where) comes off the request's own context rather than off anything a body could say;
/// see the route's own comment in `build_router` and `sending_run_id_of` below.
#[derive(Deserialize)]
pub(super) struct RelayMessageRequest {
    pub(super) text: String,
}

/// The body `POST /assistant/chats/{chat_id}/forward` takes: which turn is being forwarded, and
/// what to say.
///
/// `text` rather than the turn's own words re-read here, so the window can offer them for editing
/// before they travel. What a person forwards is what they chose to forward, and a route that
/// re-read the turn would quietly send something other than what they saw in the box.
///
/// `from_turn_id` is what makes this a relay rather than a plain message. `chat_relays` requires a
/// `sending_run_id` and a turn IS a run, so forwarding one fits the table exactly — and, more to
/// the point, `relay::chain_of` can then walk back through it, so a person's forward is bounded by
/// the same cycle and depth rules a model's is. Sending free-standing text would have needed that
/// column to become nullable, which in SQLite means rebuilding the table, and would have left the
/// forwarded message outside every brake.
#[derive(Deserialize)]
pub(super) struct ForwardRequest {
    pub(super) from_turn_id: i64,
    pub(super) text: String,
}

/// Which conversation `run_id` is answering, and which client sent the message it is answering —
/// the two facts `relay::admit` needs about the SENDER and takes as parameters rather than reading
/// for itself.
///
/// One read for both, because they come off the same row and a second query could only race the
/// first. `hooks::chat_of_run` makes the chat half of this read for its own purpose and stays where
/// it is: each caller of "which chat is this run in" has so far needed it alongside something
/// different, and a shared home would be a dependency between them that does not otherwise exist.
///
/// Two different absences, deliberately kept apart rather than folded into one `Option`:
///
/// * no row, or a row with no `chat_id` — `None`. The header named a run this daemon cannot
///   attribute to any conversation, so there is no `from_chat_id` to relay from.
/// * a row whose `origin` is NULL — `Some((chat, None))`. The run exists and belongs to a
///   conversation, but nothing recorded where its message came from. See 0119 for why that reads
///   as "unknown" and never as "shell".
///
/// `Origin::from_wire` is deliberately NOT used on the value read back, and this is the one reader
/// of the column that departs from it. That function resolves an unknown spelling to `Shell` —
/// right for a client that said nothing, since a caller stating no origin is the old shell path —
/// but here the same leniency would answer the very question this gate exists to ask. The two
/// spellings are matched exactly; anything else is unknown, and unknown is refused.
pub(super) async fn sending_turn_of(
    pool: &sqlx::SqlitePool,
    run_id: i64,
) -> Option<(String, Option<crate::assistant::Origin>)> {
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT chat_id, origin FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    let (chat_id, origin) = row?;
    let origin = match origin.as_deref() {
        // The bare legacy spelling (migration 0235 rewrites stored rows to `channel:telegram`).
        Some("telegram") => Some(crate::assistant::Origin::Channel(
            crate::channels::registry::TELEGRAM.id,
        )),
        Some("shell") => Some(crate::assistant::Origin::Shell),
        // `from_wire` is consulted only for the `channel:` family, because it maps every unknown
        // spelling to `Shell` and that leniency would certify an unreadable origin as local.
        Some(wire) if wire.starts_with("channel:") => {
            match crate::assistant::Origin::from_wire(Some(wire)) {
                // An unregistered channel id falls back to `Shell` there: unknown, so refused.
                crate::assistant::Origin::Channel(id) => {
                    Some(crate::assistant::Origin::Channel(id))
                }
                _ => None,
            }
        }
        _ => None,
    };
    Some((chat_id?, origin))
}

/// `relay::admit`'s six refusals, turned into a status and a body that names which one fired —
/// see `Refusal`'s own doc for what each means. 409 for the five the gate actually DECIDED (the
/// destination is gone or archived, the sender is remote, the chain refuses the hop, the chain has
/// spent its ceiling, the owner is away): a caller can read the slug and know why, the same way `post_assistant_message` already
/// answers 409 for a chat mid-turn. 500 for `Unreadable` alone, because that
/// variant is not a decision about the relay — the gate could not read enough to make one — and
/// there is nothing about THIS relay for a caller to act on differently next time.
pub(super) fn relay_refusal_response(
    cause: crate::relay::Refusal,
) -> (StatusCode, Json<serde_json::Value>) {
    use crate::relay::{Refusal, Verdict};
    match cause {
        Refusal::NoSuchDestination { to_chat_id } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": "no_such_destination",
                "to_chat_id": to_chat_id,
            })),
        ),
        Refusal::RemoteOrigin => refusal(StatusCode::CONFLICT, "remote_origin"),
        Refusal::Chain(Verdict::Itself) => refusal(StatusCode::CONFLICT, "relay_to_self"),
        Refusal::Chain(Verdict::Cycle { at }) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "refusal": "relay_cycle", "at": at })),
        ),
        Refusal::Chain(Verdict::TooDeep) => refusal(StatusCode::CONFLICT, "relay_too_deep"),
        // `admit` constructs `Refusal::Chain` only from a `Verdict` that is not `Ok` (see its own
        // body: the `Ok` case falls through to the INSERT instead). Reached only if that invariant
        // is ever broken elsewhere, and 500 is the honest answer to a refusal that cannot say what
        // it is refusing.
        Refusal::Chain(Verdict::Ok) => refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        Refusal::ChainOverBudget {
            spent_usd,
            limit_usd,
        } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "refusal": "relay_chain_over_budget",
                "spent_usd": spent_usd,
                "limit_usd": limit_usd,
            })),
        ),
        Refusal::OwnerAway => refusal(StatusCode::CONFLICT, "owner_away"),
        Refusal::Unreadable { step, error } => {
            tracing::warn!(step, %error, "a relay could not be decided");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    }
}

/// `assistant::send_relayed_message`'s refusals, mapped the same way `post_assistant_message` above
/// already maps `send_message_with`'s — the two share every constant because a relayed turn is
/// refused by the same brakes an ordinary one is (a chat mid-turn, neither route configured, an
/// OpenRouter key missing, the kill switch). Kept as its own small match rather than a shared function with that
/// route's: the two bodies are one line apart today, and a shared helper here would be reaching
/// into `post_assistant_message` for four string constants it would have imported anyway.
pub(super) fn relayed_send_refusal(msg: String) -> (StatusCode, Json<serde_json::Value>) {
    match msg {
        _ if msg == crate::assistant::TURN_IN_PROGRESS => {
            refusal(StatusCode::CONFLICT, "turn_in_progress")
        }
        _ if msg == crate::assistant::NO_LOCAL_MODEL => {
            refusal(StatusCode::SERVICE_UNAVAILABLE, "no_local_model")
        }
        _ if msg == crate::assistant::NO_HOSTED_MODEL => {
            refusal(StatusCode::SERVICE_UNAVAILABLE, "no_hosted_model")
        }
        _ if msg == crate::assistants::HOSTED_KEY_MISSING => {
            refusal(StatusCode::SERVICE_UNAVAILABLE, "no_openrouter_key")
        }
        _ => refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
}

/// What `admit` and `send_relayed_or_queue` can come back with, folded into one type so the
/// `uncancellable` block below has exactly one thing to return and the match after it is one
/// decision per line rather than a chain of `if let`.
pub(super) enum RelayAttempt {
    /// `sending_run_id` names no run this daemon can find a chat for — a header that parsed as a
    /// number but not as a live run, which `relay::admit` has no way to be asked about since it
    /// takes `from_chat_id` already resolved.
    UnknownSender,
    /// The sending run exists and records no origin. Its own refusal rather than folded into
    /// `UnknownSender` above, because the two are different failures wearing similar words: that
    /// one means the daemon cannot say WHICH conversation is asking, this one means it knows
    /// exactly which and cannot say where that conversation's message came from. Collapsing them
    /// would send an operator looking for a bad run id when the answer is a row that predates 0119.
    ///
    /// Refused rather than defaulted, which is that migration's whole argument: a run whose origin
    /// nobody wrote down is not a run known to be safe, and `relay::admit`'s remote-origin brake is
    /// worth exactly as much as the honesty of what is handed to it.
    UnknownOrigin,
    Refused(crate::relay::Refusal),
    NotSent(String),
}

/// `POST /assistant/chats/{chat_id}/relay`. Sequences `relay::admit` and, if it grants the hop,
/// `assistant::send_relayed_or_queue` with the id it wrote — and does nothing else: every refusal
/// either of those two can produce is already argued in `relay.rs` and `assistant.rs`, and
/// reproducing any part of that argument here would be a second copy of a decision that belongs in
/// one place. What this function owns is entirely transport: reading who is asking off the request,
/// and turning what comes back into a status and a body.
///
/// `send_relayed_or_queue` rather than `send_relayed_message` directly: `admit` grants the HOP, not
/// a guarantee that the destination has a turn free the instant this handler runs, and calling the
/// plain send here would answer `turn_in_progress` for exactly the case — a busy destination — that
/// an ordinary message already has a door for. See that function's own doc for the rest of the
/// argument, and `assistant::drain_queued` for what happens when the wait ends.
pub(super) async fn relay_send_to_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RelayMessageRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(sending_run_id) = sending_run_id_of(&headers) else {
        return Err(refusal(StatusCode::BAD_REQUEST, "missing_run_id"));
    };

    // Uncancellable for the reason `post_assistant_message` is, above: `send_relayed_message`
    // writes the new turn's `running` row and only then spawns the task that finishes it, and a
    // client that disconnects mid-request must not be able to strand that row by dropping this
    // future between the two.
    let outcome = uncancellable(async move {
        let Some((from_chat_id, sending_origin)) =
            sending_turn_of(&state.pool, sending_run_id).await
        else {
            return Err(RelayAttempt::UnknownSender);
        };
        let Some(sending_origin) = sending_origin else {
            return Err(RelayAttempt::UnknownOrigin);
        };

        // The two origins below are different facts and only look like one argument passed twice.
        //
        // `admit` is asked about the SENDER: where the message asking for this relay came from,
        // read off `runs.origin` above rather than asserted here. That read is what makes
        // `relay::Refusal::RemoteOrigin` reachable at all — before 0119 the column did not exist,
        // this handler had nothing truer than `Origin::Shell` to pass, and the brake was live in
        // its own unit tests and dead everywhere else.
        //
        // `send_relayed_or_queue` is told about the message being DELIVERED, and that one is
        // `Origin::Shell` unconditionally. `Origin` names which client sent a message; this one was
        // sent by another conversation through this daemon, which is the shell path by definition,
        // and never Telegram — a Telegram sender does not get past the line above. It matters
        // because the destination routes on it: `send_message_inner`'s `wants_local` falls back to
        // the origin for a conversation with no brain of its own, so passing the sender's origin
        // through would let one conversation decide which model answers in another.
        match crate::relay::admit(
            &state.pool,
            &from_chat_id,
            &chat_id,
            sending_run_id,
            sending_origin,
            &body.text,
            chrono::Utc::now(),
        )
        .await
        {
            Ok(relay_id) => crate::assistant::send_relayed_or_queue(
                &state,
                &chat_id,
                &body.text,
                crate::assistant::Origin::Shell,
                relay_id,
            )
            .await
            .map_err(RelayAttempt::NotSent),
            Err(cause) => Err(RelayAttempt::Refused(cause)),
        }
    })
    .await
    .map_err(|status| refusal(status, "internal"))?;

    relay_outcome_response(outcome)
}

/// What either relay route answers, once the attempt has resolved.
///
/// Shared because the two differ only in who is asking — a run, through a header it cannot forge,
/// or a person, through a body — and not at all in what a granted, queued, or refused hop means.
/// Two copies of this match is two places for the queued case to be forgotten in.
pub(super) fn relay_outcome_response(
    outcome: Result<crate::assistant::Sent, RelayAttempt>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    match outcome {
        Ok(crate::assistant::Sent::Turn(turn_id)) => {
            Ok(Json(serde_json::json!({ "turn_id": turn_id })))
        }
        // Not a turn id and not a refusal: `send_relayed_or_queue` found the destination mid-turn
        // and kept the words rather than losing them — see that function's own doc for why a relay
        // gets the same try-then-queue door an ordinary message already has. `queued` matches the
        // field `post_assistant_message` answers with for the identical outcome, so a caller reading
        // either route learns the same shape.
        Ok(crate::assistant::Sent::Queued) => Ok(Json(serde_json::json!({ "queued": true }))),
        Err(RelayAttempt::UnknownSender) => Err(refusal(StatusCode::BAD_REQUEST, "unknown_sender")),
        // 409 and not the 400 above it. A missing run id is a malformed request and the caller can
        // fix it; a run whose origin was never recorded is a well-formed request this daemon
        // declines to certify, which is the shape every other refusal here answers 409 for.
        Err(RelayAttempt::UnknownOrigin) => Err(refusal(StatusCode::CONFLICT, "unknown_origin")),
        Err(RelayAttempt::Refused(cause)) => Err(relay_refusal_response(cause)),
        Err(RelayAttempt::NotSent(msg)) => Err(relayed_send_refusal(msg)),
    }
}

/// `POST /assistant/chats/{chat_id}/forward`. A person hands a turn to another conversation.
///
/// `chat_id` is the DESTINATION, matching `/relay` beside it and every other
/// `/assistant/chats/{id}/...` route on this server. The source is derived from the turn rather
/// than stated, because a turn belongs to exactly one conversation and asking the caller to name it
/// too would be inviting the two to disagree.
///
/// **The gate is the same gate, and reads the same facts.** `relay::admit` is asked about the
/// origin of the TURN being forwarded, not about the person doing the forwarding — so forwarding a
/// turn that arrived from a remote channel is refused, exactly as a model relaying from one would be.
/// Passing `Origin::Shell` here because a human is at the window was the alternative, and it would
/// have been telling the gate something untrue about the turn in order to get a different answer.
/// One rule, read one way, from one column.
///
/// What this does NOT do is give the person a way past the chain: `sending_run_id` is the forwarded
/// turn, so `chain_of` walks straight through it, and a forward that would close a cycle or exceed
/// `MAX_RELAY_DEPTH` is refused like any other hop.
pub(super) async fn forward_to_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<ForwardRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Uncancellable for the reason `relay_send_to_chat` is: the send writes a `running` row and
    // only then spawns the task that finishes it, and a window that closes mid-request must not be
    // able to strand that row between the two.
    let outcome = uncancellable(async move {
        let Some((from_chat_id, sending_origin)) =
            sending_turn_of(&state.pool, body.from_turn_id).await
        else {
            return Err(RelayAttempt::UnknownSender);
        };
        let Some(sending_origin) = sending_origin else {
            return Err(RelayAttempt::UnknownOrigin);
        };

        match crate::relay::admit(
            &state.pool,
            &from_chat_id,
            &chat_id,
            body.from_turn_id,
            sending_origin,
            &body.text,
            chrono::Utc::now(),
        )
        .await
        {
            // `Origin::Shell` on the delivery for the reason `/relay` gives: the message is
            // arriving through this daemon, which is the shell path by definition, and the
            // destination routes its own brain on that value.
            Ok(relay_id) => crate::assistant::send_relayed_or_queue(
                &state,
                &chat_id,
                &body.text,
                crate::assistant::Origin::Shell,
                relay_id,
            )
            .await
            .map_err(RelayAttempt::NotSent),
            Err(cause) => Err(RelayAttempt::Refused(cause)),
        }
    })
    .await
    .map_err(|status| refusal(status, "internal"))?;

    relay_outcome_response(outcome)
}

/// One conversation on a relayed turn's path.
#[derive(Serialize)]
pub(super) struct ChainStep {
    pub(super) chat_id: String,
    /// Null on a conversation nobody has named. It keeps its place in the path regardless — a
    /// chain with a hole in it is a chain that lies about its own length.
    pub(super) title: Option<String>,
}

/// `GET /assistant/chats/{chat_id}/turns/{turn_id}/chain`. Where a turn came from, all the way back.
///
/// The transcript already names the conversation that handed a turn over, and with
/// `MAX_RELAY_DEPTH` at three that is a different question from "where did this start". "B spoke to
/// me" hides that A began it, and A is the conversation somebody actually typed into.
///
/// `relay::chain_of` does the walk; this adds only the names. A turn nothing relayed answers a
/// chain of one — its own conversation — which is honest rather than empty: that IS its whole path.
///
/// The `chat_id` in the path is checked against the turn's own, and it is not a security boundary:
/// the control token permits every route. It catches a window bug — a stale turn id from a
/// conversation the person has navigated away from — which would otherwise answer with somebody
/// else's path under the title of the chat on screen.
pub(super) async fn get_relay_chain(
    State(state): State<AppState>,
    Path((chat_id, turn_id)): Path<(String, i64)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let owner: Option<Option<String>> = sqlx::query_scalar("SELECT chat_id FROM runs WHERE id = ?")
        .bind(turn_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading which conversation a turn belongs to failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    if owner.flatten().as_deref() != Some(chat_id.as_str()) {
        return Err(StatusCode::NOT_FOUND);
    }

    // A walk that cannot complete is a 500 and not an empty chain, for the reason `chain_of`'s own
    // doc gives: a short chain reads as a safe one, and answering a shortened path here would be
    // the same lie in a place a person reads rather than a gate.
    let chain = crate::relay::chain_of(&state.pool, turn_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, turn_id, "a relay chain could not be walked");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // One read for the names, not one per hop: the chain is at most `MAX_RELAY_DEPTH + 1` long, so
    // the difference is small — but a loop of queries here would be the shape that stops being
    // small the day somebody raises that constant.
    let mut named = Vec::with_capacity(chain.len());
    let mut titles = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT chat_id, title FROM chats WHERE chat_id IN (",
    );
    let mut separated = titles.separated(", ");
    for step in &chain {
        separated.push_bind(step);
    }
    titles.push(")");
    let rows: Vec<(String, Option<String>)> = titles
        .build_query_as()
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    for step in chain {
        // A conversation with no `chats` row at all reads as unnamed rather than dropping out. The
        // walk found it, so it is part of the path whatever the `chats` table has to say.
        let title = rows
            .iter()
            .find(|(id, _)| *id == step)
            .and_then(|(_, title)| title.clone());
        named.push(ChainStep {
            chat_id: step,
            title,
        });
    }

    Ok(Json(serde_json::json!({ "chain": named })))
}

/// What the window says when somebody answers for a held tool call.
#[derive(serde::Deserialize)]
pub(super) struct AskAnswer {
    pub(super) allow: bool,
}

/// Answers a tool call a conversation is being held on.
///
/// 404 when there was nothing to answer, which covers both ways that happens: the window is a poll
/// behind and the turn has moved on, or the question timed out while somebody was reading it. The
/// same answer on purpose — the caller's next read tells them which.
pub(super) async fn post_ask_answer(
    Path(ask_id): Path<String>,
    Json(body): Json<AskAnswer>,
) -> Result<StatusCode, StatusCode> {
    match crate::hooks::answer_ask(&ask_id, body.allow) {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(StatusCode::NOT_FOUND),
    }
}

/// Stops a chat turn: interrupts it when its process can be spoken to, kills it otherwise.
///
/// Uncancellable for the reason `runs::cancel_run` is: the kill removes the turn's handle before it
/// writes the status, so a request dropped in the middle would leave a `running` row nothing reaches.
pub(super) async fn post_stop_turn(
    State(state): State<AppState>,
    Path(turn_id): Path<i64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match uncancellable(async move { crate::assistant::stop_turn(&state, turn_id).await }).await? {
        crate::assistant::Stopped::Interrupted => {
            Ok(Json(serde_json::json!({ "stopped": "interrupted" })))
        }
        crate::assistant::Stopped::Killed => Ok(Json(serde_json::json!({ "stopped": "killed" }))),
        crate::assistant::Stopped::NotRunning => Err(StatusCode::NOT_FOUND),
    }
}

#[derive(Deserialize)]
pub(super) struct SayNowRequest {
    pub(super) text: String,
}

/// Says something into the turn that is running, or sends it the ordinary way when none is.
pub(super) async fn post_say_now(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<SayNowRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if body.text.trim().is_empty() {
        return Err(refusal(StatusCode::BAD_REQUEST, "empty"));
    }
    let said =
        uncancellable(async move { crate::assistant::say_now(&state, &chat_id, &body.text).await })
            .await
            .map_err(|status| refusal(status, "internal"))?;
    match said {
        Ok(crate::assistant::SaidNow::Injected) => {
            Ok(Json(serde_json::json!({ "said_now": true })))
        }
        Ok(crate::assistant::SaidNow::Sent(crate::assistant::Sent::Turn(id))) => {
            Ok(Json(serde_json::json!({ "turn_id": id })))
        }
        Ok(crate::assistant::SaidNow::Sent(crate::assistant::Sent::Queued)) => {
            Ok(Json(serde_json::json!({ "queued": true })))
        }
        Err(error) => {
            tracing::warn!(%error, "saying something into a running turn failed");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

/// What one turn's tools answered.
///
/// A route of its own rather than a wider transcript, and the reason is arithmetic. The
/// transcript is polled at a live turn's cadence — about once a second while something is
/// running — and a tool answer is capped at two thousand characters. Twenty calls in a turn and a
/// hundred turns in a conversation is four megabytes on the wire every second, paid forever, for
/// something almost nobody has open. So `get_assistant_chat` strips the answers and this hands
/// them over for one turn, when somebody asks.
///
/// `404` for a turn that is not an assistant turn of this daemon's: a job's run id must not open a
/// door into the chat transcript by guessing a number.
pub(super) async fn get_turn_tools(
    State(state): State<AppState>,
    Path(turn_id): Path<i64>,
) -> Result<Json<TurnToolsOut>, StatusCode> {
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT tools_used FROM runs WHERE id = ? AND mode = 'assistant'")
            .bind(turn_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| {
                tracing::warn!(%error, turn_id, "reading a turn's tools failed");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
    let Some(tools_used) = stored else {
        return Err(StatusCode::NOT_FOUND);
    };
    // A column that will not parse reads as an empty list, exactly as the transcript treats it: a
    // turn recorded before the column existed acted on nothing as far as anything here can tell,
    // and that is a true statement about what is known rather than an error.
    Ok(Json(TurnToolsOut {
        did: tools_used
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default(),
    }))
}

#[derive(serde::Serialize)]
pub(super) struct TurnToolsOut {
    /// Every tool the turn ran, oldest first, WITH what each one answered. See `ToolCall::result`.
    pub(super) did: Vec<crate::execution::runner::ToolCall>,
}

/// How many hits a search comes back with.
///
/// Small on purpose. This answers "where did we talk about that", and a person scanning for the
/// conversation they half remember reads the first few and refines the words if none of them is
/// it. Two hundred hits is not a better answer to that question; it is the same answer with the
/// useful part further down.
pub(super) const SEARCH_HITS: i64 = 40;

/// The longest search acted on. Longer than this is a paste, not a search.
pub(super) const SEARCH_QUERY_MAX: usize = 200;

/// How much of the line around a hit comes back, in characters either side of it.
pub(super) const EXCERPT_BEFORE: usize = 60;
pub(super) const EXCERPT_AFTER: usize = 140;

#[derive(serde::Deserialize)]
pub(super) struct AssistantSearchQuery {
    pub(super) q: String,
}

/// One thing that was said, and where.
#[derive(serde::Serialize)]
pub(super) struct SearchHit {
    pub(super) chat_id: String,
    /// What the conversation is called, or null when nothing has named it.
    pub(super) title: Option<String>,
    /// The turn it was said in, so the window can scroll to it rather than merely open the chat.
    pub(super) turn_id: i64,
    /// `asked` or `answered`: which half of the exchange matched.
    ///
    /// Worth saying, because the two are found for different reasons. Somebody looking for
    /// something they asked is retracing their own steps; somebody looking for something the model
    /// said is looking for an answer they were given. A list that merged the two would make the
    /// second kind hunt through the first.
    pub(super) side: &'static str,
    /// The words around the hit, with an ellipsis on whichever side was cut.
    pub(super) excerpt: String,
    pub(super) created_at: String,
}

/// One matched row, before it is read into a hit.
#[derive(sqlx::FromRow)]
pub(super) struct SearchRow {
    pub(super) chat_id: String,
    pub(super) title: Option<String>,
    pub(super) turn_id: i64,
    pub(super) asked: String,
    pub(super) answered: Option<String>,
    pub(super) created_at: String,
}

/// Something that was SAID, across every conversation this app is holding.
///
/// The window's palette matches titles, which is the right first answer and a useless second one:
/// a title is a summary a model wrote, and what somebody comes back for is a sentence — the name
/// of a function, the error they pasted, the decision they want to quote.
///
/// Archived conversations are left out, for the same reason `chats::list` leaves them out: this
/// answers "which of my conversations", and archiving one is saying it is not among them.
///
/// Case is folded the way SQLite folds it, which is ASCII only — `Parser` finds `parser`, and `Ç`
/// does not find `ç`. Deliberately not worked around here: the excerpt below matches the same way
/// the query does, so what is highlighted is always what was actually found. A search that folded
/// more than the query did would point at a word the query never matched.
pub(super) async fn search_assistant(
    State(state): State<AppState>,
    Query(query): Query<AssistantSearchQuery>,
) -> Json<Vec<SearchHit>> {
    let needle = query.q.trim();
    // Nothing, rather than every turn in the database. An empty search is a box somebody has not
    // finished typing in, and `%%` matches all of it.
    if needle.is_empty() || needle.chars().count() > SEARCH_QUERY_MAX {
        return Json(Vec::new());
    }
    let pattern = like_pattern(needle);
    let rows = sqlx::query_as::<_, SearchRow>(
        "SELECT r.chat_id AS chat_id, c.title AS title, r.id AS turn_id,
                r.prompt AS asked, r.stdout AS answered, r.created_at AS created_at
           FROM runs r
           JOIN chats c ON c.chat_id = r.chat_id
          WHERE r.mode = 'assistant'
            AND c.archived_at IS NULL
            AND (r.prompt LIKE ?1 ESCAPE '\\' OR r.stdout LIKE ?1 ESCAPE '\\')
          ORDER BY r.id DESC
          LIMIT ?2",
    )
    .bind(&pattern)
    .bind(SEARCH_HITS)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_else(|error| {
        // Nothing found rather than a 500. A search that cannot run is a search with no results
        // from where the person is standing, and a page that refuses to draw because a LIKE went
        // wrong loses them the conversation they still have open.
        tracing::warn!(%error, "searching the conversations failed");
        Vec::new()
    });

    Json(
        rows.into_iter()
            .map(|row| {
                // The question first when it matched, because that is the half a person owns. Only
                // one hit per turn either way: a turn where both halves mention the word is one
                // exchange, and two rows for it would push a different conversation off the list.
                let (side, text) = if find_ascii_ci(&chars(&row.asked), &chars(needle)).is_some() {
                    ("asked", row.asked.as_str())
                } else {
                    ("answered", row.answered.as_deref().unwrap_or(""))
                };
                SearchHit {
                    chat_id: row.chat_id,
                    title: row.title,
                    turn_id: row.turn_id,
                    side,
                    excerpt: excerpt_of(text, needle),
                    created_at: row.created_at,
                }
            })
            .collect(),
    )
}

/// A `LIKE` pattern that matches this text and nothing cleverer.
///
/// `%` and `_` are wildcards in `LIKE`, so a search for `budget_usd` would otherwise match
/// `budgetXusd` — and, worse, a search for `%` would match every turn ever recorded. Escaped, with
/// the escape character escaped first.
pub(super) fn like_pattern(text: &str) -> String {
    let mut out = String::from("%");
    for ch in text.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// Where `pin` first occurs in `hay`, folding case the way SQLite's `LIKE` does — ASCII only.
///
/// Deliberately the same folding as the query, so the excerpt always contains the thing that was
/// matched. See `search_assistant`.
pub(super) fn find_ascii_ci(hay: &[char], pin: &[char]) -> Option<usize> {
    if pin.is_empty() || pin.len() > hay.len() {
        return None;
    }
    hay.windows(pin.len()).position(|window| {
        window
            .iter()
            .zip(pin)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    })
}

/// The words around a hit, with an ellipsis on whichever side was cut.
///
/// Around it and not from the top: a match nine hundred characters into an answer is not visible
/// in the first line of that answer, and a list of first lines is a list that does not show what
/// it found. Falls back to the head of the text when the hit cannot be located — which is only
/// reachable if the query and this disagree about folding, and the head of the text is at least
/// true.
pub(super) fn excerpt_of(text: &str, needle: &str) -> String {
    let hay = chars(text);
    let pin = chars(needle);
    let at = find_ascii_ci(&hay, &pin).unwrap_or(0);
    let start = at.saturating_sub(EXCERPT_BEFORE);
    let end = (at + pin.len() + EXCERPT_AFTER).min(hay.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(hay[start..end].iter());
    if end < hay.len() {
        out.push('…');
    }
    out
}

/// A turn while it is still being written: what has been said, and what is being done.
///
/// The distilling happens in `runner.rs` beside `extract_reply`, because knowing the CLI's stream
/// format is that module's job — and this route exists rather than pointing the window at
/// `/runs/{id}/tail` for the same reason: the tail is the raw stream, and a chat bubble is not the
/// place to learn what a `content_block_delta` is.
///
/// Read from byte zero on every poll, not from a cursor. What comes back is not a chunk to append
/// but the turn's whole state — text superseded by completed messages, a tool that has since
/// returned — and that can only be recomputed from the beginning. A turn's stream is small; a job's
/// is not, which is why `/runs/{id}/tail` keeps its cursor.
///
/// `204` when nothing is writing. That is the run having ended or this daemon never having started
/// it, and it is emphatically not "the turn said nothing" — see `read_tail`.
pub(super) async fn get_assistant_live(
    State(state): State<AppState>,
    Path(turn_id): Path<i64>,
) -> Result<Json<crate::execution::runner::LiveTurn>, StatusCode> {
    let stream = crate::execution::runs::read_tail(&state.run_tails, turn_id, 0)
        .ok_or(StatusCode::NO_CONTENT)?;
    Ok(Json(crate::execution::runner::live_from_stream(&stream)))
}

/// Length in bytes of a turn's tail, or `None` once nothing is writing it. Peeks without cloning
/// the text, so the poll loop below costs a pair of lock acquisitions per tick and not a copy.
pub(super) fn tail_len(tails: &crate::state::RunTails, run_id: i64) -> Option<usize> {
    let tail = tails.lock().unwrap().get(&run_id).cloned()?;
    let len = tail.lock().unwrap().len();
    Some(len)
}

/// `get_assistant_live` as a Server-Sent Events stream: one `live` event per change of the turn's
/// distilled state, then an `end` event when the run stops being written.
///
/// SSE rather than a socket because the traffic is one-way and plain HTTP, so the bearer token
/// travels in the header like on every other route and never in a URL. The events carry the
/// distilled [`crate::execution::runner::LiveTurn`], never the CLI's raw lines, for the reason given on
/// `get_assistant_live`: a chat bubble is not the place to learn what a `content_block_delta` is.
///
/// The tail is polled every 100 ms rather than waited on, so the writers (`runner.rs`) stay as
/// they are and nothing has to learn about subscribers. Only the length is read per tick. When it
/// grew, each subscriber reads just the new bytes from its own cursor and feeds the complete lines
/// to its own `LiveStream`, so a long turn costs what was added and not what was written; an event
/// goes out only if the state differs from the last one sent.
///
/// `204` when no tail exists, same as `/live`: the run ended or this daemon never started it.
pub(super) async fn get_assistant_live_stream(
    State(state): State<AppState>,
    Path(turn_id): Path<i64>,
) -> Result<axum::response::Response, StatusCode> {
    use tokio::io::AsyncWriteExt;

    crate::execution::runs::read_tail(&state.run_tails, turn_id, 0)
        .ok_or(StatusCode::NO_CONTENT)?;
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let tails = state.run_tails.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Where this subscriber has read to, what it has distilled so far, and the line the tail
        // has begun and not yet ended. Per subscriber: each tick reads only what is new.
        let mut cursor = 0usize;
        let mut first = true;
        let mut distilled = crate::execution::runner::LiveStream::default();
        let mut pending = String::new();
        let mut last: Option<crate::execution::runner::LiveTurn> = None;
        let mut idle = 0u32;
        loop {
            tick.tick().await;
            let Some(len) = tail_len(&tails, turn_id) else {
                let _ = writer.write_all(b"event: end\ndata: {}\n\n").await;
                break;
            };
            let mut wrote = false;
            if first || len != cursor {
                first = false;
                if let Some(chunk) = crate::execution::runs::read_tail(&tails, turn_id, cursor) {
                    cursor += chunk.len();
                    pending.push_str(&chunk);
                    while let Some(newline) = pending.find('\n') {
                        let line: String = pending.drain(..=newline).collect();
                        distilled.push_line(&line);
                    }
                    // A last line that is already whole JSON counts before its newline arrives,
                    // as it did when the stream was re-read from byte zero. Read on a copy, so
                    // the line is not pushed twice once it ends.
                    let whole = serde_json::from_str::<serde_json::Value>(pending.trim()).is_ok();
                    let live = if whole {
                        let mut view = distilled.clone();
                        view.push_line(&pending);
                        view.snapshot()
                    } else {
                        distilled.snapshot()
                    };
                    if last.as_ref() != Some(&live) {
                        let Ok(json) = serde_json::to_string(&live) else {
                            break;
                        };
                        let frame = format!("event: live\ndata: {json}\n\n");
                        if writer.write_all(frame.as_bytes()).await.is_err() {
                            break;
                        }
                        last = Some(live);
                        wrote = true;
                    }
                }
            }
            if wrote {
                idle = 0;
            } else {
                idle += 1;
                if idle >= 150 {
                    idle = 0;
                    if writer.write_all(b": keep-alive\n\n").await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    Ok((
        headers,
        axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(reader)),
    )
        .into_response())
}
