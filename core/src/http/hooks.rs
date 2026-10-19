//! Claude Code hook decisions (`/hooks/*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/hooks/pretooluse-decision", post(pretooluse_decision))
        // The blocking half of the same conversation. Beside the gate because it carries the same
        // key and answers the same question, a moment later.
        .route("/hooks/ask-wait", post(post_ask_wait))
        // The third leg: what the tool call actually did, reported back after the fact. Never a
        // `Decision` and never a second barrier — by the time this fires the call has already run
        // (or already failed), so the handler only records and always answers OK. `auth::permits`
        // gives `Scope::Run` this route beside the two above for the same reason it gives it those:
        // one run, asking about one tool call of its own.
        .route("/hooks/posttooluse", post(posttooluse_outcome))
        // The fourth leg: after the gate allowed a heavy command, the hook asks here for a place on
        // the machine. It never decides and every failure answers `none` — the hook fails open.
        .route("/hooks/admit", post(post_admit))
        // The same gate for the sessions nobody launched. It is in no scope's table, by construction
        // rather than by a list: `permits` gives `Control` and an Admin API key everything and
        // answers every other scope from an allowlist, so a route absent from all of them is
        // reachable by the control token and an Admin key only — the same reach every other
        // unlisted route has (`an_admin_api_key_reaches_everything_control_reaches` holds Admin to
        // exactly that). `Scope::Run` must never arrive here — a run has its own route, whose
        // handler checks the claimed run against the token, and this one has no run to check.
        .route(
            "/hooks/session-git-decision",
            post(crate::hooks::session_git_decision),
        )
        // The hooks a pinned workflow bundle declares (`workflows::hooks`), forwarded by the same
        // script for the session events. In no scope's table either, for the reason given above
        // `session-git-decision`: it runs library scripts in a directory the body names, and the
        // run token's six routes are a list somebody has to widen on purpose, not by a new route.
        .route("/hooks/bundle", post(crate::workflows::hooks::bundle_hooks))
}

/// What the hook says while it waits.
#[derive(serde::Deserialize)]
pub(super) struct AskWaitRequest {
    pub(super) run_id: i64,
}

/// Holds the hook's call open until somebody answers for this turn's tool call.
///
/// The one route here that is SUPPOSED to block. The CLI is sitting on a hook call, the model is
/// sitting behind that, and a person is being asked a question — so the honest shape is a call that
/// does not return until there is an answer or the window closes.
///
/// Refuses on every path that is not a clear yes: nobody answered, the question is gone, this run
/// has none. A turn that cannot be allowed is refused, never allowed by default.
pub(super) async fn post_ask_wait(
    Extension(scope): Extension<crate::auth::Scope>,
    Json(body): Json<AskWaitRequest>,
) -> Json<crate::hooks::Decision> {
    // The key decides which turn this is, for the reason `pretooluse_decision` gives at length: the
    // body is a claim, and a CLI kept alive across turns claims whatever it was spawned with.
    let run_id = match scope {
        crate::auth::Scope::Run(id) => id,
        _ => body.run_id,
    };

    match crate::hooks::wait_for_run(run_id, crate::hooks::ASK_WINDOW).await {
        Some(true) => Json(crate::hooks::Decision {
            decision: "allow".to_owned(),
            reason: "you allowed this".to_owned(),
        }),
        Some(false) => Json(crate::hooks::Decision {
            decision: "deny".to_owned(),
            reason: "you refused this".to_owned(),
        }),
        None => Json(crate::hooks::Decision {
            decision: "deny".to_owned(),
            reason: "nobody answered for this in time".to_owned(),
        }),
    }
}

/// How long one `/hooks/admit` call is held before it answers `waiting`; the hook asks again.
const ADMIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(25);

/// What the hook says about the tool call it is about to let run.
#[derive(serde::Deserialize)]
pub(super) struct AdmitRequest {
    pub(super) run_id: i64,
    #[serde(default)]
    pub(super) tool_use_id: Option<String>,
    pub(super) tool_name: String,
    #[serde(default)]
    pub(super) tool_input: serde_json::Value,
}

/// `admission` is `none` | `granted` | `waiting` | `gave_up`; only `waiting` asks the hook to call
/// again, and only `none`/`gave_up` mean the command runs without a grant.
#[derive(serde::Serialize)]
pub(super) struct AdmitAnswer {
    pub(super) admission: &'static str,
    pub(super) weight: Option<u32>,
    pub(super) position: Option<usize>,
    pub(super) waited_seconds: Option<u64>,
}

impl AdmitAnswer {
    fn none() -> Self {
        Self {
            admission: "none",
            weight: None,
            position: None,
            waited_seconds: None,
        }
    }
}

/// Waits for machine capacity on behalf of a heavy command the gate already allowed.
///
/// Never a decision and never an error status: a body that parses always gets a 200, and every
/// failure on the way answers `none`.
pub(super) async fn post_admit(
    State(state): State<AppState>,
    Extension(scope): Extension<crate::auth::Scope>,
    Json(body): Json<AdmitRequest>,
) -> Json<AdmitAnswer> {
    // The key names the run, as in `post_ask_wait`: the body's claim is only used without one.
    let run_id = match scope {
        crate::auth::Scope::Run(id) => id,
        _ => body.run_id,
    };
    if !state.run_handles.lock().unwrap().contains_key(&run_id) {
        return Json(AdmitAnswer::none());
    }
    let Some(book) = crate::execution::admission::installed() else {
        return Json(AdmitAnswer::none());
    };
    let row: Result<Option<(Option<String>,)>, _> =
        sqlx::query_as("SELECT project_id FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(&state.pool)
            .await;
    let project_id = match row {
        Ok(Some((pid,))) => pid,
        _ => return Json(AdmitAnswer::none()),
    };
    let kinds = project_id
        .and_then(|pid| {
            crate::config::load_schedule_rules(state.machine_config_root.as_deref(), &pid).ok()
        })
        .map(|rules| rules.command_kinds)
        .unwrap_or_default();
    let config = state
        .machine_config_root
        .as_ref()
        .map(|root| root.join(crate::machine_config::VERIFY_FILE))
        .map(|path| crate::config::load_verify_config(&path).admission)
        .unwrap_or_default();
    Json(admit_with(Some(&*book), run_id, body, &kinds, &config, ADMIT_WINDOW).await)
}

/// The decision itself, apart from the route so a test can hold a private book.
pub(super) async fn admit_with(
    book: Option<&crate::execution::admission::AdmissionBook>,
    run_id: i64,
    body: AdmitRequest,
    project: &crate::config::CommandKinds,
    config: &crate::config::AdmissionConfig,
    window: std::time::Duration,
) -> AdmitAnswer {
    use crate::execution::admission::Poll;

    let Some(book) = book.filter(|_| config.enabled) else {
        return AdmitAnswer::none();
    };
    let Some(tool_use_id) = body.tool_use_id.as_deref().filter(|id| !id.is_empty()) else {
        return AdmitAnswer::none();
    };
    let Some(weight) = crate::execution::admission::heavy::weight_of_call(
        &body.tool_name,
        &body.tool_input,
        project,
        config,
    ) else {
        return AdmitAnswer::none();
    };
    match book.wait(run_id, tool_use_id, weight, window).await {
        Poll::Granted { weight } => AdmitAnswer {
            admission: "granted",
            weight: Some(weight),
            position: None,
            waited_seconds: None,
        },
        Poll::Waiting { position, waited } => AdmitAnswer {
            admission: "waiting",
            weight: None,
            position: Some(position),
            waited_seconds: Some(waited.as_secs()),
        },
        Poll::GaveUp => AdmitAnswer {
            admission: "gave_up",
            weight: None,
            position: None,
            waited_seconds: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{AdmitRequest, admit_with};
    use crate::config::{AdmissionConfig, CommandKinds};
    use crate::execution::admission::AdmissionBook;
    use serde_json::json;
    use std::time::Duration;

    fn book(capacity: u32) -> AdmissionBook {
        AdmissionBook::new(
            capacity,
            Duration::from_secs(900),
            Duration::from_secs(7200),
        )
    }

    fn call(tool_use_id: Option<&str>, command: &str) -> AdmitRequest {
        AdmitRequest {
            run_id: 1,
            tool_use_id: tool_use_id.map(str::to_owned),
            tool_name: "Bash".to_owned(),
            tool_input: json!({ "command": command }),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn admit_answers_none_for_a_light_command() {
        let book = book(1);
        let answer = admit_with(
            Some(&book),
            1,
            call(Some("toolu_light"), "ls -la"),
            &CommandKinds::default(),
            &AdmissionConfig::default(),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(answer.admission, "none");
        assert_eq!(answer.weight, None);
        assert_eq!(
            book.free(),
            1,
            "a light command takes nothing from the book"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn admit_grants_a_heavy_command_that_fits() {
        let book = book(2);
        let answer = admit_with(
            Some(&book),
            1,
            call(Some("toolu_heavy"), "cargo build"),
            &CommandKinds::default(),
            &AdmissionConfig::default(),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(answer.admission, "granted");
        assert_eq!(answer.weight, Some(1));
        assert_eq!(book.free(), 1);
        // The grant is held under the call's id, so the PostToolUse hook can give it back.
        assert!(book.release(1, "toolu_heavy"));
        assert_eq!(book.free(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn admit_answers_waiting_past_capacity() {
        let book = book(1);
        // Another run already holds the only unit.
        let _held = book.request(9, "holder", 1);
        let answer = admit_with(
            Some(&book),
            1,
            call(Some("toolu_queued"), "cargo test"),
            &CommandKinds::default(),
            &AdmissionConfig::default(),
            Duration::from_secs(2),
        )
        .await;
        assert_eq!(answer.admission, "waiting");
        assert!(answer.position.is_some(), "a waiter is told its place");
        assert_eq!(answer.weight, None);
    }

    #[tokio::test(start_paused = true)]
    async fn admit_answers_none_without_a_tool_use_id_or_book() {
        let book = book(1);
        let kinds = CommandKinds::default();
        let config = AdmissionConfig::default();
        let window = Duration::from_secs(1);

        let answer = admit_with(
            Some(&book),
            1,
            call(None, "cargo build"),
            &kinds,
            &config,
            window,
        )
        .await;
        assert_eq!(answer.admission, "none", "no tool_use_id");
        let answer = admit_with(
            Some(&book),
            1,
            call(Some(""), "cargo build"),
            &kinds,
            &config,
            window,
        )
        .await;
        assert_eq!(answer.admission, "none", "an empty tool_use_id");
        let answer = admit_with(
            None,
            1,
            call(Some("toolu_nobook"), "cargo build"),
            &kinds,
            &config,
            window,
        )
        .await;
        assert_eq!(answer.admission, "none", "no book installed");
        let disabled = AdmissionConfig {
            enabled: false,
            ..AdmissionConfig::default()
        };
        let answer = admit_with(
            Some(&book),
            1,
            call(Some("toolu_off"), "cargo build"),
            &kinds,
            &disabled,
            window,
        )
        .await;
        assert_eq!(answer.admission, "none", "admission switched off");
        assert_eq!(book.free(), 1, "none of these took capacity");
    }
}
