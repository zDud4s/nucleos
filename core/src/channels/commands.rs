//! Chat commands (flow D, Anexo D D.8): the port of `shortcuts/router.go`. P0 holds the `Intent`
//! type; A2 owns the real routing and `execute`.
//!
//! `execute`, `answer_capture` and `capture_target` take `ingest::Inbound` and `contract::InMessage`
//! as annex D says. In A1 the placeholder `route` only ever yields `Pair` and `Chat`, and
//! `capture_target` finds nothing, so `execute` and `answer_capture` are unreachable until A2.

use super::strings::{self, Text};
use super::{access, contract, ingest, media, outbox};
use crate::state::AppState;
use crate::tree_job::decisions::{self, Decision, DecisionError};

/// The one channel whose owner may answer a tree decision (D7): e-mail and Fleet do not approve.
const DECISION_CHANNEL: &str = "telegram";

/// What a message asks for. Port of `shortcuts.Intent` plus the channel-only commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Help,
    Kill {
        on: bool,
    },
    Cancel,
    Budget,
    Proposals,
    Proj {
        filter: Option<String>,
    },
    Mail,
    Inbox,
    /// Empty = bare `/note`, answered with the usage line.
    Note(String),
    /// Recognised by `route`, but handled by `ingest` via `access::pair` before `admit`.
    Pair(String),
    Cap {
        job: i64,
        text: String,
    },
    /// `approve N` / `reject N`, and the degraded form of buttons.
    Approve(i64),
    Reject(i64),
    /// An answer to a tree decision. `decision` is the one `/decide <id> <text>` named, and is
    /// `None` for a plain text (`pending_answer`), which `execute` weighs before applying it.
    DecisionAnswer {
        decision: Option<String>,
        text: String,
    },
    /// A command heard in a voice note: said back, never run.
    Refused(String),
    /// Not a command: goes to the turn.
    Chat(String),
}

/// Typed text. Pure. Commands match case-insensitively and in English only; their argument keeps
/// its own case. What matches nothing is `Chat` with the text unchanged.
pub fn route(text: &str) -> Intent {
    let trimmed = text.trim();
    let lowered = trimmed.to_lowercase();

    match lowered.as_str() {
        "/start" | "/help" => return Intent::Help,
        "/kill on" => return Intent::Kill { on: true },
        "/kill off" => return Intent::Kill { on: false },
        "/cancel" => return Intent::Cancel,
        "/budget" => return Intent::Budget,
        "/proposals" => return Intent::Proposals,
        "/proj" | "/projects" => return Intent::Proj { filter: None },
        "/mail" => return Intent::Mail,
        "/inbox" => return Intent::Inbox,
        _ => {}
    }

    // Prefix checks run against `trimmed`, never `lowered`: lowercasing can change byte length.
    if has_prefix_fold(trimmed, "/kill") {
        return Intent::Help;
    }
    if let Some(arg) = note_command(trimmed) {
        return Intent::Note(arg);
    }
    if has_prefix_fold(trimmed, "/proj ") {
        let arg = trimmed["/proj ".len()..].trim();
        return Intent::Proj {
            filter: Some(arg.to_string()),
        };
    }
    if let Some(code) = word_argument(trimmed, "/pair")
        && !code.is_empty()
    {
        return Intent::Pair(code.to_string());
    }
    if let Some(rest) = word_argument(trimmed, "/cap") {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let job = parts.next().and_then(|n| n.parse::<i64>().ok());
        let body = parts.next().map(str::trim).unwrap_or("");
        return match job {
            Some(job) if !body.is_empty() => Intent::Cap {
                job,
                text: body.to_string(),
            },
            // A malformed `/cap` is answered with the usage, like a malformed `/kill`.
            _ => Intent::Help,
        };
    }
    if let Some(rest) = word_argument(trimmed, "/decide") {
        let mut parts = rest.splitn(2, char::is_whitespace);
        let id = parts.next().unwrap_or("");
        let body = parts.next().map(str::trim).unwrap_or("");
        // A malformed `/decide` is answered with the usage, like a malformed `/cap`.
        if id.is_empty() || body.is_empty() {
            return Intent::Help;
        }
        return Intent::DecisionAnswer {
            decision: Some(id.to_string()),
            text: body.to_string(),
        };
    }
    if let Some(intent) = approval(&lowered) {
        return intent;
    }

    Intent::Chat(text.to_string())
}

/// Transcribed text. A transcript is a guess over a noisy channel with no confirmation step, so
/// one that lands on a command is refused out loud (`Refused` carries the text as heard); anything
/// else is what voice notes are for and goes to the turn unchanged.
pub fn route_transcript(text: &str) -> Intent {
    match route(text) {
        Intent::Chat(_) => Intent::Chat(text.to_string()),
        _ => Intent::Refused(text.to_string()),
    }
}

/// `approve N` / `reject N`, already lowercased. Exactly two words, the second a whole number.
fn approval(lowered: &str) -> Option<Intent> {
    let mut words = lowered.split_whitespace();
    let verb = words.next()?;
    let n = words.next()?.parse::<i64>().ok()?;
    if words.next().is_some() {
        return None;
    }
    match verb {
        "approve" => Some(Intent::Approve(n)),
        "reject" => Some(Intent::Reject(n)),
        _ => None,
    }
}

/// `command` alone or followed by whitespace (case-insensitive): the trimmed rest, else `None`.
fn word_argument<'a>(trimmed: &'a str, command: &str) -> Option<&'a str> {
    if !has_prefix_fold(trimmed, command) {
        return None;
    }
    let rest = &trimmed[command.len()..];
    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
        Some(rest.trim())
    } else {
        None
    }
}

/// Reads `/note` and `/note@SomeBot`, alone or followed by any whitespace (a newline included).
/// What it misses falls through as chat, and a note is the one text that must never go there.
fn note_command(trimmed: &str) -> Option<String> {
    const COMMAND: &str = "/note";
    if !has_prefix_fold(trimmed, COMMAND) {
        return None;
    }
    let mut rest = &trimmed[COMMAND.len()..];
    if rest.starts_with('@') {
        match rest.find(char::is_whitespace) {
            None => return Some(String::new()),
            Some(end) => rest = &rest[end..],
        }
    }
    if rest.is_empty() {
        return Some(String::new());
    }
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim().to_string())
}

fn has_prefix_fold(s: &str, prefix: &str) -> bool {
    s.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Runs a command and enqueues its answer in the same conversation, through service functions only
/// (no HTTP). C2a covers help, budget, proposals, projects and a refused voice command; the rest
/// arrive with C2b (mail, inbox, note) and C3 (kill, cancel, approve, reject), and C4 (cap).
pub async fn execute(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    intent: Intent,
) -> sqlx::Result<()> {
    let now = chrono::Utc::now();
    let reply = match intent {
        Intent::Help => strings::render(&Text::Help),
        Intent::Budget => {
            let snapshot = crate::governance::budget::snapshot(&state.pool, now).await?;
            strings::render(&Text::Budget(snapshot))
        }
        Intent::Proposals => {
            let pending = crate::proposals::list_pending(&state.pool).await?;
            strings::render_proposals(&pending)
        }
        Intent::Proj { filter } => {
            let roster = crate::governance::autopilot::project_roster(&state.pool).await?;
            strings::render_projects(&roster, filter.as_deref())
        }
        Intent::Refused(text) => strings::render(&Text::HeardCommand { text }),
        Intent::Pair(_) => {
            // `ingest` runs `/pair` through `access::pair` before it reaches here.
            tracing::error!("a pairing command reached commands::execute");
            return Ok(());
        }
        Intent::Mail => run_mail(state, now).await,
        Intent::Inbox => {
            let queue = crate::integrations::email::queue(&state.pool, INBOX_LIMIT).await?;
            strings::render_inbox(&queue)
        }
        Intent::Note(text) => run_note(state, at, &text).await,
        Intent::Kill { on } => {
            crate::governance::autopilot::set_kill_switch(&state.pool, on).await?;
            strings::render(&if on {
                Text::KillEngaged
            } else {
                Text::KillDisengaged
            })
        }
        // The only answer when nothing runs is `NothingToCancel`: no pause notice and no touch on
        // the queue, so a queue that was held stays held and a free one stays free. When a turn is
        // stopped, `stop_chat` owns the single pause notice; the reply here never repeats it.
        Intent::Cancel => match crate::assistant::stop_chat(state, &at.key.chat_id()).await {
            None => strings::render(&Text::NothingToCancel),
            Some((turn, _)) => strings::render(&Text::Cancelled { turn }),
        },
        Intent::Approve(id) => {
            let decided =
                crate::proposals::approve(state, id, crate::proposals::ApproveInput::default())
                    .await
                    .map(|_| ());
            decision_reply(decided, Text::Approved { id })
        }
        Intent::Reject(id) => {
            let decided = crate::proposals::reject(state, id).await;
            decision_reply(decided, Text::Rejected { id })
        }
        Intent::Cap { job, text } => run_capture(at, state, job, &text).await,
        Intent::DecisionAnswer { decision, text } => {
            match run_decision_answer(state, at, decision, &text).await? {
                Some(reply) => reply,
                None => return Ok(()),
            }
        }
        Intent::Chat(_) => {
            // A plain message goes to the turn in `ingest`, never through `execute`.
            tracing::error!("a plain message reached commands::execute");
            return Ok(());
        }
    };
    say(state, &at.key, reply, now).await
}

/// How many queued mails `/inbox` reads: the same bound the HTTP door uses.
const INBOX_LIMIT: i64 = 200;

/// `/mail`: triage what is waiting. An unarmed pillar is an answer of its own.
async fn run_mail(state: &AppState, now: chrono::DateTime<chrono::Utc>) -> String {
    if !state.email.armed.load(std::sync::atomic::Ordering::Relaxed) {
        return strings::render(&Text::MailNotArmed);
    }
    match crate::governance::triage::triage_on_request(state, now).await {
        Ok(outcome) => strings::render_triage(&outcome),
        Err(crate::governance::triage::TriageError(status)) => strings::render(&Text::NotSent {
            refusal: format!("mail triage failed ({status})"),
        }),
    }
}

/// `/note <text>`: files the text as a note whose origin is this channel. The text never reaches a
/// log line or an error reply.
async fn run_note(state: &AppState, at: &ingest::Inbound<'_>, text: &str) -> String {
    if text.trim().is_empty() {
        return strings::render(&Text::NoteUsage);
    }
    let origin = crate::assistant::Origin::Channel(at.channel).as_wire();
    match crate::owner_notes::create(&state.pool, text, origin).await {
        Ok(id) => strings::render(&Text::Noted { id }),
        Err(error) => {
            tracing::warn!(channel = at.channel, %error, "a channel note could not be saved");
            strings::render(&Text::NoteFailed)
        }
    }
}

/// How a plain text reads as the answer to a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerClass {
    /// A bare yes or no, with the text as sent (trimmed).
    Clear(String),
    Ambiguous,
}

/// Only a bare yes or no is clear, whatever its case and with a closing `.` or `!`. Anything longer
/// may qualify the answer, so it is asked about rather than applied.
pub fn classify_answer(text: &str) -> AnswerClass {
    const CLEAR: [&str; 8] = ["sim", "yes", "ok", "aprovo", "não", "nao", "no", "rejeito"];
    let sent = text.trim();
    let bare = sent.trim_end_matches(['.', '!']).to_lowercase();
    if CLEAR.contains(&bare.as_str()) {
        AnswerClass::Clear(sent.to_string())
    } else {
        AnswerClass::Ambiguous
    }
}

/// The intent of a typed text that `route` leaves as chat while a decision waits for its owner, and
/// `None` when it stays chat. Whether the text is clear is for `execute` to weigh.
pub async fn pending_answer(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    text: &str,
) -> sqlx::Result<Option<Intent>> {
    if at.channel != DECISION_CHANNEL || !matches!(route(text), Intent::Chat(_)) {
        return Ok(None);
    }
    if open_decisions(state).await?.is_empty() {
        return Ok(None);
    }
    if let access::Verdict::Drop(_) =
        access::admit(state, at.channel, at.conversation, at.sender).await?
    {
        return Ok(None);
    }
    Ok(Some(Intent::DecisionAnswer {
        decision: None,
        text: text.to_string(),
    }))
}

/// `open` can only fail on the database, which is all `sqlx::Result` has room for.
async fn open_decisions(state: &AppState) -> sqlx::Result<Vec<Decision>> {
    decisions::open(&state.pool)
        .await
        .map_err(|error| match error {
            DecisionError::Db(error) => error,
            other => sqlx::Error::Protocol(other.to_string()),
        })
}

/// The reply to an answer to a decision, or `None` for a sender `admit` refuses, who gets no reply
/// at all. It re-checks the sender itself: this is the door that resumes a branch.
async fn run_decision_answer(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    decision: Option<String>,
    text: &str,
) -> sqlx::Result<Option<String>> {
    if let access::Verdict::Drop(_) =
        access::admit(state, at.channel, at.conversation, at.sender).await?
    {
        return Ok(None);
    }
    if at.channel != DECISION_CHANNEL {
        return Ok(Some(strings::render(&Text::DecisionWrongChannel)));
    }
    let reply = match decision {
        // Naming the decision is what makes the text unambiguous, whatever it says.
        Some(id) => apply_decision(state, at, &id, text).await?,
        None => {
            let open = open_decisions(state).await?;
            match (classify_answer(text), open.as_slice()) {
                (_, []) => strings::render(&Text::NoOpenDecision),
                (AnswerClass::Clear(clear), [only]) => {
                    apply_decision(state, at, &only.id, &clear).await?
                }
                (_, waiting) => strings::render(&Text::DecisionConfirm {
                    open: waiting
                        .iter()
                        .map(|decision| (decision.id.clone(), decision.question.clone()))
                        .collect(),
                }),
            }
        }
    };
    Ok(Some(reply))
}

/// Answers decision `id` with this channel as the origin, and says what happened. A database failure
/// goes up, so that the event stays `processing` and its redelivery runs again.
async fn apply_decision(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    id: &str,
    text: &str,
) -> sqlx::Result<String> {
    // Not `decisions::answer` alone: an answer that resumes a branch also starts its node again.
    let said = match crate::tree_job::open::answer_and_relaunch(state, id, text, at.channel).await {
        Ok(answered) => Text::DecisionApplied {
            question: answered.decision.question,
            answer: text.to_string(),
        },
        Err(DecisionError::NotFound(_)) => Text::DecisionUnknown { id: id.to_string() },
        Err(DecisionError::AlreadyDecided(_)) => Text::AlreadyHandled,
        Err(DecisionError::Db(error)) => return Err(error),
        Err(error) => {
            tracing::warn!(
                channel = at.channel,
                decision = id,
                %error,
                "a decision did not take its answer"
            );
            Text::NotSent {
                refusal: format!("decision {id} could not take the answer ({error})"),
            }
        }
    };
    Ok(strings::render(&said))
}

/// The reply to an approve or reject: `done` when it went through, "already handled" when somebody
/// decided first or the proposal is gone, the refusal sentence otherwise.
fn decision_reply(result: Result<(), crate::proposals::DecideError>, done: Text) -> String {
    use crate::proposals::DecideError;
    match result {
        Ok(()) => strings::render(&done),
        Err(DecideError::AlreadyDecided | DecideError::NotFound) => {
            strings::render(&Text::AlreadyHandled)
        }
        Err(error) => {
            let refusal = match error {
                DecideError::Conflict(message)
                | DecideError::Invalid(message)
                | DecideError::BadRequest(message)
                | DecideError::BadGateway(message)
                | DecideError::Status(_, message)
                    if !message.is_empty() =>
                {
                    message
                }
                _ => "the decision could not be recorded".to_string(),
            };
            strings::render(&Text::NotSent { refusal })
        }
    }
}

/// Queues a plain text answer in the conversation the command came from and wakes its channel.
async fn say(
    state: &AppState,
    key: &contract::ConversationKey,
    text: String,
    now: chrono::DateTime<chrono::Utc>,
) -> sqlx::Result<()> {
    let action = outbox::NewAction::Send {
        reply_to: None,
        parts: vec![contract::OutPart::Text { text }],
        buttons: None,
        silent: false,
    };
    let mut conn = state.pool.acquire().await?;
    outbox::enqueue(&mut conn, key, action, outbox::Class::Command, None, now).await?;
    drop(conn);
    outbox::wake(state, key.channel);
    Ok(())
}

/// The message's text when every part is typed text; `None` as soon as one part is not.
fn typed_text(message: &contract::InMessage) -> Option<String> {
    let mut lines = Vec::new();
    for part in &message.parts {
        match part {
            contract::InPart::Text { text } => lines.push(text.as_str()),
            _ => return None,
        }
    }
    Some(lines.join("\n"))
}

/// Which capture request a message answers.
#[derive(Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    Job(i64),
    /// A reply to a digest with several questions open: the owner must pick one.
    Ambiguous(Vec<i64>),
}

/// Which capture request a message answers, if any: a reply to a capture request or a digest this very
/// conversation was sent, else the typed `/cap <N> <text>` form. The "open captures" hint of the
/// Telegram v1 is left out on purpose (Q14).
pub async fn capture_target(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    message: &contract::InMessage,
) -> sqlx::Result<Option<CaptureTarget>> {
    if let Some(reply_to) = message.reply_to.as_deref() {
        let sent = outbox::find_by_platform_ref(&state.pool, &at.key, reply_to).await?;
        match sent.and_then(|sent| sent.subject) {
            Some(outbox::Subject::Capture(job)) => return Ok(Some(CaptureTarget::Job(job))),
            Some(outbox::Subject::CaptureDigest(id)) => {
                let open: Vec<i64> = sqlx::query_scalar(
                    "SELECT job_id FROM capture_requests WHERE digest_id = ? AND state = 'open' ORDER BY job_id",
                )
                .bind(id)
                .fetch_all(&state.pool)
                .await?;
                return Ok(match open.as_slice() {
                    [] => None,
                    [one] => Some(CaptureTarget::Job(*one)),
                    _ => Some(CaptureTarget::Ambiguous(open)),
                });
            }
            _ => {}
        }
    }
    Ok(match typed_text(message).map(|text| route(&text)) {
        Some(Intent::Cap { job, .. }) => Some(CaptureTarget::Job(job)),
        _ => None,
    })
}

/// A reply to a digest with several questions open: say which, answer nothing.
pub async fn say_digest_hint(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    jobs: Vec<i64>,
) -> sqlx::Result<()> {
    say(
        state,
        &at.key,
        strings::render(&Text::CaptureDigestHint { jobs }),
        chrono::Utc::now(),
    )
    .await
}

/// Files `text` as the answer to capture request `job`, with this channel as the note's origin, and
/// says what happened.
async fn run_capture(at: &ingest::Inbound<'_>, state: &AppState, job: i64, text: &str) -> String {
    let origin = crate::assistant::Origin::Channel(at.channel).as_wire();
    match crate::capture::answer(&state.pool, job, text, origin).await {
        Ok((_, true)) => strings::render(&Text::CaptureAnswered { job }),
        Ok((note, false)) => strings::render(&Text::CaptureLate { note }),
        Err(error) => {
            tracing::warn!(channel = at.channel, job, %error, "a capture answer was not recorded");
            strings::render(&Text::NotSent {
                refusal: format!("capture request #{job} could not take the answer ({error})"),
            })
        }
    }
}

/// Answers a capture with typed text only: voice, images and files are refused. In the `/cap <N>
/// <text>` form the answer is the text after the job number.
pub async fn answer_capture(
    state: &AppState,
    at: &ingest::Inbound<'_>,
    job: i64,
    input: &media::TurnInput,
) -> sqlx::Result<()> {
    let now = chrono::Utc::now();
    if input.non_text || input.transcribed || input.text.trim().is_empty() {
        return say(
            state,
            &at.key,
            strings::render(&Text::CaptureNeedsText),
            now,
        )
        .await;
    }
    let text = match route(&input.text) {
        Intent::Cap { job: named, text } if named == job => text,
        _ => input.text.clone(),
    };
    let reply = run_capture(at, state, job, &text).await;
    say(state, &at.key, reply, now).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(s: &str) -> Intent {
        Intent::Chat(s.to_string())
    }

    #[test]
    fn route_recognises_only_pair() {
        assert_eq!(route("/pair 123456"), Intent::Pair("123456".to_string()));
        assert_eq!(route("/pair"), chat("/pair"));
        assert_eq!(route("/pairing x"), chat("/pairing x"));
        assert_eq!(route("hello"), chat("hello"));
        assert_eq!(
            route_transcript("/pair 1"),
            Intent::Refused("/pair 1".to_string())
        );
    }

    #[test]
    fn route_help_and_slash_forms() {
        assert_eq!(route("/start"), Intent::Help);
        assert_eq!(route("  /help  "), Intent::Help);
        assert_eq!(route("/HELP"), Intent::Help);
        assert_eq!(route("/kill on"), Intent::Kill { on: true });
        assert_eq!(route("/KILL on"), Intent::Kill { on: true });
        assert_eq!(route("/Kill Off"), Intent::Kill { on: false });
        assert_eq!(route("/kill"), Intent::Help);
        assert_eq!(route("/kill maybe"), Intent::Help);
        assert_eq!(route("/Cancel"), Intent::Cancel);
        assert_eq!(route("/budget"), Intent::Budget);
        assert_eq!(route("/Proposals"), Intent::Proposals);
        assert_eq!(route("/proj"), Intent::Proj { filter: None });
        assert_eq!(route("/PROJECTS"), Intent::Proj { filter: None });
        assert_eq!(
            route("/proj nucleos"),
            Intent::Proj {
                filter: Some("nucleos".to_string())
            }
        );
        assert_eq!(
            route("/Proj NucleOS"),
            Intent::Proj {
                filter: Some("NucleOS".to_string())
            }
        );
        assert_eq!(route("/Mail"), Intent::Mail);
        assert_eq!(route("/INBOX"), Intent::Inbox);
        assert_eq!(route("/unknown"), chat("/unknown"));
        for former in ["/assunto carros", "/assuntos", "/pausa", "/retomar", "/fim"] {
            assert_eq!(route(former), chat(former));
        }
    }

    #[test]
    fn route_approve_and_reject_commands() {
        assert_eq!(route("approve 12"), Intent::Approve(12));
        assert_eq!(route("  Approve   7 "), Intent::Approve(7));
        assert_eq!(route("reject 3"), Intent::Reject(3));
        assert_eq!(route("REJECT 3"), Intent::Reject(3));
        assert_eq!(route("approve"), chat("approve"));
        assert_eq!(route("approve x"), chat("approve x"));
        assert_eq!(route("approve 1 2"), chat("approve 1 2"));
        assert_eq!(route("please approve 12"), chat("please approve 12"));
    }

    #[test]
    fn route_portuguese_approve_and_reject_are_plain_chat() {
        for text in ["aprovar 12", "rejeitar 12", "aprova 12", "rejeita 12"] {
            assert_eq!(route(text), chat(text));
        }
    }

    #[test]
    fn route_cap_note_and_pair() {
        assert_eq!(
            route("/cap 4 yes, go ahead"),
            Intent::Cap {
                job: 4,
                text: "yes, go ahead".to_string()
            }
        );
        assert_eq!(
            route("/CAP 4\nline one\nline two"),
            Intent::Cap {
                job: 4,
                text: "line one\nline two".to_string()
            }
        );
        assert_eq!(route("/cap"), Intent::Help);
        assert_eq!(route("/cap x hi"), Intent::Help);
        assert_eq!(route("/cap 4"), Intent::Help);
        assert_eq!(route("/pair 123456"), Intent::Pair("123456".to_string()));
        assert_eq!(route("/PAIR  ab12 "), Intent::Pair("ab12".to_string()));

        let note = |s: &str| Intent::Note(s.to_string());
        assert_eq!(route("/note buy milk"), note("buy milk"));
        assert_eq!(route("/NOTE  Buy Milk  "), note("Buy Milk"));
        assert_eq!(route("/Note a /kill b"), note("a /kill b"));
        assert_eq!(route("/note"), note(""));
        assert_eq!(route("  /Note  "), note(""));
        assert_eq!(
            route("/note\nline one\nline two"),
            note("line one\nline two")
        );
        assert_eq!(route("/note\tbuy milk"), note("buy milk"));
        assert_eq!(route("/note@NucleosBot buy milk"), note("buy milk"));
        assert_eq!(route("/note@NucleosBot\nbuy milk"), note("buy milk"));
        assert_eq!(route("/note@NucleosBot"), note(""));
        assert_eq!(route("/notes for the agent"), chat("/notes for the agent"));
    }

    #[test]
    fn route_transcript_never_a_command() {
        for text in [
            "/kill off",
            "/KILL OFF",
            "/note buy milk",
            "approve 12",
            "/cap 1 yes",
        ] {
            assert_eq!(route_transcript(text), Intent::Refused(text.to_string()));
        }
        assert_eq!(
            route_transcript("cancel my last turn please"),
            chat("cancel my last turn please")
        );
    }

    #[test]
    fn route_plain_text_is_chat() {
        assert_eq!(route("  ask the agent  "), chat("  ask the agent  "));
        assert_eq!(route(""), chat(""));
        assert_eq!(route("héllo wörld"), chat("héllo wörld"));
        assert_eq!(route("ß"), chat("ß"));
    }

    fn conversation() -> contract::Conversation {
        contract::Conversation {
            reference: "c1".to_string(),
            kind: contract::ConversationKind::Private,
            parent: None,
        }
    }

    fn sender() -> contract::Sender {
        contract::Sender {
            reference: "u1".to_string(),
            name: None,
        }
    }

    /// Runs `intent` as if it came from conversation `fake:c1`.
    async fn run(state: &AppState, intent: Intent) {
        let (conversation, sender) = (conversation(), sender());
        let at = ingest::Inbound {
            channel: "fake",
            key: contract::ConversationKey::new("fake", "c1"),
            conversation: &conversation,
            sender: &sender,
            message_ref: "m1",
        };
        execute(state, &at, intent).await.unwrap();
    }

    /// Every queued answer as (conversation, class, text), oldest first.
    async fn queued(state: &AppState) -> Vec<(String, String, String)> {
        let rows: Vec<(String, String, String)> =
            sqlx::query_as("SELECT conversation, class, payload FROM channel_outbox ORDER BY id")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(conversation, class, payload)| {
                let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
                let text = payload["parts"][0]["text"].as_str().unwrap().to_string();
                (conversation, class, text)
            })
            .collect()
    }

    async fn the_one_answer(state: &AppState) -> String {
        let mut all = queued(state).await;
        assert_eq!(all.len(), 1, "{all:?}");
        let (conversation, class, text) = all.remove(0);
        assert_eq!(conversation, "c1");
        assert_eq!(class, "command");
        text
    }

    #[tokio::test]
    async fn execute_help_enqueues_the_help_text() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Help).await;
        assert_eq!(the_one_answer(&state).await, strings::render(&Text::Help));
    }

    #[tokio::test]
    async fn execute_budget_enqueues_the_snapshot() {
        let state = crate::channels::testkit::state().await;
        let snapshot = crate::governance::budget::snapshot(&state.pool, chrono::Utc::now())
            .await
            .unwrap();
        run(&state, Intent::Budget).await;
        let answer = the_one_answer(&state).await;
        assert_eq!(answer, strings::render(&Text::Budget(snapshot)));
        assert!(answer.starts_with("Budget ("), "{answer}");
    }

    #[tokio::test]
    async fn execute_proposals_empty_says_none() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Proposals).await;
        assert_eq!(the_one_answer(&state).await, "No open proposals.");
    }

    #[tokio::test]
    async fn execute_proposals_lists_the_pending_ones() {
        let state = crate::channels::testkit::state().await;
        // `list_pending` is the approvals queue: only `action-approval` rows count, so a
        // contact-merge proposal would (rightly) not be listed.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO proposals
             (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
             VALUES ('action-approval', 'pending', NULL, NULL, NULL, 'Bash', 'same person', NULL, ?, NULL)
             RETURNING id",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        run(&state, Intent::Proposals).await;
        let answer = the_one_answer(&state).await;
        assert!(
            answer.contains(&format!("proposal {id} (action-approval): Bash")),
            "{answer}"
        );
        assert!(answer.contains("same person"), "{answer}");
    }

    #[tokio::test]
    async fn execute_proj_filter_applies() {
        let state = crate::channels::testkit::state().await;
        for project in ["alpha-app", "beta-tool"] {
            sqlx::query("INSERT INTO autopilot_state (project_id, mode) VALUES (?, 'off')")
                .bind(project)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        run(
            &state,
            Intent::Proj {
                filter: Some("ALPHA".to_string()),
            },
        )
        .await;
        let answer = the_one_answer(&state).await;
        assert!(answer.contains("alpha-app"), "{answer}");
        assert!(!answer.contains("beta-tool"), "{answer}");
    }

    #[tokio::test]
    async fn execute_refused_enqueues_the_heard_command_text() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Refused("kill off".to_string())).await;
        assert_eq!(
            the_one_answer(&state).await,
            strings::render(&Text::HeardCommand {
                text: "kill off".to_string()
            })
        );
    }

    #[tokio::test]
    async fn execute_mail_not_armed_says_so() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Mail).await;
        assert_eq!(the_one_answer(&state).await, "Mail is not armed.");
    }

    #[tokio::test]
    async fn execute_inbox_lists_queued_mail() {
        let state = crate::channels::testkit::state().await;
        sqlx::query(
            "INSERT INTO emails (message_id, mailbox, uidvalidity, uid, from_addr, subject,
                                 received_at, ingested_at, direction)
             VALUES ('<i1@x>', 'INBOX', 1, 1, 'rita@example.com', 'July invoice',
                     '2026-07-28T09:00:00Z', '2026-07-28T09:00:00Z', 'inbound')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        run(&state, Intent::Inbox).await;
        let answer = the_one_answer(&state).await;
        assert!(answer.contains("rita@example.com"), "{answer}");
        assert!(answer.contains("July invoice"), "{answer}");
    }

    #[tokio::test]
    async fn execute_note_creates_with_channel_origin() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Note("buy milk".to_string())).await;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT note_text, origin FROM owner_notes")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![("buy milk".to_string(), "channel:fake".to_string())]
        );
        let answer = the_one_answer(&state).await;
        assert!(answer.starts_with("Noted (#"), "{answer}");
    }

    #[tokio::test]
    async fn execute_note_bare_answers_usage() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Note(String::new())).await;
        assert_eq!(the_one_answer(&state).await, "Usage: /note <text>");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM owner_notes")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn execute_kill_on_off_sets_switch_and_answers() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Kill { on: true }).await;
        assert!(
            crate::governance::autopilot::kill_switch_engaged(&state.pool)
                .await
                .unwrap()
        );
        run(&state, Intent::Kill { on: false }).await;
        assert!(
            !crate::governance::autopilot::kill_switch_engaged(&state.pool)
                .await
                .unwrap()
        );
        let texts: Vec<String> = queued(&state).await.into_iter().map(|q| q.2).collect();
        assert_eq!(texts, vec!["Kill switch ENGAGED", "Kill switch DISENGAGED"]);
    }

    #[tokio::test]
    async fn execute_cancel_nothing_running_leaves_the_queue_hold_as_it_was() {
        let state = crate::channels::testkit::state().await;
        run(&state, Intent::Cancel).await;
        // The only message is the exact text, with no pause notice beside it. The hold itself is
        // private to the assistant module, where the `stop_chat` test asserts it stays as it was.
        assert_eq!(the_one_answer(&state).await, "Nothing to stop.");
    }

    #[tokio::test]
    async fn execute_cancel_stops_turn_and_does_not_repeat_pause_text() {
        let state = crate::channels::testkit::state().await;
        let turn: i64 = sqlx::query_scalar(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('slow', 'running', 'assistant', 'fake:c1', '2026-10-20T00:00:00Z')
             RETURNING id",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        // No process is registered for the chat, so the stop takes the kill path, which needs a handle.
        let task = tokio::spawn(tokio::time::sleep(std::time::Duration::from_secs(60)));
        state
            .run_handles
            .lock()
            .unwrap()
            .insert(turn, task.abort_handle());

        run(&state, Intent::Cancel).await;

        let answer = the_one_answer(&state).await;
        assert_eq!(answer, format!("Cancelled turn {turn}."));
        assert!(!answer.to_lowercase().contains("paused"), "{answer}");
        task.abort();
    }

    async fn a_proposal(state: &AppState, status: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO proposals
             (kind, status, run_id, session_id, project_id, tool_name, reasoning, tool_input, created_at, decided_at)
             VALUES ('action-approval', ?, NULL, NULL, NULL, 'Bash', 'why', NULL, ?, NULL)
             RETURNING id",
        )
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn execute_approve_maps_already_decided() {
        let state = crate::channels::testkit::state().await;
        let decided = a_proposal(&state, "approved").await;
        run(&state, Intent::Approve(decided)).await;
        run(&state, Intent::Approve(987_654)).await;
        let texts: Vec<String> = queued(&state).await.into_iter().map(|q| q.2).collect();
        assert_eq!(
            texts,
            vec![
                "That one was already handled.",
                "That one was already handled."
            ]
        );
    }

    #[tokio::test]
    async fn execute_reject_resolves_a_pending_proposal() {
        let state = crate::channels::testkit::state().await;
        let id = a_proposal(&state, "pending").await;
        run(&state, Intent::Reject(id)).await;
        assert_eq!(the_one_answer(&state).await, format!("Rejected #{id}."));
        let status: String = sqlx::query_scalar("SELECT status FROM proposals WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "rejected");
        // A second reject finds it decided.
        run(&state, Intent::Reject(id)).await;
        let last = queued(&state).await.pop().unwrap().2;
        assert_eq!(last, "That one was already handled.");
    }

    fn inbound<'a>(
        conversation: &'a contract::Conversation,
        sender: &'a contract::Sender,
    ) -> ingest::Inbound<'a> {
        ingest::Inbound {
            channel: "fake",
            key: contract::ConversationKey::new("fake", "c1"),
            conversation,
            sender,
            message_ref: "m1",
        }
    }

    fn message(reply_to: Option<&str>, parts: serde_json::Value) -> contract::InMessage {
        serde_json::from_value(serde_json::json!({
            "ref": "m1", "reply_to": reply_to, "parts": parts
        }))
        .unwrap()
    }

    fn text_message(reply_to: Option<&str>, text: &str) -> contract::InMessage {
        message(
            reply_to,
            serde_json::json!([{"type": "text", "text": text}]),
        )
    }

    async fn open_request(state: &AppState, job: i64) {
        sqlx::query(
            "INSERT OR IGNORE INTO jobs (id, project_id, project_root, status, max_items, created_at)
            VALUES (?, 'p', '/tmp', 'failed', 3, 't')",
        )
        .bind(job)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, \
             deadline, created_at) VALUES (?, 'p', '[]', 'what happened?', 'open', \
             '2099-01-01T00:00:00Z', '2026-10-20T00:00:00Z')",
        )
        .bind(job)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// A delivered outbox row of `conversation` that carries platform ref `platform_ref`.
    async fn delivered(state: &AppState, conversation: &str, platform_ref: &str, subject: &str) {
        sqlx::query(
            "INSERT INTO channel_outbox (channel, conversation, lane, action, payload, class, \
             subject, state, due_at, platform_refs, attempts, created_at, settled_at) \
             VALUES ('fake', ?, 'ordered', 'send', '{}', 'turn', ?, 'done', 't', ?, 0, 't', 's')",
        )
        .bind(conversation)
        .bind(subject)
        .bind(format!("[\"{platform_ref}\"]"))
        .execute(&state.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn capture_target_by_reply_to_matches_outbox_subject() {
        let state = crate::channels::testkit::state().await;
        delivered(&state, "c1", "p9", "capture:7").await;
        delivered(&state, "c1", "p8", "proposal:3").await;
        delivered(&state, "other", "p7", "capture:5").await;
        let (c, s) = (conversation(), sender());
        let at = inbound(&c, &s);
        let target = |reply_to: &'static str| {
            let message = text_message(Some(reply_to), "an answer");
            let (state, at) = (&state, &at);
            async move { capture_target(state, at, &message).await.unwrap() }
        };
        assert_eq!(target("p9").await, Some(CaptureTarget::Job(7)));
        // A reply to a proposal, to another conversation's capture, or to nothing known.
        assert_eq!(target("p8").await, None);
        assert_eq!(target("p7").await, None);
        assert_eq!(target("nope").await, None);
        let plain = text_message(None, "an answer");
        assert_eq!(capture_target(&state, &at, &plain).await.unwrap(), None);
    }

    async fn digest_request(state: &AppState, job: i64, digest: i64, state_: &str) {
        sqlx::query(
            "INSERT INTO capture_requests (job_id, project_id, causes, prompt_text, state, deadline, \
             created_at, digest_id) VALUES (?, 'p', '[]', 'q', ?, '2099-01-01T00:00:00Z', 't', ?)",
        )
        .bind(job)
        .bind(state_)
        .bind(digest)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_reply_to_a_digest_with_one_open_request_answers_it() {
        let state = crate::channels::testkit::state().await;
        delivered(&state, "c1", "p9", "capture_digest:3").await;
        digest_request(&state, 7, 3, "open").await;
        digest_request(&state, 8, 3, "answered").await;
        let (c, s) = (conversation(), sender());
        let at = inbound(&c, &s);
        let found = capture_target(&state, &at, &text_message(Some("p9"), "it was me"))
            .await
            .unwrap();
        assert_eq!(found, Some(CaptureTarget::Job(7)));
    }

    #[tokio::test]
    async fn a_reply_to_a_digest_with_several_open_requests_is_ambiguous_and_none_open_is_nothing()
    {
        let state = crate::channels::testkit::state().await;
        delivered(&state, "c1", "p9", "capture_digest:3").await;
        delivered(&state, "c1", "p8", "capture_digest:4").await;
        digest_request(&state, 9, 3, "open").await;
        digest_request(&state, 7, 3, "open").await;
        digest_request(&state, 5, 4, "expired").await;
        let (c, s) = (conversation(), sender());
        let at = inbound(&c, &s);
        let found = capture_target(&state, &at, &text_message(Some("p9"), "x"))
            .await
            .unwrap();
        assert_eq!(found, Some(CaptureTarget::Ambiguous(vec![7, 9])));
        assert_eq!(
            capture_target(&state, &at, &text_message(Some("p8"), "x"))
                .await
                .unwrap(),
            None
        );
    }

    #[test]
    fn the_digest_hint_names_the_open_jobs_and_the_cap_form() {
        assert_eq!(
            strings::render(&Text::CaptureDigestHint { jobs: vec![7, 9] }),
            "Several questions are open: #7, #9. Answer one with /cap <N> <text>."
        );
    }

    #[tokio::test]
    async fn run_capture_says_answered_only_when_a_reading_was_scheduled() {
        let state = crate::channels::testkit::state().await;
        let (c, s) = (conversation(), sender());
        let at = inbound(&c, &s);
        // The job exists: a reading was scheduled.
        open_request(&state, 7).await;
        let said = run_capture(&at, &state, 7, "it was me").await;
        assert_eq!(said, strings::render(&Text::CaptureAnswered { job: 7 }));
        // The request exists but its job row does not: the answer is kept, no reading follows.
        digest_request(&state, 8, 3, "open").await;
        let said = run_capture(&at, &state, 8, "too late").await;
        let note: i64 = sqlx::query_scalar("SELECT note_id FROM capture_requests WHERE job_id = 8")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(said, strings::render(&Text::CaptureLate { note }));
    }

    #[tokio::test]
    async fn capture_target_by_cap_form() {
        let state = crate::channels::testkit::state().await;
        let (c, s) = (conversation(), sender());
        let at = inbound(&c, &s);
        let found = capture_target(&state, &at, &text_message(None, "/cap 12 it was me"))
            .await
            .unwrap();
        assert_eq!(found, Some(CaptureTarget::Job(12)));
        // Malformed forms are not a capture answer.
        for text in ["/cap", "/cap 12", "/cap x y", "cap 12 hi"] {
            let found = capture_target(&state, &at, &text_message(None, text))
                .await
                .unwrap();
            assert_eq!(found, None, "{text}");
        }
        // The form is typed text only.
        let mixed = message(
            None,
            serde_json::json!([
                {"type": "text", "text": "/cap 12 hi"},
                {"type": "image", "media": "m_1"}
            ]),
        );
        assert_eq!(capture_target(&state, &at, &mixed).await.unwrap(), None);
    }

    #[tokio::test]
    async fn answer_capture_refuses_non_text() {
        let state = crate::channels::testkit::state().await;
        open_request(&state, 7).await;
        let (c, s) = (conversation(), sender());
        let at = inbound(&c, &s);
        for input in [
            media::TurnInput {
                text: "a caption".to_string(),
                non_text: true,
                ..Default::default()
            },
            media::TurnInput {
                text: "a transcript".to_string(),
                transcribed: true,
                non_text: true,
                ..Default::default()
            },
            media::TurnInput::default(),
        ] {
            answer_capture(&state, &at, 7, &input).await.unwrap();
        }
        let answers: Vec<String> = queued(&state).await.into_iter().map(|a| a.2).collect();
        let needs = strings::render(&Text::CaptureNeedsText);
        assert_eq!(answers, vec![needs.clone(), needs.clone(), needs]);
        let state_of: String =
            sqlx::query_scalar("SELECT state FROM capture_requests WHERE job_id = 7")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(state_of, "open", "a refused answer leaves the request open");
    }

    #[tokio::test]
    async fn answer_capture_writes_note_with_channel_origin() {
        let state = crate::channels::testkit::state().await;
        open_request(&state, 7).await;
        open_request(&state, 8).await;
        let (c, s) = (conversation(), sender());
        let at = inbound(&c, &s);
        // A reply: the whole text is the answer.
        let reply = media::TurnInput {
            text: "only I knew this".to_string(),
            ..Default::default()
        };
        answer_capture(&state, &at, 7, &reply).await.unwrap();
        // The `/cap` form: the answer is what follows the job number.
        let typed = media::TurnInput {
            text: "/cap 8 the second one".to_string(),
            ..Default::default()
        };
        answer_capture(&state, &at, 8, &typed).await.unwrap();

        let answers: Vec<String> = queued(&state).await.into_iter().map(|a| a.2).collect();
        assert_eq!(
            answers,
            vec![
                strings::render(&Text::CaptureAnswered { job: 7 }),
                strings::render(&Text::CaptureAnswered { job: 8 }),
            ]
        );
        let notes: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT r.job_id, n.note_text, n.origin FROM capture_requests r \
             JOIN owner_notes n ON n.id = r.note_id WHERE r.state = 'answered' ORDER BY r.job_id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            notes,
            vec![
                (
                    7,
                    "only I knew this".to_string(),
                    "channel:fake".to_string()
                ),
                (8, "the second one".to_string(), "channel:fake".to_string()),
            ]
        );
    }

    // An `Intent` answering a pending decision from a channel.

    use crate::tree_job::decisions;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::store::{self as tree_store, TreeJob};

    const QUESTION: &str = "Postgres or SQLite?";
    const OTHER_QUESTION: &str = "Ship it on Friday?";
    const SEEDED_AT: &str = "2026-10-30T10:00:00+00:00";

    fn explicit(id: &str, text: &str) -> Intent {
        Intent::DecisionAnswer {
            decision: Some(id.to_string()),
            text: text.to_string(),
        }
    }

    fn plain(text: &str) -> Intent {
        Intent::DecisionAnswer {
            decision: None,
            text: text.to_string(),
        }
    }

    fn parties(
        sender_ref: &str,
        conversation_ref: &str,
    ) -> (contract::Conversation, contract::Sender) {
        let conversation = contract::Conversation {
            reference: conversation_ref.to_string(),
            kind: contract::ConversationKind::Private,
            parent: None,
        };
        let sender = contract::Sender {
            reference: sender_ref.to_string(),
            name: None,
        };
        (conversation, sender)
    }

    /// Runs `intent` as if `sender_ref` wrote it in `conversation_ref` on `channel`.
    async fn run_as(
        state: &AppState,
        channel: &'static str,
        sender_ref: &str,
        conversation_ref: &str,
        intent: Intent,
    ) {
        let (conversation, sender) = parties(sender_ref, conversation_ref);
        let at = ingest::Inbound {
            channel,
            key: contract::ConversationKey::new(channel, conversation_ref),
            conversation: &conversation,
            sender: &sender,
            message_ref: "m1",
        };
        execute(state, &at, intent).await.unwrap();
    }

    /// `pending_answer` for `text`, as if `sender_ref` wrote it in `conversation_ref` on `channel`.
    async fn pending_as(
        state: &AppState,
        channel: &'static str,
        sender_ref: &str,
        conversation_ref: &str,
        text: &str,
    ) -> Option<Intent> {
        let (conversation, sender) = parties(sender_ref, conversation_ref);
        let at = ingest::Inbound {
            channel,
            key: contract::ConversationKey::new(channel, conversation_ref),
            conversation: &conversation,
            sender: &sender,
            message_ref: "m1",
        };
        pending_answer(state, &at, text).await.unwrap()
    }

    /// Makes `u1` the owner on `channel`, paired in conversation `c1`.
    async fn seed_owner(state: &AppState, channel: &str) {
        sqlx::query(
            "INSERT INTO channel_identities \
             (channel, sender_ref, conversation_ref, conversation_kind, created_at) \
             VALUES (?, 'u1', 'c1', 'private', ?)",
        )
        .bind(channel)
        .bind(SEEDED_AT)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// A job `job_id` whose one branch `b1` is `building`, then parked on `question`.
    async fn seed_parked(state: &AppState, job_id: &str, question: &str) -> decisions::Decision {
        let node_id = format!("node-{job_id}");
        let mut branch = Branch::new("b1", &[], "core", None);
        branch.state = "building".to_owned();
        branch.node_id = Some(node_id.clone());
        let mut manifest = Manifest::new(job_id, "feat/tree", "Ship it", "C:/Projects", &[]);
        manifest.branches.insert("b1".to_owned(), branch);
        let job = TreeJob {
            id: job_id.to_owned(),
            root_node_id: format!("root-{job_id}"),
            project_id: None,
            feature_branch: Some("feat/tree".to_owned()),
            goal: "Ship it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: SEEDED_AT.to_owned(),
            updated_at: SEEDED_AT.to_owned(),
        };
        tree_store::save(&state.pool, &job).await.unwrap();
        decisions::park(&state.pool, job_id, &node_id, question)
            .await
            .unwrap()
    }

    async fn decision_now(state: &AppState, id: &str) -> decisions::Decision {
        decisions::list(&state.pool)
            .await
            .unwrap()
            .into_iter()
            .find(|decision| decision.id == id)
            .expect("the decision is still there")
    }

    async fn branch_state(state: &AppState, job_id: &str) -> String {
        let job = tree_store::load(&state.pool, job_id)
            .await
            .unwrap()
            .unwrap();
        job.manifest.branches["b1"].state.clone()
    }

    /// The decision is exactly as `park` left it, and its branch still waits.
    async fn assert_still_parked(state: &AppState, parked: &decisions::Decision, job_id: &str) {
        let now = decision_now(state, &parked.id).await;
        assert_eq!(now.status, decisions::STATUS_OPEN);
        assert_eq!(now.answer, None);
        assert_eq!(now.origin, parked.origin);
        assert_eq!(branch_state(state, job_id).await, "parked");
    }

    /// The decision holds `answer`, written by Telegram, and its branch moves again.
    async fn assert_answered(state: &AppState, id: &str, job_id: &str, answer: &str) {
        let now = decision_now(state, id).await;
        assert_eq!(now.status, decisions::STATUS_ANSWERED);
        assert_eq!(now.answer.as_deref(), Some(answer));
        assert_eq!(now.origin.as_deref(), Some("telegram"));
        assert_eq!(branch_state(state, job_id).await, "building");
    }

    #[test]
    fn route_decide_command_parses_id_and_text() {
        assert_eq!(route("/decide d-1 sim"), explicit("d-1", "sim"));
        assert_eq!(
            route("  /DECIDE D-Abc   use postgres, not sqlite  "),
            explicit("D-Abc", "use postgres, not sqlite")
        );
        assert_eq!(
            route("/decide d-1\nline one\nline two"),
            explicit("d-1", "line one\nline two")
        );
    }

    #[test]
    fn route_decide_malformed_is_help() {
        for text in [
            "/decide",
            "/DECIDE",
            "/decide  ",
            "/decide d-1",
            "/decide d-1   ",
        ] {
            assert_eq!(route(text), Intent::Help, "{text:?}");
        }
    }

    #[test]
    fn route_plain_yes_stays_chat() {
        for text in ["sim", "yes", "ok", "não", "talvez"] {
            assert_eq!(route(text), chat(text), "{text:?}");
        }
        // Only the whole word is the command.
        assert_eq!(route("/decided d-1 yes"), chat("/decided d-1 yes"));
    }

    #[test]
    fn route_transcript_refuses_decide() {
        assert_eq!(
            route_transcript("/decide d-1 yes"),
            Intent::Refused("/decide d-1 yes".to_string())
        );
        assert_eq!(route_transcript("sim"), chat("sim"));
    }

    #[test]
    fn classify_answer_clear_forms() {
        let table = [
            ("sim", "sim"),
            ("yes", "yes"),
            ("ok", "ok"),
            ("aprovo", "aprovo"),
            ("não", "não"),
            ("nao", "nao"),
            ("no", "no"),
            ("rejeito", "rejeito"),
            // Case and a trailing mark do not matter, and the text stays as sent.
            ("  Sim.  ", "Sim."),
            ("YES!", "YES!"),
            ("\nNÃO\n", "NÃO"),
        ];
        for (input, sent) in table {
            assert_eq!(
                classify_answer(input),
                AnswerClass::Clear(sent.to_string()),
                "{input:?}"
            );
        }
    }

    #[test]
    fn classify_answer_ambiguous_forms() {
        let table = [
            "talvez",
            "maybe",
            "sim, mas so depois",
            "sim nao",
            "yes please",
            "usa postgres",
            "",
            "   ",
        ];
        for input in table {
            assert_eq!(classify_answer(input), AnswerClass::Ambiguous, "{input:?}");
        }
    }

    #[tokio::test]
    async fn execute_explicit_answer_from_the_telegram_owner_applies_and_resumes() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;
        assert_eq!(branch_state(&state, "job-1").await, "parked");

        // Naming the decision is what makes it unambiguous: this text would not pass as plain.
        let text = "usa postgres, por favor";
        run_as(&state, "telegram", "u1", "c1", explicit(&parked.id, text)).await;

        assert_answered(&state, &parked.id, "job-1", text).await;
        let reply = the_one_answer(&state).await;
        assert!(reply.contains(QUESTION), "{reply}");
    }

    #[tokio::test]
    async fn execute_explicit_answer_with_two_open_decisions_applies_only_the_named_one() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let first = seed_parked(&state, "job-1", QUESTION).await;
        let second = seed_parked(&state, "job-2", OTHER_QUESTION).await;

        run_as(&state, "telegram", "u1", "c1", explicit(&second.id, "sim")).await;

        assert_answered(&state, &second.id, "job-2", "sim").await;
        assert_still_parked(&state, &first, "job-1").await;
        let reply = the_one_answer(&state).await;
        assert!(reply.contains(OTHER_QUESTION), "{reply}");
    }

    #[tokio::test]
    async fn execute_plain_clear_answer_with_one_open_decision_applies() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;

        run_as(&state, "telegram", "u1", "c1", plain("sim")).await;

        assert_answered(&state, &parked.id, "job-1", "sim").await;
        let reply = the_one_answer(&state).await;
        assert!(reply.contains(QUESTION), "{reply}");
    }

    #[tokio::test]
    async fn execute_ambiguous_answer_asks_and_does_not_resume() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;

        run_as(&state, "telegram", "u1", "c1", plain("talvez")).await;

        assert_still_parked(&state, &parked, "job-1").await;
        let reply = the_one_answer(&state).await;
        assert!(reply.contains(QUESTION), "{reply}");
    }

    #[tokio::test]
    async fn execute_plain_answer_with_two_open_decisions_asks_and_resumes_neither() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let first = seed_parked(&state, "job-1", QUESTION).await;
        let second = seed_parked(&state, "job-2", OTHER_QUESTION).await;

        run_as(&state, "telegram", "u1", "c1", plain("sim")).await;

        assert_still_parked(&state, &first, "job-1").await;
        assert_still_parked(&state, &second, "job-2").await;
        let reply = the_one_answer(&state).await;
        assert!(reply.contains(QUESTION), "{reply}");
        assert!(reply.contains(OTHER_QUESTION), "{reply}");
    }

    #[tokio::test]
    async fn execute_plain_answer_with_none_open_changes_nothing() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;

        // No decision at all.
        run_as(&state, "telegram", "u1", "c1", plain("sim")).await;
        assert!(decisions::list(&state.pool).await.unwrap().is_empty());

        // A decision already answered is not open any more.
        let parked = seed_parked(&state, "job-1", QUESTION).await;
        run_as(&state, "telegram", "u1", "c1", explicit(&parked.id, "sim")).await;
        run_as(&state, "telegram", "u1", "c1", plain("não")).await;
        assert_answered(&state, &parked.id, "job-1", "sim").await;
        assert_eq!(decisions::list(&state.pool).await.unwrap().len(), 1);
    }

    /// The owner row is seeded for that very channel, so only the channel check can stop it.
    async fn assert_owner_on_channel_does_not_approve(channel: &'static str) {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, channel).await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;

        run_as(&state, channel, "u1", "c1", explicit(&parked.id, "sim")).await;
        assert_still_parked(&state, &parked, "job-1").await;

        run_as(&state, channel, "u1", "c1", plain("sim")).await;
        assert_still_parked(&state, &parked, "job-1").await;
    }

    #[tokio::test]
    async fn execute_answer_from_email_owner_does_not_approve() {
        assert_owner_on_channel_does_not_approve("email").await;
    }

    #[tokio::test]
    async fn execute_answer_from_fleet_owner_does_not_approve() {
        assert_owner_on_channel_does_not_approve("fleet").await;
    }

    #[tokio::test]
    async fn execute_answer_from_a_telegram_stranger_is_silent_and_changes_nothing() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;

        run_as(&state, "telegram", "u2", "c1", explicit(&parked.id, "sim")).await;
        run_as(&state, "telegram", "u2", "c1", plain("sim")).await;

        assert_still_parked(&state, &parked, "job-1").await;
        assert!(queued(&state).await.is_empty());
    }

    #[tokio::test]
    async fn execute_answer_from_the_owner_in_an_unpaired_conversation_changes_nothing() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;

        run_as(&state, "telegram", "u1", "c2", explicit(&parked.id, "sim")).await;
        run_as(&state, "telegram", "u1", "c2", plain("sim")).await;

        assert_still_parked(&state, &parked, "job-1").await;
        assert!(queued(&state).await.is_empty());
    }

    #[tokio::test]
    async fn execute_explicit_answer_to_an_unknown_id_says_so() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;

        run_as(&state, "telegram", "u1", "c1", explicit("d-nope", "sim")).await;

        assert_still_parked(&state, &parked, "job-1").await;
        let reply = the_one_answer(&state).await;
        assert!(reply.contains("d-nope"), "{reply}");
    }

    #[tokio::test]
    async fn execute_second_answer_keeps_the_first() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        let parked = seed_parked(&state, "job-1", QUESTION).await;

        run_as(&state, "telegram", "u1", "c1", explicit(&parked.id, "sim")).await;
        run_as(&state, "telegram", "u1", "c1", explicit(&parked.id, "não")).await;

        assert_answered(&state, &parked.id, "job-1", "sim").await;
    }

    #[tokio::test]
    async fn pending_answer_only_for_the_telegram_owner_with_an_open_decision() {
        let state = crate::channels::testkit::state().await;
        seed_owner(&state, "telegram").await;
        seed_owner(&state, "email").await;

        // No decision open yet.
        assert_eq!(
            pending_as(&state, "telegram", "u1", "c1", "sim").await,
            None
        );

        let parked = seed_parked(&state, "job-1", QUESTION).await;

        // Whether the text is clear is for `execute` to weigh, so both forms come through.
        assert_eq!(
            pending_as(&state, "telegram", "u1", "c1", "sim").await,
            Some(plain("sim"))
        );
        assert_eq!(
            pending_as(&state, "telegram", "u1", "c1", "talvez").await,
            Some(plain("talvez"))
        );

        assert_eq!(pending_as(&state, "email", "u1", "c1", "sim").await, None);
        assert_eq!(
            pending_as(&state, "telegram", "u2", "c1", "sim").await,
            None
        );
        assert_eq!(
            pending_as(&state, "telegram", "u1", "c2", "sim").await,
            None
        );

        // A text `route` recognises keeps its own meaning.
        assert_eq!(
            pending_as(&state, "telegram", "u1", "c1", "/help").await,
            None
        );
        assert_eq!(
            pending_as(&state, "telegram", "u1", "c1", "approve 3").await,
            None
        );

        // Answered, it is no longer open.
        decisions::answer(&state.pool, &parked.id, "sim", "telegram")
            .await
            .unwrap();
        assert_eq!(
            pending_as(&state, "telegram", "u1", "c1", "sim").await,
            None
        );
    }
}
