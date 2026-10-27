//! `POST /assistant/chats/{chat_id}/command`: run a typed slash command against a chat.
//!
//! The route is thin on purpose. Parsing, the registry, expansion and `/help` live in
//! `assistant::slash`; this file reads the chat, turns the module's `Outcome` into a body and its
//! `SlashError` into a status. A prompt goes through `send_expanded_or_queue`, so a command typed
//! while a turn runs waits in the queue like any other message, with its expansion kept.

use super::*;
use crate::assistant::slash::{Outcome, SlashError, expand, help, parse, registry};
use serde_json::{Value, json};

/// The body of the route: the line exactly as the composer holds it.
#[derive(serde::Deserialize)]
pub(in crate::http) struct CommandIn {
    text: String,
}

/// A refusal: a status and an `{error, detail, ...}` body.
type Refusal = (StatusCode, Json<Value>);

/// How many near names an unknown command suggests.
const SUGGESTIONS: usize = 5;

fn refuse(status: StatusCode, error: &str, detail: impl Into<String>, extra: Value) -> Refusal {
    let mut body = json!({ "error": error, "detail": detail.into() });
    if let (Some(into), Value::Object(from)) = (body.as_object_mut(), extra) {
        into.extend(from);
    }
    (status, Json(body))
}

fn internal() -> Refusal {
    refuse(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
        Value::Null,
    )
}

fn slash_refusal(error: SlashError) -> Refusal {
    match error {
        SlashError::Unknown { name, suggestions } => refuse(
            StatusCode::NOT_FOUND,
            "unknown_command",
            format!("no command named /{name}"),
            json!({ "suggestions": suggestions }),
        ),
        SlashError::InvalidArgument { usage, detail } => refuse(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            detail,
            json!({ "usage": usage }),
        ),
        SlashError::Busy => refuse(
            StatusCode::CONFLICT,
            "busy",
            "this conversation has a turn in flight",
            Value::Null,
        ),
        SlashError::NeedsProject => refuse(
            StatusCode::CONFLICT,
            "needs_project",
            "this command needs a conversation with a project directory",
            Value::Null,
        ),
        SlashError::Unsupported { engine, what } => refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_engine",
            format!("{engine:?} cannot {what}"),
            Value::Null,
        ),
        SlashError::WorkflowUnavailable { detail } => refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            "workflow_unavailable",
            detail,
            Value::Null,
        ),
        SlashError::Internal(detail) => {
            tracing::warn!(%detail, "running a slash command failed");
            internal()
        }
    }
}

/// Edit distance between two short names, for the "did you mean" list.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

/// The listed names closest to what was typed: those containing it first, then near misses.
fn suggestions(
    name: &str,
    cwd: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> Vec<String> {
    let listed = registry::listing(cwd, home);
    let wanted = name.to_lowercase();
    let mut names: Vec<String> = crate::commands::matching(&listed, &wanted)
        .into_iter()
        .map(|command| command.name)
        .collect();
    let mut near: Vec<(usize, String)> = listed
        .iter()
        .filter(|command| !names.contains(&command.name))
        .map(|command| {
            (
                distance(&wanted, &command.name.to_lowercase()),
                command.name.clone(),
            )
        })
        .filter(|(gap, _)| *gap <= 2)
        .collect();
    near.sort();
    names.extend(near.into_iter().map(|(_, name)| name));
    names.truncate(SUGGESTIONS);
    names
}

/// Whether the bundle workflow can run in this conversation: a project, wired tools, and the
/// workflow materialized there.
async fn workflow_ready(state: &AppState, cwd: Option<&str>) -> Result<(), SlashError> {
    let unavailable = |detail: &str| SlashError::WorkflowUnavailable {
        detail: detail.to_string(),
    };
    let Some(cwd) = cwd else {
        return Err(unavailable("this conversation has no project directory"));
    };
    // An unreadable roster reads as no workflow: the check fails closed.
    let roster = crate::governance::autopilot::rostered_projects(&state.pool)
        .await
        .unwrap_or_default();
    let dir = std::path::PathBuf::from(cwd);
    let machine_root = state.machine_config_root.clone();
    let (wired, missing) = tokio::task::spawn_blocking(move || {
        (
            crate::governance::autopilot::classifier_hook_is_wired(&dir),
            crate::governance::autopilot::workflow_missing(&dir, &roster, machine_root.as_deref()),
        )
    })
    .await
    .map_err(|error| SlashError::Internal(error.to_string()))?;
    if !wired {
        return Err(unavailable("the tools are not set up in this project"));
    }
    if missing {
        return Err(unavailable("the workflow is not set up in this project"));
    }
    Ok(())
}

/// Where personal and plugin commands are read from: the person's home.
#[cfg(not(test))]
fn home() -> Option<std::path::PathBuf> {
    crate::commands::home()
}

#[cfg(test)]
tokio::task_local! {
    /// Stands in for the person's home in a route test, so one can hold a personal command
    /// without writing to the real one.
    static TEST_HOME: Option<std::path::PathBuf>;
}

/// The test build's `home`: the scoped stand-in when a test set one, else the real home.
#[cfg(test)]
fn home() -> Option<std::path::PathBuf> {
    TEST_HOME
        .try_with(Clone::clone)
        .unwrap_or_else(|_| crate::commands::home())
}

/// Resolves and runs a parsed command. Blocking: it reads command files.
///
/// A bundle skill needs no case of its own: the registry offers it when its `SKILL.md` is in the
/// project, and `post_chat_command` has already checked `workflow_ready` before getting here.
fn run_command(
    parsed: &parse::Parsed,
    typed: &str,
    cwd: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> Result<Outcome, SlashError> {
    let unknown = || SlashError::Unknown {
        name: parsed.name.clone(),
        suggestions: suggestions(&parsed.name, cwd, home),
    };
    let command = registry::resolve(cwd, home, &parsed.name).ok_or_else(unknown)?;
    if command.source == crate::commands::Source::Builtin {
        return match command.name.as_str() {
            "help" => help::run(parsed, cwd, home),
            _ => Err(unknown()),
        };
    }
    // Read now and not at listing time: the file may have been deleted since the picker showed it.
    let file_text = std::fs::read_to_string(&command.path).map_err(|_| unknown())?;
    // A command that becomes a prompt belongs to a project: a conversation with none is refused
    // here, before any turn starts. Built-ins answered above and need none.
    if cwd.is_none() {
        return Err(SlashError::NeedsProject);
    }
    let base = command.path.parent().unwrap_or(std::path::Path::new(""));
    let expanded = expand::expand(&file_text, parsed, command.kind, base);
    Ok(Outcome::Prompt {
        text: expanded.text,
        typed: typed.to_string(),
    })
}

/// Runs the slash command a person typed in a conversation.
pub(in crate::http) async fn post_chat_command(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<CommandIn>,
) -> Result<Json<Value>, Refusal> {
    let cwd = crate::chats::opened_in(&state.pool, &chat_id)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "reading where a conversation runs failed");
            internal()
        })?
        .ok_or_else(|| {
            refuse(
                StatusCode::NOT_FOUND,
                "chat_not_found",
                "no such conversation",
                Value::Null,
            )
        })?;

    let parsed = match parse::parse(&body.text) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => {
            return Err(slash_refusal(SlashError::InvalidArgument {
                usage: "/name [arguments]".to_string(),
                detail: "that is not a slash command".to_string(),
            }));
        }
        Err(error) => return Err(slash_refusal(error)),
    };

    let bundle = registry::BUNDLE_SKILLS
        .iter()
        .any(|skill| skill.eq_ignore_ascii_case(&parsed.name));
    if bundle {
        workflow_ready(&state, cwd.as_deref())
            .await
            .map_err(slash_refusal)?;
    }

    let typed = body.text.trim().to_string();
    // Read here and not in the closure: the test build's stand-in is scoped to this task.
    let home = home();
    let outcome = tokio::task::spawn_blocking(move || {
        run_command(
            &parsed,
            &typed,
            cwd.as_deref().map(std::path::Path::new),
            home.as_deref(),
        )
    })
    .await
    .map_err(|_| internal())?
    .map_err(slash_refusal)?;

    match outcome {
        Outcome::Card { kind, data } => Ok(Json(json!({
            "kind": "card",
            "card": { "kind": kind, "data": data },
        }))),
        Outcome::Open { control } => Ok(Json(json!({ "kind": "open", "control": control }))),
        Outcome::Prompt { text, typed } => {
            match crate::assistant::send_expanded_or_queue(
                &state,
                &chat_id,
                &typed,
                &text,
                crate::assistant::Origin::Shell,
            )
            .await
            {
                Ok(crate::assistant::Sent::Turn(turn_id)) => {
                    Ok(Json(json!({ "kind": "prompt", "turn_id": turn_id })))
                }
                Ok(crate::assistant::Sent::Queued) => {
                    Ok(Json(json!({ "kind": "prompt", "queued": true })))
                }
                Err(msg) if msg == crate::assistant::TURN_IN_PROGRESS => {
                    Err(slash_refusal(SlashError::Busy))
                }
                Err(msg) if msg == crate::assistant::NO_LOCAL_MODEL => Err(refuse(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no_local_model",
                    "no local model is available",
                    Value::Null,
                )),
                Err(msg) if msg == crate::assistant::NO_HOSTED_MODEL => Err(refuse(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no_hosted_model",
                    "no hosted model is configured",
                    Value::Null,
                )),
                Err(msg) if msg == crate::assistants::HOSTED_KEY_MISSING => Err(refuse(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no_openrouter_key",
                    "the hosted model's key is not stored",
                    Value::Null,
                )),
                Err(error) => {
                    tracing::warn!(%error, "sending a slash command's prompt failed");
                    Err(internal())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::assistants::FixedAssistants;
    use crate::execution::runner::FakeCommandRunner;
    use crate::http::build_router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn state() -> AppState {
        crate::http::tests::test_state().await
    }

    async fn request(
        state: &AppState,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn command(state: &AppState, chat: &str, text: &str) -> (StatusCode, Value) {
        request(
            state,
            "POST",
            &format!("/assistant/chats/{chat}/command"),
            Some(json!({ "text": text })),
        )
        .await
    }

    /// A project directory holding the given `(relative path, body)` files.
    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        dir
    }

    async fn seed_chat(state: &AppState, id: &str, brain: &str, cwd: Option<&std::path::Path>) {
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at, cwd) VALUES (?, ?, '2026-08-20T10:00:00Z', ?)")
            .bind(id)
            .bind(brain)
            .bind(cwd.map(|p| p.to_string_lossy().to_string()))
            .execute(&state.pool)
            .await
            .unwrap();
    }

    const PING: (&str, &str) = (
        ".claude/commands/ping.md",
        "Ping the server named $ARGUMENTS.\n",
    );

    #[tokio::test]
    async fn command_route_unknown_command_404_with_suggestions() {
        let state = state().await;
        let dir = project(&[PING]);
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        let (status, body) = command(&state, "c1", "/hepl").await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "unknown_command", "{body}");
        let suggestions = body["suggestions"].as_array().expect("suggestions array");
        assert!(suggestions.len() <= 5, "{body}");
        assert!(
            suggestions.iter().any(|s| s
                .as_str()
                .is_some_and(|s| s.trim_start_matches('/') == "help")),
            "a near miss of /help suggests it: {body}"
        );
    }

    #[tokio::test]
    async fn command_route_invalid_argument_400() {
        let state = state().await;
        let dir = project(&[PING]);
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        // An unterminated quote cannot be split into arguments.
        let (status, body) = command(&state, "c1", "/ping \"unterminated").await;

        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"], "invalid_argument", "{body}");
        assert!(body.get("usage").is_some(), "{body}");
    }

    #[tokio::test]
    async fn command_route_chat_not_found_404() {
        let state = state().await;

        let (status, body) = command(&state, "no-such-chat", "/help").await;

        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"], "chat_not_found", "{body}");
    }

    #[tokio::test]
    async fn command_route_workflow_unavailable_422() {
        let state = state().await;
        // A project with the skill but no wired tools and no workflow: /orchestrate cannot run.
        let dir = project(&[(
            ".claude/skills/orchestrate/SKILL.md",
            "---\nname: orchestrate\n---\nGo.\n",
        )]);
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        let (status, body) = command(&state, "c1", "/orchestrate").await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["error"], "workflow_unavailable", "{body}");
    }

    #[tokio::test]
    async fn command_route_help_returns_card_200() {
        let state = state().await;
        let dir = project(&[PING]);
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        let (status, body) = command(&state, "c1", "/help").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["kind"], "card", "{body}");
        assert_eq!(body["card"]["kind"], "help", "{body}");
        assert!(body["card"].get("data").is_some(), "{body}");
    }

    #[tokio::test]
    async fn command_route_prompt_returns_turn_id_or_queued() {
        let state = state().await;
        let dir = project(&[PING]);
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        let (status, body) = command(&state, "c1", "/ping alpha").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["kind"], "prompt", "{body}");
        assert!(
            body["turn_id"].is_i64() || body["queued"] == true,
            "a prompt answers with a turn id or says it queued: {body}"
        );
    }

    #[tokio::test]
    async fn command_list_includes_builtin_help_and_hides_shadowed() {
        let state = state().await;
        // A project command named like the built-in is shadowed by it and must not be listed.
        let dir = project(&[PING, (".claude/commands/help.md", "Not the real help.\n")]);
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        let (status, body) = request(&state, "GET", "/assistant/chats/c1/commands", None).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let list = body
            .as_array()
            .or_else(|| body["commands"].as_array())
            .expect("a command list");
        let named = |n: &str| {
            list.iter()
                .filter(|c| {
                    c["name"]
                        .as_str()
                        .is_some_and(|x| x.trim_start_matches('/') == n)
                })
                .collect::<Vec<_>>()
        };
        let help = named("help");
        assert_eq!(help.len(), 1, "exactly one /help, the built-in: {body}");
        assert_eq!(help[0]["source"], "builtin", "{body}");
        assert!(
            !named("ping").is_empty(),
            "project commands stay listed: {body}"
        );
        assert_eq!(list[0]["source"], "builtin", "built-ins come first: {body}");
    }

    /// The workflow's skill as the bundle ships it: no `user-invocable` key.
    const ORCHESTRATE: (&str, &str) = (
        ".claude/skills/orchestrate/SKILL.md",
        "---\nname: orchestrate\ndescription: Run the workflow\n---\n\nTask: $ARGUMENTS\n",
    );

    #[tokio::test]
    async fn command_list_includes_bundle_orchestrate() {
        let state = state().await;
        let dir = project(&[ORCHESTRATE]);
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        let (status, body) = request(&state, "GET", "/assistant/chats/c1/commands", None).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        let list = body
            .as_array()
            .or_else(|| body["commands"].as_array())
            .expect("a command list");
        let orchestrate: Vec<&Value> = list
            .iter()
            .filter(|c| {
                c["name"]
                    .as_str()
                    .is_some_and(|x| x.trim_start_matches('/') == "orchestrate")
            })
            .collect();
        assert_eq!(orchestrate.len(), 1, "{body}");
        assert_eq!(orchestrate[0]["source"], "project", "{body}");
        assert_eq!(orchestrate[0]["kind"], "skill", "{body}");
    }

    #[tokio::test]
    async fn command_route_orchestrate_with_ready_workflow_prompts() {
        let state = state().await;
        let dir = project(&[ORCHESTRATE]);
        // Wired tools in a directory that is not a worktree: nothing is missing, the workflow runs.
        crate::governance::autopilot::wire_classifier_hook(dir.path()).unwrap();
        seed_chat(&state, "c1", "cloud", Some(dir.path())).await;

        let (status, body) = command(&state, "c1", "/orchestrate fix it").await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["kind"], "prompt", "{body}");
        assert!(
            body["turn_id"].is_i64() || body["queued"] == true,
            "a prompt answers with a turn id or says it queued: {body}"
        );
    }

    #[tokio::test]
    async fn command_route_needs_project_without_cwd() {
        let state = state().await;
        // A personal command, the only kind a conversation with no project can resolve.
        let home = project(&[PING]);
        seed_chat(&state, "c1", "cloud", None).await;

        let (status, body) = super::TEST_HOME
            .scope(
                Some(home.path().to_path_buf()),
                command(&state, "c1", "/ping alpha"),
            )
            .await;

        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"], "needs_project", "{body}");
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind("c1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0, "no turn starts");
    }

    // --- The same arguments reach every engine as the same model input ---

    const ECHO_ARGS: (&str, &str) = (
        ".claude/commands/echo-args.md",
        "All: $ARGUMENTS\nFirst: $1\n",
    );
    const ECHO_LINE: &str = "/echo-args \"a b\" c";

    struct Recording(Arc<std::sync::Mutex<Vec<Vec<Value>>>>);

    #[async_trait::async_trait]
    impl crate::models::local_agent::LocalChat for Recording {
        async fn exchange(
            &self,
            messages: Vec<Value>,
            _tools: Option<Vec<Value>>,
        ) -> std::io::Result<Value> {
            self.0.lock().unwrap().push(messages);
            Ok(json!({"role": "assistant", "content": "ok"}))
        }
    }

    struct NoTools;

    #[async_trait::async_trait]
    impl crate::models::local_agent::ToolBox for NoTools {
        fn schemas(&self) -> Vec<Value> {
            Vec::new()
        }
        async fn call(
            &self,
            _name: &str,
            _arguments: &Value,
        ) -> crate::models::local_agent::ToolAnswer {
            unreachable!("no tools are called")
        }
    }

    async fn first_user_input(seen: &Arc<std::sync::Mutex<Vec<Vec<Value>>>>) -> String {
        for _ in 0..200 {
            if let Some(first) = seen.lock().unwrap().first() {
                return first
                    .iter()
                    .rev()
                    .find(|m| m["role"] == "user")
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or_default()
                    .to_string();
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the local model was never asked");
    }

    async fn local_input(brain: crate::chats::Brain, cwd: &std::path::Path) -> String {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut state = state().await;
        state.assistants = Arc::new(FixedAssistants(Arc::new(
            crate::models::local_agent::LocalAssistant::new(
                Box::new(Recording(seen.clone())),
                Box::new(NoTools),
            ),
        )));
        let chat = crate::chats::create(&state.pool, brain, None)
            .await
            .unwrap();
        sqlx::query("UPDATE chats SET cwd = ? WHERE chat_id = ?")
            .bind(cwd.to_string_lossy().to_string())
            .bind(&chat)
            .execute(&state.pool)
            .await
            .unwrap();
        let (status, body) = command(&state, &chat, ECHO_LINE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        first_user_input(&seen).await
    }

    #[tokio::test]
    async fn echo_args_same_on_every_engine_() {
        let dir = project(&[ECHO_ARGS]);

        // claude: the prompt the CLI runner is handed.
        let fake = Arc::new(FakeCommandRunner::default());
        let mut state = state().await;
        state.runner = fake.clone();
        seed_chat(&state, "claude", "cloud", Some(dir.path())).await;
        let (status, body) = command(&state, "claude", ECHO_LINE).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let mut claude = String::new();
        for _ in 0..200 {
            claude = fake.last_prompt.lock().unwrap().clone().unwrap_or_default();
            if !claude.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert!(
            claude.contains("First: a b"),
            "$1 is the quoted pair: {claude:?}"
        );
        assert!(
            claude.contains("All: \"a b\" c"),
            "$ARGUMENTS is the line as typed: {claude:?}"
        );

        // codex: the trailing positional of the argument vector built from that same prompt.
        let request = crate::execution::runner::RunRequest {
            prompt: claude.clone(),
            env: Vec::new(),
            cwd: None,
            permission: crate::execution::runner::Permission::Default,
            resume_session_id: None,
            mcp_config: None,
            mcp_job: None,
            mcp_team_run: None,
            tool_policy: crate::execution::runner::ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: Some("123e4567-e89b-42d3-a456-426614174000".to_string()),
            fork_session: false,
            include_partial_messages: false,
            images: Vec::new(),
            steerable: false,
            classifier_governs_tools: false,
            ambient_mcp: false,
            model: None,
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            messages: None,
            request_tx: None,
            allowed_mcp_tools: None,
            background_tasks: false,
        };
        let args = crate::execution::runner::codex_cli_args(
            &request,
            "gpt-5",
            &crate::execution::runner::CodexStaged::default(),
        )
        .unwrap();
        let codex = args.last().cloned().unwrap_or_default();

        let local = local_input(crate::chats::Brain::Local, dir.path()).await;
        let openrouter = local_input(crate::chats::Brain::OpenRouter, dir.path()).await;

        assert_eq!(claude, codex, "claude vs codex");
        assert_eq!(claude, local, "claude vs local");
        assert_eq!(claude, openrouter, "claude vs openrouter");
    }
}
