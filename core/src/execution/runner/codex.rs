//! The Codex CLI backend: its arguments, MCP overrides, staged images, usage and `CodexCliRunner`.

use super::*;

/// What `run_prompt` prepared before building argv: MCP overrides, staged image paths and the
/// sandbox the runner pins.
#[derive(Debug, Default)]
pub struct CodexStaged {
    pub mcp_overrides: Vec<String>,
    pub images: Vec<std::path::PathBuf>,
    /// The sandbox this launch pins, or `None` to leave Codex's own resolution.
    pub sandbox_mode: Option<&'static str>,
}

/// The prefix Codex gives the daemon's own MCP tools: `mcp__<server>__<tool>`, server `nucleos`.
pub const DAEMON_MCP_TOOL_PREFIX: &str = "mcp__nucleos__";

/// The Codex features an `McpOnly` turn switches off (`-c features.<name>=false`).
pub const CODEX_MCP_ONLY_DISABLED_FEATURES: [&str; 10] = [
    "shell_tool",
    "browser_use",
    "browser_use_external",
    "in_app_browser",
    "computer_use",
    "image_generation",
    "apps",
    "plugins",
    "tool_suggest",
    "multi_agent",
];

/// The reason the deny hook prints. No character in it is syntax to `sh` or `cmd`.
pub const CODEX_MCP_ONLY_DENIAL: &str = "nucleos: this chat may only call the nucleos MCP tools";

/// A regex over tool names that matches every name NOT starting with `prefix`: for each position
/// `i`, the proper prefix `prefix[..i]` itself, or `prefix[..i]` followed by any character other
/// than `prefix[i]`. No lookahead, so it behaves the same under search and under full-match.
pub fn codex_deny_matcher(prefix: &str) -> String {
    assert!(
        prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_'),
        "the deny matcher prefix must be ASCII alphanumerics and underscores: {prefix}"
    );
    let chars: Vec<char> = prefix.chars().collect();
    let mut branches = Vec::new();
    for i in 0..chars.len() {
        let head: String = chars[..i].iter().collect();
        branches.push(head.clone());
        branches.push(format!("{head}[^{}].*", chars[i]));
    }
    format!("(?s)^(?:{})$", branches.join("|"))
}

/// The `-c hooks.PreToolUse=[...]` override that blocks every tool outside the daemon's own MCP
/// tools. Only exit code 2 plus stderr blocks a tool call; anything else (a missing interpreter,
/// a parse error, a crash) fails open, hence shell builtins only and no interpreter. Codex runs a
/// hook through the session's shell: `$SHELL -c` (or bash/zsh) on Unix, and PowerShell, never cmd,
/// on Windows (`shell_detect::default_user_shell`, 0.144.4). Measured live: cmd syntax there was a
/// PowerShell parse error, so the hook failed open and every tool ran.
///
/// KNOWN LIMITATION: Codex emits no init event, so the surviving tool set cannot be verified as
/// the Claude path does. Measured live on 0.144.4: with the feature flags off, `apply_patch`,
/// `view_image`, `update_plan`, `request_user_input`, the goal tools, the MCP resource tools and
/// `multi_agent_v1__*` remain, and this hook blocks each of them, including when called from
/// inside code mode (`functions.exec`). Code mode itself is not a hooked tool; it computes, and any
/// I/O it attempts goes through a hooked tool.
pub fn codex_deny_hook_override(prefix: &str) -> String {
    fn json(value: &str) -> String {
        serde_json::to_string(value).expect("serializing a string cannot fail")
    }
    let unix = format!("echo {CODEX_MCP_ONLY_DENIAL} >&2; exit 2");
    let windows = format!("[Console]::Error.WriteLine('{CODEX_MCP_ONLY_DENIAL}'); exit 2");
    format!(
        "hooks.PreToolUse=[{{matcher={},hooks=[{{type=\"command\",command={},commandWindows={}}}]}}]",
        json(&codex_deny_matcher(prefix)),
        json(&unix),
        json(&windows),
    )
}

/// Translates the daemon's stdio MCP configuration into Codex overrides.
/// `codex exec` runs with approval policy `never`, so Codex immediately declines an MCP tool call
/// that needs confirmation ("user cancelled MCP tool call" on 0.144.4). `approve` pre-approves
/// the daemon's own MCP servers, as the Claude path does with `--allowedTools mcp__nucleos__*`.
pub fn codex_mcp_overrides(
    config: &serde_json::Value,
    env_names: &[String],
) -> Result<Vec<String>, String> {
    let servers = config
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "mcp_config must contain an mcpServers object".to_string())?;
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();

    let mut overrides = Vec::new();
    for name in names {
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(format!(
                "mcp_config server name {name:?} cannot be expressed"
            ));
        }
        let server = servers
            .get(name)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("mcp_config server {name:?} must be an object"))?;
        if !matches!(
            server.get("type").and_then(serde_json::Value::as_str),
            None | Some("stdio")
        ) {
            return Err(format!("mcp_config server {name:?} must be stdio"));
        }
        let command = server
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("mcp_config server {name:?} must have a string command"))?;
        let args = match server.get("args") {
            None => Vec::new(),
            Some(args) => args
                .as_array()
                .and_then(|args| {
                    args.iter()
                        .map(|arg| arg.as_str().map(str::to_string))
                        .collect()
                })
                .ok_or_else(|| format!("mcp_config server {name:?} args must be a string array"))?,
        };
        overrides.push(format!(
            "mcp_servers.{name}.command={}",
            serde_json::to_string(command).expect("serializing a string cannot fail")
        ));
        overrides.push(format!(
            "mcp_servers.{name}.args={}",
            serde_json::to_string(&args).expect("serializing strings cannot fail")
        ));
        if !env_names.is_empty() {
            overrides.push(format!(
                "mcp_servers.{name}.env_vars={}",
                serde_json::to_string(env_names).expect("serializing strings cannot fail")
            ));
        }
        overrides.push(format!(
            "mcp_servers.{name}.default_tools_approval_mode={}",
            serde_json::to_string("approve").expect("serializing a string cannot fail")
        ));
    }
    Ok(overrides)
}

/// Reads the thread id from Codex's thread-started event.
pub fn codex_thread_id(line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    (value.get("type").and_then(serde_json::Value::as_str) == Some("thread.started"))
        .then(|| value.get("thread_id").and_then(serde_json::Value::as_str))
        .flatten()
        .map(str::to_string)
}

/// Decodes opening-turn images into files readable by the Codex CLI.
pub fn stage_codex_images(
    dir: &std::path::Path,
    stem: &str,
    images: &[Attachment],
) -> std::io::Result<Vec<std::path::PathBuf>> {
    images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let extension = match image.media_type.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => {
                    return Err(std::io::Error::other(
                        "codex exec cannot honour images: unsupported media type",
                    ));
                }
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&image.data)
                .map_err(|error| {
                    std::io::Error::other(format!("codex exec cannot honour images: {error}"))
                })?;
            let path = dir.join(format!("{stem}-{index}.{extension}"));
            std::fs::write(&path, bytes)?;
            Ok(path)
        })
        .collect()
}

/// The model a Codex launch runs: the request's own when it names one, else the runner's
/// configured model. A routed run names its model on the request, so this is where that choice
/// reaches the command line.
pub fn codex_model<'a>(request: &'a RunRequest, configured: &'a str) -> &'a str {
    request.model.as_deref().unwrap_or(configured)
}

/// The full `codex exec` argument vector for one run, or the reason this tool cannot perform the
/// run that was asked for. Pure for the same reason `cli_args` is: the flags deciding which model
/// answers and where it is allowed to work are asserted in tests instead of inspected on a live
/// process.
///
/// The `Result` is what differs from `cli_args`. Claude's CLI has a flag for every control
/// `RunRequest` carries, so building that vector cannot fail. `codex exec` has no counterpart for a
/// forked session or for a stdin later turns arrive on, and each of those decides what a run IS
/// rather than how it is decorated — so a vector that quietly dropped one would hand the caller a
/// different run than it asked for (one that loses the history it was meant to branch from, or that
/// answers once and then ignores every steering message) while `runs.rs` recorded it as completed.
/// Naming the field in the refusal is what tells an operator which request cannot take this path.
pub fn codex_cli_args(
    request: &RunRequest,
    model: &str,
    staged: &CodexStaged,
) -> Result<Vec<String>, String> {
    if request.fork_session {
        return Err(
            "codex exec cannot honour fork_session: it has no way to branch an existing session"
                .to_string(),
        );
    }
    if request.steerable && request.messages.is_some() {
        return Err(
            "codex exec cannot honour steerable: it has no stdin a later turn can arrive on"
                .to_string(),
        );
    }

    if !request.images.is_empty() && staged.images.len() != request.images.len() {
        return Err(
            "codex exec cannot honour images: staging did not produce every image".to_string(),
        );
    }

    let mut args = vec![
        // The non-interactive subcommand leads the vector; anything else opens a TUI, and a
        // daemon-spawned run has no terminal for one.
        "exec".to_string(),
        // A run works inside a worktree or a plain folder, and the CLI otherwise refuses to start
        // over the shape of that directory — a refusal about the ground rather than about the work.
        if request.resume_session_id.is_some() {
            "resume".to_string()
        } else {
            "--json".to_string()
        },
    ];
    if request.resume_session_id.is_some() {
        args.push("--json".to_string());
    }
    args.push("--skip-git-repo-check".to_string());
    if request.tool_policy == ToolPolicy::McpOnly {
        // Codex has no allow-list flag, so everything but the daemon's own MCP tools is switched
        // off one by one. `-c`, not `--disable`: `codex exec resume` lacks several exec flags.
        args.push("--ignore-user-config".to_string());
        args.push("--dangerously-bypass-hook-trust".to_string());
        for feature in CODEX_MCP_ONLY_DISABLED_FEATURES {
            args.push("-c".to_string());
            args.push(format!("features.{feature}=false"));
        }
        args.push("-c".to_string());
        args.push(r#"web_search="disabled""#.to_string());
        args.push("-c".to_string());
        args.push(codex_deny_hook_override(DAEMON_MCP_TOOL_PREFIX));
    }
    for image in &staged.images {
        args.push("-i".to_string());
        args.push(image.to_string_lossy().into_owned());
    }
    args.extend(["-m".to_string(), model.to_string()]);
    if request.resume_session_id.is_none() {
        // `-C` is the only thing keeping a fresh run inside its project.
        if let Some(dir) = &request.cwd {
            args.push("-C".to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
        for dir in &request.add_dirs {
            args.push("--add-dir".to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
    } else if !request.add_dirs.is_empty() {
        let roots: Vec<String> = request
            .add_dirs
            .iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect();
        args.extend([
            "-c".to_string(),
            format!(
                "sandbox_workspace_write.writable_roots={}",
                serde_json::to_string(&roots).expect("serializing paths cannot fail")
            ),
        ]);
    }
    // Use `-c`, not `-s`, because `codex exec resume` has no `-s` flag.
    // It beats `sandbox_mode` in the user's `~/.codex/config.toml`, which exec's default does not.
    // An `McpOnly` turn pins read-only itself when the runner pins nothing, so a no-project chat
    // never inherits the user's sandbox config.
    let sandbox_mode = staged
        .sandbox_mode
        .or((request.tool_policy == ToolPolicy::McpOnly).then_some("read-only"));
    if let Some(mode) = sandbox_mode {
        args.extend([
            "-c".to_string(),
            format!(
                "sandbox_mode={}",
                serde_json::to_string(mode).expect("serializing a string cannot fail")
            ),
        ]);
    }
    if let Some(effort) = &request.effort {
        args.extend([
            "-c".to_string(),
            format!(
                "model_reasoning_effort={}",
                serde_json::to_string(effort).expect("serializing a string cannot fail")
            ),
        ]);
    }
    for override_value in &staged.mcp_overrides {
        args.push("-c".to_string());
        args.push(override_value.clone());
    }
    if let Some(session_id) = &request.resume_session_id {
        args.push(session_id.clone());
    }
    // Trailing positional, after every flag that takes a value, so a prompt can never be consumed as
    // the argument of the option before it.
    args.push(request.prompt.clone());
    Ok(args)
}

/// Usage reported by a `codex exec` transcript's final `turn.completed` event.
///
/// Absent measurements stay `None` rather than becoming measured zeroes, exactly as `extract_usage`
/// keeps them for the Claude CLI. `budget.rs` bills autonomy against these fields, and this runner
/// exists to be the cheaper path once a spend ceiling has paused the first one — so a `Some(0)`
/// standing in for a tool that said nothing would make every run on this path look free and leave
/// the ceiling with nothing to pause on.
///
/// A plain-text transcript and a structured event carrying no `usage` object are the same silence:
/// unknown in both, and unknown is not zero. `num_turns` and `cache_creation_tokens` have no
/// counterpart in this stream and stay `None` for that reason — counting the events that happened to
/// be read is not the tool reporting a turn count.
pub fn codex_extract_usage(stdout: &str) -> RunUsage {
    let mut usage = RunUsage::default();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
            && value.get("type").and_then(serde_json::Value::as_str) == Some("turn.completed")
        {
            let reported = value.get("usage");
            usage = RunUsage {
                input_tokens: reported
                    .and_then(|reported| reported.get("input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                output_tokens: reported
                    .and_then(|reported| reported.get("output_tokens"))
                    .and_then(serde_json::Value::as_i64),
                cache_read_tokens: reported
                    .and_then(|reported| reported.get("cached_input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                // Codex reports what it read from the cache and never what it wrote there. Unknown,
                // not zero — and unknown is what disqualifies a Codex run from the cache signal
                // rather than making every one of them look like a miss.
                cache_creation_tokens: None,
                num_turns: None,
            };
        }
    }
    usage
}

/// The second agent CLI, reached only when configuration names it.
///
/// A separate `CommandRunner` rather than a mode on `ClaudeCliRunner`, for the reason `OllamaRunner`
/// is one: the two tools share neither a launch surface nor an output format, and a mode flag would
/// let a control only one of them honours look enforced on both.
pub struct CodexCliRunner {
    pub model: String,
    /// The sandbox every launch of this runner pins; `None` leaves Codex's own resolution (the
    /// user's config, else exec's read-only default).
    pub sandbox_mode: Option<&'static str>,
}

impl CodexCliRunner {
    /// Builds the runner used for a chat turn answered by Codex (`Assistants::cli_runner`).
    /// The owner's 2026-09-14 decision pins chat turns read-only; an `McpOnly` turn is pinned
    /// read-only by `codex_cli_args` too, whichever runner it goes through.
    /// KNOWN LIMITATION: a daemon whose primary runner is Codex uses its run runner through
    /// `assistant::runner_for_turn`, so it keeps the user's Codex config as before this branch.
    pub fn for_chat(model: String) -> Self {
        Self {
            model,
            sandbox_mode: Some("read-only"),
        }
    }
}

#[async_trait]
impl CommandRunner for CodexCliRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        // `McpOnly` is applied by the flags and the deny hook `codex_cli_args` adds. `None` is
        // refused before anything is sent, exactly as `OllamaRunner` refuses a policy it cannot
        // apply: `codex exec` has no way to express "no tools at all" and no init event advertising
        // which ones survived — so a run launched here under it would hold every tool while
        // `runs.rs` recorded the restriction as applied.
        if request.tool_policy != ToolPolicy::Unrestricted
            && request.tool_policy != ToolPolicy::McpOnly
        {
            return Err(std::io::Error::other(format!(
                "codex exec cannot honour ToolPolicy::{:?}: it has no tool-restriction flag",
                request.tool_policy
            )));
        }
        // The same guard, extended to the other controls this launch surface has no counterpart for.
        // Each was accepted and dropped, which is the one outcome a control must never have: the
        // caller is told the run it asked for started, and the record says so too.
        //
        // `Permission` is the reason this is a refusal rather than a log line, and every rung above
        // `Default` is refused. `Plan` is how a run is made unable to act — a catch-up run,
        // recovering a schedule the machine slept through, is forced into it precisely because
        // nobody chose for it to run NOW — so a runner that ignores it turns a deliberately
        // restrained run into an unrestrained one, in the one case where the operator was not
        // watching. `Bypass` fails the opposite way and worse: it is the CLI's permission barrier
        // standing down on the strength of a `PreToolUse` hook, which `codex exec` has no notion of,
        // so honouring it means running unbarriered where the daemon believes a second barrier took
        // over. `AcceptEdits` is a per-conversation control a person set in the window, and dropping
        // it silently leaves the row claiming something the run never had.
        //
        // `Default` alone passes, and passes by writing nothing: it is the rung that asks the launch
        // surface for no elevation at all, which is what `codex exec` already does.
        if request.permission != Permission::Default {
            return Err(std::io::Error::other(format!(
                "codex exec cannot honour Permission::{:?}: it has no permission mode that says this",
                request.permission
            )));
        }
        // MCP config is honoured through `codex_mcp_overrides`, which translates it into
        // `-c mcp_servers.<name>...` overrides.
        // The daemon's own servers are pre-approved (`default_tools_approval_mode = "approve"`).
        // Under `McpOnly` the user's servers no longer load (`--ignore-user-config`). Under
        // `Unrestricted`, servers in the user's `~/.codex/config.toml` still load with that file's
        // approval because `codex exec` has no counterpart to the Claude CLI's `--strict-mcp-config`.
        //
        // The barrier this used to stand down is `Permission::Bypass`'s to stand down now, and the
        // guard above refuses that. What is left here is a BELIEF and it is still worth refusing:
        // this flag says the daemon has decided a `PreToolUse` classifier governs what this run may
        // call — and `codex exec` knows nothing of that mechanism, so no classifier will run.
        // Dropped silently it would not weaken THIS launch, but it would leave the daemon believing
        // a run is governed by something that never ran, which is the state every guard in this
        // block exists to refuse.
        if request.classifier_governs_tools {
            return Err(std::io::Error::other(
                "codex exec cannot honour classifier_governs_tools: the daemon wires no classifier hook into codex, so nothing would do the governing the daemon believes is happening",
            ));
        }
        // Every per-conversation control this launch surface has no counterpart for, refused in
        // one place and named individually so the message says which one.
        //
        // These arrived with 0110–0113 and each was accepted and dropped here — the outcome the
        // block above exists to prevent. They are not one kind of thing, and refusing them together
        // is still right, because they fail the same way: the chat row says a conversation is
        // pinned to a model, capped at a dollar a turn, barred from `Bash` and carrying standing
        // instructions, the window draws all four, and none of them reached the process.
        //
        // `denied_tools` is the sharpest of them. It passes the `ToolPolicy` guard above — a
        // conversation can be `Unrestricted` and still have barred a tool for itself — so without
        // this it would be a restriction somebody set, saw drawn back at them, and never had.
        for (asked, control) in [
            (!request.fallback_model.is_empty(), "fallback_model"),
            (request.max_budget_usd.is_some(), "max_budget_usd"),
            (!request.agents.is_empty(), "agents"),
            (
                request.append_system_prompt.is_some(),
                "append_system_prompt",
            ),
            (!request.denied_tools.is_empty(), "denied_tools"),
        ] {
            if asked {
                return Err(std::io::Error::other(format!(
                    "codex exec cannot honour {control}: it has no counterpart for it, and dropping one would leave the chat row claiming a control the run never had"
                )));
            }
        }
        // KNOWN LIMITATION, left un-refused on purpose, beside `session_name` below:
        // `context_window` is not honoured here, and it is the one control on this list that is
        // safe to lose. It is exported as `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, which is a Claude
        // Code environment variable that `codex exec` reads no meaning into; a run that loses it
        // compacts on whatever schedule Codex has of its own, which is the schedule every run on
        // this path has always had. Nothing is loosened and no record claims otherwise — the
        // window a chat row names is drawn from the row, and the row is still true about the
        // Claude path it was written for.

        // KNOWN LIMITATION, left un-refused on purpose, beside `resume_session_id` below:
        // `session_name` is not honoured here. It reaches the Claude CLI's `--resume` picker and
        // nothing else — no decision anywhere depends on it, and no record claims it was applied —
        // so a run that loses it is a run with a nameless session, which is what every run on this
        // path has always had.

        // Partial messages are accepted and degrade: Codex's stream has no partial-message events,
        // so the reply arrives when the turn completes.
        // Resume is honoured through `codex exec resume`, which has its own argument shape.
        //
        // The args are built before anything is spawned so a refusal reaches the caller as the `Err` that means
        // the CLI never ran — which `runs::spawn_run` reads as "a retry cannot double-apply a
        // mutation", and that is precisely true of a launch that did not happen.
        // `codex_cli_args` honours MCP config, effort, and extra directories; partial messages
        // degrade because the reply arrives when the turn completes.
        let mcp_overrides = match &request.mcp_config {
            Some(path) => {
                let contents = std::fs::read(path).map_err(|error| {
                    std::io::Error::other(format!("codex exec cannot honour mcp_config: {error}"))
                })?;
                let config = serde_json::from_slice(&contents).map_err(|error| {
                    std::io::Error::other(format!("codex exec cannot honour mcp_config: {error}"))
                })?;
                let mut env_names: Vec<String> =
                    request.env.iter().map(|(name, _)| name.clone()).collect();
                env_names.sort();
                codex_mcp_overrides(&config, &env_names).map_err(|why| {
                    std::io::Error::other(format!("codex exec cannot honour mcp_config: {why}"))
                })?
            }
            None => Vec::new(),
        };
        struct StagedFiles(Vec<PathBuf>);
        impl Drop for StagedFiles {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let staged_files = if request.images.is_empty() {
            StagedFiles(Vec::new())
        } else {
            let stem = format!(
                "nucleos-codex-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            StagedFiles(stage_codex_images(
                std::env::temp_dir().as_path(),
                &stem,
                &request.images,
            )?)
        };
        let staged = CodexStaged {
            mcp_overrides,
            images: staged_files.0.clone(),
            sandbox_mode: self.sandbox_mode,
        };
        let args = codex_cli_args(&request, codex_model(&request, &self.model), &staged)
            .map_err(std::io::Error::other)?;

        // The Codex CLI binary. Overridable via `NUCLEOS_CODEX_BIN` for the same reason
        // `NUCLEOS_CLAUDE_BIN` exists: on Windows the npm-installed `codex` is a `.cmd` shim that
        // Rust's `Command` can't spawn by name, so the daemon points this at the real executable.
        // Defaults to `codex` where it's on PATH.
        let codex_bin = std::env::var("NUCLEOS_CODEX_BIN").unwrap_or_else(|_| "codex".to_string());
        let mut cmd = Command::new(&codex_bin);
        cmd.args(args);
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &request.cwd {
            cmd.current_dir(dir);
        }
        // Nothing steerable survives `codex_cli_args`, so stdin is always closed here: there is then
        // no channel for a second author to arrive on.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        crate::process_tree::spawn_in_own_group(&mut cmd);

        // The ONLY `?` from here on, for the reason spelled out in `ClaudeCliRunner`: past the spawn
        // a failure becomes a failed `RunOutcome`, because an `Err` claims no work was done.
        let mut child = cmd.spawn()?;

        // Declared after the child so it drops FIRST, which is what keeps the pid it names ours —
        // see `TreeKiller`. `codex` supervises its own tools, so terminating just the parent orphans
        // a build still holding locks inside the worktree the run was supposed to release.
        let mut tree_killer = child.id().map(TreeKiller::new);

        let stdout = child.stdout.take().expect("stdout piped above");
        let stderr = child.stderr.take().expect("stderr piped above");

        // Concurrent, because reading only stdout while the child writes stderr deadlocks the moment
        // the OS stderr pipe buffer fills.
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut buf).await;
            buf
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut stdout_acc = String::new();
        let mut post_launch_error: Option<std::io::Error> = None;
        let mut progress_timeout_elapsed: Option<Duration> = None;
        let mut turns = TurnCounter::default();
        let mut turns_exceeded: Option<i64> = None;
        let mut thread_id = None;

        loop {
            let next_line = match request.progress_timeout {
                Some(deadline) => match tokio::time::timeout(deadline, lines.next_line()).await {
                    Ok(result) => result,
                    Err(_) => {
                        progress_timeout_elapsed = Some(deadline);
                        break;
                    }
                },
                None => lines.next_line().await,
            };
            let line = match next_line {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    post_launch_error = Some(error);
                    break;
                }
            };
            stdout_acc.push_str(&line);
            stdout_acc.push('\n');
            // Mirrored as it arrives rather than at the end, because the end is exactly what a
            // wall-clock timeout never reaches, and the transcript is all a dropped run leaves.
            if let Ok(mut shared) = transcript.lock() {
                shared.push_str(&line);
                shared.push('\n');
            }
            if thread_id.is_none() {
                thread_id = codex_thread_id(&line);
                if let Some(id) = &thread_id {
                    let _ = session_tx.send(id.clone());
                }
            }
            // The same brake as the Claude body above, counting `turn.completed` instead of
            // `assistant` — `TurnCounter` knows both, so this path cannot drift out of step
            // with the other by being edited on its own.
            turns.line(&line);
            if over_turn_ceiling(turns.count(), request.max_turns) {
                turns_exceeded = Some(turns.count());
                break;
            }
        }

        // A read error is in the list for the Claude body's reason: nothing is listening any more,
        // and a CLI still alive behind a broken pipe would hold `wait()` until it chose to exit.
        if progress_timeout_elapsed.is_some()
            || turns_exceeded.is_some()
            || post_launch_error.is_some()
        {
            // Kill the whole tree first, so terminating the supervisor cannot orphan its tools.
            drop(tree_killer.take());
            let _ = child.start_kill();
        }

        let status = match child.wait().await {
            Ok(status) => Some(status),
            Err(error) => {
                post_launch_error.get_or_insert(error);
                None
            }
        };
        // Reaped, so the process is gone and there is nothing left to kill.
        if let Some(killer) = tree_killer.as_mut() {
            killer.disarm();
        }

        let mut stderr_str = stderr_task.await.unwrap_or_default();
        if let Some(deadline) = progress_timeout_elapsed {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: run went silent for {deadline:?}; progress deadline expired\n"
            ));
        }
        if let Some(reached) = turns_exceeded {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: stopped after {reached} turns; this run's ceiling was {}\n",
                request.max_turns.unwrap_or_default()
            ));
        }
        let exit_code = match (progress_timeout_elapsed, turns_exceeded, &post_launch_error) {
            (Some(_), _, _) => PROGRESS_TIMEOUT_EXIT_CODE,
            (None, Some(_), _) => TURN_CEILING_EXIT_CODE,
            // A stream that failed mid-run is a failed run, never a zero exit: the transcript is
            // incomplete, so "succeeded" is a claim this cannot make.
            (None, None, Some(error)) => {
                stderr_str.push_str(&format!("\nnucleos: stream failed after launch: {error}\n"));
                -1
            }
            (None, None, None) => status.and_then(|status| status.code()).unwrap_or(-1),
        };

        let usage = codex_extract_usage(&stdout_acc);
        Ok(RunOutcome {
            exit_code,
            stdout: stdout_acc,
            stderr: stderr_str,
            // The thread id from `thread.started` when the stream carried one, else the caller's id.
            session_id: thread_id.or_else(|| {
                request
                    .session_id
                    .clone()
                    .or_else(|| request.resume_session_id.clone())
            }),
            // This tool reports no price. Unknown, not free — `budget.rs` bills against this field,
            // and a `Some(0.0)` would make every run on this path look like it spent nothing.
            cost_usd: None,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            num_turns: usage.num_turns,
            compacted: false,
            model_requested: false,
        })
    }
}
