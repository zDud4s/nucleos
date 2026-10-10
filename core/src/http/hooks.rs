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
