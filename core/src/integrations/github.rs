//! §spec modulo-de-github
//!
//! The GitHub pillar: total capability, autonomy by list.
//!
//! Three pieces that do not know each other. `ReadOp`/`ActOp` are what was asked for, as data;
//! `Policy` answers whether it runs without asking; `execute` answers how. It is the separation
//! `vcs.rs` keeps between WHEN an operation runs and WHETHER the actor was allowed to ask for it.
//!
//! **The partition between `ReadOp` and `ActOp` is a security boundary and not a tidy-up.**
//! `permitted_after_untrusted` (`mcp_tools.rs`) is `tool_effect(name) != Acts` — it reads the table
//! by NAME and never calls `effect_of_call`. A single tool would have to be `ReadsOwn` for the
//! argument-reading arm to run at all, and `ReadsOwn` makes that function return `true`; a turn that
//! had already read a stranger's words would pass the barrier and write to GitHub. Two tools with
//! two parameter types is what makes the barrier work without touching it.
//!
//! Whoever adds a variant owes it to the partition: an operation that acts goes in `ActOp` or it
//! goes nowhere. `read_ops_and_act_ops_partition_op` fails if it does not.
//!
//! Requests are typed, never command strings — the law `vcs.rs` states for its own queue and which
//! holds here unchanged: parsing shell is the surface `classifier.rs` exists to keep closed, so the
//! daemon builds every argv itself. **`ActOp::ApiRead` carries `Vec<String>` rather than a line**, and
//! that is the variant where the law costs most and matters most: the one escape hatch for cases
//! nobody foresaw is exactly where a command string would undo it.

use crate::mcp_tools::ToolEffect;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use self::gh_map::word_is_flag;
use self::ops::{argv_safe, numeric, single_line};

/// Validating `Deserialize` for every node type, and the reason is `vcs.rs`'s: the check has to hold
/// on every route in, and there are three — the flat parameters an MCP tool call carries, a raw
/// `POST /github/requests` body that deserializes an `Op` directly, and a proposal's payload read
/// back. A `Deserialize` that validates covers all three; a checked constructor covers the first
/// only, which is how `{"op":"pr_view","repo":"--upload-pack=x"}` would have got through.
macro_rules! validating_deserialize {
    ($($type:ty),+ $(,)?) => {
        $(
            impl<'de> Deserialize<'de> for $type {
                fn deserialize<D: serde::Deserializer<'de>>(
                    deserializer: D,
                ) -> Result<Self, D::Error> {
                    <$type>::new(&String::deserialize(deserializer)?)
                        .map_err(serde::de::Error::custom)
                }
            }
        )+
    };
}

mod exec;
mod gh_map;
mod ops;
mod policy;
mod types;

pub use self::exec::*;
pub use self::gh_map::*;
pub use self::ops::*;
pub use self::policy::*;
pub use self::types::*;

validating_deserialize!(
    Repo,
    Branch,
    WorkflowName,
    Title,
    Body,
    RunId,
    PrNumber,
    IssueNumber,
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GithubConfig, load_github_config};

    /// Goes through the real loader rather than around it, so "absent" is genuinely an absent file
    /// and not a hand-built default that happens to look like one.
    fn policy_from(text: Option<&str>) -> Policy {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("github.yaml");
        if let Some(text) = text {
            std::fs::write(&path, text).expect("the sample config should be writable");
        }
        Policy::from_config(&load_github_config(&path))
    }

    fn repo() -> Repo {
        Repo::new("owner/name").expect("owner/name is a repository")
    }

    /// A pillar the owner switched off, which is what a test uses when it wants `execute` to refuse
    /// before spawning anything or touching the Credential Manager.
    fn switched_off() -> GithubRuntime {
        GithubRuntime {
            enabled: false,
            ..GithubRuntime::default()
        }
    }

    /// A runtime pointed at a stub program instead of `gh`.
    ///
    /// **These spawn `echo`, `false` and `yes` as PROGRAMS**, so on Windows they need Git's
    /// `usr/bin` on PATH — the same requirement the nine tests in `gate::` and `transcribe::`
    /// already carry, and the same failure without it: `program not found`, which reads like a
    /// broken repository and is not.
    ///
    /// The stub is what makes the spawn testable at all. `gh` is not on every machine, and the two
    /// things `execute` needs that a suite cannot have — a real Credential Manager entry and a
    /// 120-second deadline — are `spawn_gh`'s arguments rather than its constants.
    fn pointed_at(program: &str) -> GithubRuntime {
        GithubRuntime {
            enabled: true,
            configured: true,
            binary: program.to_owned(),
            policy: Policy::empty(),
        }
    }

    fn a_comment(body: &str) -> Op {
        Op::Act(ActOp::PrComment {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
            body: Body::new(body).expect("the sample body is valid"),
        })
    }

    /// The law, measured instead of argued. `echo` prints the argv it was handed, so what comes back
    /// says exactly what the operating system was asked to run.
    ///
    /// The body carries a `;`, a `$(...)` and an `&&`. If any shell were anywhere on this path they
    /// would split the command, run `whoami`, or append a second one; what comes back is one
    /// argument with all three characters still in it.
    #[tokio::test]
    async fn the_argv_is_built_and_never_interpreted() {
        let dangerous = "a; rm -rf / $(whoami) && echo pwned";
        let outcome = spawn_gh(
            &pointed_at("echo"),
            &a_comment(dangerous),
            "unused",
            Duration::from_secs(30),
        )
        .await
        .expect("echo runs");

        assert_eq!(outcome.exit_code, Some(0));
        // The whole argv, exactly, on ONE line. `echo` writes its arguments separated by spaces and
        // nothing else, so this is the operating system reporting back what it was asked to run.
        //
        // Written first as `!stdout.contains("pwned")`, which was the wrong assertion and failed:
        // the body CONTAINS that word, so `echo` prints it and always would. What distinguishes an
        // interpreted line from a literal one is not the presence of the word — it is that a shell
        // would have put it on a SECOND line, from a second command. Comparing the whole output
        // against the whole argv says that and says it exactly.
        assert_eq!(
            outcome.stdout.trim_end(),
            format!("pr comment --repo=owner/name --body={dangerous} -- 42"),
        );
    }

    /// **The credential never touches the command line.** A process's argv is readable by any
    /// process on the machine, so this is the difference between a secret and a broadcast.
    ///
    /// The token is deliberately NOT token-shaped: a `ghp_...` would be caught by `redact.rs` on the
    /// way out, and the test would then pass whether or not it had ever been in the argv. This one
    /// nothing redacts, so its absence is the argument.
    #[tokio::test]
    async fn the_token_goes_by_environment_and_never_as_an_argument() {
        let token = "plainly-not-token-shaped-9876543210";
        let outcome = spawn_gh(
            &pointed_at("echo"),
            &a_comment("an ordinary comment"),
            token,
            Duration::from_secs(30),
        )
        .await
        .expect("echo runs");

        assert!(
            !outcome.stdout.contains(token),
            "the token reached the argv"
        );
        assert!(!outcome.output_tail.contains(token));
    }

    /// And a credential that DOES come back out of the CLI's own diagnostics is redacted, which is
    /// the other half and a different claim: the first is about what goes in, this is about what
    /// comes out.
    #[tokio::test]
    async fn a_credential_in_the_output_comes_back_redacted() {
        let leaked = format!("ghp_{}", "a1B2c3D4e5".repeat(4)); // 40 chars after the prefix
        let outcome = spawn_gh(
            &pointed_at("echo"),
            &a_comment(&leaked),
            "unused",
            Duration::from_secs(30),
        )
        .await
        .expect("echo runs");

        assert!(!outcome.stdout.contains(&leaked), "{}", outcome.stdout);
        assert!(
            outcome.stdout.contains("[SECRET:github]"),
            "{}",
            outcome.stdout
        );
    }

    /// A non-zero exit is an ANSWER and not an error: it comes back with its code for the caller to
    /// read. Nothing here retries, and `PrComment` is why — a retry of one that failed halfway posts
    /// it twice, and this module cannot tell halfway from not-at-all.
    #[tokio::test]
    async fn a_non_zero_exit_comes_back_rather_than_failing() {
        let outcome = spawn_gh(
            &pointed_at("false"),
            &a_comment("a comment"),
            "unused",
            Duration::from_secs(30),
        )
        .await
        .expect("a failing command still produced a result");

        assert_eq!(outcome.exit_code, Some(1));
        assert_eq!(outcome.kind, "pr_comment");
    }

    /// The deadline, against a program that never ends. `yes` repeats its arguments forever, so
    /// this also drives the output cap: the buffer stops growing and the call still returns.
    ///
    /// It is `spawn_gh`'s argument rather than `OPERATION_TIMEOUT` for exactly this reason — the
    /// production two minutes is not a wait a suite can take, and a deadline that cannot be
    /// exercised is a deadline nobody has seen fire.
    ///
    /// **`ApiRead` and not `PrList`, and the first attempt is the reason.** `yes pr list --repo=o/r`
    /// exits 1 with "unknown option" — `yes` runs getopt over its arguments like any coreutil, so a
    /// stub cannot be handed an argv full of flags and be expected to ignore them. `ApiRead` puts every
    /// caller value after `--`, which is the one argv shape in this module that carries no option at
    /// all, so it is the shape a stub can actually receive.
    #[tokio::test]
    async fn a_command_that_never_ends_dies_on_the_deadline() {
        let refused = spawn_gh(
            &pointed_at("yes"),
            &Op::Act(ActOp::ApiRead {
                args: vec!["forever".to_owned()],
            }),
            "unused",
            Duration::from_millis(300),
        )
        .await;

        assert_eq!(refused, Err(Failure::TimedOut));
    }

    /// A machine without `gh` is a machine that cannot do this, and that is not a fault. Told
    /// anything else, somebody goes looking for a broken repository.
    #[tokio::test]
    async fn a_binary_that_is_not_there_is_missing_and_not_an_error() {
        let refused = spawn_gh(
            &pointed_at("nucleos-gh-that-is-not-installed"),
            &Op::Read(ReadOp::PrList { repo: repo() }),
            "unused",
            Duration::from_secs(30),
        )
        .await;

        assert_eq!(refused, Err(Failure::MissingCli));
    }

    /// The cap keeps the END of a long stream, which is the half a failing log is read for.
    ///
    /// `drain` and not `spawn_gh`, because a stub that emits a quarter-megabyte of DISTINGUISHABLE
    /// output is not something `echo` or `yes` can be asked for — `yes` repeats one line, and a
    /// buffer of identical lines cannot tell a kept head from a kept tail. Numbered lines can.
    ///
    /// Written after the bug shipped and was found in use: a red CI run came back with its setup
    /// lines and without its `failures:` block, under a marker claiming the opposite.
    #[tokio::test]
    async fn a_stream_past_the_cap_keeps_its_end_and_not_its_beginning() {
        let mut source: Vec<u8> = Vec::new();
        let mut n = 0_u64;
        while source.len() < MAX_OUTPUT_BYTES * 3 {
            source.extend_from_slice(format!("line {n}\n").as_bytes());
            n += 1;
        }
        let ultima = format!("line {}\n", n - 1);

        let buffer = Arc::new(Mutex::new(Vec::new()));
        drain(&source[..], buffer.clone()).await;
        let guardado = buffer.lock().expect("the buffer is not poisoned");

        let texto = String::from_utf8_lossy(&guardado);
        assert!(
            texto.ends_with(&ultima),
            "the end of the stream is missing; it ends with {:?}",
            &texto[texto.len().saturating_sub(40)..]
        );
        assert!(
            !texto.contains("line 0\n"),
            "the beginning was kept instead of the end"
        );
        assert!(
            guardado.len() > MAX_OUTPUT_BYTES,
            "nothing was kept beyond the cap, so `clip` would not report a cut"
        );
        assert!(
            guardado.len() <= MAX_OUTPUT_BYTES * 2,
            "the sliding window did not bound memory: {} bytes",
            guardado.len()
        );
    }

    /// The override exists for the reason `NUCLEOS_CLAUDE_BIN` exists, and a default that drifted
    /// from `gh` would be a pillar looking for a program nobody installs.
    #[test]
    fn the_default_binary_is_gh() {
        assert_eq!(GithubRuntime::default().binary, "gh");
    }

    /// The partition. `ReadOp` and `ActOp` cover `Op` with no overlap and no hole: an operation in
    /// both halves would be the partition undone, and one in neither would be an operation
    /// `execute` knows how to run and no tool knows how to ask for.
    #[test]
    fn read_ops_and_act_ops_partition_op() {
        let reads: Vec<&str> = ReadOp::all().iter().map(|op| op.kind()).collect();
        let acts: Vec<&str> = ActOp::all().iter().map(|op| op.kind()).collect();

        for kind in &reads {
            assert!(
                !acts.contains(kind),
                "{kind} is on both sides of the partition"
            );
        }
        let mut all: Vec<&str> = reads.iter().chain(acts.iter()).copied().collect();
        all.sort_unstable();
        let mut every: Vec<&str> = Op::all().iter().map(|op| op.kind()).collect();
        every.sort_unstable();
        assert_eq!(all, every, "an operation of `Op` belongs to neither half");
    }

    /// No read acts and no act fails to. It is the effects table, and this is what makes it
    /// checkable rather than decorative.
    #[test]
    fn every_operation_declares_an_effect() {
        for op in ReadOp::all() {
            assert!(
                matches!(
                    op.effect(),
                    ToolEffect::ReadsOwn | ToolEffect::ReadsUntrusted
                ),
                "{} is a read and may not act",
                op.kind()
            );
        }
        for op in ActOp::all() {
            assert_eq!(op.effect(), ToolEffect::Acts, "{} acts", op.kind());
        }
    }

    /// The six reads that carry a stranger's words are marked, and the four that carry structure are
    /// not. Getting this backwards is the whole failure the per-operation effect exists to stop.
    ///
    /// The list is written out here rather than derived, and the duplication is the point: `effect`
    /// is one `match` and a test that read it back would agree with whatever that `match` said. A
    /// new variant defaults to `ReadsOwn` in the `_` arm below, so an author who grades a stranger's
    /// text as structure fails here and reads the grading again.
    #[test]
    fn prose_is_untrusted_and_structure_is_not() {
        for op in ReadOp::all() {
            let expected = match op.kind() {
                "pr_view" | "issue_view" | "run_logs" | "pr_diff" | "pr_thread"
                | "checks_for_ref" => ToolEffect::ReadsUntrusted,
                _ => ToolEffect::ReadsOwn,
            };
            assert_eq!(op.effect(), expected, "{}", op.kind());
        }
    }

    /// The three reads added for a stranger's words, each in the shape its grading claims.
    ///
    /// It asserts the argv as well as the effect, because the grading is a claim ABOUT the argv: a
    /// `pr_diff` that had quietly become `gh pr view` would still say `ReadsUntrusted` and would no
    /// longer be reading a diff. And it asserts the effect through `effect_of_kind`, which is the
    /// route `mcp_tools::effect_of_call` actually takes — the one that latches the turn.
    #[test]
    fn the_three_reads_that_carry_a_strangers_words_say_so() {
        let number = PrNumber::new("42").expect("42 is a pull request number");
        let expected: [(ReadOp, Vec<&str>); 3] = [
            (
                ReadOp::PrDiff {
                    repo: repo(),
                    number: number.clone(),
                },
                vec!["pr", "diff", "--repo=owner/name", "--", "42"],
            ),
            (
                ReadOp::PrThread {
                    repo: repo(),
                    number,
                },
                vec!["pr", "view", "--repo=owner/name", "--comments", "--", "42"],
            ),
            (
                ReadOp::ChecksForRef {
                    repo: repo(),
                    r#ref: Branch::new("main").expect("main is a ref"),
                },
                vec!["pr", "checks", "--repo=owner/name", "--", "main"],
            ),
        ];

        for (op, argv) in expected {
            assert_eq!(op.argv(), argv, "{}", op.kind());
            assert_eq!(op.effect(), ToolEffect::ReadsUntrusted, "{}", op.kind());
            assert_eq!(
                ReadOp::effect_of_kind(op.kind()),
                Some(ToolEffect::ReadsUntrusted),
                "{} has to be untrusted on the route effect_of_call takes",
                op.kind()
            );
            // A read is a read: the partition does not bend for the grading.
            assert!(!ActOp::all().iter().any(|act| act.kind() == op.kind()));
        }

        // Reading the thread and posting to it are different operations on different sides of the
        // partition, and each tool sends the other's kind to the right place rather than answering
        // "unknown operation". This is what the names `pr_thread` and `pr_comment` are for; it was
        // `pr_comments` and `pr_comment` for one commit, which is one letter to carry a boundary on.
        assert!(
            ReadOp::from_request(ReadRequest {
                operation: "pr_comment".to_owned(),
                repo: "owner/name".to_owned(),
                id: Some("42".to_owned()),
            })
            .expect_err("pr_comment acts")
            .contains("github_act"),
            "posting a comment is sent to the acting tool"
        );
        assert!(
            ActOp::from_request(ActRequest {
                operation: "pr_thread".to_owned(),
                repo: Some("owner/name".to_owned()),
                id: Some("42".to_owned()),
                ..ActRequest::default()
            })
            .expect_err("pr_thread only reads")
            .contains("github_read"),
            "reading the thread is sent to the reading tool"
        );

        // And the two withdrawn names are gone rather than aliased. An operation that answered to
        // its old string would be the rename undone in the one place a caller can reach.
        for withdrawn in ["pr_files", "pr_comments"] {
            assert!(
                ReadOp::from_request(ReadRequest {
                    operation: withdrawn.to_owned(),
                    repo: "owner/name".to_owned(),
                    id: Some("42".to_owned()),
                })
                .expect_err("a withdrawn name is not an operation")
                .contains("unknown read operation"),
                "{withdrawn} may not still resolve"
            );
        }
    }

    /// None of the three is declarable, and that is the decision rather than an oversight.
    ///
    /// `READ_CEILING` is what a project may be granted, and it holds only reads of structural shape
    /// — the constant's own doc refuses `gh pr view` in those words. These three return a stranger's
    /// text by definition, so admitting them would contradict the sentence that admits anything at
    /// all. They are reachable the way `pr_view` and `issue_view` are: a run asks for one, gets it,
    /// and pays the turn's right to act for it.
    #[test]
    fn a_stranger_carrying_read_is_reachable_and_not_declarable() {
        let widest = Policy::from_config(&crate::config::GithubConfig {
            enabled: true,
            autonomous_reads: READ_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
            autonomous_actions: Vec::new(),
        });
        for op in ReadOp::all()
            .into_iter()
            .filter(|op| op.effect() == ToolEffect::ReadsUntrusted)
        {
            assert!(
                !widest.read_is_autonomous(&format!("gh {}", op.argv().join(" "))),
                "{} returns a stranger's words and may not be autonomous",
                op.kind()
            );
        }
    }

    /// The law, in the variant where it costs most and matters most. A `ApiRead` carrying a line would
    /// be "never a command string" undone by the one variant that exists for the cases nobody
    /// foresaw.
    #[test]
    fn api_read_carries_separate_arguments_and_never_a_line() {
        let op = ActOp::ApiRead {
            args: vec!["repos/o/r".into(), "-X".into(), "GET".into()],
        };
        assert_eq!(op.argv(), vec!["api", "--", "repos/o/r", "-X", "GET"]);
    }

    /// Every caller value is either behind `--` or inside a `--flag=value`. Nothing this module
    /// builds hands `gh` a bare `--flag value` pair carrying a caller's string.
    ///
    /// The exceptions are the module's own BOOLEANS, and that is the whole rule they satisfy: a flag
    /// that takes no value cannot be followed by a caller's string, so it consumes nothing and can
    /// stand bare. Named one by one rather than waved through by a prefix check — the day a flag
    /// that DOES take a value is added, the failure should be here and not in a shell.
    #[test]
    fn no_caller_value_reaches_argv_where_it_could_act_as_a_flag() {
        const OUR_OWN_BOOLEANS: &[&str] = &["--log", "--comments"];
        for op in Op::all() {
            let argv = op.argv();
            let terminator = argv.iter().position(|part| part == "--");
            for (index, part) in argv.iter().enumerate() {
                if terminator.is_some_and(|at| index > at) {
                    continue;
                }
                assert!(
                    !part.starts_with("--")
                        || part.contains('=')
                        || OUR_OWN_BOOLEANS.contains(&part.as_str())
                        || part == "--",
                    "{} puts {part} on the command line as a bare flag",
                    op.kind()
                );
            }
        }
    }

    /// The terminator goes in when there is a positional to protect and stays out when there is
    /// not. A stray `--` on `gh pr create` would be an empty positional argument, not a no-op.
    #[test]
    fn the_terminator_appears_exactly_where_a_positional_does() {
        let with = ActOp::RunRerun {
            repo: repo(),
            id: RunId::new("7").expect("7 is a run id"),
        };
        assert_eq!(
            with.argv(),
            vec!["run", "rerun", "--repo=owner/name", "--", "7"]
        );

        let without = ReadOp::PrList { repo: repo() };
        assert_eq!(without.argv(), vec!["pr", "list", "--repo=owner/name"]);
    }

    /// `--ref` is built as `--ref=value` and sits BEFORE the terminator, because the workflow name
    /// is positional. A `["--ref", value]` pair would be read as two positionals the moment
    /// anything moved.
    #[test]
    fn a_flag_that_precedes_a_positional_is_written_as_flag_equals_value() {
        let op = ActOp::WorkflowRun {
            repo: repo(),
            workflow: WorkflowName::new("CI Build").expect("CI Build is a workflow name"),
            r#ref: Branch::new("main").expect("main is a branch"),
        };
        assert_eq!(
            op.argv(),
            vec![
                "workflow",
                "run",
                "--repo=owner/name",
                "--ref=main",
                "--",
                "CI Build",
            ]
        );
    }

    /// The node types refuse what could act as an option, and a `Deserialize` that only called
    /// `String::deserialize` would let every one of these through.
    #[test]
    fn a_node_type_refuses_a_value_that_could_act_as_an_option() {
        assert!(Repo::new("--upload-pack=x").is_err());
        assert!(Repo::new("name").is_err(), "a repository is owner/name");
        assert!(Repo::new("a/b/c").is_err());
        assert!(Branch::new("--delete").is_err());
        assert!(Branch::new("").is_err());
        assert!(WorkflowName::new("-x").is_err());
        assert!(RunId::new("12; rm -rf /").is_err());
        assert!(PrNumber::new("-1").is_err());
        assert!(IssueNumber::new("").is_err());

        assert!(Repo::new("owner/name").is_ok());
        assert!(WorkflowName::new("CI Build").is_ok());
        assert!(Body::new("a body\nwith lines").is_ok());
        assert!(Body::new("a body with a \u{1b} in it").is_err());
    }

    /// Every spelling git writes for a GitHub `origin` reaches the same `owner/name`.
    ///
    /// Written as a table because the spellings are the whole content of the function: `git clone`
    /// over HTTPS, the scp-like form `git@github.com:owner/name.git` that an SSH clone leaves
    /// behind, the explicit `ssh://` form, and each of those with and without the `.git` suffix a
    /// trailing slash. A parser that handled three of the four would send `pr_list` at the wrong
    /// repository for a fourth of this house's checkouts and no error would say so.
    #[test]
    fn every_spelling_of_a_github_remote_names_the_same_repository() {
        for url in [
            "https://github.com/owner/name.git",
            "https://github.com/owner/name",
            "https://github.com/owner/name/",
            "https://user@github.com/owner/name.git",
            "git@github.com:owner/name.git",
            "git@github.com:owner/name",
            "ssh://git@github.com/owner/name.git",
            "git://github.com/owner/name.git",
            "  https://github.com/owner/name.git  ",
            // The host is a name and not a case, which is what `eq_ignore_ascii_case` is for.
            "https://GitHub.com/owner/name.git",
        ] {
            assert_eq!(
                Repo::from_remote_url(url).map(|repo| repo.as_str().to_owned()),
                Some("owner/name".to_owned()),
                "{url} names owner/name"
            );
        }
    }

    /// A remote this cannot read is `None`, and the list is the reason the caller may not turn that
    /// into an error.
    ///
    /// Each of these is an ordinary thing for a project to have. What the caller does with `None` is
    /// say which repository it is NOT — `ProjectRepo::NotGithub` carries the URL for exactly that —
    /// and a function that refused instead would make a project on GitLab look broken.
    #[test]
    fn a_remote_that_is_not_a_github_repository_names_nothing() {
        for url in [
            "https://gitlab.com/owner/name.git",
            "git@gitlab.com:owner/name.git",
            // An Enterprise host is a real deployment and it is not the one this `gh` is pointed at.
            "https://github.example.com/owner/name.git",
            "https://notgithub.com/owner/name.git",
            // A local remote — a folder, or a Windows drive letter that a naive `:` split would read
            // as a host.
            "C:/Projects/nucleos",
            "/srv/git/name.git",
            // GitHub, naming no repository or naming too much of one. `Repo::new` decides, here as
            // everywhere.
            "https://github.com/owner",
            "https://github.com/owner/name/tree/master",
            "https://github.com",
            "",
        ] {
            assert_eq!(
                Repo::from_remote_url(url),
                None,
                "{url} names no repository"
            );
        }
    }

    /// The validating `Deserialize` is what covers the raw route, and this is the case that proves
    /// it is not the constructor doing the work.
    #[test]
    fn deserializing_an_op_validates_every_node_it_carries() {
        let good: ReadOp = serde_json::from_value(serde_json::json!({
            "op": "pr_view", "repo": "owner/name", "number": "42"
        }))
        .expect("a well formed read deserializes");
        assert_eq!(good.kind(), "pr_view");

        let bad = serde_json::from_value::<ReadOp>(serde_json::json!({
            "op": "pr_view", "repo": "--upload-pack=x", "number": "42"
        }));
        assert!(bad.is_err(), "a dashed repository may not deserialize");
    }

    /// Every other field's default is a convenience; these two are a refusal.
    #[test]
    fn a_github_yaml_that_is_absent_unreadable_or_malformed_is_autonomous_in_nothing() {
        for text in [
            None,
            Some("{{{ this is not yaml"),
            Some("autonomous_reads: this is not a list\n"),
            Some(""),
        ] {
            let policy = policy_from(text);
            assert!(policy.autonomous_reads().is_empty(), "{text:?}");
            assert!(policy.autonomous_actions().is_empty(), "{text:?}");
            assert!(!policy.read_is_autonomous("gh run list"), "{text:?}");
            assert!(!policy.action_is_autonomous("pr_comment"), "{text:?}");
        }
    }

    /// A missing file leaves the pillar CAPABLE and not switched off, which is the asymmetry with
    /// the web and browser pillars that `GithubConfig`'s doc argues for. Nothing is autonomous;
    /// everything can still be asked for.
    #[test]
    fn a_missing_file_withholds_autonomy_and_not_capability() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let config = load_github_config(&directory.path().join("nothing-here.yaml"));
        assert!(
            config.enabled,
            "an absent file may not switch the pillar off"
        );
        assert!(config.autonomous_reads.is_empty());
        assert!(config.autonomous_actions.is_empty());
    }

    /// The file may narrow the ceiling and may never widen it. It asks for two things the ceiling
    /// does not have and one it does, and keeps the one it does.
    #[test]
    fn the_file_may_narrow_the_ceiling_and_never_widens_it() {
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n  - gh auth token\n\
             autonomous_actions:\n  - pr_comment\n  - api_read\n",
        ));
        assert_eq!(policy.autonomous_reads(), ["gh run list"]);
        assert_eq!(policy.autonomous_actions(), ["pr_comment"]);
        assert!(policy.read_is_autonomous("gh run list"));
        assert!(!policy.read_is_autonomous("gh auth token"));
        assert!(policy.action_is_autonomous("pr_comment"));
        assert!(!policy.action_is_autonomous("api_read"));
    }

    /// The two that never pass, with the file explicitly asking for the opposite.
    #[test]
    fn gh_api_and_gh_auth_token_are_never_autonomous() {
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh auth token\n  - gh auth status\n  - gh secret list\n  \
             - gh variable list\nautonomous_actions:\n  - api_read\n",
        ));
        assert!(policy.autonomous_reads().is_empty());
        assert!(policy.autonomous_actions().is_empty());
    }

    /// A read that returns prose is not in the ceiling, so the file cannot turn it on.
    #[test]
    fn a_read_that_returns_prose_is_not_in_the_read_ceiling() {
        for prose in ["gh pr view", "gh issue view", "gh run download"] {
            assert!(
                !READ_CEILING.contains(&prose),
                "{prose} returns a stranger's words and may not be eligible for autonomy"
            );
        }
        let policy = policy_from(Some(
            "autonomous_reads:\n  - gh pr view\n  - gh issue view\n",
        ));
        assert!(policy.autonomous_reads().is_empty());
    }

    /// **The central test.** The prefix matches and autonomy has to fall anyway: a structural read
    /// turns into prose — or into a thousand lines of it — without changing subcommand.
    #[test]
    fn a_refused_flag_takes_autonomy_from_a_prefix_that_had_it() {
        let policy = policy_from(Some("autonomous_reads:\n  - gh run view\n  - gh pr list\n"));

        assert!(policy.read_is_autonomous("gh run view 123"));
        assert!(policy.read_is_autonomous("gh pr list --state open"));
        assert!(policy.read_is_autonomous("gh pr list --author octocat"));

        for command in [
            "gh run view 123 --log",
            "gh run view 123 --log-failed",
            "gh pr list --json body",
            // pflag accepts `=`, and equality alone would let this one through.
            "gh pr list --json=body",
            // The short form of `--jq`.
            "gh pr list -q .[].body",
            "gh pr list --template {{.body}}",
            "gh pr list -t {{.body}}",
            "gh pr list --search 'in:body secret'",
            // For an injection channel, how many IS the payload.
            "gh pr list --limit 1000",
            "gh pr list --limit=1000",
            // The alias that a review forgot, on the very entry created to stop aliases being
            // forgotten.
            "gh pr list -L 1000",
        ] {
            assert!(
                !policy.read_is_autonomous(command),
                "{command} should have lost its autonomy"
            );
        }
    }

    /// pflag reads everything after a short flag's letter as its value, so `-L1000` IS `-L 1000`.
    #[test]
    fn a_short_flag_with_its_value_glued_on_is_still_that_flag() {
        let policy = policy_from(Some(
            "autonomous_reads:
  - gh pr list
",
        ));
        for command in [
            "gh pr list -L1000",
            "gh pr list -qEXPR",
            "gh pr list -t{{.body}}",
            "gh pr list -S 'in:body x'",
            "gh pr list -Sin:body",
        ] {
            assert!(
                !policy.read_is_autonomous(command),
                "{command} should have lost its autonomy"
            );
        }
        assert!(policy.read_is_autonomous("gh pr list -l bug"));
    }

    /// pflag also bundles short flags: `-dL5` is `--draft --limit 5`, so a refused letter behind a
    /// valueless one is still that flag. Behind a flag that takes a value it is part of the value.
    #[test]
    fn a_short_flag_bundled_behind_another_is_still_that_flag() {
        let policy = policy_from(Some("autonomous_reads:\n  - gh pr list\n"));
        for command in [
            "gh pr list -dL5",
            "gh pr list -dqEXPR",
            "gh pr list -wt{{.body}}",
        ] {
            assert!(
                !policy.read_is_autonomous(command),
                "{command} should have lost its autonomy"
            );
        }
        assert!(policy.read_is_autonomous("gh pr list -lLabel"));
        assert!(policy.read_is_autonomous("gh pr list -d"));
    }

    /// `-L` is `--limit` and `-l` is `--label`. Folding the comparison to one case would either
    /// miss the first or refuse the second, which is why the raw command is what gets read.
    #[test]
    fn the_flag_comparison_is_case_sensitive_because_gh_is() {
        let policy = policy_from(Some("autonomous_reads:\n  - gh pr list\n"));
        assert!(!policy.read_is_autonomous("gh pr list -L 1000"));
        assert!(policy.read_is_autonomous("gh pr list -l bug"));
    }

    /// A prefix is a prefix of WORDS. `gh run listen` is not `gh run list`, and a comparison by
    /// `starts_with` alone would have said it was.
    #[test]
    fn a_prefix_matches_whole_words_and_not_a_string() {
        let policy = policy_from(Some("autonomous_reads:\n  - gh run list\n"));
        assert!(policy.read_is_autonomous("gh run list"));
        assert!(policy.read_is_autonomous("gh run list --branch main"));
        assert!(!policy.read_is_autonomous("gh run listen"));
        assert!(!policy.read_is_autonomous("gh runlist"));
    }

    /// A pillar the owner switched off is autonomous in nothing, which is the same answer an empty
    /// list gives — and the reason `enabled` is not carried as a third state.
    #[test]
    fn enabled_false_collapses_both_lists() {
        let config = GithubConfig {
            enabled: false,
            autonomous_reads: vec!["gh run list".to_owned()],
            autonomous_actions: vec!["pr_comment".to_owned()],
        };
        let policy = Policy::from_config(&config);
        assert!(policy.autonomous_reads().is_empty());
        assert!(policy.autonomous_actions().is_empty());
        assert_eq!(
            policy.digest(),
            Policy::empty().digest(),
            "a switched-off pillar and an empty pair of lists are the same effective policy"
        );
    }

    /// The digest tracks the effective policy and nothing else: a comment, the order of the entries,
    /// and a line the ceiling drops all leave it alone.
    #[test]
    fn the_digest_changes_with_the_effective_policy_and_not_with_a_comment() {
        let base = policy_from(Some("autonomous_reads:\n  - gh run list\n  - gh pr list\n"));
        let commented = policy_from(Some(
            "# the owner's notes\nautonomous_reads:\n  - gh pr list\n  - gh run list\n",
        ));
        let dropped = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n  - gh pr list\n  - gh api\n",
        ));
        let different = policy_from(Some("autonomous_reads:\n  - gh run list\n"));

        assert_eq!(base.digest(), commented.digest());
        assert_eq!(base.digest(), dropped.digest());
        assert_ne!(base.digest(), different.digest());
        assert_ne!(base.digest(), Policy::empty().digest());
        assert_eq!(base.digest().len(), 16);
    }

    /// **The digest's OUTPUT, pinned to literals, because what it labels is already on disk.**
    ///
    /// Every other assertion about this function is RELATIVE — two policies hash alike, or they do
    /// not — and every one of them survives a change to `digest_of` itself, because both sides move
    /// together. `a_project_that_declared_nothing_is_the_machine_default_exactly` cannot help either,
    /// for exactly that reason: it compares `for_project`'s answer against the machine's, and a
    /// rewrite moves the pair. So the property that actually matters here had nothing holding it.
    ///
    /// That property is not internal consistency. `policy_digest` is a column in
    /// `shadow_decisions`, written on every governed tool call, and rows carrying these strings
    /// exist in live databases. A change that renumbers them orphans the history: `shadow_readiness`
    /// COUNTS distinct digests per action class, so the same class would suddenly be reported as
    /// spanning two policies on a machine where nobody changed anything, and
    /// `READINESS_MIN_REVIEWED` would restart ten reviews of progress toward promotion.
    ///
    /// **The third assertion is the one that guards the conditional in `digest_of`.** Drop the
    /// `if !read_ops.is_empty()` and an empty overlay starts hashing `\nread_ops=` — the machine
    /// default's own label changes, and so does every row already recorded under it. The fourth
    /// pins the other half: when there IS a name, the suffix is written, so two genuinely different
    /// effective policies still get different labels.
    ///
    /// Literals rather than a recomputation, deliberately. A test that recomputed FNV-1a here would
    /// agree with whatever `digest_of` did, which is the failure mode this exists to close.
    #[tokio::test]
    async fn the_digest_of_a_known_policy_is_a_known_string() {
        let pool = test_pool().await;

        // `v1\nreads=\nactions=` — the label a machine with no autonomy has always carried.
        assert_eq!(Policy::empty().digest(), "ef64af62014226ae");

        let machine = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n\
             autonomous_actions:\n  - pr_comment\n",
        ));
        // `v1\nreads=gh run list\nactions=pr_comment`
        assert_eq!(machine.digest(), "9cef55bb486b8280");

        // A project that declared nothing carries the machine's own byte, and that is the claim
        // about rows already written rather than a claim about two values in this process.
        assert_eq!(
            machine.for_project(&pool, "alpha").await.digest(),
            "9cef55bb486b8280"
        );
        assert_eq!(
            Policy::empty().for_project(&pool, "alpha").await.digest(),
            "ef64af62014226ae"
        );

        // And a project that declared something gets a label of its own, suffix and all:
        // `v1\nreads=gh run list\nactions=pr_comment\nread_ops=pr_list`
        crate::project::policy::declare_github_op(&pool, "alpha", "pr_list")
            .await
            .unwrap();
        assert_eq!(
            machine.for_project(&pool, "alpha").await.digest(),
            "edcfd3d2f1212aab"
        );
    }

    /// Four failures, four different places to go looking. Collapsing any two of them costs
    /// somebody an hour in the wrong one, which is the argument `health.rs` already makes about
    /// `NotRunning` versus `Missing`.
    #[test]
    fn every_failure_has_a_category_of_its_own() {
        use crate::health::FailureCategory;
        assert_eq!(
            Failure::NotConfigured.category(),
            FailureCategory::NotConfigured
        );
        assert_eq!(Failure::MissingCli.category(), FailureCategory::Missing);
        assert_eq!(
            Failure::MissingToken.category(),
            FailureCategory::PermissionDenied,
            "a missing credential may not read like a broken repository"
        );
        assert_eq!(Failure::TimedOut.category(), FailureCategory::Timeout);
        for failure in [
            Failure::NotConfigured,
            Failure::MissingCli,
            Failure::MissingToken,
            Failure::TimedOut,
        ] {
            assert!(!failure.to_string().is_empty());
        }
    }

    /// A pillar the owner switched off is refused before anything is spawned and before the
    /// Credential Manager is touched.
    #[tokio::test]
    async fn a_switched_off_pillar_executes_nothing() {
        let op = Op::Read(ReadOp::PrList { repo: repo() });
        let runtime = switched_off();
        assert_eq!(execute(&runtime, &op).await, Err(Failure::NotConfigured));
    }

    /// The tail survives and the head is what goes, because the tail of a failing workflow log is
    /// where the error is. And the cut lands on a character boundary, which a byte-wise slice of a
    /// log full of accented commit messages would not.
    #[test]
    fn clipping_keeps_the_tail_and_never_splits_a_character() {
        let short = "a short answer";
        assert_eq!(clip(short), short);

        let long = "\u{e1}".repeat(MAX_OUTPUT_BYTES);
        let clipped = clip(&long);
        assert!(clipped.starts_with("[... clipped"));
        assert!(clipped.ends_with('\u{e1}'));
        assert!(!clipped.contains('\u{fffd}'));
    }

    async fn test_pool() -> sqlx::SqlitePool {
        crate::testdb::fresh_shared_pool().await
    }

    fn runtime_with(actions: &[&str]) -> GithubRuntime {
        GithubRuntime::from_config(
            &GithubConfig {
                enabled: true,
                autonomous_reads: Vec::new(),
                autonomous_actions: actions.iter().map(|entry| (*entry).to_owned()).collect(),
            },
            true,
            // Never spawned by the tests that use this: they file a proposal, or run against a
            // pillar that is switched off. A name nothing installs is the honest value — if one of
            // them ever does start spawning, it fails as `MissingCli` instead of reaching a real
            // `gh` and doing something on GitHub from a test run.
            "nucleos-gh-never-spawned-here".to_owned(),
        )
    }

    /// A caller that asks the wrong tool is told SO, rather than being told it invented a word.
    /// Told "unknown operation: pr_comment" it would go looking for a typo in its own request; told
    /// where the operation lives it sends it there.
    #[test]
    fn from_request_says_which_tool_an_operation_belongs_to() {
        let asked_read_for_an_action = ReadOp::from_request(ReadRequest {
            operation: "pr_comment".to_owned(),
            repo: "owner/name".to_owned(),
            id: Some("1".to_owned()),
        });
        assert!(
            asked_read_for_an_action
                .unwrap_err()
                .contains("belongs to github_act")
        );

        let asked_act_for_a_read = ActOp::from_request(ActRequest {
            operation: "pr_view".to_owned(),
            repo: Some("owner/name".to_owned()),
            id: Some("1".to_owned()),
            ..ActRequest::default()
        });
        assert!(
            asked_act_for_a_read
                .unwrap_err()
                .contains("belongs to github_read")
        );

        let nonsense = ReadOp::from_request(ReadRequest {
            operation: "frobnicate".to_owned(),
            repo: "owner/name".to_owned(),
            id: None,
        });
        assert!(nonsense.unwrap_err().contains("unknown read operation"));
    }

    /// A missing field names itself, and `api_read` is settled before the repository is — otherwise the
    /// one operation that names no repository would be refused for not having one.
    #[test]
    fn from_request_names_the_field_it_is_missing() {
        let missing = ActOp::from_request(ActRequest {
            operation: "pr_comment".to_owned(),
            repo: Some("owner/name".to_owned()),
            id: Some("1".to_owned()),
            ..ActRequest::default()
        });
        assert!(missing.unwrap_err().contains("body"));

        let api_read = ActOp::from_request(ActRequest {
            operation: "api_read".to_owned(),
            args: Some(vec!["repos/o/r".to_owned()]),
            ..ActRequest::default()
        });
        assert_eq!(
            api_read.expect("api_read needs no repository").kind(),
            "api_read"
        );
    }

    /// `effect_of_kind` and `effect()` are one answer, because the first is derived from the same
    /// list the second is exhaustive over.
    #[test]
    fn the_effect_of_a_kind_is_the_effect_of_the_operation() {
        for op in ReadOp::all() {
            assert_eq!(ReadOp::effect_of_kind(op.kind()), Some(op.effect()));
        }
        assert_eq!(ReadOp::effect_of_kind("pr_comment"), None);
        assert_eq!(ReadOp::effect_of_kind(""), None);
    }

    /// **Off the list is not refused — it is filed, and the turn carries on.** That is the whole
    /// reading of this pillar, and a version that answered "denied" would be a different product.
    #[tokio::test]
    async fn an_action_off_the_list_is_filed_and_never_refused() {
        let pool = test_pool().await;
        let runtime = runtime_with(&["run_rerun"]);
        let op = Op::Act(ActOp::PrComment {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
            body: Body::new("a comment").expect("a body"),
        });

        let submitted = submit(&pool, &runtime, op).await.expect("filing succeeds");
        let Submitted::Filed { proposal_id, kind } = submitted else {
            panic!("an operation off the list must be filed, never run");
        };
        assert_eq!(kind, "pr_comment");

        let proposal = crate::proposals::get(&pool, proposal_id)
            .await
            .expect("the proposal is readable")
            .expect("the proposal exists");
        assert_eq!(proposal.kind, "github-action");
        assert_eq!(proposal.status, "pending");
        assert_eq!(
            proposal.tool_name.as_deref(),
            Some("pr_comment"),
            "the column the approvals list renders has to say what is being agreed to"
        );
    }

    /// A read never files, whatever the action list says. What limits reads is the effect and not
    /// the autonomy, and a read that queued for approval would be the two mixed up.
    #[tokio::test]
    async fn a_read_never_becomes_a_proposal() {
        let pool = test_pool().await;
        // Switched off, so `execute` refuses before spawning anything and the test needs no `gh`.
        // What is under test is which BRANCH a read takes, and the refusal proves it took the one
        // that executes rather than the one that files.
        let runtime = switched_off();
        let op = Op::Read(ReadOp::PrView {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
        });
        assert_eq!(
            submit(&pool, &runtime, op).await,
            Err(Failure::NotConfigured)
        );
        // Counted straight out of the table and not through `list_pending`, which filters
        // `kind = 'action-approval'` — asking it would have passed whether or not a row was written.
        let filed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM proposals WHERE kind = 'github-action'")
                .fetch_one(&pool)
                .await
                .expect("the table is readable");
        assert_eq!(filed, 0, "a read may never become a proposal");
    }

    /// **The claim comes before the run.** A second approval racing the first loses at the
    /// compare-and-set, which is what stops one comment being posted twice — and `execute` never
    /// retrying is the same argument from the other end.
    ///
    /// The pillar is switched off so `gh` is never spawned: what is under test is the ORDER of the
    /// two writes, and a failure to run is the sharpest way to see it. The row is `approved` with
    /// nothing published, and the note beneath it says so in words.
    #[tokio::test]
    async fn approving_claims_the_proposal_before_running_it() {
        let pool = test_pool().await;
        let op = Op::Act(ActOp::PrComment {
            repo: repo(),
            number: PrNumber::new("42").expect("42 is a number"),
            body: Body::new("a comment").expect("a body"),
        });
        let Submitted::Filed { proposal_id, .. } = submit(&pool, &runtime_with(&[]), op)
            .await
            .expect("filing succeeds")
        else {
            panic!("it should have been filed");
        };

        let off = switched_off();
        let ran = approve_proposed_operation(&pool, &off, proposal_id).await;
        assert!(
            matches!(ran, Err(DecisionError::Failed(Failure::NotConfigured))),
            "the operation could not run, and that is what the caller is told"
        );

        let proposal = crate::proposals::get(&pool, proposal_id)
            .await
            .expect("the proposal is readable")
            .expect("the proposal exists");
        assert_eq!(
            proposal.status, "approved",
            "the claim is what stops a second approval posting the same comment again"
        );

        assert!(
            matches!(
                approve_proposed_operation(&pool, &off, proposal_id).await,
                Err(DecisionError::NotPending)
            ),
            "a second approval must lose at the status check"
        );
    }

    /// A proposal whose payload is not an operation is refused rather than guessed at, and it is
    /// refused by the same validating `Deserialize` every other road in uses — so a row edited by
    /// hand in the database months later cannot put a dashed string on a command line.
    #[tokio::test]
    async fn a_proposal_carrying_no_usable_operation_is_refused() {
        let pool = test_pool().await;
        for payload in [
            "not json at all",
            r#"{"op":"pr_comment","repo":"--upload-pack=x","number":"1","body":"hi"}"#,
            r#"{"op":"no_such_operation"}"#,
        ] {
            let proposal_id =
                crate::proposals::create_github_action(&pool, "pr_comment", "why", payload)
                    .await
                    .expect("the proposal is written");
            assert!(
                matches!(
                    approve_proposed_operation(&pool, &runtime_with(&[]), proposal_id).await,
                    Err(DecisionError::Malformed)
                ),
                "{payload}"
            );
        }
    }

    /// The ceilings hold to the operations. An `ACTION_CEILING` entry naming a kind no `ActOp` has
    /// would be a line in the owner's file that grants nothing and says it grants something.
    #[test]
    fn every_action_ceiling_entry_names_a_real_operation() {
        let kinds: Vec<&str> = ActOp::all().iter().map(|op| op.kind()).collect();
        for entry in ACTION_CEILING {
            assert!(kinds.contains(entry), "{entry} is not an ActOp kind");
        }
        assert!(
            !ACTION_CEILING.contains(&"api_read"),
            "`api_read` is never eligible for autonomy"
        );
    }

    /// Decision #7 as behaviour, and not as a constant that happens to lack a word.
    ///
    /// The assertion above says `ACTION_CEILING` does not contain the string `api_read`. That is
    /// worth having and it is not the claim: the claim is that a `Policy` TOLD to allow `api_read`
    /// by every route that can build one still refuses. So this asks each route in turn, and asks
    /// the reading side too, because an operation refused as an action and admitted as a command
    /// would be the same capability through the other door.
    ///
    /// `Policy` has three constructors — `empty`, `from_config` and `for_project` — and only the
    /// last two can be told anything. That is why this is a complete enumeration rather than a
    /// sample, and why adding a fourth constructor is a change that has to come back here.
    ///
    /// **Route three is the one this chunk added, and it is the sharpest of the three.** A row in
    /// `project_github_ops` reaches `read_is_autonomous` through `op_kind_of_gh_command`, and that
    /// map DOES name `gh api …` — `ApiRead`'s argv is `api -- …`, so the form matches and the answer
    /// is `Some("api_read")`. Nothing about the map refuses it. What refuses it is `for_project`
    /// narrowing the stored rows against `declarable_ops` on the way OUT, which is the whole reason
    /// the read side narrows at all when the write route already validated: this row is written
    /// straight into the table below, exactly as an out-of-band write or a pre-ceiling row would
    /// arrive, and without that narrowing `gh api -X DELETE` would run in Bash unasked.
    #[tokio::test]
    async fn api_read_is_never_autonomous_by_any_route_that_can_ask_for_it() {
        // Route one: the owner's `~/.nucleos/github.yaml`, asking for it in both lists and asking beside
        // entries the ceilings DO admit — so a narrowing that dropped the whole file would satisfy
        // this by accident and the kept entries prove it did not.
        let owner = policy_from(Some(
            "autonomous_reads:\n  - gh api\n  - gh run list\n\
             autonomous_actions:\n  - api_read\n  - pr_create\n",
        ));
        assert_eq!(owner.autonomous_actions(), ["pr_create"]);
        assert_eq!(owner.autonomous_reads(), ["gh run list"]);

        // Route two: a `GithubConfig` built in code from the ceilings themselves — the widest policy
        // this codebase can construct, and the one `http::declarable_github_ops` asks in order to
        // decide what a project may put in `project_github_ops`. A project cannot declare what this
        // policy refuses, so refusing here is what keeps the per-project table from becoming the
        // ceiling with a different door on it.
        let widest = Policy::from_config(&crate::config::GithubConfig {
            enabled: true,
            autonomous_reads: READ_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
            autonomous_actions: ACTION_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
        });
        assert!(
            widest
                .autonomous_actions()
                .contains(&"pr_create".to_owned())
        );

        // Route three: a project's own table, written straight past the route that validates it, and
        // laid over the two widest machine policies there are.
        let pool = test_pool().await;
        for kind in ["api_read", "run_list", "pr_create"] {
            sqlx::query(
                "INSERT INTO project_github_ops (project_id, op_kind, created_at)
                 VALUES ('alpha', ?, datetime('now'))",
            )
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        let declared = widest.for_project(&pool, "alpha").await;
        let declared_over_nothing = Policy::empty().for_project(&pool, "alpha").await;
        // The kept rows prove the narrowing dropped one entry and not the whole table, the way the
        // owner's route above proves it.
        assert!(declared_over_nothing.read_is_autonomous("gh run list -R owner/name"));
        assert!(declared_over_nothing.action_is_autonomous("pr_create"));

        for policy in [
            Policy::empty(),
            Policy::default(),
            owner,
            widest,
            declared,
            declared_over_nothing,
        ] {
            assert!(
                !policy.action_is_autonomous("api_read"),
                "api_read is never an autonomous action"
            );
            // And not through the reading side either: the argv `ApiRead` actually builds matches no
            // `READ_CEILING` prefix, so `gh api` is refused as a command exactly as it is refused as
            // an operation.
            let argv = ActOp::ApiRead {
                args: vec!["repos/owner/name".to_owned()],
            }
            .argv();
            assert!(!policy.read_is_autonomous(&format!("gh {}", argv.join(" "))));
            assert!(!policy.read_is_autonomous("gh api repos/owner/name"));
            // The line the map WOULD name `api_read`, spelled the way it would cost the most.
            assert!(!policy.read_is_autonomous("gh api -X DELETE repos/owner/name"));
        }
        // Said once, outside the loop, because it is a fact about the map rather than about any
        // policy: the map names this line, and the name is not a grant.
        assert_eq!(
            op_kind_of_gh_command("gh api repos/owner/name"),
            Some("api_read"),
            "the map names it; the ceiling is what refuses it"
        );
    }

    /// The non-regression, proved rather than argued: a project that declared nothing IS the machine
    /// default, value for value.
    ///
    /// Equality over the whole `Policy` and not over its answers, which is the stronger claim and
    /// the cheaper one to keep: it covers the two lists, the third list this chunk added, and the
    /// `digest` — the last of which labels rows in a live scoreboard, so an overlay that renumbered
    /// it would tell `shadow::shadow_readiness` that one action class spans two policies on a
    /// machine where nothing changed.
    ///
    /// Asked of a machine default with entries as well as of an empty one, because the interesting
    /// direction is the one where there is something to lose.
    #[tokio::test]
    async fn a_project_that_declared_nothing_is_the_machine_default_exactly() {
        let pool = test_pool().await;
        let owner = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n\
             autonomous_actions:\n  - pr_comment\n",
        ));
        for machine in [Policy::empty(), Policy::default(), owner] {
            let for_project = machine.for_project(&pool, "alpha").await;
            assert_eq!(
                for_project, machine,
                "a project with no rows is the machine default and not a copy of it"
            );
            assert_eq!(for_project.digest(), machine.digest());
        }
    }

    /// The widening half, and the reason any of this exists: an operation this project declared runs
    /// without asking HERE and nowhere else.
    ///
    /// The machine default is empty throughout, so nothing but the project's own row can be
    /// producing the `true` — which is what makes this the test a `for_project` that ignored its
    /// rows would fail.
    #[tokio::test]
    async fn a_declared_operation_runs_without_asking_in_this_project_and_in_no_other() {
        let pool = test_pool().await;
        crate::project::policy::declare_github_op(&pool, "alpha", "run_list")
            .await
            .unwrap();
        crate::project::policy::declare_github_op(&pool, "alpha", "pr_comment")
            .await
            .unwrap();

        let machine = Policy::empty();
        let alpha = machine.for_project(&pool, "alpha").await;
        let beta = machine.for_project(&pool, "beta").await;

        assert!(alpha.read_is_autonomous("gh run list -R owner/name"));
        assert!(alpha.action_is_autonomous("pr_comment"));

        assert!(!beta.read_is_autonomous("gh run list -R owner/name"));
        assert!(!beta.action_is_autonomous("pr_comment"));
        assert!(!machine.read_is_autonomous("gh run list -R owner/name"));
        assert!(!machine.action_is_autonomous("pr_comment"));

        // A different effective policy gets a different label, which is the one property `digest`
        // has to have.
        assert_ne!(alpha.digest(), machine.digest());
        assert_eq!(beta.digest(), machine.digest());
    }

    /// It ADDS to the machine default; it does not replace it. A project that declares one operation
    /// keeps every grant the owner's file already gave the machine.
    #[tokio::test]
    async fn a_projects_declaration_adds_to_the_machine_default_and_never_replaces_it() {
        let pool = test_pool().await;
        crate::project::policy::declare_github_op(&pool, "alpha", "pr_list")
            .await
            .unwrap();

        let machine = policy_from(Some(
            "autonomous_reads:\n  - gh run list\n\
             autonomous_actions:\n  - pr_comment\n",
        ));
        let alpha = machine.for_project(&pool, "alpha").await;

        // The project's own row, granted.
        assert!(alpha.read_is_autonomous("gh pr list -R owner/name"));
        // And the machine's two, still standing. Under a replacement reading both of these would be
        // false, and the owner would have had no way to write down that they wanted them kept.
        assert!(alpha.read_is_autonomous("gh run list -R owner/name"));
        assert!(alpha.action_is_autonomous("pr_comment"));
    }

    /// The ceiling is a ceiling on the way OUT of the table too.
    ///
    /// Every row here is one `POST /projects/{id}/github-ops` would refuse, written straight into
    /// the table the way a row stored before a ceiling narrowed — or written out of band — would
    /// arrive. `run_logs` and `pr_view` are the interesting pair: both are real `ReadOp`s the map
    /// names, so nothing about the naming refuses them, and `pr_view` shares no flag with anything —
    /// it is outside `READ_CEILING` and that alone is what stops it.
    #[tokio::test]
    async fn a_row_outside_the_ceiling_grants_nothing_however_it_got_there() {
        let pool = test_pool().await;
        for kind in ["run_logs", "pr_view", "issue_view", "pr_diff", "api_read"] {
            sqlx::query(
                "INSERT INTO project_github_ops (project_id, op_kind, created_at)
                 VALUES ('alpha', ?, datetime('now'))",
            )
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        let alpha = Policy::empty().for_project(&pool, "alpha").await;

        for command in [
            "gh run view --log 1 -R owner/name",
            "gh pr view 7 -R owner/name",
            "gh issue view 7 -R owner/name",
            "gh pr diff 7 -R owner/name",
            "gh api repos/owner/name",
        ] {
            assert!(
                !alpha.read_is_autonomous(command),
                "{command:?} is outside the ceiling whatever the table says"
            );
        }
        // And the whole overlay is inert, digest included: five refused rows are no rows.
        assert_eq!(alpha, Policy::empty());
    }

    /// A declared ACTION does not open the reading door in Bash.
    ///
    /// Decision #5 keeps the partition in the types, and this is what it buys once one list of names
    /// governs both doors: `op_kind_of_gh_command` names `gh pr comment` perfectly well, and the
    /// project declared `pr_comment`, so the only thing between that line and running unattended is
    /// `read_ops` holding no action.
    ///
    /// It is the twin of a sentence the machine's own square already measures — "no action is ever
    /// autonomous in Bash", in `the_map_names_an_operation_and_does_not_yet_make_the_two_doors_agree`
    /// — asked of the door that did not exist when that was written. There it holds by ARITHMETIC,
    /// because no `ACTION_CEILING` kind has a `READ_CEILING` prefix; here the prefixes are gone and
    /// only the partition is left holding it up.
    #[tokio::test]
    async fn a_declared_action_never_becomes_an_autonomous_bash_line() {
        let pool = test_pool().await;
        for kind in declarable_act_ops() {
            crate::project::policy::declare_github_op(&pool, "alpha", kind)
                .await
                .unwrap();
        }
        let alpha = Policy::empty().for_project(&pool, "alpha").await;

        for op in ActOp::all() {
            let line = format!("gh {}", op.argv().join(" "));
            assert!(
                !alpha.read_is_autonomous(&line),
                "{line:?} acts, and no declaration makes an action an autonomous read"
            );
        }
        // The one that would hurt most, spelled by hand so the assertion survives a change to
        // `PrComment`'s argv.
        assert!(!alpha.read_is_autonomous("gh pr comment 7 --body hello -R owner/name"));
        // ...while the typed door answers yes, which is what makes the line above a partition and
        // not an accident of spelling.
        assert!(alpha.action_is_autonomous("pr_comment"));
    }

    /// **A name from the map is not a grant.** The refused flags bind the named door exactly as they
    /// bind the prefix one, and they run BEFORE it.
    ///
    /// `--log` and `--log-failed` are the flags that change WHICH operation a line is, and the map
    /// has its own guard for those. The rest of `REFUSED_READ_FLAGS` is a different job: `--json`
    /// and `--jq` change the SHAPE of what comes back, and `--limit` bounds HOW MUCH — `ReadOp::
    /// effect` grades `PrList` as `ReadsOwn` precisely because a stranger reaches it through thirty
    /// short fields and no further. A mapped name allowed past this check would take that grading
    /// with it, and `gh pr list --limit 1000` would be a thousand stranger-chosen titles in a call
    /// that marks nothing.
    #[tokio::test]
    async fn a_declared_name_does_not_survive_a_flag_that_changes_what_comes_back() {
        let pool = test_pool().await;
        for kind in ["run_status", "pr_list", "run_list"] {
            crate::project::policy::declare_github_op(&pool, "alpha", kind)
                .await
                .unwrap();
        }
        let alpha = Policy::empty().for_project(&pool, "alpha").await;

        // The plain forms run, or there would be nothing to take away below.
        assert!(alpha.read_is_autonomous("gh run view 1 -R owner/name"));
        assert!(alpha.read_is_autonomous("gh pr list -R owner/name"));

        for refused in [
            "gh run view --log 1 -R owner/name",
            "gh run view --log-failed 1 -R owner/name",
            "gh pr list --limit 1000 -R owner/name",
            "gh pr list -L 1000 -R owner/name",
            "gh pr list --json body -R owner/name",
            "gh pr list --jq .[] -R owner/name",
            "gh run list --search in:body -R owner/name",
        ] {
            assert!(
                !alpha.read_is_autonomous(refused),
                "{refused:?} carries a flag that changes what comes back"
            );
        }
    }

    /// A pillar the owner switched off stays off, whatever a project declared.
    ///
    /// `Policy::from_config` collapses a disabled pillar to `empty()`, so by the time a `Policy`
    /// exists "switched off" and "granted nothing" are the same value and the layering cannot tell
    /// them apart. `GithubRuntime::policy_for_project` is where the switch is read, and this is the
    /// test that says so — layering onto the collapsed value directly, as the second half shows,
    /// would hand a row the pillar's own off switch.
    #[tokio::test]
    async fn a_switched_off_pillar_stays_off_whatever_the_project_declared() {
        let pool = test_pool().await;
        crate::project::policy::declare_github_op(&pool, "alpha", "run_list")
            .await
            .unwrap();

        let off = GithubRuntime {
            enabled: false,
            ..GithubRuntime::default()
        };
        let policy = off.policy_for_project(&pool, "alpha").await;
        assert_eq!(policy, Policy::empty());
        assert!(!policy.read_is_autonomous("gh run list -R owner/name"));

        // The same project, the same row, through a pillar that is on.
        let on = GithubRuntime::default();
        assert!(
            on.policy_for_project(&pool, "alpha")
                .await
                .read_is_autonomous("gh run list -R owner/name"),
            "the row is a real grant; the switch is what withheld it above"
        );
    }

    /// `pr_create` is inside the ceiling; every other route to it is unchanged.
    ///
    /// A pull request is a proposal a human still has to act on, which is what puts it beside
    /// `pr_comment` rather than beside `api_read`. The file may still narrow it away, and the test
    /// says so: the ceiling decides what MAY be autonomous and the owner decides what is.
    #[test]
    fn opening_a_pull_request_is_inside_the_ceiling_and_still_the_owners_choice() {
        assert!(ACTION_CEILING.contains(&"pr_create"));

        let asked = policy_from(Some("autonomous_actions:\n  - pr_create\n"));
        assert!(asked.action_is_autonomous("pr_create"));

        let did_not_ask = policy_from(Some("autonomous_actions:\n  - pr_comment\n"));
        assert!(
            !did_not_ask.action_is_autonomous("pr_create"),
            "a ceiling entry the file does not name grants nothing"
        );
    }

    /// The reading ceiling holds to the operations too, and it is the half that had drifted.
    ///
    /// `gh workflow list` sat in `READ_CEILING` with no `ReadOp` building it, which is the mirror of
    /// the failure the test above guards against: a prefix that grants a capability the typed path
    /// cannot ask for is a line the owner reads as a permission and that permits nothing. The
    /// direction is ceiling to operation and not the reverse — a `ReadOp` outside the ceiling is
    /// ordinary (`pr_view` is one, deliberately), a ceiling entry outside the operations is not.
    #[test]
    fn every_read_ceiling_prefix_is_built_by_a_real_operation() {
        let commands: Vec<String> = ReadOp::all()
            .iter()
            .map(|op| format!("gh {}", op.argv().join(" ")))
            .collect();
        for prefix in READ_CEILING {
            assert!(
                commands
                    .iter()
                    .any(|command| command.starts_with(&format!("{prefix} "))),
                "{prefix} is in the ceiling and no ReadOp builds it"
            );
        }
    }

    /// The `gh` line an operation builds, as somebody would have typed it into Bash.
    fn gh_line(op: &Op) -> String {
        format!("gh {}", op.argv().join(" "))
    }

    /// **Decision #4's test, and the only one that would catch the map drifting from the
    /// operations.**
    ///
    /// One list of NAMES can govern both doors only if a name picks out the SAME operation in each
    /// vocabulary. So: enable exactly one kind, then ask about every operation twice — once the way
    /// the typed door asks (is this the kind that is enabled?) and once the way the Bash door would
    /// have to ask it (does the map name this command line as the kind that is enabled?). The two
    /// answers have to be the same, for all sixteen operations, sixteen times over.
    ///
    /// **What makes this more than a tautology is the pair `gh run view` serves.** A map that named
    /// both halves `run_status` would answer yes to `run_logs`'s command line while the typed door
    /// said no, and one cell of this square is where that shows up; `gh pr view` is the same trap a
    /// second time. It is written as a loop and not as a list of cases on purpose — a case per
    /// operation is a second place to forget the eleventh operation, which is the drift the whole
    /// decision exists to stop.
    ///
    /// **What it does NOT prove is that the two doors behave alike today.** They do not, and the
    /// test below measures by how much. This proves the map, which is the piece that was missing.
    #[test]
    fn one_name_picks_out_the_same_operation_at_both_doors() {
        let operations = Op::all();
        for enabled in operations.iter().map(Op::kind) {
            for op in &operations {
                let line = gh_line(op);
                let typed_door = op.kind() == enabled;
                let bash_door = op_kind_of_gh_command(&line) == Some(enabled);
                assert_eq!(
                    typed_door,
                    bash_door,
                    "with {enabled} enabled, the typed door says {typed_door} about {} and the map \
                     reads {line:?} as {:?}",
                    op.kind(),
                    op_kind_of_gh_command(&line)
                );
            }
        }
    }

    /// What the map does not do, measured rather than promised.
    ///
    /// The map names an operation; it does not change who runs it. Under the WIDEST policy a
    /// `~/.nucleos/github.yaml` can express — both ceilings, whole — the two doors still answer differently
    /// about eleven of the sixteen operations, and this pins which eleven. Reads run typed whatever
    /// the list says (`submit`: *«A READ never files a proposal, and that is not an omission»*), and
    /// no action is ever autonomous in Bash because no `ACTION_CEILING` kind has a `READ_CEILING`
    /// prefix.
    ///
    /// **`Policy::for_project` has since landed and this test did not fail, which is the fact worth
    /// recording rather than the promise it replaces.** The paragraph here said the single list
    /// would be what closed the gap. It is not, and the second half below measures why: the project
    /// list widens the BASH door by name, and `submit` — the typed door — reads `runtime.policy`,
    /// the machine default, because `POST /github/requests` carries no project. So a project that
    /// declares every operation it may declare disagrees with the typed door about exactly the same
    /// eleven. Decision #6 remains the only thing that closes it.
    ///
    /// **And the second arm is a REMINDER, not a tripwire — which this paragraph, of all paragraphs,
    /// must not get wrong twice.** It was written closing with "what will fail, correctly and loudly,
    /// on the day the typed door learns which project it is acting for", which is the same species of
    /// promise the paragraph exists to retract. It will not fail then: the closure below hardcodes
    /// `widest.action_is_autonomous(...)` and never calls `submit`, so wiring `submit` to a project
    /// changes nothing here until somebody edits that closure. What the arm does is state today's
    /// measurement where whoever does that wiring will be standing, with a comment inside the closure
    /// telling them which line to change. That is worth having and it is not a guard.
    #[tokio::test]
    async fn the_map_names_an_operation_and_does_not_yet_make_the_two_doors_agree() {
        let widest = Policy::from_config(&crate::config::GithubConfig {
            enabled: true,
            autonomous_reads: READ_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
            autonomous_actions: ACTION_CEILING
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
        });
        let operations = Op::all();

        let in_bash: Vec<&str> = operations
            .iter()
            .filter(|op| widest.read_is_autonomous(&gh_line(op)))
            .map(Op::kind)
            .collect();
        assert_eq!(
            in_bash,
            ["run_list", "run_status", "pr_list", "workflow_list"],
            "the Bash door is the four structural reads and nothing else"
        );

        // The typed door, for the same sixteen, as `submit` decides it.
        let typed = |op: &Op| match op {
            Op::Read(_) => true,
            Op::Act(act) => widest.action_is_autonomous(act.kind()),
        };
        let disagreeing: Vec<&str> = operations
            .iter()
            .filter(|op| typed(op) != widest.read_is_autonomous(&gh_line(op)))
            .map(Op::kind)
            .collect();
        assert_eq!(
            disagreeing,
            [
                // Reads the ceiling refuses because they carry a stranger's words, and `run_logs`,
                // which the ceiling would admit by prefix and `--log` takes back.
                "pr_view",
                "issue_view",
                "run_logs",
                "pr_diff",
                "pr_thread",
                "checks_for_ref",
                // Every action inside `ACTION_CEILING`: autonomous typed, never autonomous in Bash.
                "workflow_run",
                "run_rerun",
                "pr_create",
                "pr_comment",
                "issue_close",
            ],
            "eleven of sixteen, and `api_read` agrees only because both doors refuse it"
        );

        // And the same square again, for a project that declared everything it MAY declare — the
        // widest per-project policy there is, laid over the widest machine one. Same eleven, because
        // `for_project` reaches only the Bash door.
        let pool = test_pool().await;
        for kind in declarable_ops() {
            crate::project::policy::declare_github_op(&pool, "alpha", kind)
                .await
                .unwrap();
        }
        let declared = widest.for_project(&pool, "alpha").await;
        let typed_for_project = |op: &Op| match op {
            // **THIS is the line to change when `submit` learns which project it is acting for**,
            // and nothing will fail to tell you so — the header says why that is a reminder and not
            // a guard. `widest` is written deliberately, because `submit` reads `runtime.policy` and
            // never a project's: swap it for `declared` on that day and the assertion below is the
            // measurement of how much the gap actually closed.
            Op::Read(_) => true,
            Op::Act(act) => widest.action_is_autonomous(act.kind()),
        };
        let still_disagreeing: Vec<&str> = operations
            .iter()
            .filter(|op| typed_for_project(op) != declared.read_is_autonomous(&gh_line(op)))
            .map(Op::kind)
            .collect();
        assert_eq!(
            still_disagreeing, disagreeing,
            "declaring everything a project may declare moves the Bash door and not the typed one"
        );
    }

    /// The pair that makes the map a map and not a prefix table, asked the way a person types it.
    #[test]
    fn one_subcommand_two_operations_and_a_flag_between_them() {
        assert_eq!(op_kind_of_gh_command("gh run view 123"), Some("run_status"));
        assert_eq!(
            op_kind_of_gh_command("gh run view --log 123"),
            Some("run_logs")
        );
        // The flag after the positional, which is where a person actually puts it.
        assert_eq!(
            op_kind_of_gh_command("gh run view 123 --log"),
            Some("run_logs")
        );
        // pflag's other spelling for a boolean — the one `REFUSED_READ_FLAGS` had to learn too.
        assert_eq!(
            op_kind_of_gh_command("gh run view 123 --log=true"),
            Some("run_logs")
        );
        assert_eq!(op_kind_of_gh_command("gh pr view 7"), Some("pr_view"));
        assert_eq!(
            op_kind_of_gh_command("gh pr view --comments 7"),
            Some("pr_thread")
        );

        // A flag carrying a caller's value names nothing and takes nothing away, in either spelling.
        assert_eq!(
            op_kind_of_gh_command("gh run view -R owner/name 123"),
            Some("run_status")
        );
        assert_eq!(
            op_kind_of_gh_command("gh run view --repo=owner/name 123"),
            Some("run_status")
        );
        // Case-sensitive about the flag, exactly as `read_is_autonomous` is, and for its reason:
        // `gh` would not read `--LOG` as `--log` either.
        assert_eq!(
            op_kind_of_gh_command("gh run view --LOG 123"),
            Some("run_status")
        );
        // Past the terminator it is a positional and not a flag.
        assert_eq!(
            op_kind_of_gh_command("gh run view -- 123"),
            Some("run_status")
        );
    }

    /// The map is an allowlist because the operations are a closed set, so everything else is
    /// nameless — including the four reads `READ_CEILING`'s doc refuses by name.
    #[test]
    fn a_command_no_operation_builds_has_no_name() {
        for command in [
            "gh auth token",
            "gh auth status",
            "gh secret list",
            "gh variable list",
            "gh run download 1",
            // The whole-word compare `read_is_autonomous` needs a prefix rule for.
            "gh run listen",
            "gh runlist",
            "gh",
            "git status",
            "",
        ] {
            assert_eq!(op_kind_of_gh_command(command), None, "{command:?}");
        }
    }

    /// **A flag that changes what comes back changes which operation it is.**
    ///
    /// `--log-failed` is the case the derivation cannot reach: no `ReadOp` builds it, so no form
    /// names it, and without the guard `gh run view --log-failed 1` is named `run_status` — a
    /// `ReadsOwn` operation, inside `READ_CEILING`, and one a project may declare — while returning
    /// the log of a failed step, which is a stranger's words. The map would be a prefix table with
    /// respect to exactly the spelling its own doc says a prefix table gets wrong.
    ///
    /// `REFUSED_READ_FLAGS` already holds that knowledge, under its own heading: *«The first seven
    /// change the KIND of thing that comes back»*. So the map reads that list rather than growing a
    /// second one, and answers `None` — the same rule the tie-break applies, one place further. A
    /// guess is a worse answer than no answer when the caller is deciding autonomy with it.
    ///
    /// The test above covers `gh auth token` and `gh secret list` and not one flag-bearing line,
    /// which is why this gap was untested as well as unhandled.
    #[test]
    fn a_flag_that_changes_what_comes_back_takes_the_name_away() {
        for command in [
            // The one that motivated the guard, in all three spellings a person reaches for.
            "gh run view --log-failed 1",
            "gh run view 1 --log-failed",
            "gh run view --log-failed=true 1",
            // The rest of the constant, on the prefixes the ceiling does admit.
            "gh run view --jq .jobs 1",
            "gh pr list --json body",
            "gh pr list --limit 1000",
            "gh pr list -L 1000",
            "gh pr list --search in:body secret",
            "gh pr list -t {{.body}}",
        ] {
            assert_eq!(op_kind_of_gh_command(command), None, "{command:?}");
        }

        // The flag an operation NAMES is the flag that names it. `--log` is on that constant too,
        // and `run_logs` is the row that exists to carry it — the exemption is what keeps a refusal
        // written for prefixes from eating the operation a name can spell.
        assert_eq!(
            op_kind_of_gh_command("gh run view --log 1"),
            Some("run_logs")
        );
        // `--comments` is not on the constant at all, so the other pair is untouched.
        assert_eq!(
            op_kind_of_gh_command("gh pr view --comments 7"),
            Some("pr_thread")
        );
        // A flag that only chooses WHICH takes nothing away, and `-l` is `--label` while `-L` is
        // `--limit`: the case sensitivity `read_is_autonomous` keeps, kept here for the same reason.
        for (command, kind) in [
            ("gh pr list --state open", "pr_list"),
            ("gh pr list --author octocat", "pr_list"),
            ("gh pr list -l bug", "pr_list"),
            ("gh run list --branch main", "run_list"),
        ] {
            assert_eq!(op_kind_of_gh_command(command), Some(kind), "{command:?}");
        }
    }

    /// What makes "the most switches accounted for" a safe way to break a tie: inside one
    /// subcommand no two operations carry the same number of switches, so the winner is never a coin
    /// toss.
    ///
    /// **It rules out one tie and not every tie, and the difference is worth stating rather than
    /// implied.** `matches` needs the subcommand to be a PREFIX of the line, so two forms whose
    /// subcommands are of different lengths — a hypothetical `gh api` beside a `gh api graphql` —
    /// can both match, and this loop walks past that pair on the `continue`. That case is safe
    /// without being asserted here: the sort's first key is the subcommand's length, so the more
    /// specific form wins, which is the answer anybody would want. What would NOT be safe is two
    /// forms of the same shape, and that is the tie this rules out.
    #[test]
    fn no_two_operations_wear_the_same_gh_form() {
        let forms = gh_forms();
        assert_eq!(forms.len(), Op::all().len(), "one form per operation");
        for (index, one) in forms.iter().enumerate() {
            for other in &forms[index + 1..] {
                if one.subcommand != other.subcommand {
                    continue;
                }
                assert_ne!(
                    one.switches.len(),
                    other.switches.len(),
                    "{} and {} share `gh {}` and nothing tells them apart",
                    one.kind,
                    other.kind,
                    one.subcommand.join(" ")
                );
            }
        }
    }

    /// The operation the ceiling had been granting to nobody: structural, and matching its prefix.
    #[test]
    fn workflow_list_is_a_structural_read_the_ceiling_already_admitted() {
        let op = ReadOp::WorkflowList { repo: repo() };
        assert_eq!(op.kind(), "workflow_list");
        assert_eq!(op.effect(), ToolEffect::ReadsOwn);
        assert_eq!(op.argv(), vec!["workflow", "list", "--repo=owner/name"]);

        // The prefix was always there; what is new is that a typed operation reaches it.
        assert!(READ_CEILING.contains(&"gh workflow list"));
        let policy = policy_from(Some("autonomous_reads:\n  - gh workflow list\n"));
        assert!(policy.read_is_autonomous(&format!("gh {}", op.argv().join(" "))));

        // And it is buildable from the flat parameters, or the tool could not ask for it.
        assert_eq!(
            ReadOp::from_request(ReadRequest {
                operation: "workflow_list".to_owned(),
                repo: "owner/name".to_owned(),
                id: None,
            }),
            Ok(op)
        );
    }
}
