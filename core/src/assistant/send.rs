//! Message origins, sending, queued delivery, images, and history handover.

use super::*;

/// Which client sent a chat message, as the client states it.
///
/// Stated rather than inferred, and that is why this type exists at all. A Telegram group id is
/// negative, so the origin is guessable from the shape of `chat_id` — and guessing would make a
/// routing decision depend on a numbering scheme Telegram owns and can change without telling
/// anybody. A client that says nothing is `Shell`, which keeps every existing caller where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Shell,
    Telegram,
    /// Spoken into the microphone of the machine this daemon runs on.
    ///
    /// A variant and not a flag on `Shell`, because the two differ in what happens to the ANSWER:
    /// a shell turn is read, a voice turn is spoken. Recorded on the row for the same reason the
    /// other two are — a queued message must be sent as the thing it was.
    Voice,
    /// Typed in the panel of a NucleOS browser window on this machine; answered like `Shell`.
    BrowserPanel,
}

impl Origin {
    /// An unknown spelling resolves to `Shell` for the same reason absence does: this is a
    /// ship-dark feature, so anything not explicitly asking for the new path gets the old one.
    pub fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("telegram") => Self::Telegram,
            Some("voice") => Self::Voice,
            Some("browser-panel") => Self::BrowserPanel,
            _ => Self::Shell,
        }
    }

    /// How this origin is written down, so a message that waits is sent as the thing it was.
    ///
    /// The inverse of `from_wire` and asserted against it: a queued message carries its origin
    /// through the database, and an origin that did not survive the round trip would route a
    /// Telegram turn back into the shell.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Voice => "voice",
            Self::BrowserPanel => "browser-panel",
            Self::Shell => "shell",
        }
    }
}

/// Why a turn was refused before it cost anything: the chat says `local` and this machine has none.
///
/// A named constant rather than a sentence written twice, because `http.rs` turns it into the one
/// status code that tells this apart from a daemon that broke. Matched exactly there — a refusal
/// recognised by a substring is a refusal that stops being recognised when someone edits the words.
pub const NO_LOCAL_MODEL: &str = "this chat is set to the local model and none is configured";

/// Why a turn was refused before it cost anything: the chat says `openrouter` and this daemon has
/// no hosted model wired up to answer it.
///
/// `NO_LOCAL_MODEL`'s sibling and not its synonym, and the two have to stay apart because the
/// failure they each guard is aimed at a different wallet. A chat that says `local` and finds no
/// local model falls back to the CLI today when the choice was only inferred (`origin ==
/// Origin::Telegram`) and refuses only when a person SAID `local` outright — because
/// the CLI has always answered an unmarked Telegram message, so refusing there would take the bot
/// off the air to enforce a promise nobody made. `openrouter` has no such history: nothing before
/// this brain existed could ever have chosen it, so there is no old behaviour to preserve and no
/// argument for falling through at all. Every turn that reaches this brain is refused, full stop,
/// until the hosted route is actually wired — the placeholder this constant replaces the silence
/// of (`(None, Some(crate::chats::Brain::OpenRouter)) => false` in `send_message_with`) is exactly
/// what let that turn go to the cloud CLI instead: silently, and on the bill.
///
/// A named constant for the same reason `NO_LOCAL_MODEL` is one: `http.rs` will need to turn this
/// into a status code a client can act on, and a refusal recognised by a fragment of its wording
/// stops being recognised the day somebody improves the sentence.
///
pub const NO_HOSTED_MODEL: &str = "this chat is set to the hosted model and none is configured";

/// What became of a message somebody sent: a turn, or a place in the queue.
///
/// Two outcomes rather than an `Option<i64>`, because the caller has something different to say
/// about each and a null id says neither. A turn is answered by watching it; a queued message is
/// answered by showing it waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Turn(i64),
    Queued,
}

/// Sends a message, or keeps it until the conversation has a turn free.
///
/// The wall this removes: a second message used to take `TURN_IN_PROGRESS` and vanish, so a person
/// who had already thought of the next thing to say got a red note and an empty box.
///
/// Opted into rather than imposed, because waiting is not always better than being told no. The
/// Telegram sidecar gives up on a turn after a timeout and would rather refuse than answer ten
/// minutes late into a conversation that has moved on — so only a caller that can wait asks to.
///
/// Written as a try-then-queue rather than a check-then-send: `is_busy` between the two would be a
/// window in which the turn ends and the message queues behind nothing, waiting for a drain that
/// has already run. Letting `send_message` refuse is what makes the two steps one decision.
pub async fn send_or_queue(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    images: &[crate::execution::runner::Attachment],
    origin: Origin,
) -> Result<Sent, String> {
    // A person sending again is what ends a hold (after Stop, or after a failed send).
    release_queue(chat_id);
    // Serialised here rather than at the drain, because this is the only moment the bytes are in
    // hand. A message that waits with its pictures is the whole of what was sent; one that waits
    // without them is half of it, silently.
    let carried = || {
        serde_json::to_string(
            &images
                .iter()
                .map(|image| {
                    serde_json::json!({
                        "media_type": image.media_type,
                        "data": image.data,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".to_string())
    };
    // Behind what already waits, even when the chat is free: jumping the queue would answer the
    // newest thing first.
    let waiting = crate::chats::queued(&state.pool, chat_id)
        .await
        .map_err(|error| error.to_string())?;
    if !waiting.is_empty() {
        crate::chats::enqueue(&state.pool, chat_id, text, origin.as_wire(), &carried())
            .await
            .map_err(|error| error.to_string())?;
        spawn_drain_if_free(state, chat_id);
        return Ok(Sent::Queued);
    }
    match send_message_with(state, chat_id, text, images, origin).await {
        Ok(id) => Ok(Sent::Turn(id)),
        Err(refusal) if refusal == TURN_IN_PROGRESS => {
            crate::chats::enqueue(&state.pool, chat_id, text, origin.as_wire(), &carried())
                .await
                .map_err(|error| error.to_string())?;
            // The turn may have ended between the refusal and the write; then nobody would drain it.
            spawn_drain_if_free(state, chat_id);
            Ok(Sent::Queued)
        }
        Err(other) => Err(other),
    }
}

/// Starts the queue draining in a task of its own when nothing holds the chat's slot; otherwise the
/// slot's holder drains on release.
fn spawn_drain_if_free(state: &crate::state::AppState, chat_id: &str) {
    spawn_drain_through(state, chat_id, false);
}

/// [`spawn_drain_if_free`] choosing whether the drain respects a hold.
///
/// A plain function and not an async one on purpose: `drain_inner` calls it, and spawning
/// `drain_inner` from inside its own future would ask for that future's `Send` while it is still
/// being defined — a cycle the compiler refuses.
fn spawn_drain_through(state: &crate::state::AppState, chat_id: &str, respect_hold: bool) {
    if is_busy(chat_id) {
        return;
    }
    let state = state.clone();
    let chat = chat_id.to_string();
    tokio::spawn(async move {
        if respect_hold {
            drain_queued(&state, &chat).await;
        } else {
            drain_queued_now(&state, &chat).await;
        }
    });
}

/// Writes a turn's pictures where the window can ask for them again, answering their paths.
///
/// Paths and not bytes on the row: `runs` is read on every transcript poll and on every list, and a
/// column holding base64 screenshots would drag megabytes through queries that want a prompt and a
/// status.
///
/// A daemon with no files root keeps nothing and says so by answering an empty list. The turn still
/// goes — the model sees the picture either way — and what is lost is being able to look at it
/// afterwards.
pub(super) fn keep_images(
    state: &crate::state::AppState,
    id: i64,
    images: &[crate::execution::runner::Attachment],
) -> Vec<String> {
    use base64::Engine;
    let Some(root) = state.files_root.as_deref() else {
        return Vec::new();
    };
    if images.is_empty() {
        return Vec::new();
    }
    if let Err(error) = crate::files::create_folder(root, CHAT_IMAGES) {
        tracing::warn!(?error, "could not make the folder a turn's pictures go in");
        return Vec::new();
    }

    let mut kept = Vec::new();
    for (at, image) in images.iter().enumerate() {
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&image.data) else {
            tracing::warn!(id, at, "a picture could not be decoded and was not kept");
            continue;
        };
        let name = format!("{id}-{at}.{}", extension_of(&image.media_type));
        match crate::files::write_file(root, CHAT_IMAGES, &name, &bytes) {
            Ok(written) => kept.push(format!("{CHAT_IMAGES}/{written}")),
            Err(error) => tracing::warn!(?error, id, at, "a picture could not be written"),
        }
    }
    kept
}

/// Where a conversation's pictures live under the files root.
pub(super) const CHAT_IMAGES: &str = "chats";

/// The file extension for a claimed media type.
///
/// The sender's claim, taken at face value and used only to name a file. Nothing here sniffs the
/// bytes: a run reading the picture is reading it either way, and a daemon second-guessing the
/// label would be deciding on behalf of a model that can see it.
pub(super) fn extension_of(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Records where a turn's pictures were kept — `[]` for a turn that carried none.
///
/// An empty list rather than NULL, deliberately: NULL is what a turn from before this column has,
/// and "carried nothing" and "nobody asked" are different facts.
pub(super) async fn record_images(pool: &SqlitePool, id: i64, kept: &[String]) -> sqlx::Result<()> {
    let stored = serde_json::to_string(kept).unwrap_or_else(|_| "[]".to_string());
    sqlx::query("UPDATE runs SET prompt_images = ? WHERE id = ?")
        .bind(stored)
        .bind(id)
        .execute(pool)
        .await
        .map(|_| ())
}

/// Sends whatever waited, once the conversation has a turn free for it.
///
/// Called from ONE place: the release of an armed `ChatSlot` (`ChatSlot::drain_on_release`) spawns it
/// as a task of its own, after the slot has already been removed from the busy set — the drain goes
/// back through `send_message`, which takes the same chat slot, so it must run after that slot is
/// free and outside any run task a Stop could abort. `drain_waiting_queues` calls it once at start-up
/// for what a previous daemon left waiting.
///
/// Respects the hold: after a Stop (`hold_queue`), after a send that failed, or while the newest
/// assistant run of the chat is `cancelled`, nothing is sent until the person sends again.
///
/// One message per turn, not the whole queue: each drained message becomes a turn that will drain
/// again when it ends. Sending them all at once would only refuse every one after the first.
///
/// Boxed because this and `send_message` call each other — a real cycle, and the compiler needs the
/// indirection to size the future. A message a person typed that cannot be sent goes back to the
/// FRONT of the queue under its old id, the queue is held and a notice says so; a relay-born message
/// that cannot be sent is dropped, as before.
///
/// A relay-born message gets a second look here that a person's own message does not — see the
/// `Some(relay_id)` branch below for why the gap between `relay::admit` deciding and this drain
/// spending the turn is the one place its answer can go stale.
pub(super) async fn drain_queued(state: &crate::state::AppState, chat_id: &str) {
    drain_inner(state, chat_id, true).await;
}

/// [`drain_queued`] ignoring the hold: for a person who just sent a message, which is what ends it.
pub(super) async fn drain_queued_now(state: &crate::state::AppState, chat_id: &str) {
    drain_inner(state, chat_id, false).await;
}

async fn drain_inner(state: &crate::state::AppState, chat_id: &str, respect_hold: bool) {
    if respect_hold {
        if queue_is_held(chat_id) {
            return;
        }
        let latest: Option<String> = sqlx::query_scalar(
            "SELECT status FROM runs WHERE chat_id = ? AND mode = 'assistant'
             ORDER BY id DESC LIMIT 1",
        )
        .bind(chat_id)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten();
        if latest.as_deref() == Some("cancelled") {
            return;
        }
    }
    if is_busy(chat_id) {
        return;
    }
    let taken = match sqlx::query_as::<
        _,
        (
            i64,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
            String,
        ),
    >(
        "DELETE FROM chat_queue
          WHERE id = (SELECT id FROM chat_queue WHERE chat_id = ? ORDER BY id LIMIT 1)
      RETURNING id, text, origin, images, relay_id, created_at",
    )
    .bind(chat_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(taken)) => taken,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, chat_id, "could not read what was waiting for this conversation");
            return;
        }
    };
    let (queue_id, text, origin, carried, relay_id, created_at) = taken;
    // A conversation that was archived while the message waited does not get it: falling back to
    // the cloud for a chat that no longer exists would spend money on words nobody is reading.
    let archived: Option<bool> =
        sqlx::query_scalar("SELECT archived_at IS NOT NULL FROM chats WHERE chat_id = ?")
            .bind(chat_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    if archived == Some(true) {
        tracing::warn!(
            chat_id,
            "a message that had been waiting was dropped: its conversation was archived"
        );
        return;
    }
    let put_back = (
        queue_id,
        text.clone(),
        origin.clone(),
        carried.clone(),
        created_at,
    );
    // The fourth element is the relay this message travelled on, or `None` for one a person typed —
    // see `take_queued`'s own doc. It decides which of the two doors below the resend goes back
    // through, so a message that waited still produces a run recording where it came from.
    let origin = Origin::from_wire(origin.as_deref());
    let sent = match relay_id {
        // No pictures on this branch: `send_relayed_message` never takes any, for the reason its
        // own doc gives — a relay carries `chat_relays.body`, plain text, never an attachment — so
        // `carried` is not even parsed here.
        Some(relay_id) => {
            // `relay::admit` certified this hop once, at the moment `relay_send_to_chat` called
            // it — but a message that had to queue is sent LATER, here, by a drain that can run
            // minutes after that certificate was issued. Everything `admit` checked at that moment
            // can have changed since: the owner it saw present can have walked away, and the
            // destination it checked against can have been archived while the message sat
            // waiting. `admit`'s answer has an expiry, in other words, and this is the one path
            // where the gap between deciding and spending can be long enough for the answer to
            // have changed — so the two time-sensitive brakes are asked again, here, right before
            // the turn they would gate is actually spent.
            //
            // A message a person typed is never re-checked this way — see `relay_id => None`,
            // below. Re-confirming "is anyone there" before delivering someone their own words, in
            // their own conversation, would be refusing them their own conversation for the crime
            // of having stepped away from the keyboard; the presence brake exists to gate
            // autonomy, not to gate a person talking to themselves. A relay has no such standing —
            // it was admitted on ANOTHER conversation's say-so, for an owner who never typed it —
            // which is exactly the case the brake exists to catch.
            //
            // The house budget is deliberately left uncounted here, matching `relay::admit`'s own
            // choice not to touch it (see that function's doc comment): `budget.rs` already counts
            // every relay-born run on its own terms. The chain's OWN ceiling is asked again, below,
            // for the reason presence is: turns of the same chain can have spent while this waited.
            //
            // A brake that catches something here drops the message rather than retrying it:
            // `take_queued` has already deleted its row, so there is nothing left to retry — the
            // same trade this function's own doc comment already makes for a send that fails
            // outright, extended to a send this drain now declines to even attempt.
            let now = chrono::Utc::now();
            if !crate::attention::owner_is_present(&state.pool, now).await {
                tracing::warn!(
                    chat_id,
                    relay_id,
                    "a relayed message that had been waiting was dropped: the owner is no longer present"
                );
                return;
            }
            let room = match crate::relay::root_of_relay(&state.pool, relay_id).await {
                Ok(root) => crate::relay::chain_has_room(&state.pool, root, now).await,
                Err(error) => Err(crate::relay::Refusal::Unreadable {
                    step: "chain walk",
                    error: error.to_string(),
                }),
            };
            if let Err(refusal) = room {
                tracing::warn!(
                    chat_id,
                    relay_id,
                    ?refusal,
                    "a relayed message that had been waiting was dropped: its chain has no room left"
                );
                return;
            }
            match crate::chats::brain_of(&state.pool, chat_id).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    tracing::warn!(
                        chat_id,
                        relay_id,
                        "a relayed message that had been waiting was dropped: its destination is gone or archived"
                    );
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        chat_id,
                        relay_id,
                        "a relayed message that had been waiting was dropped: its destination could not be read"
                    );
                    return;
                }
            }
            Box::pin(send_relayed_message(
                state, chat_id, &text, origin, relay_id,
            ))
            .await
        }
        None => {
            // A queue row that will not parse is sent without its pictures rather than not sent at
            // all: the words are the part somebody is waiting on an answer to, and refusing the
            // whole turn over an unreadable column would lose those too.
            let images: Vec<crate::execution::runner::Attachment> = carried
                .as_deref()
                .and_then(|stored| serde_json::from_str::<Vec<serde_json::Value>>(stored).ok())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|image| {
                    Some(crate::execution::runner::Attachment {
                        media_type: image.get("media_type")?.as_str()?.to_string(),
                        data: image.get("data")?.as_str()?.to_string(),
                    })
                })
                .collect();
            Box::pin(send_message_with(state, chat_id, &text, &images, origin)).await
        }
    };
    let Err(refusal) = sent else {
        return;
    };
    tracing::warn!(
        %refusal,
        chat_id,
        "a message that had been waiting could not be sent"
    );
    if relay_id.is_some() {
        return;
    }
    put_back_refused(state, chat_id, put_back, &refusal, respect_hold).await;
}

/// What a person typed and a drain could not send, as it was taken off the queue: id, text,
/// origin, pictures, created_at.
type PutBack = (i64, String, Option<String>, Option<String>, String);

/// Puts a refused message back at the front under its old id, then decides who sends it next.
///
/// On `TURN_IN_PROGRESS` the slot's holder drains on release — but only if it releases AFTER the
/// row is back. The row was deleted before the send was tried, so a holder that released in
/// between found the queue empty, and nothing would ever drain the restored row. So the slot is
/// asked again here, as `send_or_queue` does after its own enqueue. It cannot spin: a drain is
/// spawned only when the slot is free, and a free slot is exactly what a refusal of this kind
/// did not see — the next attempt either sends, or meets a new holder that drains on release.
async fn put_back_refused(
    state: &crate::state::AppState,
    chat_id: &str,
    put_back: PutBack,
    refusal: &str,
    respect_hold: bool,
) {
    // A message a person typed is never lost: it goes back to the front under its old id.
    let (queue_id, text, origin, carried, created_at) = put_back;
    let restored = sqlx::query(
        "INSERT INTO chat_queue (id, chat_id, text, origin, images, relay_id, created_at)
         VALUES (?, ?, ?, ?, ?, NULL, ?)",
    )
    .bind(queue_id)
    .bind(chat_id)
    .bind(text)
    .bind(origin)
    .bind(carried)
    .bind(created_at)
    .execute(&state.pool)
    .await;
    if let Err(error) = restored {
        tracing::warn!(%error, chat_id, "a message that could not be sent could not be put back either");
        return;
    }
    // Somebody else holds the slot: they drain it when they release, so there is nothing to report.
    // Unless they already released, between this drain taking the row and putting it back.
    if refusal == TURN_IN_PROGRESS {
        // The same door this drain came through: a drain that respected a hold must not become
        // one that ignores it just because it lost a race.
        spawn_drain_through(state, chat_id, respect_hold);
        return;
    }
    hold_queue(chat_id);
    if let Err(error) = crate::chats::notices::post_system(
        &state.pool,
        chat_id,
        "queue",
        0,
        &format!(
            "A waiting message could not be sent ({refusal}); it is still waiting and goes when you send again."
        ),
    )
    .await
    {
        tracing::warn!(%error, chat_id, "could not post the notice about a waiting message");
    }
}

/// Drains, once, every queue a previous daemon left waiting. Held queues are left alone.
pub async fn drain_waiting_queues(state: crate::state::AppState) {
    let chats: Vec<String> = sqlx::query_scalar("SELECT DISTINCT chat_id FROM chat_queue")
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();
    for chat in chats {
        drain_queued(&state, &chat).await;
    }
}

/// Why a turn was refused before it cost anything: this conversation is already answering one.
///
/// A constant because two callers now have to recognise it and act differently on it. `http.rs`
/// turns it into the one status a client can retry on, and `scheduler.rs` reads it as "nothing was
/// started" and hands the window back so the rule tries again on the next tick.
///
/// Matched by value at both, never by substring. A refusal recognised by a fragment of its wording
/// stops being recognised the moment somebody improves the sentence, and the two behaviours that
/// depend on it would fail apart and silently.
pub const TURN_IN_PROGRESS: &str = "a turn is already in progress for this chat";

/// How many pictures one turn may carry.
///
/// A ceiling on what a person can attach in one go, not on what the model can read. Past a handful
/// the question stops being about the pictures and the cost of the turn stops being predictable.
pub const MAX_IMAGES: usize = 5;

/// The largest a single picture may be, as base64 characters. Roughly 5MB of bytes, which is what
/// the API accepts for one image.
pub const MAX_IMAGE_CHARS: usize = 7_000_000;

/// Why a turn was refused before it cost anything: it carried more pictures than a turn may.
pub const TOO_MANY_IMAGES: &str = "a turn may carry at most five pictures";

/// Why a turn was refused before it cost anything: one of its pictures is too big to send.
pub const IMAGE_TOO_LARGE: &str = "one of those pictures is too large to send";

/// Sends a message with nothing attached. Only the tests send without pictures through this
/// shorthand; production callers go through `send_message_with`.
#[cfg(any(test, feature = "testkit"))]
pub async fn send_message(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    origin: Origin,
) -> Result<i64, String> {
    send_message_with(state, chat_id, text, &[], origin).await
}

/// The conversation's pinned model (`chats::Answering::model`), read fresh rather than cached
/// across a turn.
///
/// A failure to read it must NOT refuse the turn: this returns `None` — the route's own configured
/// default — and logs a warning instead of propagating the error. `recent_exchanges` takes the same
/// posture when history cannot be read, and `chats::answering`'s own doc makes the same argument for
/// a missing row answering `Default`.
///
/// Called from inside each of `send_message_with`'s two route branches, never above them: a cloud
/// turn reaches neither branch, and hoisting the call above both would add this query to the
/// busiest path — the cloud turn — to serve two branches that both return before it even starts.
pub(super) async fn pinned_model(state: &crate::state::AppState, chat_id: &str) -> Option<String> {
    match crate::chats::answering(&state.pool, chat_id).await {
        Ok(answering) => answering.model,
        Err(error) => {
            tracing::warn!(
                %error,
                chat_id,
                "could not read the conversation's pinned model; falling back to the route's default"
            );
            None
        }
    }
}

/// Sends a message, with whatever pictures were attached to it.
///
/// The pictures travel INSIDE the message rather than as paths for the model to go and read: they
/// are part of what was said. Measured against the CLI before any of this was built — a `user` line
/// whose content is an array with an `image` block is accepted, and a solid magenta square asked
/// about came back "Magenta".
///
/// Which forces the stdin path, because an argument vector holds a string and there is nowhere in
/// it for bytes to go. `steerable` and `images` are therefore decided together, below, at the one
/// place that can see both.
///
/// Never carries a relay id — this is the door every message a person (or Telegram, or the IDE)
/// actually sent comes in through, and `send_message_inner` below is what keeps that true rather
/// than trusting every one of ITS callers to pass `None` correctly.
pub async fn send_message_with(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    images: &[crate::execution::runner::Attachment],
    origin: Origin,
) -> Result<i64, String> {
    send_message_inner(state, chat_id, text, images, origin, None).await
}

/// Sends a message that arrived by relay from another conversation, recording which relay bore it.
///
/// The one caller of `send_message_inner` allowed to pass a relay id. `relay.rs`'s header explains
/// why that matters: `chain_of` walks `runs.from_relay_id`, and a run born from a relay that did not
/// record so would read back as a turn a person wrote — the chain resets to zero exactly where the
/// cycle brake needs it not to. No images: a relay carries `chat_relays.body`, plain text, never an
/// attachment — the same reason `spawn_local_turn` never takes any either.
pub async fn send_relayed_message(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    origin: Origin,
    relay_id: i64,
) -> Result<i64, String> {
    send_message_inner(state, chat_id, text, &[], origin, Some(relay_id)).await
}

/// Sends a relayed message, or keeps it until the destination has a turn free.
///
/// The relay path's counterpart to `send_or_queue`, above, and built the same try-then-queue way for
/// the same reason: asking `is_busy` first would leave a window between "the destination looked
/// busy" and "the message was queued" in which the turn it meant to wait behind ends and the message
/// queues behind nothing, waiting for a drain that has already run. Letting `send_relayed_message`
/// itself refuse is what makes the two steps one decision, exactly as it is for a message a person
/// typed.
///
/// Before this door existed, a relay into a busy conversation had no second chance at all:
/// `TURN_IN_PROGRESS` reached the caller as an ordinary refusal, the words were never queued, and a
/// relay into a conversation that happened to be mid-turn was simply lost. `relay::admit` having
/// granted the hop is not the same fact as the destination being free to receive it *right now* —
/// those are two different moments, and only one of them was ever handled.
///
/// `chats::enqueue_relayed`, not `enqueue`: the relay id has to travel with the waiting message, or
/// `drain_queued` would have no way to tell a relay-born wait from one a person typed — and, per its
/// own comment, that distinction is what decides whether the brakes are re-checked before the turn
/// is spent.
pub async fn send_relayed_or_queue(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    origin: Origin,
    relay_id: i64,
) -> Result<Sent, String> {
    match send_relayed_message(state, chat_id, text, origin, relay_id).await {
        Ok(id) => Ok(Sent::Turn(id)),
        Err(refusal) if refusal == TURN_IN_PROGRESS => {
            crate::chats::enqueue_relayed(
                &state.pool,
                chat_id,
                text,
                origin.as_wire(),
                "[]",
                relay_id,
            )
            .await
            .map_err(|error| error.to_string())?;
            Ok(Sent::Queued)
        }
        Err(other) => Err(other),
    }
}

/// The shared body behind `send_message_with` and `send_relayed_message`.
///
/// `relay_id` is written into the `INSERT INTO runs` below in the same statement that creates the
/// row — never by an `UPDATE` once the id is known. That is the property 0122 exists for: an INSERT
/// followed by an UPDATE has a window in which a relay-born run sits with `from_relay_id IS NULL`,
/// and `relay::chain_of` cannot tell that window apart from a turn a person actually wrote — it
/// would stop its walk there and hand `relay::admits` a chain that reads as one hop shorter than it
/// is. A failure between the two statements would make that misreading permanent rather than a race
/// that usually loses.
pub(super) async fn send_message_inner(
    state: &crate::state::AppState,
    chat_id: &str,
    text: &str,
    images: &[crate::execution::runner::Attachment],
    origin: Origin,
    relay_id: Option<i64>,
) -> Result<i64, String> {
    // Refused before the slot is taken, so a turn nobody can send costs nothing and leaves the
    // conversation answerable.
    if images.len() > MAX_IMAGES {
        return Err(TOO_MANY_IMAGES.to_string());
    }
    if images.iter().any(|i| i.data.len() > MAX_IMAGE_CHARS) {
        return Err(IMAGE_TOO_LARGE.to_string());
    }
    // Held from here on: every early return, error, and dropped future below releases the chat by
    // dropping this, which is why none of them needs a cleanup statement of its own.
    // A warm-up holding the chat is waited out: it lets go in moments, and nothing is answering.
    let slot = ChatSlot::acquire_past_warmup(chat_id)
        .await
        .ok_or(TURN_IN_PROGRESS.to_string())?;

    // Who answers this conversation. Two steps, in order of how specific the fact is: the chat's
    // own row, then the origin — every Telegram conversation with no row lands on the last one.
    //
    // Precedence and not a combination, because these are not the same kind of fact. A row is a
    // choice somebody made about THIS topic; the origin is a guess about the sender, and a guess
    // must not outrank a choice.
    let chosen = crate::chats::brain_of(&state.pool, chat_id)
        .await
        .map_err(|e| e.to_string())?;
    // Whether this chat's brain is `openrouter`. Resolved and acted on BEFORE `wants_local` below,
    // and unlike it, never falls through to the block that follows: the only way to reach the
    // hosted route is a `chats` row that says so directly.
    let wants_hosted = chosen == Some(crate::chats::Brain::OpenRouter);

    // This replaces the PLACEHOLDER `(None, Some(Brain::OpenRouter)) => false` arm that used to
    // live inside `wants_local`'s own match, and answers it the opposite way the placeholder's own
    // comment warned about: an `openrouter` turn is never handed to the cloud CLI. It either goes
    // to the hosted model or it is refused outright — see `NO_HOSTED_MODEL`'s doc comment for why
    // there is no fallback here the way `wants_local` below has one: falling through would put a
    // conversation that asked to stay off the CLI onto the bill, and the person would find out from
    // the invoice.
    if wants_hosted {
        // The conversation's pinned model — see `pinned_model`'s own doc for why it is read here,
        // inside the branch, and why a read failure falls back to `None` instead of refusing.
        let pinned_model = pinned_model(state, chat_id).await;
        return match state
            .assistants
            .assistant_for(crate::chats::Brain::OpenRouter, pinned_model.as_deref())
        {
            Ok(assistant) => {
                let mut slot = slot;
                slot.drain_on_release(state);
                spawn_local_turn(
                    state,
                    slot,
                    text.to_string(),
                    assistant,
                    relay_id,
                    origin,
                    "openrouter",
                )
                .await
            }
            Err(refusal) => Err(refusal.message(crate::chats::Brain::OpenRouter).to_string()),
        };
    }

    let wants_local = match chosen {
        Some(crate::chats::Brain::Local) => true,
        // `OpenRouter` is unreachable in practice: `wants_hosted` above already returned for it.
        // The arm stays only to keep the match exhaustive over `Brain`'s third variant.
        Some(crate::chats::Brain::Cloud) | Some(crate::chats::Brain::OpenRouter) => false,
        None => origin == Origin::Telegram,
    };
    // Whether local was CHOSEN or merely inferred, which is what decides the refusal below: a
    // `chats` row saying `local` is somebody's word about where this work stays.
    let local_was_chosen = chosen == Some(crate::chats::Brain::Local);

    if wants_local {
        // The conversation's pinned model — same reason and posture as the hosted call site above;
        // see `pinned_model`'s own doc.
        let pinned_model = pinned_model(state, chat_id).await;
        match state
            .assistants
            .assistant_for(crate::chats::Brain::Local, pinned_model.as_deref())
        {
            Ok(assistant) => {
                let mut slot = slot;
                slot.drain_on_release(state);
                return spawn_local_turn(
                    state,
                    slot,
                    text.to_string(),
                    assistant,
                    relay_id,
                    origin,
                    "local",
                )
                .await;
            }
            // A conversation that SAYS `local` and has no local model refuses. Falling through to
            // the cloud would be the worst possible way to find that out: on the bill, for a chat
            // that said it was staying on the machine. The refusal comes before any row is
            // inserted, so nothing was spent and nothing has to be explained away afterwards.
            Err(refusal) if local_was_chosen => {
                return Err(refusal.message(crate::chats::Brain::Local).to_string());
            }
            // The origin path keeps its old shape on purpose: a Telegram chat with no local model
            // has always simply gone to the cloud, and has never claimed otherwise. Refusing here
            // would take the bot off the air to enforce a promise nobody made.
            Err(_) => {}
        }
    }

    let resume = get_session(&state.pool, chat_id)
        .await
        .map_err(|e| e.to_string())?;
    // Where this conversation runs, and — through `tool_policy_for` — how much it may do there.
    //
    // Read for EVERY turn and not only for the elevated ones, because the working directory is also
    // how the CLI finds a session at all: it keys its transcripts by the directory they were had in,
    // so a `--resume` launched from the wrong place does not fail, it silently starts a new session.
    // That would strand a Telegram message to an IDE-rooted chat in a fresh context while the window
    // went on showing the conversation it thought it was continuing.
    let cwd = crate::chats::cwd_of(&state.pool, chat_id)
        .await
        .map_err(|e| e.to_string())?;
    // A conversation rooted in a git worktree with no workflow of its own gets one before the
    // policy below reads the hook — otherwise it would answer `McpOnly` with nothing saying why
    // (decision of 2026-10-11; `chats::seed` has the rule). A failure is logged and the turn goes
    // on as it would have: the seed is a convenience, and the policy still reads what is on disk.
    if let Some(dir) = cwd.clone() {
        let seeded = tokio::task::spawn_blocking(move || {
            crate::chats::seed::seed_if_empty(std::path::Path::new(&dir))
        })
        .await;
        match seeded {
            Ok(Ok(crate::chats::seed::Seeded::Done { copied, hook })) => {
                tracing::info!(
                    chat_id,
                    ?copied,
                    hook,
                    "seeded the workflow into the conversation's worktree"
                );
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(chat_id, %error, "could not seed the workflow into the conversation's worktree");
            }
            Err(error) => {
                tracing::warn!(chat_id, %error, "the workflow seed did not finish");
            }
        }
    }
    // Reads the conversation's settings file: off the runtime thread, and a join failure reads as
    // "not wired", which is what the sync call answered for a file it could not read.
    let hook_wired = match cwd.clone() {
        Some(dir) => tokio::task::spawn_blocking(move || {
            crate::governance::autopilot::classifier_hook_is_wired(std::path::Path::new(&dir))
        })
        .await
        .unwrap_or(false),
        None => false,
    };
    let tool_policy = tool_policy_for(
        cwd.as_deref(),
        origin,
        hook_wired,
        // Whether another conversation handed this turn over, and if so whether it may keep the
        // tools: the first hop out of a typed turn, from a sender that read nothing untrusted
        // (decision of 2026-10-11). Read here, as the turn starts, so a relay that waited in the
        // queue is judged by what its sender has read by now.
        crate::relay::handover_of(&state.pool, relay_id).await,
    );
    // The configured Telegram doctrine, resolved HERE and not inside the spawned task below,
    // because `origin` is what decides it and `origin` does not survive to that task: the task
    // reads `chats::answering` instead, which knows the chat's own instructions and nothing about
    // which door the message came in by. `None` for every other origin, so a shell turn — sitting
    // at this machine, with a person watching — never has a channel-wide doctrine pushed onto it.
    //
    // Voice sits with the shell here, and by decision rather than by omission: a spoken turn is
    // somebody at this machine talking into its microphone, which is the same presence the
    // exemption is about — the argument `tool_policy_for` makes at length. Written out rather than
    // swept up by a wildcard, so that the next origin — which will arrive over a network, as every
    // origin after the first has — is a compile error here instead of silently inheriting the
    // doctrine a Telegram channel was given.
    let doctrine = match origin {
        Origin::Telegram => state.telegram_doctrine.clone(),
        Origin::Shell | Origin::Voice | Origin::BrowserPanel => None,
    };
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().to_string();
    let config = build_mcp_config(&exe);
    let mcp_path = mcp_config_path(chat_id);
    let written = {
        let path = mcp_path.clone();
        tokio::task::spawn_blocking(move || write_mcp_config(&path, &config)).await
    };
    match written {
        Ok(result) => result.map_err(|e| e.to_string())?,
        Err(error) => return Err(error.to_string()),
    }

    // Assigned by the daemon and persisted with the row, exactly as `runs::create_run_inner` does
    // it, so an assistant turn is not the one kind of run that can exist without a session id. A
    // first turn had neither `--resume` nor `--session-id`, so its id existed only if the CLI's
    // stream happened to announce one — and `budget.rs` keys spend on `session_id`, so a turn whose
    // stream carried no `init` event was money charged against nothing. Continuing a chat keeps the
    // session being resumed rather than minting a rival id for the same conversation.
    //
    // Writing it at INSERT rather than waiting for the stream also closes the read-untrusted
    // barrier's blind spot: `get_session` refuses to resume a session any run READ mail in, and a
    // turn killed before its stream reported an id used to leave a row with no session to match on.
    let session_id = resume.clone().unwrap_or_else(crate::auth::generate_uuid_v4);
    // `chat_id` alongside the session, because they answer different questions and diverge on
    // purpose. The session is what the NEXT turn resumes, and this module drops it whenever a turn
    // read third-party text — so a conversation that has read mail once is spread across several
    // sessions, and no amount of joining on `session_id` reassembles it. The chat is the thread.
    //
    // `answered_by` alongside them, written at INSERT for the same reason `session_id` is: a turn
    // that is cancelled before it produces a word still has to say who was answering it. It is a
    // literal here rather than a parameter because everything that reaches this line is on the CLI
    // path — the branch above is where the other answer is given.
    //
    // `from_relay_id` alongside them for the reason this function's own doc comment gives: it is
    // `relay_id` unchanged, in the same statement, so a relay-born run can never exist without it.
    //
    // `origin` alongside those, and written HERE rather than derived later, because there is
    // nowhere later to derive it from: it is what the client said when it sent this message, and
    // this INSERT is the last moment anything holds that word. 0119 exists for one reader —
    // `relay::admit`, deciding whether the turn asking for a relay was a Telegram turn — and a
    // reader that has to guess is the failure that migration is fixing.
    let id = sqlx::query(
        "INSERT INTO runs (prompt, status, mode, session_id, chat_id, answered_by, from_relay_id,
                           origin, created_at)
         VALUES (?, 'running', 'assistant', ?, ?, 'cloud', ?, ?, ?)",
    )
    .bind(text)
    .bind(&session_id)
    .bind(chat_id)
    .bind(relay_id)
    .bind(origin.as_wire())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.pool)
    .await
    .map_err(|e| e.to_string())?
    .last_insert_rowid();
    crate::chats::stamp_project(&state.pool, id, chat_id).await;

    // The relay learns which turn answered it, now that the turn has an id to learn.
    //
    // After the INSERT and not inside it, because the run's id does not exist until the run does —
    // which is exactly why this pointer and `runs.from_relay_id` are not equally trusted. That one
    // is written in the same statement as the run and is what `relay::chain_of` walks; this one is
    // a second write that can fail on its own, and 0122's header says plainly that nothing
    // security-critical may read it.
    //
    // A warning and not a refusal, therefore. The turn is already under way; undoing it because an
    // audit pointer would not write would be trading the thing somebody is waiting for against the
    // record of it.
    if let Some(relay_id) = relay_id
        && let Err(error) = crate::relay::mark_delivered(&state.pool, relay_id, id).await
    {
        tracing::warn!(%error, id, relay_id, "the relayed turn started but was not stamped onto its relay");
    }

    // Written after the row exists, because the turn's own id is what names the files — which is
    // what makes two people pasting the same screenshot two different files rather than a race.
    //
    // A failure here is not a failed turn. The pictures are already in memory and on their way to
    // the model; what is lost is the window's ability to show them afterwards, which is worth a
    // warning and not worth refusing a turn somebody is waiting for.
    let kept = {
        let state = state.clone();
        let images = images.to_vec();
        tokio::task::spawn_blocking(move || keep_images(&state, id, &images))
            .await
            .unwrap_or_default()
    };
    if let Err(error) = record_images(&state.pool, id, &kept).await {
        tracing::warn!(%error, id, "the turn was sent but its pictures were not recorded");
    }

    let prompt = text.to_string();
    // The conversation so far, for a turn that has no session to hold it.
    //
    // `resume` is `None` on a first turn — where there is nothing to replay and this adds nothing —
    // and on the three cases where a session was deliberately let go of: a turn that read
    // third-party text, somebody asking for a fresh context, and a picked-up conversation too large
    // for any window a model has. Size on its own is no longer one of them; the CLI compacts inside
    // the session instead, so an ordinary long conversation never reaches this branch at all.
    //
    // What is left is genuinely a new context, and this is what stops it beginning as a stranger
    // while the transcript above it reads as one unbroken conversation.
    //
    // The same answer the LOCAL path has always given, for the same reason and through the same
    // function: no session to resume, so the exchanges are read back instead. A failure to read
    // them is not a failure of the turn — a model answering without the history is worse than one
    // answering with it, and better than one that refuses.
    let prompt = match &resume {
        Some(_) => prompt,
        None => match recent_exchanges(&state.pool, chat_id).await {
            Ok(history) if !history.is_empty() => replayed(&history, &prompt),
            // Nothing of our own to replay. On an ordinary new conversation that is the truth and
            // the prompt stands alone — but a chat picked up from the editor has a past that simply
            // is not in our runs, and beginning it blank is the complaint this feature answers.
            Ok(_) => match handed_over(&state.pool, chat_id).await {
                history if !history.is_empty() => replayed(&history, &prompt),
                _ => prompt,
            },
            Err(error) => {
                tracing::warn!(%error, chat_id, "could not read the conversation to replay it");
                prompt
            }
        },
    };
    let cwd = cwd.map(std::path::PathBuf::from);

    let mut slot = slot;
    slot.drain_on_release(state);
    spawn_assistant_turn(
        state,
        TurnLaunch {
            id,
            slot,
            text: prompt,
            images: images.to_vec(),
            resume,
            session_id,
            mcp_path,
            cwd,
            tool_policy,
            doctrine,
        },
    );
    Ok(id)
}

/// How many past exchanges a local turn is shown.
///
/// Small, and bounded again by characters below, because every one of these is re-sent on every
/// round of the tool loop — so a generous history is multiplied by `MAX_TOOL_ROUNDS` before it
/// reaches `TURN_NUM_CTX`. Six is enough for "and the second one?" and for the follow-up after that.
pub(super) const HISTORY_TURNS: i64 = 6;

/// The character budget for replayed history, counted newest-first.
///
/// A ceiling on turns alone is not a ceiling: one pasted stack trace answered by a long reply is a
/// single exchange and thousands of characters. What overflows the window is length, so length is
/// what is bounded.
pub(super) const HISTORY_CHARS: usize = 6_000;

/// The conversation so far, in front of the message that follows it.
///
/// Written as plainly as it can be, because it is read by a model that has NO memory of any of it
/// and must not mistake a replayed question for the one being asked now. The last line says which
/// is which.
///
/// `you:` and `núcleo:` rather than `user`/`assistant`: the CLI has its own idea of those roles and
/// this text is a user message, not a transcript it should adopt. Naming them after the roles would
/// invite the model to continue the transcript rather than answer the question.
///
/// The opening line used to be "This conversation has just begun a new context, so you do not
/// remember what is below", and changing it is not cosmetic. A model told it has forgotten answers
/// like someone who has forgotten — it hedges, it re-asks what was settled, it treats the replay as
/// hearsay. The person on the other side reads that as a different assistant, which is the whole of
/// the complaint this change answers. What is true is narrower and worth saying instead: this is the
/// conversation, here is the part of it that fits, carry on.
pub(super) fn replayed(history: &[(String, String)], prompt: &str) -> String {
    let mut out = String::from(
        "Here is the conversation so far, oldest first. Continue it as your own — it is yours, and \
         what is below is the part of it being carried forward:

",
    );
    for (asked, answered) in history {
        out.push_str("you: ");
        out.push_str(asked);
        out.push_str(
            "
núcleo: ",
        );
        out.push_str(answered);
        out.push_str(
            "

",
        );
    }
    out.push_str(
        "That is the replay. The new message follows.

",
    );
    out.push_str(prompt);
    out
}

/// The tail an editor session was picked up with, as exchanges, or empty.
///
/// Read from the chat rather than from the transcript: the file can be tens of megabytes and the
/// turn path must not go near it. It was taken once, at pick-up, when the file was already being
/// read to measure the session — and storing it means the compaction is a row somebody can read
/// afterwards, which is the whole of `handoff.rs`'s argument against rewriting history invisibly.
///
/// A failure to read or parse it is empty, not an error. A turn answering without its predecessor's
/// tail is worse than one answering with it, and better than one that refuses.
pub async fn handed_over(pool: &SqlitePool, chat_id: &str) -> Vec<(String, String)> {
    let stored = match crate::chats::handover_of(pool, chat_id).await {
        Ok(Some(stored)) => stored,
        Ok(None) => return Vec::new(),
        Err(error) => {
            tracing::warn!(%error, chat_id, "could not read what this conversation was handed");
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<(String, String)>>(&stored) {
        Ok(history) => history,
        Err(error) => {
            tracing::warn!(%error, chat_id, "the stored handover could not be read");
            Vec::new()
        }
    }
}

/// The exchanges a local turn may be shown, oldest first.
///
/// The `id >` clause is the same barrier `get_session` applies to the CLI path, stated for a path
/// that has no session to refuse. `hooks.rs` stops a turn acting after it has read third-party
/// text; without this, a local turn would be handed that text as history — and a local turn can
/// call `create_run`. So a conversation resumes only from the point after anything read mail, which
/// is exactly what "a chat that has read mail is spread across several sessions" already means on
/// the other path.
///
/// Only `completed` turns, and only ones with a reply: a failed turn's row has no answer, and
/// replaying a question that was never answered invites the model to answer it now, out of order.
///
/// Two floors, and the later one wins — `max` of the untrusted-read barrier above and the chat's
/// own `cleared_after_run_id` (0114). They are separate because they mean different things: one is
/// a safety property this daemon imposes, the other is somebody asking to start again, and either
/// alone must still hold when the other is absent.
pub async fn recent_exchanges(
    pool: &SqlitePool,
    chat_id: &str,
) -> sqlx::Result<Vec<(String, String)>> {
    let mut rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT prompt, stdout FROM runs
          WHERE chat_id = ?
            AND mode = 'assistant'
            AND status = 'completed'
            AND stdout IS NOT NULL
            AND stdout <> ''
            AND id > max(
                      (SELECT COALESCE(MAX(id), 0) FROM runs
                        WHERE chat_id = ? AND read_untrusted = 1),
                      COALESCE((SELECT cleared_after_run_id FROM chats
                                 WHERE chat_id = ?), 0))
          ORDER BY id DESC
          LIMIT ?",
    )
    .bind(chat_id)
    .bind(chat_id)
    .bind(chat_id)
    .bind(HISTORY_TURNS)
    .fetch_all(pool)
    .await?;

    // Trimmed newest-first, then flipped, so the exchanges that survive a tight budget are the
    // recent ones. Dropping from the other end would keep the oldest and answer a question about
    // "the second one" with the conversation from an hour ago.
    let mut budget = HISTORY_CHARS;
    rows.retain(|(asked, answered)| {
        let cost = asked.chars().count() + answered.chars().count();
        match budget.checked_sub(cost) {
            Some(left) => {
                budget = left;
                true
            }
            None => false,
        }
    });
    rows.reverse();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistants::NoAssistants;
    use crate::auth::Token;
    use crate::execution::runner::FakeCommandRunner;
    use crate::state::AppState;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    /// A chat id no other test in this process is using: slots are process-global.
    fn a_chat(prefix: &str) -> String {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
        format!(
            "{prefix}:{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    async fn test_state() -> AppState {
        AppState {
            token: Token("t".into()),
            pool: crate::testdb::fresh_pool().await,
            telegram_doctrine: None,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            assistants: Arc::new(NoAssistants),
            run_handles: Arc::new(Mutex::new(HashMap::new())),
            run_messages: Arc::new(Mutex::new(HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            files_trash: None,
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            voice: std::sync::Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: std::sync::Arc::new(crate::browser::BrowserRuntime::disabled()),
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

    /// The prompts of a chat's assistant turns, oldest first, polled until there are `at_least`.
    async fn prompts_of(pool: &SqlitePool, chat: &str, at_least: usize) -> Vec<String> {
        let mut asked = Vec::new();
        for _ in 0..200 {
            asked = sqlx::query_scalar(
                "SELECT prompt FROM runs WHERE chat_id = ? AND mode = 'assistant' ORDER BY id",
            )
            .bind(chat)
            .fetch_all(pool)
            .await
            .unwrap();
            if asked.len() >= at_least {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        asked
    }

    async fn waiting_texts(pool: &SqlitePool, chat: &str) -> Vec<String> {
        crate::chats::queued(pool, chat)
            .await
            .unwrap()
            .into_iter()
            .map(|message| message.text)
            .collect()
    }

    async fn settled(pool: &SqlitePool, id: i64) {
        for _ in 0..500 {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
            if status != "running" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("turn {id} never settled");
    }

    /// Any released slot that was armed sends what waited, in a task of its own.
    #[tokio::test]
    async fn a_released_slot_sends_what_waited_in_its_own_task() {
        let state = test_state().await;
        let chat = a_chat("released");
        crate::chats::enqueue(&state.pool, &chat, "waited for the slot", "shell", "[]")
            .await
            .unwrap();

        let mut slot = ChatSlot::acquire(&chat).expect("the chat should have been free");
        slot.drain_on_release(&state);
        assert!(is_busy(&chat));
        drop(slot);

        let asked = prompts_of(&state.pool, &chat, 1).await;
        assert_eq!(asked.len(), 1, "what waited was never sent: {asked:?}");
        assert!(asked[0].contains("waited for the slot"), "{asked:?}");
        assert!(waiting_texts(&state.pool, &chat).await.is_empty());
    }

    /// A message a person typed that the drain cannot send is not lost: it goes back to the front,
    /// the queue is held and a notice says so.
    #[tokio::test]
    async fn a_waiting_message_that_cannot_be_sent_goes_back_to_the_front() {
        let state = test_state().await;
        // A local chat on a machine with no local model: `send_message_with` refuses it.
        let chat = crate::chats::create(&state.pool, crate::chats::Brain::Local, None)
            .await
            .unwrap();
        crate::chats::enqueue(&state.pool, &chat, "first", "shell", "[]")
            .await
            .unwrap();
        crate::chats::enqueue(&state.pool, &chat, "second", "shell", "[]")
            .await
            .unwrap();
        let before: Vec<(i64, String)> = crate::chats::queued(&state.pool, &chat)
            .await
            .unwrap()
            .into_iter()
            .map(|message| (message.id, message.text))
            .collect();

        drain_queued(&state, &chat).await;

        let after: Vec<(i64, String)> = crate::chats::queued(&state.pool, &chat)
            .await
            .unwrap()
            .into_iter()
            .map(|message| (message.id, message.text))
            .collect();
        assert_eq!(
            after, before,
            "the refused message must keep its place and id"
        );
        assert!(
            queue_is_held(&chat),
            "a failing queue is held, not retried in a loop"
        );
        let notices = crate::chats::notices::for_chat(&state.pool, &chat)
            .await
            .unwrap();
        assert!(
            notices
                .iter()
                .any(|notice| notice.body.contains("could not be sent")
                    && notice.body.contains(NO_LOCAL_MODEL)),
            "{notices:?}"
        );
        release_queue(&chat);
    }

    /// A message put back on `TURN_IN_PROGRESS` after the slot's holder already released — so the
    /// holder's own drain found the queue empty — is still sent, not stranded.
    #[tokio::test]
    async fn a_put_back_whose_holder_already_released_is_still_sent() {
        let state = test_state().await;
        let chat = a_chat("put-back-race");
        assert!(!is_busy(&chat), "the holder has already released");

        put_back_refused(
            &state,
            &chat,
            (
                4242,
                "lost the race".to_string(),
                Some("shell".to_string()),
                Some("[]".to_string()),
                "2026-01-01T00:00:00Z".to_string(),
            ),
            TURN_IN_PROGRESS,
            true,
        )
        .await;

        let asked = prompts_of(&state.pool, &chat, 1).await;
        assert_eq!(
            asked.len(),
            1,
            "the restored row was never drained: {asked:?}"
        );
        assert!(asked[0].contains("lost the race"), "{asked:?}");
        assert!(waiting_texts(&state.pool, &chat).await.is_empty());
        assert!(
            !queue_is_held(&chat),
            "a lost race is not a failure: the queue is not held"
        );
    }

    /// Queues left waiting by a previous daemon are drained once at start-up, held ones excepted.
    #[tokio::test]
    async fn waiting_queues_are_drained_once_at_startup() {
        let state = test_state().await;
        let idle = a_chat("startup-idle");
        let other = a_chat("startup-other");
        let held = a_chat("startup-held");
        for (chat, text) in [
            (&idle, "idle words"),
            (&other, "other words"),
            (&held, "held words"),
        ] {
            crate::chats::enqueue(&state.pool, chat, text, "shell", "[]")
                .await
                .unwrap();
        }
        hold_queue(&held);

        drain_waiting_queues(state.clone()).await;

        let idle_asked = prompts_of(&state.pool, &idle, 1).await;
        let other_asked = prompts_of(&state.pool, &other, 1).await;
        assert!(
            idle_asked.iter().any(|p| p.contains("idle words")),
            "{idle_asked:?}"
        );
        assert!(
            other_asked.iter().any(|p| p.contains("other words")),
            "{other_asked:?}"
        );
        assert_eq!(
            waiting_texts(&state.pool, &held).await,
            vec!["held words".to_string()]
        );
        release_queue(&held);
    }

    /// A new message does not jump over the ones already waiting, even when the chat is free.
    #[tokio::test]
    async fn a_new_message_waits_behind_what_is_already_waiting() {
        let state = test_state().await;
        let chat = a_chat("behind");
        crate::chats::enqueue(&state.pool, &chat, "older", "shell", "[]")
            .await
            .unwrap();

        let outcome = send_or_queue(&state, &chat, "newer", &[], Origin::Shell).await;

        assert_eq!(outcome, Ok(Sent::Queued));
        let asked = prompts_of(&state.pool, &chat, 1).await;
        assert!(
            asked[0].contains("older"),
            "the oldest message goes first: {asked:?}"
        );
    }

    /// After Stop the queue stays put until the person sends again.
    #[tokio::test]
    async fn after_a_stop_what_waited_stays_waiting() {
        let fake = Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("stopped");
        // A conversation with no directory of its own keeps no process, so Stop takes the kill path.
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(&chat)
            .bind("2026-01-01T00:00:00Z")
            .execute(&state.pool)
            .await
            .unwrap();
        *fake.delay.lock().unwrap() = Some(Duration::from_secs(30));
        let turn = send_message(&state, &chat, "slow one", Origin::Shell)
            .await
            .unwrap();
        for _ in 0..300 {
            if state.run_handles.lock().unwrap().contains_key(&turn) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let outcome = send_or_queue(&state, &chat, "behind the slow one", &[], Origin::Shell).await;
        assert_eq!(outcome, Ok(Sent::Queued));

        let stopped = stop_turn_within(&state, turn, Duration::from_millis(50)).await;

        assert_eq!(stopped, Stopped::Killed);
        settled(&state.pool, turn).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            waiting_texts(&state.pool, &chat).await,
            vec!["behind the slow one".to_string()],
            "Stop must not start what waited"
        );
        assert_eq!(prompts_of(&state.pool, &chat, 1).await.len(), 1);
        assert!(queue_is_held(&chat));
        release_queue(&chat);
    }

    /// Stop on a turn that already ended answers NotRunning and aborts whatever shares its id.
    #[tokio::test]
    async fn stopping_a_turn_that_already_ended_aborts_nothing() {
        let state = test_state().await;
        let chat = a_chat("ended");
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('done', 'completed', 'assistant', ?, '2026-08-09T00:00:00Z') RETURNING id",
        )
        .bind(&chat)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let task = tokio::spawn(tokio::time::sleep(Duration::from_secs(60)));
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(id, task.abort_handle());

        let stopped = stop_turn_within(&state, id, Duration::from_millis(50)).await;

        assert_eq!(stopped, Stopped::NotRunning);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.is_finished(), "the task of an ended turn was aborted");
        assert!(
            state.run_handles.lock().unwrap().contains_key(&id),
            "the handle of an ended turn was taken"
        );
        task.abort();
    }

    /// A lock a panic poisoned is taken anyway: the data is still there and the next caller needs it.
    #[test]
    fn a_poisoned_lock_is_recovered_rather_than_panicked_on() {
        let lock = Arc::new(Mutex::new(vec![1, 2]));
        let poisoner = lock.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poisoning the lock on purpose");
        })
        .join();
        assert!(lock.is_poisoned());

        relock(&lock).push(3);

        assert_eq!(*relock(&lock), vec![1, 2, 3]);
    }

    /// Pictures are written off the runtime thread and still land where the window can ask for them.
    #[tokio::test]
    async fn pictures_written_off_the_runtime_are_still_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(dir.path()).unwrap();
        let mut state = test_state().await;
        state.files_root = Some(root.clone());
        let images = vec![crate::execution::runner::Attachment {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }];

        let id = send_message_with(&state, &a_chat("pictures"), "look", &images, Origin::Shell)
            .await
            .unwrap();

        let stored: Option<String> =
            sqlx::query_scalar("SELECT prompt_images FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let paths: Vec<String> =
            serde_json::from_str(&stored.expect("the turn recorded no pictures")).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(std::fs::read(root.join(&paths[0])).unwrap(), b"hello");
    }

    /// A send that arrives while the chat's process is being warmed waits the warm-up out and starts
    /// its turn: nothing was answering, so it is not queued behind a phantom turn, and the chat never
    /// read as busy (which is what made an archive answer 409 in that window).
    #[tokio::test]
    async fn a_send_during_a_prewarm_is_not_reported_as_queued() {
        let state = test_state().await;
        let chat = a_chat("warming");
        let warm = ChatSlot::acquire_for_warmup(&chat).expect("the chat starts free");
        assert!(
            !is_busy(&chat),
            "a warm-up read as a turn in flight, so an archive would be refused"
        );
        assert!(
            ChatSlot::acquire(&chat).is_none(),
            "a warm-up must still keep a turn from starting beside it"
        );
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(warm);
        });

        let outcome = send_or_queue(&state, &chat, "hello", &[], Origin::Shell).await;
        release.await.unwrap();

        assert!(
            matches!(outcome, Ok(Sent::Turn(_))),
            "a send during a warm-up was answered {outcome:?}"
        );
        assert!(
            waiting_texts(&state.pool, &chat).await.is_empty(),
            "the message was queued behind the warm-up"
        );
    }
}
