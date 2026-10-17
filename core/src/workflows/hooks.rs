//! §spec motor-de-workflows
//!
//! Claude Code hooks a workflow bundle declares, and running them on the daemon's side.
//!
//! A bundle's `bundle.yaml` may carry a `hooks:` list — an event, an optional tool matcher, a
//! script relative to the bundle's version directory, its arguments and a timeout. The project's
//! `.claude/settings.json` does NOT gain an entry per hook: that file stays the daemon's
//! (`materialize::RESERVED`), wired once with the one forwarding script `ask_daemon.py`
//! (`governance::autopilot::wire_classifier_hook`). When Claude Code fires an event the script
//! hands the payload to `POST /hooks/bundle`, and this module finds the project the session is
//! standing in, the bundles it pins, and the hooks they declare for that event.
//!
//! # The script runs from the library, never from the project
//!
//! The path that is executed is `<library>/<name>/<version>/<script>`, and nothing here ever copies
//! it into a checkout. That is the whole integrity argument: a hook is very often a GATE on the
//! agent whose session fires it (a `Stop` hook that refuses to let a session end with work open),
//! and a gate that lives in the working tree is one the agent it watches can edit. `owns:` decides
//! what a project is given; a hook script may sit outside it, and then no materialization ever
//! puts a copy where an agent could reach it.
//!
//! # Failing open
//!
//! Every way of not knowing — no project for the directory, no pins, a manifest that will not
//! parse, a script that will not start — answers with no result for that hook, which the script
//! reads as "nothing to say". A broken guard must never wedge a session: a `Stop` hook that cannot
//! run and refused instead would leave a person with a conversation that can never end.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::workflows;

/// The events a bundle may declare a hook on. The first three are tool events, which take a
/// matcher; the rest are the session events `autopilot::SESSION_EVENTS` wires.
pub const EVENTS: [&str; 6] = [
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SubagentStop",
    "UserPromptSubmit",
];

/// Whether `event` is about one tool call, and so has a tool name a matcher can be read against.
fn is_tool_event(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse" | "PostToolUseFailure")
}

/// A hook's own timeout when its declaration names none.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// How long one call of the route may spend running hooks, all of them together.
///
/// Claude Code kills a command hook after sixty seconds unless the settings entry says otherwise,
/// and the entry `ask_daemon.py` is registered under says nothing (it is the same entry for every
/// event). So the daemon has to answer inside that, with room for the script's own start and the
/// round trip; a later hook gets what is left rather than its full timeout. It is also the ceiling
/// a declared `timeout_secs` is validated against, because a longer one could never be honoured.
pub const TOTAL_BUDGET: Duration = Duration::from_secs(50);

/// How much of each stream is handed back. A hook's output becomes context in a conversation, and
/// a script that prints a megabyte would put a megabyte into it.
const MAX_OUTPUT: usize = 64 * 1024;

/// One hook, as a bundle's manifest declares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookDecl {
    /// One of [`EVENTS`].
    pub event: String,
    /// Which tool calls it is for, Claude Code's way: absent, empty or `*` is every one, anything
    /// else a regular expression the whole tool name must match (`Edit|Write`). Ignored on the
    /// session events, which carry no tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// Relative to the bundle's version directory, and inside it.
    pub script: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl HookDecl {
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS))
    }

    /// Whether this hook is for `event`, and — on a tool event — for `tool_name`.
    fn applies_to(&self, event: &str, tool_name: Option<&str>) -> bool {
        if self.event != event {
            return false;
        }
        if !is_tool_event(event) {
            return true;
        }
        match self.matcher.as_deref().map(str::trim) {
            None | Some("") | Some("*") => true,
            Some(pattern) => regex::Regex::new(&format!("^(?:{pattern})$"))
                .is_ok_and(|matcher| matcher.is_match(tool_name.unwrap_or(""))),
        }
    }
}

/// Check a manifest's hooks against the files the bundle has. `is_file` answers for a normalised,
/// bundle-relative path.
///
/// Refused rather than skipped, because this runs where somebody can still fix the manifest —
/// packaging, and the daemon reading a bundle — and a hook that silently never runs is a guard that
/// is off while the manifest says it is on.
pub fn validate(hooks: &[HookDecl], is_file: impl Fn(&str) -> bool) -> Result<(), String> {
    for hook in hooks {
        if !EVENTS.contains(&hook.event.as_str()) {
            return Err(format!(
                "hook event `{}` is not one of {}",
                hook.event,
                EVENTS.join(", ")
            ));
        }
        let Some(rel) = crate::ownership::normalise(&hook.script) else {
            return Err(format!(
                "hook script `{}` is not a relative path inside the bundle (no `..`, nothing \
                 absolute, forward slashes)",
                hook.script
            ));
        };
        if !is_file(&rel) {
            return Err(format!("hook script `{rel}` is not a file in the bundle"));
        }
        if let Some(secs) = hook.timeout_secs
            && (secs == 0 || secs > TOTAL_BUDGET.as_secs())
        {
            return Err(format!(
                "hook `{rel}`: timeout_secs must be between 1 and {}",
                TOTAL_BUDGET.as_secs()
            ));
        }
        if let Some(pattern) = hook.matcher.as_deref()
            && is_tool_event(&hook.event)
            && regex::Regex::new(&format!("^(?:{pattern})$")).is_err()
        {
            return Err(format!(
                "hook `{rel}`: matcher `{pattern}` is not a regular expression"
            ));
        }
    }
    Ok(())
}

/// The hooks one bundle version declares, validated against the files it actually has.
pub fn manifest_hooks(dir: &Path) -> Result<Vec<HookDecl>, String> {
    let manifest_path = dir.join(workflows::MANIFEST);
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    let manifest: workflows::ManifestFile = serde_yaml::from_str::<Option<_>>(&text)
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?
        .unwrap_or_default();
    let hooks = parse(manifest.hooks)
        .and_then(|hooks| validate(&hooks, |rel| dir.join(rel).is_file()).map(|()| hooks))
        .map_err(|error| format!("{}: {error}", manifest_path.display()))?;
    Ok(hooks)
}

/// The manifest's `hooks:` value as declarations. Absent and `null` are no hooks.
pub fn parse(raw: serde_yaml::Value) -> Result<Vec<HookDecl>, String> {
    if raw.is_null() {
        return Ok(Vec::new());
    }
    serde_yaml::from_value(raw).map_err(|error| format!("hooks: {error}"))
}

/// A hook chosen to run, and the bundle it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Selected {
    /// `name@version`, for the result and for the log line when it goes wrong.
    pub bundle: String,
    /// The bundle's version directory in the library.
    pub dir: PathBuf,
    pub hook: HookDecl,
}

/// Every hook the pinned bundles declare for this event, pins in file order and each bundle's
/// hooks in manifest order — the order they run in.
///
/// Always the library's copy, whatever the pin says about an ejected one: a project's own copy is
/// in the working tree, which is exactly where a gate must not be read from (module header).
/// A pin the library cannot answer, or a manifest that does not validate, contributes nothing and
/// is logged: failing open, for the reason the module header gives.
pub fn select(
    library: &Path,
    pins: &workflows::Pins,
    event: &str,
    tool_name: Option<&str>,
) -> Vec<Selected> {
    let mut selected = Vec::new();
    for pin in &pins.workflows {
        let dir = library.join(&pin.name).join(&pin.version);
        if !dir.join(workflows::MANIFEST).is_file() {
            continue;
        }
        let hooks = match manifest_hooks(&dir) {
            Ok(hooks) => hooks,
            Err(error) => {
                tracing::warn!(bundle = %pin.name, version = %pin.version, %error,
                    "a pinned bundle's hooks were skipped");
                continue;
            }
        };
        for hook in hooks {
            if hook.applies_to(event, tool_name) {
                selected.push(Selected {
                    bundle: format!("{}@{}", pin.name, pin.version),
                    dir: dir.clone(),
                    hook,
                });
            }
        }
    }
    selected
}

/// What one hook did. `exit_code` is `None` when there is no code to report: it would not start,
/// or it ran past its timeout and was killed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookOutcome {
    pub bundle: String,
    pub script: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

fn capped(bytes: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Run one hook: `<interpreter> <dir>/<script> <args...>`, the payload on stdin, in `cwd`, with
/// the daemon's environment, for at most `limit`.
///
/// `interpreter` is a parameter so the tests can say which one; production passes
/// [`crate::governance::autopilot::HOOK_INTERPRETER`], the same one `ask_daemon.py` is registered
/// under, for the reasons given there.
pub async fn run_one(
    interpreter: &str,
    selected: &Selected,
    payload: &[u8],
    cwd: &Path,
    limit: Duration,
) -> HookOutcome {
    use tokio::io::AsyncWriteExt;

    let script = selected.dir.join(&selected.hook.script);
    let outcome = |exit_code, stdout: &[u8], stderr: String| HookOutcome {
        bundle: selected.bundle.clone(),
        script: selected.hook.script.clone(),
        exit_code,
        stdout: capped(stdout),
        stderr,
    };

    let mut command = tokio::process::Command::new(interpreter);
    command
        .arg(&script)
        .args(&selected.hook.args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // A hook past its time is killed, not left running behind an answer already given.
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return outcome(
                None,
                b"",
                format!(
                    "could not start {interpreter} {}: {error}",
                    script.display()
                ),
            );
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        // Written beside the wait rather than before it, so a script that prints a lot before it
        // reads cannot deadlock against a full pipe. One that never reads closes the pipe early;
        // that is its business, not a failure of the hook.
        let payload = payload.to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&payload).await;
        });
    }
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(output)) => outcome(output.status.code(), &output.stdout, capped(&output.stderr)),
        Ok(Err(error)) => outcome(None, b"", format!("waiting for the hook: {error}")),
        Err(_) => outcome(
            None,
            b"",
            format!("ran past its {limit:?} timeout and was stopped"),
        ),
    }
}

/// Run `selected` in order inside [`TOTAL_BUDGET`], stopping at the first that exits 2.
///
/// Stopping there because exit 2 is Claude Code's "block": the event is refused and only that
/// hook's stderr is read, so running the rest would spend the budget on output nobody sees — and a
/// later hook with side effects would act on an event that did not happen.
pub async fn run_all(
    interpreter: &str,
    selected: &[Selected],
    payload: &[u8],
    cwd: &Path,
) -> Vec<HookOutcome> {
    let deadline = Instant::now() + TOTAL_BUDGET;
    let mut outcomes = Vec::new();
    for one in selected {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let outcome = run_one(interpreter, one, payload, cwd, one.hook.timeout().min(left)).await;
        let blocked = outcome.exit_code == Some(2);
        outcomes.push(outcome);
        if blocked {
            break;
        }
    }
    outcomes
}

/// Everything the route needs, resolved from the hook payload: which project the session is in,
/// what it pins, what to run. An empty answer for every case that has nothing to run.
pub async fn for_payload(
    pool: &sqlx::SqlitePool,
    library: &Path,
    machine_root: &Path,
    interpreter: &str,
    payload: &serde_json::Value,
) -> Vec<HookOutcome> {
    let Some(event) = payload.get("hook_event_name").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    let Some(cwd) = payload.get("cwd").and_then(|v| v.as_str()) else {
        return Vec::new();
    };
    let cwd = Path::new(cwd);
    let tool_name = payload.get("tool_name").and_then(|v| v.as_str());

    // The same resolution the vcs queue uses for a session's own directory: the working tree it
    // stands in, then the roster's project whose main checkout that tree belongs to — so a linked
    // worktree answers for its project exactly as the main checkout does.
    let deadline = Instant::now() + Duration::from_secs(10);
    let Ok(root) = crate::execution::git_exec::toplevel(cwd, deadline).await else {
        return Vec::new();
    };
    let project_id = match crate::execution::vcs::project_for_worktree(pool, &root, deadline).await
    {
        Ok(project_id) => project_id,
        Err(_) => return Vec::new(),
    };
    let Some(pins_file) = crate::project::state::file(
        Some(machine_root),
        &project_id,
        crate::project::state::PINS_FILE,
    ) else {
        return Vec::new();
    };
    let pins = match workflows::read_pins(&pins_file) {
        Ok(pins) => pins,
        Err(error) => {
            tracing::warn!(project = %project_id, %error, "bundle hooks skipped: unreadable pins");
            return Vec::new();
        }
    };

    let selected = select(library, &pins, event, tool_name);
    if selected.is_empty() {
        return Vec::new();
    }
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    run_all(interpreter, &selected, &bytes, cwd).await
}

/// The answer of `POST /hooks/bundle`.
#[derive(Debug, Serialize, Deserialize)]
pub struct BundleHooksAnswer {
    pub results: Vec<HookOutcome>,
}

/// `POST /hooks/bundle`: the hook payload in, what each matching bundle hook did out.
///
/// Always 200. A machine with no library or no home has no bundle hooks, which is an empty answer
/// and not an error — the script would read an error as "nothing to say" anyway, and an empty list
/// says that without a log line on every prompt.
pub async fn bundle_hooks(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Json(payload): axum::Json<serde_json::Value>,
) -> axum::Json<BundleHooksAnswer> {
    let results = match (
        state.workflow_library.as_deref(),
        state.machine_config_root.as_deref(),
    ) {
        (Some(library), Some(machine_root)) => {
            for_payload(
                &state.pool,
                library,
                machine_root,
                crate::governance::autopilot::HOOK_INTERPRETER,
                &payload,
            )
            .await
        }
        _ => Vec::new(),
    };
    axum::Json(BundleHooksAnswer { results })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_available() -> bool {
        std::process::Command::new(crate::governance::autopilot::HOOK_INTERPRETER)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn decl(event: &str, script: &str) -> HookDecl {
        HookDecl {
            event: event.to_string(),
            matcher: None,
            script: script.to_string(),
            args: Vec::new(),
            timeout_secs: None,
        }
    }

    /// A library with one bundle version holding `manifest` and `files`.
    fn shelf(library: &Path, name: &str, version: &str, manifest: &str, files: &[(&str, &str)]) {
        let dir = library.join(name).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(workflows::MANIFEST), manifest).unwrap();
        for (rel, contents) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
    }

    fn pins(entries: &[(&str, &str)]) -> workflows::Pins {
        workflows::Pins {
            workflows: entries
                .iter()
                .map(|(name, version)| workflows::Pin {
                    name: name.to_string(),
                    version: version.to_string(),
                    origin: format!("library:{name}@{version}"),
                    hash: "sha256:any".to_string(),
                    ejected_at: None,
                    path: None,
                    nodes: Default::default(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_hook_on_a_known_event_with_its_script_present_is_accepted() {
        let mut hook = decl("Stop", "scripts/guard.py");
        hook.timeout_secs = Some(10);
        assert_eq!(validate(&[hook], |rel| rel == "scripts/guard.py"), Ok(()));
    }

    #[test]
    fn an_unknown_event_is_refused() {
        let error = validate(&[decl("SessionEnd", "guard.py")], |_| true).unwrap_err();
        assert!(error.contains("SessionEnd"), "{error}");
    }

    /// The script is joined onto the library's version directory and executed, so a path that
    /// leaves it would run whatever it named.
    #[test]
    fn a_script_outside_the_bundle_or_absent_from_it_is_refused() {
        for script in [
            "../other/guard.py",
            "/etc/guard.py",
            "C:/guard.py",
            "a\\b.py",
        ] {
            assert!(
                validate(&[decl("Stop", script)], |_| true).is_err(),
                "{script}"
            );
        }
        let error = validate(&[decl("Stop", "scripts/guard.py")], |_| false).unwrap_err();
        assert!(error.contains("not a file"), "{error}");
    }

    #[test]
    fn a_timeout_the_route_could_never_honour_is_refused() {
        let mut hook = decl("Stop", "guard.py");
        hook.timeout_secs = Some(TOTAL_BUDGET.as_secs() + 1);
        assert!(validate(&[hook.clone()], |_| true).is_err());
        hook.timeout_secs = Some(0);
        assert!(validate(&[hook], |_| true).is_err());
    }

    #[test]
    fn a_manifest_key_nobody_reads_is_refused_rather_than_ignored() {
        let temp = tempfile::tempdir().unwrap();
        shelf(
            temp.path(),
            "dev",
            "1.0",
            "hooks:\n  - event: Stop\n    script: guard.py\n    timeout: 5\n",
            &[("guard.py", "")],
        );
        assert!(manifest_hooks(&temp.path().join("dev/1.0")).is_err());
    }

    /// Tool events go by the matcher, the way Claude Code reads it; session events ignore it.
    #[test]
    fn hooks_are_selected_by_event_and_on_tool_events_by_matcher() {
        let temp = tempfile::tempdir().unwrap();
        shelf(
            temp.path(),
            "dev",
            "1.0",
            "owns: []\nhooks:\n  - event: Stop\n    script: stop.py\n    args: [\"--stop-hook\"]\n  \
             - event: PreToolUse\n    matcher: \"Edit|Write\"\n    script: edit.py\n  \
             - event: PreToolUse\n    script: any.py\n",
            &[("stop.py", ""), ("edit.py", ""), ("any.py", "")],
        );
        let pinned = pins(&[("dev", "1.0")]);

        let stop = select(temp.path(), &pinned, "Stop", None);
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0].bundle, "dev@1.0");
        assert_eq!(stop[0].hook.args, vec!["--stop-hook".to_string()]);

        let scripts = |tool| {
            select(temp.path(), &pinned, "PreToolUse", Some(tool))
                .into_iter()
                .map(|one| one.hook.script)
                .collect::<Vec<_>>()
        };
        assert_eq!(scripts("Write"), vec!["edit.py", "any.py"]);
        assert_eq!(scripts("WriteFile"), vec!["any.py"]);
        assert!(select(temp.path(), &pinned, "UserPromptSubmit", None).is_empty());
    }

    /// Failing open: a pin the library cannot answer and a bundle whose manifest does not validate
    /// contribute nothing, and do not stop the bundles beside them from answering.
    #[test]
    fn a_missing_or_broken_bundle_contributes_nothing_and_stops_nothing() {
        let temp = tempfile::tempdir().unwrap();
        shelf(
            temp.path(),
            "broken",
            "1.0",
            "hooks:\n  - event: Stop\n    script: not-there.py\n",
            &[],
        );
        shelf(
            temp.path(),
            "good",
            "1.0",
            "hooks:\n  - event: Stop\n    script: stop.py\n",
            &[("stop.py", "")],
        );
        let pinned = pins(&[("absent", "9.9"), ("broken", "1.0"), ("good", "1.0")]);

        let stop = select(temp.path(), &pinned, "Stop", None);
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0].bundle, "good@1.0");
    }

    /// The script runs from the library with the payload on stdin and in the session's directory;
    /// the first exit 2 is a block, and nothing after it runs.
    #[tokio::test]
    async fn hooks_run_from_the_library_and_the_first_block_ends_the_run() {
        if !python_available() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("lib");
        shelf(
            &library,
            "dev",
            "1.0",
            "hooks:\n  - event: Stop\n    script: echo.py\n    args: [\"tag\"]\n  \
             - event: Stop\n    script: block.py\n  \
             - event: Stop\n    script: never.py\n",
            &[
                (
                    "echo.py",
                    "import json, os, sys\n\
                     payload = json.load(sys.stdin)\n\
                     print(sys.argv[1], payload['hook_event_name'], os.path.basename(os.getcwd()))\n",
                ),
                (
                    "block.py",
                    "import sys\nsys.stderr.write('work is open')\nsys.exit(2)\n",
                ),
                ("never.py", "open('ran', 'w').write('x')\n"),
            ],
        );
        let session = temp.path().join("session");
        std::fs::create_dir_all(&session).unwrap();

        let selected = select(&library, &pins(&[("dev", "1.0")]), "Stop", None);
        assert_eq!(selected.len(), 3);
        let outcomes = run_all(
            crate::governance::autopilot::HOOK_INTERPRETER,
            &selected,
            br#"{"hook_event_name":"Stop"}"#,
            &session,
        )
        .await;

        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        assert_eq!(outcomes[0].exit_code, Some(0));
        assert_eq!(outcomes[0].stdout.trim(), "tag Stop session");
        assert_eq!(outcomes[1].exit_code, Some(2));
        assert_eq!(outcomes[1].stderr, "work is open");
        assert!(!session.join("ran").exists());
    }

    /// A hook that does not finish is stopped at its timeout and reported with no exit code,
    /// which the script reads as no opinion rather than a block.
    #[tokio::test]
    async fn a_hook_past_its_timeout_is_stopped_and_reports_no_exit_code() {
        if !python_available() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        shelf(
            temp.path(),
            "dev",
            "1.0",
            "hooks:\n  - event: Stop\n    script: slow.py\n",
            &[("slow.py", "import time\ntime.sleep(30)\n")],
        );
        let selected = select(temp.path(), &pins(&[("dev", "1.0")]), "Stop", None);

        let outcome = run_one(
            crate::governance::autopilot::HOOK_INTERPRETER,
            &selected[0],
            b"{}",
            temp.path(),
            Duration::from_millis(500),
        )
        .await;

        assert_eq!(outcome.exit_code, None);
        assert!(outcome.stderr.contains("timeout"), "{}", outcome.stderr);
    }

    /// The route's whole path: a session standing in a rostered checkout gets the hooks its
    /// project pins; one standing anywhere else gets nothing.
    #[tokio::test]
    async fn a_session_in_a_rostered_project_runs_what_that_project_pins() {
        if !python_available() {
            return;
        }
        let pool = crate::testdb::fresh_pool().await;
        let repo = crate::execution::git_exec::testkit::space_free_tempdir("nucleos-bundle-hooks-");
        crate::execution::git_exec::testkit::initialize_repo(repo.path());
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root) VALUES (?, 'shadow', ?)",
        )
        .bind("p")
        .bind(repo.path().to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .unwrap();

        let home = tempfile::tempdir().unwrap();
        let library = home.path().join("workflows");
        shelf(
            &library,
            "dev",
            "1.0",
            "hooks:\n  - event: UserPromptSubmit\n    script: context.py\n",
            &[("context.py", "print('remember the spec')\n")],
        );
        let pins_file =
            crate::project::state::file(Some(home.path()), "p", crate::project::state::PINS_FILE)
                .unwrap();
        std::fs::create_dir_all(pins_file.parent().unwrap()).unwrap();
        std::fs::write(&pins_file, workflows::render_pins(&pins(&[("dev", "1.0")]))).unwrap();

        let payload = |cwd: &Path| {
            serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "cwd": cwd.to_string_lossy(),
                "prompt": "go",
            })
        };
        let outcomes = for_payload(
            &pool,
            &library,
            home.path(),
            crate::governance::autopilot::HOOK_INTERPRETER,
            &payload(repo.path()),
        )
        .await;
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert_eq!(outcomes[0].stdout.trim(), "remember the spec");

        let elsewhere = tempfile::tempdir().unwrap();
        assert!(
            for_payload(
                &pool,
                &library,
                home.path(),
                crate::governance::autopilot::HOOK_INTERPRETER,
                &payload(elsewhere.path()),
            )
            .await
            .is_empty()
        );
    }
}
