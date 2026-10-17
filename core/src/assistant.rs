//! §spec pilar-de-web

use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use crate::execution::runner::extract_reply;

mod chat_key;

mod live;
mod send;
mod serve;
mod slots;
mod spontaneous;
mod turn;

pub use self::live::*;
pub use self::send::*;
use self::serve::*;
pub use self::slots::*;
use self::spontaneous::*;
pub use self::turn::*;

/// The context window a conversation is given, unless its own row asks for a different one.
///
/// An ABSOLUTE token count, not a fraction, because everything it is compared against is one:
/// `runs.context_fill` is `input_tokens + cache_read_input_tokens` off the live assistant events,
/// and `sessions::Conversation::context_estimate` is a character count divided by four. 140k is
/// ≈0.7 of the 200k window the runner assumes as its conservative floor — deliberately below it, so
/// an ordinary conversation is summarised while it is still cheap to summarise.
///
/// This number USED to mean "the point past which this daemon refuses to resume". It no longer
/// refuses anything: it is handed to the CLI as `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, and the CLI
/// compacts its own context inside the same session rather than the daemon minting a new one. See
/// `0116_chats_context_window.sql` for why that swap is both gentler and cheaper.
pub const CONTEXT_WINDOW_TOKENS: i64 = 140_000;

/// The largest window there is any point asking for.
///
/// The CLI takes `CLAUDE_CODE_AUTO_COMPACT_WINDOW` up to a million and then caps it at the model's
/// real window anyway, so a bigger number here would buy a promise the model cannot keep. 200k is
/// the window the runner already assumes as its conservative floor, and assuming the same number in
/// two places is how the two come to disagree — so this is the one that names it.
///
/// Only the pick-up path ever reaches for it: a conversation continued from the editor may arrive
/// carrying more than the default window can hold, and raising its window to fit is what lets it be
/// resumed instead of stumped.
pub const LARGEST_WINDOW_TOKENS: i64 = 200_000;

/// The headroom the CLI keeps below the window before it compacts.
///
/// Read out of the CLI's own binary — its threshold is `window - 13000` — rather than guessed, and
/// named here because the pick-up path has to answer "will this session fit" and the honest answer
/// is "does it fit under the line the CLI will actually draw", not "under the window".
pub const COMPACTION_HEADROOM: i64 = 13_000;

/// The window this conversation runs in: its own if it asked for one, the default otherwise.
///
/// Clamped on the way out rather than on the way in, because a row written by an older daemon — or
/// by a pick-up whose ceiling has since changed — is not something a turn should refuse over. The
/// CLI clamps this again at its own end; agreeing with it here means the number the window SHOWS is
/// the number the CLI will actually use.
pub fn window_of(asked: Option<i64>) -> i64 {
    asked
        .unwrap_or(CONTEXT_WINDOW_TOKENS)
        .clamp(CONTEXT_WINDOW_TOKENS, LARGEST_WINDOW_TOKENS)
}

/// The session a chat's next turn resumes, or `None` when it must start clean.
///
/// The `NOT EXISTS` is the second half of the barrier `hooks.rs` opens. That one refuses to let a
/// turn act after it has read third-party text; this one stops the text outliving the turn. Without
/// it the barrier holds for one message and no longer: `--resume` hands the next turn the same
/// context, that turn's own row is clean, and so the `approve_proposal` a mail body asked for is
/// simply made in the message after the one that read it.
///
/// Expressed as a condition on the READ rather than as a delete when the turn ends, deliberately.
/// A turn ends by completing, by failing, by timing out, by being cancelled, and by the daemon
/// being killed underneath it — five paths, of which the last runs no cleanup code at all. A
/// session that is unresumable because of what the database says about it is unresumable on every
/// one of them, including across a restart.
///
/// There is deliberately no second condition about SIZE, and there used to be one.
///
/// A conversation whose context had grown past 140k was refused here, which minted a fresh session
/// and left the model with six replayed exchanges and no memory of the rest. Size is now the CLI's
/// business: it is given the window as `CLAUDE_CODE_AUTO_COMPACT_WINDOW` and compacts inside this
/// same session, so a long conversation stays ONE conversation. What remains here is the untrusted
/// barrier, which is a safety property and not a cost one — the two were never the same rule and
/// only ever shared a `WHERE`.
pub async fn get_session(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<Option<String>> {
    let session_id: Option<Option<String>> = sqlx::query_scalar(
        "SELECT s.session_id FROM assistant_sessions s
          WHERE s.chat_id = ?
            AND NOT EXISTS (SELECT 1 FROM runs r
                             WHERE r.session_id = s.session_id
                               AND r.read_untrusted = 1)",
    )
    .bind(chat_id)
    .fetch_optional(pool)
    .await?;

    Ok(session_id.flatten())
}

/// Leaves a chat with nothing to resume, so its next turn starts on a fresh context.
///
/// Not an error path: this is what a turn that read third-party text is supposed to leave behind.
pub async fn forget_session(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM assistant_sessions WHERE chat_id = ?")
        .bind(chat_id)
        .execute(pool)
        .await
        .map(|_| ())
}

pub async fn upsert_session(
    pool: &SqlitePool,
    chat_id: &str,
    session_id: &str,
    updated_at: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO assistant_sessions (chat_id, session_id, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(chat_id) DO UPDATE SET
             session_id = excluded.session_id,
             updated_at = excluded.updated_at",
    )
    .bind(chat_id)
    .bind(session_id)
    .bind(updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Where a chat's throwaway MCP config lives, with `chat_id` encoded rather than interpolated.
///
/// `chat_id` arrives from a sidecar and is an opaque string to us, so it cannot be trusted to be a
/// filename. Interpolated raw it reached `Path::join`, which **discards the base** when the joined
/// component is absolute: a `chat_id` of `C:/Windows/System32/x` wrote the config there instead of
/// in the temp directory, and `TurnGuard::drop` then removed whatever it had landed on — an
/// arbitrary write and delete for anything holding the daemon token.
///
/// Encoding, not validating: the id stays opaque (chats are not required to look like numbers, and
/// the tests rely on that), and the mapping stays injective, so two chats differing only in an
/// escaped character cannot collide onto one file and clobber each other's config mid-turn.
///
/// **The process id is in the name, and it is not decoration.** The temp directory is shared by
/// every process on the machine, so a name built only from the id is the SAME path in two of them
/// — and both write it and both delete it. On this machine that is not hypothetical: the daemon
/// runs while suites run, and several checkouts run suites at once, each with tests that use fixed
/// ids. One deleting the other's config mid-turn is a failure with no cause visible anywhere near
/// it. `transcribe.rs` already names its recordings this way, for the same reason.
fn mcp_config_path(chat_id: &str) -> std::path::PathBuf {
    let mut safe = String::with_capacity(chat_id.len());
    for byte in chat_id.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => safe.push(byte as char),
            // `%` itself lands here, which is what keeps the encoding reversible.
            other => safe.push_str(&format!("%{other:02x}")),
        }
    }
    std::env::temp_dir().join(format!("nucleos-mcp-{}-{safe}.json", std::process::id()))
}

fn write_mcp_config(path: &std::path::Path, config: &serde_json::Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(config).map_err(std::io::Error::other)?;
    crate::storage::write_atomic(path, &bytes)
}

/// The throwaway MCP config a turn is launched with.
pub fn build_mcp_config(exe_path: &str) -> serde_json::Value {
    let args = vec!["--mcp-tools".to_string()];
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": args
            }
        }
    })
}

/// The throwaway MCP config a job node is launched with.
pub fn build_job_node_mcp_config(exe_path: &str, job_id: i64) -> serde_json::Value {
    let args = vec![
        "--mcp-tools".to_string(),
        "--box".to_string(),
        "job-node".to_string(),
        "--job".to_string(),
        job_id.to_string(),
    ];
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": args
            }
        }
    })
}

/// The throwaway MCP config a team agent's node run is launched with.
pub fn build_team_mcp_config(exe_path: &str, run_id: i64) -> serde_json::Value {
    let args = vec![
        "--mcp-tools".to_string(),
        "--box".to_string(),
        "team".to_string(),
        "--run".to_string(),
        run_id.to_string(),
    ];
    serde_json::json!({
        "mcpServers": {
            "nucleos": {
                "type": "stdio",
                "command": exe_path,
                "args": args
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistants::{FixedAssistants, NoAssistants, RecordingAssistants};
    use crate::auth::Token;
    use crate::execution::runner::FakeCommandRunner;
    use crate::state::AppState;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    /// A chat id no other test in this process is using.
    ///
    /// `ChatSlot::acquire` is process-global, while these tests run in parallel. Reusing an id in
    /// two tests lets one test borrow the other's slot and makes the loser report a busy chat.
    fn a_chat(prefix: &str) -> String {
        static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
        format!(
            "{prefix}:{}",
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )
    }

    struct CliAssistants {
        claude: Option<Arc<dyn crate::execution::runner::CommandRunner>>,
        codex: Option<Arc<dyn crate::execution::runner::CommandRunner>>,
    }

    #[async_trait::async_trait]
    impl crate::assistants::Assistants for CliAssistants {
        fn assistant_for(
            &self,
            _brain: crate::chats::Brain,
            _model: Option<&str>,
        ) -> Result<Arc<crate::models::local_agent::LocalAssistant>, crate::assistants::Refusal>
        {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        fn serves(&self, _brain: crate::chats::Brain) -> Result<(), crate::assistants::Refusal> {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        async fn can_serve(
            &self,
            _brain: crate::chats::Brain,
            _model: &str,
        ) -> Result<(), crate::assistants::Refusal> {
            Err(crate::assistants::Refusal::RouteNotConfigured)
        }

        async fn declared_for(
            &self,
            _brain: crate::chats::Brain,
            _models: &[String],
        ) -> std::collections::HashMap<String, crate::capabilities::Declared> {
            std::collections::HashMap::new()
        }

        fn cli_runner(
            &self,
            cli: &str,
            _model: Option<&str>,
        ) -> Option<Arc<dyn crate::execution::runner::CommandRunner>> {
            match cli {
                "claude" => self.claude.clone(),
                "codex" => self.codex.clone(),
                _ => None,
            }
        }
    }

    fn turn_config() -> crate::config::ModelsConfig {
        crate::config::ModelsConfig {
            assistant_choices: vec![
                crate::config::AssistantChoice {
                    id: "sonnet".to_string(),
                    label: "Sonnet".to_string(),
                    brain: "cloud".to_string(),
                    efforts: ["low", "medium", "high", "xhigh", "max"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    runner: Some("claude".to_string()),
                    tools: None,
                    installed: None,
                },
                crate::config::AssistantChoice {
                    id: "opus".to_string(),
                    label: "Opus".to_string(),
                    brain: "cloud".to_string(),
                    efforts: ["low", "medium", "high", "xhigh", "max"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    runner: Some("claude".to_string()),
                    tools: None,
                    installed: None,
                },
                crate::config::AssistantChoice {
                    id: "gpt-5.6-terra".to_string(),
                    label: "GPT-5.6 Terra".to_string(),
                    brain: "cloud".to_string(),
                    efforts: ["low", "medium", "high", "xhigh", "max", "ultra"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    runner: Some("codex".to_string()),
                    tools: None,
                    installed: None,
                },
                crate::config::AssistantChoice {
                    id: "gpt-5.5".to_string(),
                    label: "GPT-5.5".to_string(),
                    brain: "cloud".to_string(),
                    efforts: ["low", "medium", "high", "xhigh"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                    runner: Some("codex".to_string()),
                    tools: None,
                    installed: None,
                },
            ],
            ..crate::config::ModelsConfig::default()
        }
    }

    async fn test_pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn test_state() -> AppState {
        AppState {
            token: Token("t".into()),
            pool: test_pool().await,
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

    /// A local assistant that answers one fixed sentence and calls no tools.
    fn fake_local_assistant(
        answer: &'static str,
    ) -> Arc<crate::models::local_agent::LocalAssistant> {
        struct OneLiner(&'static str);
        #[async_trait::async_trait]
        impl crate::models::local_agent::LocalChat for OneLiner {
            async fn exchange(
                &self,
                _messages: Vec<serde_json::Value>,
                _tools: Option<Vec<serde_json::Value>>,
            ) -> std::io::Result<serde_json::Value> {
                Ok(serde_json::json!({"role": "assistant", "content": self.0}))
            }
        }

        struct NoTools;
        #[async_trait::async_trait]
        impl crate::models::local_agent::ToolBox for NoTools {
            fn schemas(&self) -> Vec<serde_json::Value> {
                Vec::new()
            }
            async fn call(
                &self,
                _name: &str,
                _arguments: &serde_json::Value,
            ) -> crate::models::local_agent::ToolAnswer {
                unreachable!("this assistant answers without calling tools")
            }
        }

        Arc::new(crate::models::local_agent::LocalAssistant::new(
            Box::new(OneLiner(answer)),
            Box::new(NoTools),
        ))
    }

    /// A local assistant whose turn reads mail and then fails at the endpoint.
    ///
    /// The shape that matters: the taint happens, and then there is no `Turn` to carry it. It is
    /// why the flag became an `AtomicBool` the caller owns, and it had no test.
    fn local_assistant_that_reads_mail_then_dies() -> Arc<crate::models::local_agent::LocalAssistant>
    {
        struct CallsThenDies;
        #[async_trait::async_trait]
        impl crate::models::local_agent::LocalChat for CallsThenDies {
            async fn exchange(
                &self,
                messages: Vec<serde_json::Value>,
                _tools: Option<Vec<serde_json::Value>>,
            ) -> std::io::Result<serde_json::Value> {
                // First exchange: ask for the mail. Second: the model is gone.
                if messages.iter().any(|m| m["role"] == "tool") {
                    return Err(std::io::Error::other("ollama went away"));
                }
                Ok(serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"function": {"name": "get_email", "arguments": {"id": 1}}}]
                }))
            }
        }

        struct MailBox;
        #[async_trait::async_trait]
        impl crate::models::local_agent::ToolBox for MailBox {
            fn schemas(&self) -> Vec<serde_json::Value> {
                vec![serde_json::json!({
                    "type": "function",
                    "function": {"name": "get_email", "description": "read", "parameters": {}}
                })]
            }
            async fn call(
                &self,
                _name: &str,
                _arguments: &serde_json::Value,
            ) -> crate::models::local_agent::ToolAnswer {
                crate::models::local_agent::ToolAnswer {
                    text: serde_json::json!({"body": "olá"}).to_string(),
                    untrusted: true,
                }
            }
        }

        Arc::new(crate::models::local_agent::LocalAssistant::new(
            Box::new(CallsThenDies),
            Box::new(MailBox),
        ))
    }

    /// A turn that read mail and then died is still marked as having read it.
    ///
    /// The flag used to ride on the returned `Turn`, which a transport error destroys — so the run
    /// row said clean, `recent_exchanges` would hand the next turn a history containing a
    /// stranger's words, and that turn's barrier would start open.
    #[tokio::test]
    async fn a_turn_that_read_mail_and_then_failed_is_still_marked() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(local_assistant_that_reads_mail_then_dies()));

        let id = send_message(
            &state,
            "tg-taint-survives",
            "que mail chegou?",
            Origin::Telegram,
        )
        .await
        .unwrap();
        let (status, _) = settled_turn(&state.pool, id).await;

        assert_eq!(status, "failed");
        let marked: i64 = sqlx::query_scalar("SELECT read_untrusted FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            marked, 1,
            "the turn read mail and the row does not say so, so the next turn inherits it clean"
        );
    }

    /// Polls until the turn leaves `running`, the way every other test in this module waits for a
    /// spawned turn, and returns its status and reply.
    /// Waits for a turn to be over in BOTH senses, because they are not the same moment.
    ///
    /// A turn ends twice. The `runs` row reaches a terminal status inside the turn's task; the
    /// [`TurnGuard`] holding the chat's slot drops when that task ENDS, which is strictly later.
    /// In between, the row says settled and `send_message` still answers "a turn is already in
    /// progress for this chat" — so a test that sends its second message on the row alone is
    /// racing the guard's drop and will lose it sometimes.
    ///
    /// **Measured, 2026-08-22, and this is how it was found.** Six full suite runs at the default
    /// thread count were green; three more at `--test-threads=32` produced one failure, on the
    /// `unwrap` of the second `send_message` in
    /// `a_conversation_without_tools_starts_a_process_for_every_turn`. Four times the cores is what
    /// widens the window: the task is descheduled between writing the row and dropping the guard
    /// for long enough that the next send sees the slot still held. Seven other tests in this
    /// module send a second message to one chat behind this same helper, so the race was theirs
    /// too — it simply landed on that one first.
    ///
    /// [`is_busy`] is the answer to the right question, and its own doc already says why the row is
    /// not: asking `runs` is "wrong in both directions". The chat is read OFF the row rather than
    /// passed in, so that no call site can forget to wait for it.
    ///
    /// A turn the fake delays (up to 2 s) must fit inside the first loop's budget.
    async fn settled_turn(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        let mut settled = None;
        for _ in 0..500 {
            let row: (String, Option<String>, Option<String>) =
                sqlx::query_as("SELECT status, stdout, chat_id FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if row.0 != "running" {
                settled = Some(row);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let Some((status, stdout, chat_id)) = settled else {
            panic!("turn {id} never left running");
        };

        // The second ending. A run with no chat is not an assistant turn and holds no slot to wait
        // on — the column is nullable precisely because most runs are not turns.
        let Some(chat_id) = chat_id else {
            return (status, stdout);
        };
        for _ in 0..100 {
            if !is_busy(&chat_id) {
                return (status, stdout);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("turn {id} settled but chat {chat_id} was never released");
    }

    /// What `FakeCommandRunner::default` answers, so a test can say "this went down the CLI path"
    /// without asserting on a string whose meaning is not obvious at the call site.
    const CLI_FAKE_REPLY: &str = "fake output";

    /// A model only a vendor list (or the built-in catalogue) names has no config row, so its id
    /// is what says which CLI answers it.
    #[test]
    fn a_discovered_gpt_model_is_answered_by_codex() {
        let config = turn_config();
        assert_eq!(answering_cli(&config, Some("gpt-9.9-nova")), "codex");
        assert_eq!(answering_cli(&config, Some("o9-mini")), "codex");
        assert_eq!(answering_cli(&config, Some("claude-opus-9-9")), "claude");
        // An id that says nothing keeps the active runner.
        assert_eq!(
            answering_cli(&config, Some("llama3.2:3b")),
            if config.active_runner() == "codex" {
                "codex"
            } else {
                "claude"
            }
        );
    }

    #[test]
    fn a_pinned_codex_model_is_answered_by_the_codex_runner() {
        let daemon: Arc<dyn crate::execution::runner::CommandRunner> =
            Arc::new(FakeCommandRunner::default());
        let codex: Arc<dyn crate::execution::runner::CommandRunner> =
            Arc::new(FakeCommandRunner::default());
        let assistants = CliAssistants {
            claude: None,
            codex: Some(codex.clone()),
        };
        let config = turn_config();

        let (runner, cli) = runner_for_turn(&daemon, &assistants, &config, Some("gpt-5.5"));
        assert!(Arc::ptr_eq(&runner, &codex));
        assert_eq!(cli, "codex");
        assert_eq!(answering_cli(&config, Some("gpt-5.5")), "codex");

        let without_codex = CliAssistants {
            claude: None,
            codex: None,
        };
        let (runner, cli) = runner_for_turn(&daemon, &without_codex, &config, Some("gpt-5.5"));
        assert!(Arc::ptr_eq(&runner, &daemon));
        assert_eq!(cli, "codex");
    }

    #[test]
    fn an_unpinned_or_claude_pinned_turn_keeps_the_daemons_runner() {
        let daemon: Arc<dyn crate::execution::runner::CommandRunner> =
            Arc::new(FakeCommandRunner::default());
        let assistants = CliAssistants {
            claude: Some(Arc::new(FakeCommandRunner::default())),
            codex: Some(Arc::new(FakeCommandRunner::default())),
        };
        let config = turn_config();

        for pinned in [None, Some("sonnet"), Some("not-in-the-catalogue")] {
            let (runner, cli) = runner_for_turn(&daemon, &assistants, &config, pinned);
            assert!(Arc::ptr_eq(&runner, &daemon), "{pinned:?}");
            assert_eq!(cli, "claude", "{pinned:?}");
        }
    }

    #[test]
    fn a_codex_turn_never_keeps_a_live_process() {
        assert!(!may_keep_process(
            crate::execution::runner::ToolPolicy::Unrestricted,
            "codex"
        ));
        assert!(may_keep_process(
            crate::execution::runner::ToolPolicy::Unrestricted,
            "claude"
        ));
        assert!(!may_keep_process(
            crate::execution::runner::ToolPolicy::McpOnly,
            "claude"
        ));
    }

    #[test]
    fn only_an_explicit_telegram_origin_is_telegram() {
        assert_eq!(Origin::from_wire(Some("telegram")), Origin::Telegram);
        // Everything else is the shell, which is what makes this ship dark: a client that has not
        // been taught the field keeps the behaviour it has today.
        for value in [None, Some("shell"), Some("Telegram"), Some(""), Some("tg")] {
            assert_eq!(Origin::from_wire(value), Origin::Shell, "{value:?}");
        }
    }

    /// A queued message carries its origin through the database, so the browser panel's spelling
    /// must come back as the panel and not decay into the shell.
    #[test]
    fn browser_panel_origin_round_trips_through_the_wire() {
        assert_eq!(Origin::BrowserPanel.as_wire(), "browser-panel");
        assert_eq!(
            Origin::from_wire(Some("browser-panel")),
            Origin::BrowserPanel
        );
        assert_eq!(
            Origin::from_wire(Some(Origin::BrowserPanel.as_wire())),
            Origin::BrowserPanel
        );
    }

    /// Send now from the browser panel is recorded as the panel, not fixed to the shell.
    #[tokio::test]
    async fn say_now_from_records_the_browser_panel_origin_mid_turn() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("said-now-panel");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        *fake.delay.lock().unwrap() = Some(Duration::from_secs(2));
        let second = send_message(&state, &chat, "segundo", Origin::Shell)
            .await
            .unwrap();
        the_process_is_steerable(&chat).await;

        let said = say_now_from(&state, &chat, "e também isto", Origin::BrowserPanel).await;
        assert!(
            matches!(said, Ok(SaidNow::Injected)),
            "a live steerable turn takes the text"
        );

        let (run_id, origin): (i64, String) =
            sqlx::query_as("SELECT run_id, origin FROM chat_said_now WHERE chat_id = ?")
                .bind(&chat)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(run_id, second);
        assert_eq!(origin, Origin::BrowserPanel.as_wire());

        settled_turn(&state.pool, second).await;
        LIVE_CHATS.lock().unwrap().remove(&chat);
    }

    /// With no turn running the text is sent the ordinary way, as the panel.
    #[tokio::test]
    async fn say_now_from_with_no_running_turn_sends_with_the_browser_panel_origin() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("said-now-panel-idle");
        unrooted_chat(&state, &chat).await;

        let said = say_now_from(&state, &chat, "olá", Origin::BrowserPanel).await;

        let Ok(SaidNow::Sent(Sent::Turn(turn))) = said else {
            panic!("nothing was running, so the text becomes a turn: {said:?}");
        };
        let recorded: Option<String> = sqlx::query_scalar("SELECT origin FROM runs WHERE id = ?")
            .bind(turn)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(recorded.as_deref(), Some("browser-panel"));
        settled_turn(&state.pool, turn).await;
        LIVE_CHATS.lock().unwrap().remove(&chat);
    }

    /// The ship-dark guarantee, and the test most likely to be needed later: with no model
    /// configured, a Telegram turn is answered exactly as it was before any of this existed.
    #[tokio::test]
    async fn a_telegram_turn_uses_the_cli_when_no_local_model_is_configured() {
        let state = test_state().await;
        // Translated from `state.local_assistant.is_none()`: the ship-dark default is now the
        // factory refusing the local route, not a `None` singleton field — same meaning.
        assert!(
            state
                .assistants
                .assistant_for(crate::chats::Brain::Local, None)
                .is_err()
        );

        let id = send_message(&state, "tg-no-local", "hello", Origin::Telegram)
            .await
            .unwrap();
        let (status, reply) = settled_turn(&state.pool, id).await;
        assert_eq!(status, "completed");
        assert_eq!(
            reply.as_deref(),
            Some(CLI_FAKE_REPLY),
            "expected the CLI fake's reply, so this turn took the CLI path"
        );
    }

    #[tokio::test]
    async fn a_telegram_turn_is_answered_on_this_machine_when_a_model_is_configured() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant(
            "três corridas a andar",
        )));

        let id = send_message(&state, "tg-local", "o que está a correr?", Origin::Telegram)
            .await
            .unwrap();
        let (status, reply) = settled_turn(&state.pool, id).await;
        assert_eq!(status, "completed");
        assert_eq!(reply.as_deref(), Some("três corridas a andar"));
    }

    /// The other half of the routing rule. Configuring a local model must not quietly move the
    /// desktop app's chat onto it — the shell is where the long, tool-heavy conversations happen.
    #[tokio::test]
    async fn a_shell_turn_stays_on_the_cli_even_with_a_local_model_configured() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant("never asked")));

        let id = send_message(&state, "shell-with-local", "hello", Origin::Shell)
            .await
            .unwrap();
        let (_, reply) = settled_turn(&state.pool, id).await;
        assert_eq!(
            reply.as_deref(),
            Some(CLI_FAKE_REPLY),
            "a configured local model must not move the shell's chat onto it"
        );
    }

    async fn answered_by(pool: &SqlitePool, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT answered_by FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Written when the row is BORN, not when the turn ends: a turn that is cancelled still has to
    /// say who was answering it, and the transcript draws its memory cut from this column.
    #[tokio::test]
    async fn a_cloud_turn_records_that_the_cloud_answered_it() {
        let state = test_state().await;

        let id = send_message(&state, "who-answered", "hello", Origin::Shell)
            .await
            .unwrap();

        assert_eq!(answered_by(&state.pool, id).await.as_deref(), Some("cloud"));
    }

    /// The half that closes a hole predating this work: until now a Telegram turn answered on this
    /// machine was indistinguishable from a cloud one in the runs table.
    #[tokio::test]
    async fn a_local_turn_records_that_the_local_model_answered_it() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant("aqui mesmo")));

        let id = send_message(&state, "tg-who-answered", "olá", Origin::Telegram)
            .await
            .unwrap();

        assert_eq!(answered_by(&state.pool, id).await.as_deref(), Some("local"));
    }

    #[tokio::test]
    async fn a_chat_marked_local_is_answered_locally_even_from_the_shell() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant("na máquina")));
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Local, None)
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();

        assert_eq!(
            answered_by(&state.pool, turn).await.as_deref(),
            Some("local")
        );
    }

    #[tokio::test]
    async fn a_chat_marked_cloud_is_answered_in_the_cloud_even_from_telegram() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant("never asked")));
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Telegram)
            .await
            .unwrap();

        // The chat's own row wins over the sender. Anything else would make the app unable to say
        // "answer this one in the cloud" for a conversation it opened.
        assert_eq!(
            answered_by(&state.pool, turn).await.as_deref(),
            Some("cloud")
        );
    }

    /// The test that kills the placeholder.
    ///
    /// `send_message_with` carries a match arm, `(None, Some(crate::chats::Brain::OpenRouter)) =>
    /// false`, written as a placeholder because nothing could reach it yet — no API door and no
    /// picker write `openrouter` onto a chat row. This test is what CAN reach it: `chats::create`
    /// writes the row directly, exactly as `a_chat_marked_local_is_answered_locally_even_from_the_shell`
    /// does for `Brain::Local` a few tests up. With the placeholder still in place this turn falls
    /// through to the ordinary cloud path and SUCCEEDS — the exact hazard the placeholder's own
    /// comment names: a hosted conversation answered by the cloud CLI, silently, and on the bill.
    ///
    /// This is not spelled out here as a condition on `state.hosted_assistant` being `None` — `test_state()`
    /// just leaves it that way, the same as every field a given test does not care about. That is
    /// already enough: `main.rs` only ever builds `Some` there when a daemon was started with BOTH a
    /// `hosted_assistant_model` AND an OpenRouter key, and short of that every `openrouter` turn
    /// refuses, which is `NO_HOSTED_MODEL`'s own doc comment's point. Nothing before this brain
    /// existed could have chosen it, so there is no old promise to preserve by falling back.
    #[tokio::test]
    async fn a_chat_marked_openrouter_refuses_rather_than_billing_the_cloud() {
        let state = test_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::OpenRouter, None)
            .await
            .unwrap();

        let outcome = send_message(&state, &id, "olá", Origin::Shell).await;

        assert_eq!(outcome, Err(NO_HOSTED_MODEL.to_string()));
    }

    /// The guard on this whole change. No `chats` row anywhere is every Telegram conversation, and
    /// every conversation that predates the table — the old rule, unchanged.
    #[tokio::test]
    async fn a_conversation_with_no_row_routes_exactly_as_it_did_before() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant("na máquina")));
        let telegram_chat = a_chat("-100200300");

        let from_telegram = send_message(&state, &telegram_chat, "olá", Origin::Telegram)
            .await
            .unwrap();
        let from_shell = send_message(&state, "no-row-shell", "olá", Origin::Shell)
            .await
            .unwrap();

        assert_eq!(
            answered_by(&state.pool, from_telegram).await.as_deref(),
            Some("local")
        );
        assert_eq!(
            answered_by(&state.pool, from_shell).await.as_deref(),
            Some("cloud")
        );
    }

    #[tokio::test]
    async fn a_local_chat_with_no_local_model_refuses_instead_of_quietly_costing_money() {
        // No local model configured: `state.local_assistant` is None.
        let state = test_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Local, None)
            .await
            .unwrap();

        let outcome = send_message(&state, &id, "olá", Origin::Shell).await;

        assert!(
            outcome.is_err(),
            "a chat that says local must not fall through to the cloud"
        );
        // And it must not have spent anything trying: no row, no bill.
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(runs, 0);
    }

    /// The other side of the refusal above, and the reason it is stated on the CHAT and not on the
    /// origin: a Telegram conversation with no local model has always simply gone to the cloud, and
    /// has never claimed otherwise. Breaking that would take the bot off the air.
    #[tokio::test]
    async fn a_telegram_conversation_with_no_local_model_still_falls_through_to_the_cloud() {
        let state = test_state().await;

        let turn = send_message(&state, "-100200301", "olá", Origin::Telegram)
            .await
            .unwrap();

        assert_eq!(
            answered_by(&state.pool, turn).await.as_deref(),
            Some("cloud")
        );
    }

    #[tokio::test]
    async fn a_local_chat_turn_declares_as_its_own_run() {
        struct DeclaresOnce;
        #[async_trait::async_trait]
        impl crate::models::local_agent::LocalChat for DeclaresOnce {
            async fn exchange(
                &self,
                messages: Vec<serde_json::Value>,
                _tools: Option<Vec<serde_json::Value>>,
            ) -> std::io::Result<serde_json::Value> {
                if messages.iter().any(|message| message["role"] == "tool") {
                    return Ok(serde_json::json!({"role": "assistant", "content": "noted"}));
                }
                Ok(serde_json::json!({
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{"function": {
                        "name": "declare_refinement",
                        "arguments": {
                            "kind": "memory",
                            "title": "the daemon holds nucleos-core.exe",
                            "body": "stop it before building",
                            "reasoning": "a later run will hit it"
                        }
                    }}]
                }))
            }
        }

        let mut state = test_state().await;
        let door = axum::Router::new()
            .route(
                "/knowledge",
                axum::routing::post(crate::door::post_knowledge),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move {
            axum::serve(listener, door).await.unwrap();
        });
        let toolbox = crate::mcp_tools::LocalToolBox::new(
            format!("http://{address}"),
            "t".into(),
            state.pool.clone(),
        );
        state.assistants = Arc::new(FixedAssistants(Arc::new(
            crate::models::local_agent::LocalAssistant::new(
                Box::new(DeclaresOnce),
                Box::new(toolbox),
            ),
        )));

        let run_id = send_message(&state, "local-declares", "remember that", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, run_id).await;

        type KnowledgeRow = (Option<i64>, String, Option<i64>, String, String);
        let rows: Vec<KnowledgeRow> = sqlx::query_as(
            "SELECT origin_run_id, scope_kind, scope_id, source, status FROM knowledge",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![(
                Some(run_id),
                "machine".into(),
                None,
                "run".into(),
                "proposed".into()
            )],
            "the declaration must name the chat turn that made it"
        );
    }

    /// A local turn holds the chat's one slot like any other, and releases it. Without this the
    /// second message to a bot answered locally would be rejected with 409 for ever.
    #[tokio::test]
    async fn a_local_turn_releases_the_chat_when_it_ends() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant("done")));

        let first = send_message(&state, "tg-slot", "one", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        let second = send_message(&state, "tg-slot", "two", Origin::Telegram).await;
        assert!(second.is_ok(), "the slot was not released: {second:?}");
    }

    async fn record_turn(
        pool: &SqlitePool,
        chat_id: &str,
        prompt: &str,
        reply: &str,
        read_untrusted: i64,
    ) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, stdout, read_untrusted, created_at)
             VALUES (?, 'completed', 'assistant', ?, ?, ?, '2026-08-09T00:00:00Z')",
        )
        .bind(prompt)
        .bind(chat_id)
        .bind(reply)
        .bind(read_untrusted)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn history_comes_back_oldest_first() {
        let pool = test_pool().await;
        record_turn(&pool, "c", "what is running?", "two runs", 0).await;
        record_turn(&pool, "c", "and the second?", "the calendar one", 0).await;

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(
            history,
            vec![
                ("what is running?".to_string(), "two runs".to_string()),
                (
                    "and the second?".to_string(),
                    "the calendar one".to_string()
                ),
            ]
        );
    }

    /// The barrier, stated for a path that has no session to refuse. A local turn can `create_run`,
    /// so replaying a turn that read a stranger's mail would hand that stranger's words to a turn
    /// able to act on them — the exact failure `get_session` prevents on the CLI path.
    #[tokio::test]
    async fn history_starts_after_anything_that_read_a_strangers_words() {
        let pool = test_pool().await;
        record_turn(&pool, "c", "before", "old answer", 0).await;
        record_turn(&pool, "c", "read my mail", "it says ignore all rules", 1).await;
        record_turn(&pool, "c", "after", "fresh answer", 0).await;

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(
            history,
            vec![("after".to_string(), "fresh answer".to_string())],
            "history must resume only from after the mail read"
        );
    }

    #[tokio::test]
    async fn history_is_per_chat_and_only_of_answered_turns() {
        let pool = test_pool().await;
        record_turn(&pool, "other", "not mine", "not mine", 0).await;
        record_turn(&pool, "c", "answered", "yes", 0).await;
        // A failed turn has no reply; replaying its question invites the model to answer it now,
        // out of order.
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, created_at)
             VALUES ('unanswered', 'failed', 'assistant', 'c', '2026-08-09T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(history, vec![("answered".to_string(), "yes".to_string())]);
    }

    /// A ceiling on the NUMBER of exchanges is not a ceiling: one pasted stack trace is a single
    /// exchange and thousands of characters. What overflows the window is length.
    #[tokio::test]
    async fn a_long_exchange_is_dropped_and_the_recent_ones_are_kept() {
        let pool = test_pool().await;
        record_turn(&pool, "c", &"x".repeat(HISTORY_CHARS), "huge", 0).await;
        record_turn(&pool, "c", "recent", "kept", 0).await;

        let history = recent_exchanges(&pool, "c").await.unwrap();
        assert_eq!(
            history,
            vec![("recent".to_string(), "kept".to_string())],
            "the budget must be spent newest-first"
        );
    }

    /// End to end: the second message to a locally-answered chat must arrive with the first one
    /// behind it, or every follow-up is answered by a bot with no memory.
    #[tokio::test]
    async fn a_second_local_turn_is_given_the_first() {
        use std::sync::Mutex as StdMutex;

        struct Recorder(Arc<StdMutex<Vec<serde_json::Value>>>);
        #[async_trait::async_trait]
        impl crate::models::local_agent::LocalChat for Recorder {
            async fn exchange(
                &self,
                messages: Vec<serde_json::Value>,
                _tools: Option<Vec<serde_json::Value>>,
            ) -> std::io::Result<serde_json::Value> {
                *self.0.lock().unwrap() = messages;
                Ok(serde_json::json!({"role": "assistant", "content": "ok"}))
            }
        }
        struct NoTools;
        #[async_trait::async_trait]
        impl crate::models::local_agent::ToolBox for NoTools {
            fn schemas(&self) -> Vec<serde_json::Value> {
                Vec::new()
            }
            async fn call(
                &self,
                _: &str,
                _: &serde_json::Value,
            ) -> crate::models::local_agent::ToolAnswer {
                unreachable!()
            }
        }

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(Arc::new(
            crate::models::local_agent::LocalAssistant::new(
                Box::new(Recorder(seen.clone())),
                Box::new(NoTools),
            ),
        )));

        let first = send_message(&state, "tg-memory", "primeira", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        let second = send_message(&state, "tg-memory", "segunda", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        let messages = seen.lock().unwrap().clone();
        let contents: Vec<String> = messages
            .iter()
            .map(|m| m["content"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            contents,
            vec![
                crate::models::local_agent::SYSTEM_PROMPT.to_string(),
                "primeira".to_string(),
                "ok".to_string(),
                "segunda".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn unknown_chat_has_no_session() {
        let pool = test_pool().await;

        assert_eq!(get_session(&pool, "unknown").await.unwrap(), None);
    }

    #[tokio::test]
    async fn upsert_stores_session() {
        let pool = test_pool().await;

        upsert_session(&pool, "chat-1", "session-1", "2026-07-21T10:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, "chat-1").await.unwrap(),
            Some("session-1".to_string())
        );
    }

    #[tokio::test]
    async fn upsert_updates_existing_session() {
        let pool = test_pool().await;

        upsert_session(&pool, "chat-1", "session-1", "2026-07-21T10:00:00Z")
            .await
            .unwrap();
        upsert_session(&pool, "chat-1", "session-2", "2026-07-21T11:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, "chat-1").await.unwrap(),
            Some("session-2".to_string())
        );
    }

    /// The half of the barrier that has to survive the daemon dying.
    ///
    /// `hooks.rs` refuses to let a turn act after it has read third-party text, and that refusal is
    /// recorded against the RUN. A session outlives the run: `--resume` hands the next turn the same
    /// context, and the next turn's own row is clean, so a mail body refused once would simply be
    /// obeyed one message later. The check therefore lives on the read, where no cleanup code has to
    /// have run for it to hold — this test writes the rows directly for that reason, standing in for
    /// a turn that was cancelled, timed out, or killed with the daemon.
    #[tokio::test]
    async fn a_session_a_turn_read_mail_in_is_never_resumed() {
        let pool = test_pool().await;
        let chat_id = "assistant-untrusted-session-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, read_untrusted, created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-mail', 1, '2026-07-29T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-mail", "2026-07-29T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            None,
            "a session whose turn read a stranger's words must not be resumable"
        );
    }

    /// The contrast, so the test above cannot pass by refusing everything: an ordinary turn is what
    /// makes the bot conversational, and it keeps its session.
    #[tokio::test]
    async fn a_session_no_turn_read_mail_in_is_resumed_as_before() {
        let pool = test_pool().await;
        let chat_id = "assistant-clean-session-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-clean', '2026-07-29T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-clean", "2026-07-29T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            Some("sess-clean".to_string())
        );
    }

    /// A long conversation stays ONE conversation, and this is the test that used to say the
    /// opposite.
    ///
    /// It asserted that a session whose `context_fill` had passed 140k stopped being resumable, so
    /// the next message started clean. That was the fork the person on the other side could feel:
    /// the model lost the conversation while the transcript above it read as unbroken. Size is the
    /// CLI's business now — it is handed the window and compacts inside this same session — so a
    /// full context is a reason to summarise and never a reason to start again.
    ///
    /// Written as rows rather than driven through a turn, for the same reason the mail-read test
    /// above is: the condition lives on the READ, so whatever it says must hold for a session whose
    /// turn died without running any cleanup.
    #[tokio::test]
    async fn a_chat_whose_session_filled_the_context_is_still_resumed() {
        let pool = test_pool().await;
        let chat_id = "assistant-full-session-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, context_fill, created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-full', ?, '2026-08-08T00:00:00Z')",
        )
        .bind(LARGEST_WINDOW_TOKENS * 4)
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-full", "2026-08-08T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            Some("sess-full".to_string()),
            "a full context is compacted inside the session, never traded for a new one"
        );
    }

    /// The one rule that DOES still refuse a resume, kept apart from the one that no longer does.
    ///
    /// Cost and safety shared a `WHERE` and were never the same rule. Removing the size half must
    /// not quietly remove the other: a session that read third-party text stays unresumable however
    /// much room is left in it, because what it is carrying is somebody else's instructions.
    #[tokio::test]
    async fn a_full_session_that_read_untrusted_text_is_still_refused() {
        let pool = test_pool().await;
        let chat_id = "assistant-full-and-tainted-chat";

        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, context_fill, read_untrusted,
                               created_at)
             VALUES ('x', 'completed', 'assistant', 'sess-tainted', ?, 1, '2026-08-08T00:00:00Z')",
        )
        .bind(LARGEST_WINDOW_TOKENS * 4)
        .execute(&pool)
        .await
        .unwrap();
        upsert_session(&pool, chat_id, "sess-tainted", "2026-08-08T00:00:00Z")
            .await
            .unwrap();

        assert_eq!(
            get_session(&pool, chat_id).await.unwrap(),
            None,
            "the untrusted barrier is a safety property and does not lapse with the cost ceiling"
        );
    }

    /// The window a conversation runs in, and the two ways a row can ask for a silly one.
    ///
    /// Clamped rather than trusted, because this number is written by the pick-up path against a
    /// ceiling that can change between releases — and because the CLI clamps it again at its own
    /// end, so a number the window SHOWS that the CLI would not honour is a lie in the interface.
    #[test]
    fn a_conversation_runs_in_its_own_window_within_reason() {
        assert_eq!(
            window_of(None),
            CONTEXT_WINDOW_TOKENS,
            "the default is the default"
        );
        assert_eq!(
            window_of(Some(180_000)),
            180_000,
            "a picked-up session gets the room it needs"
        );
        assert_eq!(
            window_of(Some(20_000)),
            CONTEXT_WINDOW_TOKENS,
            "below the default is not an economy, it is a conversation that compacts every turn"
        );
        assert_eq!(
            window_of(Some(900_000)),
            LARGEST_WINDOW_TOKENS,
            "a window larger than the model's is a promise the model cannot keep"
        );
    }

    /// A conversation too large to resume is still continued, from what it was handed.
    ///
    /// The answer to a context that genuinely cannot be resumed has always been a verbatim tail.
    /// Its source was the turns of the chat itself — and a chat picked up from the editor has none,
    /// so a session too large to resume began knowing nothing at all. That is the original
    /// complaint with a new hat: the sessions appear, you continue one, and it has never heard of
    /// you. Rare now that only a session past every model's window reaches this, and no less wrong
    /// when it happens.
    ///
    /// The tail is read from the transcript once, when the session is picked up, and stored on the
    /// chat. This asserts the far end of that: what the model is actually handed.
    #[tokio::test]
    async fn a_picked_up_conversation_is_replayed_from_what_it_was_handed() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner::default());
        state.runner = runner.clone();
        let chat_id = "handover-chat";
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at, ide_session_id, handover)
             VALUES (?, 'cloud', '2026-08-19T10:00:00Z', 'aaaa-1111', ?)",
        )
        .bind(chat_id)
        .bind(r#"[["arranja o parser","arranjado, o mes vinha antes do dia"]]"#)
        .execute(&state.pool)
        .await
        .unwrap();

        let id = send_message(&state, chat_id, "e agora os testes", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        let prompt = runner
            .last_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("no prompt reached the runner");

        assert!(
            prompt.contains("arranjado, o mes vinha antes do dia"),
            "the editor's tail was not replayed: {prompt}"
        );
        assert!(
            prompt.contains("e agora os testes"),
            "the new message was lost: {prompt}"
        );
        // Framed as a replay, not as the conversation itself — the same frame every replay uses.
        assert!(prompt.contains("replay"), "{prompt}");
    }

    /// A finished turn records how full its context was.
    ///
    /// Found against the live daemon rather than here: a real turn came back with
    /// `context_fill: null`, and the reading the window draws from it was therefore drawn from a
    /// column this path never wrote. `runs.rs` observes the stream and stores it at its terminal
    /// write; an assistant turn has a terminal write of its own, and did not.
    ///
    /// It is what the meter under every turn reads, so a turn that does not record it is a
    /// conversation whose fullness cannot be seen coming.
    #[tokio::test]
    async fn a_finished_turn_records_how_full_its_context_was() {
        let mut state = test_state().await;
        let stream = format!(
            "{}\n{}\n",
            r#"{"type":"assistant","message":{"usage":{"input_tokens":9000,"cache_read_input_tokens":87000},"content":[{"type":"text","text":"pronto"}]}}"#,
            r#"{"type":"result","subtype":"success","result":"pronto"}"#,
        );
        state.runner = Arc::new(FakeCommandRunner {
            canned: Mutex::new(Some(crate::execution::runner::RunOutcome {
                exit_code: 0,
                stdout: stream,
                stderr: String::new(),
                session_id: Some("s".to_string()),
                cost_usd: Some(0.01),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            })),
            ..Default::default()
        });

        let id = send_message(&state, "fill-chat", "olá", Origin::Shell)
            .await
            .unwrap();
        let (status, _) = settled_turn(&state.pool, id).await;
        assert_eq!(status, "completed");

        let fill: Option<i64> = sqlx::query_scalar("SELECT context_fill FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();

        // Cache-read tokens occupy the window exactly as fresh input tokens do, which is the whole
        // reason this is not just `input_tokens`: a resumed conversation is nearly all cache.
        assert_eq!(fill, Some(96_000), "the turn recorded no context fill");
    }

    /// The columns a chat turn's terminal write reads off its outcome, and the numbers it hands in.
    type Measured = (
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );

    /// Runs one chat turn against `outcome` and reads back what its row recorded.
    async fn measured_turn(
        chat_id: &str,
        outcome: crate::execution::runner::RunOutcome,
    ) -> Measured {
        let mut state = test_state().await;
        state.runner = Arc::new(FakeCommandRunner {
            canned: Mutex::new(Some(outcome)),
            ..Default::default()
        });
        let id = send_message(&state, chat_id, "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;
        sqlx::query_as(
            "SELECT status, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                    num_turns FROM runs WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    fn measured_outcome(exit_code: i32, stdout: &str) -> crate::execution::runner::RunOutcome {
        crate::execution::runner::RunOutcome {
            exit_code,
            stdout: stdout.to_string(),
            stderr: String::new(),
            session_id: Some("s".to_string()),
            cost_usd: Some(0.01),
            input_tokens: Some(3),
            output_tokens: Some(40),
            cache_read_tokens: Some(900),
            cache_creation_tokens: Some(15),
            num_turns: Some(2),
            compacted: false,
        }
    }

    /// A finished turn records what it read and how many turns it took.
    ///
    /// Found by measurement: every chat turn read back `num_turns` and every token column NULL,
    /// while `runs.rs`, the council and the team all stored them. The runner had them; this write
    /// was the one that did not ask.
    #[tokio::test]
    async fn a_finished_turn_records_its_tokens_and_turns() {
        let stream = r#"{"type":"result","subtype":"success","result":"pronto"}"#;

        let row = measured_turn("tokens-chat", measured_outcome(0, stream)).await;

        assert_eq!(
            row,
            (
                "completed".to_string(),
                Some(3),
                Some(40),
                Some(900),
                Some(15),
                Some(2)
            )
        );
    }

    /// A turn that answered nothing still spent what it spent. The ceiling is the sharpest case: the
    /// turn stopped there read the most, and was the one whose row said the least.
    #[tokio::test]
    async fn a_turn_stopped_before_it_answered_still_records_what_it_read() {
        let stream =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a meio"}]}}"#;

        let row = measured_turn(
            "ceiling-chat",
            measured_outcome(crate::execution::runner::TURN_CEILING_EXIT_CODE, stream),
        )
        .await;

        assert_eq!(
            row,
            (
                "failed".to_string(),
                Some(3),
                Some(40),
                Some(900),
                Some(15),
                Some(2)
            )
        );
    }

    /// What a Telegram user actually received when the tool-policy barrier killed a turn: the CLI's
    /// own stream, `SessionStart` hook payload and all, delivered as though it were the answer.
    ///
    /// The shape below is the real one — three `system` events and no `result`, because the run was
    /// killed at `init`. The assertion that matters is the negative one: whatever the chat is shown,
    /// it must not be the stream. `status` carries the rest of the fix; the sidecar renders a
    /// `failed` turn as its stderr, which is where the CLI says which tools it objected to.
    #[tokio::test]
    async fn a_turn_with_no_result_event_fails_instead_of_replying_with_its_own_stream() {
        let mut state = test_state().await;
        let hook_body = r#"{"type":"system","subtype":"hook_response","hook_name":"SessionStart:resume","output":"You have superpowers. If you think there is even a 1% chance a skill might apply"}"#;
        let stream = format!(
            "{}\n{}\n{}\n",
            r#"{"type":"system","subtype":"hook_started","hook_name":"SessionStart:resume"}"#,
            hook_body,
            r#"{"type":"system","subtype":"init","session_id":"s","tools":["TaskCreate"]}"#,
        );
        state.runner = Arc::new(FakeCommandRunner {
            canned: Mutex::new(Some(crate::execution::runner::RunOutcome {
                exit_code: -1,
                stdout: stream.clone(),
                stderr: "nucleos: ToolPolicy::McpOnly violated by CLI-advertised tools: TaskCreate"
                    .to_string(),
                session_id: None,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            })),
            ..Default::default()
        });
        let chat_id = "assistant-no-result-event-chat";

        let id = send_message(
            &state,
            chat_id,
            "Le me o ultimo mail que recebi",
            Origin::Shell,
        )
        .await
        .unwrap();

        let mut row = None;
        for _ in 0..500 {
            let (status, stdout, stderr): (String, Option<String>, Option<String>) =
                sqlx::query_as("SELECT status, stdout, stderr FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            if status != "running" {
                row = Some((status, stdout, stderr));
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let (status, stdout, stderr) = row.expect("the turn must reach a terminal status");

        assert_eq!(
            status, "failed",
            "a turn that answered nothing is not a completed turn"
        );
        assert_eq!(
            stdout, None,
            "the reply column must stay empty rather than carry the stream: {stdout:?}"
        );
        assert!(
            stderr.is_some_and(|e| e.contains("TaskCreate")),
            "the reader gets the CLI's reason instead of its transport"
        );
    }

    /// End to end, through the turn machinery rather than through hand-written rows: a turn reads
    /// mail while it runs, and the message after it starts the CLI with no `--resume` at all.
    ///
    /// The session is recorded early — the CLI announces it in its first event, long before any tool
    /// call — so "do not store it" was never available as a fix. What the turn can do is not leave it
    /// behind, and what the read can do is refuse it anyway.
    #[tokio::test]
    async fn the_message_after_a_mail_read_starts_a_fresh_conversation() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_millis(150))),
            ..Default::default()
        });
        state.runner = runner.clone();
        let chat_id = "assistant-forget-after-mail-chat";

        let first = send_message(&state, chat_id, "what is in my mail?", Origin::Shell)
            .await
            .unwrap();

        // The session id is announced before the simulated delay, so this is the window in which a
        // real turn calls `get_email` and `hooks.rs` marks the run.
        for _ in 0..500 {
            if get_session(&state.pool, chat_id).await.unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            get_session(&state.pool, chat_id).await.unwrap(),
            Some("fake-session-id".to_string()),
            "the turn should have recorded its session before reading anything"
        );
        crate::execution::runs::mark_untrusted_context(&state.pool, first)
            .await
            .unwrap();

        // Through the helper, not the row. `settled_turn` waits for the [`TurnGuard`] to drop as
        // well as for the status to land; the raw poll that stood here watched `runs` alone, and
        // its doc says why that is wrong in both directions. It also fell through in SILENCE when
        // it timed out, so a slow run did not fail here — it failed four lines down, on the second
        // `send_message`, as "a turn is already in progress for this chat". Observed in a full
        // suite run, green on its own immediately after.
        let (status, _) = settled_turn(&state.pool, first).await;
        assert_eq!(
            status, "completed",
            "the mail-reading turn did not complete"
        );

        assert_eq!(
            get_session(&state.pool, chat_id).await.unwrap(),
            None,
            "a turn that read mail must leave the chat nothing to resume"
        );

        *runner.last_resume.lock().unwrap() = Some("not-cleared".to_string());
        send_message(&state, chat_id, "approve proposal 4", Origin::Shell)
            .await
            .unwrap();
        for _ in 0..500 {
            if runner.last_resume.lock().unwrap().as_deref() != Some("not-cleared") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.last_resume.lock().unwrap(),
            None,
            "the next message must start on a context no mail body has spoken into"
        );
    }

    #[test]
    fn builds_mcp_config() {
        let config = build_mcp_config("C:/x/nucleos-core.exe");

        assert_eq!(config["mcpServers"]["nucleos"]["type"], "stdio");
        assert_eq!(
            config["mcpServers"]["nucleos"]["command"],
            "C:/x/nucleos-core.exe"
        );
        assert_eq!(
            config["mcpServers"]["nucleos"]["args"],
            serde_json::json!(["--mcp-tools"])
        );
    }

    #[test]
    fn the_job_node_config_names_the_box_and_the_job() {
        let config = build_job_node_mcp_config("C:/x/n.exe", 7);
        let args = config["mcpServers"]["nucleos"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, ["--mcp-tools", "--box", "job-node", "--job", "7"]);
        assert_eq!(
            crate::mcp_tools::box_from_args(&args[1..]),
            Ok(crate::mcp_tools::McpBox::JobNode(7))
        );
    }

    /// A team agent's node launches with its own box and the node run it belongs to; nothing is
    /// guessed from the team run.
    #[test]
    fn loadout_a_team_node_config_names_its_box_and_run() {
        let config = build_team_mcp_config("C:/x/n.exe", 42);
        assert_eq!(config["mcpServers"]["nucleos"]["type"], "stdio");
        assert_eq!(config["mcpServers"]["nucleos"]["command"], "C:/x/n.exe");
        let args = config["mcpServers"]["nucleos"]["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, ["--mcp-tools", "--box", "team", "--run", "42"]);
        assert_eq!(
            crate::mcp_tools::box_from_args(&args[1..]),
            Ok(crate::mcp_tools::McpBox::Team(42))
        );
    }

    #[test]
    fn a_configuracao_mcp_e_escrita_por_inteiro() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        let long_path = "C:/um/caminho/deliberadamente/muito/comprido/para/nucleos-core.exe";
        let short_path = "C:/n.exe";

        write_mcp_config(&path, &build_mcp_config(long_path)).unwrap();
        write_mcp_config(&path, &build_mcp_config(short_path)).unwrap();

        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["nucleos"]["command"],
            serde_json::json!(short_path)
        );
    }

    #[test]
    fn only_one_turn_can_be_in_flight_per_chat() {
        let chat_id = "busy-test-chat";

        let slot = ChatSlot::acquire(chat_id).expect("the chat starts free");
        assert!(ChatSlot::acquire(chat_id).is_none());
        drop(slot);
        assert!(ChatSlot::acquire(chat_id).is_some());
    }

    /// The chat is marked busy synchronously, but the work that follows — reading the session,
    /// writing the MCP config, inserting the run row — spans awaits, and the Telegram sidecar's HTTP
    /// client can give up inside that span. A cancelled request drops this future exactly the way
    /// `abort()` drops a turn's, so a chat slot released only by a trailing statement is never
    /// released at all. That is the same 409-forever symptom as a cancelled turn, reached through a
    /// different door: the bot simply stops answering until the daemon restarts.
    #[tokio::test]
    async fn a_dropped_message_request_frees_the_chat() {
        use std::future::Future;

        let state = test_state().await;
        let chat_id = "assistant-dropped-request-chat";

        let mut request = Box::pin(send_message(&state, chat_id, "hello", Origin::Shell));

        // One poll is all it takes to claim the chat; the future then parks on the session lookup.
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            request.as_mut().poll(&mut context).is_pending(),
            "the request ran to completion before it could be dropped"
        );
        assert!(
            BUSY_CHATS.lock().unwrap().contains(chat_id),
            "the turn should have claimed the chat"
        );
        drop(request);

        assert!(
            !BUSY_CHATS.lock().unwrap().contains(chat_id),
            "an abandoned request must not leave the chat busy forever"
        );
    }

    #[tokio::test]
    async fn send_message_creates_assistant_run_and_upserts_session() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let chat_id = "assistant-send-test-chat";

        let id = send_message(&state, chat_id, "hello", Origin::Shell)
            .await
            .unwrap();
        let mode: String = sqlx::query_scalar("SELECT mode FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, "assistant");

        let mut status = String::new();
        let mut session = None;
        for _ in 0..50 {
            status = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
            if status == "completed" {
                session = get_session(&pool, chat_id).await.unwrap();
                if session.is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(status, "completed");
        assert_eq!(session, Some("fake-session-id".to_string()));
    }

    /// A run born from a relay must record which relay bore it, in the very row that creates it —
    /// not as a fact added afterwards. `relay::chain_of` (`relay.rs`) walks `runs.from_relay_id`
    /// back to `chat_relays.sending_run_id` and on, stopping the first time it meets a `NULL`,
    /// which it reads as "a person's own turn, nothing relayed yet". A relay-born run that reached
    /// this table without that column set would be indistinguishable from one — the cycle brake
    /// `relay::admits` enforces at write time would simply forget it ever ran, on exactly the runs
    /// it exists to bound.
    ///
    /// Checked immediately after the call returns, before the spawned turn has had any chance to
    /// run: `send_relayed_message` performs the INSERT synchronously, so this is the row the turn
    /// was CREATED with, not a state it might reach later.
    ///
    /// Pinned alongside the ordinary case in the same test, not a separate one, because the two are
    /// one property: a turn a person wrote must be as reliably `NULL` as a relayed one must be set —
    /// either half wrong and `chain_of`'s walk reads the wrong story about where a turn came from.
    #[tokio::test]
    async fn a_relayed_turn_records_which_relay_it_came_from() {
        let state = test_state().await;
        let pool = state.pool.clone();

        let relayed_id = send_relayed_message(
            &state,
            "relay-origin-destination-chat",
            "onward",
            Origin::Shell,
            7,
        )
        .await
        .unwrap();
        let from_relay: Option<i64> =
            sqlx::query_scalar("SELECT from_relay_id FROM runs WHERE id = ?")
                .bind(relayed_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            from_relay,
            Some(7),
            "a run born from a relay must record which relay bore it"
        );

        let ordinary_id = send_message(&state, "a-persons-own-chat", "hello", Origin::Shell)
            .await
            .unwrap();
        let from_relay: Option<i64> =
            sqlx::query_scalar("SELECT from_relay_id FROM runs WHERE id = ?")
                .bind(ordinary_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            from_relay, None,
            "a turn a person wrote must not read back as relay-born"
        );
    }

    /// A turn records which client sent it, on both paths that write a `runs` row.
    ///
    /// Written down rather than only acted on, which is what 0119 changed. `Origin` was a parameter
    /// that routed a turn and was then dropped, so `runs` could say a great deal about a turn and
    /// nothing about where it came from — and `relay::admit`'s `TelegramOrigin` brake, whose whole
    /// job is to read exactly that, had no fact to read. It was reachable only by a caller willing
    /// to state an origin it had no way to know, which is to say it was not reachable at all.
    ///
    /// Both paths in one test, because the column is only as good as its least careful writer: a
    /// relay is admitted on what `runs.origin` says about the SENDING turn, and a sending turn that
    /// happened to be answered locally would, with one path left unwired, come back NULL and be
    /// refused — or worse, be read as some default nobody wrote.
    #[tokio::test]
    async fn a_turn_records_which_client_sent_it() {
        let state = test_state().await;
        let pool = state.pool.clone();
        let telegram_chat = a_chat("-100200300");

        let from_telegram = send_message(&state, &telegram_chat, "olá", Origin::Telegram)
            .await
            .unwrap();
        let from_shell = send_message(&state, "a-shell-chat", "hello", Origin::Shell)
            .await
            .unwrap();

        let origin_of = |id: i64| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>("SELECT origin FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(origin_of(from_telegram).await.as_deref(), Some("telegram"));
        assert_eq!(origin_of(from_shell).await.as_deref(), Some("shell"));

        // The local path writes its own INSERT — see `spawn_local_turn` — so the column is wired
        // there separately or not at all, and "not at all" is a NULL that reads as a run whose
        // origin nobody knows.
        let local = AppState {
            assistants: Arc::new(FixedAssistants(fake_local_assistant("answered here"))),
            ..test_state().await
        };
        let local_chat = a_chat("a-local-chat");
        crate::chats::set_brain(&local.pool, &local_chat, crate::chats::Brain::Local)
            .await
            .unwrap();
        let locally = send_message(&local, &local_chat, "hello", Origin::Shell)
            .await
            .unwrap();
        let recorded: Option<String> = sqlx::query_scalar("SELECT origin FROM runs WHERE id = ?")
            .bind(locally)
            .fetch_one(&local.pool)
            .await
            .unwrap();
        assert_eq!(
            recorded.as_deref(),
            Some("shell"),
            "a turn answered by the local model records its origin too"
        );
    }

    /// Every run carries a daemon-assigned session id, and an assistant turn is a run. Its first
    /// turn had nothing to resume, so it was launched with neither `--resume` nor `--session-id`:
    /// the run had an id only if the CLI's stream volunteered one, so a first turn whose stream
    /// carried no `init` event — the case `runner.rs` already has a test for — was spend no
    /// conversation owned.
    ///
    /// Asserted on the row and on what the runner was handed, because either alone is satisfiable
    /// without the other: a row written and never passed to the CLI leaves the two disagreeing about
    /// which session the spend belongs to.
    #[tokio::test]
    async fn an_assistant_first_turn_is_assigned_a_session_id() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner {
            // Silent about its session, like a stream that never emits an init event. Whatever the
            // turn ends up carrying is therefore the daemon's own doing, not the CLI's.
            canned: Mutex::new(Some(crate::execution::runner::RunOutcome {
                exit_code: 0,
                stdout: "hello back".into(),
                stderr: String::new(),
                session_id: None,
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            })),
            ..Default::default()
        });
        state.runner = runner.clone();
        let pool = state.pool.clone();

        let id = send_message(&state, "assistant-first-turn-chat", "hello", Origin::Shell)
            .await
            .unwrap();

        let stored: Option<String> = sqlx::query_scalar("SELECT session_id FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let stored =
            stored.expect("a first turn's row must carry the session its spend is billed to");
        assert_eq!(
            stored.len(),
            36,
            "the assigned id must be a v4 uuid, which is what the CLI accepts: {stored}"
        );

        for _ in 0..500 {
            if runner.last_session_id.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.last_session_id.lock().unwrap(),
            Some(stored),
            "the CLI must be told the same session the row was written with"
        );
        assert_eq!(
            *runner.last_resume.lock().unwrap(),
            None,
            "a first turn has nothing to resume, so the id has to be assigned rather than inherited"
        );
    }

    /// The orchestrator is supposed to reach NucleOS and nothing else, and for a long time the
    /// only thing standing between a Telegram message and the filesystem was an `--allowedTools`
    /// line that does not restrict anything (measured against CLI 2.1.198: an allowlist grants,
    /// it never revokes). The restriction is the tool policy, so the policy is what is asserted.
    #[tokio::test]
    async fn a_turn_launches_the_cli_restricted_to_the_nucleos_server() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner::default());
        state.runner = runner.clone();

        send_message(&state, "assistant-tool-policy-chat", "hello", Origin::Shell)
            .await
            .unwrap();
        for _ in 0..500 {
            if runner.last_tool_policy.lock().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }

        assert_eq!(
            *runner.last_tool_policy.lock().unwrap(),
            Some(crate::execution::runner::ToolPolicy::McpOnly),
            "a Telegram turn must not be launched with the built-in tools available"
        );
    }

    /// A cancelled turn's future is dropped where it is parked, but `abort()` reaches it only at
    /// that drop — so a cancel that has already written `cancelled` can be followed by the turn
    /// waking up once more and writing its own `completed`, reply text and all, over a turn whose
    /// CLI was killed. The status a chat reports must be the first one written, not the last.
    #[tokio::test]
    async fn a_turn_completion_never_overwrites_a_finalised_status() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_secs(1))),
            ..Default::default()
        });
        state.runner = runner.clone();
        let chat_id = "assistant-finalised-status-chat";

        let id = send_message(&state, chat_id, "take your time", Origin::Shell)
            .await
            .unwrap();
        // The fake runner counts the call before it sleeps, so this parks the turn inside the CLI
        // call: past the point of no return for its terminal write, and short of running it.
        for _ in 0..500 {
            if *runner.calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert_eq!(
            *runner.calls.lock().unwrap(),
            1,
            "the turn should have started"
        );

        // What `finalize_termination` writes when it gets there first — written directly, because
        // aborting the task would drop the very future whose last write is the thing under test.
        sqlx::query("UPDATE runs SET status = 'cancelled', completed_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();

        // The handle is released by the guard the task captured, so an empty map is proof the turn
        // reached the end — its terminal write included — rather than proof that time passed.
        for _ in 0..500 {
            if !state.run_handles.lock().unwrap().contains_key(&id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !state.run_handles.lock().unwrap().contains_key(&id),
            "the turn never finished"
        );

        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status, "cancelled",
            "a killed turn must not report itself completed"
        );
    }

    #[test]
    fn a_hostile_chat_id_cannot_escape_the_temp_directory() {
        // `chat_id` arrives from a sidecar and is opaque to us. Interpolated raw it reached
        // `Path::join`, which DISCARDS the base when the joined component is absolute — so the
        // config landed on an arbitrary path, and `TurnGuard::drop` then deleted whatever was there.
        let temp = std::env::temp_dir();
        for chat_id in [
            "C:/Windows/System32/nucleos",
            r"C:\Windows\System32\nucleos",
            "../../../evil",
            r"..\..\..\evil",
            "/etc/passwd",
            r"\\server\share\evil",
        ] {
            let path = mcp_config_path(chat_id);
            assert_eq!(
                path.parent(),
                Some(temp.as_path()),
                "{chat_id:?} escaped the temp directory"
            );
        }
    }

    #[test]
    fn distinct_chats_never_share_a_config_path() {
        // Encoding rather than stripping, so the mapping stays injective: two chats that differ
        // only in an escaped character must not collide onto one file and clobber each other.
        assert_ne!(mcp_config_path("a/b"), mcp_config_path("a-b"));
        assert_ne!(mcp_config_path("a/b"), mcp_config_path(r"a\b"));
        assert_ne!(mcp_config_path("a%2fb"), mcp_config_path("a/b"));

        // The ordinary case stays readable rather than being hex soup: a real Telegram group id.
        // Asserted around the process id rather than over it — pinning the whole name would pin
        // this process's pid, which is a different number every run.
        let readable = mcp_config_path("-1001234567890");
        let readable = readable.to_string_lossy();
        assert!(readable.ends_with("--1001234567890.json"), "got {readable}");
        assert!(
            readable.contains(&format!("nucleos-mcp-{}-", std::process::id())),
            "the config has to be this process's, or two suites share one file: {readable}"
        );
    }

    #[tokio::test]
    async fn cancelling_a_turn_frees_the_chat_and_removes_its_mcp_config() {
        let mut state = test_state().await;
        // A slow runner keeps the turn parked on an await, which is where a real `/cancel` lands.
        let runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_secs(30))),
            ..Default::default()
        });
        state.runner = runner.clone();
        let chat_id = "assistant-cancel-test-chat";
        let mcp_path = mcp_config_path(chat_id);

        let id = send_message(&state, chat_id, "take your time", Origin::Shell)
            .await
            .unwrap();
        // Wait until the CLI is actually under way; cancelling a turn still queued would exercise a
        // different (and easier) path than the one a user hits.
        for _ in 0..50 {
            if *runner.calls.lock().unwrap() > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            *runner.calls.lock().unwrap(),
            1,
            "the turn should have started"
        );

        assert!(
            crate::execution::runs::finalize_termination(&state, id, "cancelled").await,
            "the turn should still have been in flight"
        );

        // `finalize_termination` aborts the task, which drops its future mid-await. Cleanup that
        // lives in trailing statements never runs — and a chat left in BUSY_CHATS rejects every
        // later message with 409 until the daemon restarts, which is indistinguishable from the bot
        // having died.
        for _ in 0..50 {
            if !mcp_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !mcp_path.exists(),
            "the temp mcp config should not outlive a cancelled turn"
        );
        assert!(
            ChatSlot::acquire(chat_id).is_some(),
            "a cancelled turn must free the chat for the next message"
        );
    }

    // ---- routing: a chat's row, then the origin -----------------------------------------------

    /// An `AppState` whose file root is a real directory, which the default empty root is not.
    fn with_files_root(state: AppState, root: std::path::PathBuf) -> AppState {
        AppState {
            files_root: Some(root),
            workflow_library: None,
            machine_config_root: None,
            secrets: std::sync::Arc::new(crate::secrets::InMemorySecrets::default()),
            ..state
        }
    }

    /// A state with a file root, the directory that root points at, and the fake runner still
    /// typed — returned together so the caller keeps the `TempDir` alive for as long as the state
    /// is used and can still read what the launch was handed.
    async fn files_root_state() -> (AppState, tempfile::TempDir, Arc<FakeCommandRunner>) {
        let dir = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeCommandRunner::default());
        let state = AppState {
            runner: runner.clone(),
            ..with_files_root(test_state().await, dir.path().to_path_buf())
        };
        (state, dir, runner)
    }

    /// A Telegram topic with no `chats` row is answered by its origin: on this machine. If this one
    /// ever fails, the routing of every group chat has changed.
    #[tokio::test]
    async fn a_telegram_topic_is_answered_by_origin() {
        let mut state = test_state().await;
        state.assistants = Arc::new(FixedAssistants(fake_local_assistant("na máquina")));

        let id = send_message(&state, "-100200300:5", "olá", Origin::Telegram)
            .await
            .unwrap();

        assert_eq!(answered_by(&state.pool, id).await.as_deref(), Some("local"));
    }

    /* ------------------------------------------ what a turn's prompt cost -- */

    /// Waits for the turn's own accounting of its prompt to land, and answers `None` if it never
    /// does.
    ///
    /// A poll rather than a single read, because the recording happens inside the task
    /// `spawn_assistant_turn` spawns and `send_message` returns the moment the row exists. `None` is
    /// distinguishable from a recorded zero on purpose — the column is nullable and NULL is a real
    /// answer there, so a test asking whether anything was recorded at all must be able to tell the
    /// two apart.
    async fn await_authored_chars(pool: &SqlitePool, id: i64) -> Option<i64> {
        for _ in 0..100 {
            let recorded: Option<i64> =
                sqlx::query_scalar("SELECT authored_prompt_chars FROM runs WHERE id = ?")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if recorded.is_some() {
                return recorded;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        None
    }

    /// A fake that stands in for the CLI runner on the one question these two tests ask: what the
    /// launch site recorded about the prompt it wrote. See `FakeCommandRunner::prices_its_prompt`.
    fn pricing_fake() -> Arc<FakeCommandRunner> {
        Arc::new(FakeCommandRunner {
            prices_its_prompt: true,
            ..Default::default()
        })
    }

    /// A chat turn records what it authored, and the schema block is in the number.
    ///
    /// **This is the launcher the measurement exists for.** `runs::spawn_run` records the same thing
    /// and sets no `mcp_config` at all, so every row it writes carries a schema cost of zero — a
    /// real zero, correctly recorded, and completely beside the point: the tool schemas are the
    /// largest thing this daemon puts in front of a model, and until this turn was wired they were
    /// measured, tested and stored against the only launcher that never pays for them.
    ///
    /// The assertion is arithmetic over values the test can name — the surface, asked of the same
    /// function that priced it, plus the prompt — rather than a literal. The surface moves whenever
    /// a tool's description changes, and a pinned byte count would be a test that fails on every
    /// honest edit while proving nothing about the wiring.
    #[tokio::test]
    async fn a_chat_turn_records_what_it_authored_including_the_schema_block() {
        let mut state = test_state().await;
        let runner = pricing_fake();
        state.runner = runner.clone();

        let id = send_message(&state, "authored-chat", "hello", Origin::Shell)
            .await
            .unwrap();
        let recorded = await_authored_chars(&state.pool, id)
            .await
            .expect("a chat turn must record what the daemon wrote into its prompt");

        assert!(
            runner.last_mcp_config.lock().unwrap().is_some(),
            "the premise of this test is that a chat turn IS offered a server — if that stops \
             being true, the figure below stops being about anything"
        );
        // Its server announces the whole tool list.
        let surface = crate::mcp_tools::NucleosTools::advertised_schema_chars(None) as i64;
        assert!(surface > 0, "the daemon's server announces nothing at all");
        assert_eq!(
            recorded,
            surface + "hello".len() as i64,
            "a chat turn's authored prompt is the schema block its server announces plus the \
             prompt itself; nothing else was set on this turn"
        );
    }

    /// A conversation keeps the config it has today, argument for argument.
    #[test]
    fn an_ordinary_chats_config_does_not_change() {
        let config = build_mcp_config("C:/x/nucleos-core.exe");
        assert_eq!(
            config["mcpServers"]["nucleos"]["args"],
            serde_json::json!(["--mcp-tools"])
        );
    }

    /// An ordinary chat has no folder to run in, and must keep getting no `cwd` at all — a turn
    /// silently given one would be a turn whose relative paths moved.
    #[tokio::test]
    async fn an_ordinary_chats_turn_still_runs_nowhere_in_particular() {
        let (state, _dir, runner) = files_root_state().await;

        let id = send_message(&state, "plain-chat", "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        assert_eq!(runner.last_cwd.lock().unwrap().clone(), None);
    }

    /// The whole change, end to end: what a conversation CHOSE reaches the launch.
    ///
    /// `cli_args` proves the flags are built out of a request, and the fake runner proves what this
    /// module puts in one. Between those two there was a gap wide enough for `model: None` to sit in
    /// for the entire life of the feature — the flag existed, the field existed, and the one kind of
    /// run a person actually watches was the one kind that could not reach either.
    #[tokio::test]
    async fn the_conversations_model_and_effort_reach_the_launch() {
        let (state, _dir, runner) = files_root_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        crate::chats::set_model(&state.pool, &id, Some("opus"))
            .await
            .unwrap();
        crate::chats::set_effort(&state.pool, &id, Some("xhigh"))
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        assert_eq!(
            runner.last_model.lock().unwrap().clone(),
            Some(Some("opus".to_string())),
            "the conversation's model never reached the launch"
        );
        assert_eq!(
            runner.last_effort.lock().unwrap().clone(),
            Some(Some("xhigh".to_string())),
            "the conversation's effort never reached the launch"
        );
    }

    /// RED: a local turn must ask the assistant factory for the model the CONVERSATION pinned
    /// (`chats.model`, read by `chats::answering`), not the route's own default. Fails today because
    /// the local call site in `send_message_with` deliberately passes `None` for the pin — GREEN's
    /// whole job is to replace that `None` with a read of `chats::answering`.
    #[tokio::test]
    async fn um_turno_local_pede_a_fabrica_o_modelo_que_a_conversa_fixou() {
        let mut state = test_state().await;
        let recording = Arc::new(RecordingAssistants::new(fake_local_assistant("na máquina")));
        state.assistants = recording.clone();
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Local, None)
            .await
            .unwrap();
        crate::chats::set_model(&state.pool, &id, Some("qwen3:8b"))
            .await
            .unwrap();

        send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();

        let calls = recording.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![(crate::chats::Brain::Local, Some("qwen3:8b".to_string()))],
            "the factory must be asked for the model this conversation pinned, not None"
        );
    }

    /// RED: the hosted route's own half of the same bug. Fails today for the same reason as
    /// `um_turno_local_pede_a_fabrica_o_modelo_que_a_conversa_fixou` — the hosted call site also
    /// passes `None` for the pin.
    #[tokio::test]
    async fn um_turno_alojado_pede_a_fabrica_o_modelo_que_a_conversa_fixou() {
        let mut state = test_state().await;
        let recording = Arc::new(RecordingAssistants::new(fake_local_assistant("no ar")));
        state.assistants = recording.clone();
        let id = crate::chats::create(&state.pool, crate::chats::Brain::OpenRouter, None)
            .await
            .unwrap();
        crate::chats::set_model(&state.pool, &id, Some("anthropic/claude-sonnet-4.5"))
            .await
            .unwrap();

        send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();

        let calls = recording.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![(
                crate::chats::Brain::OpenRouter,
                Some("anthropic/claude-sonnet-4.5".to_string())
            )],
            "the factory must be asked for the model this conversation pinned, not None"
        );
    }

    /// A GUARD, not a driver — this one PASSES today and must keep passing after GREEN. A chat with
    /// no pinned model must still reach the factory with `None`, so that GREEN's read of
    /// `chats::answering` cannot accidentally turn "no pin" into an empty string or a refusal: the
    /// route's own configured default has to keep answering an unpinned conversation exactly as it
    /// does today.
    #[tokio::test]
    async fn um_turno_sem_modelo_fixado_pede_a_fabrica_o_omissao_da_rota() {
        let mut state = test_state().await;
        let recording = Arc::new(RecordingAssistants::new(fake_local_assistant(
            "sem fixação",
        )));
        state.assistants = recording.clone();
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Local, None)
            .await
            .unwrap();

        send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();

        let calls = recording.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![(crate::chats::Brain::Local, None)],
            "an unpinned conversation must still reach the factory, asking for the route's own \
             configured default"
        );
    }

    /// The other three of 0111, end to end: what a conversation was told reaches the launch.
    #[tokio::test]
    async fn a_conversations_reach_and_ceiling_reach_the_launch() {
        let (state, dir, runner) = files_root_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        let extra = dir.path().join("beside");
        std::fs::create_dir_all(&extra).unwrap();
        crate::chats::set_fallback(
            &state.pool,
            &id,
            &["opus".to_string(), "sonnet".to_string()],
        )
        .await
        .unwrap();
        crate::chats::set_extra_dirs(&state.pool, &id, &[extra.to_string_lossy().into_owned()])
            .await
            .unwrap();
        crate::chats::set_turn_budget(&state.pool, &id, Some(0.25))
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        assert_eq!(
            runner.last_fallback_model.lock().unwrap().clone(),
            Some(vec!["opus".to_string(), "sonnet".to_string()])
        );
        assert_eq!(
            runner.last_add_dirs.lock().unwrap().clone(),
            Some(vec![extra])
        );
        assert_eq!(
            runner.last_max_budget_usd.lock().unwrap().clone(),
            Some(Some(0.25))
        );
    }

    /// The helpers a conversation defined reach the launch, with their names intact.
    ///
    /// The name is the thing worth asserting here: it is stored as the object's KEY, dropped from
    /// the value on the way to the flag, and put back on the way out. Three places to lose it, and
    /// a helper that arrives anonymous is one the model cannot delegate to.
    #[tokio::test]
    async fn the_helpers_a_conversation_defined_reach_the_launch() {
        let (state, _dir, runner) = files_root_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        crate::chats::set_agents(
            &state.pool,
            &id,
            &[crate::execution::runner::Subagent {
                name: "reviewer".to_string(),
                description: "Reviews code".to_string(),
                prompt: "You are a code reviewer".to_string(),
                tools: None,
                model: Some("opus".to_string()),
                effort: None,
            }],
        )
        .await
        .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        let sent = runner.last_agents.lock().unwrap().clone().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].name, "reviewer");
        assert_eq!(sent[0].description, "Reviews code");
        assert_eq!(sent[0].model.as_deref(), Some("opus"));
    }

    /// What a conversation was told about itself reaches the launch: its instructions, what it may
    /// not reach for, and what to call its session.
    #[tokio::test]
    async fn a_conversations_instructions_and_denials_reach_the_launch() {
        let (state, _dir, runner) = files_root_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        crate::chats::set_system_prompt(&state.pool, &id, Some("Answer in Portuguese."))
            .await
            .unwrap();
        crate::chats::set_denied_tools(&state.pool, &id, &["Bash".to_string()])
            .await
            .unwrap();
        crate::chats::rename(&state.pool, &id, Some("o refactor do runner"))
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        assert_eq!(
            runner.last_append_system_prompt.lock().unwrap().clone(),
            Some(Some("Answer in Portuguese.".to_string()))
        );
        assert_eq!(
            runner.last_denied_tools.lock().unwrap().clone(),
            Some(vec!["Bash".to_string()])
        );
        // Cosmetic, and the reason it is carried at all: without it every session this daemon mints
        // is a nameless timestamp in the CLI's own `--resume` picker.
        assert_eq!(
            runner.last_session_name.lock().unwrap().clone(),
            Some(Some("o refactor do runner".to_string()))
        );
    }

    /// A state carrying the configured Telegram doctrine, with the fake runner still typed — the
    /// same shape `files_root_state` returns, minus the file root neither new test below needs.
    async fn state_with_doctrine(doctrine: Option<&str>) -> (AppState, Arc<FakeCommandRunner>) {
        let runner = Arc::new(FakeCommandRunner::default());
        let state = AppState {
            runner: runner.clone(),
            telegram_doctrine: doctrine.map(str::to_string),
            ..test_state().await
        };
        (state, runner)
    }

    #[tokio::test]
    async fn a_claude_turn_is_told_what_the_house_knows_and_leaves_no_trace() {
        let (state, runner) = state_with_doctrine(None).await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'machine', NULL, 'owner', 'memory',
                     'zanzibar house rule', 'body', 'active',
                     '2026-08-19T00:00:00+00:00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();

        let turn = send_message(&state, &id, "zanzibar?", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        let sent = runner.last_append_system_prompt.lock().unwrap().clone();
        assert!(matches!(
            sent,
            Some(Some(ref prompt)) if prompt.contains("zanzibar house rule")
        ));
        let traces: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_knowledge")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(traces, 0);
    }

    #[tokio::test]
    async fn a_conversations_own_instructions_come_first_and_the_knowledge_after() {
        let (state, runner) = state_with_doctrine(None).await;
        sqlx::query(
            "INSERT INTO knowledge
               (layer, scope_kind, scope_id, source, kind, title, body, status, created_at)
             VALUES ('semantic', 'machine', NULL, 'owner', 'memory',
                     'zanzibar house rule', 'body', 'active',
                     '2026-08-19T00:00:00+00:00')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        crate::chats::set_system_prompt(&state.pool, &id, Some("Answer in Portuguese."))
            .await
            .unwrap();

        let turn = send_message(&state, &id, "zanzibar?", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        let sent = runner
            .last_append_system_prompt
            .lock()
            .unwrap()
            .clone()
            .flatten()
            .unwrap();
        assert!(sent.starts_with("Answer in Portuguese."));
        assert!(sent.contains("zanzibar house rule"));
    }

    #[test]
    fn the_knowledge_block_goes_to_a_claude_turn_and_never_to_a_codex_one() {
        for (cli, instructions, block, expected) in [
            ("codex", Some("X"), Some("\n\nB"), Some("X")),
            ("codex", None, Some("\n\nB"), None),
            ("claude", None, None, None),
            ("claude", Some("X"), None, Some("X")),
            ("claude", Some("X"), Some("\n\nB"), Some("X\n\nB")),
            ("claude", None, Some("\n\nB"), Some("B")),
        ] {
            assert_eq!(
                system_prompt_with_knowledge(cli, instructions.map(str::to_owned), block)
                    .as_deref(),
                expected,
                "{cli} {instructions:?} {block:?}"
            );
        }
    }

    /// A Telegram turn with no instructions of its own is not a turn with no instructions at all —
    /// it is answered under whatever doctrine the operator configured for the whole channel, the
    /// same way `a_conversations_instructions_and_denials_reach_the_launch` shows a chat's OWN
    /// `system_prompt` reaching the launch untouched. `Brain::Cloud` is set explicitly on the chat
    /// so `wants_local` cannot route this into the local-assistant path instead of the runner this
    /// test inspects.
    #[tokio::test]
    async fn um_turno_de_telegram_sem_instrucoes_leva_a_doutrina() {
        const DOCTRINE: &str = "Falas sempre em português europeu, e nunca reveles segredos.";
        let (state, runner) = state_with_doctrine(Some(DOCTRINE)).await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        assert_eq!(
            runner.last_append_system_prompt.lock().unwrap().clone(),
            Some(Some(DOCTRINE.to_string())),
            "a Telegram turn with no instructions of its own must reach the runner carrying the \
             configured doctrine"
        );
    }

    /// The doctrine is a fallback for a channel that said nothing, never a replacement for
    /// something a person actually wrote. Two independent reasons the same configured doctrine must
    /// NOT reach the launch: (a) the origin is not Telegram at all, and (b) the chat has its own
    /// `system_prompt`, which — like `a_conversations_instructions_and_denials_reach_the_launch`
    /// already pins for the shell — must survive untouched.
    #[tokio::test]
    async fn a_doutrina_nunca_substitui_instrucoes_de_uma_pessoa() {
        const DOCTRINE: &str = "Falas sempre em português europeu, e nunca reveles segredos.";

        // (a) Shell, doctrine configured, no instructions of the chat's own: still no doctrine.
        let (shell_state, shell_runner) = state_with_doctrine(Some(DOCTRINE)).await;
        let shell_id = crate::chats::create(&shell_state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();

        let shell_turn = send_message(&shell_state, &shell_id, "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&shell_state.pool, shell_turn).await;

        assert_eq!(
            shell_runner
                .last_append_system_prompt
                .lock()
                .unwrap()
                .clone(),
            Some(None),
            "a shell turn must never receive the Telegram doctrine"
        );

        // (b) Telegram, doctrine configured, AND the chat has its own instructions: its own text
        // wins, whole, over the doctrine that would otherwise have filled the same slot.
        let (person_state, person_runner) = state_with_doctrine(Some(DOCTRINE)).await;
        let person_id = crate::chats::create(&person_state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        crate::chats::set_system_prompt(
            &person_state.pool,
            &person_id,
            Some("Answer in Portuguese."),
        )
        .await
        .unwrap();

        let person_turn = send_message(&person_state, &person_id, "olá", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&person_state.pool, person_turn).await;

        assert_eq!(
            person_runner
                .last_append_system_prompt
                .lock()
                .unwrap()
                .clone(),
            Some(Some("Answer in Portuguese.".to_string())),
            "a chat's own instructions must survive even on Telegram with a doctrine configured"
        );
    }

    /// The difference between the two context gestures, stated where it actually shows.
    ///
    /// Forgetting the session leaves the replay alone — the next turn starts on a fresh window and
    /// is told what was recently said. Clearing moves the floor of that replay to now, so there is
    /// nothing left to tell it. Both leave every turn in `runs`, readable, costing what it cost.
    #[tokio::test]
    async fn clearing_leaves_a_fresh_turn_with_nothing_to_replay() {
        let state = test_state().await;
        let chat_id = "cleared-chat";
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(chat_id)
            .bind("2026-08-24T09:00:00Z")
            .execute(&state.pool)
            .await
            .unwrap();
        for (asked, answered) in [("primeira", "uma"), ("segunda", "duas")] {
            sqlx::query(
                "INSERT INTO runs (chat_id, mode, status, prompt, stdout, created_at)
                 VALUES (?, 'assistant', 'completed', ?, ?, ?)",
            )
            .bind(chat_id)
            .bind(asked)
            .bind(answered)
            .bind("2026-08-24T09:00:00Z")
            .execute(&state.pool)
            .await
            .unwrap();
        }

        // Before: a fresh turn would be handed both exchanges.
        assert_eq!(
            recent_exchanges(&state.pool, chat_id).await.unwrap().len(),
            2
        );

        crate::chats::clear_context(&state.pool, chat_id)
            .await
            .unwrap();

        assert!(
            recent_exchanges(&state.pool, chat_id)
                .await
                .unwrap()
                .is_empty(),
            "a cleared conversation still had something to replay"
        );
        // And the turns are still there. Clearing decides what the MODEL is shown, not what
        // happened — the window draws all of it either way.
        let still: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE chat_id = ? AND mode = 'assistant'",
        )
        .bind(chat_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(still, 2);
    }

    /// The tail an editor session was picked up with is older than every turn here, so a cut
    /// anywhere in the conversation is a cut above it. Without this, a clear would leave the one
    /// piece of history it was most obviously asked to drop.
    #[tokio::test]
    async fn clearing_also_drops_the_tail_the_conversation_was_picked_up_with() {
        let state = test_state().await;
        let chat_id = "cleared-pickup";
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at, ide_session_id, handover)
             VALUES (?, 'cloud', ?, 'sess-1', ?)",
        )
        .bind(chat_id)
        .bind("2026-08-24T09:00:00Z")
        .bind(r#"[["o que fizemos?","isto e aquilo"]]"#)
        .execute(&state.pool)
        .await
        .unwrap();

        assert_eq!(handed_over(&state.pool, chat_id).await.len(), 1);

        crate::chats::clear_context(&state.pool, chat_id)
            .await
            .unwrap();

        assert!(handed_over(&state.pool, chat_id).await.is_empty());
    }

    /// And a conversation that chose nothing overrides nothing, which leaves the runner's configured
    /// model and the CLI's own effort exactly where they were. That is what every chat in this
    /// daemon did before these columns existed, and an upgrade must not change it.
    #[tokio::test]
    async fn a_conversation_that_chose_nothing_overrides_nothing() {
        let (state, _dir, runner) = files_root_state().await;
        let id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();

        let turn = send_message(&state, &id, "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, turn).await;

        assert_eq!(runner.last_model.lock().unwrap().clone(), Some(None));
        assert_eq!(runner.last_effort.lock().unwrap().clone(), Some(None));
    }

    /// An ordinary chat still answers with the emergency stop engaged. The kill switch is about
    /// what runs on its own, not about whether the owner may talk to their own bot — and a stop that
    /// also takes the chat off the air is a stop nobody will engage.
    #[tokio::test]
    async fn the_kill_switch_does_not_silence_an_ordinary_chat() {
        let state = test_state().await;
        crate::governance::autopilot::set_kill_switch(&state.pool, true)
            .await
            .unwrap();

        let id = send_message(&state, "conversa", "olá", Origin::Shell)
            .await
            .unwrap();

        assert_eq!(settled_turn(&state.pool, id).await.0, "completed");
    }

    /// Every combination, because the rule's whole value is that the conditions are AND-ed: stated
    /// as separate tests, a change that dropped one of them would leave the others green.
    ///
    /// The table below is the ordinary-turn half — a message somebody typed — and every row of it
    /// passes `false` for the fourth condition. The relayed half is
    /// `a_relayed_turn_never_gets_the_tools_however_rooted_the_conversation_is`, below, which is
    /// where that condition is actually exercised.
    #[test]
    fn only_a_rooted_conversation_spoken_to_from_the_machine_with_a_wired_hook_gets_the_tools() {
        use crate::execution::runner::ToolPolicy;
        let root = Some("C:/Projects/nucleos");

        for (cwd, origin, wired, expected, why) in [
            (
                root,
                Origin::Shell,
                true,
                ToolPolicy::Unrestricted,
                "all three",
            ),
            (
                root,
                Origin::Shell,
                false,
                ToolPolicy::McpOnly,
                "no hook watching",
            ),
            (
                root,
                Origin::Telegram,
                true,
                ToolPolicy::McpOnly,
                "over the network",
            ),
            (
                root,
                Origin::Telegram,
                false,
                ToolPolicy::McpOnly,
                "neither",
            ),
            (None, Origin::Shell, true, ToolPolicy::McpOnly, "no root"),
            (
                None,
                Origin::Shell,
                false,
                ToolPolicy::McpOnly,
                "no root, no hook",
            ),
            (
                None,
                Origin::Telegram,
                true,
                ToolPolicy::McpOnly,
                "no root, over the network",
            ),
            (
                None,
                Origin::Telegram,
                false,
                ToolPolicy::McpOnly,
                "nothing at all",
            ),
        ] {
            assert_eq!(
                tool_policy_for(cwd, origin, wired, crate::relay::Handover::Typed),
                expected,
                "{why}: cwd={cwd:?} origin={origin:?} wired={wired}"
            );
        }
    }

    /// A turn another conversation handed over keeps the CLI's tools only as the first clean hop
    /// out of a typed turn — the owner's answer to the relay design's §13 (2026-10-11).
    ///
    /// It used to be refused outright, and the history is worth keeping: a relay is DELIVERED with
    /// the sender's origin, so before any rule existed a relay into a rooted conversation with a
    /// wired hook collected the whole tool surface, and nobody had chosen that. The owner has now
    /// chosen delegation — a conversation in repository X working there for another — behind three
    /// brakes. Two of them live in `Handover`, asserted here; the third, the spend ceiling of a
    /// chain, refuses the relay itself and is `relay::chain_has_room`'s test.
    ///
    /// `Withheld` — deeper than one hop, or from a sender that read untrusted text — is refused on
    /// every row: NO combination of the other three conditions rescues it.
    #[test]
    fn only_the_first_clean_hop_of_a_relay_keeps_the_tools() {
        for cwd in [None, Some("C:/repo")] {
            for origin in [Origin::Shell, Origin::Telegram] {
                for wired in [true, false] {
                    assert_eq!(
                        tool_policy_for(cwd, origin, wired, crate::relay::Handover::Withheld),
                        crate::execution::runner::ToolPolicy::McpOnly,
                        "withheld: cwd={cwd:?} origin={origin:?} wired={wired}"
                    );
                    assert_eq!(
                        tool_policy_for(cwd, origin, wired, crate::relay::Handover::FirstHop),
                        tool_policy_for(cwd, origin, wired, crate::relay::Handover::Typed),
                        "a first hop diverged from a typed turn: cwd={cwd:?} origin={origin:?} wired={wired}"
                    );
                }
            }
        }

        // The one row where the difference shows, stated on its own so it is legible without the
        // loop above.
        assert_eq!(
            tool_policy_for(
                Some("C:/repo"),
                Origin::Shell,
                true,
                crate::relay::Handover::FirstHop
            ),
            crate::execution::runner::ToolPolicy::Unrestricted
        );
        assert_eq!(
            tool_policy_for(
                Some("C:/repo"),
                Origin::Shell,
                true,
                crate::relay::Handover::Withheld
            ),
            crate::execution::runner::ToolPolicy::McpOnly
        );
    }

    /// The conversations that exist today have no root, and this is the line that says so out loud:
    /// whatever else changes here, none of them may pick up the filesystem by accident.
    #[test]
    fn a_conversation_with_no_directory_keeps_exactly_the_policy_it_always_had() {
        for origin in [Origin::Shell, Origin::Telegram, Origin::Voice] {
            for wired in [true, false] {
                assert_eq!(
                    tool_policy_for(None, origin, wired, crate::relay::Handover::Typed),
                    crate::execution::runner::ToolPolicy::McpOnly
                );
            }
        }
    }

    /// **A question does not get a weaker policy for having been spoken.**
    ///
    /// The security decision of the voice-conversation design, written as the test that fails if
    /// somebody reverts it. `Origin::Voice` is on the `Unrestricted` arm because the fact that arm
    /// turns on is physical presence, and a microphone is a stricter proof of it than a keyboard: a
    /// spoken turn requires being in the room, a typed one only requires reaching the machine.
    ///
    /// The failure this prevents is silent and would be very hard to recognise. Drop `Origin::Voice`
    /// from the arm and nothing errors — voice turns simply fall to `McpOnly`, so the SAME question
    /// that works when typed answers "I cannot do that" when spoken, in a directory that is onboarded
    /// and with the hook wired. Nothing on screen would connect that to the microphone.
    #[test]
    fn a_spoken_turn_is_trusted_exactly_as_much_as_a_typed_one() {
        for wired in [true, false] {
            assert_eq!(
                tool_policy_for(
                    Some("C:/Projects/nucleos"),
                    Origin::Voice,
                    wired,
                    crate::relay::Handover::Typed
                ),
                tool_policy_for(
                    Some("C:/Projects/nucleos"),
                    Origin::Shell,
                    wired,
                    crate::relay::Handover::Typed
                ),
                "a spoken turn diverged from a typed one at wired={wired}"
            );
        }
        // And the direction is the permissive one, so this cannot pass by both being McpOnly.
        assert_eq!(
            tool_policy_for(
                Some("C:/Projects/nucleos"),
                Origin::Voice,
                true,
                crate::relay::Handover::Typed
            ),
            crate::execution::runner::ToolPolicy::Unrestricted
        );
    }

    /// Voice does not lift Telegram along with it.
    ///
    /// The arm names two origins now, and a third could be added to it by a careless edit — so the
    /// thing that must stay true is stated separately: what keeps a message that crossed the network
    /// away from this machine's shell is the policy, not the MCP allowlist.
    #[test]
    fn a_message_over_the_network_is_still_kept_off_the_machine() {
        assert_eq!(
            tool_policy_for(
                Some("C:/Projects/nucleos"),
                Origin::Telegram,
                true,
                crate::relay::Handover::Typed
            ),
            crate::execution::runner::ToolPolicy::McpOnly
        );
    }

    /// A session the daemon never started has no rows in `runs`, so both of `get_session`'s
    /// `NOT EXISTS` guards pass for want of anything to refuse — and the first turn resumes it.
    ///
    /// This is the behaviour the owner asked for, and it is what the code already did by accident.
    /// The test is here to make it a decision: anyone tightening either guard sees this fail and
    /// learns that continuing an IDE conversation depends on it.
    #[tokio::test]
    async fn a_session_the_daemon_never_ran_is_resumable_because_there_is_nothing_against_it() {
        let state = test_state().await;
        let chat_id = crate::chats::create(
            &state.pool,
            crate::chats::Brain::Cloud,
            Some(&crate::sessions::had_in("C:/x", "had-in-the-ide")),
        )
        .await
        .unwrap();
        upsert_session(
            &state.pool,
            &chat_id,
            "had-in-the-ide",
            &chrono::Utc::now().to_rfc3339(),
        )
        .await
        .unwrap();

        assert_eq!(
            get_session(&state.pool, &chat_id).await.unwrap().as_deref(),
            Some("had-in-the-ide"),
        );
    }

    /// And the directory travels with it. The CLI keys its transcripts by the directory a session
    /// was had in, so a turn launched from anywhere else does not fail — it quietly starts a new
    /// session, and the window goes on showing a conversation that is no longer being continued.
    #[tokio::test]
    async fn a_continued_conversation_remembers_where_it_is_to_be_resumed_from() {
        let state = test_state().await;
        let chat_id = crate::chats::create(
            &state.pool,
            crate::chats::Brain::Cloud,
            Some(&crate::sessions::had_in(
                "C:/Projects/nucleos-canvas",
                "aaaa-1111",
            )),
        )
        .await
        .unwrap();

        assert_eq!(
            crate::chats::cwd_of(&state.pool, &chat_id).await.unwrap(),
            Some("C:/Projects/nucleos-canvas".to_string()),
        );
    }

    /// The one line that decides whether picking up a conversation gives you an agent or a
    /// pen-friend — and the two halves of it, asserted together.
    ///
    /// A session had in a directory with no classifier hook continues on the MCP server alone: no
    /// `Read`, no `Edit`, no `Bash`. That is not a bug, it is the barrier working — but it is also
    /// why continuing a coding conversation could feel like nothing happened, and why the window
    /// has to be able to fix it rather than only report it.
    #[test]
    fn wiring_a_project_is_what_turns_a_continued_conversation_from_talk_into_tools() {
        let root = tempfile::TempDir::new().unwrap();
        let dir = root.path().to_str().unwrap();

        assert_eq!(
            tool_policy_for(
                Some(dir),
                Origin::Shell,
                crate::governance::autopilot::classifier_hook_is_wired(root.path()),
                crate::relay::Handover::Typed
            ),
            crate::execution::runner::ToolPolicy::McpOnly,
        );

        crate::governance::autopilot::wire_classifier_hook(root.path()).unwrap();

        assert_eq!(
            tool_policy_for(
                Some(dir),
                Origin::Shell,
                crate::governance::autopilot::classifier_hook_is_wired(root.path()),
                crate::relay::Handover::Typed
            ),
            crate::execution::runner::ToolPolicy::Unrestricted,
        );
    }

    /// A turn's stream is published WHILE it is being written, so the window can show the work
    /// instead of a spinner.
    ///
    /// The buffer is taken out of the map while the turn is in flight and read again after it ends:
    /// that is what proves the published handle is the one the runner writes into, rather than an
    /// empty one registered beside it. Registering the wrong buffer would look identical from
    /// outside — a tail that answers, and never says anything.
    #[tokio::test]
    async fn a_turn_in_flight_publishes_its_stream_so_the_window_can_watch() {
        let mut state = test_state().await;
        state.runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_millis(150))),
            ..Default::default()
        });

        let id = send_message(&state, "assistant-watching-chat", "olá", Origin::Shell)
            .await
            .unwrap();

        let mut published = None;
        for _ in 0..500 {
            let found = state.run_tails.lock().unwrap().get(&id).cloned();
            if let Some(buffer) = found {
                published = Some(buffer);
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let published = published.expect("a turn in flight published no stream to watch");

        settled_turn(&state.pool, id).await;

        // Taken out when the turn ends. A tail left behind is the run's whole output held in memory
        // until the daemon restarts, which is why `Registration` removes it rather than this code.
        //
        // Waited for rather than asserted outright: the row leaves `running` from INSIDE the task,
        // and the registration is dropped when that task ends — a moment later. Asserting on the
        // row's timing would be asserting on a race this does not care about.
        let mut gone = false;
        for _ in 0..500 {
            if state.run_tails.lock().unwrap().get(&id).is_none() {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(gone, "the tail outlived the turn");
        let written = published.lock().unwrap().clone();
        assert!(written.contains("fake output"), "{written:?}");
    }

    /// The turn asks the CLI to stream its message as it is written, not only when it is finished.
    ///
    /// Without this the stream carries whole blocks, and a conversation shows nothing for as long as
    /// the model takes to write a paragraph — which is exactly the wait the tail above exists to
    /// fill. The flag costs nothing when nobody is watching: it adds events to a stream the daemon
    /// was already reading line by line.
    #[tokio::test]
    async fn an_assistant_turn_asks_the_cli_for_partial_messages() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner::default());
        state.runner = runner.clone();

        let id = send_message(&state, "assistant-partials-chat", "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        assert_eq!(
            *runner.last_include_partial_messages.lock().unwrap(),
            Some(true)
        );
    }

    /// A runner whose stream carries a tool call before its result.
    fn ran_a_tool(stdout_lines: &[&str]) -> Arc<FakeCommandRunner> {
        Arc::new(FakeCommandRunner {
            canned: Mutex::new(Some(crate::execution::runner::RunOutcome {
                exit_code: 0,
                stdout: stdout_lines.join(
                    "
",
                ),
                stderr: String::new(),
                session_id: Some("fake-session-id".into()),
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
            })),
            ..Default::default()
        })
    }

    /// What a turn DID outlives the stream it did it in.
    ///
    /// The live tail is taken away the moment the turn ends, so without this the actions are
    /// visible for as long as the turn runs and then gone for ever — and a conversation reopened
    /// tomorrow shows a paragraph with nothing to say where it came from.
    #[tokio::test]
    async fn a_finished_turn_records_what_it_did() {
        let mut state = test_state().await;
        state.runner = ran_a_tool(&[
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"core/src/parser.rs"}}]}}"#,
            r#"{"type":"result","subtype":"success","result":"é o parser de datas"}"#,
        ]);

        let id = send_message(&state, "assistant-did-chat", "arranja", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        let stored: Option<String> = sqlx::query_scalar("SELECT tools_used FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let did: Vec<crate::execution::runner::ToolCall> =
            serde_json::from_str(&stored.expect("the turn recorded no actions")).unwrap();

        assert_eq!(did.len(), 1);
        assert_eq!(did[0].name, "Read");
        assert_eq!(did[0].detail.as_deref(), Some("core/src/parser.rs"));
    }

    /// Typing while it works keeps the words instead of refusing them.
    ///
    /// A second message used to take `TURN_IN_PROGRESS` and vanish: the chat's one turn slot was
    /// held, the route answered 409, and the window put a red note under the box. That is the wall
    /// this removes — and it is a wall, not a safeguard, because the thing on the other side of it
    /// is a person who has already thought of the next thing to say.
    ///
    /// Only for a caller that asked to wait. The Telegram sidecar gives up on a turn after a
    /// timeout and would rather be told no than be answered ten minutes later into a conversation
    /// that has moved on, so waiting is opted into rather than imposed.
    #[tokio::test]
    async fn a_message_sent_while_a_turn_runs_is_kept_when_the_caller_asked_to_wait() {
        let state = test_state().await;
        let chat_id = "queue-chat";
        let _first = send_message(&state, chat_id, "arranja o parser", Origin::Shell)
            .await
            .unwrap();

        let outcome =
            send_or_queue(&state, chat_id, "e os testes tambem", &[], Origin::Shell).await;

        assert_eq!(outcome, Ok(Sent::Queued));
        let waiting: Vec<String> = crate::chats::queued(&state.pool, chat_id)
            .await
            .unwrap()
            .into_iter()
            .map(|message| message.text)
            .collect();
        assert_eq!(waiting, vec!["e os testes tambem".to_string()]);
    }

    /// A caller that did not ask to wait is still refused, exactly as before.
    #[tokio::test]
    async fn a_caller_that_cannot_wait_is_still_refused_rather_than_quietly_queued() {
        let state = test_state().await;
        let chat_id = "no-wait-chat";
        let _first = send_message(&state, chat_id, "arranja o parser", Origin::Shell)
            .await
            .unwrap();

        let refused = send_message(&state, chat_id, "e os testes", Origin::Telegram).await;

        assert_eq!(refused, Err(TURN_IN_PROGRESS.to_string()));
        assert!(
            crate::chats::queued(&state.pool, chat_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// The whole point: what waited is sent, without anybody pressing anything again.
    ///
    /// Drained after the turn guard falls rather than before it, because the drain goes back through
    /// `send_message` and that takes the same slot the finished turn is still holding. A drain one
    /// line earlier refuses itself and the message waits for ever.
    #[tokio::test]
    async fn what_waited_becomes_the_next_turn_once_the_slot_is_free() {
        let state = test_state().await;
        let chat_id = "drain-chat";
        let first = send_message(&state, chat_id, "arranja o parser", Origin::Shell)
            .await
            .unwrap();
        send_or_queue(&state, chat_id, "e os testes tambem", &[], Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        // The drain runs at the very end of the finished turn's task, so the second turn appears a
        // moment later rather than in the same breath.
        let mut asked: Vec<String> = Vec::new();
        for _ in 0..200 {
            asked = sqlx::query_scalar(
                "SELECT prompt FROM runs WHERE chat_id = ? AND mode = 'assistant' ORDER BY id",
            )
            .bind(chat_id)
            .fetch_all(&state.pool)
            .await
            .unwrap();
            if asked.len() > 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        assert_eq!(asked.len(), 2, "what waited was never sent: {asked:?}");
        assert!(asked[1].contains("e os testes tambem"), "{asked:?}");
        // And it is off the queue: a message drained and still listed would be sent twice.
        assert!(
            crate::chats::queued(&state.pool, chat_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Two messages typed in the same second must not swap places.
    #[tokio::test]
    async fn what_waits_is_kept_in_the_order_it_was_typed() {
        let state = test_state().await;
        let chat_id = "order-chat";
        let _first = send_message(&state, chat_id, "primeiro", Origin::Shell)
            .await
            .unwrap();
        send_or_queue(&state, chat_id, "segundo", &[], Origin::Shell)
            .await
            .unwrap();
        send_or_queue(&state, chat_id, "terceiro", &[], Origin::Shell)
            .await
            .unwrap();

        let waiting: Vec<String> = crate::chats::queued(&state.pool, chat_id)
            .await
            .unwrap()
            .into_iter()
            .map(|message| message.text)
            .collect();
        assert_eq!(waiting, vec!["segundo".to_string(), "terceiro".to_string()]);
    }

    /// With nothing in flight there is nothing to wait for, and asking to wait must not make a
    /// message wait anyway — it is sent, and the caller is handed the turn it became.
    #[tokio::test]
    async fn asking_to_wait_still_sends_immediately_when_nothing_is_running() {
        let state = test_state().await;

        let outcome = send_or_queue(&state, "idle-chat", "arranja isso", &[], Origin::Shell).await;

        assert!(matches!(outcome, Ok(Sent::Turn(_))), "{outcome:?}");
    }

    // -- relay queueing and the drain's re-check --------------------------------------------
    //
    // Four tests, one per behaviour `send_relayed_or_queue` and the relay branch of `drain_queued`
    // add: a busy relay waits instead of being lost, the drain refuses a waiting relay whose owner
    // left, refuses one whose destination was archived while it waited, and — the test that stops
    // either refusal from leaking where it must not — a person's own queued message is unaffected
    // by either check.

    /// Writes a `chat_relays` row directly and returns its id, the way `relay::admit` would have
    /// once it granted the hop, without paying for any of `admit`'s own brakes.
    ///
    /// `sending_run_id` is a dummy: nothing under test here ever calls `relay::chain_of`, which is
    /// the only reader that cares what it points at. `relay.rs`'s own suite is what proves `admit`
    /// grants and refuses correctly; these tests start from "a relay was already granted" and ask
    /// what happens to the message next, so re-deriving a real chain for each one would test a fact
    /// `relay.rs` already pins, under a different name, for nothing this module needs.
    async fn seed_relay(pool: &SqlitePool, from_chat_id: &str, to_chat_id: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO chat_relays (from_chat_id, to_chat_id, sending_run_id, body, depth, created_at)
             VALUES (?, ?, 0, '', 1, ?) RETURNING id",
        )
        .bind(from_chat_id)
        .bind(to_chat_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// The relay row learns which turn answered it — the mirror of `runs.from_relay_id`, written
    /// back from the other side.
    ///
    /// 0122 declared this column and nothing ever wrote it, so `chat_relays` could say a relay had
    /// been admitted and never whether it landed. That is not a security gap — `chain_of` walks
    /// `runs.from_relay_id` and never this — but it is the whole of what the table was for from a
    /// person's point of view, and a column that is NULL on every row without exception is
    /// indistinguishable from one nobody wired up.
    ///
    /// Both paths, for the reason the origin test above covers both: the local path writes its own
    /// `runs` row, so a stamp wired only into `send_message_inner` would leave every relay into a
    /// conversation set to `Local` looking undelivered for ever.
    #[tokio::test]
    async fn a_relay_learns_which_turn_answered_it() {
        let state = test_state().await;
        let relay_id = seed_relay(
            &state.pool,
            "sender-chat",
            "relay-delivered-destination-chat",
        )
        .await;

        let turn_id = send_relayed_message(
            &state,
            "relay-delivered-destination-chat",
            "onward",
            Origin::Shell,
            relay_id,
        )
        .await
        .unwrap();
        let delivered: Option<i64> =
            sqlx::query_scalar("SELECT delivered_to_run_id FROM chat_relays WHERE id = ?")
                .bind(relay_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            delivered,
            Some(turn_id),
            "the relay must name the turn that answered it"
        );

        let local = AppState {
            assistants: Arc::new(FixedAssistants(fake_local_assistant("answered here"))),
            ..test_state().await
        };
        crate::chats::set_brain(
            &local.pool,
            "a-local-destination",
            crate::chats::Brain::Local,
        )
        .await
        .unwrap();
        let local_relay = seed_relay(&local.pool, "sender-chat", "a-local-destination").await;
        let local_turn = send_relayed_message(
            &local,
            "a-local-destination",
            "onward",
            Origin::Shell,
            local_relay,
        )
        .await
        .unwrap();
        let delivered: Option<i64> =
            sqlx::query_scalar("SELECT delivered_to_run_id FROM chat_relays WHERE id = ?")
                .bind(local_relay)
                .fetch_one(&local.pool)
                .await
                .unwrap();
        assert_eq!(
            delivered,
            Some(local_turn),
            "a relay answered by the local model is stamped too"
        );
    }

    /// The gap this task closes: before `send_relayed_or_queue` existed, a relay into a busy
    /// conversation took `TURN_IN_PROGRESS` and was never seen again — no queue row, no second
    /// chance, the words simply gone. This is the try-then-queue door now, and it must leave a row
    /// that names the relay it travelled on, or the drain has nothing to re-check later.
    #[tokio::test]
    async fn a_relay_into_a_busy_conversation_waits_in_the_queue_with_its_relay_id() {
        let state = test_state().await;
        let chat_id = "relay-busy-chat";
        let relay_id = seed_relay(&state.pool, "sender-chat", chat_id).await;
        let _held = take_the_slot_for_testing(chat_id);

        let outcome = send_relayed_or_queue(
            &state,
            chat_id,
            "mensagem retransmitida",
            Origin::Shell,
            relay_id,
        )
        .await;

        assert_eq!(outcome, Ok(Sent::Queued));
        let (text, stored_relay_id): (String, Option<i64>) =
            sqlx::query_as("SELECT text, relay_id FROM chat_queue WHERE chat_id = ?")
                .bind(chat_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(text, "mensagem retransmitida");
        assert_eq!(
            stored_relay_id,
            Some(relay_id),
            "the queued row must carry the relay it travelled on, or the drain has nothing to re-check"
        );
    }

    /// `admit`'s answer to "is the owner present" was true the moment the relay was granted — that
    /// is what let it queue instead of being refused outright. By the time the drain gets to spend
    /// a turn on it, nobody has come back, and this re-checks rather than trusting a fact that has
    /// had time to go stale.
    #[tokio::test]
    async fn the_drain_refuses_a_relay_born_turn_when_the_owner_has_gone_away() {
        let state = test_state().await;
        let chat_id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        let relay_id = seed_relay(&state.pool, "sender-chat", &chat_id).await;
        crate::chats::enqueue_relayed(
            &state.pool,
            &chat_id,
            "mensagem retransmitida",
            "shell",
            "[]",
            relay_id,
        )
        .await
        .unwrap();
        // No heartbeat recorded for this test at all: `owner_is_present` fails closed to "nobody is
        // there", the same default `relay.rs`'s own `an_absent_owner_refuses_the_relay` relies on.

        drain_queued(&state, &chat_id).await;

        let turns: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(&chat_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            turns, 0,
            "no turn should have started: the owner is not present to see it land"
        );
        assert!(
            crate::chats::queued(&state.pool, &chat_id)
                .await
                .unwrap()
                .is_empty(),
            "dropped, not retried: take_queued already removed the row before this was even checked"
        );
    }

    /// The other half of the same expiry: the owner is still present, but the destination `admit`
    /// checked no longer exists by the time the drain would spend a turn on it — archived while the
    /// message sat waiting, exactly the gap between deciding and spending the whole re-check exists
    /// to close.
    #[tokio::test]
    async fn the_drain_refuses_a_relay_born_turn_when_the_destination_was_archived_while_it_waited()
    {
        let state = test_state().await;
        crate::attention::record_heartbeat(
            &state.pool,
            &crate::attention::AttentionScope::Global,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let chat_id = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
            .await
            .unwrap();
        let relay_id = seed_relay(&state.pool, "sender-chat", &chat_id).await;
        crate::chats::enqueue_relayed(
            &state.pool,
            &chat_id,
            "mensagem retransmitida",
            "shell",
            "[]",
            relay_id,
        )
        .await
        .unwrap();
        // Admitted once, archived since — the destination this message was queued for is gone by
        // the time the drain would spend a turn on it.
        crate::chats::archive(&state.pool, &chat_id).await.unwrap();

        drain_queued(&state, &chat_id).await;

        let turns: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(&chat_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            turns, 0,
            "no turn should have started: the destination is archived"
        );
        assert!(
            crate::chats::queued(&state.pool, &chat_id)
                .await
                .unwrap()
                .is_empty(),
            "dropped, not retried"
        );
    }

    /// The test that stops the fix from breaking ordinary use: the re-check is what a relay pays
    /// for nobody being at the keyboard, and a message a person actually typed must not pay it too.
    /// No heartbeat is recorded here either — the owner is just as away as in the relay test above
    /// — and this queued message still becomes a turn, because its row carries no relay id for the
    /// drain to re-check anything against.
    #[tokio::test]
    async fn a_persons_own_queued_message_still_drains_when_the_owner_is_away() {
        let state = test_state().await;
        let chat_id = "person-queued-owner-away";
        crate::chats::enqueue(&state.pool, chat_id, "mensagem da pessoa", "shell", "[]")
            .await
            .unwrap();

        drain_queued(&state, chat_id).await;

        let turns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE chat_id = ? AND mode = 'assistant'",
        )
        .bind(chat_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            turns, 1,
            "a person's own queued message must still be sent even while the owner is away"
        );
        assert!(
            crate::chats::queued(&state.pool, chat_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A picture sent with a turn reaches the model INSIDE the message.
    ///
    /// Not as a path for it to go and read: it is part of what was said. Measured against the CLI
    /// first — a `user` line whose content is an array with an `image` block is accepted, and a
    /// solid magenta square asked about came back "Magenta".
    ///
    /// Which forces the stdin path. An argument vector holds a string and there is nowhere in it
    /// for bytes to go, so a turn carrying a picture is `steerable` and one carrying none is not —
    /// the two are decided together, here, because a run given images and not steerable would drop
    /// them without a word.
    #[tokio::test]
    async fn a_turn_sent_with_a_picture_carries_it_into_the_run() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let images = vec![crate::execution::runner::Attachment {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }];

        let id = send_message_with(
            &state,
            "picture-chat",
            "que cor e esta?",
            &images,
            Origin::Shell,
        )
        .await
        .unwrap();
        settled_turn(&state.pool, id).await;

        let images = fake.last_images.lock().unwrap().clone().unwrap_or_default();

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].data, "aGVsbG8=");
        // Bytes have nowhere to go in an argv, so a turn carrying them must take the other door.
        assert_eq!(
            *fake.last_steerable.lock().unwrap(),
            Some(true),
            "a turn with a picture must go by stdin"
        );
    }

    /// A conversation with a living process answers the next turn down it, without starting one.
    ///
    /// The lines it gathers are the turn's own transcript, and the outcome is the turn's own bill.
    #[tokio::test]
    async fn a_living_conversation_answers_a_later_turn_down_the_process_it_already_has() {
        let (mut live, mut said, events, _why) = live_chat_for_testing();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        events
            .send(crate::execution::runner::TurnEvent::Line(
                "hello".to_owned(),
            ))
            .unwrap();
        events
            .send(crate::execution::runner::TurnEvent::Ended(
                crate::execution::runner::TurnOutcome {
                    cost_usd: Some(0.08),
                    ..Default::default()
                },
            ))
            .unwrap();

        let LiveTurn::Answered(outcome) = live.turn("e agora?", &[], &transcript, None).await
        else {
            panic!("a process with an answer queued must answer");
        };

        assert_eq!(outcome.cost_usd, Some(0.08));
        assert_eq!(transcript.lock().unwrap().as_str(), "hello\n");
        assert_eq!(
            said.recv().await.map(|turn| turn.text).as_deref(),
            Some("e agora?")
        );
    }

    /// A turn stops at its OWN end, leaving whatever comes after it for the turn that comes after.
    ///
    /// The one thing this type exists to get right. A turn that read past its `Ended` would swallow
    /// the next turn's opening lines, and one that stopped short would hand the next turn the tail
    /// of this one — and both look identical from outside: a conversation whose answers are subtly
    /// somebody else's.
    #[tokio::test]
    async fn a_turn_gathers_its_own_lines_and_leaves_the_next_turns_alone() {
        let (mut live, _said, events, _why) = live_chat_for_testing();
        let first = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let second = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        for event in [
            crate::execution::runner::TurnEvent::Line("mine".to_owned()),
            crate::execution::runner::TurnEvent::Ended(
                crate::execution::runner::TurnOutcome::default(),
            ),
            crate::execution::runner::TurnEvent::Line("theirs".to_owned()),
            crate::execution::runner::TurnEvent::Ended(
                crate::execution::runner::TurnOutcome::default(),
            ),
        ] {
            events.send(event).unwrap();
        }

        assert!(matches!(
            live.turn("um", &[], &first, None).await,
            LiveTurn::Answered(_)
        ));
        assert!(matches!(
            live.turn("dois", &[], &second, None).await,
            LiveTurn::Answered(_)
        ));

        assert_eq!(first.lock().unwrap().as_str(), "mine\n");
        assert_eq!(second.lock().unwrap().as_str(), "theirs\n");
    }

    /// A process that died must say so at once rather than leave the conversation waiting on a pipe
    /// nobody is writing to. The caller then starts one, which is what every turn did before this
    /// existed — so the worst case is last week's speed, not a conversation that hangs.
    #[tokio::test]
    async fn a_conversation_whose_process_is_gone_says_so_instead_of_waiting() {
        let (mut live, said, events, _why) = live_chat_for_testing();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        // BOTH ends, because that is what a process actually being gone looks like: the runner's
        // future owns the stdin the turns are written to and the sender the events come out of, so
        // when it ends it drops the pair. Dropping only one of them models nothing.
        drop(events);
        drop(said);

        // `NotWritten` and not `DiedMidTurn`, and the difference decides whether the caller may
        // start over: nothing reached the process, so as far as anything outside is concerned this
        // turn has not happened yet.
        assert!(matches!(
            live.turn("estas ai?", &[], &transcript, None).await,
            LiveTurn::NotWritten
        ));
    }

    /// A process that heard the turn and died before answering is NOT the same as one that never
    /// heard it, and the caller must be able to tell — starting over would run a second time
    /// whatever the first attempt had already done.
    #[tokio::test]
    async fn a_process_that_dies_mid_turn_is_told_apart_from_one_that_never_heard() {
        let (mut live, said, events, _why) = live_chat_for_testing();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        events
            .send(crate::execution::runner::TurnEvent::Line(
                "working on it".to_owned(),
            ))
            .unwrap();
        drop(events);

        assert!(matches!(
            live.turn("faz isso", &[], &transcript, None).await,
            LiveTurn::DiedMidTurn(_)
        ));
        // It really was written — which is the whole reason this case may not be retried.
        drop(said);
    }

    /// A `LiveChat` wired to plain channels instead of a process: the far end of its stdin, and a
    /// handle to say what it answers.
    ///
    /// No process, because what these tests are about is where one turn ends and the next begins,
    /// and a real CLI would make them depend on one being installed. No recorder task either: what
    /// was written is read straight off the channel, so an assertion cannot pass or fail on whether
    /// some other task happened to have run yet.
    fn live_chat_for_testing() -> (
        LiveChat,
        tokio::sync::mpsc::UnboundedReceiver<crate::execution::runner::LaterTurn>,
        tokio::sync::mpsc::UnboundedSender<crate::execution::runner::TurnEvent>,
        tokio::sync::watch::Sender<Option<String>>,
    ) {
        let (messages, said) =
            tokio::sync::mpsc::unbounded_channel::<crate::execution::runner::LaterTurn>();
        let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
        // The supervisor's voice: in a real one this is the task that owns the process saying why it
        // stopped, and here it is whatever the test wants said.
        let (why_stopped, stopped_because) = tokio::sync::watch::channel(None);
        (
            LiveChat::new(
                messages,
                events,
                std::sync::Arc::new(std::sync::Mutex::new(Some("s-1".to_owned()))),
                // Nothing to stop, but the field is what stops a real one, so it is not optional.
                tokio::spawn(std::future::pending::<()>()).abort_handle(),
                stopped_because,
                crate::execution::runner::Permission::Default,
                None,
                String::new(),
                None,
            ),
            said,
            events_tx,
            why_stopped,
        )
    }

    /// A conversation whose turns get tools: a directory of its own, and the classifier hook wired
    /// inside it.
    ///
    /// `tool_policy_for` grants tools only when both are there, so without them a turn is `McpOnly`,
    /// carries the control token, and keeps no process — a different rule entirely, and one that
    /// would let a weaker version of every test below pass.
    ///
    /// The `TempDir` is returned rather than dropped: dropping it deletes the directory, and a
    /// conversation whose working directory vanished is not the thing under test.
    async fn rooted_chat(state: &AppState, chat_id: &str) -> tempfile::TempDir {
        let root = tempfile::TempDir::new().unwrap();
        crate::governance::autopilot::wire_classifier_hook(root.path()).unwrap();
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at, cwd)
             VALUES (?, 'cloud', '2026-01-01T00:00:00Z', ?)",
        )
        .bind(chat_id)
        .bind(root.path().to_str().unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
        root
    }

    /// A process that has ended is not kept as a handle to nothing.
    ///
    /// The next turn would survive finding one — it writes, the write fails, and it starts a process
    /// instead — but the entry sits there until then holding a conversation's place, and on a daemon
    /// where nobody comes back it sits there for good. A closed stdin is the runner's future having
    /// ended, which is the only signal there is that the process behind it is gone.
    #[tokio::test]
    async fn a_process_that_ended_is_not_kept_as_a_handle_to_nothing() {
        let (live, said, _events, _why) = live_chat_for_testing();
        LIVE_CHATS
            .lock()
            .unwrap()
            .insert("ended-chat".to_owned(), live);

        // What the runner's future ending does: it owns the far end of this conversation's stdin.
        drop(said);

        reap_now();

        assert!(!LIVE_CHATS.lock().unwrap().contains_key("ended-chat"));
    }

    /// A process nobody came back to is stopped rather than left holding a few hundred megabytes.
    ///
    /// What keeping one buys is a burst — the turns somebody takes while working on something. Past
    /// that the next turn is minutes away, where the start-up it saves is not what anybody is
    /// waiting on, and a desktop app is the wrong place to spend the memory.
    #[tokio::test]
    async fn a_process_nobody_came_back_to_is_stopped() {
        let (mut live, _said, _events, _why) = live_chat_for_testing();
        live.idle_since = std::time::Instant::now()
            .checked_sub(LIVE_IDLE * 2)
            .expect("a machine that has been up two minutes");
        LIVE_CHATS
            .lock()
            .unwrap()
            .insert("abandoned-chat".to_owned(), live);

        reap_now();

        assert!(!LIVE_CHATS.lock().unwrap().contains_key("abandoned-chat"));
    }

    /// And one still standing, still recent, is left exactly where it is — or the reaper would be
    /// taking away the thing it exists to protect.
    #[tokio::test]
    async fn a_process_still_standing_and_still_recent_is_left_alone() {
        let (live, _said, _events, _why) = live_chat_for_testing();
        LIVE_CHATS
            .lock()
            .unwrap()
            .insert("working-chat".to_owned(), live);

        reap_now();

        assert!(LIVE_CHATS.lock().unwrap().contains_key("working-chat"));
        // Taken back out, because this map outlives the test that wrote to it.
        LIVE_CHATS.lock().unwrap().remove("working-chat");
    }

    // ---- idle time per chat and the cap on live processes ----------------------------------------

    /// An assistant event whose content is a `tool_use` of the named tool.
    fn idle_lru_tool_use(name: &str) -> String {
        format!(
            r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"tu-1","name":"{name}","input":{{"prompt":"look"}}}}]}}}}"#
        )
    }

    #[test]
    fn idle_lru_a_turn_that_called_a_subagent_gets_the_development_idle() {
        for tool in ["Agent", "Task"] {
            let stdout = format!("{}\n", idle_lru_tool_use(tool));
            assert_eq!(
                idle_for_turn(&stdout, Duration::from_secs(1)),
                DEV_IDLE,
                "a turn that called {tool} is development work"
            );
        }
    }

    #[test]
    fn idle_lru_a_turn_longer_than_five_minutes_gets_the_development_idle() {
        assert_eq!(
            idle_for_turn("", DEV_TURN + Duration::from_secs(1)),
            DEV_IDLE
        );
    }

    #[test]
    fn idle_lru_a_short_plain_turn_keeps_the_conversation_idle() {
        let mentions = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ask the Agent"}]}}"#;
        let reads = idle_lru_tool_use("Read");
        let stdout = format!("{mentions}\n{reads}\n");
        assert_eq!(idle_for_turn(&stdout, Duration::from_secs(1)), LIVE_IDLE);
    }

    #[tokio::test]
    async fn idle_lru_a_live_turn_that_called_agent_is_kept_for_the_development_idle() {
        for (prefix, line, expected) in [
            ("idle-lru-dev", idle_lru_tool_use("Agent"), DEV_IDLE),
            (
                "idle-lru-plain",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hello"}]}}"#
                    .to_owned(),
                LIVE_IDLE,
            ),
        ] {
            let chat = a_chat(prefix);
            let (live, mut said, events, _why) = live_chat_for_testing();
            LIVE_CHATS.lock().unwrap().insert(chat.clone(), live);
            // The process answers only once the message is written: an answer queued before it
            // would be one begun before the message, which is set aside as spontaneous.
            let responder = tokio::spawn(async move {
                said.recv().await.expect("the message was never written");
                events
                    .send(crate::execution::runner::TurnEvent::Line(line))
                    .unwrap();
                events
                    .send(crate::execution::runner::TurnEvent::Ended(
                        crate::execution::runner::TurnOutcome::default(),
                    ))
                    .unwrap();
            });
            let runner: Arc<dyn crate::execution::runner::CommandRunner> =
                Arc::new(FakeCommandRunner::default());
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let transcript = Arc::new(Mutex::new(String::new()));

            let served = serve_turn(
                &runner,
                silence_request(),
                tx,
                &transcript,
                &chat,
                TurnDeadlines {
                    silence: Some(Duration::from_secs(5)),
                    ceiling: Duration::from_secs(10),
                },
                true,
                None,
                "",
            )
            .await;
            let kept_for = LIVE_CHATS.lock().unwrap().get(&chat).map(|l| l.idle_for);
            evict_live(&chat);
            responder.await.unwrap();

            served.expect("in time").expect("the process answered");
            assert_eq!(kept_for, Some(expected));
        }
    }

    #[tokio::test]
    async fn idle_lru_a_development_chat_outlives_ninety_seconds_and_is_reaped_after_fifteen_minutes()
     {
        let chat = a_chat("idle-lru-reap");
        let (mut live, _said, _events, _why) = live_chat_for_testing();
        live.idle_for = DEV_IDLE;
        live.idle_since = std::time::Instant::now()
            .checked_sub(LIVE_IDLE * 2)
            .expect("a machine that has been up three minutes");
        LIVE_CHATS.lock().unwrap().insert(chat.clone(), live);

        reap_now();
        assert!(
            LIVE_CHATS.lock().unwrap().contains_key(&chat),
            "a development chat idle for 180 s is still wanted"
        );

        LIVE_CHATS
            .lock()
            .unwrap()
            .get_mut(&chat)
            .unwrap()
            .idle_since = std::time::Instant::now()
            .checked_sub(DEV_IDLE + Duration::from_secs(1))
            .expect("a machine that has been up sixteen minutes");
        reap_now();
        assert!(
            !LIVE_CHATS.lock().unwrap().contains_key(&chat),
            "past fifteen minutes it is reaped"
        );
    }

    /// Six lives "c0".."c5", c0 the least recently used.
    fn idle_lru_six() -> (HashMap<String, LiveChat>, Vec<impl Sized>) {
        let mut kept = HashMap::new();
        let mut held = Vec::new();
        for i in 0..6u64 {
            let (mut live, said, events, why) = live_chat_for_testing();
            live.idle_since = std::time::Instant::now()
                .checked_sub(Duration::from_secs(60 - i))
                .expect("a machine that has been up a minute");
            kept.insert(format!("c{i}"), live);
            held.push((said, events, why));
        }
        (kept, held)
    }

    #[tokio::test]
    async fn idle_lru_keeping_a_sixth_process_evicts_the_least_recently_used_idle_one() {
        let (mut kept, _held) = idle_lru_six();

        let evicted = make_room(&mut kept, 6, "c5", LIVE_CAP, &HashSet::new());

        assert_eq!(evicted.len(), 1);
        assert!(!kept.contains_key("c0"), "the oldest idle one goes");
        assert_eq!(kept.len(), 5);
        assert!(kept.contains_key("c5"), "never the one being kept");
    }

    #[tokio::test]
    async fn idle_lru_a_process_with_a_background_task_is_never_evicted() {
        let (mut kept, _held) = idle_lru_six();
        kept.get_mut("c0")
            .unwrap()
            .background
            .insert("bg".to_owned());

        let evicted = make_room(&mut kept, 6, "c5", LIVE_CAP, &HashSet::new());

        assert_eq!(evicted.len(), 1);
        assert!(kept.contains_key("c0"), "busy: left alone however old");
        assert!(!kept.contains_key("c1"), "the next-oldest idle one goes");
    }

    #[test]
    fn idle_lru_a_background_reply_does_not_lower_a_development_idle() {
        let plain = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#;
        assert_eq!(
            idle_after_spontaneous(DEV_IDLE, plain, Duration::from_secs(1)),
            DEV_IDLE,
            "a short plain background reply keeps the development idle"
        );
        assert_eq!(
            idle_after_spontaneous(LIVE_IDLE, plain, Duration::from_secs(1)),
            LIVE_IDLE
        );
        assert_eq!(
            idle_after_spontaneous(
                LIVE_IDLE,
                &idle_lru_tool_use("Agent"),
                Duration::from_secs(1)
            ),
            DEV_IDLE,
            "a background turn may still raise it"
        );
    }

    #[tokio::test]
    async fn idle_lru_a_process_holding_an_unclaimed_answer_is_never_evicted() {
        let (mut kept, _held) = idle_lru_six();
        kept.get_mut("c0")
            .unwrap()
            .carried
            .push_back(crate::execution::runner::TurnEvent::Line(
                "an answer".to_owned(),
            ));

        let evicted = make_room(&mut kept, 6, "c5", LIVE_CAP, &HashSet::new());

        assert_eq!(evicted.len(), 1);
        assert!(kept.contains_key("c0"), "its answer is not yet claimed");
        assert!(!kept.contains_key("c1"), "the next-oldest idle one goes");
    }

    #[tokio::test]
    async fn idle_lru_when_every_other_process_is_busy_the_new_one_is_not_kept() {
        let mut kept: HashMap<String, LiveChat> = HashMap::new();
        let mut held = Vec::new();
        for id in ["c0", "c1", "c2", "c3", "new"] {
            let (mut live, said, events, why) = live_chat_for_testing();
            if id != "new" {
                live.background.insert("bg".to_owned());
            }
            kept.insert(id.to_owned(), live);
            held.push((said, events, why));
        }

        let evicted = make_room(&mut kept, 6, "new", LIVE_CAP, &HashSet::new());

        assert_eq!(evicted.len(), 1);
        assert!(!kept.contains_key("new"), "the one being kept is dropped");
        for id in ["c0", "c1", "c2", "c3"] {
            assert!(kept.contains_key(id), "{id} is busy and stays");
        }

        // With a background task of its own it is not dropped either.
        let (mut live, said, events, why) = live_chat_for_testing();
        live.background.insert("bg".to_owned());
        kept.insert("new".to_owned(), live);
        held.push((said, events, why));

        let evicted = make_room(&mut kept, 6, "new", LIVE_CAP, &HashSet::new());

        assert!(evicted.is_empty());
        assert_eq!(kept.len(), 5);
    }

    /// A chat whose transcript the visible view polled a moment ago is open on someone's screen:
    /// its idle process is not reaped, however long the process itself has been quiet.
    #[tokio::test]
    async fn open_chat_a_chat_polled_recently_is_not_reaped() {
        let (mut live, _said, _events, _why) = live_chat_for_testing();
        live.idle_since = std::time::Instant::now()
            .checked_sub(LIVE_IDLE * 2)
            .expect("a machine that has been up three minutes");
        let chat = a_chat("open-polled");
        LIVE_CHATS.lock().unwrap().insert(chat.clone(), live);
        mark_open(&chat);

        reap_now();
        let kept = LIVE_CHATS.lock().unwrap().contains_key(&chat);

        LIVE_CHATS.lock().unwrap().remove(&chat);
        SEEN_CHATS.lock().unwrap().remove(&chat);
        assert!(kept, "an open chat's process was reaped");
    }

    /// The idle clock restarts at the last poll: a poll inside the idle window keeps the process,
    /// one just past it does not.
    #[tokio::test]
    async fn open_chat_the_idle_clock_counts_from_the_last_poll() {
        let now = std::time::Instant::now();
        let long_ago = now
            .checked_sub(LIVE_IDLE * 2)
            .expect("a machine that has been up three minutes");

        let (mut fresh, _said, _events, _why) = live_chat_for_testing();
        fresh.idle_since = long_ago;
        let (mut stale, _said2, _events2, _why2) = live_chat_for_testing();
        stale.idle_since = long_ago;
        let fresh_id = a_chat("open-fresh");
        let stale_id = a_chat("open-stale");
        {
            let mut lives = LIVE_CHATS.lock().unwrap();
            lives.insert(fresh_id.clone(), fresh);
            lives.insert(stale_id.clone(), stale);
        }
        {
            let mut seen = SEEN_CHATS.lock().unwrap();
            seen.insert(
                fresh_id.clone(),
                now.checked_sub(Duration::from_secs(60)).unwrap(),
            );
            seen.insert(
                stale_id.clone(),
                now.checked_sub(LIVE_IDLE + Duration::from_secs(1)).unwrap(),
            );
        }

        reap_now();
        let (fresh_kept, stale_kept) = {
            let lives = LIVE_CHATS.lock().unwrap();
            (lives.contains_key(&fresh_id), lives.contains_key(&stale_id))
        };

        for id in [&fresh_id, &stale_id] {
            LIVE_CHATS.lock().unwrap().remove(id);
            SEEN_CHATS.lock().unwrap().remove(id);
        }
        assert!(fresh_kept, "polled 60s ago is inside the idle window");
        assert!(!stale_kept, "polled past the idle window is reaped");
    }

    #[tokio::test]
    async fn open_chat_make_room_evicts_a_non_open_process_before_an_open_one() {
        let (mut kept, _held) = idle_lru_six();
        let open: HashSet<String> = ["c0".to_owned()].into_iter().collect();

        let evicted = make_room(&mut kept, 6, "c5", LIVE_CAP, &open);

        assert_eq!(evicted.len(), 1);
        assert!(kept.contains_key("c0"), "open, so spared though the oldest");
        assert!(!kept.contains_key("c1"), "the oldest non-open one goes");
    }

    /// Open never outranks pinned: a process with a background task or an unclaimed answer is
    /// not a candidate at all, so when the rest are open the eviction falls on an open one.
    #[tokio::test]
    async fn open_chat_pinned_processes_stay_pinned_when_every_other_is_open() {
        let open: HashSet<String> = ["c1", "c2", "c3", "c4"]
            .into_iter()
            .map(str::to_owned)
            .collect();

        let (mut kept, _held) = idle_lru_six();
        kept.get_mut("c0")
            .unwrap()
            .background
            .insert("bg".to_owned());
        let evicted = make_room(&mut kept, 6, "c5", LIVE_CAP, &open);
        assert_eq!(evicted.len(), 1);
        assert!(kept.contains_key("c0"), "background task: pinned");
        assert!(!kept.contains_key("c1"), "the oldest open one goes");

        let (mut kept, _held) = idle_lru_six();
        kept.get_mut("c0")
            .unwrap()
            .carried
            .push_back(crate::execution::runner::TurnEvent::Line(
                "an answer".to_owned(),
            ));
        let evicted = make_room(&mut kept, 6, "c5", LIVE_CAP, &open);
        assert_eq!(evicted.len(), 1);
        assert!(kept.contains_key("c0"), "unclaimed answer: pinned");
        assert!(!kept.contains_key("c1"), "the oldest open one goes");
    }

    #[test]
    fn open_chat_is_open_only_inside_the_window() {
        let now = std::time::Instant::now();
        let ago = |s: u64| now.checked_sub(Duration::from_secs(s));
        assert!(is_open(ago(29), now));
        assert!(!is_open(ago(31), now));
        assert!(!is_open(None, now));
    }

    /// The whole point: a conversation's second turn is answered by the process its first one
    /// started.
    ///
    /// Counted at the runner, because the runner is where the cost is. `calls` counts LAUNCHES, and
    /// two turns down one process is one launch — measured on the real CLI, the launch it saves is
    /// worth 14 to 19 seconds: 5.8s to reach `init` against 1.5s, and 26.4s before the first shell
    /// command runs against 6.7s.
    ///
    /// Both turns still get a row of their own, because a turn is a billed run whichever process
    /// answered it. That half is asserted here too: a conversation that answered twice and recorded
    /// once would be cheaper to run and impossible to read.
    #[tokio::test]
    async fn a_conversations_second_turn_is_answered_by_the_process_the_first_one_started() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "warm-chat").await;

        let first = send_message(&state, "warm-chat", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        let second = send_message(&state, "warm-chat", "segundo", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            1,
            "the second turn started a second process instead of speaking to the first"
        );
        for (turn, id) in [("first", first), ("second", second)] {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(status, "completed", "the {turn} turn did not complete");
        }
    }

    /// What waited is answered by the process that was busy when it was typed.
    ///
    /// The two halves of this branch meet here and nowhere else. A message typed while a
    /// conversation is working is kept rather than refused; a conversation keeps its process between
    /// turns. The drain runs at the very end of the finished turn's task — after the guard falls, so
    /// the slot is free — with the process that turn just finished with standing right there.
    ///
    /// Both were tested apart and neither was tested against the other, and the queue's own test
    /// cannot reach this: its chat has no directory and no hook, so it is `McpOnly` and keeps no
    /// process at all. If the two stopped composing the symptom would be a queued message answered
    /// slowly, which looks exactly like a queued message answered.
    #[tokio::test]
    async fn what_waited_is_answered_by_the_process_that_was_busy_when_it_was_typed() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "warm-drain").await;

        let first = send_message(&state, "warm-drain", "primeiro", Origin::Shell)
            .await
            .unwrap();
        send_or_queue(&state, "warm-drain", "segundo", &[], Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        // The drain runs at the very end of the finished turn's task, so the second turn appears a
        // moment later rather than in the same breath.
        let mut ids: Vec<i64> = Vec::new();
        for _ in 0..200 {
            ids = sqlx::query_scalar(
                "SELECT id FROM runs WHERE chat_id = ? AND mode = 'assistant' ORDER BY id",
            )
            .bind("warm-drain")
            .fetch_all(&state.pool)
            .await
            .unwrap();
            if ids.len() > 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(ids.len(), 2, "what waited was never sent");
        settled_turn(&state.pool, ids[1]).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            1,
            "the drained turn started a process of its own instead of using the one that was there"
        );
        for id in &ids {
            let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(status, "completed", "turn {id} did not complete");
        }
    }

    /// A conversation given a fresh context must not be answered by the process
    /// holding the old one.
    ///
    /// Rotation is the daemon deciding this conversation starts again: the window is full, or
    /// something untrusted was read and the session may no longer be resumed. A process kept from
    /// before is holding exactly the session that decision just abandoned, so speaking down it would
    /// continue the conversation that was supposed to have ended — with every one of those reasons
    /// still true, and nothing in the transcript to show it happened.
    #[tokio::test]
    async fn a_conversation_let_go_of_is_not_answered_by_the_process_holding_the_old_session() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "restarted-chat").await;

        let first = send_message(&state, "restarted-chat", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        // What letting go of a session leaves behind: nothing to resume. The reasons differ — a
        // mail body read, somebody asking to start over — and both arrive here as the same absence.
        sqlx::query("DELETE FROM assistant_sessions WHERE chat_id = 'restarted-chat'")
            .execute(&state.pool)
            .await
            .unwrap();

        let second = send_message(&state, "restarted-chat", "segundo", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            2,
            "the restarted turn was answered by the process holding the session it left"
        );
    }

    /// A conversation that moved is not answered by the process standing where it used to be.
    ///
    /// A working directory is resolved per turn — the chat's own can be repointed from the
    /// window — while a process is standing wherever it was spawned and cannot be told to move. Answering down it would run the turn's commands, and
    /// resolve every relative path it wrote, in the wrong tree; the transcript would look right.
    #[tokio::test]
    async fn a_conversation_that_moved_is_not_answered_by_the_process_standing_where_it_was() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "moving-chat").await;

        let first = send_message(&state, "moving-chat", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        // Wired in the new place too, so what this test measures is the MOVE and not the loss of
        // tools: an unwired directory would take the one-shot path anyway and pass for free.
        let elsewhere = tempfile::TempDir::new().unwrap();
        crate::governance::autopilot::wire_classifier_hook(elsewhere.path()).unwrap();
        sqlx::query("UPDATE chats SET cwd = ? WHERE chat_id = 'moving-chat'")
            .bind(elsewhere.path().to_str().unwrap())
            .execute(&state.pool)
            .await
            .unwrap();

        let second = send_message(&state, "moving-chat", "segundo", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            2,
            "the turn was answered by a process standing in the old directory"
        );
    }

    /// The seam: this file's own code driving the REAL CLI, and a second turn served by the process
    /// the first one started.
    ///
    /// `#[ignore]` because it needs the Claude Code CLI installed, an authenticated session, and
    /// about forty cents.
    ///
    /// Everything else about a living conversation is tested against `FakeCommandRunner` — which
    /// proves the wiring, and which this month proved it against a double that could not do the
    /// thing being wired: it had no `run_prompt_with_turns`, so every rooted turn in the suite took
    /// a path that could not work and every assertion still passed. `runner.rs` asks the real CLI
    /// whether it serves a second turn down one stdin. This asks whether THIS code does, which is
    /// the half in between and the half nothing covered.
    ///
    /// No daemon, no HTTP, no port: the daemon's database is the operator's own and its port is
    /// whatever is already listening on this machine. What is under test is `serve_turn` and what it
    /// keeps, so that is what is called.
    #[tokio::test]
    #[ignore = "spawns the real Claude CLI and spends money; run with --include-ignored"]
    async fn the_real_cli_serves_a_conversations_second_turn_through_serve_turn() {
        /// The real runner with a tally, because "one process served both" is a claim about
        /// LAUNCHES and the real runner keeps no count of its own.
        struct Counting {
            inner: crate::execution::runner::ClaudeCliRunner,
            launches: Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl crate::execution::runner::CommandRunner for Counting {
            async fn run_prompt(
                &self,
                request: crate::execution::runner::RunRequest,
                session_tx: tokio::sync::mpsc::UnboundedSender<String>,
                transcript: std::sync::Arc<Mutex<String>>,
            ) -> std::io::Result<crate::execution::runner::RunOutcome> {
                *self.launches.lock().unwrap() += 1;
                self.inner.run_prompt(request, session_tx, transcript).await
            }

            async fn run_prompt_with_turns(
                &self,
                request: crate::execution::runner::RunRequest,
                session_tx: tokio::sync::mpsc::UnboundedSender<String>,
                transcript: std::sync::Arc<Mutex<String>>,
                context_fill: std::sync::Arc<Mutex<Option<i64>>>,
                turn_events: Option<
                    tokio::sync::mpsc::UnboundedSender<crate::execution::runner::TurnEvent>,
                >,
            ) -> std::io::Result<crate::execution::runner::RunOutcome> {
                *self.launches.lock().unwrap() += 1;
                self.inner
                    .run_prompt_with_turns(
                        request,
                        session_tx,
                        transcript,
                        context_fill,
                        turn_events,
                    )
                    .await
            }
        }

        // Held typed as well as behind the trait object, so the tally can actually be read. A
        // counter nobody asserts on is the shape of a test that passes for the wrong reason, which
        // is the failure this whole test exists to rule out.
        let counting = std::sync::Arc::new(Counting {
            inner: crate::execution::runner::ClaudeCliRunner {
                model: "sonnet".to_owned(),
                plan_model: None,
                review_model: None,
                resolve_model: None,
                resolve_effort: None,
            },
            launches: Mutex::new(0),
        });
        let runner: std::sync::Arc<dyn crate::execution::runner::CommandRunner> = counting.clone();
        let root = tempfile::TempDir::new().unwrap();
        let chat_id = "the-seam";

        // Turn one: no session to resume, so it starts a process and keeps it.
        let first = std::sync::Arc::new(Mutex::new(String::new()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let opening = serve_turn(
            &runner,
            seam_request("Reply with the single word one.", root.path(), None),
            tx,
            &first,
            chat_id,
            TurnDeadlines {
                silence: None,
                ceiling: std::time::Duration::from_secs(180),
            },
            true,
            None,
            "",
        )
        .await
        .expect("the turn should not have timed out")
        .expect("the CLI should have run");

        let session = opening
            .session_id
            .clone()
            .expect("a turn announces its session");
        assert!(opening.stdout.contains("\"type\":\"result\""));

        // Turn two, down the process turn one left standing. The session it names is the one that
        // process is on, which is what the guard in `serve_turn` requires before it will speak to it.
        let second_said = std::sync::Arc::new(Mutex::new(String::new()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let second = serve_turn(
            &runner,
            seam_request(
                "Reply with the single word two.",
                root.path(),
                Some(session.clone()),
            ),
            tx,
            &second_said,
            chat_id,
            TurnDeadlines {
                silence: None,
                ceiling: std::time::Duration::from_secs(180),
            },
            true,
            None,
            "",
        )
        .await
        .expect("the second turn should not have timed out")
        .expect("the CLI should still have been there");

        // Taken out before any assertion can fail, or a panic leaves a real CLI running.
        evict_live(chat_id);

        assert_eq!(
            *counting.launches.lock().unwrap(),
            1,
            "the second turn started a second CLI instead of speaking to the first"
        );
        assert_eq!(second.session_id.as_deref(), Some(session.as_str()));
        // The boundary, against a real stream: turn two got ITS answer and not turn one's as well.
        assert_eq!(second.stdout.matches("\"type\":\"result\"").count(), 1);
        assert!(!second.stdout.contains("single word one"));
    }

    /// One turn of a rooted conversation, in the shape `send_message_with` builds.
    #[cfg(any(test, feature = "testkit"))]
    fn seam_request(
        prompt: &str,
        cwd: &std::path::Path,
        resume: Option<String>,
    ) -> crate::execution::runner::RunRequest {
        crate::execution::runner::RunRequest {
            prompt: prompt.to_owned(),
            env: Vec::new(),
            cwd: Some(cwd.to_path_buf()),
            permission: crate::execution::runner::Permission::Default,
            resume_session_id: resume,
            mcp_config: None,
            mcp_job: None,
            mcp_team_run: None,
            tool_policy: crate::execution::runner::ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: Some(crate::auth::generate_uuid_v4()),
            fork_session: false,
            include_partial_messages: true,
            images: Vec::new(),
            steerable: false,
            classifier_governs_tools: false,
            messages: None,
            ambient_mcp: false,
            model: Some("sonnet".to_owned()),
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            allowed_mcp_tools: None,
            background_tasks: false,
        }
    }

    /// A picture on a later turn goes down the process that is already standing.
    ///
    /// It used to start a new one. The channel a later turn arrives on carried a `String`, and the
    /// steering task wrote it with no attachments — so a conversation keeping its process had to
    /// give it up the moment somebody pasted a screenshot, which is exactly when a coding
    /// conversation is most alive and the start-up costs most.
    ///
    /// The old comment was true when it was written: the channel existed only for the queue drain,
    /// where a later turn really was text somebody typed while waiting. A conversation that keeps
    /// its process made every turn after the first a later turn.
    #[tokio::test]
    async fn a_picture_on_a_later_turn_goes_down_the_process_that_is_already_standing() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "picture-again").await;

        let first = send_message(&state, "picture-again", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        let images = vec![crate::execution::runner::Attachment {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }];
        let second = send_message_with(&state, "picture-again", "e isto?", &images, Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            1,
            "the picture made the conversation give up its process"
        );
        let later = fake.later_turns.lock().unwrap().clone();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].text, "e isto?");
        assert_eq!(later[0].images.len(), 1, "the picture did not travel");
        assert_eq!(later[0].images[0].data, "aGVsbG8=");
    }

    /// Dropping a conversation's `LiveChat` stops the process behind it.
    ///
    /// The mechanism the whole lifetime rests on. Closing stdin would let a process finish whatever
    /// turn it is on first — measured, and not a way to interrupt one — so the handle carries an
    /// abort and `Drop` is where it is used. Everything else is arranged so that dropping happens:
    /// a turn holds the only handle while it runs, and hands it back only on the way out.
    #[tokio::test]
    async fn dropping_a_conversations_handle_stops_the_process_behind_it() {
        // Built here rather than adapted from the helper: a type with a `Drop` cannot be moved out
        // of, which is the same property being tested.
        let (messages, _said) = tokio::sync::mpsc::unbounded_channel();
        let (_events_tx, events) = tokio::sync::mpsc::unbounded_channel();
        let running = tokio::spawn(std::future::pending::<()>());
        let live = LiveChat::new(
            messages,
            events,
            std::sync::Arc::new(Mutex::new(Some("s-1".to_owned()))),
            running.abort_handle(),
            tokio::sync::watch::channel(None).1,
            crate::execution::runner::Permission::Default,
            None,
            String::new(),
            None,
        );

        drop(live);

        // A task that would otherwise never finish, finishing — which is only true if it was
        // aborted. Bounded, because the failure here is a task that never ends: without the clock a
        // broken `Drop` would hang this test instead of failing it, and a suite that hangs says
        // less than one that fails.
        let stopped = tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .expect("dropping the handle must stop the process, not leave it running");
        assert!(stopped.unwrap_err().is_cancelled());
    }

    /// A kept process that ends stops vouching for its turns' tasks, and closes their rows.
    ///
    /// Authority comes from the process being alive (`live_process_served`), so it must end the
    /// instant the handle drops; the `running` rows are closed by a spawned task, so those are
    /// polled for. Its own chat id: `PROCESS_BARRIERS` is shared by every test in the process.
    #[tokio::test]
    async fn a_dropped_process_closes_its_tasks_and_stops_vouching_for_them() {
        let state = test_state().await;
        let (messages, _said) = tokio::sync::mpsc::unbounded_channel();
        let (_events_tx, events) = tokio::sync::mpsc::unbounded_channel();
        let running = tokio::spawn(std::future::pending::<()>());
        let live = LiveChat::new(
            messages,
            events,
            std::sync::Arc::new(Mutex::new(Some("s-1".to_owned()))),
            running.abort_handle(),
            tokio::sync::watch::channel(None).1,
            crate::execution::runner::Permission::Default,
            None,
            String::new(),
            None,
        );
        note_served(
            live.process_key(),
            "drop-vouch-chat",
            4242,
            "auto",
            &state.pool,
        );
        sqlx::query(
            "INSERT INTO chat_tasks (chat_id, launched_by_run_id, tool_use_id, kind, status, started_at)
             VALUES ('drop-vouch-chat', 4242, 'toolu_drop', 'background_agent', 'running', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        assert!(live_process_served("drop-vouch-chat", 4242));

        drop(live);

        assert!(
            !live_process_served("drop-vouch-chat", 4242),
            "a dropped process must stop vouching at once"
        );
        let mut status = String::new();
        for _ in 0..250 {
            status = sqlx::query_scalar::<_, String>(
                "SELECT status FROM chat_tasks WHERE chat_id = 'drop-vouch-chat' AND tool_use_id = 'toolu_drop'",
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
            if status == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status, "stopped");
    }

    /// Cancelling a turn takes its conversation's process with it.
    ///
    /// A cancelled turn is one somebody stopped, and the process answering it is mid-answer. Left
    /// standing it would go on working for a turn nobody is waiting for, and the NEXT turn would
    /// find its leftovers where its own answer should be — a conversation quietly answered by the
    /// one before it.
    ///
    /// The double here does not end when its stdin closes, which is what makes this observable: a
    /// real CLI told no more turns are coming finishes the one it is on first, so "the process
    /// stopped" and "the process was allowed to finish" would otherwise look identical from outside.
    #[tokio::test]
    async fn cancelling_a_turn_takes_the_conversations_process_with_it() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "stopped-chat").await;

        let first = send_message(&state, "stopped-chat", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        assert!(LIVE_CHATS.lock().unwrap().contains_key("stopped-chat"));

        // From here the process takes its time, so the second turn can be caught in flight.
        *fake.delay.lock().unwrap() = Some(Duration::from_secs(30));
        let second = send_message(&state, "stopped-chat", "segundo", Origin::Shell)
            .await
            .unwrap();
        // Waited on the turn having TAKEN the process, which is the entry leaving the registry —
        // not on its task existing. `spawn_registered` inserts the handle before the body runs, so
        // cancelling on that signal races the turn to the process and usually wins, which proves
        // nothing: a cancel that lands before a turn picks the process up has no process to stop.
        for _ in 0..200 {
            if !LIVE_CHATS.lock().unwrap().contains_key("stopped-chat") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        crate::execution::runs::cancel_run(
            axum::extract::State(state.clone()),
            axum::extract::Path(second),
        )
        .await;
        settled_turn(&state.pool, second).await;

        assert!(
            !LIVE_CHATS.lock().unwrap().contains_key("stopped-chat"),
            "a cancelled turn left its process where the next one would find it"
        );
        for _ in 0..200 {
            if *fake.stopped_early.lock().unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            *fake.stopped_early.lock().unwrap(),
            "the process was left running for a turn nobody is waiting for"
        );
    }

    /// The opening turn launches a background task and answers.
    const LAUNCHES_A_TASK: &str = concat!(
        r#"{"type":"system","subtype":"task_started","task_id":"bg1","is_backgrounded":true}"#,
        "\n",
        r#"{"type":"result","subtype":"success","result":"fake output"}"#
    );
    /// What the CLI says on its own when that task ends (spike 2026-10-05 (c)).
    const ANSWERS_ON_ITS_OWN: &str = concat!(
        r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#,
        "\n",
        r#"{"type":"system","subtype":"task_notification","task_id":"bg1","status":"completed"}"#,
        "\n",
        r#"{"type":"system","subtype":"init","session_id":"fake-session-id"}"#,
        "\n",
        r#"{"type":"result","subtype":"success","result":"task follow-up","total_cost_usd":0.25}"#
    );

    /// A fake whose process launches a background task, and the sender that makes it answer
    /// unprompted.
    fn fake_with_a_background_task() -> (
        Arc<FakeCommandRunner>,
        tokio::sync::mpsc::UnboundedSender<String>,
    ) {
        let fake = Arc::new(FakeCommandRunner::default());
        *fake.canned.lock().unwrap() = Some(crate::execution::runner::RunOutcome {
            exit_code: 0,
            stdout: LAUNCHES_A_TASK.to_owned(),
            stderr: String::new(),
            session_id: Some("fake-session-id".into()),
            cost_usd: Some(0.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
        });
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        *fake.unprompted.lock().unwrap() = Some(rx);
        (fake, tx)
    }

    type TaskTurnRow = (
        i64,
        String,
        Option<String>,
        Option<f64>,
        String,
        i64,
        Option<String>,
    );

    /// The chat's `origin = 'task'` turn once it has settled, or `None` if none appeared in ~6 s.
    async fn settled_task_turn(pool: &SqlitePool, chat_id: &str) -> Option<TaskTurnRow> {
        for _ in 0..300 {
            let row: Option<TaskTurnRow> = sqlx::query_as(
                "SELECT id, status, stdout, cost_usd, prompt, read_untrusted, permission_mode
                   FROM runs WHERE chat_id = ? AND origin = 'task' ORDER BY id LIMIT 1",
            )
            .bind(chat_id)
            .fetch_optional(pool)
            .await
            .unwrap();
            if let Some(row) = row.filter(|row| row.1 != "running") {
                for _ in 0..100 {
                    if !is_busy(chat_id) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                return Some(row);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        None
    }

    /// Background tasks are followed from the stream: a backgrounded start adds one, a foreground
    /// one is ignored, a snapshot replaces the set, and a notification or a terminal update ends one.
    #[test]
    fn background_tasks_are_tracked_from_the_stream() {
        let mut running = HashSet::new();
        let started =
            r#"{"type":"system","subtype":"task_started","task_id":"a","is_backgrounded":true}"#;
        track_background(started, &mut running);
        assert!(running.contains("a"));

        let foreground =
            r#"{"type":"system","subtype":"task_started","task_id":"fg","is_backgrounded":false}"#;
        track_background(foreground, &mut running);
        assert!(!running.contains("fg"), "a foreground task was tracked");

        let snapshot = r#"{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"a"},{"task_id":"b"}]}"#;
        track_background(snapshot, &mut running);
        assert_eq!(
            running,
            HashSet::from(["a".to_owned(), "b".to_owned()]),
            "a snapshot is the whole truth"
        );

        let ended =
            r#"{"type":"system","subtype":"task_notification","task_id":"a","status":"completed"}"#;
        track_background(ended, &mut running);
        assert_eq!(running, HashSet::from(["b".to_owned()]));

        let killed = r#"{"type":"system","subtype":"task_updated","task_id":"b","patch":{"status":"killed"}}"#;
        track_background(killed, &mut running);
        assert!(running.is_empty());

        for line in [started, foreground, snapshot, ended, killed] {
            assert!(is_task_bookkeeping(line), "not bookkeeping: {line}");
        }
        assert!(!is_task_bookkeeping(
            r#"{"type":"result","subtype":"success","result":"hi"}"#
        ));
    }

    /// A process with a background task still running is the one thing the reaper must not take, however
    /// long it has been idle; once the task is over the ordinary idle rule applies again.
    #[tokio::test]
    async fn a_process_with_a_background_task_running_is_not_reaped() {
        let (mut live, _said, _events, _why) = live_chat_for_testing();
        live.background.insert("bg1".to_owned());
        live.idle_since = std::time::Instant::now()
            .checked_sub(LIVE_IDLE * 2)
            .expect("a machine that has been up two minutes");
        let chat = a_chat("pinned");
        LIVE_CHATS.lock().unwrap().insert(chat.clone(), live);

        reap_now();
        assert!(
            LIVE_CHATS.lock().unwrap().contains_key(&chat),
            "a process with a task running was reaped"
        );

        LIVE_CHATS
            .lock()
            .unwrap()
            .get_mut(&chat)
            .unwrap()
            .background
            .clear();
        reap_now();
        assert!(!LIVE_CHATS.lock().unwrap().contains_key(&chat));
    }

    /// A background task that ends AFTER its turn makes the CLI answer on its own. That answer is
    /// the task's, recorded as a turn of its own between the two human ones — not handed to
    /// whoever types next, who would read it as the reply to their question and pay for it.
    #[tokio::test]
    async fn a_spontaneous_answer_between_turns_is_its_own_turn_and_not_the_next_ones() {
        let (fake, unprompted) = fake_with_a_background_task();
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("spontaneous");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        assert!(
            LIVE_CHATS
                .lock()
                .unwrap()
                .get(&chat)
                .is_some_and(|live| live.background.contains("bg1")),
            "the background task the opening turn launched was not tracked"
        );

        unprompted.send(ANSWERS_ON_ITS_OWN.to_owned()).unwrap();
        // Not unwrapped yet: the assertion about the person's turn comes first, so a failure names
        // the harm and not just the absence of a row.
        let task = settled_task_turn(&state.pool, &chat).await;

        let second = send_message(&state, &chat, "segundo", Origin::Shell)
            .await
            .unwrap();
        let (_, answer) = settled_turn(&state.pool, second).await;

        assert_eq!(
            answer.as_deref(),
            Some("fake output"),
            "the person's turn was answered with the assistant's unprompted reply"
        );
        let task = task.expect("the unprompted answer was never recorded as a turn of its own");
        assert_eq!(
            LIVE_CHATS
                .lock()
                .unwrap()
                .get(&chat)
                .map(|live| live.background.is_empty()),
            Some(true),
            "the task's end was not tracked, or its process was not kept"
        );
        assert_eq!(task.1, "completed");
        assert_eq!(task.2.as_deref(), Some("task follow-up"));
        assert_eq!(task.3, Some(0.25));
        assert!(
            task.4.contains("bg1"),
            "the turn's prompt should name the notification, got: {}",
            task.4
        );
        assert!(
            first < task.0 && task.0 < second,
            "the spontaneous turn must sit between the two human ones: {first} < {} < {second}",
            task.0
        );
        let (second_cost,): (Option<f64>,) =
            sqlx::query_as("SELECT cost_usd FROM runs WHERE id = ?")
                .bind(second)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_ne!(
            second_cost,
            Some(0.25),
            "the person was billed for the task's answer"
        );
    }

    /// A task the opening turn launched is a durable row from that turn's end, and its end event
    /// arriving between turns closes the row with status, tokens and summary.
    #[tokio::test]
    async fn a_background_task_is_recorded_from_its_launch_to_its_end_between_turns() {
        let fake = Arc::new(FakeCommandRunner::default());
        *fake.canned.lock().unwrap() = Some(crate::execution::runner::RunOutcome {
            exit_code: 0,
            stdout: concat!(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"sleep 20","run_in_background":true}}]}}"#,
                "
",
                r#"{"type":"system","subtype":"task_started","task_id":"bg1","tool_use_id":"toolu_bg","is_backgrounded":true}"#,
                "
",
                r#"{"type":"result","subtype":"success","result":"fake output"}"#
            )
            .to_owned(),
            stderr: String::new(),
            session_id: Some("fake-session-id".into()),
            cost_usd: Some(0.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
        });
        let (unprompted, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        *fake.unprompted.lock().unwrap() = Some(rx);
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("task-row");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "lança", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        let mut rows = Vec::new();
        for _ in 0..300 {
            rows = crate::chats::tasks::for_chat(&state.pool, &chat)
                .await
                .unwrap();
            if !rows.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let [launched] = rows.as_slice() else {
            panic!("expected one task row after the launching turn, got {rows:?}");
        };
        assert_eq!(launched.status, "running");
        assert_eq!(launched.kind, "background_bash");
        assert_eq!(launched.launched_by_run_id, first);
        assert_eq!(launched.task_id.as_deref(), Some("bg1"));

        unprompted
            .send(
                r#"{"type":"system","subtype":"task_notification","task_id":"bg1","tool_use_id":"toolu_bg","status":"completed","summary":"(exit code 0)","usage":{"total_tokens":321}}"#
                    .to_owned(),
            )
            .unwrap();
        for _ in 0..300 {
            rows = crate::chats::tasks::for_chat(&state.pool, &chat)
                .await
                .unwrap();
            if rows.first().is_some_and(|row| row.status != "running") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let [ended] = rows.as_slice() else {
            panic!("expected one task row, got {rows:?}");
        };
        assert_eq!(ended.status, "completed");
        assert_eq!(ended.total_tokens, Some(321));
        assert_eq!(ended.summary.as_deref(), Some("(exit code 0)"));
        assert!(ended.finished_at.is_some());
    }

    /// The spontaneous turn runs the same process the person's last turn did, so it carries that
    /// turn's barrier: the strictest `read_untrusted` and the same `permission_mode`.
    #[tokio::test]
    async fn a_spontaneous_turn_inherits_the_barrier_of_the_turn_before_it() {
        let (fake, unprompted) = fake_with_a_background_task();
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("spontaneous-barrier");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();
        let (mode,): (Option<String>,) =
            sqlx::query_as("SELECT permission_mode FROM runs WHERE id = ?")
                .bind(first)
                .fetch_one(&state.pool)
                .await
                .unwrap();

        unprompted.send(ANSWERS_ON_ITS_OWN.to_owned()).unwrap();
        let task = settled_task_turn(&state.pool, &chat)
            .await
            .expect("the unprompted answer was never recorded as a turn of its own");

        assert_eq!(task.5, 1, "the spontaneous turn dropped the read barrier");
        assert!(
            mode.is_some(),
            "the opening turn recorded no permission mode"
        );
        assert_eq!(
            task.6, mode,
            "the spontaneous turn changed the permission mode"
        );
    }

    /// Waits until a turn's task is registered, which is what `finalize_termination` needs to find.
    ///
    /// A stop that lands before the handle exists has nothing to abort, and the test would then be
    /// measuring a race instead of the stop.
    async fn the_turn_has_its_handle(state: &AppState, id: i64) {
        for _ in 0..300 {
            if state.run_handles.lock().unwrap().contains_key(&id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("turn {id} never registered its task");
    }

    /// Waits until a conversation's process is reachable mid-turn: taken from the registry of idle
    /// ones, and listed with the ones that can be written to.
    async fn the_process_is_steerable(chat_id: &str) {
        for _ in 0..300 {
            if !LIVE_CHATS.lock().unwrap().contains_key(chat_id) && is_steerable(chat_id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{chat_id} never became steerable");
    }

    /// A conversation with no directory of its own, so its turns keep no process.
    async fn unrooted_chat(state: &AppState, chat_id: &str) {
        sqlx::query("INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', ?)")
            .bind(chat_id)
            .bind("2026-01-01T00:00:00Z")
            .execute(&state.pool)
            .await
            .unwrap();
    }

    async fn run_status(pool: &SqlitePool, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT status, stdout FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Stop on a turn running in a live process asks the CLI to stop and leaves the process alone.
    ///
    /// Spike CLI 2.1.280: a `control_request` interrupt ends the turn in about 30 ms, with the
    /// partial answer already streamed, and the process and its session survive. So the turn is
    /// recorded `cancelled` WITH what it had said, the process goes back to the registry, and
    /// nothing was killed — `stopped_early` is the double's way of saying the process was dropped.
    #[tokio::test]
    async fn stopping_a_live_turn_interrupts_it_and_keeps_the_process() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("interrupted-chat");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        assert!(LIVE_CHATS.lock().unwrap().contains_key(&chat));

        *fake.delay.lock().unwrap() = Some(Duration::from_secs(30));
        let second = send_message(&state, &chat, "segundo", Origin::Shell)
            .await
            .unwrap();
        the_process_is_steerable(&chat).await;

        let stopped = stop_turn(&state, second).await;
        assert!(
            matches!(stopped, Stopped::Interrupted),
            "a live turn is interrupted, not killed"
        );
        settled_turn(&state.pool, second).await;

        let (status, stdout) = run_status(&state.pool, second).await;
        assert_eq!(status, "cancelled");
        assert_eq!(
            stdout.as_deref(),
            Some("half an answer"),
            "the partial answer is kept"
        );
        assert!(
            LIVE_CHATS.lock().unwrap().contains_key(&chat),
            "the process went back to the registry"
        );
        assert!(
            !*fake.stopped_early.lock().unwrap(),
            "an interrupt must not take the process with it"
        );
        LIVE_CHATS.lock().unwrap().remove(&chat);
    }

    /// An interrupt nobody answers is not waited on forever: the kill path runs.
    ///
    /// The line may have been written before the CLI started the turn (its start-up takes ~14 s), in
    /// which case nothing ever answers it. A hand-opened registry entry stands in for that process:
    /// it takes the interrupt and says nothing back.
    #[tokio::test]
    async fn an_interrupt_nobody_answers_falls_back_to_the_kill() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("unanswered-chat");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        *fake.delay.lock().unwrap() = Some(Duration::from_secs(30));
        let second = send_message(&state, &chat, "segundo", Origin::Shell)
            .await
            .unwrap();
        the_process_is_steerable(&chat).await;

        // Replaces the turn's own entry, so the interrupt goes to a channel nobody reads for the
        // double. The receiver is kept to see what was written.
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::execution::runner::LaterTurn>();
        let _guard = SteerGuard::open(&chat, &tx);

        let stopped = stop_turn_within(&state, second, Duration::from_millis(200)).await;

        assert!(
            matches!(stopped, Stopped::Killed),
            "an unanswered interrupt must end in the kill"
        );
        let written = rx.try_recv().expect("the interrupt was written");
        assert!(written.interrupt);
        settled_turn(&state.pool, second).await;
        assert_eq!(run_status(&state.pool, second).await.0, "cancelled");
        for _ in 0..200 {
            if *fake.stopped_early.lock().unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            *fake.stopped_early.lock().unwrap(),
            "the kill path stops the process"
        );
        LIVE_CHATS.lock().unwrap().remove(&chat);
    }

    /// Stop agent writes one stop of that task into the running process, never an interrupt, and
    /// says so when there is no running process to write it into.
    #[test]
    fn stopping_one_task_asks_the_running_process_and_nothing_else() {
        let chat = "stop-task-chat";
        assert_eq!(stop_task(chat, "b84qqcytz"), StoppedTask::NotRunning);

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::execution::runner::LaterTurn>();
        let _guard = SteerGuard::open(chat, &tx);
        assert_eq!(stop_task(chat, "b84qqcytz"), StoppedTask::Asked);
        let written = rx.try_recv().expect("the stop was written");
        assert_eq!(written.stop_task.as_deref(), Some("b84qqcytz"));
        assert!(!written.interrupt, "the turn itself goes on");
        assert!(written.text.is_empty());
    }

    /// A turn with no process of its own to talk to is cancelled exactly as it always was.
    #[tokio::test]
    async fn stopping_a_turn_without_a_live_process_cancels_as_before() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("one-shot-stop");
        unrooted_chat(&state, &chat).await;

        *fake.delay.lock().unwrap() = Some(Duration::from_secs(30));
        let turn = send_message(&state, &chat, "devagar", Origin::Shell)
            .await
            .unwrap();
        the_turn_has_its_handle(&state, turn).await;
        assert!(!is_steerable(&chat), "an unrooted chat keeps no process");

        let stopped = stop_turn(&state, turn).await;

        assert!(matches!(stopped, Stopped::Killed));
        settled_turn(&state.pool, turn).await;
        assert_eq!(run_status(&state.pool, turn).await.0, "cancelled");
    }

    /// Send now goes into the turn that is running, and nowhere else.
    ///
    /// The text reaches the process's stdin as a plain user line, a `chat_said_now` row remembers
    /// where it was said, and the queue is never involved — a queued copy would be sent a second
    /// time when the turn ended.
    #[tokio::test]
    async fn saying_now_writes_into_the_running_turn_and_never_queues() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("said-now-chat");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        *fake.delay.lock().unwrap() = Some(Duration::from_secs(2));
        let second = send_message(&state, &chat, "segundo", Origin::Shell)
            .await
            .unwrap();
        the_process_is_steerable(&chat).await;

        let said = say_now(&state, &chat, "e também isto").await;
        assert!(
            matches!(said, Ok(SaidNow::Injected)),
            "a live steerable turn takes the text"
        );

        let (run_id, text, origin): (i64, String, String) =
            sqlx::query_as("SELECT run_id, text, origin FROM chat_said_now WHERE chat_id = ?")
                .bind(&chat)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(run_id, second);
        assert_eq!(text, "e também isto");
        assert_eq!(origin, Origin::Shell.as_wire());

        for _ in 0..300 {
            let arrived = fake
                .later_turns
                .lock()
                .unwrap()
                .iter()
                .any(|turn| turn.text == "e também isto" && !turn.interrupt);
            if arrived {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            fake.later_turns
                .lock()
                .unwrap()
                .iter()
                .any(|turn| turn.text == "e também isto" && !turn.interrupt),
            "the text reached the process as an ordinary line, not an interrupt"
        );

        let (status, _) = settled_turn(&state.pool, second).await;
        assert_eq!(status, "completed");
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_queue WHERE chat_id = ?")
            .bind(&chat)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(queued, 0, "Send now never touches the queue");
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE chat_id = ?")
            .bind(&chat)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            runs, 2,
            "the text was not sent a second time as a turn of its own"
        );
        LIVE_CHATS.lock().unwrap().remove(&chat);
    }

    /// With nothing to steer, Send now is just a message: sent, or queued behind a busy turn.
    #[tokio::test]
    async fn saying_now_without_a_live_turn_queues_as_before() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("said-now-unrooted");
        unrooted_chat(&state, &chat).await;

        *fake.delay.lock().unwrap() = Some(Duration::from_secs(30));
        let turn = send_message(&state, &chat, "devagar", Origin::Shell)
            .await
            .unwrap();
        the_turn_has_its_handle(&state, turn).await;

        let said = say_now(&state, &chat, "agora").await;

        assert!(
            matches!(said, Ok(SaidNow::Sent(Sent::Queued))),
            "a turn with no process behind it cannot be steered"
        );
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_queue WHERE chat_id = ?")
            .bind(&chat)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(queued, 1);
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chat_said_now WHERE chat_id = ?")
                .bind(&chat)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recorded, 0, "nothing was said into a turn");

        crate::execution::runs::finalize_termination(&state, turn, "cancelled").await;
    }

    /// Send now does not carry a message across a change of permission mode.
    ///
    /// The running turn was launched under the mode it snapshot into `runs.permission_mode`. Words
    /// written into it after the person moved the chat to another rung would be acted on under the
    /// old one, so the message waits for a turn that launches under the new one.
    #[tokio::test]
    async fn saying_now_does_not_cross_a_changed_permission_mode() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("said-now-mode");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        *fake.delay.lock().unwrap() = Some(Duration::from_secs(2));
        let second = send_message(&state, &chat, "segundo", Origin::Shell)
            .await
            .unwrap();
        the_process_is_steerable(&chat).await;
        crate::chats::set_permission_mode(&state.pool, &chat, crate::chats::PermissionMode::Plan)
            .await
            .unwrap();

        let said = say_now(&state, &chat, "agora").await;

        assert!(
            matches!(said, Ok(SaidNow::Sent(Sent::Queued))),
            "the turn runs under another mode than the chat now holds"
        );
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chat_said_now WHERE chat_id = ?")
                .bind(&chat)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recorded, 0);
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_queue WHERE chat_id = ?")
            .bind(&chat)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(queued, 1);
        // When turn 2 ends the queued message is drained and starts as a turn of its own.
        let mut third: Option<i64> = None;
        for _ in 0..500 {
            third = sqlx::query_scalar(
                "SELECT id FROM runs WHERE chat_id = ? AND id > ? ORDER BY id LIMIT 1",
            )
            .bind(&chat)
            .bind(second)
            .fetch_optional(&state.pool)
            .await
            .unwrap();
            if third.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let third = third.expect("the queued message launched as a turn of its own");
        // The row is inserted before the turn's task stamps its permission mode, so let it settle.
        settled_turn(&state.pool, third).await;
        let third_mode: Option<String> =
            sqlx::query_scalar("SELECT permission_mode FROM runs WHERE id = ?")
                .bind(third)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            third_mode.as_deref(),
            Some(crate::chats::PermissionMode::Plan.as_str())
        );
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_queue WHERE chat_id = ?")
            .bind(&chat)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(queued, 0);
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM chat_said_now WHERE chat_id = ?")
                .bind(&chat)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recorded, 0);
        LIVE_CHATS.lock().unwrap().remove(&chat);
    }

    /// The spontaneous turn's barrier is the process's, not the newest row's: a newer row for the
    /// chat that never reached this process (stopped before it stamped its mode) must not reset it.
    #[tokio::test]
    async fn a_spontaneous_turn_takes_its_barrier_from_its_process_not_the_newest_row() {
        let (fake, unprompted) = fake_with_a_background_task();
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("spontaneous-process-barrier");
        let _root = rooted_chat(&state, &chat).await;
        crate::chats::set_permission_mode(&state.pool, &chat, crate::chats::PermissionMode::Manual)
            .await
            .unwrap();

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();
        // A newer turn that was stopped before it stamped anything.
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, chat_id, answered_by, read_untrusted,
                               permission_mode, created_at)
             VALUES ('parado', 'cancelled', 'assistant', ?, 'cloud', 0, NULL, ?)",
        )
        .bind(&chat)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&state.pool)
        .await
        .unwrap();

        unprompted.send(ANSWERS_ON_ITS_OWN.to_owned()).unwrap();
        let task = settled_task_turn(&state.pool, &chat)
            .await
            .expect("the unprompted answer was never recorded as a turn of its own");

        assert_eq!(
            task.6.as_deref(),
            Some("manual"),
            "the spontaneous turn took its mode from the newest row instead of its process"
        );
        assert_eq!(
            task.5, 1,
            "the spontaneous turn was cleaner than the turn its process served"
        );
    }

    /// A one-shot turn (here a Telegram one, which runs `McpOnly` and so never touches the kept
    /// process) must not stamp its own, wider mode onto the barrier of the process kept from an
    /// earlier turn: that process never served it.
    #[tokio::test]
    async fn a_one_shot_turn_does_not_change_the_barrier_of_a_kept_process() {
        let (fake, unprompted) = fake_with_a_background_task();
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("one-shot-barrier");
        let _root = rooted_chat(&state, &chat).await;
        crate::chats::set_brain(&state.pool, &chat, crate::chats::Brain::Cloud)
            .await
            .unwrap();
        crate::chats::set_permission_mode(&state.pool, &chat, crate::chats::PermissionMode::Manual)
            .await
            .unwrap();

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        crate::chats::set_permission_mode(&state.pool, &chat, crate::chats::PermissionMode::Bypass)
            .await
            .unwrap();
        let one_shot = send_message(&state, &chat, "de fora", Origin::Telegram)
            .await
            .unwrap();
        settled_turn(&state.pool, one_shot).await;

        unprompted.send(ANSWERS_ON_ITS_OWN.to_owned()).unwrap();
        let task = settled_task_turn(&state.pool, &chat)
            .await
            .expect("the unprompted answer was never recorded as a turn of its own");

        assert_eq!(
            task.6.as_deref(),
            Some("manual"),
            "a one-shot turn's mode leaked into the barrier of a process that never served it"
        );
    }

    /// `read_untrusted` is the strictest of every turn the process served, not just the last one.
    #[tokio::test]
    async fn a_spontaneous_turn_keeps_read_untrusted_from_any_turn_its_process_served() {
        let (fake, unprompted) = fake_with_a_background_task();
        let mut state = test_state().await;
        state.runner = fake.clone();
        let chat = a_chat("spontaneous-any-untrusted");
        let _root = rooted_chat(&state, &chat).await;

        let first = send_message(&state, &chat, "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        let second = send_message(&state, &chat, "segundo", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;
        sqlx::query("UPDATE runs SET read_untrusted = 1 WHERE id = ?")
            .bind(first)
            .execute(&state.pool)
            .await
            .unwrap();

        unprompted.send(ANSWERS_ON_ITS_OWN.to_owned()).unwrap();
        let task = settled_task_turn(&state.pool, &chat)
            .await
            .expect("the unprompted answer was never recorded as a turn of its own");

        assert_eq!(
            task.5, 1,
            "a clean latest turn hid an earlier one that read untrusted text"
        );
        assert!(
            task.6.is_some(),
            "the spontaneous turn has no permission mode"
        );
    }

    /// A process that dies mid-turn says WHY, in the conversation rather than only in a log.
    ///
    /// The case that actually happens is the tool-policy barrier: the runner kills a CLI whose
    /// `init` advertised tools its policy forbids, and the message it leaves names them. A turn cut
    /// off by that showed "the conversation's process ended in the middle of this turn" and the one
    /// useful fact went somewhere only whoever reads the daemon's log can see.
    #[tokio::test]
    async fn a_process_that_dies_mid_turn_says_why() {
        let (mut live, said, events, why) = live_chat_for_testing();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        // What the supervisor does when the runner hands it back a process that stopped badly.
        why.send(Some("advertised Bash under McpOnly".to_owned()))
            .unwrap();
        drop(events);

        let LiveTurn::DiedMidTurn(reason) = live.turn("faz isso", &[], &transcript, None).await
        else {
            panic!("a process that never answered must not report a turn");
        };

        assert_eq!(reason.as_deref(), Some("advertised Bash under McpOnly"));
        // It really was written, which is what makes this the case that may not be retried.
        drop(said);
    }

    /// A conversation in planning launches a run that cannot act.
    ///
    /// Planning has existed since the runs pillar was built and every conversation passed `false`.
    /// `cli_args` turns the rung into `--permission-mode plan`, and since the fold it is one field
    /// holding one value — so a planning run cannot also be carrying `bypassPermissions`, by
    /// construction rather than by an ordering rule. It is the one mode a person reaches for before
    /// letting an agent near a codebase, and it was reachable by every kind of run here except the
    /// kind a person is watching.
    #[tokio::test]
    async fn a_conversation_in_planning_launches_a_run_that_cannot_act() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "planning-chat").await;
        crate::chats::set_permission_mode(
            &state.pool,
            "planning-chat",
            crate::chats::PermissionMode::Plan,
        )
        .await
        .unwrap();

        let id = send_message(&state, "planning-chat", "como farias isto?", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        assert_eq!(
            *fake.last_permission.lock().unwrap(),
            Some(crate::execution::runner::Permission::Plan)
        );
    }

    /// The row says what this turn was governed by, which is what the hook reads back.
    ///
    /// `accept_edits` rather than `plan`, on purpose: `plan` is also a CLI flag, so a test using it
    /// could pass on the flag alone and say nothing about the column. This rung and `manual` and
    /// `auto` are invisible on the command line — they live entirely in what the hook lets through
    /// — so the column is the only place the fact can be.
    #[tokio::test]
    async fn a_cloud_turn_records_the_mode_it_started_with() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "recorded").await;
        crate::chats::set_permission_mode(
            &state.pool,
            "recorded",
            crate::chats::PermissionMode::AcceptEdits,
        )
        .await
        .unwrap();

        let id = send_message(&state, "recorded", "muda isto", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT permission_mode FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(recorded.as_deref(), Some("accept_edits"));
    }

    /// A turn the local brain answered records NOTHING, and the NULL is the invariant.
    ///
    /// `spawn_local_turn` writes its own INSERT four hundred lines from the cloud one, never builds
    /// a `RunRequest`, and never fires a `PreToolUse` hook — so there are no permissions to govern
    /// on that path. Filling the column there would be the easiest possible version of this feature
    /// to write and the one that does nothing: green tests, and `NULL` on every turn that actually
    /// runs a CLI. `permission_mode IS NULL` means "no CLI was involved", and this is what keeps
    /// that true.
    #[tokio::test]
    async fn a_turn_the_local_brain_answered_records_no_mode_at_all() {
        let local = AppState {
            assistants: Arc::new(FixedAssistants(fake_local_assistant("answered here"))),
            ..test_state().await
        };
        let chat_id = a_chat("a-local-chat");
        crate::chats::set_brain(&local.pool, &chat_id, crate::chats::Brain::Local)
            .await
            .unwrap();
        crate::chats::set_permission_mode(
            &local.pool,
            &chat_id,
            crate::chats::PermissionMode::Bypass,
        )
        .await
        .unwrap();

        let id = send_message(&local, &chat_id, "olá", Origin::Shell)
            .await
            .unwrap();

        let recorded: Option<String> =
            sqlx::query_scalar("SELECT permission_mode FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&local.pool)
                .await
                .unwrap();
        assert_eq!(
            recorded, None,
            "the conversation says `bypass` and no CLI is being launched, so the run must say \
             nothing rather than claim a mode nothing will read"
        );
    }

    /// A turn whose mode could not be written down is refused, not run under a wider one.
    ///
    /// The degrade is only safe downward. Falling back to `auto` here would hand somebody who chose
    /// `manual` a turn that edits without asking — and it would do it silently, on the one path
    /// where nothing else would say so. `mint_chat_token`, which keys the process, fails in the same
    /// direction, and this is the same argument.
    ///
    /// The write is broken with a trigger because that is the only honest way to fail exactly this
    /// statement from outside: the column cannot be dropped without rebuilding `runs`, and breaking
    /// the table wholesale would fail the turn somewhere earlier and prove nothing about this line.
    #[tokio::test]
    async fn a_turn_whose_mode_could_not_be_recorded_is_refused_rather_than_widened() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "unrecordable").await;
        crate::chats::set_permission_mode(
            &state.pool,
            "unrecordable",
            crate::chats::PermissionMode::Manual,
        )
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER no_permission_mode BEFORE UPDATE OF permission_mode ON runs
             BEGIN SELECT RAISE(ABORT, 'the column will not take a value today'); END",
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let id = send_message(&state, "unrecordable", "faz isso", Origin::Shell)
            .await
            .unwrap();
        let (status, _) = settled_turn(&state.pool, id).await;

        assert_eq!(status, "failed");
        assert_eq!(
            *fake.calls.lock().unwrap(),
            0,
            "the CLI must not have been launched at all — a run that started is a run that acted"
        );
    }

    /// A conversation that changed its mind is not answered by the process it changed it from.
    ///
    /// `--permission-mode plan` is an argument, fixed when the process was spawned: one started to
    /// act cannot be asked to stop, and one started to plan cannot be let loose. Speaking down the
    /// old one would answer under the mode somebody had just turned off — and the transcript would
    /// look exactly right.
    #[tokio::test]
    async fn a_conversation_that_changed_its_mind_is_not_answered_by_the_old_process() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "mind-changed").await;

        let first = send_message(&state, "mind-changed", "faz isso", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;

        crate::chats::set_permission_mode(
            &state.pool,
            "mind-changed",
            crate::chats::PermissionMode::Plan,
        )
        .await
        .unwrap();
        let second = send_message(&state, "mind-changed", "afinal planeia", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            2,
            "the turn was answered by a process started in the other mode"
        );
        assert_eq!(
            *fake.last_permission.lock().unwrap(),
            Some(crate::execution::runner::Permission::Plan)
        );
    }

    /// A conversation with no tools keeps no process, and the reason is not caution.
    ///
    /// A rooted turn carries a key scoped to its conversation, which stays true as the turns change
    /// under it. An `McpOnly` turn carries the daemon's CONTROL token — safe only because that
    /// policy leaves it no Bash, no Read and no Write to look at its own environment with — and a
    /// process holding that key, kept alive and idle between turns, is a different and much worse
    /// proposition. The barrier that earns a conversation its tools is the same one that lets it
    /// keep a process.
    #[tokio::test]
    async fn a_conversation_without_tools_starts_a_process_for_every_turn() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();

        // No directory and no hook: `tool_policy_for` answers `McpOnly`.
        for text in ["primeiro", "segundo"] {
            let id = send_message(&state, "cold-chat", text, Origin::Shell)
                .await
                .unwrap();
            settled_turn(&state.pool, id).await;
        }

        assert_eq!(*fake.calls.lock().unwrap(), 2);
    }

    /// A rooted turn carries a key scoped to the CONVERSATION, not to the turn.
    ///
    /// The turn is what gets a `runs` row, so a key naming the run is the obvious thing and is what
    /// this handed out until now. It is also the single reason a CLI process cannot outlive its
    /// turn: a process is given its environment once, at spawn, so on turn two it would still be
    /// presenting turn one's key — which `auth::resolve` has by then retired, and which would name
    /// the wrong turn if it had not.
    ///
    /// Measured before it was built, on the real CLI: a second turn fed down a live process's stdin
    /// reaches `init` in 1.5s against 5.8s for a fresh spawn that resumes, runs its first shell
    /// command at 6.7s against 26.4s, and finishes in 12.0s against 26-32s. None of it is cost —
    /// turn two's price was inside the noise of a resumed spawn's, because the prompt cache lives at
    /// the API and not in the process.
    #[tokio::test]
    async fn a_rooted_turn_carries_a_key_scoped_to_its_conversation() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();

        let _root = rooted_chat(&state, "keyed-chat").await;

        let id = send_message(&state, "keyed-chat", "arranja o parser", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        let env = fake.last_env.lock().unwrap().clone().unwrap_or_default();
        let key = env
            .iter()
            .find(|(name, _)| name == "NUCLEOS_DAEMON_TOKEN")
            .map(|(_, value)| value.clone())
            .expect("a rooted turn is given a key");

        assert!(
            key.starts_with("chat:keyed-chat."),
            "expected a key naming the conversation, got {key}"
        );
        // The thing this key exists to not be. A rooted turn has Bash, Read and Write, so it can
        // read its own environment — which is exactly why it must not find the control token there.
        assert_ne!(key, state.token.0);
    }

    /// A kept CLI process still holds a key that resolves on its second turn.
    ///
    /// The process was given its environment once, at spawn, so the key it presents on turn two
    /// is the one turn one handed it. If turn two rotated the stored secret, that key would be
    /// answered 401 — the 401 on turn two of a kept process.
    #[tokio::test]
    async fn a_kept_process_still_holds_a_key_that_resolves_on_the_second_turn() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "kept-key").await;

        let first = send_message(&state, "kept-key", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        let env = fake.last_env.lock().unwrap().clone().unwrap_or_default();
        let key1 = env
            .iter()
            .find(|(name, _)| name == "NUCLEOS_DAEMON_TOKEN")
            .map(|(_, value)| value.clone())
            .expect("a rooted turn is given a key");

        let second = send_message(&state, "kept-key", "segundo", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            1,
            "the second turn started a second process instead of speaking to the first"
        );
        let stored: String =
            sqlx::query_scalar("SELECT token FROM chat_tokens WHERE chat_id = 'kept-key'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            key1,
            format!("chat:kept-key.{stored}"),
            "the key the kept process holds no longer matches the stored secret"
        );
    }

    /// A fresh process is given a fresh key, and the old one is retired.
    ///
    /// Rotation belongs to a fresh spawn: the process that held the old key is gone, so the stored
    /// secret may change, and the new process must hold the new one.
    #[tokio::test]
    async fn a_fresh_process_is_given_a_fresh_key_that_retires_the_old_one() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "rekeyed").await;

        let first = send_message(&state, "rekeyed", "faz isso", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        let env = fake.last_env.lock().unwrap().clone().unwrap_or_default();
        let key1 = env
            .iter()
            .find(|(name, _)| name == "NUCLEOS_DAEMON_TOKEN")
            .map(|(_, value)| value.clone())
            .expect("a rooted turn is given a key");

        crate::chats::set_permission_mode(
            &state.pool,
            "rekeyed",
            crate::chats::PermissionMode::Plan,
        )
        .await
        .unwrap();
        let second = send_message(&state, "rekeyed", "afinal planeia", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            2,
            "the turn was answered by a process started in the other mode"
        );
        let env = fake.last_env.lock().unwrap().clone().unwrap_or_default();
        let key2 = env
            .iter()
            .find(|(name, _)| name == "NUCLEOS_DAEMON_TOKEN")
            .map(|(_, value)| value.clone())
            .expect("a fresh process is given a key");
        let stored: String =
            sqlx::query_scalar("SELECT token FROM chat_tokens WHERE chat_id = 'rekeyed'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_ne!(key1, key2, "a fresh process was handed the old key");
        assert_eq!(key2, format!("chat:rekeyed.{stored}"));
    }

    /// A kept process is replaced when the chat denies a tool after it was spawned.
    ///
    /// The denial is a spawn-time flag: a process started without it would go on using the tool for
    /// as long as it was kept, whatever the chat now says.
    #[tokio::test]
    async fn a_kept_process_is_replaced_when_the_chat_denies_a_tool() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "deny-kept").await;

        let first = send_message(&state, "deny-kept", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        assert_eq!(*fake.calls.lock().unwrap(), 1);

        crate::chats::set_denied_tools(&state.pool, "deny-kept", &["Bash".to_owned()])
            .await
            .unwrap();
        let second = send_message(&state, "deny-kept", "segundo", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            2,
            "the turn was answered by a process that still had the tool the chat now denies"
        );
        assert_eq!(
            *fake.last_denied_tools.lock().unwrap(),
            Some(vec!["Bash".to_owned()])
        );
    }

    /// A kept process is replaced when the chat's effort changes after it was spawned.
    #[tokio::test]
    async fn a_kept_process_is_replaced_when_the_effort_changes() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "effort-kept").await;

        let first = send_message(&state, "effort-kept", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, first).await;
        assert_eq!(*fake.calls.lock().unwrap(), 1);

        crate::chats::set_effort(&state.pool, "effort-kept", Some("high"))
            .await
            .unwrap();
        let second = send_message(&state, "effort-kept", "segundo", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, second).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            2,
            "the turn was answered by a process started at the old effort"
        );
        assert_eq!(
            *fake.last_effort.lock().unwrap(),
            Some(Some("high".to_owned()))
        );
    }

    /// Opening a rooted chat starts its process before the first message, and that message is
    /// then served by it instead of by a second one.
    #[tokio::test]
    async fn opening_a_rooted_chat_warms_its_process_before_the_first_turn() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();
        let _root = rooted_chat(&state, "warm-open").await;
        upsert_session(
            &state.pool,
            "warm-open",
            "sess-warm",
            "2026-01-01T00:00:00Z",
        )
        .await
        .unwrap();

        prewarm_open_chat(&state, "warm-open");

        let mut kept = false;
        for _ in 0..500 {
            kept = *fake.calls.lock().unwrap() == 1
                && LIVE_CHATS.lock().unwrap().contains_key("warm-open");
            if kept {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(kept, "opening the chat did not leave a kept process behind");
        assert_eq!(
            fake.last_prompt.lock().unwrap().as_deref(),
            Some(""),
            "a warmed process is started with no opening message"
        );
        assert_eq!(
            fake.last_resume.lock().unwrap().as_deref(),
            Some("sess-warm"),
            "a warmed process resumes the conversation it was opened for"
        );

        let id = send_message(&state, "warm-open", "primeiro", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        assert_eq!(
            *fake.calls.lock().unwrap(),
            1,
            "the first turn started a second process instead of using the warmed one"
        );
        let later = fake.later_turns.lock().unwrap().clone();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].text, "primeiro");
    }

    /// Warming is only for a rooted chat that could resume, is idle, has no process, is not already
    /// being warmed, and leaves room under the cap.
    #[test]
    fn warming_is_declined_for_unrooted_busy_kept_or_already_warming_chats() {
        use crate::execution::runner::ToolPolicy;
        // rooted, cli, policy, has_session, busy, kept, warming, alive
        assert!(should_prewarm(
            true,
            "claude",
            ToolPolicy::Unrestricted,
            true,
            false,
            false,
            false,
            0
        ));
        assert!(!should_prewarm(
            false,
            "claude",
            ToolPolicy::Unrestricted,
            true,
            false,
            false,
            false,
            0
        ));
        assert!(!should_prewarm(
            true,
            "codex",
            ToolPolicy::Unrestricted,
            true,
            false,
            false,
            false,
            0
        ));
        assert!(!should_prewarm(
            true,
            "claude",
            ToolPolicy::McpOnly,
            true,
            false,
            false,
            false,
            0
        ));
        assert!(!should_prewarm(
            true,
            "claude",
            ToolPolicy::Unrestricted,
            false,
            false,
            false,
            false,
            0
        ));
        assert!(!should_prewarm(
            true,
            "claude",
            ToolPolicy::Unrestricted,
            true,
            true,
            false,
            false,
            0
        ));
        assert!(!should_prewarm(
            true,
            "claude",
            ToolPolicy::Unrestricted,
            true,
            false,
            true,
            false,
            0
        ));
        assert!(!should_prewarm(
            true,
            "claude",
            ToolPolicy::Unrestricted,
            true,
            false,
            false,
            true,
            0
        ));
        assert!(!should_prewarm(
            true,
            "claude",
            ToolPolicy::Unrestricted,
            true,
            false,
            false,
            false,
            LIVE_CAP
        ));
    }

    /// A turn a kept process will serve skips the briefing: the process would discard it, since the
    /// block travels as a per-spawn system flag.
    #[test]
    fn a_turn_a_kept_process_will_serve_skips_the_briefing() {
        assert!(briefing_wanted("claude", false));
        assert!(!briefing_wanted("claude", true));
        assert!(!briefing_wanted("codex", false));
    }

    /// A finished turn is unregistered before the message that waited for it starts, so Stop on the
    /// waiting one never finds the finished one's handle.
    #[tokio::test]
    async fn a_finished_turn_is_unregistered_before_what_waited_starts() {
        let mut state = test_state().await;
        state.runner = Arc::new(FakeCommandRunner {
            delay: Mutex::new(Some(Duration::from_millis(300))),
            ..Default::default()
        });
        let chat_id = "unregistered-before-drain";

        let first = send_message(&state, chat_id, "primeiro", Origin::Shell)
            .await
            .unwrap();
        assert_eq!(
            send_or_queue(&state, chat_id, "segundo", &[], Origin::Shell).await,
            Ok(Sent::Queued)
        );

        let mut second_row = None;
        for _ in 0..5000 {
            second_row = sqlx::query_scalar::<_, i64>("SELECT id FROM runs WHERE id > ? LIMIT 1")
                .bind(first)
                .fetch_optional(&state.pool)
                .await
                .unwrap();
            if second_row.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(second_row.is_some(), "what waited never started");
        assert!(
            !state.run_handles.lock().unwrap().contains_key(&first),
            "the finished turn was still registered when what waited began"
        );
    }

    /// A turn carrying nothing keeps the argument vector it has always used.
    #[tokio::test]
    async fn a_turn_sent_with_nothing_keeps_the_argument_vector() {
        let fake = std::sync::Arc::new(FakeCommandRunner::default());
        let mut state = test_state().await;
        state.runner = fake.clone();

        let id = send_message(&state, "plain-chat", "arranja o parser", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        assert!(
            fake.last_images
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_default()
                .is_empty()
        );
        assert_eq!(*fake.last_steerable.lock().unwrap(), Some(false));
    }

    /// What was sent is kept, or the conversation shows a question about a picture nobody can see.
    ///
    /// Paths and not bytes: `runs` is read on every transcript poll, and a column holding base64
    /// screenshots would drag megabytes through queries that want a prompt and a status.
    #[tokio::test]
    async fn a_picture_sent_with_a_turn_is_kept_where_it_can_be_opened_again() {
        // Through `ensure_root`, the way every other test of the files pillar builds one: a bare
        // temp path is not a root — `resolve_within` canonicalises, and on Windows a canonical path
        // carries a prefix a raw one does not, so every write under it reads as an escape.
        let dir = tempfile::tempdir().unwrap();
        let root = crate::files::ensure_root(dir.path()).unwrap();
        let mut state = test_state().await;
        state.files_root = Some(root.clone());
        let images = vec![crate::execution::runner::Attachment {
            media_type: "image/png".into(),
            // "hello" — not a real PNG, and nothing here claims to check.
            data: "aGVsbG8=".into(),
        }];

        let id = send_message_with(
            &state,
            "kept-chat",
            "que cor e esta?",
            &images,
            Origin::Shell,
        )
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
        assert!(paths[0].ends_with(".png"), "{paths:?}");
        // And the bytes are actually there, under the files root, where the window can ask for them.
        let written = root.join(&paths[0]);
        assert_eq!(std::fs::read(&written).unwrap(), b"hello");
    }

    /// A turn that carried none records an EMPTY list, never nothing at all.
    ///
    /// NULL is what a turn from before the column has, and the two are different facts: one is a
    /// turn known to have carried nothing, the other is a turn nobody asked.
    #[tokio::test]
    async fn a_turn_that_carried_no_picture_records_an_empty_list() {
        let state = test_state().await;

        let id = send_message(&state, "empty-chat", "arranja o parser", Origin::Shell)
            .await
            .unwrap();

        let stored: Option<String> =
            sqlx::query_scalar("SELECT prompt_images FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();

        assert_eq!(stored.as_deref(), Some("[]"));
    }

    /// An origin has to survive being written down, or a queued Telegram turn comes back as the
    /// shell's and is answered into the wrong place.
    #[test]
    fn an_origin_written_down_reads_back_as_itself() {
        for origin in [Origin::Shell, Origin::Telegram] {
            assert_eq!(Origin::from_wire(Some(origin.as_wire())), origin);
        }
    }

    /// How much a turn THOUGHT outlives the stream it thought it in, as what it did does.
    ///
    /// The live tail is discarded the moment the turn ends, so without a column this is visible
    /// while the turn runs and gone for ever afterwards. Written from a real stream's shape: the
    /// thinking block arrives with its text stripped and the size arrives on a `system` line beside
    /// it, which is the only part of a thought this machine is given.
    #[tokio::test]
    async fn a_finished_turn_records_how_much_it_thought() {
        let mut state = test_state().await;
        state.runner = ran_a_tool(&[
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":177,"estimated_tokens_delta":27}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"","signature":"ErwFCqUB"}]}}"#,
            r#"{"type":"result","subtype":"success","result":"é o parser de datas"}"#,
        ]);

        let id = send_message(&state, "assistant-thought-chat", "arranja", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        let (thought, tokens): (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT thought, thought_tokens FROM runs WHERE id = ?")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap();

        assert_eq!(tokens, Some(177));
        // And the words are recorded as the nothing they were, rather than as an empty string
        // pretending to be a thought.
        assert_eq!(thought.as_deref(), Some("[]"));
    }

    /// A turn that only talked records an EMPTY list, not nothing at all.
    ///
    /// NULL is what a turn from before this column has, and the two are different facts: one is a
    /// turn known to have acted on nothing, the other is a turn nobody asked. Only the first can be
    /// said out loud.
    #[tokio::test]
    async fn a_turn_that_only_talked_records_an_empty_list_rather_than_nothing() {
        let mut state = test_state().await;
        state.runner = ran_a_tool(&[r#"{"type":"result","subtype":"success","result":"olá"}"#]);

        let id = send_message(&state, "assistant-talked-chat", "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        let stored: Option<String> = sqlx::query_scalar("SELECT tools_used FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();

        assert_eq!(stored.as_deref(), Some("[]"));
    }

    /// Records a finished exchange in a chat, the way a turn that completed leaves one.
    async fn past_exchange(pool: &SqlitePool, chat_id: &str, asked: &str, answered: &str) {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, session_id, chat_id, stdout, created_at)
             VALUES (?, 'completed', 'assistant', 'old-session', ?, ?, '2026-08-11T10:00:00+00:00')",
        )
        .bind(asked)
        .bind(chat_id)
        .bind(answered)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A conversation that cannot resume is READ BACK to the model instead of starting blank.
    ///
    /// The cases that still reach it are narrow now — a turn that read third-party text, somebody
    /// asking for a fresh context, and a picked-up session larger than any model window — but each
    /// of them genuinely ends one context and begins another, and a model that begins the next one
    /// blank is a stranger answering in the same thread. The local path has always replayed its
    /// history for want of a session protocol; this is the same answer to the same problem.
    #[tokio::test]
    async fn a_turn_that_cannot_resume_is_replayed_the_conversation_so_far() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner::default());
        state.runner = runner.clone();
        let chat_id = "assistant-restarted-chat";
        past_exchange(&state.pool, chat_id, "arranja o parser", "está arranjado").await;

        let id = send_message(&state, chat_id, "e os testes?", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        let launched = runner.last_prompt.lock().unwrap().clone().unwrap();
        assert!(launched.contains("arranja o parser"), "{launched}");
        assert!(launched.contains("está arranjado"), "{launched}");
        assert!(launched.contains("e os testes?"), "{launched}");

        // The ROW keeps what the person typed. It is what the list shows, what the next replay
        // reads, and what a person recognises as their own message — a row carrying the preamble
        // would grow a copy of the conversation into every turn of it.
        let stored: String = sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(stored, "e os testes?");
    }

    /// And a turn that CAN resume is handed nothing but the message.
    ///
    /// The session already holds those exchanges. Replaying them into it would put the conversation
    /// in the window twice and invite the model to answer the older question again.
    #[tokio::test]
    async fn a_turn_that_resumes_is_replayed_nothing() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner::default());
        state.runner = runner.clone();
        let chat_id = "assistant-resuming-chat";
        past_exchange(&state.pool, chat_id, "arranja o parser", "está arranjado").await;
        upsert_session(
            &state.pool,
            chat_id,
            "still-good",
            "2026-08-11T10:00:00+00:00",
        )
        .await
        .unwrap();

        let id = send_message(&state, chat_id, "e os testes?", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        assert_eq!(
            runner.last_prompt.lock().unwrap().clone().unwrap(),
            "e os testes?"
        );
    }

    /// A conversation with nothing behind it is not given an empty preamble.
    #[tokio::test]
    async fn a_first_turn_is_replayed_nothing_because_there_is_nothing() {
        let mut state = test_state().await;
        let runner = Arc::new(FakeCommandRunner::default());
        state.runner = runner.clone();

        let id = send_message(&state, "assistant-brand-new-chat", "olá", Origin::Shell)
            .await
            .unwrap();
        settled_turn(&state.pool, id).await;

        assert_eq!(runner.last_prompt.lock().unwrap().clone().unwrap(), "olá");
    }

    // ---- chat turn silence deadline -------------------------------------------------------------

    /// The request a rooted live turn is served with: `take_live` only reuses a process standing on
    /// the same ground (here no cwd, default permission) and resuming the session it already has.
    fn silence_request() -> crate::execution::runner::RunRequest {
        let mut request = seam_request("go", &std::env::temp_dir(), Some("s-1".into()));
        request.cwd = None;
        request
    }

    /// A canned one-shot answer: `lines` assistant events and a closing `result`.
    fn silence_canned(lines: usize) -> crate::execution::runner::RunOutcome {
        let mut stdout = String::new();
        for n in 0..lines {
            stdout.push_str(&format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"part {n}"}}]}}}}"#
            ));
            stdout.push('\n');
        }
        stdout.push_str(r#"{"type":"result","subtype":"success","result":"done"}"#);
        crate::execution::runner::RunOutcome {
            exit_code: 0,
            stdout,
            stderr: String::new(),
            session_id: Some("s-1".to_owned()),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            num_turns: None,
            compacted: false,
        }
    }

    fn silence_runner(
        lines: usize,
        delay: Duration,
    ) -> std::sync::Arc<dyn crate::execution::runner::CommandRunner> {
        Arc::new(FakeCommandRunner {
            canned: Mutex::new(Some(silence_canned(lines))),
            delay: Mutex::new(Some(delay)),
            ..Default::default()
        })
    }

    #[test]
    fn chat_silence_defaults_give_a_rooted_turn_ten_minutes_of_silence_and_four_hours() {
        let deadlines = turn_deadlines(
            crate::state::DEFAULT_RUN_TIMEOUT,
            crate::state::DEFAULT_PROGRESS_TIMEOUT,
            crate::execution::runner::ToolPolicy::Unrestricted,
        );
        assert_eq!(
            deadlines,
            TurnDeadlines {
                silence: Some(Duration::from_secs(600)),
                ceiling: Duration::from_secs(14_400),
            }
        );
    }

    #[test]
    fn chat_silence_an_mcp_only_turn_keeps_the_run_timeout_and_no_silence_rule() {
        let deadlines = turn_deadlines(
            crate::state::DEFAULT_RUN_TIMEOUT,
            crate::state::DEFAULT_PROGRESS_TIMEOUT,
            crate::execution::runner::ToolPolicy::McpOnly,
        );
        assert_eq!(
            deadlines,
            TurnDeadlines {
                silence: None,
                ceiling: Duration::from_secs(600),
            }
        );
    }

    #[tokio::test]
    async fn chat_silence_a_live_rooted_turn_that_keeps_streaming_outlives_the_run_timeout() {
        let chat_id = "chat-silence-live-streaming";
        let (live, _said, events, _why) = live_chat_for_testing();
        LIVE_CHATS.lock().unwrap().insert(chat_id.to_owned(), live);
        let deadlines = turn_deadlines(
            Duration::from_millis(100),
            Duration::from_millis(100),
            crate::execution::runner::ToolPolicy::Unrestricted,
        );
        let feeder = tokio::spawn(async move {
            for n in 0..20 {
                let _ = events.send(crate::execution::runner::TurnEvent::Line(format!(
                    "line {n}"
                )));
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            let _ = events.send(crate::execution::runner::TurnEvent::Ended(
                crate::execution::runner::TurnOutcome::default(),
            ));
            events
        });
        let runner: Arc<dyn crate::execution::runner::CommandRunner> =
            Arc::new(FakeCommandRunner::default());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(Mutex::new(String::new()));

        let started = std::time::Instant::now();
        let served = serve_turn(
            &runner,
            silence_request(),
            tx,
            &transcript,
            chat_id,
            deadlines,
            true,
            None,
            "",
        )
        .await;
        let elapsed = started.elapsed();
        let _events = feeder.await.unwrap();
        evict_live(chat_id);

        let outcome = served
            .expect("a turn that keeps streaming is not past any deadline")
            .expect("the process answered");
        assert_eq!(outcome.exit_code, 0);
        assert!(
            elapsed > Duration::from_millis(100),
            "the turn must outlast the old run_timeout wall, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn chat_silence_a_live_rooted_turn_that_goes_silent_is_stopped() {
        let chat_id = "chat-silence-live-silent";
        let (live, _said, events, _why) = live_chat_for_testing();
        LIVE_CHATS.lock().unwrap().insert(chat_id.to_owned(), live);
        let deadlines = turn_deadlines(
            Duration::from_millis(100),
            Duration::from_millis(100),
            crate::execution::runner::ToolPolicy::Unrestricted,
        );
        // One line, then the stream stays open and says nothing: `events` is held, not dropped.
        events
            .send(crate::execution::runner::TurnEvent::Line(
                "one line".to_owned(),
            ))
            .unwrap();
        let runner: Arc<dyn crate::execution::runner::CommandRunner> =
            Arc::new(FakeCommandRunner::default());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(Mutex::new(String::new()));

        let started = std::time::Instant::now();
        let served = serve_turn(
            &runner,
            silence_request(),
            tx,
            &transcript,
            chat_id,
            deadlines,
            true,
            None,
            "",
        )
        .await;
        let elapsed = started.elapsed();
        let still_kept = LIVE_CHATS.lock().unwrap().contains_key(chat_id);
        evict_live(chat_id);
        drop(events);

        let outcome = served
            .expect("silence is reported as an outcome, not as the ceiling elapsing")
            .expect("the turn was written");
        assert_eq!(
            outcome.exit_code,
            crate::execution::runner::PROGRESS_TIMEOUT_EXIT_CODE
        );
        assert!(outcome.stderr.contains("went silent"), "{}", outcome.stderr);
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
        assert!(!still_kept, "a process that went silent must be dropped");
    }

    #[tokio::test]
    async fn chat_silence_a_live_rooted_turn_past_the_ceiling_is_stopped_while_streaming() {
        let chat_id = "chat-silence-live-ceiling";
        let (live, _said, events, _why) = live_chat_for_testing();
        LIVE_CHATS.lock().unwrap().insert(chat_id.to_owned(), live);
        let deadlines = TurnDeadlines {
            silence: Some(Duration::from_millis(200)),
            ceiling: Duration::from_millis(400),
        };
        let feeder = tokio::spawn(async move {
            while events
                .send(crate::execution::runner::TurnEvent::Line(
                    "still going".to_owned(),
                ))
                .is_ok()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        let runner: Arc<dyn crate::execution::runner::CommandRunner> =
            Arc::new(FakeCommandRunner::default());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(Mutex::new(String::new()));

        let started = std::time::Instant::now();
        let served = serve_turn(
            &runner,
            silence_request(),
            tx,
            &transcript,
            chat_id,
            deadlines,
            true,
            None,
            "",
        )
        .await;
        let elapsed = started.elapsed();
        evict_live(chat_id);
        // The turn was dropped with the process, so the feeder's sends start failing and it ends.
        let _ = tokio::time::timeout(Duration::from_secs(2), feeder).await;

        assert!(
            served.is_err(),
            "the ceiling must stop a turn still streaming"
        );
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    }

    #[tokio::test]
    async fn chat_silence_a_one_shot_rooted_turn_that_keeps_streaming_outlives_the_run_timeout() {
        let deadlines = turn_deadlines(
            Duration::from_millis(100),
            Duration::from_millis(100),
            crate::execution::runner::ToolPolicy::Unrestricted,
        );
        let runner = silence_runner(20, Duration::from_millis(30));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(Mutex::new(String::new()));

        let served = serve_turn(
            &runner,
            silence_request(),
            tx,
            &transcript,
            "chat-silence-one-shot-streaming",
            deadlines,
            false,
            None,
            "",
        )
        .await
        .expect("a turn that keeps streaming is not past any deadline")
        .expect("the runner answered");

        assert_eq!(served.exit_code, 0);
    }

    #[tokio::test]
    async fn chat_silence_a_one_shot_rooted_turn_that_goes_silent_is_stopped() {
        let deadlines = TurnDeadlines {
            silence: Some(Duration::from_millis(100)),
            ceiling: Duration::from_secs(2),
        };
        let runner = silence_runner(5, Duration::from_millis(500));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(Mutex::new(String::new()));

        let served = serve_turn(
            &runner,
            silence_request(),
            tx,
            &transcript,
            "chat-silence-one-shot-silent",
            deadlines,
            false,
            None,
            "",
        )
        .await
        .expect("silence is the runner's outcome, not the ceiling elapsing")
        .expect("the runner answered");

        assert_eq!(
            served.exit_code,
            crate::execution::runner::PROGRESS_TIMEOUT_EXIT_CODE
        );
    }

    #[tokio::test]
    async fn chat_silence_a_one_shot_rooted_turn_past_the_ceiling_is_stopped_while_streaming() {
        let deadlines = TurnDeadlines {
            silence: Some(Duration::from_millis(200)),
            ceiling: Duration::from_millis(300),
        };
        let runner = silence_runner(100, Duration::from_millis(20));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(Mutex::new(String::new()));

        let started = std::time::Instant::now();
        let served = serve_turn(
            &runner,
            silence_request(),
            tx,
            &transcript,
            "chat-silence-one-shot-ceiling",
            deadlines,
            false,
            None,
            "",
        )
        .await;

        assert!(
            served.is_err(),
            "the ceiling must stop a turn still streaming"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn chat_silence_an_mcp_only_turn_still_dies_at_the_run_timeout() {
        let deadlines = turn_deadlines(
            Duration::from_millis(100),
            Duration::from_millis(100),
            crate::execution::runner::ToolPolicy::McpOnly,
        );
        let runner = silence_runner(5, Duration::from_secs(2));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let transcript = Arc::new(Mutex::new(String::new()));

        let started = std::time::Instant::now();
        let served = serve_turn(
            &runner,
            silence_request(),
            tx,
            &transcript,
            "chat-silence-mcp-only",
            deadlines,
            false,
            None,
            "",
        )
        .await;

        assert!(
            served.is_err(),
            "an McpOnly turn keeps the plain wall clock"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn chat_silence_a_rooted_turn_that_went_silent_is_recorded_timed_out() {
        let mut state = test_state().await;
        state.runner = silence_runner(5, Duration::from_secs(5));
        state.progress_timeout = Duration::from_millis(50);
        let _root = rooted_chat(&state, "silent-chat").await;

        let id = send_message(&state, "silent-chat", "go", Origin::Shell)
            .await
            .unwrap();
        let (status, _) = settled_turn(&state.pool, id).await;
        evict_live("silent-chat");

        assert_eq!(status, "timed_out");
        let stderr: Option<String> = sqlx::query_scalar("SELECT stderr FROM runs WHERE id = ?")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(
            stderr.unwrap_or_default().contains("went silent"),
            "the silence line must reach the row"
        );
    }

    /// The owner's opt-in reaches a turn only where ambient servers can be governed at all.
    ///
    /// `McpOnly` is the unrooted, Telegram and relayed shape: its whole point is that nothing but
    /// the NucleOS server is reachable, so no toggle may widen it. `None` offers no tools to widen.
    #[test]
    fn ambient_mcp_never_reaches_an_mcp_only_turn() {
        use crate::execution::runner::ToolPolicy;

        assert!(ambient_mcp_for(ToolPolicy::Unrestricted, true));
        assert!(!ambient_mcp_for(ToolPolicy::Unrestricted, false));
        assert!(!ambient_mcp_for(ToolPolicy::McpOnly, true));
        assert!(!ambient_mcp_for(ToolPolicy::McpOnly, false));
        assert!(!ambient_mcp_for(ToolPolicy::None, true));
    }
}
