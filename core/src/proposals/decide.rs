//! Deciding a proposal as a service function: `approve`, with the answers an HTTP door needs but no
//! HTTP types in the way it is called.
//!
//! Started with the four kinds that start no run (`contact-merge`, `refinement`, `calendar-event`,
//! `github-action`); the handler delegates exactly those and keeps the rest until they move here.
//! The messages are the ones the handler has always sent: moving the code must not change what a
//! person reads.

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::door::uncancellable;
use crate::state::AppState;

/// Why a decision did not happen. [`DecideError::into_http`] is the one place that turns it into a
/// status and a body.
#[derive(Debug)]
pub enum DecideError {
    /// No such proposal.
    NotFound,
    /// Somebody decided it first.
    AlreadyDecided,
    /// A refusal the person can resolve and then decide again. 409 with the sentence.
    Conflict(String),
    /// The request names something that cannot be done (an unusable scope, a proposal carrying no
    /// usable payload). 422 with the sentence.
    Invalid(String),
    /// The request itself is malformed. 400 with the sentence.
    BadRequest(String),
    /// A call out to another service failed. 502 with the sentence.
    BadGateway(String),
    /// A fault on our side. The text is the body the caller gets; empty means no body.
    Internal(String),
    /// A refusal that carries the status of its own cause. Transitional: the GitHub failure of a
    /// proposal that is already `approved` answers 503, 403, 504 or 500 depending on what failed,
    /// and none of the fixed variants above says that without changing what the door answers today.
    Status(StatusCode, String),
}

impl DecideError {
    /// The status and body the HTTP door answers with. `id` only fills in the not-found sentence.
    pub fn into_http(self, id: i64) -> (StatusCode, String) {
        match self {
            Self::NotFound => (StatusCode::NOT_FOUND, format!("there is no proposal {id}")),
            Self::AlreadyDecided => (
                StatusCode::CONFLICT,
                "this proposal has already been decided".to_owned(),
            ),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::Invalid(message) => (StatusCode::UNPROCESSABLE_ENTITY, message),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::BadGateway(message) => (StatusCode::BAD_GATEWAY, message),
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
            Self::Status(status, message) => (status, message),
        }
    }
}

/// What a person may add to an approval. Every field is optional, and each is read by one kind only.
#[derive(Default)]
pub struct ApproveInput {
    /// Only a recruitment reads it: the corrected hire.
    pub hire: Option<crate::agent::AgentRequest>,
    /// Only a browser-wheel approval reads it: `"shell"` or `"window"`.
    pub seat: Option<String>,
    /// Only a `refinement` approval reads it: `agent:<id>`, `team:<id>` or `project:<id>`.
    pub scope: Option<String>,
}

/// Approves proposal `id`.
///
/// Reads the proposal for its kind and dispatches; the kind never changes and every branch is a
/// compare-and-set on `status = 'pending'`, so a second decision racing this one loses there.
/// A kind that has not moved here yet is a `BadRequest`: the handler never sends one.
pub async fn approve(state: &AppState, id: i64, input: ApproveInput) -> Result<Value, DecideError> {
    let proposal = super::get(&state.pool, id)
        .await
        .map_err(|error| {
            tracing::warn!(proposal_id = id, %error, "reading a proposal to approve failed");
            DecideError::Internal("the proposal could not be read".to_owned())
        })?
        .ok_or(DecideError::NotFound)?;
    match proposal.kind.as_str() {
        "contact-merge" => approve_contact_merge(state, id).await,
        "refinement" => approve_refinement(state, id, input.scope.as_deref()).await,
        "calendar-event" => approve_calendar_event(state, id).await,
        "github-action" => approve_github_action(state, id).await,
        other => Err(DecideError::BadRequest(format!(
            "a `{other}` proposal is not approved here"
        ))),
    }
}

async fn approve_contact_merge(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // Uncancellable for the same reason the resume is: the decision commits, and a request dropped
    // mid-flight must not leave the record disagreeing with what happened.
    let state = state.clone();
    let outcome = uncancellable(async move {
        crate::integrations::contacts::approve_merge(&state.pool, id).await
    })
    .await
    .map_err(|_| DecideError::Internal("the merge task did not finish".to_owned()))?;
    merge_decision_response(outcome)
}

async fn approve_refinement(
    state: &AppState,
    id: i64,
    rescope: Option<&str>,
) -> Result<Value, DecideError> {
    // The third kind that starts no run: approving activates the refinement and decides the
    // proposal in one transaction. Uncancellable for the reason the others give — a dropped request
    // must not leave the proposal and the layer disagreeing about whether the agent was allowed to
    // learn something.
    let target = rescope
        .map(|raw| {
            refinement_scope(raw).ok_or_else(|| {
                DecideError::Invalid(format!(
                    "`{raw}` is not a scope a refinement can be approved into: agent:<id>, team:<id> or project:<id>"
                ))
            })
        })
        .transpose()?;
    // A scope naming an agent or a team must name one that exists; and a scope equal to the
    // declared one is the plain approval, so a refusal below can only be about the move.
    let target = match target {
        Some(scope) => {
            refuse_unknown_owner(&state.pool, &scope).await?;
            match crate::knowledge::declared_in(&state.pool, id, &scope).await {
                Ok(true) => None,
                Ok(false) => Some(scope),
                Err(error) => return Err(refinement_refusal(error, id, false)),
            }
        }
        None => None,
    };
    let was_rescoped = target.is_some();
    let state = state.clone();
    let activated = uncancellable(async move {
        crate::knowledge::approve_as(&state.pool, id, target.as_ref()).await
    })
    .await
    .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match activated {
        Ok(refinement_id) => Ok(json!({ "refinement_id": refinement_id })),
        Err(error) => Err(refinement_refusal(error, id, was_rescoped)),
    }
}

async fn approve_calendar_event(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // Approving writes the event and the decision in one transaction. Uncancellable for the same
    // reason as the others — a dropped request must not leave the proposal and the calendar
    // disagreeing.
    use crate::integrations::calendar::DecisionError;
    let state = state.clone();
    let created = uncancellable(async move {
        crate::integrations::calendar::approve_proposed_event(&state.pool, id).await
    })
    .await
    .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match created {
        Ok(event_id) => Ok(json!({ "event_id": event_id })),
        Err(DecisionError::NotFound) => Err(DecideError::NotFound),
        Err(DecisionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(DecisionError::Malformed) => {
            tracing::warn!(
                proposal_id = id,
                "a calendar proposal carried no usable event"
            );
            Err(DecideError::Invalid(
                "this proposal carries no usable event, so there is nothing to create".to_owned(),
            ))
        }
        Err(DecisionError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "approving a calendar proposal failed");
            Err(DecideError::Internal(
                "the event could not be written".to_owned(),
            ))
        }
    }
}

async fn approve_github_action(state: &AppState, id: i64) -> Result<Value, DecideError> {
    // The kind that ACTS on approval, where `team-action` waits for a tick. A pillar answering
    // synchronously has no later pass to be picked up on, so without this the button would approve
    // nothing.
    //
    // Uncancellable for the reason all of its neighbours are, and here it carries more: the work
    // between the claim and the note is a live call to GitHub, and a request dropped across it
    // would leave a comment published and the row saying only that somebody said yes.
    use crate::integrations::github::DecisionError;
    let state = state.clone();
    let ran = uncancellable(async move {
        crate::integrations::github::approve_proposed_operation(&state.pool, &state.github, id)
            .await
    })
    .await
    .map_err(|_| DecideError::Internal("the approval task did not finish".to_owned()))?;
    match ran {
        Ok(outcome) => Ok(json!({
            "operation": outcome.kind,
            "exit_code": outcome.exit_code,
            "stdout": outcome.stdout,
            "output_tail": outcome.output_tail,
        })),
        Err(DecisionError::NotFound) => Err(DecideError::NotFound),
        Err(DecisionError::NotPending) => Err(DecideError::AlreadyDecided),
        Err(DecisionError::Malformed) => {
            tracing::warn!(
                proposal_id = id,
                "a github proposal carried no usable operation"
            );
            Err(DecideError::Invalid(
                "this proposal carries no usable operation, so there is nothing to run".to_owned(),
            ))
        }
        // The proposal is already `approved` and the note beneath it says what happened. The
        // status is the failure's own, so a missing token does not read as a broken daemon.
        Err(DecisionError::Failed(failure)) => Err(DecideError::Status(
            github_failure_status(&failure),
            format!("approved, and it did not run: {failure}"),
        )),
        Err(DecisionError::Db(error)) => {
            tracing::warn!(proposal_id = id, %error, "approving a github proposal failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

/// The same table `http::common::github_failure_status` holds; that one is private to the HTTP
/// module, which this file must not depend on.
fn github_failure_status(failure: &crate::integrations::github::Failure) -> StatusCode {
    match failure {
        crate::integrations::github::Failure::NotConfigured
        | crate::integrations::github::Failure::MissingCli => StatusCode::SERVICE_UNAVAILABLE,
        crate::integrations::github::Failure::MissingToken => StatusCode::FORBIDDEN,
        crate::integrations::github::Failure::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        crate::integrations::github::Failure::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// `agent:<id>`, `team:<id>` or `project:<id>`; the same grammar as `http::common::refinement_target`.
fn refinement_scope(raw: &str) -> Option<crate::knowledge::Scope> {
    let (kind, id) = raw.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    match kind {
        "project" => Some(crate::knowledge::Scope::Project(id.to_owned())),
        "team" => Some(crate::knowledge::Scope::Team(id.to_owned())),
        "agent" => Some(crate::knowledge::Scope::Agent(id.to_owned())),
        _ => None,
    }
}

/// Turns a merge decision's outcome into the answer the caller gets.
///
/// The conflict is a 409 with a body, not a bare status: it is the one refusal here that names
/// something the person can go and change, and a status code cannot say which two instructions
/// disagree.
fn merge_decision_response(
    outcome: Result<
        crate::integrations::contacts::MergeOutcome,
        crate::integrations::contacts::DecisionError,
    >,
) -> Result<Value, DecideError> {
    match outcome {
        Ok(crate::integrations::contacts::MergeOutcome::Merged) => Ok(json!({ "merged": true })),
        Ok(crate::integrations::contacts::MergeOutcome::RefusedConflictingVerdicts {
            keep,
            absorb,
        }) => {
            tracing::info!(%keep, %absorb, "refused a contact merge with conflicting verdicts");
            Err(DecideError::Conflict(format!(
                "these two people carry standing decisions that disagree — {keep} against {absorb}; settle one of them and decide this again"
            )))
        }
        Err(crate::integrations::contacts::DecisionError::NotPending) => Err(
            DecideError::Conflict("this suggestion has already been decided".to_owned()),
        ),
        Err(crate::integrations::contacts::DecisionError::Db(error)) => {
            tracing::warn!(%error, "deciding a contact merge failed");
            Err(DecideError::Internal(
                "the decision could not be recorded".to_owned(),
            ))
        }
    }
}

/// 422 when `scope` names an agent or a team the núcleo does not have. A project is not checked:
/// a project id is whatever a run was started with, and has no table of its own to be missing from.
async fn refuse_unknown_owner(
    pool: &sqlx::SqlitePool,
    scope: &crate::knowledge::Scope,
) -> Result<(), DecideError> {
    let (sql, noun, owner) = match scope {
        crate::knowledge::Scope::Agent(owner) => {
            ("SELECT 1 FROM agents WHERE id = ?", "agent", owner)
        }
        crate::knowledge::Scope::Team(owner) => ("SELECT 1 FROM teams WHERE id = ?", "team", owner),
        _ => return Ok(()),
    };
    let found: Option<i64> = sqlx::query_scalar(sql)
        .bind(owner)
        .fetch_optional(pool)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "checking the scope an approval names failed");
            DecideError::Internal("the scope could not be checked".to_owned())
        })?;
    match found {
        Some(_) => Ok(()),
        None => Err(DecideError::Invalid(format!(
            "there is no {noun} `{owner}` to keep this refinement"
        ))),
    }
}

/// What the refinement door says when deciding refuses. `Malformed` after a real move can only be
/// the move itself (a successor stays where its predecessor lived); otherwise the proposal names
/// no row.
fn refinement_refusal(
    error: crate::knowledge::DecisionError,
    _id: i64,
    was_rescoped: bool,
) -> DecideError {
    match error {
        crate::knowledge::DecisionError::NotFound => DecideError::NotFound,
        crate::knowledge::DecisionError::NotPending => DecideError::AlreadyDecided,
        crate::knowledge::DecisionError::Malformed => DecideError::Invalid(if was_rescoped {
            "this refinement replaces an earlier one, so it can only be approved in the scope \
             it was declared in"
                .to_owned()
        } else {
            "this proposal does not name a refinement".to_owned()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistants::NoAssistants;
    use crate::auth::Token;
    use crate::execution::runner::FakeCommandRunner;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    async fn test_state() -> AppState {
        AppState {
            startup_config: Default::default(),
            token: Token("t".into()),
            pool: crate::testdb::fresh_pool().await,
            telegram_doctrine: None,
            channels: Default::default(),
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
            secrets: Arc::new(crate::secrets::InMemorySecrets::default()),
            email: Arc::new(crate::state::EmailRuntime::default()),
            voice: Arc::new(crate::integrations::voice::VoiceRuntime::default()),
            browser: Arc::new(crate::browser::BrowserRuntime::disabled()),
            github: Arc::new(crate::integrations::github::GithubRuntime::default()),
            web: Arc::new(crate::integrations::web::WebRuntime::disabled()),
            quota: Arc::new(crate::quota::QuotaRuntime::disabled()),
            judge: Arc::new(crate::judge::JudgeRuntime::disabled()),
            calendar: Arc::new(crate::integrations::calendar::CalendarRuntime::default()),
            council: Arc::new(crate::council::CouncilRuntime::default()),
            progress_timeout: crate::state::DEFAULT_PROGRESS_TIMEOUT,
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    async fn a_contact(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES (NULL, ?)")
            .bind("2026-10-19T08:00:00+00:00")
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    #[tokio::test]
    async fn decide_approve_unknown_proposal_is_not_found() {
        let state = test_state().await;
        let result = approve(&state, 999_999, ApproveInput::default()).await;
        assert!(matches!(result, Err(DecideError::NotFound)), "{result:?}");
    }

    #[tokio::test]
    async fn decide_approve_contact_merge_twice_is_already_decided() {
        let state = test_state().await;
        let keep = a_contact(&state.pool).await;
        let absorb = a_contact(&state.pool).await;
        let id = crate::proposals::create_contact_merge(&state.pool, keep, absorb, "same name")
            .await
            .unwrap();

        let first = approve(&state, id, ApproveInput::default()).await.unwrap();
        assert_eq!(first, json!({ "merged": true }));

        // The second decision finds the proposal no longer pending; the contact-merge door says
        // so with its own sentence, a 409 either way.
        let second = approve(&state, id, ApproveInput::default()).await;
        let error = second.expect_err("a second approval must be refused");
        let (status, body) = error.into_http(id);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body, "this suggestion has already been decided");
    }

    #[test]
    fn decide_error_into_http_maps_each_variant_to_its_status() {
        let cases = [
            (
                DecideError::NotFound,
                StatusCode::NOT_FOUND,
                "there is no proposal 7",
            ),
            (
                DecideError::AlreadyDecided,
                StatusCode::CONFLICT,
                "this proposal has already been decided",
            ),
            (
                DecideError::Conflict("c".to_owned()),
                StatusCode::CONFLICT,
                "c",
            ),
            (
                DecideError::Invalid("i".to_owned()),
                StatusCode::UNPROCESSABLE_ENTITY,
                "i",
            ),
            (
                DecideError::BadRequest("b".to_owned()),
                StatusCode::BAD_REQUEST,
                "b",
            ),
            (
                DecideError::BadGateway("g".to_owned()),
                StatusCode::BAD_GATEWAY,
                "g",
            ),
            (
                DecideError::Internal("x".to_owned()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "x",
            ),
            (
                DecideError::Internal(String::new()),
                StatusCode::INTERNAL_SERVER_ERROR,
                "",
            ),
            (
                DecideError::Status(StatusCode::FORBIDDEN, "s".to_owned()),
                StatusCode::FORBIDDEN,
                "s",
            ),
        ];
        for (error, status, body) in cases {
            let label = format!("{error:?}");
            assert_eq!(error.into_http(7), (status, body.to_owned()), "{label}");
        }
    }
}
