use async_trait::async_trait;
use base64::Engine;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
// The `TreeKiller` this module used to define lives in `process_tree.rs` now, together with the
// spawn contract that makes it correct off Windows. This copy was the one whose `Drop` called
// `taskkill` with no `cfg` at all — a silent no-op on every other platform.
use crate::process_tree::TreeKiller;

mod auth_watch;
mod claude;
mod claude_args;
mod codex;
mod command;
#[cfg(any(test, feature = "testkit"))]
mod fake;
mod live;
mod local_triage;
mod ollama;
mod request;
mod stream;
mod turns;
mod wire;

pub use self::auth_watch::watch_claude_auth;
pub use self::claude::*;
pub use self::claude_args::*;
pub use self::codex::*;
pub use self::command::*;
#[cfg(any(test, feature = "testkit"))]
pub use self::fake::*;
pub use self::live::*;
pub use self::local_triage::*;
pub use self::ollama::*;
pub use self::request::*;
pub use self::stream::*;
pub use self::turns::*;
pub use self::wire::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// A transcript sink for the tests that do not read one. Named rather than inlined so that a
    /// test which DOES care about the transcript is visibly different at the call site.
    fn discard_transcript() -> std::sync::Arc<std::sync::Mutex<String>> {
        std::sync::Arc::new(std::sync::Mutex::new(String::new()))
    }

    /// A notebook write says which notebook, like every other write says which file.
    ///
    /// `detail_of` is what `ask_about` puts beside a tool name, so while `notebook_path` was off
    /// the list a `manual` rung asking about a `NotebookEdit` showed the tool and nothing else
    /// — a question about a write with the write left out, which is not a question anybody
    /// can answer. The second case pins the ORDER rather than restating the first: `description`
    /// is deliberately last, and a path must beat the sentence a model wrote about it.
    #[test]
    fn a_notebook_write_says_which_notebook() {
        assert_eq!(
            detail_of(&serde_json::json!({"notebook_path": "C:/repo/notes.ipynb"})),
            Some("C:/repo/notes.ipynb".to_owned())
        );
        assert_eq!(
            detail_of(&serde_json::json!({
                "description": "tidy the notebook up a bit",
                "notebook_path": "notes.ipynb",
            })),
            Some("notes.ipynb".to_owned())
        );
    }

    /// An `AskUserQuestion` call carries its question under `questions[0].question`; that is the
    /// detail worth showing beside the tool name, not nothing.
    #[test]
    fn ask_user_question_detail_is_the_question() {
        assert_eq!(
            detail_of(&serde_json::json!({
                "questions": [{"question": "  Which database?  ", "options": []}]
            })),
            Some("Which database?".to_owned())
        );
        assert_eq!(
            detail_of(&serde_json::json!({"questions": [{"question": "   "}]})),
            None
        );
    }

    #[test]
    fn cli_args_always_assigns_a_session_id() {
        let request = baseline_run_request();

        let args = cli_args(&request, "sonnet");

        assert!(args.windows(2).any(|pair| {
            pair[0] == "--session-id" && pair[1] == "123e4567-e89b-42d3-a456-426614174000"
        }));
    }

    #[test]
    fn cli_args_names_every_fallback_in_the_order_given() {
        let mut request = baseline_run_request();
        request.fallback_model = vec!["opus".to_string(), "sonnet".to_string()];

        let args = cli_args(&request, "fable");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--fallback-model" && pair[1] == "opus,sonnet"),
            "the fallbacks never reached the argument vector: {args:?}"
        );
    }

    #[test]
    fn cli_args_carries_every_extra_directory() {
        let mut request = baseline_run_request();
        request.add_dirs = vec![PathBuf::from("/one"), PathBuf::from("/two")];

        let args = cli_args(&request, "sonnet");

        let at = args
            .iter()
            .position(|a| a == "--add-dir")
            .expect("no --add-dir");
        assert_eq!(&args[at + 1..at + 3], ["/one", "/two"]);
    }

    /// `--add-dir` is variadic: it swallows every following argument until the next flag. On the
    /// argv path the prompt is a POSITIONAL, so a variadic flag written above it would be handed
    /// the person's message as a directory — the run would ask for tool access to their sentence
    /// and never say what it was answering.
    #[test]
    fn the_variadic_directory_flag_never_swallows_the_prompt() {
        let mut request = baseline_run_request();
        request.add_dirs = vec![PathBuf::from("/one")];

        let args = cli_args(&request, "sonnet");

        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "test prompt", "the prompt moved: {args:?}");
        let at = args.iter().position(|a| a == "--add-dir").unwrap();
        assert!(at > 1, "--add-dir was written above the prompt: {args:?}");
    }

    #[test]
    fn cli_args_carries_the_ceiling_a_turn_may_spend() {
        let mut request = baseline_run_request();
        request.max_budget_usd = Some(0.5);

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--max-budget-usd" && pair[1] == "0.5"),
            "{args:?}"
        );
    }

    /// One helper, for the tests below to vary.
    fn a_helper(name: &str) -> Subagent {
        Subagent {
            name: name.to_string(),
            description: "Reviews code".to_string(),
            prompt: "You are a code reviewer".to_string(),
            tools: None,
            model: None,
            effort: None,
        }
    }

    /// The flag takes ONE argv element holding a JSON object keyed by name — not a repeatable
    /// `--agents name=…`, and not the list this daemon holds internally.
    #[test]
    fn the_helper_set_travels_as_one_object_keyed_by_name() {
        let mut request = baseline_run_request();
        request.agents = vec![a_helper("reviewer")];

        let args = cli_args(&request, "sonnet");

        let at = args
            .iter()
            .position(|arg| arg == "--agents")
            .expect("no --agents");
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).expect("not JSON");
        assert_eq!(sent["reviewer"]["description"], "Reviews code");
        assert_eq!(sent["reviewer"]["prompt"], "You are a code reviewer");
        // The name is the KEY. Sent inside the object as well it would be an unknown field, which
        // the CLI's schema strips today — a contract that holds only because the other side is
        // forgiving, and this asserts we do not rely on that.
        assert!(
            sent["reviewer"].get("name").is_none(),
            "the name was sent twice: {}",
            args[at + 1]
        );
    }

    /// Absent keys, not null ones. `{"model": null}` is not what "inherit the conversation's model"
    /// looks like to a schema that types `model` as a string.
    #[test]
    fn a_helper_that_named_no_model_sends_no_model_key() {
        let mut request = baseline_run_request();
        request.agents = vec![a_helper("reviewer")];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert!(sent["reviewer"].get("model").is_none(), "{}", args[at + 1]);
        assert!(sent["reviewer"].get("effort").is_none(), "{}", args[at + 1]);
    }

    /// Absent, not `null` and not `[]`: a helper that named no restriction inherits the parent's
    /// whole tool surface, today's behaviour, and the object sent to the CLI says nothing at all
    /// rather than saying "no restriction" in a way that could later be confused with "no tools".
    #[test]
    fn a_helper_that_named_no_tools_sends_no_tools_key() {
        let mut request = baseline_run_request();
        request.agents = vec![a_helper("reviewer")];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert!(sent["reviewer"].get("tools").is_none(), "{}", args[at + 1]);
    }

    /// A helper that named a restriction sends exactly that list, so the CLI grants it those tools
    /// and nothing else.
    #[test]
    fn a_helper_may_be_restricted_to_named_tools() {
        let mut request = baseline_run_request();
        let mut helper = a_helper("reviewer");
        helper.tools = Some(vec!["Read".to_string(), "Grep".to_string()]);
        request.agents = vec![helper];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert_eq!(
            sent["reviewer"]["tools"],
            serde_json::json!(["Read", "Grep"])
        );
    }

    /// A helper stored before this field existed — its JSON object has no `tools` key at all —
    /// deserialises identically to one that named no restriction, and runs the same way: inheriting
    /// the parent's whole surface, exactly as it did before this field was added.
    #[test]
    fn a_helper_stored_before_tools_existed_still_deserialises() {
        let stored = r#"{"description":"Reviews code","prompt":"You are a code reviewer"}"#;
        let agent: Subagent = serde_json::from_str(stored).unwrap();

        assert_eq!(agent.tools, None);
        assert_eq!(agent.description, "Reviews code");
        assert_eq!(agent.prompt, "You are a code reviewer");
    }

    #[test]
    fn a_helper_may_answer_on_its_own_model_and_effort() {
        let mut request = baseline_run_request();
        let mut helper = a_helper("reviewer");
        helper.model = Some("opus".to_string());
        helper.effort = Some("high".to_string());
        request.agents = vec![helper];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert_eq!(sent["reviewer"]["model"], "opus");
        assert_eq!(sent["reviewer"]["effort"], "high");
        // And the conversation's own model is untouched: a helper's model is not the turn's.
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "sonnet"),
            "{args:?}"
        );
    }

    /// Not `--agents {}`. The CLI answers unparseable JSON with an EMPTY agent list and no error,
    /// so from outside there is nothing to tell "a flag that parsed to nothing" from "a flag that
    /// threw" — and only one of those is a state somebody chose. Sending no flag keeps the two
    /// apart.
    #[test]
    fn a_conversation_with_no_helpers_writes_no_flag_at_all() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        assert!(
            !args.iter().any(|arg| arg == "--agents"),
            "an empty helper set was sent: {args:?}"
        );
    }

    #[test]
    fn cli_args_appends_standing_instructions_rather_than_replacing_the_system_prompt() {
        let mut request = baseline_run_request();
        request.append_system_prompt = Some("Answer in Portuguese.".to_string());

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--append-system-prompt"
                    && pair[1] == "Answer in Portuguese."),
            "{args:?}"
        );
        // The REPLACING flag must never appear. It drops the CLI's tool descriptions and safety
        // framing, and a run that lost those reads as a run whose model got worse.
        assert!(
            !args.iter().any(|arg| arg == "--system-prompt"),
            "the system prompt was replaced instead of appended: {args:?}"
        );
    }

    #[test]
    fn cli_args_names_the_session_so_it_is_findable_outside_this_app() {
        let mut request = baseline_run_request();
        request.session_name = Some("o refactor do runner".to_string());

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--name" && pair[1] == "o refactor do runner"),
            "{args:?}"
        );
    }

    /// The compaction event, as a real headless stream emits it.
    ///
    /// Both lines below were copied out of `claude -p --resume` run with the window forced low, not
    /// written from the source: the point of the test is that this daemon reads what the CLI
    /// actually sends. The `compact_result` line is deliberately NOT what is read — a compaction
    /// that began is the honest answer to "was the context summarised here" whether or not it
    /// finished, and a mark that appeared and then had to be taken back would be worse than one that
    /// says where the summarising happened.
    #[test]
    fn a_compaction_is_read_off_the_stream_and_stays_read() {
        let started =
            r#"{"type":"system","subtype":"status","status":"compacting","session_id":"s"}"#;
        let finished = r#"{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"too_few_groups","session_id":"s"}"#;

        assert!(
            compacted_from_line(started, false),
            "the status line says it"
        );
        assert!(
            !compacted_from_line(finished, false),
            "the result line alone is not the event"
        );
        assert!(
            compacted_from_line(finished, true),
            "a compaction already seen is not un-seen by the lines after it"
        );
        assert!(
            !compacted_from_line(r#"{"type":"assistant"}"#, false),
            "an ordinary line says nothing about compaction"
        );
        assert!(
            !compacted_from_line("not json at all", false),
            "an unparseable line is not evidence of anything"
        );
    }

    /// A compaction belongs to the turn it happened in, and to no other turn of the same process.
    ///
    /// The splitter is where this has to hold: on the multi-turn path one process answers several
    /// times, and a flag left standing would mark every later turn as the one where the
    /// conversation got shorter.
    #[test]
    fn a_compaction_marks_one_turn_and_not_the_ones_after_it() {
        let mut splitter = TurnSplitter::new();
        let result =
            r#"{"type":"result","subtype":"success","total_cost_usd":0.1,"session_id":"s"}"#;

        splitter.line(
            r#"{"type":"system","subtype":"status","status":"compacting","session_id":"s"}"#.into(),
        );
        let first = splitter.line(result.into());
        let TurnEvent::Ended(first) = &first[1] else {
            panic!("the result line ends a turn: {first:?}");
        };
        assert!(first.compacted, "the turn it happened in carries it");

        let second = splitter.line(result.into());
        let TurnEvent::Ended(second) = &second[1] else {
            panic!("the result line ends a turn: {second:?}");
        };
        assert!(
            !second.compacted,
            "the next turn did not compact, and must not inherit that it did"
        );
    }

    /// The merge that must not be two flags.
    ///
    /// `--disallowedTools` is variadic, so a second occurrence REPLACES the first rather than
    /// adding to it. Written twice, a conversation asking not to run `Bash` would have taken
    /// `ToolPolicy::McpOnly` down with it and come back holding the whole built-in tool set — a
    /// safety property undone by a preference, silently, and looking like it still held.
    #[test]
    fn a_runs_own_denials_and_its_policys_travel_as_one_flag() {
        let mut request = baseline_run_request();
        request.tool_policy = ToolPolicy::McpOnly;
        request.denied_tools = vec!["mcp__other__write".to_string()];

        let args = cli_args(&request, "sonnet");

        let flags = args
            .iter()
            .filter(|arg| arg.as_str() == "--disallowedTools")
            .count();
        assert_eq!(flags, 1, "the deny flag was written twice: {args:?}");
        let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
        let denied: Vec<&str> = args[at + 1].split(',').collect();
        assert!(denied.contains(&"Bash"), "the policy's denials were lost");
        assert!(
            denied.contains(&"mcp__other__write"),
            "the run's own denial was lost"
        );
    }

    /// A conversation may narrow itself even where the policy denies nothing.
    #[test]
    fn an_unrestricted_run_still_honours_the_tools_it_was_told_not_to_use() {
        let mut request = baseline_run_request();
        request.tool_policy = ToolPolicy::Unrestricted;
        request.denied_tools = vec!["Bash".to_string(), "Edit".to_string()];

        let args = cli_args(&request, "sonnet");

        let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(args[at + 1], "Bash,Edit");
    }

    /// And the wildcard stands alone. Naming individual tools beside `*` would add stderr lines
    /// about rules matching nothing, on top of a denial that already covers everything.
    #[test]
    fn the_deny_everything_policy_is_not_diluted_by_a_conversations_own_list() {
        assert_eq!(
            denied_tools(&ToolPolicy::None, &["Bash".to_string()]),
            vec!["*".to_string()]
        );
    }

    #[test]
    fn cli_args_is_silent_about_instructions_and_denials_when_none_were_chosen() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        for flag in ["--append-system-prompt", "--name", "--disallowedTools"] {
            assert!(
                !args.iter().any(|arg| arg == flag),
                "{flag} was sent: {args:?}"
            );
        }
    }

    /// And says nothing when nobody asked, in all three. A flag always present would replace the
    /// CLI's own behaviour with this daemon's guess at it for every run that never expressed one.
    #[test]
    fn cli_args_is_silent_about_reach_and_ceiling_when_none_were_chosen() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        for flag in ["--fallback-model", "--add-dir", "--max-budget-usd"] {
            assert!(
                !args.iter().any(|arg| arg == flag),
                "{flag} was sent: {args:?}"
            );
        }
    }

    /// The flag exists on the CLI (2.1.198, `low | medium | high | xhigh | max`) and the daemon had
    /// no way to send it. This is the half that builds it.
    #[test]
    fn cli_args_carries_the_effort_when_one_was_chosen() {
        let mut request = baseline_run_request();
        request.effort = Some("xhigh".to_string());

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--effort" && pair[1] == "xhigh"),
            "the chosen effort never reached the argument vector: {args:?}"
        );
    }

    /// And says nothing when nobody chose. An always-present flag would replace the CLI's own
    /// default with this daemon's guess at it, for every run that never asked.
    #[test]
    fn cli_args_is_silent_about_effort_when_none_was_chosen() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        assert!(
            !args.iter().any(|arg| arg == "--effort"),
            "an effort was sent for a run that chose none: {args:?}"
        );
    }

    /// `--effort` must not displace `--exclude-dynamic-system-prompt-sections`, whose own comment
    /// reserves the position immediately after `--verbose` for prompt-cache prefix matching. A flag
    /// added carelessly is exactly how that invariant dies quietly.
    #[test]
    fn the_effort_flag_does_not_disturb_the_cache_prefix() {
        let mut request = baseline_run_request();
        request.effort = Some("max".to_string());

        let args = cli_args(&request, "sonnet");

        let verbose = args.iter().position(|a| a == "--verbose").unwrap();
        assert_eq!(
            args[verbose + 1],
            "--exclude-dynamic-system-prompt-sections",
            "something was inserted between --verbose and the cache flag: {args:?}"
        );
    }

    #[test]
    fn cli_args_forks_the_session_only_when_asked() {
        let mut forked = baseline_run_request();
        forked.fork_session = true;
        let forked_args = cli_args(&forked, "sonnet");

        let not_forked = baseline_run_request();
        let not_forked_args = cli_args(&not_forked, "sonnet");

        assert!(forked_args.iter().any(|arg| arg == "--fork-session"));
        assert!(!not_forked_args.iter().any(|arg| arg == "--fork-session"));
    }

    /// Every rung writes exactly one flag, and the SPELLINGS are the assertion.
    ///
    /// They were measured off the installed CLI (2.1.260), whose choices are `acceptEdits`, `auto`,
    /// `bypassPermissions`, `manual`, `dontAsk` and `plan`. Two of those are traps this test exists
    /// to keep shut: `default` is what the documentation calls the unelevated rung and the command
    /// line does not accept it, so writing it — the obvious thing to do, reading the variant's name
    /// — would fail the start of every run this daemon launches; and `auto` is the CLI's OWN
    /// classifier model, a second billed opinion nobody reconciled with the classifier the daemon
    /// runs.
    ///
    /// "No flag at all" is asserted absent for the same reason it stopped being written: from
    /// v2.1.228 the default start-up mode on Pro/Max/Team plans is `auto`, so silence buys the very
    /// thing the line above refuses.
    #[test]
    fn cli_args_writes_one_measured_permission_spelling_per_rung() {
        for (permission, expected) in [
            (Permission::Default, "manual"),
            (Permission::AcceptEdits, "acceptEdits"),
            (Permission::Plan, "plan"),
            (Permission::Bypass, "bypassPermissions"),
        ] {
            let mut request = baseline_run_request();
            request.permission = permission;
            let args = cli_args(&request, "sonnet");

            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--permission-mode".to_string(), expected.to_string()]),
                "{permission:?} must launch as {expected}: {args:?}"
            );
            assert_eq!(
                args.iter()
                    .filter(|arg| *arg == "--permission-mode")
                    .count(),
                1,
                "two permission modes on one command line is the CLI's choice, not ours: {args:?}"
            );
            assert!(
                !args.iter().any(|arg| arg == "default" || arg == "auto"),
                "{permission:?} wrote a spelling the CLI does not accept, or its own classifier: {args:?}"
            );
        }
    }

    /// The safety property, now unrepresentable rather than ordered.
    ///
    /// A run that must not act — a catch-up run, recovering a schedule the machine slept through,
    /// forced into planning precisely because nobody chose for it to run NOW — used to keep that
    /// restraint by winning an `else if` against `classifier_governs_tools`. The test that pinned
    /// that order said in its own words that "order is not where a safety property belongs", and it
    /// was right: there is one field now, it holds one value, and a request carrying the belief that
    /// the classifier governs cannot ALSO be carrying the flag that stands the barrier down.
    ///
    /// The belief itself survives, and this asserts that too: `classifier_governs_tools` still means
    /// something to `codex_cli_args`' refusal and to whoever reads the row — what it lost is the
    /// power to write this flag.
    #[test]
    fn a_restrained_run_cannot_also_carry_the_standing_down_flag() {
        let mut restrained = baseline_run_request();
        restrained.permission = Permission::Plan;
        restrained.classifier_governs_tools = true;
        let args = cli_args(&restrained, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--permission-mode".to_string(), "plan".to_string()]),
            "plan must survive: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "bypassPermissions"),
            "a restrained run must never also be handed the standing-down flag: {args:?}"
        );
    }

    #[test]
    fn cli_args_streams_partial_messages_only_when_asked() {
        let mut streaming = baseline_run_request();
        streaming.include_partial_messages = true;
        let streaming_args = cli_args(&streaming, "sonnet");

        let not_streaming = baseline_run_request();
        let not_streaming_args = cli_args(&not_streaming, "sonnet");

        assert!(
            streaming_args
                .iter()
                .any(|arg| arg == "--include-partial-messages")
        );
        assert!(
            !not_streaming_args
                .iter()
                .any(|arg| arg == "--include-partial-messages")
        );
    }

    /// The opt-in path. `--input-format stream-json` is what turns the CLI's stdin into a channel a
    /// later turn can arrive on, and the initial prompt has to travel that same channel: measured
    /// against CLI 2.1.198, `-p <prompt> --input-format stream-json` reads the positional AND waits
    /// on stdin, so leaving the prompt in argv would enqueue the same instruction twice.
    // The final assertion is `!args.iter().any(...)`, which clippy would rather see as
    // `!args.contains(...)`. Allowed rather than rewritten: this test is frozen, and an assertion is
    // evidence — rephrasing one to satisfy a style lint edits the record of what was checked, even
    // when the two forms agree.
    #[allow(clippy::manual_contains)]
    #[test]
    fn a_steerable_run_writes_its_prompt_as_a_stream_json_user_line() {
        let mut request = baseline_run_request();
        request.steerable = true;

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--input-format" && pair[1] == "stream-json"),
            "a steerable run reads its turns from stdin: {args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|pair| pair[0] == "-p" && pair[1] == request.prompt),
            "the prompt belongs on stdin as a user line, not after -p: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| *arg == request.prompt),
            "a steered run's prompt must not reach argv at all: {args:?}"
        );
    }

    /// The regression guard for every path that did not ask to be steerable — the email pillar's
    /// triage runs among them. Whatever the opt-in adds, a run without it keeps today's argument
    /// vector: `-p <prompt>`, no `--input-format`, and therefore a stdin nothing can write to.
    #[test]
    fn a_non_steerable_run_keeps_the_argv_prompt() {
        let request = baseline_run_request();
        assert!(!request.steerable, "the baseline must not opt in");

        let args = cli_args(&request, "sonnet");

        assert_eq!(args.first().map(String::as_str), Some("-p"));
        assert_eq!(
            args.get(1).map(String::as_str),
            Some(request.prompt.as_str()),
            "the prompt must stay the argv positional it is today: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "--input-format"),
            "a run that did not opt in must keep its stdin closed: {args:?}"
        );
    }

    /// The other half of the opt-in: the argv assertions above prove the prompt LEFT argv, and this
    /// proves what it became. The shape was measured against CLI 2.1.198 by probe; a probe is a
    /// finding until something holds it in place.
    ///
    /// Asserted on a text carrying a newline and a quote because a turn is delimited by a newline:
    /// building this line by hand would split such a prompt into two half-parsed instructions, and
    /// the CLI would act on the first one alone.
    #[test]
    fn a_steering_turn_is_one_json_user_line_whatever_its_text_contains() {
        let text = "stop after this file\nand say \"done\"";

        let line = user_message_line(text, &[]);

        assert!(line.ends_with('\n'), "a turn is terminated: {line:?}");
        let body = line.strip_suffix('\n').unwrap();
        assert_eq!(
            body.lines().count(),
            1,
            "one turn must be one line: {line:?}"
        );
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["type"], "user");
        assert_eq!(parsed["message"]["role"], "user");
        assert_eq!(
            parsed["message"]["content"], text,
            "the text must survive framing intact: {line:?}"
        );
    }

    #[tokio::test]
    async fn a_stream_with_no_init_event_still_reports_the_assigned_session_id() {
        let assigned_session_id = "123e4567-e89b-42d3-a456-426614174000";
        let stdout =
            r#"{"type":"result","subtype":"success","result":"done","total_cost_usd":0.08}"#;
        let runner = FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
                session_id: None,
                cost_usd: Some(0.08),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
                model_requested: false,
            })),
            ..Default::default()
        };
        let request = baseline_run_request();
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = runner
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .unwrap();

        assert_eq!(
            outcome.session_id.as_deref(),
            Some(assigned_session_id),
            "the assigned session id must survive a stream with no init event"
        );
    }

    /// The wildcard is what every caller but a department gets, and a department gets exactly its
    /// own list — prefixed here, so callers name tools rather than CLI syntax.
    #[test]
    fn a_narrowed_request_advertises_only_the_tools_it_names() {
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("mcp.json"));

        let wide = cli_args(&request, "claude-sonnet-5");
        let flag = wide.windows(2).find(|w| w[0] == "--allowedTools").unwrap();
        assert_eq!(flag[1], "mcp__nucleos__*");

        request.allowed_mcp_tools =
            Some(vec!["list_files".to_owned(), "read_team_file".to_owned()]);
        let narrow = cli_args(&request, "claude-sonnet-5");
        let flag = narrow
            .windows(2)
            .find(|w| w[0] == "--allowedTools")
            .unwrap();
        assert_eq!(
            flag[1],
            "mcp__nucleos__list_files,mcp__nucleos__read_team_file"
        );
        assert!(
            !narrow.iter().any(|arg| arg == "mcp__nucleos__*"),
            "the wildcard must be replaced, not accompanied — one of the two would win and it \
             would not be obvious which"
        );
    }

    /// Narrowing without an MCP config narrows nothing, which is the harmless direction and worth
    /// pinning: the flag is only ever written inside the `mcp_config` branch.
    #[test]
    fn narrowing_a_request_with_no_mcp_server_adds_no_flag() {
        let mut request = baseline_run_request();
        request.allowed_mcp_tools = Some(vec!["list_files".to_owned()]);
        assert!(
            !cli_args(&request, "claude-sonnet-5")
                .iter()
                .any(|a| a == "--allowedTools")
        );
    }

    /// A run offered no server is charged nothing for schemas, and that zero is an answer rather
    /// than a gap.
    ///
    /// This daemon writes `--mcp-config` or it does not; when it does not, the model is offered no
    /// tools by us and there is no schema block in its prompt to pay for.
    #[test]
    fn a_request_offered_no_server_is_charged_nothing_for_schemas() {
        let mut request = baseline_run_request();
        // `McpOnly` and not the baseline's `Unrestricted`, so that the zero below is attributable
        // to the ABSENT SERVER. An unrestricted run defers its schemas and reads 0 whatever its
        // config says, which would make the assertion pass without touching what it is about.
        request.tool_policy = ToolPolicy::McpOnly;
        assert!(request.mcp_config.is_none());
        assert_eq!(authored_prompt(&request).schema_chars, 0);
    }

    /// The same server, announced twice, charged once — because only one of the two runs was sent
    /// the schemas.
    ///
    /// **The assertion is the SPLIT, not either half.** One `RunRequest` with one `mcp_config` is
    /// read under both policies, so nothing but the regime differs between the two readings; a test
    /// that only pinned the zero would pass against code that charged nobody, and one that only
    /// pinned the price would pass against the old code that charged everybody.
    ///
    /// Measured on 2026-09-07 against CLI 2.1.263, on one prompt with one flag moved per arm. The
    /// same 48 nucleos tools cost **850** input tokens when the CLI kept `ToolSearch` and advertised
    /// them by name, and **~12,225** when every built-in was denied and it had to ship the schemas.
    /// That is the whole of why the daemon may not charge both alike: on the deferred run
    /// `advertised_schema_chars / 4` claims ~10,250 tokens for a block the model never read, and
    /// `runs::with_prompt_budget` subtracts it, so the CLI's own share is understated by as much.
    #[test]
    fn a_deferred_schema_is_not_charged_to_the_prompt_that_never_held_it() {
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("mcp.json"));

        // The ordinary chat turn: `assistant::tool_policy_for` returns this for a turn with a cwd,
        // so the deferring regime is the common one and not the exotic one.
        request.tool_policy = ToolPolicy::Unrestricted;
        assert_eq!(
            authored_prompt(&request).schema_chars,
            0,
            "the CLI keeps `ToolSearch` here, advertises the tools by name and fetches a schema only \
             when asked — so the schema block is not in this prompt and may not be billed to it"
        );

        // One flag different. Same request, same server, same announcement.
        request.tool_policy = ToolPolicy::McpOnly;
        assert_eq!(
            authored_prompt(&request).schema_chars,
            crate::mcp_tools::NucleosTools::advertised_schema_chars(None),
            "denying the built-ins denies `ToolSearch` with them, the CLI cannot defer, and the run \
             really does read every schema its server announces"
        );
    }

    /// Taking `ToolSearch` away BY NAME puts an unrestricted run back in the shipped regime, and it
    /// must be charged like one.
    ///
    /// This is the test that makes the helper's contract "ask `denied_tools`" rather than "match the
    /// policy": the policy here is `Unrestricted`, so a variant match would call this deferred and
    /// charge it nothing, while the flag `cli_args` writes says otherwise and the CLI obeys the flag.
    ///
    /// It is also where the count hypothesis dies. The regime could in principle have been chosen by
    /// how many tools were on offer rather than by which ones — so a further arm on 2026-09-07 denied
    /// every built-in EXCEPT `ToolSearch`, against the same 48-tool server: **49 tools, 5,578 input
    /// tokens, deferred**, where **48 tools with no `ToolSearch` cost 15,837 and shipped**. No count
    /// threshold makes 49 defer while 48 ships. `ToolSearch` is the variable.
    #[test]
    fn denying_tool_search_by_name_is_charged_as_a_shipped_run() {
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("mcp.json"));
        assert!(
            matches!(request.tool_policy, ToolPolicy::Unrestricted),
            "the point of this test is a policy that would otherwise defer"
        );
        assert_eq!(
            authored_prompt(&request).schema_chars,
            0,
            "untouched, this request defers"
        );

        request.denied_tools = vec!["ToolSearch".to_string()];
        assert_eq!(
            authored_prompt(&request).schema_chars,
            crate::mcp_tools::NucleosTools::advertised_schema_chars(None),
            "one name on `--disallowedTools` and the CLI has no way to fetch a schema on demand, so \
             it ships them all — the price must follow the flag, not the policy the flag sits under"
        );
    }

    fn baseline_run_request() -> RunRequest {
        RunRequest {
            prompt: "test prompt".to_string(),
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
        }
    }

    /// Two turns down one REAL CLI, through the argument vector this daemon actually builds.
    ///
    /// `#[ignore]` because it needs the Claude Code CLI installed, an authenticated session, and
    /// about twenty cents of somebody's money. Everything else about a conversation keeping its
    /// process is tested against `FakeCommandRunner`, which proves the wiring and proves nothing
    /// about whether the CLI serves a second turn down a stdin this code built.
    ///
    /// That exact gap has already bitten once, this month: the fake did not implement
    /// `run_prompt_with_turns` at all, so every rooted chat turn in the suite took a path that could
    /// not work — and every assertion still passed, because they all look at what the runner was
    /// HANDED rather than at what came back.
    ///
    /// What it pins: one process answering twice, one session across both, and the two turns' costs
    /// adding up to the process's own. That last one is the whole of `TurnSplitter`'s reason to
    /// exist — the CLI prints a RUNNING TOTAL on every `result`, so a second turn recorded verbatim
    /// bills the first one again, and a third bills the first two.
    #[tokio::test]
    #[ignore = "spawns the real Claude CLI and spends money; run with --include-ignored"]
    async fn a_real_cli_answers_a_second_turn_down_the_same_stdin() {
        let (messages, incoming) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        let (turn_events, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let mut request = test_run_request("Reply with the single word one.");
        request.steerable = true;
        request.messages = Some(incoming);
        request.model = Some("sonnet".to_owned());

        let runner = ClaudeCliRunner {
            model: "sonnet".to_owned(),
            plan_model: None,
            review_model: None,
            resolve_model: None,
            resolve_effort: None,
        };
        let process = tokio::spawn(async move {
            runner
                .run_prompt_with_turns(
                    request,
                    session_tx,
                    std::sync::Arc::new(std::sync::Mutex::new(String::new())),
                    std::sync::Arc::new(std::sync::Mutex::new(None)),
                    Some(turn_events),
                )
                .await
        });

        let mut ended = Vec::new();
        while let Some(event) = events.recv().await {
            let TurnEvent::Ended(turn) = event else {
                continue;
            };
            ended.push(turn);
            if ended.len() == 1 {
                // The second turn, written while the process that answered the first is still
                // standing. This is the line the whole feature is about.
                messages
                    .send(LaterTurn {
                        text: "Reply with the single word two.".to_owned(),
                        images: Vec::new(),
                        interrupt: false,
                        stop_task: None,
                    })
                    .unwrap();
            } else {
                break;
            }
        }
        // Closing stdin is how a steerable run is told no more turns are coming.
        drop(messages);
        let outcome = process.await.unwrap().expect("the CLI should have run");

        assert_eq!(ended.len(), 2, "one process must have answered twice");

        let summed: f64 = ended.iter().filter_map(|turn| turn.cost_usd).sum();
        let whole = outcome
            .cost_usd
            .expect("a finished process reports what it spent");
        assert!(
            (summed - whole).abs() < 1e-6,
            "the turns must add up to the process: {summed} against {whole}"
        );

        // One session across both turns, which is what lets the conversation be resumed by it after
        // the process is gone — so losing the process costs speed and never continuity.
        let announced = session_rx
            .recv()
            .await
            .expect("the CLI announces its session");
        assert!(!announced.is_empty());
        assert!(
            session_rx.try_recv().is_err(),
            "a second session was started"
        );
    }

    /// Points `NUCLEOS_CLAUDE_BIN` at a program for as long as it lives, and puts the previous value
    /// back on drop.
    ///
    /// Under `worktree::test_env_lock`, which every test that writes the process environment takes:
    /// the variable is process-wide, and a second test pointing it elsewhere mid-run would hand this
    /// one a CLI it did not write.
    struct FakeClaudeBin {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl FakeClaudeBin {
        fn set(program: &str) -> Self {
            let lock = crate::execution::worktree::test_env_lock();
            let previous = std::env::var_os("NUCLEOS_CLAUDE_BIN");
            unsafe { std::env::set_var("NUCLEOS_CLAUDE_BIN", program) };
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for FakeClaudeBin {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("NUCLEOS_CLAUDE_BIN", value),
                    None => std::env::remove_var("NUCLEOS_CLAUDE_BIN"),
                }
            }
        }
    }

    /// A shell script that prints `lines` verbatim, one per line.
    fn printing(lines: &[&str]) -> String {
        let mut script = String::from("cat <<'STREAM'\n");
        for line in lines {
            script.push_str(line);
            script.push('\n');
        }
        script.push_str("STREAM\n");
        script
    }

    /// A request whose CLI is `script`, spawned through the real runner loop with `sh` as the
    /// program — so the test holds a [`FakeClaudeBin`] set to `"sh"`.
    ///
    /// It works because of where the runner puts things: `-p` first and a non-steerable run's
    /// prompt second, so a prompt that is the script's path makes `sh -p <script> <flags...>` run
    /// it, every flag after arriving as a positional argument it ignores. `-p` is `sh`'s own
    /// privileged-mode switch and harmless here. One script for every platform where a `.bat` would
    /// serve one, and `sh` is the program the gate tests already need.
    fn fake_cli(dir: &std::path::Path, script: &str) -> RunRequest {
        let path = dir.join("fake-claude.sh");
        std::fs::write(&path, script).expect("write the fake CLI");
        // Forward slashes: on Windows `sh` is MSYS, which reads `C:/...` reliably.
        test_run_request(&path.display().to_string().replace('\\', "/"))
    }

    fn claude_runner() -> ClaudeCliRunner {
        ClaudeCliRunner {
            model: "sonnet".to_owned(),
            plan_model: None,
            review_model: None,
            resolve_model: None,
            resolve_effort: None,
        }
    }

    fn test_run_request(prompt: &str) -> RunRequest {
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
        }
    }

    /// The regression this bug was filed over. `usage` and `cost_usd` are set from the last `Ended`
    /// turn and never reset — see the `TurnEvent::Ended` arm inside `run_prompt_with_turns` — so a
    /// process that answers once and then loops into the ceiling on its NEXT turn must still report
    /// what the first one cost. Before this test existed, that property was pinned only by
    /// `a_real_cli_answers_a_second_turn_down_the_same_stdin`, which needs a paid, authenticated CLI
    /// and is `#[ignore]`d for exactly that reason — this is the same claim, proven deterministically
    /// against a real spawned process reading a scripted stream, so it runs in the gate.
    ///
    /// `steerable: false`, matching the run this bug was actually filed against: an ordinary
    /// autopilot/job run, the kind `runs.rs` spawns, not a chat turn, a council seat or a team
    /// member. What still lets a second turn exist without this request opting into steering is
    /// nothing the request controls — the CLI's own stream decides where a `result` line falls —
    /// and this fake reproduces a transcript where one already had, exactly as
    /// `create_run_inner(..., steerable: true)` lets an operator-messaged run on the Runs page do.
    #[tokio::test]
    async fn a_ceiling_death_after_one_full_turn_still_reports_that_turns_cost_and_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"init","session_id":"fake-session","tools":[]}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"turn one"}]}}"#,
                r#"{"type":"result","subtype":"success","total_cost_usd":0.05,"num_turns":1,"session_id":"fake-session","usage":{"input_tokens":11,"output_tokens":22,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"loop one"}]}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"loop two"}]}}"#,
            ]),
        );
        let _bin = FakeClaudeBin::set("sh");
        // One answer for the turn that ends, two more to trip the ceiling mid-way through the turn
        // that never does: an `assistant` event with no `message.id` is an answer of its own to
        // [`TurnCounter`], so "turn one", "loop one" and "loop two" are three, and
        // `over_turn_ceiling` fires once the count reaches 3.
        request.max_turns = Some(3);
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns and runs to the ceiling");

        assert_eq!(
            outcome.exit_code, TURN_CEILING_EXIT_CODE,
            "the fake transcript must trip the ceiling before its second turn ends"
        );
        assert_eq!(
            outcome.cost_usd,
            Some(0.05),
            "the first turn's cost must survive a ceiling death in the turn after it — this is \
             the NULL the owner reported, and a real number exists here to lose"
        );
        assert_eq!(
            outcome.num_turns,
            Some(1),
            "the first turn's own turn count must survive, not read back as unknown"
        );
        assert_eq!(outcome.input_tokens, Some(11));
        assert_eq!(outcome.output_tokens, Some(22));
    }

    /// `system/init` is the earliest point a model request may be in flight, so it is where the
    /// runner says so. The marker means "may have been sent", not "answered".
    #[tokio::test]
    async fn the_init_line_reports_the_model_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"init","session_id":"fake-session","tools":[]}"#,
                r#"{"type":"result","subtype":"success","total_cost_usd":0.01,"num_turns":1,"session_id":"fake-session"}"#,
            ]),
        );
        let _bin = FakeClaudeBin::set("sh");
        let (request_tx, mut request_rx) = tokio::sync::oneshot::channel::<()>();
        request.request_tx = Some(request_tx);
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns and prints its stream");

        assert_eq!(
            request_rx.try_recv(),
            Ok(()),
            "an init line must report that a model request may be in flight"
        );
    }

    /// A stream that never reached `init` sent nothing to a model, so nothing is reported — even
    /// when a `session_id` already arrived on an earlier line, which is why that is not the signal.
    #[tokio::test]
    async fn a_stream_ending_before_init_reports_no_request() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"hook_started","session_id":"fake-session"}"#,
            ]),
        );
        let _bin = FakeClaudeBin::set("sh");
        let (request_tx, mut request_rx) = tokio::sync::oneshot::channel::<()>();
        request.request_tx = Some(request_tx);
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns and prints its stream");

        assert_eq!(
            session_rx.try_recv().as_deref(),
            Ok("fake-session"),
            "the session is announced before init, and must not count as a request"
        );
        assert_eq!(
            request_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed),
            "a stream that ended before init must drop the watcher without reporting"
        );
    }

    /// Only a runner that fires the signal may be watched: arming a watcher on one that never
    /// reports would read every run as "never requested" and price it at zero.
    #[test]
    fn a_runner_without_a_watcher_never_arms() {
        assert!(claude_runner().reports_model_request());
        let codex = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
            sandbox_mode: None,
        };
        assert!(!codex.reports_model_request());
        assert!(!FakeCommandRunner::default().reports_model_request());
    }

    #[tokio::test]
    async fn fake_runner_returns_canned_outcome() {
        let runner = FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: "42".into(),
                stderr: String::new(),
                session_id: Some("sess-1".into()),
                cost_usd: Some(1.5),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
                model_requested: false,
            })),
            last_permission: std::sync::Mutex::new(None),
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = runner
            .run_prompt(test_run_request("what is 6*7"), tx, discard_transcript())
            .await
            .unwrap();
        assert_eq!(outcome.stdout, "42");
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.cost_usd, Some(1.5));
    }

    #[tokio::test]
    async fn progress_deadline_holds_while_events_arrive() {
        let progress_timeout = std::time::Duration::from_millis(30);
        let runner = FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: [
                    r#"{"type":"system","subtype":"init","session_id":"sess-progress"}"#,
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"one"}]}}"#,
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}"#,
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"three"}]}}"#,
                    r#"{"type":"result","subtype":"success","result":"done"}"#,
                ]
                .join("\n"),
                stderr: String::new(),
                session_id: Some("sess-progress".into()),
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
                compacted: false,
                model_requested: false,
            })),
            // The fake releases one canned event per interval. The complete run therefore lasts
            // well beyond the progress deadline while every individual quiet gap stays below it.
            delay: std::sync::Mutex::new(Some(std::time::Duration::from_millis(20))),
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let started = tokio::time::Instant::now();
        let mut request = test_run_request("keep working");
        request.progress_timeout = Some(progress_timeout);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            runner.run_prompt(request, tx, discard_transcript()),
        )
        .await
        .expect("events should keep the progress deadline alive")
        .expect("the fake run should complete");

        assert_eq!(outcome.exit_code, 0);
        assert!(
            started.elapsed() >= progress_timeout * 3,
            "the fixture must outlive the progress deadline several times over"
        );
    }

    const TOOL_USE_LINE: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}]}}"#;
    const TOOL_RESULT_LINE: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]}}"#;

    /// A long tool call prints nothing until its result. The one-shot loop read that quiet as a
    /// stalled run and killed it mid-tool; a kept process (`LiveChat::gather`) already knew better.
    /// Driven through the loop's own read step over a pipe whose far end goes quiet for several
    /// deadlines while the tool "runs".
    #[tokio::test]
    async fn a_one_shot_turn_with_an_open_tool_call_is_not_cut_off_for_silence() {
        let silence = std::time::Duration::from_millis(20);
        let (mut cli, stdout) = tokio::io::duplex(4096);
        let mut lines = BufReader::new(stdout).lines();
        let mut open = std::collections::HashSet::new();

        cli.write_all(format!("{TOOL_USE_LINE}\n").as_bytes())
            .await
            .unwrap();
        let opened = next_line_unless_silent(&mut lines, Some(silence), &open)
            .await
            .expect("the tool_use line is already there")
            .unwrap()
            .unwrap();
        track_open_tools(&opened, &mut open);
        assert!(!open.is_empty(), "the tool_use line opens a tool call");

        let tool = tokio::spawn(async move {
            tokio::time::sleep(silence * 5).await;
            cli.write_all(format!("{TOOL_RESULT_LINE}\n").as_bytes())
                .await
                .unwrap();
            cli
        });
        let answered = next_line_unless_silent(&mut lines, Some(silence), &open)
            .await
            .expect("an open tool call is work in progress, not silence")
            .unwrap()
            .unwrap();
        track_open_tools(&answered, &mut open);
        assert!(open.is_empty(), "the tool_result closes the call");
        drop(tool.await.unwrap());
    }

    /// The other half: with nothing open, quiet past the deadline is still a stalled run.
    #[tokio::test]
    async fn a_one_shot_turn_with_no_open_tool_call_is_cut_off_for_silence() {
        let silence = std::time::Duration::from_millis(20);
        // The writer is held, so the stream neither ends nor speaks.
        let (_cli, stdout) = tokio::io::duplex(4096);
        let mut lines = BufReader::new(stdout).lines();

        let read =
            next_line_unless_silent(&mut lines, Some(silence), &std::collections::HashSet::new())
                .await;

        assert_eq!(read.err(), Some(silence));
    }

    #[tokio::test]
    async fn fake_runner_emits_session_id_before_returning() {
        let runner = FakeCommandRunner::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            runner
                .run_prompt(test_run_request("hi"), tx, discard_transcript())
                .await
        });
        let sid = rx.recv().await;
        assert_eq!(sid.as_deref(), Some("fake-session-id"));
        let outcome = handle.await.unwrap().unwrap();
        assert_eq!(outcome.session_id.as_deref(), Some("fake-session-id"));
    }

    #[tokio::test]
    async fn fake_runner_records_the_resume_session_id() {
        let runner = FakeCommandRunner::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut request = test_run_request("resume please");
        request.resume_session_id = Some("sess-9".to_string());
        runner
            .run_prompt(request, tx, discard_transcript())
            .await
            .unwrap();
        assert_eq!(
            *runner.last_resume.lock().unwrap(),
            Some("sess-9".to_string())
        );
    }

    #[tokio::test]
    async fn fake_runner_records_the_mcp_config() {
        let runner = FakeCommandRunner::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut request = test_run_request("use mcp");
        request.mcp_config = Some(std::path::PathBuf::from("C:/tmp/mcp.json"));
        request.tool_policy = ToolPolicy::McpOnly;
        runner
            .run_prompt(request, tx, discard_transcript())
            .await
            .unwrap();
        assert_eq!(
            *runner.last_mcp_config.lock().unwrap(),
            Some(std::path::PathBuf::from("C:/tmp/mcp.json"))
        );
    }

    /// The plan, which the window had as the word `TodoWrite` and nothing else.
    ///
    /// A turn that writes a plan and then works through it is the shape of most real work, and none
    /// of it reached the page: `detail_of` looks for a path or a command, a `TodoWrite` carries
    /// neither, so the call arrived as a bare name. Watching a model tick items off is half of what
    /// a person is looking at when they look at the editor.
    #[test]
    fn a_plan_is_carried_beside_the_call_that_wrote_it() {
        let stream = [message(serde_json::json!([{
            "type": "tool_use", "name": "TodoWrite",
            "input": {"todos": [
                {"content": "ler o parser", "status": "completed"},
                {"content": "arranjar as datas", "status": "in_progress"},
                {"content": "correr os testes", "status": "pending"},
            ]}
        }]))]
        .join("\n");

        let did = live_from_stream(&stream).did;

        assert_eq!(did.len(), 1);
        assert_eq!(
            did[0].todos,
            vec![
                Todo {
                    text: "ler o parser".into(),
                    status: "completed".into()
                },
                Todo {
                    text: "arranjar as datas".into(),
                    status: "in_progress".into()
                },
                Todo {
                    text: "correr os testes".into(),
                    status: "pending".into()
                },
            ]
        );
    }

    /// Every other tool carries no plan, and says so with an empty list rather than with a shape
    /// the window has to test for.
    #[test]
    fn a_call_that_is_not_a_plan_carries_no_plan() {
        let stream = message(serde_json::json!([{
            "type": "tool_use", "name": "Read", "input": {"file_path": "C:/x.rs"}
        }]));

        let did = live_from_stream(&stream).did;

        assert!(did[0].todos.is_empty());
        assert_eq!(did[0].detail.as_deref(), Some("C:/x.rs"));
    }

    /// A row written before this existed deserialises, and reads as a call that wrote no plan.
    /// `tools_used` is stored JSON: every turn already recorded is a row without the field.
    #[test]
    fn a_call_stored_before_plans_existed_still_reads() {
        let old: ToolCall =
            serde_json::from_str(r#"{"name":"Bash","detail":"cargo test"}"#).unwrap();

        assert_eq!(old.name, "Bash");
        assert!(old.todos.is_empty());
    }

    /// A subagent's call said `Task` and nothing else, which is the one call where the name alone
    /// says least: every `Task` looks like every other, and what distinguishes them is the sentence
    /// the model wrote to describe the work. It carries no path and no command, so the fixed list
    /// of keys walked straight past it.
    #[test]
    fn a_subagent_call_says_what_it_was_sent_to_do() {
        let stream = message(serde_json::json!([{
            "type": "tool_use", "name": "Task",
            "input": {"description": "rever o diff", "prompt": "olha para tudo", "subagent_type": "reviewer"}
        }]));

        let did = live_from_stream(&stream).did;

        assert_eq!(did[0].detail.as_deref(), Some("rever o diff"));
    }

    /// Ordered, not searched: a tool carrying both keeps the one that says where it acted. The
    /// description is the last resort, never the preferred answer.
    #[test]
    fn a_description_never_wins_over_the_thing_that_was_acted_on() {
        let stream = message(serde_json::json!([{
            "type": "tool_use", "name": "Edit",
            "input": {"file_path": "C:/x.rs", "description": "arranjar isto"}
        }]));

        let did = live_from_stream(&stream).did;

        assert_eq!(did[0].detail.as_deref(), Some("C:/x.rs"));
    }

    /// A turn carrying a picture is a content ARRAY, which is the API's own shape for one.
    ///
    /// Asked of the CLI before this existed, because nothing here could answer it: a `user` line
    /// whose content is an array with an `image` block in it is accepted by
    /// `--input-format stream-json`, and the model SEES it — sent a solid magenta square and asked
    /// what colour it was, it answered "Magenta", which is not a thing anybody guesses.
    ///
    /// The image comes FIRST and the words after. That is the order the API documents for a
    /// question about a picture, and the order a person types in: the screenshot, then what they
    /// want to know about it.
    #[test]
    fn a_turn_carrying_a_picture_is_written_as_a_content_array() {
        let line = user_message_line(
            "what colour is this?",
            &[Attachment {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
        );

        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let content = value
            .pointer("/message/content")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "aGVsbG8=");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "what colour is this?");
    }

    /// A turn carrying nothing keeps the plain string it has always been.
    ///
    /// Not tidiness: the string form is the one measured working against the CLI, and every run in
    /// this daemon that is not a chat uses it. Rewriting them all as arrays to make one new case
    /// uniform would be changing what is proven to make room for what is not.
    #[test]
    fn a_turn_carrying_nothing_is_written_as_the_plain_string_it_has_always_been() {
        let line = user_message_line("arranja o parser", &[]);

        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            value.pointer("/message/content").unwrap(),
            "arranja o parser"
        );
    }

    /// Stopping one task is one `control_request` line naming it, and never an interrupt.
    #[test]
    fn stopping_a_task_is_one_control_request_naming_it() {
        let line = stop_task_line("b84qqcytz");
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["type"], "control_request");
        assert_eq!(value.pointer("/request/subtype").unwrap(), "stop_task");
        assert_eq!(value.pointer("/request/task_id").unwrap(), "b84qqcytz");
        assert_ne!(
            value["request_id"],
            serde_json::from_str::<serde_json::Value>(stop_task_line("b84qqcytz").trim_end())
                .unwrap()["request_id"],
        );
    }

    /// An interrupt is ONE `control_request` line, and every one carries an id of its own.
    ///
    /// The id is how the CLI answers a request (`control_response` echoes it), so two interrupts
    /// sharing one would make the second unanswerable. One line, because stdin is newline-delimited.
    #[test]
    fn the_interrupt_is_one_control_request_line_with_its_own_id() {
        let first = interrupt_line();
        let second = interrupt_line();

        assert!(first.ends_with('\n'));
        assert_eq!(first.trim_end().lines().count(), 1);

        let a: serde_json::Value = serde_json::from_str(first.trim()).unwrap();
        let b: serde_json::Value = serde_json::from_str(second.trim()).unwrap();
        assert_eq!(a["type"], "control_request");
        assert_eq!(a.pointer("/request/subtype").unwrap(), "interrupt");
        assert_eq!(b["type"], "control_request");
        assert_eq!(b.pointer("/request/subtype").unwrap(), "interrupt");

        let id_a = a["request_id"].as_str().expect("a request_id");
        let id_b = b["request_id"].as_str().expect("a request_id");
        assert!(id_a.starts_with("nucleos-interrupt-"));
        assert!(id_b.starts_with("nucleos-interrupt-"));
        assert_ne!(id_a, id_b);
    }

    /// A turn that ended because the person stopped it reads differently from one that failed.
    ///
    /// Both end in `error_during_execution`; what separates them is the user line the CLI writes
    /// first, `[Request interrupted by user]`. Neither alone is enough.
    #[test]
    fn an_interrupted_turn_is_told_apart_from_a_failed_one() {
        let marker = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": INTERRUPT_MARKER}]}
        })
        .to_string();
        let answer = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"half an answer"}]}}"#;
        let interrupted_result = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"stop_reason":null,"num_turns":2,"total_cost_usd":0}"#;
        let failed_result = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":1,"total_cost_usd":0}"#;
        let ok_result =
            r#"{"type":"result","subtype":"success","result":"done","total_cost_usd":0}"#;

        // The marker followed by the error result: stopped by the person.
        assert!(interrupted_by_user(&format!(
            "{answer}\n{marker}\n{interrupted_result}"
        )));
        // The same error with no marker: a failure.
        assert!(!interrupted_by_user(&format!("{answer}\n{failed_result}")));
        // The marker, but the turn went on to succeed: not a stop.
        assert!(!interrupted_by_user(&format!("{marker}\n{ok_result}")));
        // The marker with no result at all: nothing has ended yet.
        assert!(!interrupted_by_user(&format!("{answer}\n{marker}")));
        // The LAST result decides: an earlier interrupted turn does not colour a later failure.
        assert!(!interrupted_by_user(&format!(
            "{marker}\n{interrupted_result}\n{failed_result}"
        )));
        assert!(!interrupted_by_user(""));
    }

    /// One line of a `--include-partial-messages` stream: a slice of text as it is typed.
    fn delta(text: &str) -> String {
        serde_json::json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": text}}
        })
        .to_string()
    }

    /// A whole assistant message, which is what arrives with or without partials.
    fn message(blocks: serde_json::Value) -> String {
        serde_json::json!({"type": "assistant", "message": {"content": blocks}}).to_string()
    }

    fn said(text: &str) -> serde_json::Value {
        serde_json::json!([{"type": "text", "text": text}])
    }

    /* ------------------------------------------- what a tool answered -- */

    /// One `tool_result`, as the CLI sends one.
    fn answered(tool_use_id: &str, content: serde_json::Value, is_error: bool) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error
            }]}
        })
        .to_string()
    }

    /// A turn said what it REACHED FOR and never what it found.
    ///
    /// `Bash` beside `cargo test dates::`, with no way to learn from the conversation whether the
    /// tests passed — the paragraph underneath is the model's summary of exactly that, and a
    /// summary is what somebody opening a tool call has decided not to take on trust.
    #[test]
    fn a_tool_call_carries_what_the_tool_answered() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Bash",
                "input": {"command": "cargo test dates::"}
            }])),
            answered(
                "toolu_1",
                serde_json::json!("test result: ok. 3 passed"),
                false,
            ),
        ]
        .join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(live.did.len(), 1);
        assert_eq!(
            live.did[0].result.as_deref(),
            Some("test result: ok. 3 passed")
        );
        assert_eq!(live.did[0].result_chars, Some(25));
        assert!(!live.did[0].result_failed);
        // And the tool has stopped running, which is the behaviour that was already here.
        assert_eq!(live.doing, None);
    }

    /// Paired by id and never by position.
    ///
    /// The CLI runs tool calls concurrently, so answers arrive in whatever order the tools finish
    /// — "the most recent call" is wrong exactly when it matters, and the failure is quiet: two
    /// real answers, each filed under the other's question.
    #[test]
    fn two_tools_in_flight_get_their_own_answers_back() {
        let stream = [
            message(serde_json::json!([
                {"type": "tool_use", "id": "toolu_slow", "name": "Bash",
                 "input": {"command": "cargo test"}},
                {"type": "tool_use", "id": "toolu_fast", "name": "Read",
                 "input": {"file_path": "core/src/dates.rs"}}
            ])),
            // The second call answers first, which is the whole point of this test.
            answered("toolu_fast", serde_json::json!("fn is_leap_year"), false),
            answered("toolu_slow", serde_json::json!("test result: ok"), false),
        ]
        .join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(live.did[0].name, "Bash");
        assert_eq!(live.did[0].result.as_deref(), Some("test result: ok"));
        assert_eq!(live.did[1].name, "Read");
        assert_eq!(live.did[1].result.as_deref(), Some("fn is_leap_year"));
    }

    /// A `Read` answers with the whole file, and the whole file does not go in a database row.
    ///
    /// The full length travels beside the cut so the window can say what it is NOT showing — a
    /// truncation presented as the whole answer is how somebody concludes a command printed
    /// nothing after the first thirty lines.
    #[test]
    fn a_long_answer_is_cut_and_says_how_long_it_really_was() {
        let whole = "x".repeat(RESULT_LIMIT + 500);
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Read",
                "input": {"file_path": "big.rs"}
            }])),
            answered("toolu_1", serde_json::json!(whole), false),
        ]
        .join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(
            live.did[0].result.as_ref().map(|kept| kept.chars().count()),
            Some(RESULT_LIMIT)
        );
        assert_eq!(live.did[0].result_chars, Some((RESULT_LIMIT + 500) as i64));
    }

    /// "The command failed" and "the command printed something that mentions an error" are
    /// different facts, and only the stream knows which this was.
    #[test]
    fn a_tool_that_failed_is_recorded_as_having_failed() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Bash",
                "input": {"command": "cargo test"}
            }])),
            answered(
                "toolu_1",
                serde_json::json!("error: could not compile"),
                true,
            ),
        ]
        .join("\n");

        assert!(live_from_stream(&stream).did[0].result_failed);
    }

    /// The CLI sends `content` two ways, and both are ordinary.
    #[test]
    fn an_answer_sent_as_blocks_reads_back_as_its_text() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Grep", "input": {"pattern": "leap"}
            }])),
            answered(
                "toolu_1",
                serde_json::json!([{"type": "text", "text": "core/src/dates.rs:12"}]),
                false,
            ),
        ]
        .join("\n");

        assert_eq!(
            live_from_stream(&stream).did[0].result.as_deref(),
            Some("core/src/dates.rs:12")
        );
    }

    /// An answer whose id names no call this stream made belongs to something that is not in this
    /// list, and is dropped rather than attached to whatever happened to be nearest.
    #[test]
    fn an_answer_to_a_call_this_stream_never_made_is_dropped() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "a.rs"}
            }])),
            answered("toolu_somebody_else", serde_json::json!("not ours"), false),
        ]
        .join("\n");

        let live = live_from_stream(&stream);
        assert_eq!(live.did[0].result, None);
        // It still ended the wait: a tool answered, whichever one it was.
        assert_eq!(live.doing, None);
    }

    /// A turn recorded before any of this existed still reads back.
    ///
    /// `tools_used` is stored JSON, and every row already in the database is a call with none of
    /// these three fields. They default to absent, which is exactly what those turns knew.
    #[test]
    fn a_stored_call_from_before_the_answers_existed_still_parses() {
        let old: ToolCall =
            serde_json::from_str(r#"{"name":"Read","detail":"core/src/dates.rs"}"#).unwrap();

        assert_eq!(old.name, "Read");
        assert_eq!(old.result, None);
        assert_eq!(old.result_chars, None);
        assert!(!old.result_failed);
    }

    /// And a call with nothing to say about its answer says nothing on the wire.
    ///
    /// `skip_serializing_if` is what keeps the transcript the size it was: three null fields per
    /// call, over a hundred turns, on a route polled once a second.
    #[test]
    fn a_call_without_an_answer_serialises_without_the_fields() {
        let bare = ToolCall {
            name: "Read".to_string(),
            ..ToolCall::default()
        };

        let json = serde_json::to_string(&bare).unwrap();

        assert!(
            !json.contains("result"),
            "the empty answer was serialised: {json}"
        );
    }

    /// The agent map's whole input from one stream: a `Task` call, the calls its subagent made,
    /// a background `Bash`, and the task events that say what became of it.
    #[test]
    fn a_stream_with_a_subagent_and_a_background_task_fills_the_agent_map_fields() {
        let stream = [
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_task","name":"Task","input":{"description":"Find callers","prompt":"p","subagent_type":"Explore","model":"haiku"}}]}}"#,
            r#"{"type":"assistant","parent_tool_use_id":"toolu_task","message":{"content":[{"type":"text","text":"subagent words"},{"type":"tool_use","id":"toolu_child","name":"Grep","input":{"pattern":"fn main"}}]}}"#,
            r#"{"type":"user","parent_tool_use_id":"toolu_task","timestamp":"2026-10-03T10:00:02.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_child","content":"src/main.rs"}]}}"#,
            r#"{"type":"user","parent_tool_use_id":null,"timestamp":"2026-10-03T10:00:05.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_task","content":"done"}]},"tool_use_result":{"status":"completed","totalTokens":4242}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"sleep 20","run_in_background":true}}]}}"#,
            r#"{"type":"system","subtype":"task_started","task_id":"b1","tool_use_id":"toolu_bg","description":"sleep 20","task_type":"local_bash"}"#,
            r#"{"type":"user","parent_tool_use_id":null,"timestamp":"2026-10-03T10:00:06.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_bg","content":"Command running in background with ID: b1"}]},"tool_use_result":{"backgroundTaskId":"b1"}}"#,
            r#"{"type":"system","subtype":"task_updated","task_id":"b1","patch":{"status":"completed","end_time":1791021626000}}"#,
            r#"{"type":"system","subtype":"task_notification","task_id":"b1","tool_use_id":"toolu_bg","status":"completed"}"#,
        ]
        .join("\n");

        let live = live_from_stream(&stream);
        let [task, child, bash] = live.did.as_slice() else {
            panic!("expected three calls, got {:?}", live.did);
        };

        assert_eq!(task.id.as_deref(), Some("toolu_task"));
        assert_eq!(task.parent, None);
        assert_eq!(task.subagent_type.as_deref(), Some("Explore"));
        assert_eq!(task.model.as_deref(), Some("haiku"));
        assert_eq!(task.tokens, Some(4242));
        assert!(task.started_at.is_some());
        assert_eq!(
            task.finished_at.as_deref(),
            Some("2026-10-03T10:00:05.000Z")
        );
        assert!(!task.background && task.status.is_none());

        assert_eq!(child.parent.as_deref(), Some("toolu_task"));
        assert_eq!(child.subagent_type, None);
        assert_eq!(
            child.finished_at.as_deref(),
            Some("2026-10-03T10:00:02.000Z")
        );

        assert!(bash.background);
        assert_eq!(bash.status.as_deref(), Some("completed"));
        assert_eq!(
            bash.finished_at.as_deref(),
            chrono::DateTime::from_timestamp_millis(1791021626000)
                .map(|end| end.to_rfc3339())
                .as_deref()
        );

        // The subagent's words are its own, not the turn's answer.
        assert!(!live.text.contains("subagent words"), "{}", live.text);
    }

    /// A task killed under the turn reads as killed, and `stopped` is the CLI's word for it.
    #[test]
    fn a_background_task_stopped_reads_as_killed() {
        let stream = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"sleep 99","run_in_background":true}}]}}"#,
            r#"{"type":"system","subtype":"task_notification","task_id":"b2","tool_use_id":"toolu_bg","status":"stopped"}"#,
        ]
        .join("\n");

        let did = live_from_stream(&stream).did;

        assert_eq!(did[0].status.as_deref(), Some("killed"));
        assert!(did[0].finished_at.is_some());
    }

    /// A background call carries the CLI's own id for its task, from whichever event names it
    /// first: the task's start, or the answer to the launch.
    #[test]
    fn a_background_call_carries_its_task_id() {
        let from_the_start = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_bg","name":"Bash","input":{"command":"sleep 20","run_in_background":true}}]}}"#,
            r#"{"type":"system","subtype":"task_started","task_id":"b1","tool_use_id":"toolu_bg","is_backgrounded":true}"#,
        ]
        .join("
");
        assert_eq!(
            live_from_stream(&from_the_start).did[0].task_id.as_deref(),
            Some("b1")
        );

        let from_the_answer = [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_bg2","name":"Bash","input":{"command":"sleep 20","run_in_background":true}}]}}"#,
            r#"{"type":"user","parent_tool_use_id":null,"timestamp":"2026-10-03T10:00:06.000Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_bg2","content":"Command running in background with ID: b2"}]},"tool_use_result":{"backgroundTaskId":"b2"}}"#,
        ]
        .join("
");
        assert_eq!(
            live_from_stream(&from_the_answer).did[0].task_id.as_deref(),
            Some("b2")
        );

        let plain = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_read","name":"Read","input":{"file_path":"a"}}]}}"#;
        assert_eq!(live_from_stream(plain).did[0].task_id, None);
    }

    /// The daemon-clock fallback is fixed by the first parse, so a re-read does not move it.
    #[test]
    fn a_start_time_without_a_stream_clock_is_stable_across_reads() {
        let stream = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_stable_clock","name":"Read","input":{"file_path":"a"}}]}}"#;

        let first = live_from_stream(stream).did[0].started_at.clone();
        std::thread::sleep(Duration::from_millis(5));
        let second = live_from_stream(stream).did[0].started_at.clone();

        assert!(first.is_some());
        assert_eq!(first, second);
    }

    /// `tools_used` rows recorded before the agent-map fields still read back.
    #[test]
    fn an_old_tools_used_row_still_deserialises() {
        let old =
            r#"[{"name":"Read","detail":"src/a.rs","todos":[],"result":"x","result_chars":1}]"#;

        let calls: Vec<ToolCall> = serde_json::from_str(old).unwrap();

        assert_eq!(calls[0].name, "Read");
        assert_eq!(calls[0].id, None);
        assert!(!calls[0].background);
        assert_eq!(calls[0].status, None);
    }

    #[test]
    fn the_cache_ttl_comes_from_the_ephemeral_split() {
        let usage = |five: u64, hour: u64| {
            format!(
                r#"{{"type":"assistant","message":{{"usage":{{"cache_creation":{{"ephemeral_5m_input_tokens":{five},"ephemeral_1h_input_tokens":{hour}}}}}}}}}"#
            )
        };

        assert_eq!(cache_ttl_from_stream(&usage(120, 0)), Some("5m"));
        assert_eq!(
            cache_ttl_from_stream(&[usage(120, 0), usage(0, 40)].join("\n")),
            Some("1h")
        );
        assert_eq!(cache_ttl_from_stream(&usage(0, 0)), None);
        assert_eq!(
            cache_ttl_from_stream(r#"{"type":"result","usage":{"cache_creation_input_tokens":9}}"#),
            None
        );
        // The result is the fallback when no main-agent message carried the split.
        assert_eq!(
            cache_ttl_from_stream(
                r#"{"type":"result","usage":{"cache_creation":{"ephemeral_1h_input_tokens":7}}}"#
            ),
            Some("1h")
        );
    }

    #[test]
    fn the_model_is_the_main_agents_not_a_subagents() {
        let stream = [
            r#"{"type":"system","subtype":"init","model":"claude-opus-4"}"#,
            r#"{"type":"assistant","parent_tool_use_id":"toolu_x","message":{"model":"claude-haiku"}}"#,
            r#"{"type":"assistant","parent_tool_use_id":null,"message":{"model":"claude-sonnet"}}"#,
        ]
        .join("\n");

        assert_eq!(model_from_stream(&stream).as_deref(), Some("claude-sonnet"));
        assert_eq!(
            model_from_stream(r#"{"type":"system","subtype":"init","model":"claude-opus-4"}"#)
                .as_deref(),
            Some("claude-opus-4")
        );
    }

    #[test]
    fn a_stream_in_flight_reads_back_as_what_has_been_written_so_far() {
        let stream = [delta("está"), delta(" quase")].join(
            "
",
        );

        let live = live_from_stream(&stream);

        assert_eq!(live.text, "está quase");
        assert_eq!(live.doing, None);
    }

    /// The deltas and the completed message describe the SAME words, and a reader that took both
    /// would show every sentence twice — which is what a naive concatenation does, and it looks like
    /// the model stuttering rather than like a parsing bug.
    #[test]
    fn a_completed_message_supersedes_the_deltas_it_was_written_from() {
        let stream = [delta("está"), delta(" quase"), message(said("está quase"))].join(
            "
",
        );

        assert_eq!(live_from_stream(&stream).text, "está quase");
    }

    /// One line of a partial stream: a slice of THINKING as it is typed.
    ///
    /// A separate delta type from `text_delta`, and that is the whole point: the two arrive
    /// interleaved on the same stream, and a reader that took `delta.text` alone would find nothing
    /// here and show a model that sat silent through the part worth watching.
    fn pondered(text: &str) -> String {
        serde_json::json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "thinking_delta", "thinking": text}}
        })
        .to_string()
    }

    /// What the model thought is carried BESIDE what it said, never folded into it.
    ///
    /// The editor shows thinking; this window dropped it on the floor. `live_from_stream` kept only
    /// `text` blocks, so an answer whose reasoning WAS the work arrived as its conclusion alone —
    /// and a conclusion with nothing visible behind it is exactly the thing a reader cannot check.
    ///
    /// Beside, and not concatenated: thinking is not the reply. Joining them would put the model's
    /// working into the answer this conversation records, into the replay built from that answer,
    /// and into every place downstream that treats `text` as the thing that was said.
    #[test]
    fn what_the_model_thought_is_carried_beside_what_it_said() {
        let stream = message(serde_json::json!([
            {"type": "thinking", "thinking": "o mês vem antes do dia neste formato"},
            {"type": "text", "text": "é o parser de datas"}
        ]));

        let live = live_from_stream(&stream);

        assert_eq!(live.thought, vec!["o mês vem antes do dia neste formato"]);
        assert_eq!(live.text, "é o parser de datas");
    }

    /// Thinking streams before it completes, exactly as text does.
    ///
    /// Without this the window shows nothing at all for the longest stretch of a hard turn, and
    /// then the whole of the reasoning at once, after it has stopped being interesting.
    #[test]
    fn thinking_in_flight_reads_back_before_its_block_completes() {
        let stream = [pondered("o mês"), pondered(" vem antes")].join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(live.thought, vec!["o mês vem antes"]);
        // And it did not leak into the reply: nothing has been SAID yet.
        assert_eq!(live.text, "");
    }

    /// The deltas and the completed block are the SAME thought, and a reader that took both would
    /// show it twice — the identical bug `a_completed_message_supersedes_the_deltas` guards for
    /// speech, on the channel beside it.
    #[test]
    fn a_completed_thought_supersedes_the_deltas_it_was_written_from() {
        let stream = [
            pondered("o mês"),
            pondered(" vem antes"),
            message(serde_json::json!([{"type": "thinking", "thinking": "o mês vem antes"}])),
        ]
        .join("\n");

        assert_eq!(live_from_stream(&stream).thought, vec!["o mês vem antes"]);
    }

    /// A real stream carries the SIZE of a thought and never its words.
    ///
    /// Built from a stream this machine actually produced, because the synthetic ones above are
    /// what let a feature be written that could never draw anything. Asked of the CLI directly:
    /// every `thinking` block arrives with `thinking: ""` -- 610 of them across one interactive
    /// session, not one with a word in it. Claude Code emits the signature and a running
    /// `thinking_tokens` estimate and withholds the text.
    ///
    /// So this is the honest claim a window can make about a thought, and it is the only one.
    #[test]
    fn a_thought_is_measured_because_the_cli_withholds_its_words() {
        let stream = [
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":50,"estimated_tokens_delta":50}"#.to_string(),
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":177,"estimated_tokens_delta":27}"#.to_string(),
            message(serde_json::json!([{"type": "thinking", "thinking": "", "signature": "ErwFCqUB"}])),
            message(said("17 x 23 = 391")),
        ]
        .join("
");

        let live = live_from_stream(&stream);

        assert_eq!(live.thought_tokens, Some(177));
        // And nothing is claimed to have been said in it, because nothing was.
        assert!(live.thought.is_empty());
        assert_eq!(live.text, "17 x 23 = 391");
    }

    /// A turn that did not think says so as nothing, never as zero.
    #[test]
    fn a_turn_that_did_not_think_reports_no_measurement_at_all() {
        assert_eq!(live_from_stream(&message(said("ola"))).thought_tokens, None);
    }

    /// Text, a tool, then more text is ONE answer with a gap in the middle. Keeping only the last
    /// message would throw away everything said before the model reached for anything.
    #[test]
    fn text_written_before_and_after_a_tool_call_is_one_answer() {
        let stream = [
            message(said("deixa ver o ficheiro")),
            message(serde_json::json!([{"type": "tool_use", "name": "Read", "input": {}}])),
            serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "content": "ok"}]}
            })
            .to_string(),
            message(said("é o parser de datas")),
        ]
        .join(
            "
",
        );

        let live = live_from_stream(&stream);

        assert_eq!(
            live.text,
            "deixa ver o ficheiro

é o parser de datas"
        );
        assert_eq!(live.doing, None);
    }

    /// What it is doing right now, which is the half a spinner cannot say.
    /// What the turn DID, kept beside what it said. A turn that read four files and ran the tests
    /// answered with more than its last paragraph, and the paragraph alone reads as an opinion.
    #[test]
    fn a_stream_reads_back_the_tools_it_ran_in_the_order_it_ran_them() {
        let stream = [
            message(serde_json::json!([
                {"type": "tool_use", "name": "Read", "input": {"file_path": "core/src/parser.rs"}}
            ])),
            message(serde_json::json!([
                {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test parser"}}
            ])),
        ]
        .join(
            "
",
        );

        let did = live_from_stream(&stream).did;

        assert_eq!(did.len(), 2);
        assert_eq!(did[0].name, "Read");
        assert_eq!(did[0].detail.as_deref(), Some("core/src/parser.rs"));
        assert_eq!(did[1].name, "Bash");
        assert_eq!(did[1].detail.as_deref(), Some("cargo test parser"));
    }

    /// A tool whose arguments this does not recognise is still a tool that ran. Naming it with no
    /// detail says less than the truth; leaving it out says something false.
    #[test]
    fn a_tool_with_no_argument_worth_showing_is_still_recorded() {
        let stream = message(serde_json::json!([
            {"type": "tool_use", "name": "TodoWrite", "input": {"todos": []}}
        ]));

        let did = live_from_stream(&stream).did;

        assert_eq!(did.len(), 1);
        assert_eq!(did[0].name, "TodoWrite");
        assert_eq!(did[0].detail, None);
    }

    #[test]
    fn a_stream_says_which_tool_is_running_until_that_tool_returns() {
        let calling = [
            message(said("deixa ver")),
            message(serde_json::json!([{"type": "tool_use", "name": "Bash", "input": {}}])),
        ]
        .join(
            "
",
        );

        assert_eq!(live_from_stream(&calling).doing.as_deref(), Some("Bash"));

        let returned = [
            calling,
            serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "content": "ok"}]}
            })
            .to_string(),
        ]
        .join(
            "
",
        );

        assert_eq!(live_from_stream(&returned).doing, None);
    }

    /// A turn that has only just started has said nothing, and that is not the same as a turn that
    /// answered with nothing — the caller is asking about a run still in flight.
    #[test]
    fn a_stream_carrying_only_transport_reads_back_empty() {
        let stream = [
            serde_json::json!({"type": "system", "subtype": "init", "session_id": "s"}).to_string(),
            "not json at all".to_string(),
        ]
        .join(
            "
",
        );

        let live = live_from_stream(&stream);

        assert_eq!(live.text, "");
        assert_eq!(live.doing, None);
    }

    #[test]
    fn extract_reply_pulls_the_result_text() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}
{"type":"result","subtype":"success","result":"Here are your projects: alpha, beta.","total_cost_usd":0.08}"#;

        assert_eq!(
            extract_reply(stdout),
            Some("Here are your projects: alpha, beta.".to_string())
        );
    }

    /// Job 26's review, as it printed it: a node that never reached the API.
    #[test]
    fn a_review_that_never_reached_the_api_failed_on_something_transient() {
        assert!(failed_on_a_transient_api_error(
            REVIEW_THAT_NEVER_REACHED_THE_API
        ));
    }

    /// No status, a timeout, rate limiting and the 5xx family are worth a second attempt; a request
    /// the API refused is not, because it will be refused again.
    #[test]
    fn only_an_api_error_a_second_attempt_can_get_past_is_transient() {
        let ended_on = |status: &str| {
            format!(
                "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}}\n\
                 {{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\
                 \"terminal_reason\":\"api_error\",\"api_error_status\":{status},\
                 \"result\":\"API Error\"}}"
            )
        };
        for (status, transient) in [
            ("null", true),
            ("408", true),
            ("429", true),
            ("500", true),
            ("503", true),
            ("529", true),
            ("400", false),
            ("401", false),
            ("404", false),
        ] {
            assert_eq!(
                failed_on_a_transient_api_error(&ended_on(status)),
                transient,
                "api_error_status {status}"
            );
        }
    }

    /// Only a 429 is a rate limit: an overloaded service (529), no status at all, or a turn that
    /// ended for another reason is not the subscription being held.
    #[test]
    fn a_429_result_is_a_rate_limit() {
        let ended_on = |status: &str| {
            format!(
                "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}}\n\
                 {{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\
                 \"terminal_reason\":\"api_error\",\"api_error_status\":{status},\
                 \"result\":\"API Error\"}}"
            )
        };
        for (status, limited) in [
            ("429", true),
            ("529", false),
            ("500", false),
            ("null", false),
            ("400", false),
        ] {
            assert_eq!(
                failed_on_rate_limit(&ended_on(status)),
                limited,
                "api_error_status {status}"
            );
        }
        let not_an_api_error = r#"{"type":"result","is_error":true,"terminal_reason":"max_turns","api_error_status":429}"#;
        assert!(!failed_on_rate_limit(not_an_api_error));
        let recovered = format!(
            "{}\n{}",
            ended_on("429"),
            r#"{"type":"result","is_error":false,"result":"done"}"#
        );
        assert!(!failed_on_rate_limit(&recovered));
        assert!(!failed_on_rate_limit(""));
    }

    /// A turn that ended for any other reason ended on something the work did, and a stream with no
    /// result at all says nothing about why it stopped.
    #[test]
    fn a_turn_that_ended_for_any_other_reason_is_not_transient() {
        let max_turns = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"terminal_reason":"max_turns","api_error_status":null,"result":""}"#;
        assert!(!failed_on_a_transient_api_error(max_turns));

        let success = r#"{"type":"result","subtype":"success","is_error":false,"result":"Nothing wrong, nothing missing.","total_cost_usd":0.12}"#;
        assert!(!failed_on_a_transient_api_error(success));

        let no_result =
            r#"{"type":"system","subtype":"api_retry","attempt":1,"error_status":null}"#;
        assert!(!failed_on_a_transient_api_error(no_result));
        assert!(!failed_on_a_transient_api_error(""));

        // The LAST result decides: an error the CLI went on past is not how the run ended.
        let recovered = format!("{REVIEW_THAT_NEVER_REACHED_THE_API}\n{success}");
        assert!(!failed_on_a_transient_api_error(&recovered));
    }

    #[test]
    fn extract_reply_returns_none_without_result_event() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}"#;

        assert_eq!(extract_reply(stdout), None);
    }

    #[test]
    fn usage_absent_is_none_not_zero() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","total_cost_usd":0.08}"#;

        let usage = extract_usage(stdout);

        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.cache_read_tokens, None);
        assert_eq!(usage.num_turns, None);
    }

    #[test]
    fn partial_usage_keeps_missing_fields_none() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","total_cost_usd":0.08,"num_turns":12,"usage":{"input_tokens":1000,"output_tokens":500,"num_turns":99}}"#;

        let usage = extract_usage(stdout);

        assert_eq!(usage.input_tokens, Some(1000));
        assert_eq!(usage.output_tokens, Some(500));
        assert_eq!(usage.cache_read_tokens, None);
        assert_eq!(usage.num_turns, Some(12));
    }

    /// A turn's own lines must arrive before the word that it ended.
    ///
    /// The order is the whole contract. A consumer builds each turn's transcript out of the lines
    /// and closes it on `Ended`, so an end delivered before its own `result` line would close every
    /// turn one line short of what it actually said — and that line is the one carrying the answer.
    #[test]
    fn a_turns_lines_arrive_before_the_word_that_it_ended() {
        let mut splitter = TurnSplitter::new();
        let mut events = Vec::new();
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
            r#"{"type":"result","subtype":"success","total_cost_usd":0.10}"#,
        ] {
            events.extend(splitter.line(line.to_string()));
        }

        assert!(matches!(events[0], TurnEvent::Line(_)));
        assert!(matches!(events[1], TurnEvent::Line(_)));
        // The result line belongs to the turn it ends, so it is a `Line` too — and only then `Ended`.
        assert!(matches!(events[2], TurnEvent::Line(_)));
        assert!(matches!(events[3], TurnEvent::Ended(_)));
        assert_eq!(events.len(), 4);
    }

    /// One process, two answers, two turns — and the second one billed for what it added rather than
    /// for everything the process had spent by then.
    #[test]
    fn two_answers_down_one_process_are_two_turns() {
        let mut splitter = TurnSplitter::new();
        let mut ended = Vec::new();
        for line in [
            r#"{"type":"result","subtype":"success","total_cost_usd":0.1046}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
            r#"{"type":"result","subtype":"success","total_cost_usd":0.2024}"#,
        ] {
            for event in splitter.line(line.to_string()) {
                if let TurnEvent::Ended(turn) = event {
                    ended.push(turn);
                }
            }
        }

        assert_eq!(ended.len(), 2);
        assert_eq!(ended[0].cost_usd, Some(0.1046));
        assert!((ended[1].cost_usd.unwrap() - 0.0978).abs() < 1e-9);
        // And the process's own total is still available to whoever is billing the process.
        assert!((splitter.spent() - 0.2024).abs() < 1e-9);
    }

    /// A process that answers twice reports what it has spent in total, not what the last turn
    /// added. Measured on the real CLI, two turns down one stdin: `total_cost_usd` read 0.1046 and
    /// then 0.2024, so recording the second `result` verbatim bills that turn for the first one as
    /// well — and every turn after it, compounding.
    #[test]
    fn a_later_turns_cost_is_what_it_added_not_what_the_process_has_spent() {
        let first =
            r#"{"type":"result","subtype":"success","total_cost_usd":0.1046,"num_turns":1}"#;
        let second =
            r#"{"type":"result","subtype":"success","total_cost_usd":0.2024,"num_turns":1}"#;

        let (one, spent) = turn_from_result(first, 0.0).unwrap();
        assert_eq!(one.cost_usd, Some(0.1046));

        let (two, spent) = turn_from_result(second, spent).unwrap();
        assert!(
            (two.cost_usd.unwrap() - 0.0978).abs() < 1e-9,
            "expected the difference, got {:?}",
            two.cost_usd
        );
        assert!((spent - 0.2024).abs() < 1e-9);
    }

    /// The counts beside the cost are the turn's OWN — `num_turns` read 1 on both of those two
    /// results rather than 1 and 2 — so differencing them would turn a correct number into a
    /// negative one the moment a turn used fewer tokens than the turn before it.
    #[test]
    fn a_turns_token_counts_are_its_own_and_are_not_differenced() {
        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.30,"num_turns":1,"usage":{"input_tokens":40,"output_tokens":500,"cache_read_input_tokens":32194,"cache_creation_input_tokens":0}}"#;

        let (turn, _) = turn_from_result(line, 0.25).unwrap();

        assert_eq!(turn.usage.input_tokens, Some(40));
        assert_eq!(turn.usage.output_tokens, Some(500));
        assert_eq!(turn.usage.cache_read_tokens, Some(32194));
        assert_eq!(turn.usage.num_turns, Some(1));
    }

    /// Everything else on the stream is not a turn ending, and a boundary drawn on the wrong line
    /// would close a run row while the CLI was still mid-answer.
    #[test]
    fn only_a_result_line_ends_a_turn() {
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "not json at all",
            "",
        ] {
            assert!(
                turn_from_result(line, 0.0).is_none(),
                "{line} must not be read as the end of a turn"
            );
        }
    }

    #[test]
    fn extract_usage_reads_cache_creation() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","num_turns":3,"usage":{"input_tokens":40,"output_tokens":500,"cache_read_input_tokens":0,"cache_creation_input_tokens":12000}}"#;

        let usage = extract_usage(stdout);

        // The run that pays to fill the cache reads nothing back, and this is the shape that proves
        // the two are different facts: without the second number it is indistinguishable from a run
        // that missed the prefix entirely.
        assert_eq!(usage.cache_read_tokens, Some(0));
        assert_eq!(usage.cache_creation_tokens, Some(12000));
    }

    #[test]
    fn absent_cache_creation_stays_unknown() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","usage":{"input_tokens":1000,"cache_read_input_tokens":9000}}"#;

        let usage = extract_usage(stdout);

        // Not `Some(0)`. A transcript that never mentioned cache creation has not reported writing
        // nothing — it has reported nothing, and a detector must be able to tell those apart.
        assert_eq!(usage.cache_creation_tokens, None);
    }

    /// One fold for both CLIs, because their per-turn events cannot appear in the same stream.
    ///
    /// Claude says `assistant` once per content block of a model message; `codex exec` says
    /// `turn.completed` once per turn. Counting both in one place is what keeps the ceiling from
    /// being a Claude-only brake — a limit that silently does not apply on one of the two paths is
    /// worse than no limit, because somebody will believe it is there.
    #[test]
    fn a_turn_is_counted_once_per_model_response_on_either_cli() {
        let claude = r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"hi"}]}}"#;
        let codex = r#"{"type":"turn.completed","usage":{"input_tokens":10}}"#;

        let mut turns = TurnCounter::default();
        assert!(turns.line(claude));
        assert!(turns.line(codex));
        assert_eq!(turns.count(), 2);

        // Everything else in either stream is not a turn. `stream_event` in particular arrives by
        // the hundred for a single message — counting it would trip a ceiling of 200 inside one
        // paragraph of the model's first answer.
        for quiet in [
            r#"{"type":"stream_event","event":{"delta":{"text":"tok"}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"result","subtype":"success","num_turns":9}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
            "not json at all",
            "",
        ] {
            assert!(!turns.line(quiet), "counted a turn for: {quiet}");
        }
        assert_eq!(turns.count(), 2);
    }

    /// The shape of a real answer, and the reason the counter reads ids at all: one response that
    /// says something and calls two tools is three `assistant` events with one `message.id`. Counted
    /// as three, run 900463 was stopped at 125 responses under a ceiling of 200.
    #[test]
    fn the_blocks_of_one_answer_are_one_turn_however_many_tools_it_calls() {
        let block = |id: &str, kind: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"{id}","content":[{{"type":"{kind}"}}]}}}}"#
            )
        };
        let mut turns = TurnCounter::default();

        assert!(turns.line(&block("msg_a", "text")));
        assert!(!turns.line(&block("msg_a", "tool_use")));
        assert!(!turns.line(&block("msg_a", "tool_use")));
        assert_eq!(turns.count(), 1, "three blocks of one answer are one turn");

        // The tool results in between are not turns, and the next answer is one.
        assert!(!turns.line(r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#));
        assert!(turns.line(&block("msg_b", "text")));
        // An id already counted stays counted, even with another answer's blocks in between.
        assert!(!turns.line(&block("msg_a", "text")));
        assert_eq!(turns.count(), 2);

        // With no id there is nothing to join events by, so each counts, as every event did before.
        let anonymous =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"}]}}"#;
        assert!(turns.line(anonymous));
        assert!(turns.line(anonymous));
        assert_eq!(turns.count(), 4);
    }

    /// The input side of a stream, once per answer; the output side, not at all.
    #[test]
    fn a_stream_with_no_result_reports_its_input_side_once_per_answer() {
        let stdout = [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"text"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"tool_use"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":2,"cache_read_input_tokens":200,"output_tokens":4},"content":[{"type":"text"}]}}"#,
        ]
        .join("\n");

        let usage = usage_without_a_result(&stdout);

        assert_eq!(usage.input_tokens, Some(5), "m1 once, not once per block");
        assert_eq!(usage.cache_read_tokens, Some(300));
        assert_eq!(usage.cache_creation_tokens, Some(10));
        assert_eq!(
            usage.output_tokens, None,
            "an assistant event's output count is taken mid-message; it is not the answer's total"
        );
        assert_eq!(usage.num_turns, Some(2));
    }

    /// Unknown is not zero: a stream that never answered has not reported reading nothing.
    #[test]
    fn a_stream_that_never_answered_reports_nothing_rather_than_zero() {
        let usage =
            usage_without_a_result(r#"{"type":"system","subtype":"init","session_id":"s"}"#);
        assert_eq!(usage, RunUsage::default());
    }

    /// The run this was filed over, through the real loop against a spawned process: stopped at
    /// its ceiling with no `result` ever written. A pure test of `usage_without_a_result` would
    /// pass with the function never called, and not being called is what left 900463 all NULL.
    #[tokio::test]
    async fn a_run_stopped_before_its_result_still_reports_what_its_stream_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"init","session_id":"fake","tools":[]}"#,
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"text","text":"looking"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":2,"cache_read_input_tokens":200,"cache_creation_input_tokens":0,"output_tokens":1},"content":[{"type":"text","text":"again"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m3","usage":{"input_tokens":1,"cache_read_input_tokens":300,"cache_creation_input_tokens":5,"output_tokens":1},"content":[{"type":"text","text":"and again"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m4","usage":{"input_tokens":1000,"cache_read_input_tokens":1000,"cache_creation_input_tokens":1000,"output_tokens":1},"content":[{"type":"text","text":"never read"}]}}"#,
            ]),
        );
        // Three responses in four events: counted by event, the ceiling would trip on m2.
        request.max_turns = Some(3);
        let _bin = FakeClaudeBin::set("sh");
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns");

        assert_eq!(
            outcome.exit_code, TURN_CEILING_EXIT_CODE,
            "{}",
            outcome.stderr
        );
        assert_eq!(outcome.num_turns, Some(3), "the count that stopped it");
        assert_eq!(
            outcome.input_tokens,
            Some(6),
            "m1 once, m2, m3 — and never m4"
        );
        assert_eq!(outcome.cache_read_tokens, Some(600));
        assert_eq!(outcome.cache_creation_tokens, Some(15));
        assert_eq!(outcome.output_tokens, None);
        assert_eq!(
            outcome.cost_usd, None,
            "the budget's own time estimate covers this"
        );
    }

    /// The other side of the same line: once a `result` has arrived, its figures are the run's,
    /// and the stream's partial ones never overwrite them.
    #[tokio::test]
    async fn a_run_that_reached_its_result_reports_the_results_figures() {
        let dir = tempfile::tempdir().unwrap();
        let request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"init","session_id":"fake","tools":[]}"#,
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"text","text":"done"}]}}"#,
                r#"{"type":"result","subtype":"success","result":"done","total_cost_usd":0.02,"num_turns":1,"session_id":"fake","usage":{"input_tokens":7,"output_tokens":50,"cache_read_input_tokens":900,"cache_creation_input_tokens":40}}"#,
            ]),
        );
        let _bin = FakeClaudeBin::set("sh");
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns");

        assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
        assert_eq!(outcome.input_tokens, Some(7));
        assert_eq!(outcome.output_tokens, Some(50));
        assert_eq!(outcome.cache_read_tokens, Some(900));
        assert_eq!(outcome.cache_creation_tokens, Some(40));
        assert_eq!(outcome.num_turns, Some(1));
        assert_eq!(outcome.cost_usd, Some(0.02));
    }

    /// Who keeps background tasks: only a run whose process outlives its turn.
    #[test]
    fn a_run_nothing_can_wake_is_given_no_background_tasks() {
        let taken = Some(("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"));

        assert_eq!(background_env(&test_run_request("p")), taken);

        let mut steerable_alone = test_run_request("p");
        steerable_alone.steerable = true;
        assert_eq!(
            background_env(&steerable_alone),
            taken,
            "with no channel, stdin closes after the opening turn and nothing can wake it either"
        );

        let (_later, turns) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        let mut conversation = test_run_request("p");
        conversation.steerable = true;
        conversation.messages = Some(turns);
        assert_eq!(background_env(&conversation), None);

        // A chat turn keeps them even on the one-shot path: a person is watching it.
        let mut chat = test_run_request("p");
        chat.background_tasks = true;
        assert_eq!(background_env(&chat), None);
    }

    /// The signature, as run 900473's own stream wrote it.
    #[test]
    fn a_task_killed_after_the_last_answer_is_named_as_orphaned() {
        let stdout = [
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"I'll wait for the background gate build to finish"}]}}"#,
            r#"{"type":"result","subtype":"success","stop_reason":"end_turn","session_id":"s"}"#,
            r#"{"type":"system","subtype":"background_tasks_changed","tasks":[],"session_id":"s"}"#,
            r#"{"type":"system","subtype":"task_updated","task_id":"b84qqcytz","patch":{"status":"killed","end_time":1789005221034},"session_id":"s"}"#,
            r#"{"type":"system","subtype":"task_notification","task_id":"b84qqcytz","tool_use_id":"toolu_1","status":"stopped","session_id":"s"}"#,
        ]
        .join("\n");

        assert_eq!(
            orphaned_background_tasks(&stdout),
            vec!["b84qqcytz".to_string()],
            "one task, named once although two events report it"
        );
    }

    /// Stopped by the model mid-turn is a decision; finishing after the answer is not dying.
    #[test]
    fn a_task_stopped_mid_turn_or_finished_after_it_is_not_orphaned() {
        let stdout = [
            r#"{"type":"system","subtype":"task_updated","task_id":"early","patch":{"status":"killed"}}"#,
            r#"{"type":"result","subtype":"success"}"#,
            r#"{"type":"system","subtype":"task_notification","task_id":"late","status":"completed"}"#,
        ]
        .join("\n");

        assert!(orphaned_background_tasks(&stdout).is_empty());
    }

    /// Both lines of defence, through the real loop: the variable reaches the spawned process, and
    /// a clean exit that left a task behind is recorded as the failure it is.
    #[tokio::test]
    async fn a_headless_run_that_leaves_a_task_running_is_not_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = printing(&[
            r#"{"type":"system","subtype":"init","session_id":"fake","tools":[]}"#,
        ]) + r#"printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"background-off=%s"}]}}\n' "$CLAUDE_CODE_DISABLE_BACKGROUND_TASKS""#
            + "\n"
            + &printing(&[
                r#"{"type":"result","subtype":"success","result":"waiting","stop_reason":"end_turn","session_id":"fake"}"#,
                r#"{"type":"system","subtype":"task_updated","task_id":"b84qqcytz","patch":{"status":"killed"},"session_id":"fake"}"#,
                r#"{"type":"system","subtype":"task_notification","task_id":"b84qqcytz","status":"stopped","session_id":"fake"}"#,
            ]);
        let request = fake_cli(dir.path(), &script);
        let _bin = FakeClaudeBin::set("sh");
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns");

        assert!(
            outcome.stdout.contains(r#""text":"background-off=1""#),
            "the variable must reach the process it is meant for: {}",
            outcome.stdout
        );
        assert_eq!(outcome.exit_code, -1, "{}", outcome.stderr);
        assert!(
            outcome.stderr.contains("b84qqcytz"),
            "the stderr names the task: {}",
            outcome.stderr
        );
    }

    /// Off by one is the entire bug class of a ceiling, so it is asserted on both sides of the edge.
    #[test]
    fn the_ceiling_trips_at_the_number_it_names_and_never_without_one() {
        assert!(!over_turn_ceiling(199, Some(200)));
        assert!(over_turn_ceiling(200, Some(200)));
        assert!(over_turn_ceiling(201, Some(200)));

        // `None` is no ceiling, and it must stay no ceiling however long the run goes. Every caller
        // that has not chosen a limit keeps exactly today's behaviour.
        assert!(!over_turn_ceiling(1, None));
        assert!(!over_turn_ceiling(1_000_000, None));

        // A ceiling of zero or less would stop a run before its first answer. Refused as "no
        // ceiling" rather than honoured, because a misconfiguration that silently disables every
        // run is worse than one that disables the brake.
        assert!(!over_turn_ceiling(1, Some(0)));
        assert!(!over_turn_ceiling(5, Some(-3)));
    }

    /// A run the daemon stopped has not succeeded, and must not be readable as either of the other
    /// two ways a run can end without doing its work.
    #[test]
    fn a_run_stopped_by_the_ceiling_cannot_be_read_as_a_success_or_as_a_timeout() {
        assert_ne!(TURN_CEILING_EXIT_CODE, 0);
        assert_ne!(TURN_CEILING_EXIT_CODE, -1);
        assert_ne!(TURN_CEILING_EXIT_CODE, PROGRESS_TIMEOUT_EXIT_CODE);
    }

    #[test]
    fn context_fill_reads_usage_from_an_assistant_event() {
        let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":9000}}}"#;

        assert_eq!(
            crate::execution::runner::context_fill_from_line(line, None),
            Some(10_000)
        );
    }

    /// Thinking tokens are not context, and a run with no usage line knows nothing.
    ///
    /// This read `thinking_tokens` as a fallback, and the two numbers are not the same kind of
    /// thing: one is what the model spent reasoning, the other is how full its window is. Measured
    /// against a real stream they are not even close — 177 tokens of thinking on a turn carrying
    /// 48,733 of context, out by two hundred and fifty times.
    ///
    /// It matters because of what reads this column. `get_session` refuses to resume past
    /// `CONTEXT_ROTATION_TOKENS`, and the window draws "x of 140k" under every turn: a run that
    /// recorded 177 was a run claiming to be nearly empty while it was a third full. Unknown is the
    /// honest answer, and the one the ceiling already handles — a NULL fill has never tripped it,
    /// and neither did the wrong number.
    #[test]
    fn thinking_tokens_are_not_read_as_context_because_they_are_not_context() {
        let thinking = r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":125}"#;

        assert_eq!(
            crate::execution::runner::context_fill_from_line(thinking, None),
            None
        );
        // And it does not overwrite a real reading that arrived before it, either.
        assert_eq!(
            crate::execution::runner::context_fill_from_line(thinking, Some(48_733)),
            Some(48_733)
        );
    }

    /// The whole point of a second column: the last turn and the worst moment are different facts.
    #[test]
    fn the_peak_keeps_the_fullest_line_and_not_the_last() {
        let subiu = r#"{"type":"assistant","message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":189000}}}"#;
        let desceu = r#"{"type":"assistant","message":{"usage":{"input_tokens":500,"cache_read_input_tokens":39500}}}"#;

        let mut pico = None;
        pico = context_peak_from_line(subiu, pico);
        pico = context_peak_from_line(desceu, pico);

        // The last turn says 40k. The worst moment said 190k, and that is what pressure reads.
        assert_eq!(pico, Some(190_000));
    }

    #[test]
    fn a_line_that_says_nothing_about_usage_leaves_the_peak_alone() {
        assert_eq!(
            context_peak_from_line(r#"{"type":"system"}"#, Some(120_000)),
            Some(120_000)
        );
        assert_eq!(
            context_peak_from_line("not json", Some(120_000)),
            Some(120_000)
        );
    }

    #[test]
    fn the_peak_of_a_stream_that_never_spoke_is_nothing() {
        assert_eq!(context_peak_from_line("not json", None), None);
    }

    /// The transcript a triage run actually produces: the model's own text arrives in a `content`
    /// array several events BEFORE the `result`, and any parse that takes the first JSON array it
    /// sees would answer with the model's thinking instead of its verdict.
    #[test]
    fn extract_reply_ignores_content_arrays_before_the_result() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s","tools":[]}
{"type":"assistant","message":{"content":[{"type":"text","text":"[{\"uid\": 1, \"class\": \"noise\"}]"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"reconsidering"}]}}
{"type":"result","subtype":"success","result":"[{\"uid\": 1, \"class\": \"urgent\", \"summary\": \"server down\"}]","total_cost_usd":0.02}"#;

        let reply = extract_reply(stdout).expect("the result event carries the verdict");
        assert!(reply.contains("urgent"), "{reply}");
        assert!(!reply.contains("noise"), "the draft must not win: {reply}");
    }

    fn args_for(policy: ToolPolicy, mcp: Option<&std::path::Path>) -> Vec<String> {
        let mut request = test_run_request("triage this");
        request.mcp_config = mcp.map(std::path::Path::to_path_buf);
        request.tool_policy = policy;
        cli_args(&request, "sonnet")
    }

    fn advertised_tools_from_event(json: &str) -> Option<Vec<String>> {
        let init = serde_json::from_str(json).expect("test init event must be valid JSON");
        advertised_tools_from_init(&init)
    }

    #[test]
    fn an_init_without_tools_fails_a_toolless_policy_run() {
        let advertised = advertised_tools_from_event(r#"{"type":"system","subtype":"init"}"#);

        assert!(advertised_tools_violate(ToolPolicy::None, advertised.as_deref()).is_some());
    }

    #[test]
    fn an_init_without_tools_fails_an_mcp_only_policy_run() {
        let advertised = advertised_tools_from_event(r#"{"type":"system","subtype":"init"}"#);

        assert!(advertised_tools_violate(ToolPolicy::McpOnly, advertised.as_deref()).is_some());
    }

    #[test]
    fn an_explicitly_empty_tool_advertisement_passes_a_toolless_policy_run() {
        let advertised =
            advertised_tools_from_event(r#"{"type":"system","subtype":"init","tools":[]}"#);

        assert!(advertised_tools_violate(ToolPolicy::None, advertised.as_deref()).is_none());
    }

    /// The hole the field-level check cannot see. `advertised_tools_violate` only ever runs inside
    /// the `init` branch, so a CLI that renamed or dropped the event would skip the assertion
    /// entirely and the run would complete looking verified. That is the same failure the assertion
    /// exists to prevent, one level up.
    #[test]
    fn a_restrictive_policy_that_never_saw_an_init_event_is_unverified() {
        for policy in [ToolPolicy::None, ToolPolicy::McpOnly] {
            assert!(
                policy_unverified_after_stream(policy, false).is_some(),
                "{policy:?} must fail closed when no init event arrived"
            );
        }
    }

    #[test]
    fn an_init_event_that_did_arrive_leaves_the_stream_level_check_silent() {
        for policy in [ToolPolicy::None, ToolPolicy::McpOnly] {
            assert!(policy_unverified_after_stream(policy, true).is_none());
        }
    }

    /// `Unrestricted` asserts nothing about tools, so it has nothing to be unable to verify.
    /// Failing it here would break every ordinary run whose transcript happens to lack an `init`.
    #[test]
    fn unrestricted_needs_no_init_event() {
        assert!(policy_unverified_after_stream(ToolPolicy::Unrestricted, false).is_none());
    }

    #[test]
    fn unrestricted_is_unaffected_by_missing_or_empty_tool_advertisements() {
        let missing = advertised_tools_from_event(r#"{"type":"system","subtype":"init"}"#);
        let empty = advertised_tools_from_event(r#"{"type":"system","subtype":"init","tools":[]}"#);

        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, missing.as_deref()).is_none());
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, empty.as_deref()).is_none());
    }

    #[test]
    fn a_toolless_policy_that_receives_tools_fails_the_run() {
        let empty = Vec::new();
        assert!(advertised_tools_violate(ToolPolicy::None, Some(&empty)).is_none());

        let advertised = vec!["Bash".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::None, Some(&advertised)).is_some());
    }

    #[test]
    fn mcp_only_rejects_an_advertised_builtin() {
        let mcp_tools = vec![
            "mcp__nucleos__get_run".to_string(),
            "mcp__nucleos__list_projects".to_string(),
        ];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&mcp_tools)).is_none());

        let mut with_builtin = mcp_tools;
        with_builtin.push("Bash".to_string());
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&with_builtin)).is_some());
    }

    #[test]
    fn advertised_tool_match_is_segment_not_prefix() {
        let valid = vec!["mcp__nucleos__get_run".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&valid)).is_none());

        let nested_server = vec!["mcp__nucleos__x__evil".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&nested_server)).is_some());
    }

    #[test]
    fn unrestricted_accepts_any_advertised_tool_set() {
        let empty = Vec::new();
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, Some(&empty)).is_none());

        let builtins = vec![
            "Read".to_string(),
            "Bash".to_string(),
            "Write".to_string(),
            "Edit".to_string(),
        ];
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, Some(&builtins)).is_none());
    }

    /// An autopilot run keeps the full tool set — the hook and the classifier are what govern it,
    /// and denying tools here would break every real run. What it no longer keeps is the AMBIENT MCP
    /// surface: every server the operator happens to have configured is inherited by a spawned run,
    /// and each one's tool definitions are re-sent in full on every turn. Nothing in the daemon's
    /// design calls those servers, so the run pays for them and gets nothing back.
    ///
    /// The two halves are deliberately different directions: no `--disallowedTools` (the tools this
    /// run may call are the classifier's business), but `--strict-mcp-config` (the servers it
    /// inherits are nobody's).
    #[test]
    fn unrestricted_runs_are_strict_about_mcp_by_default() {
        let args = args_for(ToolPolicy::Unrestricted, None);

        assert!(
            !args.iter().any(|a| a == "--disallowedTools"),
            "an autopilot run keeps its tools; the classifier is what governs them: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--strict-mcp-config"),
            "an ambient MCP server nobody asked for is re-described on every turn: {args:?}"
        );
    }

    /// The escape hatch for the run that genuinely needs an operator's own servers. It is an opt-in
    /// on the request rather than a default, because the cost is paid by every run that did not ask.
    /// It must not drag a tool DENIAL in with it: dropping the strict flag widens which servers the
    /// run inherits, and that is the only thing it is allowed to change.
    #[test]
    fn ambient_mcp_opt_in_drops_the_strict_flag() {
        let mut request = baseline_run_request();
        request.ambient_mcp = true;

        let args = cli_args(&request, "sonnet");

        assert!(
            !args.iter().any(|a| a == "--strict-mcp-config"),
            "a run that asked for the ambient servers must be allowed to see them: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--disallowedTools"),
            "the opt-in governs servers, not tools: {args:?}"
        );
    }

    /// The strict default must not cost the orchestrator the one server the daemon passes in. The
    /// same property `mcp_only_keeps_the_nucleos_server_reachable` holds for `McpOnly`, asserted for
    /// `Unrestricted` because that arm now carries the strict flag too — and this is the pairing
    /// that would break silently, taking every `mcp__nucleos__*` call with it.
    #[test]
    fn the_nucleos_mcp_server_survives_the_strict_default() {
        let args = args_for(
            ToolPolicy::Unrestricted,
            Some(std::path::Path::new("C:/tmp/mcp.json")),
        );

        assert!(
            args.windows(2).any(|w| w[0] == "--mcp-config"),
            "the daemon's own server must still be named: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--strict-mcp-config"),
            "naming a server is not asking for everyone else's: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--allowedTools" && w[1] == "mcp__nucleos__*"),
            "the server is reachable only if its tools are granted: {args:?}"
        );
    }

    /// The dynamic sections of the system prompt (the date among them) change between runs, and the
    /// prompt cache is prefix-matched: one moving token near the front misses the cache for every
    /// token behind it. Excluding them is worth nothing unless the flag itself sits at a STABLE
    /// position, which is why the placement is asserted and not just the presence.
    #[test]
    fn every_run_excludes_dynamic_system_prompt_sections() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        assert!(
            args.iter()
                .any(|a| a == "--exclude-dynamic-system-prompt-sections"),
            "a system prompt that changes every run cannot be cached: {args:?}"
        );
        assert!(
            args.windows(2).any(|pair| pair[0] == "--verbose"
                && pair[1] == "--exclude-dynamic-system-prompt-sections"),
            "the flag must keep a fixed place in the vector, or the prefix it protects moves: \
             {args:?}"
        );
    }

    /// A per-request model is what lets one job run its plan turn on a larger model than its
    /// implement turns. The request wins over the runner's configured default because the runner is
    /// built once at startup and the request is made per run; the reverse ordering would make the
    /// per-run choice unexpressible.
    #[test]
    fn cli_args_prefer_the_request_model_over_the_runner_default() {
        let mut pinned = baseline_run_request();
        pinned.model = Some("claude-haiku-4-5".to_string());
        let pinned_args = cli_args(&pinned, "sonnet");

        assert!(
            pinned_args
                .windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "claude-haiku-4-5"),
            "the request's model must reach the command line: {pinned_args:?}"
        );
        assert!(
            !pinned_args.iter().any(|a| a == "sonnet"),
            "the runner default must not survive alongside it: {pinned_args:?}"
        );

        let unpinned = baseline_run_request();
        assert!(unpinned.model.is_none(), "the baseline must pin nothing");
        let unpinned_args = cli_args(&unpinned, "sonnet");

        assert!(
            unpinned_args
                .windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "sonnet"),
            "a request that pins nothing keeps today's model: {unpinned_args:?}"
        );
    }

    /// Which stages may be routed elsewhere, and — the half that matters — which may not. `plan` and
    /// `review` are read-mostly turns whose output is short; `implement` is the turn that writes the
    /// code, and answering `Some(_)` for it would silently move real work onto whatever model the
    /// file happens to name. `None` for an unnamed stage keeps every existing caller on the runner's
    /// own model.
    #[test]
    fn plan_and_review_stages_get_their_configured_models() {
        let configured = ClaudeCliRunner {
            model: "claude-sonnet-5".to_string(),
            plan_model: Some("claude-opus-4-8".to_string()),
            review_model: Some("claude-haiku-4-5".to_string()),
            resolve_model: Some("opus".to_string()),
            resolve_effort: Some("high".to_string()),
        };

        assert_eq!(
            configured.model_for_stage(Some("plan")).as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            configured.model_for_stage(Some("review")).as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            configured.model_for_stage(Some("implement")),
            None,
            "the turn that writes code is not routable by this file"
        );
        assert_eq!(
            configured.model_for_stage(None),
            None,
            "a run that named no stage keeps the runner's own model"
        );
        assert_eq!(
            configured.model_for_stage(Some(RESOLVE_STAGE)).as_deref(),
            Some("opus")
        );
        assert_eq!(
            configured.effort_for_stage(Some(RESOLVE_STAGE)).as_deref(),
            Some("high")
        );
        for stage in [Some("plan"), Some("review"), Some("implement"), None] {
            assert_eq!(
                configured.effort_for_stage(stage),
                None,
                "only the resolution carries a configured effort: {stage:?}"
            );
        }

        let unconfigured = ClaudeCliRunner {
            model: "claude-sonnet-5".to_string(),
            plan_model: None,
            review_model: None,
            resolve_model: None,
            resolve_effort: None,
        };
        for stage in [
            Some("plan"),
            Some("review"),
            Some("implement"),
            Some(RESOLVE_STAGE),
            None,
        ] {
            assert_eq!(
                unconfigured.model_for_stage(stage),
                None,
                "an unconfigured runner routes nothing anywhere: {stage:?}"
            );
            assert_eq!(
                unconfigured.effort_for_stage(stage),
                None,
                "an unconfigured runner sends no effort anywhere: {stage:?}"
            );
        }
    }

    /// The orchestrator reaches NucleOS through its MCP server and must reach nothing else. This is
    /// the property the `--allowedTools` line alone was wrongly believed to provide.
    #[test]
    fn mcp_only_denies_the_built_in_tools() {
        let args = args_for(ToolPolicy::McpOnly, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        for tool in [
            "Read",
            "Bash",
            "Write",
            "Edit",
            "Task",
            "WebFetch",
            "WebSearch",
            "PowerShell",
        ] {
            assert!(
                denied.split(',').any(|t| t == tool),
                "{tool} must be denied under McpOnly"
            );
        }
    }

    /// `BUILTIN_TOOLS` is a DENYLIST, and the direction is the whole point of this test.
    ///
    /// THREAT_MODEL known gap 7 describes the CLI's own web tools as reachable by cron, repo and
    /// manual runs, and says removing them from this list would "unify the path". Read literally
    /// that is backwards in both halves, and acting on it would be a security regression:
    ///
    /// - Those runs reach the web because `ToolPolicy::Unrestricted` pushes **no tool denial at
    ///   all** — asserted next door in `unrestricted_runs_are_strict_about_mcp_by_default`, which
    ///   also pins the one flag that arm DOES push, and it governs MCP servers rather than tools.
    ///   Their presence in this list has nothing to do with it.
    /// - What this list actually does is DENY them to `McpOnly`, which is what every assistant turn
    ///   runs under. Removing a name from here GRANTS that tool to the surface that reads
    ///   summaries of mail written by strangers.
    ///
    /// Closing the real gap means adding `--disallowedTools WebFetch,WebSearch` to the
    /// `Unrestricted` arm, which is a different edit in a different place. This test exists so that
    /// somebody who reaches for the sentence in the threat model instead meets a red test first.
    #[test]
    fn removing_a_web_tool_from_the_denylist_widens_the_assistant_rather_than_narrowing_a_run() {
        // What the denylist governs: the assistant's surface, and nothing else.
        let mcp_only = args_for(ToolPolicy::McpOnly, None);
        let denied = mcp_only
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        for tool in ["WebFetch", "WebSearch"] {
            assert!(
                denied.split(',').any(|t| t == tool),
                "{tool} must stay denied to assistant turns; deleting it from BUILTIN_TOOLS grants \
                 it to the one surface that reads a stranger's words"
            );
        }

        // And what it does NOT govern: an autonomous run's web access, which no flag here touches.
        // If this ever stops holding, the gap closed somewhere else and the threat model's known
        // gap 7 needs rewriting rather than this test relaxing.
        let unrestricted = args_for(ToolPolicy::Unrestricted, None);
        assert!(
            !unrestricted.iter().any(|a| a == "--disallowedTools"),
            "an Unrestricted run carries no denial, so BUILTIN_TOOLS cannot be what governs it"
        );
    }

    /// The regression that took every assistant turn down. The CLI grew a task-management family
    /// on a version this list had already been measured against; the four names were not denied, so
    /// they were advertised, so `advertised_tools_violate` killed each turn at its `init` event.
    ///
    /// Named one by one rather than by prefix: `Task` was in the list and its relatives were not,
    /// which is exactly the gap a prefix check would paper over. `AskUserQuestion` rides along for
    /// the same reason — a built-in that was never on the list, waiting to break the next turn.
    #[test]
    fn mcp_only_denies_the_task_family_and_the_question_tool() {
        let args = args_for(ToolPolicy::McpOnly, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        for tool in [
            "TaskCreate",
            "TaskGet",
            "TaskList",
            "TaskUpdate",
            "AskUserQuestion",
        ] {
            assert!(
                denied.split(',').any(|t| t == tool),
                "{tool} must be denied under McpOnly, or it is advertised and kills the turn"
            );
        }
    }

    /// The two tests above assert hand-picked subsets — the ones that already cost a dead run — and
    /// that is documentation worth keeping, but it left the other ~31 names in `BUILTIN_TOOLS` with
    /// no assertion at all: a name could fall out of the list on an edit and nothing here would
    /// notice. This iterates the whole const instead, so the list and the flag it produces can never
    /// drift apart silently again.
    #[test]
    fn every_built_in_name_reaches_the_deny_flag() {
        let args = args_for(ToolPolicy::McpOnly, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        let denied: std::collections::HashSet<&str> = denied.split(',').collect();
        for tool in BUILTIN_TOOLS {
            assert!(
                denied.contains(tool),
                "{tool} is in BUILTIN_TOOLS but missing from --disallowedTools: {denied:?}"
            );
        }
    }

    /// Every MCP server the user happens to have configured is ambient to a spawned run, including
    /// file-writing connectors. Only the server the daemon passes in may survive.
    #[test]
    fn mcp_only_drops_the_ambient_mcp_servers() {
        let args = args_for(ToolPolicy::McpOnly, None);
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }

    /// Barrier 1 of spec §5.5. Measured against CLI 2.1.198, a wildcard deny yields an `init` event
    /// advertising no tools at all — the capability is absent, not refused, so a prompt injected
    /// into a mail body has nothing to talk the model into reaching for.
    #[test]
    fn the_no_tools_policy_denies_everything() {
        let args = args_for(ToolPolicy::None, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("the triage policy must deny tools");
        assert_eq!(denied, "*");
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }

    /// The restriction must not cost the orchestrator the one server it exists to call.
    #[test]
    fn mcp_only_keeps_the_nucleos_server_reachable() {
        let args = args_for(
            ToolPolicy::McpOnly,
            Some(std::path::Path::new("C:/tmp/mcp.json")),
        );
        assert!(args.windows(2).any(|w| w[0] == "--mcp-config"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--allowedTools" && w[1] == "mcp__nucleos__*")
        );
    }

    #[test]
    fn a_job_node_request_allows_exactly_the_finding_and_verify_tools() {
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("C:/tmp/job-node.json"));
        request.allowed_mcp_tools = Some(
            crate::mcp_tools::JOB_NODE_TOOLS
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect(),
        );

        let args = cli_args(&request, "claude-sonnet-5");
        let allowed = args
            .windows(2)
            .find(|pair| pair[0] == "--allowedTools")
            .map(|pair| pair[1].as_str())
            .expect("a configured server must carry an allow-list");
        assert_eq!(
            allowed,
            "mcp__nucleos__note_finding,mcp__nucleos__record,mcp__nucleos__verify,\
             mcp__nucleos__verify_status"
        );
        assert_ne!(allowed, "mcp__nucleos__*");
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
    }

    #[test]
    fn a_job_node_server_is_priced_by_its_box() {
        let mut request = baseline_run_request();
        request.tool_policy = ToolPolicy::McpOnly;
        request.mcp_config = Some(PathBuf::from("C:/tmp/job-node.json"));
        request.mcp_job = Some(1);

        let job_price = crate::mcp_tools::NucleosTools::advertised_schema_chars(Some(1));
        assert_eq!(authored_prompt(&request).schema_chars, job_price);
        assert_ne!(
            job_price,
            crate::mcp_tools::NucleosTools::advertised_schema_chars(None)
        );
    }

    #[test]
    fn a_team_node_server_is_priced_by_its_box() {
        let mut request = baseline_run_request();
        request.tool_policy = ToolPolicy::McpOnly;
        request.mcp_config = Some(PathBuf::from("C:/tmp/team-node.json"));
        request.mcp_team_run = Some(7);

        let team_price = crate::mcp_tools::NucleosTools::advertised_box_schema_chars(
            &crate::mcp_tools::McpBox::Team(7),
        );
        assert!(team_price > 0);
        assert_eq!(authored_prompt(&request).schema_chars, team_price);
        assert_ne!(
            team_price,
            crate::mcp_tools::NucleosTools::advertised_schema_chars(None)
        );
    }

    #[tokio::test]
    async fn fake_runner_fails_configured_times_then_succeeds() {
        let runner = FakeCommandRunner {
            fail_times: std::sync::Mutex::new(2),
            ..Default::default()
        };

        for _ in 0..2 {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            assert!(
                runner
                    .run_prompt(test_run_request("x"), tx, discard_transcript())
                    .await
                    .is_err()
            );
        }
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            runner
                .run_prompt(test_run_request("x"), tx, discard_transcript())
                .await
                .is_ok()
        );
        assert_eq!(*runner.calls.lock().unwrap(), 3);
    }

    type SeenBodies = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

    /// Like `ollama_runner_returning`, but keeps every JSON body posted to the chat endpoint.
    ///
    /// The stub used to answer with a canned reply and never look at the request, which meant every
    /// assertion here was about the OUTCOME. An edit that dropped `num_ctx`, dropped the sampling
    /// grammar, or flipped `think` would have left all of them green — so the wire format needs a test
    /// that reads the wire.
    async fn ollama_runner_capturing(answer: &'static str) -> (OllamaRunner, SeenBodies) {
        let seen: SeenBodies = SeenBodies::default();
        let recorder = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/show",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "model_info": {"qwen2.context_length": 8192}
                    }))
                }),
            )
            .fallback(axum::routing::post(
                move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let recorder = recorder.clone();
                    async move {
                        recorder.lock().unwrap().push(body);
                        axum::Json(serde_json::json!({
                            "response": answer,
                            "message": {"role": "assistant", "content": answer},
                            "done": true
                        }))
                    }
                },
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        (
            OllamaRunner::new(format!("http://{address}"), "qwen2".to_string()),
            seen,
        )
    }

    async fn ollama_runner_returning(answer: &'static str) -> OllamaRunner {
        ollama_runner_capturing(answer).await.0
    }

    async fn run_local(
        runner: &OllamaRunner,
        tool_policy: ToolPolicy,
    ) -> std::io::Result<RunOutcome> {
        run_local_prompt(runner, tool_policy, "triage this message").await
    }

    async fn run_local_prompt(
        runner: &OllamaRunner,
        tool_policy: ToolPolicy,
        prompt: &str,
    ) -> std::io::Result<RunOutcome> {
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel();
        // `prompt`, not a fixed string: this helper exists so the batch-bounds and
        // context-contract tests can vary what is sent, and hardcoding it would make both of them
        // assert against a prompt they did not choose.
        let mut request = test_run_request(prompt);
        request.tool_policy = tool_policy;
        runner
            .run_prompt(request, session_tx, discard_transcript())
            .await
    }

    /// Guards the `ollama_chat` extraction, and it has to read the request because every other test
    /// here reads only the outcome.
    ///
    /// Each field asserted is one whose loss is invisible downstream: without `num_ctx` Ollama
    /// truncates the prompt in silence and mail gets filed as unreadable; without the grammar a
    /// malformed answer reaches `parse_verdict`; with `think` on, reasoning tokens land in the
    /// transcript. A refactor that dropped any of them would otherwise ship green.
    #[tokio::test]
    async fn ollama_request_body_carries_ctx_grammar_and_think() {
        let (runner, seen) =
            ollama_runner_capturing(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        run_local(&runner, ToolPolicy::None).await.unwrap();

        let body = seen
            .lock()
            .unwrap()
            .first()
            .cloned()
            .expect("run_prompt posted a chat request");
        assert_eq!(
            body["options"]["num_ctx"].as_u64(),
            Some(crate::governance::triage::LOCAL_NUM_CTX as u64)
        );
        assert_eq!(body["options"]["temperature"].as_u64(), Some(0));
        assert_eq!(body["think"].as_bool(), Some(false));
        assert_eq!(body["stream"].as_bool(), Some(false));
        assert_eq!(body["format"]["type"].as_str(), Some("array"));
        assert_eq!(
            body["format"]["items"]["required"],
            serde_json::json!(["id", "class", "summary"])
        );
    }

    /// The batch-size half of the same contract, on a prompt that actually carries message markers.
    ///
    /// Worth its own test because the fixed `"triage this message"` prompt every other case uses
    /// contains none, so `message_count` is zero and the `maxItems` branch — the one that stops the
    /// model returning verdicts for messages that were not in the batch — never executes.
    #[tokio::test]
    async fn ollama_request_body_bounds_items_to_the_batch() {
        let (runner, seen) =
            ollama_runner_capturing(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        let prompt = "=== BEGIN MESSAGE id=1 ===\nfirst\n=== BEGIN MESSAGE id=2 ===\nsecond";
        run_local_prompt(&runner, ToolPolicy::None, prompt)
            .await
            .unwrap();

        let body = seen
            .lock()
            .unwrap()
            .first()
            .cloned()
            .expect("run_prompt posted a chat request");
        assert_eq!(body["format"]["minItems"].as_u64(), Some(2));
        assert_eq!(body["format"]["maxItems"].as_u64(), Some(2));
    }

    /// A batch bigger than the probed context contract is a programming error, not a case to handle:
    /// the alternative is letting Ollama truncate the tail and filing the messages it never saw as
    /// unreadable.
    #[tokio::test]
    async fn a_batch_beyond_the_context_contract_is_refused() {
        let (runner, _seen) = ollama_runner_capturing("[]").await;

        let prompt = (0..=crate::governance::triage::LOCAL_BATCH_MAX)
            .map(|id| format!("=== BEGIN MESSAGE id={id} ===\nbody"))
            .collect::<Vec<_>>()
            .join("\n");
        let error = run_local_prompt(&runner, ToolPolicy::None, &prompt)
            .await
            .expect_err("a prompt past LOCAL_BATCH_MAX must not reach the model");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// A local inference has no billable session. Leaving the cost unknown is not neutral:
    /// `budget::time_approx` assigns every unknown-cost run a paid sixty-second floor, so free mail
    /// triage would steadily consume the autonomy budget.
    #[tokio::test]
    async fn local_triage_run_reports_zero_cost() {
        let runner =
            ollama_runner_returning(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        let outcome = run_local(&runner, ToolPolicy::None).await.unwrap();

        assert_eq!(outcome.cost_usd, Some(0.0));
        assert_eq!(outcome.session_id, None);
    }

    /// The local model boundary has no tool protocol at all. Accepting a wider policy would turn a
    /// caller mistake into an apparently sandboxed triage run whose actual capability was never
    /// enforced.
    #[tokio::test]
    async fn a_local_runner_refuses_a_tool_policy_other_than_none() {
        let runner =
            ollama_runner_returning(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        for policy in [ToolPolicy::Unrestricted, ToolPolicy::McpOnly] {
            assert!(
                run_local(&runner, policy).await.is_err(),
                "{policy:?} must be rejected before local inference"
            );
        }
    }

    /// Structurally valid JSON can still contain no usable verdict. Treating that as success sends
    /// every unanswered message through the content-failure counter and can permanently file it as
    /// failed after two model answers that said nothing.
    #[tokio::test]
    async fn an_empty_or_summaryless_answer_is_an_infrastructure_failure() {
        for answer in ["[]", r#"[{"id":1,"class":"info","summary":""}]"#] {
            let runner = ollama_runner_returning(answer).await;
            let outcome = run_local(&runner, ToolPolicy::None).await.unwrap();
            assert_ne!(
                outcome.exit_code, 0,
                "an unusable answer must not look like a completed triage run: {answer}"
            );
        }
    }

    /// Ollama's response grammar cannot make a verdict useful: malformed JSON, the wrong top-level
    /// shape, or a missing usable summary must fail at the runner boundary. Otherwise unanswered
    /// mail is counted as a content failure and can be permanently filed as unreadable.
    #[tokio::test]
    async fn a_verdict_without_a_summary_key_is_unusable() {
        for answer in [
            r#"[{"id":1,"class":"info"}]"#,
            r#"[{"id":1,"class":"info","summary":"   "}]"#,
            r#"{"id":1,"class":"info","summary":"x"}"#,
            "not json at all",
        ] {
            let runner = ollama_runner_returning(answer).await;
            let outcome = run_local(&runner, ToolPolicy::None).await.unwrap();

            assert_ne!(
                outcome.exit_code, 0,
                "an unusable local verdict must not look successful: {answer}"
            );
        }
    }

    /// The negative cases carry the safety property: a missing key, a missing model, or a context
    /// one token too small must all fail closed instead of letting Ollama truncate the worst-case
    /// Portuguese mail batch.
    #[test]
    fn the_context_probe_rejects_a_model_that_cannot_hold_the_worst_case() {
        let required_tokens = 8192;
        for response in [
            r#"{"model_info":{"qwen2.context_length":8191}}"#,
            r#"{"error":"model 'qwen2' not found"}"#,
            r#"{"model_info":{}}"#,
            "",
            "{not-json",
        ] {
            let result: Result<(), ModelError> = interpret_context_probe(response, required_tokens);
            assert!(
                result.is_err(),
                "an unsafe or unreadable probe must fail closed: {response:?}"
            );
        }

        let result: Result<(), ModelError> = interpret_context_probe(
            r#"{"model_info":{"qwen2.context_length":8192}}"#,
            required_tokens,
        );
        assert!(result.is_ok());
    }

    /// Startup must fail closed for every unsafe or unreadable probe result: silently substituting
    /// the remote CLI would leak the message bodies that selecting local triage was meant to keep
    /// on the machine.
    #[test]
    fn a_failed_startup_probe_disables_local_triage() {
        assert!(matches!(
            local_triage_decision(Ok(())),
            LocalTriageDecision::Enabled
        ));

        for error in [
            ModelError::ContextTooSmall {
                available_tokens: 4096,
                required_tokens: 8192,
            },
            ModelError::ModelUnavailable("qwen3.5:4b is not installed".to_string()),
            ModelError::MissingModelInfo,
            ModelError::MissingContextLength,
            ModelError::InvalidContextLength,
            ModelError::UnparseableResponse("invalid JSON".to_string()),
            ModelError::AmbiguousContextLength,
        ] {
            match local_triage_decision(Err(error)) {
                LocalTriageDecision::Disabled(reason) => {
                    assert!(
                        !reason.trim().is_empty(),
                        "a disabled local runner needs an operator-facing reason"
                    );
                }
                LocalTriageDecision::Enabled => {
                    panic!("a failed probe must never enable local triage")
                }
            }
        }

        let LocalTriageDecision::Disabled(reason) =
            local_triage_decision(Err(ModelError::ContextTooSmall {
                available_tokens: 4096,
                required_tokens: 8192,
            }))
        else {
            panic!("an undersized context must disable local triage");
        };
        assert!(reason.contains("4096"), "{reason}");
        assert!(reason.contains("8192"), "{reason}");
    }

    /// Multimodal model metadata can advertise several context lengths. Selecting the first key
    /// lets an unrelated projector either reject a safe model or bless an unsafe one, while
    /// accepting ambiguous metadata makes a safety claim the probe cannot support.
    #[test]
    fn the_probe_picks_the_context_of_the_declared_architecture() {
        let required_tokens = 8192;

        let language_model = interpret_context_probe(
            r#"{"model_info":{"general.architecture":"qwen2","clip.context_length":77,"qwen2.context_length":32768}}"#,
            required_tokens,
        );
        assert!(
            language_model.is_ok(),
            "the declared qwen2 architecture has enough context: {language_model:?}"
        );

        let vision_model = interpret_context_probe(
            r#"{"model_info":{"general.architecture":"clip","clip.context_length":77,"qwen2.context_length":32768}}"#,
            required_tokens,
        );
        assert!(
            vision_model.is_err(),
            "the declared clip architecture is too small"
        );

        let ambiguous = interpret_context_probe(
            r#"{"model_info":{"clip.context_length":77,"qwen2.context_length":32768}}"#,
            required_tokens,
        );
        assert!(
            ambiguous.is_err(),
            "multiple context lengths without an architecture must fail closed"
        );
    }

    /// The `codex exec` launch surface, asserted the way `cli_args` is: purely, so the flags that
    /// decide which model answers and where it is allowed to work are checked here rather than
    /// inspected on a live process.
    ///
    /// `--skip-git-repo-check` is what lets a run start at all in a directory the CLI would
    /// otherwise refuse, and `-C` is the only thing keeping a run inside the project it was spawned
    /// for — a vector missing it runs wherever the daemon happens to have been launched.
    #[test]
    fn codex_cli_args_carry_the_model_and_the_working_directory() {
        let mut request = baseline_run_request();
        request.cwd = Some(std::path::PathBuf::from("C:/work/repo"));

        let args = codex_cli_args(&request, "gpt-5.6-terra", &CodexStaged::default())
            .expect("a baseline request asks for nothing the tool cannot honour");

        assert_eq!(
            args.first().map(String::as_str),
            Some("exec"),
            "the non-interactive subcommand leads the vector: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == "--skip-git-repo-check"),
            "a run must not be refused for the shape of the directory it works in: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-m" && pair[1] == "gpt-5.6-terra"),
            "the model must immediately follow -m: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-C" && pair[1] == "C:/work/repo"),
            "the working directory must immediately follow -C: {args:?}"
        );

        let directoryless = baseline_run_request();
        assert!(
            directoryless.cwd.is_none(),
            "the baseline must name no directory"
        );
        let directoryless_args =
            codex_cli_args(&directoryless, "gpt-5.6-terra", &CodexStaged::default())
                .expect("a request without a directory is still honourable");
        assert!(
            !directoryless_args.iter().any(|arg| arg == "-C"),
            "an absent cwd must not invent a directory: {directoryless_args:?}"
        );
    }

    /// A routed run names its model on the request, and the Codex launch must run THAT model rather
    /// than the one the runner was built with. Without this the router's advice would be recorded
    /// as applied while the configured model answered.
    #[test]
    fn codex_launch_uses_the_requested_model_over_its_own() {
        let mut request = baseline_run_request();
        request.model = None;
        assert_eq!(codex_model(&request, "cfg-model"), "cfg-model");

        request.model = Some("gpt-x".into());
        assert_eq!(codex_model(&request, "cfg-model"), "gpt-x");

        let args = codex_cli_args(
            &request,
            codex_model(&request, "cfg-model"),
            &CodexStaged::default(),
        )
        .expect("a baseline request asks for nothing the tool cannot honour");
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-m" && pair[1] == "gpt-x"),
            "the requested model must follow -m: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "cfg-model"),
            "the configured model must not reach the vector: {args:?}"
        );
    }

    #[test]
    fn codex_cli_args_ask_for_the_json_event_stream() {
        let fresh = codex_cli_args(
            &baseline_run_request(),
            "gpt-5.6-terra",
            &CodexStaged::default(),
        )
        .unwrap();
        assert!(fresh.iter().any(|arg| arg == "--json"), "{fresh:?}");

        let mut resumed = baseline_run_request();
        resumed.resume_session_id = Some("sess-1".to_string());
        let resumed = codex_cli_args(&resumed, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        assert!(resumed.iter().any(|arg| arg == "--json"), "{resumed:?}");
    }

    #[test]
    fn codex_cli_args_resume_a_session_through_the_resume_subcommand() {
        let mut request = baseline_run_request();
        request.resume_session_id = Some("sess-1".to_string());
        request.cwd = Some(PathBuf::from("C:/work/repo"));
        request.add_dirs = vec![PathBuf::from("C:/work/other")];

        let args = codex_cli_args(&request, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        assert_eq!(&args[..2], ["exec", "resume"], "{args:?}");
        assert!(!args.iter().any(|arg| arg == "-C"), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "--add-dir"), "{args:?}");
        assert_eq!(
            &args[args.len() - 2..],
            ["sess-1", "test prompt"],
            "{args:?}"
        );
    }

    #[test]
    fn codex_cli_args_carry_effort_as_a_reasoning_override() {
        let mut request = baseline_run_request();
        request.effort = Some("high".to_string());
        let args = codex_cli_args(&request, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| { pair[0] == "-c" && pair[1] == r#"model_reasoning_effort="high""# }),
            "{args:?}"
        );

        let without_effort = codex_cli_args(
            &baseline_run_request(),
            "gpt-5.6-terra",
            &CodexStaged::default(),
        )
        .unwrap();
        assert!(
            !without_effort
                .iter()
                .any(|arg| arg.contains("model_reasoning_effort")),
            "{without_effort:?}"
        );
    }

    #[test]
    fn codex_cli_args_carry_extra_dirs_on_both_launch_shapes() {
        let directories = vec![PathBuf::from("C:/work/a"), PathBuf::from("C:/work/b")];
        let mut fresh = baseline_run_request();
        fresh.add_dirs = directories.clone();
        let fresh = codex_cli_args(&fresh, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        for directory in ["C:/work/a", "C:/work/b"] {
            assert!(
                fresh
                    .windows(2)
                    .any(|pair| pair[0] == "--add-dir" && pair[1] == directory),
                "{fresh:?}"
            );
        }

        let mut resumed = baseline_run_request();
        resumed.resume_session_id = Some("sess-1".to_string());
        resumed.add_dirs = directories;
        let resumed = codex_cli_args(&resumed, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        let roots = serde_json::to_string(&["C:/work/a", "C:/work/b"]).unwrap();
        assert!(
            resumed.windows(2).any(|pair| {
                pair[0] == "-c"
                    && pair[1] == format!("sandbox_workspace_write.writable_roots={roots}")
            }),
            "{resumed:?}"
        );
        assert!(!resumed.iter().any(|arg| arg == "--add-dir"), "{resumed:?}");
    }

    /// An explicit override beats the user's `~/.codex/config.toml`, which exec's own default does
    /// not; this is `-c` because `codex exec resume` has no `-s`.
    #[test]
    fn codex_cli_args_pin_the_sandbox_on_both_launch_shapes() {
        let fresh_request = baseline_run_request();
        let mut resumed_request = baseline_run_request();
        resumed_request.resume_session_id =
            Some("123e4567-e89b-42d3-a456-426614174001".to_string());
        let pinned = CodexStaged {
            sandbox_mode: Some("read-only"),
            ..CodexStaged::default()
        };

        let fresh = codex_cli_args(&fresh_request, "gpt-5.6-terra", &pinned).unwrap();
        let resumed = codex_cli_args(&resumed_request, "gpt-5.6-terra", &pinned).unwrap();
        let sandbox = r#"sandbox_mode="read-only""#;

        for args in [&fresh, &resumed] {
            assert_eq!(
                args.windows(2)
                    .filter(|pair| pair[0] == "-c" && pair[1] == sandbox)
                    .count(),
                1,
                "{args:?}"
            );
            let sandbox_index = args
                .windows(2)
                .position(|pair| pair[0] == "-c" && pair[1] == sandbox)
                .unwrap();
            assert!(sandbox_index + 1 < args.len() - 1, "{args:?}");
        }
        let resumed_sandbox_index = resumed
            .windows(2)
            .position(|pair| pair[0] == "-c" && pair[1] == sandbox)
            .unwrap();
        let session_index = resumed
            .iter()
            .position(|arg| arg == "123e4567-e89b-42d3-a456-426614174001")
            .unwrap();
        assert!(resumed_sandbox_index + 1 < session_index, "{resumed:?}");

        for request in [&fresh_request, &resumed_request] {
            let args = codex_cli_args(request, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
            assert!(
                !args.iter().any(|arg| arg.starts_with("sandbox_mode=")),
                "{args:?}"
            );
        }
    }

    /// The owner decided a Codex chat turn runs read-only; the run runner in `main.rs` keeps `None`
    /// so a run keeps the user's Codex configuration.
    #[test]
    fn codex_chat_runner_pins_the_read_only_sandbox() {
        let runner = CodexCliRunner::for_chat("gpt-5.5".to_string());

        assert_eq!(runner.model, "gpt-5.5");
        assert_eq!(runner.sandbox_mode, Some("read-only"));
    }

    #[test]
    fn codex_cli_args_attach_images_without_swallowing_the_prompt() {
        let mut request = baseline_run_request();
        request.images = vec![
            Attachment {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            },
            Attachment {
                media_type: "image/jpeg".to_string(),
                data: "d29ybGQ=".to_string(),
            },
        ];
        let staged = CodexStaged {
            mcp_overrides: Vec::new(),
            images: vec![
                PathBuf::from("C:/stage/one.png"),
                PathBuf::from("C:/stage/two.jpg"),
            ],
            sandbox_mode: None,
        };
        let args = codex_cli_args(&request, "gpt-5.6-terra", &staged).unwrap();
        for image in ["C:/stage/one.png", "C:/stage/two.jpg"] {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "-i" && pair[1] == image),
                "{args:?}"
            );
            assert!(
                args.windows(3).any(|triple| triple[0] == "-i"
                    && triple[1] == image
                    && triple[2].starts_with('-')),
                "{args:?}"
            );
        }
        assert_eq!(
            args.last().map(String::as_str),
            Some("test prompt"),
            "{args:?}"
        );

        let too_few = CodexStaged {
            mcp_overrides: Vec::new(),
            images: vec![PathBuf::from("C:/stage/one.png")],
            sandbox_mode: None,
        };
        let error = codex_cli_args(&request, "gpt-5.6-terra", &too_few).unwrap_err();
        assert!(error.contains("images"), "{error}");
    }

    #[test]
    fn codex_mcp_overrides_translate_the_daemons_mcp_config() {
        let config = serde_json::json!({"mcpServers":{"nucleos":{"type":"stdio","command":"C:/x/nucleos-core.exe","args":["--mcp-tools","a"]}}});
        let env_names = vec![
            "NUCLEOS_DAEMON_TOKEN".to_string(),
            "NUCLEOS_DAEMON_URL".to_string(),
        ];
        let overrides = codex_mcp_overrides(&config, &env_names).unwrap();
        assert_eq!(
            overrides,
            vec![
                r#"mcp_servers.nucleos.command="C:/x/nucleos-core.exe""#.to_string(),
                r#"mcp_servers.nucleos.args=["--mcp-tools","a"]"#.to_string(),
                r#"mcp_servers.nucleos.env_vars=["NUCLEOS_DAEMON_TOKEN","NUCLEOS_DAEMON_URL"]"#
                    .to_string(),
                r#"mcp_servers.nucleos.default_tools_approval_mode="approve""#.to_string(),
            ]
        );

        let mut request = baseline_run_request();
        request.env = vec![(
            "NUCLEOS_DAEMON_TOKEN".to_string(),
            "secret-token-value".to_string(),
        )];
        let args = codex_cli_args(
            &request,
            "gpt-5.6-terra",
            &CodexStaged {
                mcp_overrides: overrides.clone(),
                images: Vec::new(),
                sandbox_mode: None,
            },
        )
        .unwrap();
        for override_value in overrides {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "-c" && pair[1] == override_value),
                "{args:?}"
            );
        }
        assert!(
            !args.iter().any(|arg| arg.contains("secret-token-value")),
            "{args:?}"
        );
    }

    #[test]
    fn a_codex_job_node_carries_the_box_in_its_server_args() {
        let config = crate::assistant::build_job_node_mcp_config("C:/x/n.exe", 7);
        let overrides = codex_mcp_overrides(&config, &[]).unwrap();

        assert!(overrides.iter().any(|value| {
            value == r#"mcp_servers.nucleos.args=["--mcp-tools","--box","job-node","--job","7"]"#
        }));
    }

    /// `codex exec` declines an unconfirmed MCP call; Claude pre-approves these same tools.
    #[test]
    fn codex_mcp_overrides_pre_approve_the_daemons_own_tools() {
        let config = serde_json::json!({"mcpServers":{"nucleos":{"type":"stdio","command":"C:/x/nucleos-core.exe","args":["--mcp-tools"]}}});
        let overrides = codex_mcp_overrides(&config, &[]).unwrap();
        assert_eq!(
            overrides,
            vec![
                r#"mcp_servers.nucleos.command="C:/x/nucleos-core.exe""#.to_string(),
                r#"mcp_servers.nucleos.args=["--mcp-tools"]"#.to_string(),
                r#"mcp_servers.nucleos.default_tools_approval_mode="approve""#.to_string(),
            ]
        );
    }

    #[test]
    fn codex_mcp_overrides_refuse_a_config_they_cannot_express() {
        let cases = [
            serde_json::json!({}),
            serde_json::json!({"mcpServers": []}),
            serde_json::json!({"mcpServers":{"nucleos":{"type":"http","command":"C:/x"}}}),
            serde_json::json!({"mcpServers":{"not nucleos":{"type":"stdio","command":"C:/x"}}}),
            serde_json::json!({"mcpServers":{"nucleos":{"type":"stdio","command":3}}}),
        ];
        for config in cases {
            let error = codex_mcp_overrides(&config, &[]).unwrap_err();
            assert!(error.contains("mcp_config"), "{error}");
        }
    }

    #[tokio::test]
    async fn the_codex_runner_refuses_an_mcp_config_it_cannot_read() {
        let runner = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
            sandbox_mode: None,
        };
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("C:/does-not-exist/mcp.json"));
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let error = runner
            .run_prompt(
                request,
                session_tx,
                std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("mcp_config"), "{error}");
        assert!(
            !error.contains("cannot honour mcp_config: it has no flag"),
            "{error}"
        );
    }

    #[test]
    fn codex_thread_id_is_read_from_the_thread_started_event() {
        assert_eq!(
            codex_thread_id(r#"{"type":"thread.started","thread_id":"t-123"}"#),
            Some("t-123".to_string())
        );
        assert_eq!(codex_thread_id(r#"{"type":"turn.completed"}"#), None);
        assert_eq!(codex_thread_id("not json"), None);
    }

    #[test]
    fn extract_reply_reads_a_codex_agent_message() {
        let codex = r#"{"type":"item.completed","item":{"type":"agent_message","text":"draft"}}
{"type":"item.completed","item":{"type":"agent_message","text":"final"}}"#;
        assert_eq!(extract_reply(codex), Some("final".to_string()));
        assert_eq!(
            extract_reply(r#"{"type":"result","result":"olá"}"#),
            Some("olá".to_string())
        );
    }

    #[test]
    fn codex_cli_args_accept_a_one_turn_steerable_request() {
        let mut one_turn = baseline_run_request();
        one_turn.steerable = true;
        assert!(codex_cli_args(&one_turn, "gpt-5.6-terra", &CodexStaged::default()).is_ok());

        let mut live = baseline_run_request();
        live.steerable = true;
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        live.messages = Some(rx);
        let error = codex_cli_args(&live, "gpt-5.6-terra", &CodexStaged::default()).unwrap_err();
        assert!(error.contains("steerable"), "{error}");
    }

    #[test]
    fn codex_images_are_staged_as_files_the_cli_can_read() {
        let temp = tempfile::TempDir::new().unwrap();
        let images = vec![
            Attachment {
                media_type: "image/png".to_string(),
                data: "cG5n".to_string(),
            },
            Attachment {
                media_type: "image/jpeg".to_string(),
                data: "anBlZw==".to_string(),
            },
        ];
        let staged = stage_codex_images(temp.path(), "stem", &images).unwrap();
        assert_eq!(staged.len(), 2, "{staged:?}");
        assert!(staged[0].ends_with("stem-0.png"), "{staged:?}");
        assert!(staged[1].ends_with("stem-1.jpg"), "{staged:?}");
        assert_eq!(std::fs::read(&staged[0]).unwrap(), b"png");
        assert_eq!(std::fs::read(&staged[1]).unwrap(), b"jpeg");

        let pdf = [Attachment {
            media_type: "application/pdf".to_string(),
            data: "cGRm".to_string(),
        }];
        let error = stage_codex_images(temp.path(), "stem", &pdf).unwrap_err();
        assert!(error.to_string().contains("images"), "{error}");
    }

    /// A control this tool cannot honour must fail the launch instead of vanishing from it.
    ///
    /// `fork_session` and `steerable` each change what a run IS, not how it is decorated: a fork
    /// continues someone else's session rather than starting a new one, and a steerable run promises
    /// a stdin that later turns can arrive on. Building a vector that silently omits either hands the
    /// caller a different run than the one it asked for — a run that answers once and then ignores
    /// every steering message, or one that loses the history it was supposed to branch from — and
    /// `runs.rs` records that as a completed run. The refusal names the flag so an operator reading
    /// the failure learns which request cannot take this cheaper path.
    #[test]
    fn codex_cli_args_refuse_what_the_tool_cannot_honour() {
        let honourable = baseline_run_request();
        assert!(
            codex_cli_args(&honourable, "gpt-5.6-terra", &CodexStaged::default()).is_ok(),
            "the control case must build, or a refusal proves nothing"
        );

        let mut forked = baseline_run_request();
        forked.fork_session = true;
        let forked_refusal = codex_cli_args(&forked, "gpt-5.6-terra", &CodexStaged::default())
            .expect_err("a forked session cannot be honoured here");
        assert!(
            forked_refusal.contains("fork_session"),
            "the refusal must name what it could not honour: {forked_refusal}"
        );

        let mut steerable = baseline_run_request();
        steerable.steerable = true;
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        steerable.messages = Some(rx);
        let steerable_refusal =
            codex_cli_args(&steerable, "gpt-5.6-terra", &CodexStaged::default())
                .expect_err("a steerable run cannot be honoured here");
        assert!(
            steerable_refusal.contains("steerable"),
            "the refusal must name what it could not honour: {steerable_refusal}"
        );
    }

    /// Barrier 1 of the tool model is the CLI's OWN refusal, and `codex exec` cannot perform it: it
    /// has no flag that denies a tool and no init event advertising which ones survived, so a
    /// restrictive policy could be neither applied at launch nor verified from the stream. A run
    /// started anyway would hold every tool while `runs.rs` recorded the restriction as honoured —
    /// and `ToolPolicy::None` is what lets a run read a stranger's mail at all.
    ///
    /// Asserted against `run_prompt` rather than `codex_cli_args`, because the refusal lives in the
    /// runner and the arg builder never sees the policy. Cheap for the same reason it is safe: it
    /// returns before the spawn, so no `codex` binary has to exist for this to run.
    #[tokio::test]
    async fn the_codex_runner_refuses_a_restricted_tool_policy() {
        let runner = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
            sandbox_mode: None,
        };

        // `McpOnly` left this list: a Codex chat turn applies it with feature flags and a deny
        // hook (`codex_cli_args`), so only the policy that cannot be expressed is still refused.
        for policy in [ToolPolicy::None] {
            let mut request = baseline_run_request();
            request.tool_policy = policy;
            let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

            let refusal = runner
                .run_prompt(request, session_tx, std::sync::Arc::clone(&transcript))
                .await
                .expect_err("a policy the tool cannot apply must not reach a launch");

            assert!(
                refusal.to_string().contains(&format!("{policy:?}")),
                "the refusal must name the policy it could not honour: {refusal}"
            );
            assert!(
                transcript.lock().unwrap().is_empty(),
                "{policy:?}: a refused run must produce no transcript, because nothing ran"
            );
            assert!(
                session_rx.try_recv().is_err(),
                "{policy:?}: a run that never launched must not announce a session"
            );
        }

        // The control, without which a runner that refused EVERY policy would pass the loop above
        // while quietly making the second CLI unusable.
        //
        // `Unrestricted` plus a request the arg builder rejects: the run gets past the policy gate
        // and dies one step later, on `fork_session`. That is the whole point of choosing this
        // request — a control that only set `Unrestricted` would have to spawn `codex` to prove
        // anything, and a unit test must not start an agent on whatever machine it runs on.
        let mut allowed = baseline_run_request();
        allowed.tool_policy = ToolPolicy::Unrestricted;
        allowed.fork_session = true;
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let stopped_later = runner
            .run_prompt(
                allowed,
                session_tx,
                std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            )
            .await
            .expect_err("this control request is refused by the arg builder, not by the policy")
            .to_string();

        assert!(
            stopped_later.contains("fork_session"),
            "an unrestricted run must reach the arg builder: {stopped_later}"
        );
        assert!(
            !stopped_later.contains("ToolPolicy"),
            "the tool policy must not be what stops an unrestricted run: {stopped_later}"
        );

        // The same control for `McpOnly`, which now passes the policy gate and is stopped one
        // step later by the arg builder on `fork_session` (so no `codex` has to be spawned).
        let mut mcp_only = baseline_run_request();
        mcp_only.tool_policy = ToolPolicy::McpOnly;
        mcp_only.fork_session = true;
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mcp_only_stopped_later = runner
            .run_prompt(
                mcp_only,
                session_tx,
                std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            )
            .await
            .expect_err("this control request is refused by the arg builder, not by the policy")
            .to_string();

        assert!(
            mcp_only_stopped_later.contains("fork_session"),
            "an McpOnly run must reach the arg builder: {mcp_only_stopped_later}"
        );
        assert!(
            !mcp_only_stopped_later.contains("ToolPolicy"),
            "the tool policy must not be what stops an McpOnly run: {mcp_only_stopped_later}"
        );
    }

    /// The features an `McpOnly` Codex turn switches off, spelled as the `-c` override each one is.
    fn mcp_only_feature_overrides() -> Vec<String> {
        [
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
        ]
        .iter()
        .map(|feature| format!("features.{feature}=false"))
        .collect()
    }

    /// An `McpOnly` Codex turn (a chat with no project) must leave the model nothing but the
    /// daemon's own MCP tools. Codex has no allow-list flag, so every built-in is switched off one
    /// by one with `-c` (not `--disable`, because `codex exec resume` lacks several exec flags), the
    /// user's config is ignored, and a deny hook covers what has no feature flag. All of it sits
    /// before the session id and the prompt, on both launch shapes.
    #[test]
    fn codex_mcp_only_args_disable_every_builtin_feature() {
        let mut fresh_request = baseline_run_request();
        fresh_request.tool_policy = ToolPolicy::McpOnly;
        let mut resumed_request = baseline_run_request();
        resumed_request.tool_policy = ToolPolicy::McpOnly;
        resumed_request.resume_session_id =
            Some("123e4567-e89b-42d3-a456-426614174002".to_string());

        let fresh = codex_cli_args(&fresh_request, "gpt-5.5", &CodexStaged::default()).unwrap();
        let resumed = codex_cli_args(&resumed_request, "gpt-5.5", &CodexStaged::default()).unwrap();

        for args in [&fresh, &resumed] {
            // Everything the hardening adds must precede the trailing session id and prompt.
            let tail = if args.iter().any(|arg| arg == "resume") {
                2
            } else {
                1
            };
            let flags = &args[..args.len() - tail];

            for flag in ["--ignore-user-config", "--dangerously-bypass-hook-trust"] {
                assert_eq!(
                    flags.iter().filter(|arg| *arg == flag).count(),
                    1,
                    "{flag} must appear once, before the session id and prompt: {args:?}"
                );
            }
            for feature in mcp_only_feature_overrides() {
                assert!(
                    flags
                        .windows(2)
                        .any(|pair| pair[0] == "-c" && pair[1] == feature),
                    "{feature} must be switched off through -c: {args:?}"
                );
            }
            assert!(
                flags
                    .windows(2)
                    .any(|pair| pair[0] == "-c" && pair[1] == r#"web_search="disabled""#),
                "web search must be disabled: {args:?}"
            );
            let hooks: Vec<&String> = flags
                .windows(2)
                .filter(|pair| pair[0] == "-c" && pair[1].starts_with("hooks.PreToolUse="))
                .map(|pair| &pair[1])
                .collect();
            assert_eq!(hooks.len(), 1, "exactly one deny hook override: {args:?}");
            let hook = hooks[0];
            assert!(
                hook.contains("matcher=") && hook.contains("commandWindows="),
                "the hook carries a matcher, a Unix command and a Windows command: {hook}"
            );
            assert_eq!(
                hook,
                &codex_deny_hook_override(DAEMON_MCP_TOOL_PREFIX),
                "the hook is the one the generator builds for the daemon's prefix"
            );
            assert_eq!(
                args.last().map(String::as_str),
                Some("test prompt"),
                "the prompt stays the trailing positional: {args:?}"
            );
        }
        assert_eq!(
            &resumed[resumed.len() - 2..],
            ["123e4567-e89b-42d3-a456-426614174002", "test prompt"],
            "{resumed:?}"
        );
    }

    /// The hardening is keyed on the policy and nothing else: an unrestricted turn (a run, or a
    /// rooted chat) keeps exactly the argv it had before, and the constants the policy adds stay in
    /// one place so a typo cannot hide in a test.
    #[test]
    fn codex_unrestricted_args_carry_no_mcp_only_hardening() {
        let mut resumed_request = baseline_run_request();
        resumed_request.resume_session_id =
            Some("123e4567-e89b-42d3-a456-426614174003".to_string());
        for request in [baseline_run_request(), resumed_request] {
            assert_eq!(request.tool_policy, ToolPolicy::Unrestricted);
            let args = codex_cli_args(&request, "gpt-5.5", &CodexStaged::default()).unwrap();
            for forbidden in [
                "--ignore-user-config",
                "--dangerously-bypass-hook-trust",
                "--disable",
            ] {
                assert!(
                    !args.iter().any(|arg| arg == forbidden),
                    "{forbidden} belongs to McpOnly only: {args:?}"
                );
            }
            assert!(
                !args.iter().any(|arg| arg.starts_with("features.")
                    || arg.starts_with("hooks.")
                    || arg.starts_with("web_search")),
                "no McpOnly override may reach an unrestricted launch: {args:?}"
            );
            assert!(
                !args.iter().any(|arg| arg.starts_with("sandbox_mode=")),
                "an unrestricted launch pins no sandbox of its own: {args:?}"
            );
        }
    }

    /// The deny hook is a regex over tool names. Codex names an MCP tool `mcp__<server>__<tool>`, so
    /// the matcher must hit everything that is not under the daemon's own prefix and miss every
    /// daemon tool. It is built without lookahead, so it behaves the same under search and under
    /// full-match semantics, which is why both are asserted here.
    #[test]
    fn codex_deny_matcher_denies_everything_but_the_daemons_tools() {
        let matcher = codex_deny_matcher(DAEMON_MCP_TOOL_PREFIX);
        let pattern = regex::Regex::new(&matcher).expect("the matcher must be a valid regex");
        let full = regex::Regex::new(&format!("^(?:{matcher})$")).unwrap();

        for denied in [
            "Bash",
            "apply_patch",
            "view_image",
            "update_plan",
            "spawn_agent",
            "list_mcp_resources",
            "mcp__other__x",
            "mcp__nucleo",
            "mcp__nucleosX__y",
        ] {
            assert!(
                pattern.is_match(denied),
                "{denied} must be denied: {matcher}"
            );
            assert!(full.is_match(denied), "{denied} must be denied: {matcher}");
        }
        for allowed in [
            "mcp__nucleos__send_to_chat",
            "mcp__nucleos__anything_at_all",
        ] {
            assert!(
                !pattern.is_match(allowed),
                "{allowed} is the daemon's own tool and must pass: {matcher}"
            );
            assert!(
                !full.is_match(allowed),
                "{allowed} is the daemon's own tool and must pass: {matcher}"
            );
        }
    }

    /// A hook that cannot start fails OPEN (only exit code 2 blocks), so the commands are plain
    /// `echo` + `exit 2` builtins with no interpreter, and the reason carries no character either
    /// shell would read as syntax.
    #[test]
    fn codex_deny_hook_commands_are_shell_inert() {
        let reason = CODEX_MCP_ONLY_DENIAL;
        assert!(!reason.is_empty());
        for metacharacter in [
            '&', '|', ';', '<', '>', '`', '$', '"', '\'', '\\', '(', ')', '%', '^', '!', '\n',
        ] {
            assert!(
                !reason.contains(metacharacter),
                "the denial reason must not contain {metacharacter:?}: {reason}"
            );
        }

        let hook = codex_deny_hook_override(DAEMON_MCP_TOOL_PREFIX);
        assert!(hook.starts_with("hooks.PreToolUse=["), "{hook}");
        let unix = serde_json::to_string(&format!("echo {reason} >&2; exit 2")).unwrap();
        let windows =
            serde_json::to_string(&format!("[Console]::Error.WriteLine('{reason}'); exit 2"))
                .unwrap();
        assert!(
            hook.contains(&format!("command={unix}")),
            "the Unix command must be `echo <reason> >&2; exit 2`: {hook}"
        );
        // Codex runs Windows hooks under PowerShell, not cmd: cmd syntax there is a parse error,
        // which fails open.
        assert!(
            hook.contains(&format!("commandWindows={windows}")),
            "the Windows command must be PowerShell `[Console]::Error.WriteLine('<reason>'); exit 2`: {hook}"
        );
        assert!(
            hook.contains(r#"type="command""#),
            "the hook is a command hook: {hook}"
        );
    }

    /// `for_chat` pins read-only, but a daemon whose primary runner is Codex builds the runner
    /// without a pinned sandbox, and a no-project chat must not inherit the user's config there.
    /// So the policy itself pins it when the runner does not, and the runner's own pin still wins.
    #[test]
    fn codex_mcp_only_pins_read_only_when_the_runner_pins_nothing() {
        let sandbox = r#"sandbox_mode="read-only""#;
        let mut mcp_only = baseline_run_request();
        mcp_only.tool_policy = ToolPolicy::McpOnly;
        let mut resumed = baseline_run_request();
        resumed.tool_policy = ToolPolicy::McpOnly;
        resumed.resume_session_id = Some("123e4567-e89b-42d3-a456-426614174004".to_string());

        for request in [&mcp_only, &resumed] {
            let args = codex_cli_args(request, "gpt-5.5", &CodexStaged::default()).unwrap();
            assert_eq!(
                args.windows(2)
                    .filter(|pair| pair[0] == "-c" && pair[1] == sandbox)
                    .count(),
                1,
                "McpOnly pins read-only even with an unpinned runner: {args:?}"
            );

            let pinned = CodexStaged {
                sandbox_mode: Some("read-only"),
                ..CodexStaged::default()
            };
            let args = codex_cli_args(request, "gpt-5.5", &pinned).unwrap();
            assert_eq!(
                args.windows(2)
                    .filter(|pair| pair[0] == "-c" && pair[1] == sandbox)
                    .count(),
                1,
                "a runner that already pins read-only must not double it: {args:?}"
            );
        }

        let unrestricted =
            codex_cli_args(&baseline_run_request(), "gpt-5.5", &CodexStaged::default()).unwrap();
        assert!(
            !unrestricted
                .iter()
                .any(|arg| arg.starts_with("sandbox_mode=")),
            "only McpOnly pins a sandbox on its own: {unrestricted:?}"
        );
    }

    /// The other controls `codex exec` has no counterpart for. Each was accepted and dropped, which
    /// is the one outcome a control must never have: the caller was told the run it asked for
    /// started, and the record agreed.
    ///
    /// `permission` is why this is a refusal rather than a warning. `Plan` is how a run is made
    /// unable to act — a catch-up run, recovering a schedule the machine slept through, is forced
    /// into it precisely because nobody chose for it to run now — so a runner that ignores it
    /// converts a deliberately restrained run into an unrestrained one, in the one case where the
    /// operator is not watching. `Bypass` is refused for the opposite reason and it is the sharper
    /// of the two: it stands the CLI's permission barrier down on the strength of a `PreToolUse`
    /// hook this launch surface has never heard of.
    ///
    /// `mcp_config` and `resume_session_id` are deliberately NOT here, because both are honoured now:
    /// the MCP config becomes `-c mcp_servers.<name>...` overrides (see `codex_mcp_overrides` and its
    /// tests), and a resumed run continues its own thread through `codex exec resume`.
    ///
    /// Asserted against `run_prompt` rather than `codex_cli_args`, because that builder's purity
    /// contract is frozen. Cheap for the same reason it is safe: every case returns before the
    /// spawn, so no `codex` binary has to exist for this to run.
    #[tokio::test]
    async fn the_codex_runner_refuses_flags_it_cannot_honour() {
        let runner = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
            sandbox_mode: None,
        };

        let mut restrained = baseline_run_request();
        restrained.permission = Permission::Plan;
        // The other direction, and the one that fails open rather than closed: honouring it would
        // mean running unbarriered where the daemon believes a classifier took over.
        let mut unbarriered = baseline_run_request();
        unbarriered.permission = Permission::Bypass;
        // The per-conversation controls of 0110–0113. Each of these is drawn back at the person in
        // the window as a setting their conversation has, so a launch that dropped one would leave
        // the row claiming something the run never had.
        let mut degrading = baseline_run_request();
        degrading.fallback_model = vec!["opus".to_string()];
        let mut capped = baseline_run_request();
        capped.max_budget_usd = Some(0.5);
        let mut helped = baseline_run_request();
        helped.agents = vec![a_helper("reviewer")];
        let mut instructed = baseline_run_request();
        instructed.append_system_prompt = Some("Answer in Portuguese.".to_string());
        // The sharpest of them: it passes the `ToolPolicy` guard, because a conversation can be
        // unrestricted and still have barred a tool for itself.
        let mut barred = baseline_run_request();
        barred.denied_tools = vec!["Bash".to_string()];

        for (field, request) in [
            ("Permission::Plan", restrained),
            ("Permission::Bypass", unbarriered),
            ("fallback_model", degrading),
            ("max_budget_usd", capped),
            ("agents", helped),
            ("append_system_prompt", instructed),
            ("denied_tools", barred),
        ] {
            let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

            let refusal = runner
                .run_prompt(request, session_tx, std::sync::Arc::clone(&transcript))
                .await
                .expect_err("a control the tool cannot apply must not reach a launch")
                .to_string();

            assert!(
                refusal.contains(field),
                "the refusal must name the field it could not honour: {refusal}"
            );
            assert!(
                transcript.lock().unwrap().is_empty(),
                "{field}: a refused run must produce no transcript, because nothing ran"
            );
            assert!(
                session_rx.try_recv().is_err(),
                "{field}: a run that never launched must not announce a session"
            );
        }

        // The control. A runner that refused everything would pass the loop above while making the
        // second CLI unusable — and the baseline carries a caller-assigned `session_id`, so this also
        // pins that identity is not what a refusal keys on.
        let honourable = baseline_run_request();
        assert!(
            honourable.session_id.is_some(),
            "the control must carry the identity the daemon assigns every run"
        );
        assert!(
            codex_cli_args(&honourable, "gpt-5.6-terra", &CodexStaged::default()).is_ok(),
            "a request asking for none of the above must still build a launch"
        );

        // Resume is honoured, through `codex exec resume`, and must still build a launch.
        let mut resumed = baseline_run_request();
        resumed.resume_session_id = Some("123e4567-e89b-42d3-a456-426614174001".to_string());
        assert!(
            codex_cli_args(&resumed, "gpt-5.6-terra", &CodexStaged::default()).is_ok(),
            "resume is honoured through `exec resume`"
        );

        // And the other one, for the same reason. A display name reaches the Claude CLI's `--resume`
        // picker and nothing else; no record claims it was applied, so losing it costs a nameless
        // session, which is what every run on this path has always had.
        let mut named = baseline_run_request();
        named.session_name = Some("o refactor do runner".to_string());
        let (session_tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let outcome = runner
            .run_prompt(named, session_tx, transcript)
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            !outcome.contains("session_name"),
            "a cosmetic name is dropped on this path, not refused: {outcome}"
        );
    }

    /// A missing measurement must not read as a measurement of zero — the same invariant
    /// `usage_absent_is_none_not_zero` pins for the Claude CLI and the local runner returns `None`
    /// for by construction.
    ///
    /// These are the `RunOutcome` fields `budget.rs` bills autonomy against. A second runner exists
    /// precisely to be the cheaper path once a spend ceiling has paused the first one, so one that
    /// reported `Some(0)` for a tool that said nothing would make every fallback run look free and
    /// leave the ceiling unable to pause anything.
    ///
    /// Both a plain-text transcript and a structured event carrying no usage object are silence: the
    /// answer is unknown in each, and unknown is not zero.
    #[test]
    fn codex_usage_absent_is_none_not_zero() {
        for stdout in [
            "the crate builds and the suite is green\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
        ] {
            let usage = codex_extract_usage(stdout);

            assert_eq!(usage.input_tokens, None, "{stdout}");
            assert_eq!(usage.output_tokens, None, "{stdout}");
            assert_eq!(usage.cache_read_tokens, None, "{stdout}");
        }
    }
}
