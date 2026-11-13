//! Council and teams (`/council/*`, `/teams/*`, `/team-*`).

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        // The teams pillar. Everything here is the owner's except the last line: `/team-files/read`
        // is the only one a team run's own key opens, and which folder it reads is decided by that
        // key and never by the body — see `team::post_read_file`. `auth::TEAM_ROUTES` is where that
        // split is actually enforced; this is only where the names appear.
        .route(
            "/teams",
            get(crate::team::list_teams).post(crate::team::create_team),
        )
        .route(
            "/teams/{id}",
            get(crate::team::get_team)
                .put(crate::team::update_team)
                .delete(crate::team::delete_team),
        )
        .route("/teams/{id}/runs", post(crate::team::post_team_run))
        .route("/team-runs", get(crate::team::list_team_runs))
        .route(
            "/team-runs/{id}",
            get(crate::team::get_team_run).delete(crate::team::delete_team_run),
        )
        .route(
            "/team-runs/{id}/cancel",
            post(crate::team::post_team_run_cancel),
        )
        .route(
            "/team-runs/{id}/actions",
            get(crate::team::list_team_run_actions),
        )
        .route("/team-files/read", post(crate::team::post_read_file))
        // `/team-pressure` and not `/teams/pressure`: the siblings that collect across all teams
        // are called `/team-runs`, `/team-files`, `/team-actions`, and under `/teams/` the static
        // segment would shadow `/teams/{id}` for a team that happened to be named `pressure`.
        .route("/team-pressure", get(get_team_pressure))
        // Two callers, two methods, two scopes. A department POSTs what it would like done; only
        // the owner reads the queue of them. `auth::TEAM_ROUTES` lists the POST and not the GET, and
        // that pair is the whole of a department's authority to act.
        .route(
            "/team-actions",
            post(crate::team::post_team_action).get(crate::team::list_open_actions),
        )
        .route("/team-recruits", post(crate::team::post_team_recruit))
        // POST only, and no GET twin, for the reason `/team-actions` has one and `/team-recruits`
        // does not: a member may leave words for a colleague, and may not read the department's
        // mail. What each node is told arrives in its prompt, chosen by the daemon, and a route that
        // let a specialist read every note of the run would hand it the conversation it was
        // deliberately not part of.
        .route("/team-notes", post(crate::team::post_team_note))
        // POST only, again, and here the absent GET matters more: a department that could READ the
        // conversation it reports into would be reading the owner's chat. It may speak there and it
        // may not listen.
        .route("/team-reports", post(crate::team::post_team_report))
        // All Control, and NONE of them in `auth::TEAM_ROUTES`. A department neither arms nor fires
        // a rule, and that is not an oversight: it is what stops a chain feeding itself underneath
        // the graph the cycle check walks.
        .route(
            "/team-triggers",
            get(crate::team::list_triggers).post(crate::team::create_trigger),
        )
        .route(
            "/team-triggers/{id}",
            axum::routing::delete(crate::team::delete_trigger),
        )
        .route(
            "/team-triggers/{id}/enable",
            post(crate::team::post_trigger_enable),
        )
        .route(
            "/team-triggers/{id}/next",
            get(crate::team::get_trigger_next),
        )
        // The council. In no scope table in `auth.rs`, which leaves it to Admin and the control
        // token — the fail-closed default that module documents, and the right one for a route
        // whose POST spends money across up to nine model invocations.
        .route(
            "/council",
            get(crate::council::list_councils).post(crate::council::post_council),
        )
        // The form's defaults. A static segment, so the router prefers it over `{id}` whatever
        // the order — it sits first anyway, so a reader does not need to know that to see it win.
        .route("/council/config", get(crate::council::get_council_config))
        .route("/council/{id}", get(crate::council::get_council))
        .route(
            "/council/{id}/cancel",
            post(crate::council::post_council_cancel),
        )
}

#[derive(Deserialize)]
pub(super) struct PressureQuery {
    /// The team, by name. Resolves to its most recent job.
    pub(super) team: Option<String>,
    /// Or one specific job, by `team_runs.id`. Wins over the name when both are given.
    pub(super) job: Option<String>,
}

/// How much window each agent of the team cost, per round.
///
/// `protected` and not public: it is telemetry of the agent machine, and it names agents, rounds and
/// how much context each one spent.
///
/// Neither parameter has a default. Guessing a team would give an answer about the wrong team,
/// which is worse than none -- and whoever asks would have no way to notice.
pub(super) async fn get_team_pressure(
    State(state): State<AppState>,
    Query(query): Query<PressureQuery>,
) -> Result<Json<crate::pressure::Report>, StatusCode> {
    let scope = match (query.job, query.team) {
        (Some(job), _) => crate::pressure::Scope::Job(job),
        (None, Some(team)) => crate::pressure::Scope::Team(team),
        (None, None) => return Err(StatusCode::BAD_REQUEST),
    };
    match crate::pressure::measure(&state.pool, scope).await {
        Ok(report) => Ok(Json(report)),
        // Errar o nome e nunca ter corrido sao coisas diferentes: esta e a primeira, e a outra sai
        // daqui com 200 e `never_ran`.
        Err(sqlx::Error::RowNotFound) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
