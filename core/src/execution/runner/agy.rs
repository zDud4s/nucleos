//! Tests of the Antigravity CLI backend (plan Node 2, tests 17 to 29).
//!
//! RED: `agy_program`, `build_args`, `Options`, `Refusal`, `stage_prompt` and `AgyCliRunner` do not
//! exist yet, so this module fails to compile. Every run-level test goes through a fake binary
//! chosen by `NUCLEOS_AGY_BIN`; the real `agy` is never started.

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

        // Any other policy is either refused by name or carries the sandbox; never neither.
        for policy in [ToolPolicy::McpOnly, ToolPolicy::None] {
            asked.tool_policy = policy;
            match build_args(&asked, &options()) {
                Ok(args) => assert!(has(&args, "--sandbox"), "{policy:?}: {args:?}"),
                Err(refusal) => assert!(
                    refusal.to_string().contains("the agy engine does not support it"),
                    "{policy:?}: {refusal}"
                ),
            }
        }
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
            ("mcp_config", |r| r.mcp_config = Some(PathBuf::from("mcp.json"))),
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
        assert!(total < 10_000, "argv is {total} bytes: it must not carry the prompt");
        assert!(!args.iter().any(|arg| arg.contains(&long[..100])), "{args:?}");
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
        assert_eq!(stage_prompt(&request(&at_limit), other.path()).unwrap(), None);
        assert!(!other.path().join(STAGED_PROMPT_FILE).exists());

        // End to end: the fake records its argv, and the 40,000 bytes are not in it.
        let long_request = request(&long);
        let fake = FakeAgy::new(&success_json("staged ok"), 0, false);
        let outcome = fake.run(long_request);
        assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
        let recorded = fake.recorded_args();
        let recorded_len: usize = recorded.iter().map(String::len).sum();
        assert!(recorded_len < 10_000, "the CLI saw {recorded_len} bytes of argv");
        assert!(recorded.iter().any(|arg| arg == "--add-dir"), "{recorded:?}");
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
        let wait = if hang { "ping -n 60 127.0.0.1 >nul\r\n" } else { "" };
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
        assert_eq!(outcome.cost_usd, None, "agy reports no price: unknown, not free");
        // The reply is read the way every other runner's is: from a synthesised `result` line.
        assert_eq!(extract_reply(&outcome.stdout).as_deref(), Some("hello there"));
        let mirrored = transcript.lock().unwrap().clone();
        assert_eq!(extract_reply(&mirrored).as_deref(), Some("hello there"));
        // The vector the fake saw is the one `build_args` makes.
        let recorded = fake.recorded_args();
        assert!(recorded.iter().any(|arg| arg == "--output-format"), "{recorded:?}");
        assert!(recorded.iter().any(|arg| arg == "gemini-3.8-flash-high"), "{recorded:?}");
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

            assert_ne!(outcome.exit_code, 0, "status {status} must not read as success");
            assert!(
                outcome.stderr.contains(&format!("agy returned status {status}")),
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
        assert!(outcome.stderr.contains("this is not json"), "{}", outcome.stderr);

        // Valid JSON with no `response` is no answer either.
        let fake = FakeAgy::new(r#"{"status":"SUCCESS","conversation_id":"c"}"#, 0, false);
        let outcome = fake.run(request("hi"));
        assert_ne!(outcome.exit_code, 0);
        assert!(outcome.stderr.contains("SUCCESS"), "the first bytes are shown: {}", outcome.stderr);

        // The echoed bytes are bounded.
        let fake = FakeAgy::new(&"x".repeat(5_000), 0, false);
        let outcome = fake.run(request("hi"));
        assert_ne!(outcome.exit_code, 0);
        assert!(outcome.stderr.len() < 1_500, "{} bytes", outcome.stderr.len());
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

        assert_eq!(outcome.exit_code, PROGRESS_TIMEOUT_EXIT_CODE, "{}", outcome.stderr);
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

        let fake = FakeAgy::new(&body(1), 0, false);
        let mut asked = request("hi");
        asked.max_turns = Some(2);
        assert_eq!(fake.run(asked).exit_code, 0, "one turn is under the ceiling");
    }
}
