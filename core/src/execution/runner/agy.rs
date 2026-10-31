//! The Antigravity CLI (`agy`) backend: binary resolution, the argument vector, long-prompt staging
//! and `AgyCliRunner`.
//!
//! `agy -p` runs one prompt and prints a single JSON object. Anything the CLI cannot honour is
//! refused by name before it is started, exactly as `CodexCliRunner` does, and a reply that does not
//! carry an exact `SUCCESS` status fails closed. `--dangerously-skip-permissions` is never emitted:
//! `Permission::Bypass` is refused, not translated. The tests below drive a fake binary chosen by
//! `NUCLEOS_AGY_BIN`; the real `agy` is never started by them.

use super::*;
use std::ffi::{OsStr, OsString};
use std::path::Path;

/// Prompts up to this many bytes travel as the `-p` argument. Longer ones are staged in a file so
/// the Windows argument limit (about 32k characters) is never approached.
pub const INLINE_PROMPT_LIMIT: usize = 8000;

/// The file name a long prompt is staged under, inside the run's stage directory.
pub const STAGED_PROMPT_FILE: &str = "prompt.md";

/// The effort levels `--effort` accepts.
const VALID_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// How much of a malformed stdout a failure message echoes.
const ECHO_LIMIT: usize = 500;

/// A control the agy engine cannot honour. Its text names the control and always ends with the
/// sentence the engine contract fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(String);

impl Refusal {
    fn control(control: &str) -> Self {
        Self(format!(
            "agy cannot honour {control}: the agy engine does not support it"
        ))
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// What `build_args` needs beyond the request.
#[derive(Debug, Clone)]
pub struct Options {
    /// The model id; it may carry the catalogue's `agy/` prefix, which the CLI does not know.
    pub model: String,
    /// The runner's configured effort; a request's own effort wins.
    pub effort: Option<String>,
    /// Where a long prompt is staged.
    pub stage_dir: PathBuf,
}

/// PURE (filesystem reads only): the agy program given the three lookups. `env_bin` is
/// `NUCLEOS_AGY_BIN` and is taken as given; then `agy.exe`/`agy` along `path_var`; then
/// `<local_app_data>/agy/bin/agy.exe`.
pub fn agy_program_from(
    env_bin: Option<OsString>,
    path_var: Option<OsString>,
    local_app_data: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(configured) = env_bin {
        return Some(PathBuf::from(configured));
    }
    if let Some(path_var) = path_var.as_deref().map(OsStr::to_owned) {
        let on_path = std::env::split_paths(&path_var).find_map(|directory| {
            ["agy.exe", "agy"]
                .into_iter()
                .map(|name| directory.join(name))
                .find(|candidate| candidate.is_file())
        });
        if on_path.is_some() {
            return on_path;
        }
    }
    let installed = PathBuf::from(local_app_data?)
        .join("agy")
        .join("bin")
        .join("agy.exe");
    installed.is_file().then_some(installed)
}

/// The program that launches agy: `NUCLEOS_AGY_BIN`, else `PATH`, else `%LOCALAPPDATA%\agy\bin`.
/// `None` when agy is not installed. Resolves only; nothing is run.
pub fn agy_program() -> Option<PathBuf> {
    agy_program_from(
        std::env::var_os("NUCLEOS_AGY_BIN"),
        std::env::var_os("PATH"),
        std::env::var_os("LOCALAPPDATA"),
    )
}

/// Writes the prompt to `<stage_dir>/STAGED_PROMPT_FILE` when it is longer than
/// `INLINE_PROMPT_LIMIT`; returns the file, or `None` when the prompt goes inline (nothing written).
pub fn stage_prompt(request: &RunRequest, stage_dir: &Path) -> std::io::Result<Option<PathBuf>> {
    if request.prompt.len() <= INLINE_PROMPT_LIMIT {
        return Ok(None);
    }
    std::fs::create_dir_all(stage_dir)?;
    let file = stage_dir.join(STAGED_PROMPT_FILE);
    std::fs::write(&file, &request.prompt)?;
    Ok(Some(file))
}

/// The argument vector for one run, or the control agy cannot honour. Pure: it writes no file.
pub fn build_args(request: &RunRequest, options: &Options) -> Result<Vec<String>, Refusal> {
    // `Bypass` is how a run stands the CLI's permission barrier down; agy has no counterpart and
    // the skip-permissions flag is never emitted for it.
    if request.permission == Permission::Bypass {
        return Err(Refusal::control("Permission::Bypass"));
    }
    // Each of these would be accepted and dropped otherwise, which is the outcome a control must
    // never have.
    for (asked, control) in [
        (!request.fallback_model.is_empty(), "fallback_model"),
        (request.max_budget_usd.is_some(), "max_budget_usd"),
        (!request.agents.is_empty(), "agents"),
        (
            request.append_system_prompt.is_some(),
            "append_system_prompt",
        ),
        (!request.denied_tools.is_empty(), "denied_tools"),
        (request.mcp_config.is_some(), "mcp_config"),
        (!request.mcp_servers.is_empty(), "mcp_servers"),
        (request.classifier_governs_tools, "classifier_governs_tools"),
        (!request.images.is_empty(), "images"),
    ] {
        if asked {
            return Err(Refusal::control(control));
        }
    }

    let model = options.model.strip_prefix("agy/").unwrap_or(&options.model);
    let effort = request.effort.as_ref().or(options.effort.as_ref());
    if let Some(effort) = effort
        && !VALID_EFFORTS.contains(&effort.as_str())
    {
        return Err(Refusal(format!(
            "agy effort {effort:?} is not one of {}",
            VALID_EFFORTS.join(", ")
        )));
    }

    let mut args = Vec::new();
    let mut add_dirs: Vec<String> = Vec::new();
    args.push("-p".to_string());
    if request.prompt.len() <= INLINE_PROMPT_LIMIT {
        args.push(request.prompt.clone());
    } else {
        let file = options.stage_dir.join(STAGED_PROMPT_FILE);
        args.push(format!(
            "Read the file {} and follow it exactly; it is your complete task.",
            file.display()
        ));
        add_dirs.push(options.stage_dir.display().to_string());
    }
    args.extend(["--output-format".to_string(), "json".to_string()]);
    args.extend(["--model".to_string(), model.to_string()]);
    if let Some(effort) = effort {
        args.extend(["--effort".to_string(), effort.clone()]);
    }
    // agy has no tool-restriction flag, so plan mode plus its sandbox is the closest it offers to a
    // restricted turn. A restricted policy therefore overrides the request's permission mode.
    match request.tool_policy {
        ToolPolicy::McpOnly | ToolPolicy::None => {
            args.extend(["--mode".to_string(), "plan".to_string()]);
            args.push("--sandbox".to_string());
        }
        ToolPolicy::Unrestricted => match request.permission {
            Permission::Plan => args.extend(["--mode".to_string(), "plan".to_string()]),
            Permission::AcceptEdits => {
                args.extend(["--mode".to_string(), "accept-edits".to_string()]);
            }
            Permission::Default | Permission::Bypass => {}
        },
    }
    if let Some(conversation) = &request.resume_session_id {
        args.extend(["--conversation".to_string(), conversation.clone()]);
    }
    if let Some(deadline) = request.progress_timeout {
        // Whole seconds, rounded up and never zero: zero would read as "no limit" or "now".
        let secs = deadline.as_secs() + u64::from(deadline.subsec_nanos() > 0);
        args.extend(["--print-timeout".to_string(), secs.max(1).to_string()]);
    }
    add_dirs.extend(request.add_dirs.iter().map(|dir| dir.display().to_string()));
    for dir in add_dirs {
        args.extend(["--add-dir".to_string(), dir]);
    }
    Ok(args)
}

/// The first `ECHO_LIMIT` bytes of `text`, cut on a character boundary.
fn first_bytes(text: &str) -> &str {
    let mut end = text.len().min(ECHO_LIMIT);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// What a successful agy reply carries.
struct AgyReply {
    response: String,
    conversation_id: Option<String>,
    num_turns: Option<i64>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
}

/// Parses agy's one-shot JSON. `Err` is the failure text: a status other than exactly `SUCCESS`, or
/// a body that is not the expected object, with the first bytes of what was printed.
fn parse_reply(stdout: &str) -> Result<AgyReply, String> {
    let malformed = || {
        format!(
            "agy printed no usable reply; first bytes of stdout: {}",
            first_bytes(stdout)
        )
    };
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|_| malformed())?;
    let status = value
        .get("status")
        .and_then(|status| status.as_str())
        .ok_or_else(malformed)?;
    if status != "SUCCESS" {
        return Err(format!("agy returned status {status}"));
    }
    let response = value
        .get("response")
        .and_then(|response| response.as_str())
        .ok_or_else(malformed)?
        .to_string();
    let usage = value.get("usage");
    let count = |name: &str| usage.and_then(|u| u.get(name)).and_then(|n| n.as_i64());
    let output = match (count("output_tokens"), count("thinking_tokens")) {
        (Some(output), Some(thinking)) => Some(output + thinking),
        (output, thinking) => output.or(thinking),
    };
    Ok(AgyReply {
        response,
        conversation_id: value
            .get("conversation_id")
            .and_then(|id| id.as_str())
            .map(str::to_string),
        num_turns: value.get("num_turns").and_then(|turns| turns.as_i64()),
        input_tokens: count("input_tokens"),
        output_tokens: output,
        cache_read_tokens: count("cache_read_tokens"),
    })
}

/// The Antigravity CLI backend. Chat-only for now: agents, teams and council refuse the engine.
pub struct AgyCliRunner {
    pub model: String,
    pub effort: Option<String>,
}

/// Removes a run's stage directory when the run ends, however it ends.
struct StageDir(PathBuf);

impl Drop for StageDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Appends `text` to `stderr` on its own line.
fn note(stderr: &mut String, text: String) {
    if !stderr.is_empty() && !stderr.ends_with('\n') {
        stderr.push('\n');
    }
    stderr.push_str(&text);
}

#[async_trait]
impl CommandRunner for AgyCliRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        let stage = StageDir(std::env::temp_dir().join(format!(
            "nucleos-agy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )));
        let options = Options {
            model: request.model.clone().unwrap_or_else(|| self.model.clone()),
            effort: self.effort.clone(),
            stage_dir: stage.0.clone(),
        };
        // Built before anything is spawned, so a refusal is the `Err` that means the CLI never ran.
        let args = build_args(&request, &options).map_err(std::io::Error::other)?;
        stage_prompt(&request, &stage.0)?;

        let agy_bin = agy_program().unwrap_or_else(|| PathBuf::from("agy"));
        let mut cmd = Command::new(&agy_bin);
        cmd.args(args);
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &request.cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        crate::process_tree::spawn_in_own_group(&mut cmd);

        // The ONLY `?` from here on: past the spawn a failure is a failed `RunOutcome`.
        let mut child = cmd.spawn()?;
        // Declared after the child so it drops first; see `TreeKiller`.
        let mut tree_killer = child.id().map(TreeKiller::new);

        let stdout = child.stdout.take().expect("stdout piped above");
        let stderr = child.stderr.take().expect("stderr piped above");
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
            match next_line {
                Ok(Some(line)) => {
                    stdout_acc.push_str(&line);
                    stdout_acc.push('\n');
                }
                Ok(None) => break,
                Err(error) => {
                    post_launch_error = Some(error);
                    break;
                }
            }
        }

        if progress_timeout_elapsed.is_some() || post_launch_error.is_some() {
            // The whole tree first, so ending the supervisor cannot orphan its tools.
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
        if let Some(killer) = tree_killer.as_mut() {
            killer.disarm();
        }

        let mut stderr_str = stderr_task.await.unwrap_or_default();
        let mut exit_code = status.and_then(|status| status.code()).unwrap_or(-1);
        let mut reply = None;
        if let Some(deadline) = progress_timeout_elapsed {
            note(
                &mut stderr_str,
                format!("nucleos: run went silent for {deadline:?}; progress deadline expired\n"),
            );
            exit_code = PROGRESS_TIMEOUT_EXIT_CODE;
        } else if let Some(error) = &post_launch_error {
            note(
                &mut stderr_str,
                format!("nucleos: stream failed after launch: {error}\n"),
            );
            exit_code = -1;
        } else if exit_code != 0 {
            note(
                &mut stderr_str,
                format!(
                    "agy exited with code {exit_code}; first bytes of stdout: {}\n",
                    first_bytes(&stdout_acc)
                ),
            );
        } else {
            match parse_reply(&stdout_acc) {
                Err(why) => {
                    note(&mut stderr_str, format!("{why}\n"));
                    exit_code = 1;
                }
                Ok(parsed) => {
                    if let (Some(turns), Some(ceiling)) = (parsed.num_turns, request.max_turns)
                        && over_turn_ceiling(turns, Some(ceiling))
                    {
                        note(
                            &mut stderr_str,
                            format!(
                                "nucleos: stopped after {turns} turns; this run's ceiling was {ceiling}\n"
                            ),
                        );
                        exit_code = TURN_CEILING_EXIT_CODE;
                    }
                    reply = Some(parsed);
                }
            }
        }

        // One synthesised `result` line, so `extract_reply` reads this runner like every other.
        let mut out = stdout_acc;
        let mut session_id = request
            .session_id
            .clone()
            .or_else(|| request.resume_session_id.clone());
        let (mut input_tokens, mut output_tokens, mut cache_read_tokens, mut num_turns) =
            (None, None, None, None);
        if let Some(parsed) = reply {
            if let Some(id) = &parsed.conversation_id {
                let _ = session_tx.send(id.clone());
                session_id = Some(id.clone());
            }
            let line = serde_json::json!({"type": "result", "result": parsed.response}).to_string();
            if exit_code == 0 {
                if let Ok(mut shared) = transcript.lock() {
                    shared.push_str(&line);
                    shared.push('\n');
                }
                out = format!("{line}\n");
            }
            input_tokens = parsed.input_tokens;
            output_tokens = parsed.output_tokens;
            cache_read_tokens = parsed.cache_read_tokens;
            num_turns = parsed.num_turns;
        }

        Ok(RunOutcome {
            exit_code,
            stdout: out,
            stderr: stderr_str,
            session_id,
            // agy reports no price: unknown, not free.
            cost_usd: None,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens: None,
            num_turns,
            compacted: false,
            model_requested: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::{
        CommandRunner, PROGRESS_TIMEOUT_EXIT_CODE, Permission, RunRequest, Subagent, ToolPolicy,
        extract_reply,
    };
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    // ---- helpers ----------------------------------------------------------------------------

    /// A request with every control at its neutral value; each test names the one it changes.
    fn request(prompt: &str) -> RunRequest {
        RunRequest {
            prompt: prompt.to_string(),
            env: Vec::new(),
            cwd: None,
            permission: Permission::Default,
            resume_session_id: None,
            mcp_config: None,
            mcp_job: None,
            mcp_team_run: None,
            tool_policy: ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: None,
            fork_session: false,
            include_partial_messages: false,
            images: Vec::new(),
            steerable: false,
            classifier_governs_tools: false,
            mcp_servers: Vec::new(),
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
        }
    }

    fn options() -> Options {
        Options {
            model: "agy/gemini-3.8-flash-high".to_string(),
            effort: None,
            stage_dir: std::env::temp_dir().join("nucleos-agy-test-stage"),
        }
    }

    fn pair(args: &[String], flag: &str) -> Option<String> {
        args.windows(2)
            .find(|window| window[0] == flag)
            .map(|window| window[1].clone())
    }

    fn has(args: &[String], flag: &str) -> bool {
        args.iter().any(|arg| arg == flag)
    }

    // ---- 17: binary resolution --------------------------------------------------------------

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn agy_program_prefers_env_then_path_then_localappdata() {
        let on_path = tempfile::tempdir().unwrap();
        touch(&on_path.path().join("agy.exe"));
        let path_var = std::env::join_paths([on_path.path()]).unwrap();
        let local = tempfile::tempdir().unwrap();
        touch(&local.path().join("agy").join("bin").join("agy.exe"));
        let local_var = Some(OsString::from(local.path()));

        // The override is taken as given, even when nothing exists there.
        let override_bin = PathBuf::from("/nowhere/at-all/agy-override");
        assert_eq!(
            agy_program_from(
                Some(override_bin.clone().into_os_string()),
                Some(path_var.clone()),
                local_var.clone()
            ),
            Some(override_bin)
        );
        // Then PATH.
        assert_eq!(
            agy_program_from(None, Some(path_var), local_var.clone()),
            Some(on_path.path().join("agy.exe"))
        );
        // Then %LOCALAPPDATA%\agy\bin\agy.exe, when PATH has no agy.
        let empty = tempfile::tempdir().unwrap();
        let empty_path = std::env::join_paths([empty.path()]).unwrap();
        assert_eq!(
            agy_program_from(None, Some(empty_path.clone()), local_var),
            Some(local.path().join("agy").join("bin").join("agy.exe"))
        );
        // Nothing anywhere: not installed.
        assert_eq!(agy_program_from(None, Some(empty_path), None), None);
        assert_eq!(agy_program_from(None, None, None), None);
    }

    // ---- 18 to 23: the argument vector ------------------------------------------------------

    #[test]
    fn build_args_never_emits_dangerously_skip_permissions() {
        for permission in [
            Permission::Default,
            Permission::AcceptEdits,
            Permission::Plan,
            Permission::Bypass,
        ] {
            for tool_policy in [
                ToolPolicy::Unrestricted,
                ToolPolicy::McpOnly,
                ToolPolicy::None,
            ] {
                for classifier in [false, true] {
                    let mut asked = request("do the thing");
                    asked.permission = permission;
                    asked.tool_policy = tool_policy;
                    asked.classifier_governs_tools = classifier;
                    if let Ok(args) = build_args(&asked, &options()) {
                        assert!(
                            !args
                                .iter()
                                .any(|arg| arg.contains("dangerously-skip-permissions")),
                            "{permission:?}/{tool_policy:?}/{classifier}: {args:?}"
                        );
                    }
                }
            }
        }
        // Bypass is refused by name rather than translated into anything.
        let mut asked = request("do the thing");
        asked.permission = Permission::Bypass;
        let refusal = build_args(&asked, &options()).unwrap_err().to_string();
        assert!(refusal.contains("Bypass"), "{refusal}");
        assert!(
            refusal.contains("the agy engine does not support it"),
            "{refusal}"
        );
    }

    #[test]
    fn build_args_maps_permission_plan_to_mode_plan_and_accept_edits_to_mode_accept_edits() {
        let mut asked = request("p");
        asked.permission = Permission::Plan;
        let args = build_args(&asked, &options()).unwrap();
        assert_eq!(pair(&args, "--mode").as_deref(), Some("plan"), "{args:?}");

        asked.permission = Permission::AcceptEdits;
        let args = build_args(&asked, &options()).unwrap();
        assert_eq!(
            pair(&args, "--mode").as_deref(),
            Some("accept-edits"),
            "{args:?}"
        );

        // Default asks for nothing extra: agy's own default mode.
        asked.permission = Permission::Default;
        let args = build_args(&asked, &options()).unwrap();
        assert!(!has(&args, "--mode"), "{args:?}");

        // The fixed part of every vector.
        assert_eq!(args.first().map(String::as_str), Some("-p"), "{args:?}");
        assert_eq!(args.get(1).map(String::as_str), Some("p"), "{args:?}");
        assert_eq!(pair(&args, "--output-format").as_deref(), Some("json"));
        assert_eq!(
            pair(&args, "--model").as_deref(),
            Some("gemini-3.8-flash-high"),
            "the agy/ prefix is the catalogue's, not the CLI's: {args:?}"
        );
    }

    #[test]
    fn build_args_adds_sandbox_unless_unrestricted() {
        let mut asked = request("p");
        let args = build_args(&asked, &options()).unwrap();
        assert!(!has(&args, "--sandbox"), "{args:?}");

        // A restricted policy always runs in plan mode with the sandbox, whatever the
        // permission would otherwise map to (accept-edits included).
        for policy in [ToolPolicy::McpOnly, ToolPolicy::None] {
            for permission in [Permission::Default, Permission::AcceptEdits] {
                asked.tool_policy = policy;
                asked.permission = permission;
                let args = build_args(&asked, &options())
                    .unwrap_or_else(|refusal| panic!("{policy:?}/{permission:?}: {refusal}"));
                assert_eq!(
                    pair(&args, "--mode").as_deref(),
                    Some("plan"),
                    "{policy:?}/{permission:?}: {args:?}"
                );
                assert!(
                    has(&args, "--sandbox"),
                    "{policy:?}/{permission:?}: {args:?}"
                );
            }
        }

        // Unrestricted carries no sandbox, also with accept-edits.
        asked.tool_policy = ToolPolicy::Unrestricted;
        asked.permission = Permission::AcceptEdits;
        let args = build_args(&asked, &options()).unwrap();
        assert!(!has(&args, "--sandbox"), "{args:?}");
    }

    #[test]
    fn build_args_resume_uses_conversation_not_continue() {
        let mut asked = request("p");
        asked.resume_session_id = Some("conv-9".to_string());
        let args = build_args(&asked, &options()).unwrap();
        assert_eq!(
            pair(&args, "--conversation").as_deref(),
            Some("conv-9"),
            "{args:?}"
        );
        assert!(!has(&args, "-c") && !has(&args, "--continue"), "{args:?}");

        // No resume, no conversation flag.
        let args = build_args(&request("p"), &options()).unwrap();
        assert!(!has(&args, "--conversation"), "{args:?}");

        // The run's silence deadline reaches the CLI as its own print timeout, in seconds.
        let mut asked = request("p");
        asked.progress_timeout = Some(std::time::Duration::from_secs(120));
        let args = build_args(&asked, &options()).unwrap();
        assert_eq!(
            pair(&args, "--print-timeout").as_deref(),
            Some("120"),
            "{args:?}"
        );
    }

    #[test]
    fn build_args_refuses_bypass_fallback_budget_agents_append_denied_mcp_by_name() {
        type Change = fn(&mut RunRequest);
        let cases: [(&str, Change); 8] = [
            ("Bypass", |r| r.permission = Permission::Bypass),
            ("fallback_model", |r| r.fallback_model = vec!["x".into()]),
            ("max_budget_usd", |r| r.max_budget_usd = Some(1.0)),
            ("agents", |r| {
                r.agents = vec![Subagent {
                    name: "helper".into(),
                    description: "d".into(),
                    prompt: "p".into(),
                    tools: None,
                    model: None,
                    effort: None,
                }]
            }),
            ("append_system_prompt", |r| {
                r.append_system_prompt = Some("be brief".into())
            }),
            ("denied_tools", |r| r.denied_tools = vec!["Bash".into()]),
            ("mcp_config", |r| {
                r.mcp_config = Some(PathBuf::from("mcp.json"))
            }),
            ("classifier_governs_tools", |r| {
                r.classifier_governs_tools = true
            }),
        ];
        for (name, change) in cases {
            let mut asked = request("p");
            change(&mut asked);
            let refusal = build_args(&asked, &options())
                .expect_err(&format!("{name} must be refused"))
                .to_string();
            assert!(refusal.contains(name), "{name}: {refusal}");
            assert!(
                refusal.contains("the agy engine does not support it"),
                "{name}: {refusal}"
            );
        }
    }

    #[test]
    fn build_args_rejects_invalid_effort() {
        for level in ["low", "medium", "high", "xhigh", "max"] {
            let mut asked = request("p");
            asked.effort = Some(level.to_string());
            let args = build_args(&asked, &options()).unwrap();
            assert_eq!(pair(&args, "--effort").as_deref(), Some(level), "{args:?}");
        }
        for bad in ["", "ultra", "HIGH", "high --sandbox"] {
            let mut asked = request("p");
            asked.effort = Some(bad.to_string());
            let refusal = build_args(&asked, &options())
                .expect_err(&format!("effort {bad:?} must be refused"))
                .to_string();
            assert!(refusal.contains("effort"), "{bad:?}: {refusal}");
        }
        // The runner's configured effort is checked the same way, and no effort writes no flag.
        let mut configured = options();
        configured.effort = Some("ultra".to_string());
        assert!(build_args(&request("p"), &configured).is_err());
        let args = build_args(&request("p"), &options()).unwrap();
        assert!(!has(&args, "--effort"), "{args:?}");
    }

    // ---- 24: long prompts -------------------------------------------------------------------

    #[test]
    fn long_prompt_is_staged_in_a_file_and_argv_stays_short() {
        let stage = tempfile::tempdir().unwrap();
        let mut staged_options = options();
        staged_options.stage_dir = stage.path().to_path_buf();
        let long = "0123456789".repeat(4_000);
        assert_eq!(long.len(), 40_000);
        let asked = request(&long);

        let args = build_args(&asked, &staged_options).unwrap();
        let total: usize = args.iter().map(String::len).sum();
        assert!(
            total < 10_000,
            "argv is {total} bytes: it must not carry the prompt"
        );
        assert!(
            !args.iter().any(|arg| arg.contains(&long[..100])),
            "{args:?}"
        );
        let file = stage.path().join(STAGED_PROMPT_FILE);
        let instruction = pair(&args, "-p").expect("-p is present");
        assert!(
            instruction.contains(&file.display().to_string()),
            "the instruction names the staged file: {instruction}"
        );
        assert_eq!(
            pair(&args, "--add-dir").as_deref(),
            Some(stage.path().display().to_string().as_str()),
            "{args:?}"
        );

        // `stage_prompt` writes the file the instruction points at, with the whole prompt in it.
        let written = stage_prompt(&asked, stage.path()).unwrap();
        assert_eq!(written.as_deref(), Some(file.as_path()));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), long);

        // The threshold: exactly the limit goes inline, one byte more is staged.
        let at_limit = "a".repeat(INLINE_PROMPT_LIMIT);
        let args = build_args(&request(&at_limit), &staged_options).unwrap();
        assert_eq!(pair(&args, "-p").as_deref(), Some(at_limit.as_str()));
        assert!(!has(&args, "--add-dir"), "inline prompts add no directory");
        let over = "a".repeat(INLINE_PROMPT_LIMIT + 1);
        let args = build_args(&request(&over), &staged_options).unwrap();
        assert!(has(&args, "--add-dir"), "{args:?}");
        let other = tempfile::tempdir().unwrap();
        assert_eq!(
            stage_prompt(&request(&at_limit), other.path()).unwrap(),
            None
        );
        assert!(!other.path().join(STAGED_PROMPT_FILE).exists());

        // End to end: the fake records its argv, and the 40,000 bytes are not in it.
        let long_request = request(&long);
        let fake = FakeAgy::new(&success_json("staged ok"), 0, false);
        let outcome = fake.run(long_request);
        assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
        let recorded = fake.recorded_args();
        let recorded_len: usize = recorded.iter().map(String::len).sum();
        assert!(
            recorded_len < 10_000,
            "the CLI saw {recorded_len} bytes of argv"
        );
        assert!(
            recorded.iter().any(|arg| arg == "--add-dir"),
            "{recorded:?}"
        );
    }

    // ---- 25 to 29: the runner against a fake binary -----------------------------------------

    /// Points `NUCLEOS_AGY_BIN` at a program for as long as it lives and puts the previous value
    /// back on drop. Under `worktree::test_env_lock`, which every test that writes the process
    /// environment takes: the variable is process-wide.
    struct AgyBinGuard {
        previous: Option<OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl AgyBinGuard {
        fn set(program: &Path) -> Self {
            let lock = crate::execution::worktree::test_env_lock();
            let previous = std::env::var_os("NUCLEOS_AGY_BIN");
            unsafe { std::env::set_var("NUCLEOS_AGY_BIN", program) };
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for AgyBinGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("NUCLEOS_AGY_BIN", value),
                    None => std::env::remove_var("NUCLEOS_AGY_BIN"),
                }
            }
        }
    }

    /// A stand-in `agy`: records its argv to `args.txt`, prints `out.json`, optionally hangs, and
    /// exits with `exit`. A `.cmd` on Windows and a `sh` script elsewhere.
    struct FakeAgy {
        dir: tempfile::TempDir,
        _guard: AgyBinGuard,
    }

    impl FakeAgy {
        fn new(stdout: &str, exit: i32, hang: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("out.json"), stdout).unwrap();
            let program = write_script(dir.path(), exit, hang);
            let guard = AgyBinGuard::set(&program);
            Self { dir, _guard: guard }
        }

        fn recorded_args(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("args.txt"))
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect()
        }

        fn run(&self, request: RunRequest) -> crate::execution::runner::RunOutcome {
            self.run_with_channels(request).0
        }

        fn run_with_channels(
            &self,
            request: RunRequest,
        ) -> (
            crate::execution::runner::RunOutcome,
            tokio::sync::mpsc::UnboundedReceiver<String>,
            std::sync::Arc<std::sync::Mutex<String>>,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (session_tx, session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let runner = AgyCliRunner {
                model: "agy/gemini-3.8-flash-high".to_string(),
                effort: None,
            };
            let outcome = runtime
                .block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        runner.run_prompt(request, session_tx, transcript.clone()),
                    )
                    .await
                })
                .expect("the fake agy finished within a minute")
                .expect("the fake agy spawned");
            (outcome, session_rx, transcript)
        }
    }

    #[cfg(windows)]
    fn write_script(dir: &Path, exit: i32, hang: bool) -> PathBuf {
        let path = dir.join("agy.cmd");
        let wait = if hang {
            "ping -n 60 127.0.0.1 >nul\r\n"
        } else {
            ""
        };
        let script = format!(
            "@echo off\r\necho %*> \"%~dp0args.txt\"\r\n{wait}type \"%~dp0out.json\"\r\nexit /b {exit}\r\n"
        );
        std::fs::write(&path, script).unwrap();
        path
    }

    #[cfg(unix)]
    fn write_script(dir: &Path, exit: i32, hang: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("agy.sh");
        let wait = if hang { "sleep 60\n" } else { "" };
        let script = format!(
            "#!/bin/sh\nd=$(dirname \"$0\")\nprintf '%s\\n' \"$@\" > \"$d/args.txt\"\n{wait}cat \"$d/out.json\"\nexit {exit}\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn success_json(response: &str) -> String {
        serde_json::json!({
            "conversation_id": "conv-1",
            "status": "SUCCESS",
            "response": response,
            "duration_seconds": 1.5,
            "num_turns": 2,
            "usage": {
                "input_tokens": 11,
                "output_tokens": 22,
                "thinking_tokens": 0,
                "cache_read_tokens": 3,
                "total_tokens": 33
            }
        })
        .to_string()
    }

    #[test]
    fn fake_agy_success_json_yields_reply_session_and_tokens() {
        let fake = FakeAgy::new(&success_json("hello there"), 0, false);

        let (outcome, mut session_rx, transcript) = fake.run_with_channels(request("hi"));

        assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
        assert_eq!(outcome.session_id.as_deref(), Some("conv-1"));
        assert_eq!(session_rx.try_recv().ok().as_deref(), Some("conv-1"));
        assert_eq!(outcome.input_tokens, Some(11));
        assert_eq!(outcome.output_tokens, Some(22));
        assert_eq!(outcome.cache_read_tokens, Some(3));
        assert_eq!(outcome.num_turns, Some(2));
        assert_eq!(
            outcome.cost_usd, None,
            "agy reports no price: unknown, not free"
        );
        // The reply is read the way every other runner's is: from a synthesised `result` line.
        assert_eq!(
            extract_reply(&outcome.stdout).as_deref(),
            Some("hello there")
        );
        let mirrored = transcript.lock().unwrap().clone();
        assert_eq!(extract_reply(&mirrored).as_deref(), Some("hello there"));
        // The vector the fake saw is the one `build_args` makes.
        let recorded = fake.recorded_args();
        assert!(
            recorded.iter().any(|arg| arg == "--output-format"),
            "{recorded:?}"
        );
        assert!(
            recorded.iter().any(|arg| arg == "gemini-3.8-flash-high"),
            "{recorded:?}"
        );
    }

    #[test]
    fn fake_agy_non_success_status_fails_closed_naming_status() {
        for status in ["FAILED", "CANCELLED", "success"] {
            let body = serde_json::json!({
                "conversation_id": "conv-2",
                "status": status,
                "response": "looks fine but is not",
                "num_turns": 1,
                "usage": { "input_tokens": 1, "output_tokens": 1 }
            })
            .to_string();
            let fake = FakeAgy::new(&body, 0, false);

            let outcome = fake.run(request("hi"));

            assert_ne!(
                outcome.exit_code, 0,
                "status {status} must not read as success"
            );
            assert!(
                outcome
                    .stderr
                    .contains(&format!("agy returned status {status}")),
                "{status}: {}",
                outcome.stderr
            );
        }
    }

    #[test]
    fn fake_agy_garbage_stdout_fails_closed() {
        let fake = FakeAgy::new("this is not json at all", 0, false);
        let outcome = fake.run(request("hi"));
        assert_ne!(outcome.exit_code, 0);
        assert!(
            outcome.stderr.contains("this is not json"),
            "{}",
            outcome.stderr
        );
        // The guard holds the test env lock: release it before the next fake takes it.
        drop(fake);

        // Valid JSON with no `response` is no answer either.
        let fake = FakeAgy::new(r#"{"status":"SUCCESS","conversation_id":"c"}"#, 0, false);
        let outcome = fake.run(request("hi"));
        assert_ne!(outcome.exit_code, 0);
        assert!(
            outcome.stderr.contains("SUCCESS"),
            "the first bytes are shown: {}",
            outcome.stderr
        );
        drop(fake);

        // The echoed bytes are bounded.
        let fake = FakeAgy::new(&"x".repeat(5_000), 0, false);
        let outcome = fake.run(request("hi"));
        assert_ne!(outcome.exit_code, 0);
        assert!(
            outcome.stderr.len() < 1_500,
            "{} bytes",
            outcome.stderr.len()
        );
    }

    #[test]
    fn fake_agy_nonzero_exit_fails_closed() {
        // A well-formed SUCCESS body does not rescue a process that exited non-zero.
        let fake = FakeAgy::new(&success_json("hello"), 3, false);
        let outcome = fake.run(request("hi"));
        assert_ne!(outcome.exit_code, 0, "{}", outcome.stderr);
    }

    #[test]
    fn fake_agy_hang_is_killed_on_progress_timeout() {
        let fake = FakeAgy::new(&success_json("never printed"), 0, true);
        let mut asked = request("hi");
        asked.progress_timeout = Some(std::time::Duration::from_millis(500));

        let started = std::time::Instant::now();
        let outcome = fake.run(asked);

        assert_eq!(
            outcome.exit_code, PROGRESS_TIMEOUT_EXIT_CODE,
            "{}",
            outcome.stderr
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the hanging fake was killed, not waited out"
        );
    }

    #[test]
    fn num_turns_over_max_turns_fails_closed() {
        let body = |turns: i64| {
            serde_json::json!({
                "conversation_id": "conv-3",
                "status": "SUCCESS",
                "response": "done",
                "num_turns": turns,
                "usage": { "input_tokens": 1, "output_tokens": 1 }
            })
            .to_string()
        };
        let fake = FakeAgy::new(&body(5), 0, false);
        let mut asked = request("hi");
        asked.max_turns = Some(2);
        let outcome = fake.run(asked);
        assert_ne!(outcome.exit_code, 0, "five turns under a ceiling of two");
        assert!(outcome.stderr.contains("turns"), "{}", outcome.stderr);
        // The guard holds the test env lock: release it before the next fake takes it.
        drop(fake);

        let fake = FakeAgy::new(&body(1), 0, false);
        let mut asked = request("hi");
        asked.max_turns = Some(2);
        assert_eq!(
            fake.run(asked).exit_code,
            0,
            "one turn is under the ceiling"
        );
    }
}
