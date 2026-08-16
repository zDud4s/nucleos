//! The one route in this daemon that puts a message beyond recall.
//!
//! Everything else the email pillar does is a read: a bad poll is re-read, a wrong triage class is
//! re-classified, a filed attachment is moved. A message that has left the submission server is
//! gone, under the mailbox owner's own address, to someone who will answer the person and not the
//! process. That asymmetry is why sending is its own module rather than three more handlers in
//! `http.rs`, and why the whole of its refusal logic lives in a pure function that can be asserted
//! against a table instead of against a mail server.
//!
//! Two things are kept apart here on purpose. `validate` decides whether a request is a message at
//! all — it is total, synchronous and has no idea a sidecar exists. `post_email_send` decides
//! whether this daemon is in a position to send one, and hands the bytes to the sidecar that owns
//! SMTP. The núcleo never opens a socket to a mail server; `sidecars/email/send` does, and it frames
//! the message itself so that framing has exactly one implementation.
//!
//! Nothing in this module logs. `to`, `subject` and `body` are the most private three strings the
//! daemon ever holds, and a rotating log file on disk is exactly the wrong place for them — so
//! there is no tracing call here to be careless with rather than a careful one.

use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// How long the sidecar gets to hand a message to the submission server.
///
/// Generous, because the far side is doing a TLS handshake, an authentication round trip and an
/// upload — over somebody's home connection, possibly with an attachment behind it. Finite, because
/// a request that never returns leaves the person who pressed send with no idea whether it went.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// One message, as the shell hands it over.
///
/// Deliberately three fields. Cc, Bcc and a reply-to are all header lines a caller could otherwise
/// smuggle through `to` — see `validate` — so the way to add one is to add a field here and a rule
/// there, not to widen what a recipient may contain.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct SendRequest {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// PURE: whether these two strings are a message at all.
///
/// Total, synchronous, and with no idea a sidecar exists — which is the whole point. The rule it
/// enforces is a header's, not an address's: a header ends at the first line break, so a `\r` or
/// `\n` accepted in `to` or `subject` does not corrupt the message, it ends that header and starts
/// another one that nobody approved. `Err` carries a reason fit to hand back to the caller, since a
/// 400 that will not say what was wrong is a 400 somebody retries unchanged.
///
/// It is not an address validator and must not become one. RFC 5322 permits things no mail server
/// would accept and forbids things every one of them does, so the bar here is the narrow one this
/// route actually needs: somebody to send to, on one side of exactly one `@`.
pub fn validate(to: &str, subject: &str) -> Result<(), &'static str> {
    if to.contains(['\r', '\n']) {
        return Err("a recipient must not contain a line break");
    }
    if subject.contains(['\r', '\n']) {
        return Err("a subject must not contain a line break");
    }
    if to.is_empty() {
        return Err("a message needs somebody to send it to");
    }
    // Exactly one `@`, with something either side of it. Zero is not an address; two is a decision
    // about which half is the domain that this code has no business making for a stranger.
    match to.split_once('@') {
        Some((local, domain))
            if !local.is_empty() && !domain.is_empty() && !domain.contains('@') => {}
        _ => return Err("a recipient is a name and a domain either side of exactly one @"),
    }
    Ok(())
}

/// `POST /email/send` — Admin-only, and the only route in this daemon that cannot be undone.
///
/// Three answers before anything is attempted, in the order that keeps the caller informed without
/// telling them anything about the deployment they did not already know:
///
/// - **400** — the request is not a message (`validate`). The caller's mistake, and fixable.
/// - **503** — this daemon is not in a position to send one: no submission host configured, or no
///   key minted for the sidecar. Nothing was attempted, and the shape of the deployment is at
///   fault rather than the caller, so a person fixes it rather than a retry.
/// - **502** — the sidecar was asked and something went wrong there.
///
/// The distinction between the last two is load-bearing: a 502 means bytes left this process, and
/// this route must not be able to say that while it has no key to go with.
pub async fn post_email_send(
    State(state): State<AppState>,
    Json(body): Json<SendRequest>,
) -> Response {
    if let Err(reason) = validate(&body.to, &body.subject) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }

    if state.email.smtp_host.trim().is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no submission host is configured — set smtp_host in .ai/email.yaml",
        )
            .into_response();
    }

    // The sidecar's OWN key, and no fallback. `state.token` is the control token: handing it to the
    // process that parses MIME written by strangers would be removing the arrangement that keeps it
    // away from there, at exactly the moment something has already gone wrong.
    let Some(sidecar_token) = state.email.sidecar_token.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the email sidecar has no key of its own — it was not started",
        )
            .into_response();
    };

    let Ok(client) = reqwest::Client::builder().timeout(SEND_TIMEOUT).build() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    // The same loopback address the attachment fetches use — one fact, one constant, so the two
    // processes cannot come to disagree about where the sidecar is.
    let response = client
        .post(format!("http://{}/send", crate::sidecar::EMAIL_FETCH_ADDR))
        .bearer_auth(sidecar_token)
        .json(&body)
        .send()
        .await;

    match response {
        Ok(response) if response.status().is_success() => StatusCode::NO_CONTENT.into_response(),
        // Both arms are 502 on purpose: "the sidecar refused" and "the sidecar could not be
        // reached" are the same answer to the person waiting — it did not go, and this daemon is
        // not the thing that failed. What must never land here is a request that was never sent.
        _ => (
            StatusCode::BAD_GATEWAY,
            "the email sidecar could not send the message",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Token;
    use crate::runner::FakeCommandRunner;
    use crate::state::{AppState, EmailRuntime};
    use axum::Json;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use std::sync::Arc;

    /// A recipient and a subject are headers, and a header ends at the first line break.
    ///
    /// `to` and `subject` reach this route from the núcleo relaying a model's output, which makes
    /// them the least trustworthy strings in the pillar. A `\r` or `\n` accepted in either one does
    /// not corrupt the message — it ENDS the header and starts another, so `"a@b.com\r\nBcc: …"` is
    /// a recipient the person who approved the send never saw, and no part of the UI would have
    /// shown it to them. The sidecar refuses this too (`send.Build`), and that is deliberate
    /// duplication rather than redundancy: this side answers 400 (the caller made a mistake), and a
    /// guard that only exists across a process boundary is one a future in-process caller skips.
    ///
    /// The `@` rule is the same argument in a quieter register. Exactly one, because zero is not an
    /// address and two is a decision about which half is the domain that this code has no business
    /// making on a stranger's behalf.
    #[test]
    fn a_header_break_in_a_recipient_or_subject_is_refused() {
        const REFUSED: &[(&str, &str, &str)] = &[
            // A carriage return, a newline, and the pair, in the recipient.
            ("a\r@b.com", "hello", "CR in the recipient"),
            ("a\n@b.com", "hello", "LF in the recipient"),
            ("a\r\n@b.com", "hello", "CRLF in the recipient"),
            (
                "a@b.com\r\nBcc: someone@else.example",
                "hello",
                "a smuggled Bcc header",
            ),
            // The same three in the subject, with a recipient that is otherwise fine, so it is the
            // subject and nothing else that decides the outcome.
            ("a@b.com", "hel\rlo", "CR in the subject"),
            ("a@b.com", "hel\nlo", "LF in the subject"),
            ("a@b.com", "hel\r\nlo", "CRLF in the subject"),
            (
                "a@b.com",
                "hello\r\nBcc: someone@else.example",
                "a smuggled Bcc header in the subject",
            ),
            // Nobody to send to.
            ("", "hello", "an empty recipient"),
            // Not exactly one `@`.
            ("a@b@c", "hello", "two at-signs"),
            ("a@b@c@d", "hello", "three at-signs"),
            ("noatsign", "hello", "no at-sign at all"),
            ("@", "hello", "an at-sign and nothing else"),
        ];

        for (to, subject, why) in REFUSED {
            assert!(
                validate(to, subject).is_err(),
                "{why}: validate({to:?}, {subject:?}) should have been refused"
            );
        }

        // The negative half only means something beside a positive one: a `validate` that refused
        // everything would pass every assertion above and send no mail ever again.
        assert!(
            validate("someone@example.com", "a perfectly ordinary subject").is_ok(),
            "an ordinary message must still go out"
        );
        // An empty subject is not a header injection. Terse is not malformed.
        assert!(
            validate("someone@example.com", "").is_ok(),
            "a subject nobody wrote is still a message"
        );
    }

    /// Minting failed, or startup never got that far — and the answer is to stop, not to substitute.
    ///
    /// `main.rs` mints the sidecar a key of its own before spawning it, precisely so the process
    /// that parses MIME written by strangers does not hold the control token. If that mint fails the
    /// sidecar is never started, and the tempting shape here is the one-line fallback to
    /// `state.token.0` — the daemon's own key, which opens every route — on the grounds that the
    /// request would otherwise fail. It would be handing the full key to the one process the
    /// arrangement exists to keep it away from, at exactly the moment something has already gone
    /// wrong. So: 503. Nothing was attempted, the shape of the deployment is at fault rather than
    /// the caller, and a person fixes it rather than a retry.
    ///
    /// `smtp_host` is set on purpose. An unconfigured host is the OTHER 503, and leaving it empty
    /// here would let this test pass for a reason that has nothing to do with the token.
    #[tokio::test]
    async fn an_unminted_sidecar_token_is_service_unavailable_not_the_control_token() {
        let state = test_state(EmailRuntime {
            smtp_host: "smtp.example.com".to_string(),
            sidecar_token: None,
            ..EmailRuntime::default()
        })
        .await;

        let status = post_email_send(
            State(state),
            Json(SendRequest {
                to: "someone@example.com".to_string(),
                subject: "hello".to_string(),
                body: "a message a person would have typed".to_string(),
            }),
        )
        .await
        .into_response()
        .status();

        // 503 and not 502: a 502 would mean something went to the sidecar and the sidecar refused,
        // which is the answer this route must NOT be able to give while it has no key to go with.
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "with no minted key the send must stop here, not fall back to the control token"
        );
    }

    async fn test_state(email: EmailRuntime) -> AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(":memory:")
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        AppState {
            // Recognisable on purpose: this is the value a fallback would reach for.
            token: Token("control-token".into()),
            pool,
            runner: Arc::new(FakeCommandRunner::default()),
            triage_runner: None,
            local_triage_disabled: None,
            local_assistant: None,
            run_handles: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_messages: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            run_tails: Default::default(),
            files_root: None,
            email: Arc::new(email),
            voice: Arc::new(crate::voice::VoiceRuntime::default()),
            web: Arc::new(crate::web::WebRuntime::disabled()),
            calendar: Arc::new(crate::calendar::CalendarRuntime::default()),
            council: std::sync::Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }
}
