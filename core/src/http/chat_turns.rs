//! Sending a message and following its turn (`/assistant/message`, `/assistant/turns/*`, `/assistant/{turn_id}/live*`, relay, forward, browser agents and chat links).

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
        // A model asks for a NEW browser-agent conversation; the sender is read off
        // `RUN_ID_HEADER` exactly as `/relay` does, and the task travels as a relay.
        .route(
            "/assistant/browser-agents",
            get(get_browser_agents).post(post_browser_agent),
        )
        // Which conversation a chat answers to (a browser agent's mother), and the newest delivery
        // recorded on that link.
        .route(
            "/assistant/chats/{chat_id}/link",
            get(get_chat_link).post(post_chat_link),
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

/// The body `POST /assistant/browser-agents` takes: what the agent is to do, and optionally the
/// page to start on. The mother is never in the body; it is read off `RUN_ID_HEADER`, like `/relay`.
#[derive(Deserialize)]
pub(super) struct BrowserAgentRequest {
    pub(super) task: String,
    #[serde(default)]
    pub(super) url: Option<String>,
}

/// How a browser-agent creation can end short of a delivered task.
enum BrowserAgentFailure {
    /// The relay path's own failures, answered exactly as `/relay` answers them.
    Relay(RelayAttempt),
    /// Creating the child or its link failed outright.
    Internal,
}

/// `POST /assistant/browser-agents`. A model asks for a NEW browser-agent conversation.
///
/// The task travels as a relay from the asking turn to the new child, so every brake `/relay`
/// has (remote origin, owner away, chain budget) applies unchanged; the child is created and linked
/// first because `admit` wants a destination that exists, and is archived again when the gate or
/// the send says no, so a refusal leaves no live conversation behind.
pub(super) async fn post_browser_agent(
    State(state): State<AppState>,
    scope: Option<axum::Extension<crate::auth::Scope>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<BrowserAgentRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let Some(sending_run_id) = sending_run_id_of(&headers) else {
        return Err(refusal(StatusCode::BAD_REQUEST, "missing_run_id"));
    };
    // A chat turn's key (the `require_token` door) speaks for its own turn only: the header may
    // not name another conversation's turn and so borrow that turn's chat, cwd and origin. The
    // control token names whatever turn it acts for, as on `/relay`.
    if let Some(axum::Extension(crate::auth::Scope::Run(own_run))) = &scope
        && *own_run != sending_run_id
    {
        return Err(refusal(StatusCode::FORBIDDEN, "run_mismatch"));
    }
    // The chat key lives in a rooted turn's environment (`NUCLEOS_DAEMON_TOKEN`), so a turn that
    // read untrusted text could reach this route with curl and skip the classification the MCP
    // `browser_agent` tool goes through. On that door a tainted sender is refused outright; a
    // failed read refuses too, since it is no evidence the turn is clean.
    if let Some(axum::Extension(crate::auth::Scope::Run(own_run))) = &scope {
        match crate::execution::runs::read_untrusted_context(&state.pool, *own_run).await {
            Ok(false) => {}
            Ok(true) => return Err(refusal(StatusCode::FORBIDDEN, "untrusted_context")),
            Err(error) => {
                tracing::warn!(%error, run_id = *own_run, "could not read a sender's taint");
                return Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"));
            }
        }
    }
    let task = body.task.trim().to_string();
    if task.is_empty() {
        return Err(refusal(StatusCode::BAD_REQUEST, "empty_task"));
    }
    let start_page = match body.url.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => match reqwest::Url::parse(raw) {
            Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => Some(raw.to_string()),
            _ => return Err(refusal(StatusCode::BAD_REQUEST, "bad_url")),
        },
    };
    let message = match &start_page {
        Some(page) => format!("{task}\n\nStart page: {page}"),
        None => task.clone(),
    };

    // Uncancellable for the reason `relay_send_to_chat` is: the child is created in several steps
    // and the send writes a `running` row before spawning its task.
    let outcome = uncancellable(async move {
        let Some((mother, sending_origin)) = sending_turn_of(&state.pool, sending_run_id).await
        else {
            return Err(BrowserAgentFailure::Relay(RelayAttempt::UnknownSender));
        };
        let Some(sending_origin) = sending_origin else {
            return Err(BrowserAgentFailure::Relay(RelayAttempt::UnknownOrigin));
        };

        let child = crate::chats::create_on(
            &state.pool,
            crate::chats::Brain::Cloud,
            None,
            crate::chats::PermissionMode::Auto,
        )
        .await
        .map_err(|_| BrowserAgentFailure::Internal)?;
        let first_chars: String = task.chars().take(40).collect();
        let prepared: sqlx::Result<()> = async {
            crate::chats::rename(
                &state.pool,
                &child,
                Some(&format!("Browser · {first_chars}")),
            )
            .await?;
            if let Some(cwd) = crate::chats::cwd_of(&state.pool, &mother).await? {
                crate::chats::set_cwd(&state.pool, &child, &cwd).await?;
            }
            Ok(())
        }
        .await;
        let opened = match prepared {
            Ok(()) => crate::chats::links::open(
                &state.pool,
                &child,
                Some(&mother),
                "browser",
                Some(sending_run_id),
            )
            .await
            .is_ok(),
            Err(_) => false,
        };
        if !opened {
            let _ = crate::chats::archive(&state.pool, &child).await;
            return Err(BrowserAgentFailure::Internal);
        }

        let attempt = match crate::relay::admit(
            &state.pool,
            &mother,
            &child,
            sending_run_id,
            sending_origin,
            &message,
            chrono::Utc::now(),
        )
        .await
        {
            // `Origin::Shell` on the delivery for the reason `/relay` gives.
            Ok(relay_id) => crate::assistant::send_relayed_or_queue(
                &state,
                &child,
                &message,
                crate::assistant::Origin::Shell,
                relay_id,
            )
            .await
            .map_err(RelayAttempt::NotSent),
            Err(cause) => Err(RelayAttempt::Refused(cause)),
        };
        match attempt {
            Ok(sent) => Ok((child, sent)),
            Err(failure) => {
                // Archiving closes the link too; `unlink` is the explicit half in case the
                // archive found the chat already archived.
                let _ = crate::chats::archive(&state.pool, &child).await;
                let _ = crate::chats::links::unlink(&state.pool, &child).await;
                Err(BrowserAgentFailure::Relay(failure))
            }
        }
    })
    .await
    .map_err(|status| refusal(status, "internal"))?;

    match outcome {
        Ok((child, crate::assistant::Sent::Turn(turn_id))) => Ok(Json(
            serde_json::json!({ "chat_id": child, "turn_id": turn_id }),
        )),
        Ok((child, crate::assistant::Sent::Queued)) => Ok(Json(
            serde_json::json!({ "chat_id": child, "queued": true }),
        )),
        Err(BrowserAgentFailure::Relay(attempt)) => {
            relay_outcome_response(Err(attempt)).map(|_| Json(serde_json::Value::Null))
        }
        Err(BrowserAgentFailure::Internal) => {
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

/// The body `POST /assistant/chats/{chat_id}/link` takes. `parent_chat_id: null` is a link to
/// nobody: only the panel talks to that child.
#[derive(Deserialize)]
pub(super) struct ChatLinkRequest {
    pub(super) parent_chat_id: Option<String>,
    #[serde(default)]
    pub(super) kind: Option<String>,
}

/// `POST /assistant/chats/{chat_id}/link`. Closes the child's active link and opens a new one.
pub(super) async fn post_chat_link(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<ChatLinkRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    use crate::chats::links::LinkError;
    let kind = body.kind.as_deref().unwrap_or("browser");
    match crate::chats::links::open(
        &state.pool,
        &chat_id,
        body.parent_chat_id.as_deref(),
        kind,
        None,
    )
    .await
    {
        Ok(link) => Ok(Json(serde_json::json!({ "link": link }))),
        Err(LinkError::ChildNotFound) => Err(refusal(StatusCode::NOT_FOUND, "chat_not_found")),
        Err(LinkError::ToSelf) => Err(refusal(StatusCode::CONFLICT, "link_to_self")),
        Err(LinkError::ParentNotFound) => Err(refusal(StatusCode::CONFLICT, "parent_not_found")),
        Err(LinkError::Cycle { at }) => Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "refusal": "link_cycle", "at": at })),
        )),
        Err(LinkError::Db(error)) => {
            tracing::warn!(%error, "a chat link could not be written");
            Err(refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal"))
        }
    }
}

/// `GET /assistant/chats/{chat_id}/link`: the active link and the newest delivery recorded on it.
pub(super) async fn get_chat_link(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let internal = |error: sqlx::Error| {
        tracing::warn!(%error, "a chat link could not be read");
        refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    };
    let link = crate::chats::links::active(&state.pool, &chat_id)
        .await
        .map_err(internal)?;
    let last_delivery = match &link {
        Some(link) => {
            let row: Option<(Option<i64>, String, Option<String>, String)> = sqlx::query_as(
                "SELECT run_id, outcome, reason, created_at FROM chat_link_deliveries
                  WHERE link_id = ? ORDER BY created_at DESC, id DESC LIMIT 1",
            )
            .bind(link.id)
            .fetch_optional(&state.pool)
            .await
            .map_err(internal)?;
            row.map(|(run_id, outcome, reason, created_at)| {
                serde_json::json!({
                    "run_id": run_id,
                    "outcome": outcome,
                    "reason": reason,
                    "created_at": created_at,
                })
            })
        }
        None => None,
    };
    Ok(Json(
        serde_json::json!({ "link": link, "last_delivery": last_delivery }),
    ))
}

#[derive(Deserialize)]
pub(super) struct BrowserAgentsQuery {
    #[serde(default)]
    pub(super) limit: Option<i64>,
    #[serde(default)]
    pub(super) project_id: Option<String>,
    #[serde(default)]
    pub(super) no_project: Option<String>,
}

/// `GET /assistant/browser-agents`: children with an active link, newest link first. A child's
/// project is the project of its newest run, and none when it has no run yet.
pub(super) async fn get_browser_agents(
    State(state): State<AppState>,
    Query(query): Query<BrowserAgentsQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let no_project = query
        .no_project
        .as_deref()
        .is_some_and(|value| !matches!(value, "" | "0" | "false"));
    type Row = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT chat_id, title, parent_chat_id, parent_title, project_id, created_at FROM (
             SELECT l.child_chat_id AS chat_id, c.title AS title,
                    l.parent_chat_id AS parent_chat_id, p.title AS parent_title,
                    (SELECT r.project_id FROM runs r WHERE r.chat_id = l.child_chat_id
                      ORDER BY r.created_at DESC, r.id DESC LIMIT 1) AS project_id,
                    l.created_at AS created_at, l.id AS link_id
               FROM chat_links l
               JOIN chats c ON c.chat_id = l.child_chat_id
               LEFT JOIN chats p ON p.chat_id = l.parent_chat_id
              WHERE l.unlinked_at IS NULL
         )
          WHERE (? = 0 OR project_id IS NULL) AND (? IS NULL OR project_id = ?)
          ORDER BY created_at DESC, link_id DESC
          LIMIT ?",
    )
    .bind(no_project as i64)
    .bind(query.project_id.clone())
    .bind(query.project_id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(|error| {
        tracing::warn!(%error, "the browser agents could not be listed");
        refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
    })?;
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(chat_id, title, parent_chat_id, parent_title, project_id, created_at)| {
                serde_json::json!({
                    "chat_id": chat_id,
                    "title": title,
                    "parent_chat_id": parent_chat_id,
                    "parent_title": parent_title,
                    "project_id": project_id,
                    "created_at": created_at,
                })
            },
        )
        .collect();
    Ok(Json(serde_json::Value::Array(items)))
}

#[cfg(test)]
mod tests {
    //! Browser agents and chat links (`/assistant/browser-agents`, `/assistant/chats/{id}/link`).
    //!
    //! The mother's turn is a `running` row written by SQL, never sent through
    //! `assistant::send_message`: that would hold the mother's `ChatSlot` for a turn nobody
    //! finishes, and these routes only need the row the header names.

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    async fn state() -> AppState {
        super::super::tests::test_state().await
    }

    /// One request through the real router: optional `RUN_ID_HEADER`, optional JSON body.
    async fn call(
        state: &AppState,
        method: &str,
        uri: &str,
        run_id: Option<i64>,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", "Bearer test-token");
        if let Some(run_id) = run_id {
            request = request.header(crate::daemon_client::RUN_ID_HEADER, run_id.to_string());
        }
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

    /// The one row `attention::owner_is_present` reads, as the relay route tests seed it.
    async fn owner_present(state: &AppState) {
        sqlx::query(
            "INSERT INTO attention_heartbeats (scope, project_id, last_seen_at) VALUES ('global', '', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn chat(state: &AppState) -> String {
        crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap()
    }

    /// A `running` turn of `chat_id` with `origin` (`None` = not recorded), written by SQL.
    async fn running_turn(state: &AppState, chat_id: &str, origin: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, origin, created_at)
             VALUES ('ola', 'running', 'assistant', ?, ?, ?) RETURNING id",
        )
        .bind(chat_id)
        .bind(origin)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    /// Every `chat_links` row whose parent is `parent`, oldest first:
    /// `(child_chat_id, kind, created_by_run, unlinked_at)`.
    async fn links_under(
        state: &AppState,
        parent: &str,
    ) -> Vec<(String, String, Option<i64>, Option<String>)> {
        sqlx::query_as(
            "SELECT child_chat_id, kind, created_by_run, unlinked_at
               FROM chat_links WHERE parent_chat_id = ? ORDER BY id",
        )
        .bind(parent)
        .fetch_all(&state.pool)
        .await
        .unwrap()
    }

    async fn archived_at(state: &AppState, chat_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT archived_at FROM chats WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn count(state: &AppState, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn link(
        state: &AppState,
        child: &str,
        parent: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        call(
            state,
            "POST",
            &format!("/assistant/chats/{child}/link"),
            None,
            Some(serde_json::json!({ "parent_chat_id": parent })),
        )
        .await
    }

    #[tokio::test]
    async fn browser_agent_creates_a_linked_child_and_delivers_the_task_as_a_relay() {
        let state = state().await;
        owner_present(&state).await;
        let mother = chat(&state).await;
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        crate::chats::set_cwd(&state.pool, &mother, &cwd)
            .await
            .unwrap();
        let mother_turn = running_turn(&state, &mother, Some("shell")).await;
        let task = "Compare the prices of three laptops on the shop and report back";

        let (status, body) = call(
            &state,
            "POST",
            "/assistant/browser-agents",
            Some(mother_turn),
            Some(serde_json::json!({ "task": task, "url": "https://shop.example.com/" })),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let child = body["chat_id"]
            .as_str()
            .expect("the answer names the new child conversation")
            .to_string();
        assert_ne!(child, mother);

        let links = links_under(&state, &mother).await;
        assert_eq!(links.len(), 1, "one active link to the mother");
        let (linked_child, kind, created_by_run, unlinked_at) = &links[0];
        assert_eq!(linked_child, &child);
        assert_eq!(kind, "browser");
        assert_eq!(*created_by_run, Some(mother_turn));
        assert_eq!(*unlinked_at, None);

        assert_eq!(
            crate::chats::cwd_of(&state.pool, &child)
                .await
                .unwrap()
                .as_deref(),
            Some(cwd.as_str()),
            "the child works in the mother's folder"
        );
        let title: Option<String> =
            sqlx::query_scalar("SELECT title FROM chats WHERE chat_id = ?")
                .bind(&child)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let first_forty: String = task.chars().take(40).collect();
        assert_eq!(title, Some(format!("Browser · {first_forty}")));

        let relays: Vec<(String, String, i64, String)> = sqlx::query_as(
            "SELECT from_chat_id, to_chat_id, sending_run_id, body FROM chat_relays",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            relays,
            vec![(
                mother.clone(),
                child.clone(),
                mother_turn,
                format!("{task}\n\nStart page: https://shop.example.com/"),
            )],
            "the task travels as a relay from the mother's turn"
        );

        let turn_id = body["turn_id"]
            .as_i64()
            .expect("a free child answers with the turn the task opened");
        let (from_relay_id, turn_chat): (Option<i64>, String) =
            sqlx::query_as("SELECT from_relay_id, chat_id FROM runs WHERE id = ?")
                .bind(turn_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(
            from_relay_id.is_some(),
            "the child's turn records the relay that bore it"
        );
        assert_eq!(turn_chat, child);
        assert_eq!(
            crate::relay::chain_of(&state.pool, turn_id).await.unwrap(),
            vec![mother.clone(), child.clone()]
        );
    }

    #[tokio::test]
    async fn browser_agent_refused_by_the_gate_archives_the_child_and_closes_the_link() {
        let state = state().await;
        // No heartbeat: the owner is away.
        let mother = chat(&state).await;
        let mother_turn = running_turn(&state, &mother, Some("shell")).await;

        let (status, body) = call(
            &state,
            "POST",
            "/assistant/browser-agents",
            Some(mother_turn),
            Some(serde_json::json!({ "task": "look something up" })),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["refusal"], "owner_away");
        let links = links_under(&state, &mother).await;
        assert_eq!(links.len(), 1, "the link is closed, not deleted");
        let (child, _, _, unlinked_at) = &links[0];
        assert!(
            unlinked_at.is_some(),
            "a refused task leaves no active link"
        );
        assert!(
            archived_at(&state, child).await.is_some(),
            "the child the refusal orphaned is archived"
        );
        assert_eq!(
            count(&state, "SELECT COUNT(*) FROM chat_relays").await,
            0,
            "a refused relay leaves no row behind"
        );
    }

    #[tokio::test]
    async fn browser_agent_from_a_remote_channel_turn_is_refused() {
        let state = state().await;
        owner_present(&state).await;
        let mother = chat(&state).await;
        let mother_turn = running_turn(&state, &mother, Some("channel:telegram")).await;

        let (status, body) = call(
            &state,
            "POST",
            "/assistant/browser-agents",
            Some(mother_turn),
            Some(serde_json::json!({ "task": "look something up" })),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["refusal"], "remote_origin");
        assert_eq!(count(&state, "SELECT COUNT(*) FROM chat_relays").await, 0);
        for (child, _, _, unlinked_at) in links_under(&state, &mother).await {
            assert!(unlinked_at.is_some(), "no active link survives a refusal");
            assert!(archived_at(&state, &child).await.is_some());
        }
    }

    #[tokio::test]
    async fn browser_agent_needs_a_run_header_a_task_and_a_web_address() {
        let state = state().await;
        owner_present(&state).await;
        let mother = chat(&state).await;
        let mother_turn = running_turn(&state, &mother, Some("shell")).await;
        let uri = "/assistant/browser-agents";

        let (status, body) = call(
            &state,
            "POST",
            uri,
            None,
            Some(serde_json::json!({ "task": "look" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["refusal"], "missing_run_id");

        let (status, body) = call(
            &state,
            "POST",
            uri,
            Some(mother_turn),
            Some(serde_json::json!({ "task": "  \n " })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["refusal"], "empty_task");

        for url in [
            "ftp://example.com/",
            "javascript:alert(1)",
            "file:///C:/secret",
            "not a url",
        ] {
            let (status, body) = call(
                &state,
                "POST",
                uri,
                Some(mother_turn),
                Some(serde_json::json!({ "task": "look", "url": url })),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{url}: {body}");
            assert_eq!(body["refusal"], "bad_url", "{url}");
        }

        let (status, body) = call(
            &state,
            "POST",
            uri,
            Some(987_654),
            Some(serde_json::json!({ "task": "look" })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["refusal"], "unknown_sender");

        assert_eq!(
            count(&state, "SELECT COUNT(*) FROM chats").await,
            1,
            "a malformed request creates no conversation"
        );
        assert_eq!(count(&state, "SELECT COUNT(*) FROM chat_links").await, 0);
    }

    #[tokio::test]
    async fn relinking_a_chat_closes_the_old_link_and_opens_the_new_one() {
        let state = state().await;
        let child = chat(&state).await;
        let first_mother = chat(&state).await;
        let second_mother = chat(&state).await;

        let (status, first) = link(&state, &child, Some(&first_mother)).await;
        assert_eq!(status, StatusCode::OK, "{first}");
        assert_eq!(first["link"]["child_chat_id"], child.as_str());
        assert_eq!(first["link"]["parent_chat_id"], first_mother.as_str());
        assert_eq!(first["link"]["kind"], "browser", "kind defaults to browser");
        assert!(first["link"]["unlinked_at"].is_null());

        let (status, second) = call(
            &state,
            "POST",
            &format!("/assistant/chats/{child}/link"),
            None,
            Some(serde_json::json!({ "parent_chat_id": second_mother, "kind": "browser" })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{second}");
        assert_eq!(second["link"]["parent_chat_id"], second_mother.as_str());
        assert_ne!(second["link"]["id"], first["link"]["id"]);

        let history: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT parent_chat_id, unlinked_at FROM chat_links WHERE child_chat_id = ? ORDER BY id",
        )
        .bind(&child)
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(history.len(), 2, "relinking keeps the old row");
        assert_eq!(history[0].0.as_deref(), Some(first_mother.as_str()));
        assert!(history[0].1.is_some(), "the old link is closed");
        assert_eq!(history[1].0.as_deref(), Some(second_mother.as_str()));
        assert!(history[1].1.is_none(), "the new link is the active one");
    }

    #[tokio::test]
    async fn a_link_to_itself_a_missing_parent_or_a_cycle_is_refused() {
        let state = state().await;
        let a = chat(&state).await;
        let b = chat(&state).await;
        let archived = chat(&state).await;
        crate::chats::archive(&state.pool, &archived).await.unwrap();

        let (status, body) = link(&state, "never-created", Some(&a)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["refusal"], "chat_not_found");

        let (status, body) = link(&state, &a, Some(&a)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["refusal"], "link_to_self");

        for parent in ["never-created", archived.as_str()] {
            let (status, body) = link(&state, &a, Some(parent)).await;
            assert_eq!(status, StatusCode::CONFLICT, "{parent}: {body}");
            assert_eq!(body["refusal"], "parent_not_found", "{parent}");
        }

        // b answers to a; a answering to b would close a loop at a.
        let (status, body) = link(&state, &b, Some(&a)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = link(&state, &a, Some(&b)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["refusal"], "link_cycle");
        assert_eq!(body["at"], a.as_str(), "the loop closes at the child");

        let a_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chat_links WHERE child_chat_id = ?")
                .bind(&a)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(a_rows, 0, "a refused link writes nothing");
    }

    #[tokio::test]
    async fn linking_with_a_null_parent_opens_a_panel_only_link() {
        let state = state().await;
        let child = chat(&state).await;

        let (status, body) = link(&state, &child, None).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["link"]["child_chat_id"], child.as_str());
        assert!(body["link"]["parent_chat_id"].is_null(), "{body}");
        assert_eq!(body["link"]["kind"], "browser");
        assert!(body["link"]["created_by_run"].is_null());
        assert!(body["link"]["unlinked_at"].is_null());
        assert!(body["link"]["id"].is_i64());
        assert!(body["link"]["created_at"].is_string());
    }

    #[tokio::test]
    async fn get_link_returns_the_active_link_and_the_last_delivery() {
        let state = state().await;
        let child = chat(&state).await;
        let mother = chat(&state).await;
        let uri = format!("/assistant/chats/{child}/link");

        let (status, body) = call(&state, "GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body,
            serde_json::json!({ "link": null, "last_delivery": null }),
            "an unlinked chat has neither"
        );

        let (status, posted) = link(&state, &child, Some(&mother)).await;
        assert_eq!(status, StatusCode::OK, "{posted}");
        let link_id = posted["link"]["id"].as_i64().expect("a link id");

        let (_, body) = call(&state, "GET", &uri, None, None).await;
        assert_eq!(
            body["link"], posted["link"],
            "GET reads back the link POST wrote"
        );
        assert!(body["last_delivery"].is_null(), "nothing delivered yet");

        for (run_id, outcome, reason, at) in [
            (
                11_i64,
                "refused",
                Some("owner_away"),
                "2026-10-22T10:00:00+00:00",
            ),
            (12_i64, "delivered", None, "2026-10-22T10:05:00+00:00"),
        ] {
            sqlx::query(
                "INSERT INTO chat_link_deliveries (link_id, run_id, relay_id, outcome, reason, created_at)
                 VALUES (?, ?, NULL, ?, ?, ?)",
            )
            .bind(link_id)
            .bind(run_id)
            .bind(outcome)
            .bind(reason)
            .bind(at)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        let (status, body) = call(&state, "GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["link"]["id"], link_id);
        assert_eq!(
            body["last_delivery"],
            serde_json::json!({
                "run_id": 12,
                "outcome": "delivered",
                "reason": null,
                "created_at": "2026-10-22T10:05:00+00:00",
            }),
            "the newest delivery, not the first"
        );
    }

    #[tokio::test]
    async fn get_browser_agents_lists_active_links_newest_first_filtered_by_scope() {
        let state = state().await;
        let mother = chat(&state).await;
        crate::chats::rename(&state.pool, &mother, Some("Mother"))
            .await
            .unwrap();
        let in_project = chat(&state).await;
        crate::chats::rename(&state.pool, &in_project, Some("Browser · one"))
            .await
            .unwrap();
        let panel_only = chat(&state).await;
        let closed = chat(&state).await;
        let oldest = chat(&state).await;

        // (child, parent, link created_at, unlinked_at)
        for (child, parent, created_at, unlinked_at) in [
            (&oldest, Some(&mother), "2026-10-22T09:00:00+00:00", None),
            (&in_project, Some(&mother), "2026-10-22T10:00:00+00:00", None),
            (&panel_only, None, "2026-10-22T11:00:00+00:00", None),
            (
                &closed,
                Some(&mother),
                "2026-10-22T12:00:00+00:00",
                Some("2026-10-22T12:30:00+00:00"),
            ),
        ] {
            sqlx::query(
                "INSERT INTO chat_links (child_chat_id, parent_chat_id, kind, created_by_run, created_at, unlinked_at)
                 VALUES (?, ?, 'browser', NULL, ?, ?)",
            )
            .bind(child)
            .bind(parent)
            .bind(created_at)
            .bind(unlinked_at)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        // A child's project is its NEWEST run's: `in_project` moved from p-old to p1.
        for (chat_id, project, created_at) in [
            (&in_project, "p-old", "2026-10-22T10:01:00+00:00"),
            (&in_project, "p1", "2026-10-22T10:02:00+00:00"),
            (&oldest, "p2", "2026-10-22T09:01:00+00:00"),
            (&closed, "p1", "2026-10-22T12:01:00+00:00"),
        ] {
            sqlx::query(
                "INSERT INTO runs (prompt, status, mode, chat_id, project_id, origin, created_at)
                 VALUES ('t', 'completed', 'assistant', ?, ?, 'shell', ?)",
            )
            .bind(chat_id)
            .bind(project)
            .bind(created_at)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        let ids = |body: &serde_json::Value| -> Vec<String> {
            body.as_array()
                .unwrap_or_else(|| panic!("a list, got {body}"))
                .iter()
                .map(|item| item["chat_id"].as_str().unwrap().to_string())
                .collect()
        };

        let (status, all) = call(
            &state,
            "GET",
            "/assistant/browser-agents?limit=20",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{all}");
        assert_eq!(
            ids(&all),
            vec![panel_only.clone(), in_project.clone(), oldest.clone()],
            "active links only, newest link first"
        );
        let item = &all[1];
        assert_eq!(item["title"], "Browser · one");
        assert_eq!(item["parent_chat_id"], mother.as_str());
        assert_eq!(item["parent_title"], "Mother");
        assert_eq!(item["project_id"], "p1", "the newest run decides the project");
        assert!(item["created_at"].is_string());
        let panel = &all[0];
        assert!(panel["title"].is_null());
        assert!(panel["parent_chat_id"].is_null());
        assert!(panel["parent_title"].is_null());
        assert!(panel["project_id"].is_null(), "no run, no project");

        let (_, limited) = call(
            &state,
            "GET",
            "/assistant/browser-agents?limit=1",
            None,
            None,
        )
        .await;
        assert_eq!(ids(&limited), vec![panel_only.clone()]);

        let (status, p1) = call(
            &state,
            "GET",
            "/assistant/browser-agents?limit=20&project_id=p1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{p1}");
        assert_eq!(
            ids(&p1),
            vec![in_project.clone()],
            "a closed link is not listed"
        );

        let (status, none) = call(
            &state,
            "GET",
            "/assistant/browser-agents?limit=20&no_project=1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{none}");
        assert_eq!(ids(&none), vec![panel_only.clone()]);
    }

    /// Spec §5 through the panel's own list: a child whose mother was archived is still listed,
    /// with no parent; a child that was itself archived is not.
    #[tokio::test]
    async fn get_browser_agents_keeps_a_child_whose_mother_was_archived_and_drops_an_archived_child()
    {
        let state = state().await;
        let mother = chat(&state).await;
        let child = chat(&state).await;
        crate::chats::links::open(&state.pool, &child, Some(&mother), "browser", None)
            .await
            .unwrap();

        crate::chats::archive(&state.pool, &mother).await.unwrap();

        let (status, listed) = call(
            &state,
            "GET",
            "/assistant/browser-agents?limit=20",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        let items = listed.as_array().unwrap_or_else(|| panic!("a list, got {listed}"));
        assert_eq!(items.len(), 1, "{listed}");
        assert_eq!(items[0]["chat_id"], child.as_str());
        assert!(items[0]["parent_chat_id"].is_null(), "{listed}");
        assert!(items[0]["parent_title"].is_null(), "{listed}");

        crate::chats::archive(&state.pool, &child).await.unwrap();

        let (status, listed) = call(
            &state,
            "GET",
            "/assistant/browser-agents?limit=20",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        assert_eq!(listed, serde_json::json!([]), "an archived child is not listed");
    }

    /// One POST to `/assistant/browser-agents` with `bearer` instead of the control token.
    async fn post_browser_agents_as(
        state: &AppState,
        bearer: &str,
        run_id: i64,
    ) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/assistant/browser-agents")
            .header("Authorization", format!("Bearer {bearer}"))
            .header(crate::daemon_client::RUN_ID_HEADER, run_id.to_string())
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "task": "find the opening hours" }).to_string(),
            ))
            .unwrap();
        let response = super::super::build_router(state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    /// The door is for a conversation's key only. A chat turn's own run token (`runs.token`) is
    /// what a rooted turn's shell could read from its environment and replay, so it must not open
    /// it, even for a running turn that belongs to a conversation. And a chat key may only speak
    /// for its own turn: naming another conversation's turn in the header is refused.
    #[tokio::test]
    async fn a_chat_runs_own_run_token_may_not_post_browser_agents() {
        let state = state().await;
        owner_present(&state).await;
        let mother = chat(&state).await;
        let turn = running_turn(&state, &mother, Some("shell")).await;
        let (run_bearer, secret) = crate::auth::mint_run_token(turn);
        sqlx::query("UPDATE runs SET token = ? WHERE id = ?")
            .bind(&secret)
            .bind(turn)
            .execute(&state.pool)
            .await
            .unwrap();

        let (status, body) = post_browser_agents_as(&state, &run_bearer, turn).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a run token is not a chat key: {body}");

        let other = chat(&state).await;
        let other_turn = running_turn(&state, &other, Some("shell")).await;
        let key = crate::auth::mint_chat_token(&state.pool, &mother).await.unwrap();
        let (status, body) = post_browser_agents_as(&state, &key, other_turn).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["refusal"], "run_mismatch");

        let (status, body) = post_browser_agents_as(&state, &key, turn).await;
        assert_eq!(status, StatusCode::OK, "the chat key of the running turn is admitted: {body}");
        assert!(body["chat_id"].is_string(), "{body}");
    }

    /// The chat key is in a rooted turn's environment, so a turn that read untrusted text could
    /// call the route itself and skip the MCP tool's classification. A tainted sender is refused
    /// on the chat-key door, and no child conversation is left behind.
    #[tokio::test]
    async fn a_tainted_chat_turn_may_not_post_browser_agents_with_its_chat_key() {
        let state = state().await;
        owner_present(&state).await;
        let mother = chat(&state).await;
        let turn = running_turn(&state, &mother, Some("shell")).await;
        crate::execution::runs::mark_untrusted_context(&state.pool, turn)
            .await
            .unwrap();
        let chats_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chats")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let key = crate::auth::mint_chat_token(&state.pool, &mother).await.unwrap();

        let (status, body) = post_browser_agents_as(&state, &key, turn).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["refusal"], "untrusted_context");
        let chats_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chats")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(chats_after, chats_before, "a refused sender creates no child");
    }
}
