//! The distiller's brain: which model the worker asks (cloud | local | openrouter), persisted, and the choosing logic `distill::run_distill_loop` reads every tick.

use crate::assistants::Refusal;
use crate::chats::Brain;
use crate::local_agent::LocalAssistant;
use crate::map_intent::Extractor;
use crate::runner::{CommandRunner, RunOutcome, RunRequest};
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Where the choice lives: a row of `schema_meta`, the daemon's own key/value table, so the setting
/// needs no migration of its own. Only the núcleo writes it.
pub const SETTING_KEY: &str = "distiller.model";

/// Whether the last tick's choice was refused, so the refusal is logged on the way IN and not on
/// every tick a worker that polls twice a minute would repeat it.
static REFUSED: AtomicBool = AtomicBool::new(false);

/// The one spelling a REQUEST may use: exactly `cloud`, `local` or `openrouter`.
///
/// `Brain::from_wire` reads anything it does not know as `Cloud`, which is the right reading for a
/// stored value (a brain nobody can parse is a brain nobody chose) and the wrong one for a request:
/// a typo from the shell would be saved as the cloud, a choice nobody made. So the door refuses what
/// the reader would forgive.
pub fn parse_choice(spelling: &str) -> Option<Brain> {
    match spelling {
        "cloud" => Some(Brain::Cloud),
        "local" => Some(Brain::Local),
        "openrouter" => Some(Brain::OpenRouter),
        _ => None,
    }
}

/// The stored choice. Nothing stored, a spelling nobody here wrote, or a database that cannot answer
/// all read as `Cloud` — what the distiller has always used — so the loop that asks every tick never
/// has a reason to stop.
pub async fn read(pool: &SqlitePool) -> Brain {
    sqlx::query_scalar::<_, String>("SELECT value FROM schema_meta WHERE key = ?")
        .bind(SETTING_KEY)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|stored| Brain::from_wire(&stored))
        .unwrap_or(Brain::Cloud)
}

/// Stores the choice in its wire spelling, replacing an earlier one.
pub async fn write(pool: &SqlitePool, brain: Brain) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO schema_meta (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(SETTING_KEY)
    .bind(brain.as_str())
    .execute(pool)
    .await?;
    Ok(())
}

/// The hosted brain wearing the agent CLI's trait, so the distiller's one `Extractor::Cli` arm
/// serves it and `map_intent::ask_once` stays the single door a prompt goes through.
///
/// Only `run_prompt` is implemented: everything else on `CommandRunner` has a default that routes
/// back here. No tools are offered (`LocalAssistant::one_shot`), and neither the prompt nor the
/// answer is ever logged — both are a dossier built from what the owner worked on.
pub struct HostedRunner(pub Arc<LocalAssistant>);

#[async_trait::async_trait]
impl CommandRunner for HostedRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        _session_tx: tokio::sync::mpsc::UnboundedSender<String>,
        _transcript: Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        // A hosted chat has no system-prompt flag, so the standing instruction goes first in the
        // one user message, ahead of the prompt it governs.
        let prompt = match &request.append_system_prompt {
            Some(standing) => format!("{standing}\n\n{}", request.prompt),
            None => request.prompt,
        };
        let answer = self.0.one_shot(&prompt).await?;

        // A model that answered nothing must not read as "nothing to learn": the row would be
        // marked done and its dossier never looked at again. A non-zero exit is how `ask_once`
        // hears a failed run.
        if answer.trim().is_empty() {
            return Ok(RunOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: "the hosted model returned no text".to_string(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            });
        }

        Ok(RunOutcome {
            exit_code: 0,
            stdout: answer,
            stderr: String::new(),
            session_id: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
        })
    }
}

/// Which brain a tick asks, once the choice has been checked against what this machine can serve.
pub enum Route {
    /// The agent CLI: what the distiller has always used.
    Cli,
    /// The model on this machine, reached over loopback.
    Loopback { model: String },
    /// The hosted model, reached as a runner.
    Hosted(HostedRunner),
}

/// PURE: the route for a choice, or the sentence that says why this machine cannot serve it.
///
/// A choice that cannot be served is REFUSED and never answered by the cloud instead: the owner
/// picked `local` or `openrouter` to keep a dossier off the cloud CLI, and quietly sending it there
/// would be the one outcome the choice exists to prevent. `hosted` is a factory so the cloud and
/// local routes never ask for an assistant (a cost and, with no key, a refusal that has nothing to
/// do with them), and `local_model` is the one thing the local route needs.
pub fn route(
    brain: Brain,
    local_model: Option<String>,
    hosted: impl FnOnce() -> Result<Arc<LocalAssistant>, Refusal>,
) -> Result<Route, &'static str> {
    match brain {
        Brain::Cloud => Ok(Route::Cli),
        Brain::Local => match local_model {
            Some(model) => Ok(Route::Loopback { model }),
            None => Err("no local model is configured"),
        },
        // Route off and a model named without a key are one refusal from here: neither is the cloud,
        // and the operator's fix for each lives on the hosted route's own screen.
        Brain::OpenRouter => match hosted() {
            Ok(assistant) => Ok(Route::Hosted(HostedRunner(assistant))),
            Err(_) => Err("the hosted route is not configured"),
        },
    }
}

impl Route {
    /// The asking half of the route, borrowing from `self` for the hosted runner and the loopback
    /// model name. The hosted brain is a runner, so it takes the CLI arm.
    pub fn extractor<'a>(
        &'a self,
        runner: &'a dyn CommandRunner,
        http: &'a reqwest::Client,
    ) -> Extractor<'a> {
        match self {
            Route::Cli => Extractor::Cli(runner),
            Route::Loopback { model } => Extractor::Loopback {
                client: http,
                base_url: crate::runner::OLLAMA_BASE_URL,
                model: model.as_str(),
            },
            Route::Hosted(hosted) => Extractor::Cli(hosted),
        }
    }
}

/// The route for THIS tick: the stored choice, the configured local model, and the hosted factory.
///
/// Read per call so a change takes effect on the next pass without a restart. A refused choice is
/// logged once when it starts to be refused (brain and reason only, never a dossier) and not again
/// until the choice has been served in between.
pub async fn route_for(state: &AppState) -> Result<Route, &'static str> {
    let brain = read(&state.pool).await;
    let local_model = crate::config::models_config_now().local_triage_model;
    let routed = route(brain, local_model, || {
        state.assistants.assistant_for(Brain::OpenRouter, None)
    });
    match &routed {
        Ok(_) => REFUSED.store(false, Ordering::Relaxed),
        Err(reason) => {
            if !REFUSED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    brain = brain.as_str(),
                    reason = *reason,
                    "distillation: the chosen model cannot be served; its queue waits"
                );
            }
        }
    }
    routed
}

/// `GET /config/distiller`.
pub async fn get_distiller_config(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "model": read(&state.pool).await.as_str() }))
}

/// The body of `POST /config/distiller`.
#[derive(serde::Deserialize)]
pub struct ModelBody {
    pub model: String,
}

/// `POST /config/distiller`: an unknown spelling is `422 {"error":"unknown_model"}` and is never
/// saved, a database error is 500, anything else answers the stored choice.
pub async fn post_distiller_config(
    State(state): State<AppState>,
    Json(body): Json<ModelBody>,
) -> axum::response::Response {
    let Some(brain) = parse_choice(&body.model) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "error": "unknown_model" })),
        )
            .into_response();
    };
    if write(&state.pool, brain).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    Json(serde_json::json!({ "model": brain.as_str() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistants::Refusal;
    use crate::chats::Brain;
    use crate::local_agent::{LocalAssistant, LocalChat, ToolAnswer, ToolBox};
    use crate::map_intent::Extractor;
    use crate::runner::{CommandRunner, RunOutcome, RunRequest};
    use serde_json::{Value, json};
    use sqlx::SqlitePool;

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    /// Answers one fixed message and remembers every conversation it was asked, so a test can read
    /// what the hosted model was actually sent.
    struct Recording {
        reply: Value,
        seen: Seen,
    }

    #[async_trait::async_trait]
    impl LocalChat for Recording {
        async fn exchange(
            &self,
            messages: Vec<Value>,
            _tools: Option<Vec<Value>>,
        ) -> std::io::Result<Value> {
            self.seen.lock().unwrap().push(messages);
            Ok(self.reply.clone())
        }
    }

    struct NoTools;

    #[async_trait::async_trait]
    impl ToolBox for NoTools {
        fn schemas(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn call(&self, _name: &str, _arguments: &Value) -> ToolAnswer {
            unreachable!("the distiller's hosted brain is asked once and offered no tools")
        }
    }

    /// Every request body the fake hosted brain received, in order.
    type Seen = Arc<Mutex<Vec<Vec<Value>>>>;

    /// A hosted assistant that answers `text`, and the log of what it was sent.
    fn hosted_saying(text: &str) -> (Arc<LocalAssistant>, Seen) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let chat = Recording {
            reply: json!({"role": "assistant", "content": text}),
            seen: seen.clone(),
        };
        let assistant = LocalAssistant::new(Box::new(chat), Box::new(NoTools));
        (Arc::new(assistant), seen)
    }

    /// A runner that must never be reached: a route under test that does not ask the CLI would
    /// fail loudly if one ever did.
    struct NeverRuns;

    #[async_trait::async_trait]
    impl CommandRunner for NeverRuns {
        async fn run_prompt(
            &self,
            _request: RunRequest,
            _session_tx: tokio::sync::mpsc::UnboundedSender<String>,
            _transcript: Arc<Mutex<String>>,
        ) -> std::io::Result<RunOutcome> {
            unreachable!("this route does not go through the agent CLI")
        }
    }

    /// The route, or a panic that names the refusal. `Route` carries no `Debug`, so `unwrap` on
    /// the whole result would not say which sentence came back.
    fn routed(result: Result<Route, &'static str>) -> Route {
        match result {
            Ok(route) => route,
            Err(why) => panic!("the choice was refused: {why}"),
        }
    }

    #[tokio::test]
    async fn the_setting_defaults_to_cloud_and_round_trips() {
        let pool = test_pool().await;

        // Nobody has chosen: the cloud is what the distiller has always used.
        assert_eq!(read(&pool).await, Brain::Cloud);

        write(&pool, Brain::Local).await.unwrap();
        assert_eq!(read(&pool).await, Brain::Local);

        // A second write replaces the first rather than failing on the key or adding a row.
        write(&pool, Brain::OpenRouter).await.unwrap();
        assert_eq!(read(&pool).await, Brain::OpenRouter);

        write(&pool, Brain::Cloud).await.unwrap();
        assert_eq!(read(&pool).await, Brain::Cloud);

        let (rows, stored): (i64, String) =
            sqlx::query_as("SELECT COUNT(*), MAX(value) FROM schema_meta WHERE key = ?")
                .bind(SETTING_KEY)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows, 1,
            "one setting is one row, however often it is written"
        );
        assert_eq!(stored, "cloud", "the stored spelling is the wire spelling");
        assert_eq!(SETTING_KEY, "distiller.model");
    }

    #[tokio::test]
    async fn an_unreadable_stored_value_reads_as_cloud() {
        let pool = test_pool().await;

        // A spelling nobody here wrote: a brain nobody chose.
        sqlx::query("INSERT INTO schema_meta (key, value) VALUES (?, 'gpt-9')")
            .bind(SETTING_KEY)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(read(&pool).await, Brain::Cloud);

        // And a database that cannot answer at all reads the same way, rather than panicking the
        // loop that asks every tick.
        sqlx::query("DROP TABLE schema_meta")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(read(&pool).await, Brain::Cloud);
    }

    #[test]
    fn only_the_three_spellings_are_accepted_on_write() {
        assert_eq!(parse_choice("cloud"), Some(Brain::Cloud));
        assert_eq!(parse_choice("local"), Some(Brain::Local));
        assert_eq!(parse_choice("openrouter"), Some(Brain::OpenRouter));

        // `Brain::from_wire` reads an unknown spelling as Cloud, which is right for a stored value
        // and wrong for a request: a typo from the shell must be refused, not saved as a choice.
        for refused in [
            "",
            " ",
            "Cloud",
            "LOCAL",
            "OpenRouter",
            " local",
            "local ",
            "hosted",
            "claude",
            "ollama",
            "open-router",
        ] {
            assert_eq!(
                parse_choice(refused),
                None,
                "{refused:?} is not one of the three spellings"
            );
        }
    }

    #[test]
    fn each_choice_routes_to_its_own_brain() {
        let client = reqwest::Client::new();
        let runner = NeverRuns;

        // The cloud never builds the hosted assistant: asking would be a cost and, with no key, a
        // refusal that has nothing to do with this choice.
        let cloud = routed(route(Brain::Cloud, Some("qwen3".to_string()), || {
            unreachable!("the cloud route must not ask for a hosted assistant")
        }));
        assert!(matches!(cloud, Route::Cli));
        assert!(matches!(
            cloud.extractor(&runner, &client),
            Extractor::Cli(_)
        ));

        let local = routed(route(Brain::Local, Some("qwen3".to_string()), || {
            unreachable!("the local route must not ask for a hosted assistant")
        }));
        match &local {
            Route::Loopback { model } => assert_eq!(model, "qwen3"),
            _ => panic!("local must route to the loopback model"),
        }
        match local.extractor(&runner, &client) {
            Extractor::Loopback {
                base_url, model, ..
            } => {
                assert_eq!(base_url, crate::runner::OLLAMA_BASE_URL);
                assert_eq!(model, "qwen3");
            }
            _ => panic!("the local route asks the model on this machine"),
        }

        let (assistant, _) = hosted_saying("unused");
        let hosted = routed(route(Brain::OpenRouter, None, || Ok(assistant)));
        assert!(matches!(hosted, Route::Hosted(_)));
        // The hosted brain is reached as a runner, so the distiller's one `Extractor::Cli` arm
        // serves it.
        assert!(matches!(
            hosted.extractor(&runner, &client),
            Extractor::Cli(_)
        ));
    }

    #[test]
    fn a_choice_that_cannot_be_served_refuses_rather_than_falling_to_the_cloud() {
        // Local with no model configured: refused, never answered by the cloud instead. The
        // hosted factory is not consulted for a local choice.
        let asked = AtomicBool::new(false);
        let refused = route(Brain::Local, None, || {
            asked.store(true, Ordering::SeqCst);
            Err(Refusal::RouteNotConfigured)
        });
        assert_eq!(refused.err(), Some("no local model is configured"));
        assert!(!asked.load(Ordering::SeqCst));

        // Hosted with the route off, and hosted with a model named but no key: both are the same
        // refusal from the distiller's side, and neither is the cloud. A configured LOCAL model
        // does not stand in for the hosted route either.
        for why in [
            Refusal::RouteNotConfigured,
            Refusal::HostedModelNamedButNoKey,
        ] {
            let refused = route(Brain::OpenRouter, Some("qwen3".to_string()), move || {
                Err(why)
            });
            assert_eq!(refused.err(), Some("the hosted route is not configured"));
        }
    }

    #[tokio::test]
    async fn the_hosted_brain_answers_through_the_same_door_the_cli_does() {
        let (assistant, seen) = hosted_saying("the distilled answer");
        let hosted = HostedRunner(assistant);

        // `ask_once` is the CLI path's own door: a prompt, a standing instruction, and a plain
        // string back. Going through it proves the hosted runner is a drop-in for the cloud one.
        let answer = crate::map_intent::ask_once(
            &hosted,
            "the dossier".to_string(),
            "distillation",
            Some("STANDING INSTRUCTION"),
        )
        .await
        .unwrap();
        assert_eq!(answer, "the distilled answer");

        // One message, the standing instruction first and the prompt after it: a hosted chat has
        // no system-prompt flag to carry the instruction another way.
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "the hosted model is asked exactly once");
            assert_eq!(
                seen[0],
                vec![json!({
                    "role": "user",
                    "content": "STANDING INSTRUCTION\n\nthe dossier"
                })]
            );
        }

        // Without a standing instruction the prompt goes as it is, with no stray separator.
        let (assistant, seen) = hosted_saying("ok");
        let hosted = HostedRunner(assistant);
        crate::map_intent::ask_once(&hosted, "alone".to_string(), "distillation", None)
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0],
            vec![json!({"role": "user", "content": "alone"})]
        );
    }

    #[tokio::test]
    async fn an_empty_hosted_answer_is_a_failure_not_an_empty_list() {
        // A model that answered nothing must not read as "nothing to learn": the row would be
        // marked done and its dossier never looked at again.
        for blank in ["", "   ", "\n\n"] {
            let (assistant, _) = hosted_saying(blank);
            let hosted = HostedRunner(assistant);

            let error = crate::map_intent::ask_once(
                &hosted,
                "the dossier".to_string(),
                "distillation",
                Some("STANDING"),
            )
            .await
            .expect_err("a blank answer is a failed run");

            let message = error.to_string();
            assert!(
                message.contains("exit code 1"),
                "the run must fail with exit code 1, got: {message}"
            );
            assert!(
                message.contains("the hosted model returned no text"),
                "the failure must say why, got: {message}"
            );
        }
    }
}
