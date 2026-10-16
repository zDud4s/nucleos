//! The wider area a conversation may work in: its root, the other checkouts of the same repository,
//! and the system temp directory.
//!
//! **Only a conversation gets it, and only for the length of one call.** An unattended run is
//! confined to its own worktree on purpose — that isolation is what lets several of them work at
//! once — so nothing here is on unless `within` is wrapped around the call. A rooted turn is the
//! owner working, and the owner's work spans the project's worktrees and logs written to `$TEMP`:
//! measured on the owner's transcripts on 2026-10-15, 384 of 400 asks the chat judge could not
//! approve were a `cd` into a sibling worktree or a log redirected to the temp directory.
//!
//! The comparison stays lexical, as everywhere else in the classifier: these are more roots for
//! the same `normalize_path` + `fold_for_containment` test, nothing more.

use super::*;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// The roots besides the workspace, and the temp directory, both normalised and folded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Area {
    roots: Vec<String>,
    temp: Option<String>,
}

impl Area {
    pub fn new(roots: &[PathBuf], temp: Option<&Path>) -> Self {
        let fold =
            |path: &Path| fold_for_containment(&normalize_path(&path.to_string_lossy(), None));
        Self {
            roots: roots.iter().map(|root| fold(root)).collect(),
            temp: temp.map(fold),
        }
    }
}

impl Area {
    /// The same area with more roots, for a measurement that has to rebuild checkouts the disk no
    /// longer holds.
    #[cfg(test)]
    pub fn with_roots(mut self, roots: &[PathBuf]) -> Self {
        self.roots.extend(Self::new(roots, None).roots);
        self
    }
}

// The initializer IS `const`; clippy 1.97 still reports it inside the macro.
#[allow(clippy::missing_const_for_thread_local)]
mod active {
    use super::Area;
    use std::cell::RefCell;
    thread_local! {
        pub(super) static ACTIVE: RefCell<Option<Area>> = const { RefCell::new(None) };
    }
}
use active::ACTIVE;

/// Runs `f` with `area` counted as inside the run. Restored afterwards, also on a panic, so a
/// conversation's area can never leak into the next call this thread classifies.
pub fn within<T>(area: &Area, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Area>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            ACTIVE.with(|active| *active.borrow_mut() = previous);
        }
    }
    let _restore = Restore(ACTIVE.with(|active| active.replace(Some(area.clone()))));
    f()
}

/// PURE but for the active area: whether a resolved, folded path lies in the workspace or, inside
/// `within`, in one of the area's roots or the temp directory.
pub(super) fn inside(resolved: &str, workspace: &str) -> bool {
    let under = |root: &str| resolved == root || resolved.starts_with(&format!("{root}/"));
    under(workspace)
        || ACTIVE.with(|active| {
            active.borrow().as_ref().is_some_and(|area| {
                area.roots.iter().any(|root| under(root)) || area.temp.as_deref().is_some_and(under)
            })
        })
}

/// The temp directory a path names through the shell, spelled out, when an area is active:
/// `$TEMP/x`, `${TMP}/x`, `$TMPDIR/x`, `$env:TEMP\x`, `%TEMP%\x`, and Git bash's `/tmp/x`.
/// `None` for anything else, which keeps its old reading: a `$` the shell rewrites is outside.
pub(super) fn expand_temp(
    candidate: &str,
    shell: Option<crate::command_reader::Shell>,
) -> Option<String> {
    let temp = ACTIVE.with(|active| active.borrow().as_ref().and_then(|area| area.temp.clone()))?;
    let lowered = candidate.to_ascii_lowercase();
    let spellings = [
        "$temp",
        "${temp}",
        "$tmp",
        "${tmp}",
        "$tmpdir",
        "${tmpdir}",
        "$env:temp",
        "$env:tmp",
        "%temp%",
        "%tmp%",
    ];
    let rest = spellings
        .iter()
        .find_map(|spelling| {
            lowered
                .strip_prefix(spelling)
                .filter(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
                .map(|rest| &candidate[candidate.len() - rest.len()..])
        })
        .or_else(|| {
            (shell == Some(crate::command_reader::Shell::Posix))
                .then(|| lowered.strip_prefix("/tmp"))
                .flatten()
                .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                .map(|rest| &candidate[candidate.len() - rest.len()..])
        })?;
    // Anything the shell would still rewrite past the prefix is not a place this can name.
    if rest.contains(['$', '%', '~']) {
        return None;
    }
    Some(format!("{temp}{}", rest.replace('\\', "/")))
}

/// Whether an area is active on this thread, that is, whether the call runs inside `within`.
///
/// The POSIX reader (`command_reader::parse`) spells assigned literals out for every line it reads,
/// because it cannot know who asks; whether that spelling may be USED is this question, and the
/// answer is the same one `inside` gives: only for the length of a conversation's call.
pub(super) fn active() -> bool {
    ACTIVE.with(|active| active.borrow().is_some())
}

/// A PowerShell line with the variables it assigned itself spelled out, when an area is active and
/// the line has any: `$S = "<scratchpad>"; Get-Content $S\gate.log` is read as the path it is,
/// rather than as a `$` the shell rewrites into somewhere unknown. `None` leaves the line as it was.
///
/// POSIX lines are NOT spelled here. `command_reader::parse` reads them with a real grammar and
/// remembers an assignment only for the commands of its own scope (not out of a subshell, a branch,
/// a loop body or a pipeline), which is what this lexical pass could never tell; its answer is
/// `SimpleCommand::spelled`. This pass stays for PowerShell, whose grammar the parser does not read
/// until phase 2 of the command-parser design.
///
/// Only a segment that is nothing but one literal assignment counts (`$NAME = value`), and only for
/// the segments after it. A value holding a `$`, a backtick or a `(` is not literal and is not
/// recorded, so the reference keeps its old reading. Lexical, like the rest: an assignment inside a
/// branch that did not run is spelled out anyway, and what is spelled out is still checked like any
/// other path.
pub(super) fn spelled_out_powershell(command: &str) -> Option<String> {
    use crate::command_reader::Shell;
    if !active() || !command.contains('$') {
        return None;
    }
    let base = command.as_ptr() as usize;
    let mut assigned: Vec<(String, String)> = Vec::new();
    let mut out = String::with_capacity(command.len());
    let mut copied = 0;
    for segment in crate::command_reader::segments(command, Shell::PowerShell) {
        let start = (segment.as_ptr() as usize).wrapping_sub(base);
        if start < copied || start + segment.len() > command.len() {
            return None;
        }
        out.push_str(&command[copied..start]);
        out.push_str(&substitute(segment, &assigned));
        copied = start + segment.len();
        if let Some((name, value)) = literal_assignment(segment) {
            assigned.retain(|(known, _)| !known.eq_ignore_ascii_case(&name));
            assigned.push((name, value));
        }
    }
    out.push_str(&command[copied..]);
    (out != command).then_some(out)
}

fn literal_assignment(segment: &str) -> Option<(String, String)> {
    let words = shell_words(segment);
    let (name, value) = match words.as_slice() {
        [name, equals, value] if equals == "=" => (name.strip_prefix('$')?, value.as_str()),
        [word] => {
            let (name, value) = word.split_once('=')?;
            (name.strip_prefix('$')?, value)
        }
        _ => return None,
    };
    let identifier = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    (identifier && !value.is_empty() && !value.contains(['$', '`', '(']))
        .then(|| (name.to_owned(), value.to_owned()))
}

/// `$NAME` and `${NAME}` replaced by what the line assigned, the longest name first so `$SP` is not
/// read as `$S` followed by `P`. PowerShell names ignore case.
fn substitute(segment: &str, assigned: &[(String, String)]) -> String {
    let mut text = segment.to_owned();
    let mut ordered: Vec<&(String, String)> = assigned.iter().collect();
    ordered.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    for (name, value) in ordered {
        let braced = format!("${{{name}}}");
        text = text.replace(&braced, value);
        let bare = format!("${name}");
        let mut result = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(at) = rest.to_ascii_lowercase().find(&bare.to_ascii_lowercase()) {
            let after = &rest[at + bare.len()..];
            result.push_str(&rest[..at]);
            if after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
                result.push_str(&rest[at..at + bare.len()]);
            } else {
                result.push_str(value);
            }
            rest = after;
        }
        result.push_str(rest);
        text = result;
    }
    text
}

/// How long a conversation's area is reused before the disk is read again. A worktree made a
/// minute ago is in the next one.
const AREA_TTL: Duration = Duration::from_secs(30);

static AREAS: LazyLock<Mutex<std::collections::HashMap<String, (Instant, Area)>>> =
    LazyLock::new(Default::default);

/// The area of a conversation rooted in `root`: the repository's main checkout and its worktrees,
/// minus the daemon's own (see `daemon_worktree`), and the system temp directory. Cached briefly,
/// because a hook runs in front of every tool call.
pub fn conversation_area(root: &Path) -> Area {
    let key = root.to_string_lossy().into_owned();
    if let Ok(areas) = AREAS.lock()
        && let Some((at, area)) = areas.get(&key)
        && at.elapsed() < AREA_TTL
    {
        return area.clone();
    }
    let area = Area::new(&checkouts_of(root), Some(&std::env::temp_dir()));
    if let Ok(mut areas) = AREAS.lock() {
        areas.insert(key, (Instant::now(), area.clone()));
    }
    area
}

/// Every checkout of the repository `root` belongs to, from the disk: the main one (whose `.git` is
/// a directory) and each `.git/worktrees/<name>/gitdir`, which names that worktree's `.git` file.
fn checkouts_of(root: &Path) -> Vec<PathBuf> {
    let Some(main) = main_checkout_of(root) else {
        return Vec::new();
    };
    let main = PathBuf::from(main);
    let mut checkouts = vec![main.clone()];
    let Ok(entries) = std::fs::read_dir(main.join(".git").join("worktrees")) else {
        return checkouts;
    };
    for entry in entries.flatten() {
        let admin = entry.path();
        let Ok(gitdir) = std::fs::read_to_string(admin.join("gitdir")) else {
            continue;
        };
        let Some(worktree) = Path::new(gitdir.trim()).parent() else {
            continue;
        };
        let head = std::fs::read_to_string(admin.join("HEAD")).unwrap_or_default();
        if !daemon_worktree(worktree, &head) {
            checkouts.push(worktree.to_path_buf());
        }
    }
    checkouts
}

/// The daemon's own worktrees stay out of every conversation's area: a run's branch
/// (`nucleos/run-<id>`, and the rest under `nucleos/`) and the queue's merge workspaces
/// (`integration-*`), which an autonomous run or `compute_merge` is using at that moment.
fn daemon_worktree(worktree: &Path, head: &str) -> bool {
    let name = worktree
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    head.trim().starts_with("ref: refs/heads/nucleos/")
        || ["integration-", "run-", "job-"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_area_counts_only_inside_within_and_is_restored_after() {
        let area = Area::new(
            &[PathBuf::from("C:/Projects/repo-wt")],
            Some(Path::new("C:/Temp")),
        );
        let workspace = fold_for_containment("C:/Projects/repo");
        let sibling = fold_for_containment("C:/Projects/repo-wt/core");
        assert!(!inside(&sibling, &workspace));
        assert!(within(&area, || inside(&sibling, &workspace)));
        assert!(!inside(&sibling, &workspace));
        // A sibling whose name merely starts the same is not inside.
        let lookalike = fold_for_containment("C:/Projects/repo-wt2/core");
        assert!(!within(&area, || inside(&lookalike, &workspace)));
    }

    #[test]
    fn the_temp_directory_is_spelled_out_only_inside_within() {
        let area = Area::new(&[], Some(Path::new("C:/Temp")));
        let posix = Some(crate::command_reader::Shell::Posix);
        assert_eq!(expand_temp("$TEMP/gate.log", posix), None);
        within(&area, || {
            let temp = fold_for_containment("C:/Temp");
            assert_eq!(
                expand_temp("$TEMP/gate.log", posix),
                Some(format!("{temp}/gate.log"))
            );
            assert_eq!(
                expand_temp("%TEMP%\\x.log", None),
                Some(format!("{temp}/x.log"))
            );
            assert_eq!(
                expand_temp("$env:TEMP\\x.log", None),
                Some(format!("{temp}/x.log"))
            );
            assert_eq!(
                expand_temp("/tmp/x.log", posix),
                Some(format!("{temp}/x.log"))
            );
            assert_eq!(expand_temp("/tmp/x.log", None), None);
            assert_eq!(expand_temp("$TEMPLATE/x", posix), None);
            assert_eq!(expand_temp("$TEMP/$HOME/x", posix), None);
            assert_eq!(expand_temp("$HOME/x", posix), None);
        });
    }

    #[test]
    fn the_daemons_own_worktrees_are_left_out() {
        assert!(daemon_worktree(
            Path::new("C:/P/x"),
            "ref: refs/heads/nucleos/run-7\n"
        ));
        assert!(daemon_worktree(
            Path::new("C:/P/integration-3"),
            "ref: refs/heads/master"
        ));
        assert!(!daemon_worktree(
            Path::new("C:/P/nucleos-chat-perms"),
            "ref: refs/heads/fix/x"
        ));
    }

    #[test]
    fn the_guard_lets_a_conversation_into_its_checkouts_and_temp_and_no_further() {
        let root = Path::new("C:/Projects/repo");
        let area = Area::new(
            &[PathBuf::from("C:/Projects/repo-wt")],
            Some(Path::new("C:/Temp")),
        );
        let bash = |command: &str| serde_json::json!({ "command": command });
        let guard = |command: &str| judge_guard("Bash", &bash(command), root);
        let opens = [
            "cd /c/Projects/repo-wt && cargo test -p core",
            "cargo test > $TEMP/gate.log 2>&1",
            "python x.py > /tmp/gate.log",
        ];
        for command in opens {
            assert_eq!(guard(command), Some(JudgeGuard::OutsideTheRun), "{command}");
            assert_eq!(within(&area, || guard(command)), None, "{command}");
        }
        let still_out = [
            "cd /c/Projects/other && cargo test",
            "cat $HOME/notes.txt",
            "cp x ~/y",
            "cat $TEMP/../secret.txt",
        ];
        for command in still_out {
            assert_eq!(
                within(&area, || guard(command)),
                Some(JudgeGuard::OutsideTheRun),
                "{command}"
            );
        }
        // G1 and G3 do not move with the area.
        assert_eq!(
            within(&area, || guard("cat /c/Projects/repo-wt/.env")),
            Some(JudgeGuard::SensitiveSource)
        );
    }

    #[test]
    fn the_classifier_writes_into_a_sibling_checkout_only_inside_the_area() {
        let root = Path::new("C:/Projects/repo");
        let area = Area::new(&[PathBuf::from("C:/Projects/repo-wt")], None);
        let write =
            serde_json::json!({ "file_path": "C:/Projects/repo-wt/src/a.rs", "content": "" });
        let classify_write = || {
            classify(
                "Write",
                &write,
                Some(root),
                &crate::integrations::github::Policy::empty(),
                &crate::project::policy::ShellRules::default(),
                Unrecognized::AsksAPerson,
            )
            .decision
            .decision
        };
        assert_eq!(classify_write(), "deny");
        assert_eq!(within(&area, classify_write), "allow");
    }

    #[test]
    fn a_line_reads_the_variables_it_assigned_itself_inside_an_area() {
        // POSIX lines are spelled by `command_reader::parse`, so this goes through the two
        // public doors a line can enter by, `judge_guard` and `classify`, and never calls the
        // lexical spelling directly.
        let area = Area::new(&[], Some(Path::new("C:/Temp")));
        let guard_of = |command: &str| {
            judge_guard(
                "Bash",
                &serde_json::json!({ "command": command }),
                Path::new("C:/Projects/repo"),
            )
        };
        let verdict_of = |command: &str| {
            let c = classify(
                "Bash",
                &serde_json::json!({ "command": command }),
                Some(Path::new("C:/Projects/repo")),
                &crate::integrations::github::Policy::empty(),
                &crate::project::policy::ShellRules::default(),
                Unrecognized::AsksAPerson,
            );
            (c.decision.decision, c.action_class)
        };
        let assigned = r#"S="C:/Temp/pad"; cargo test > "$S/gate.log" 2>&1"#;
        let spelled = r#"cargo test > "C:/Temp/pad/gate.log" 2>&1"#;
        let braced = r#"S="C:/Temp/pad"; cargo test > "$S/gate.log" 2>&1; echo ${S}/x"#;
        // `$SP` is never assigned, so it stays an unknown word.
        let unassigned = r#"S="C:/Temp/pad"; cargo test > "$S/gate.log" 2>&1; echo ${S}/x $SP"#;
        // Outside an area nothing is spelled out: the variable is unknown.
        assert_eq!(guard_of(assigned), Some(JudgeGuard::OutsideTheRun));
        within(&area, || {
            assert_eq!(guard_of(assigned), None);
            assert_eq!(guard_of(braced), None);
            assert_eq!(guard_of(unassigned), Some(JudgeGuard::OutsideTheRun));
            // Spelled out by the parser, the classifier sees what it would see for the
            // literal path.
            assert_eq!(verdict_of(assigned), verdict_of(spelled));
            assert_ne!(verdict_of(assigned).0, "deny");
            // Not literal, so not recorded: the variable stays unknown.
            assert_eq!(
                guard_of(r#"S=$(pwd); cat "$S/x" > "$S/y""#),
                Some(JudgeGuard::OutsideTheRun)
            );
            // Only after the assignment.
            assert_eq!(
                guard_of(r#"cat "$S/x"; S=C:/Temp"#),
                Some(JudgeGuard::OutsideTheRun)
            );
            // PowerShell keeps its lexical spelling, under its new one-argument name.
            let ps = spelled_out_powershell(r#"$L = "C:/Temp/a"; Get-Content $l"#).unwrap();
            assert!(ps.contains(r"C:/Temp/a"), "{ps}");
        });
    }

    /// A literal assignment is known only to the commands in its own scope, the way the shell
    /// has it: a subshell's or a branch's assignment is gone when the construct ends.
    #[test]
    fn an_assignment_does_not_leave_its_subshell_or_branch() {
        let area = Area::new(&[], Some(Path::new("C:/Temp")));
        let guard_of = |command: &str| {
            judge_guard(
                "Bash",
                &serde_json::json!({ "command": command }),
                Path::new("C:/Projects/repo"),
            )
        };
        within(&area, || {
            for command in [
                r#"(S=C:/Temp/pad); cargo test > "$S/gate.log""#,
                r#"if true; then S=C:/Temp/pad; fi; cargo test > "$S/gate.log""#,
            ] {
                assert_eq!(
                    guard_of(command),
                    Some(JudgeGuard::OutsideTheRun),
                    "{command}"
                );
            }
        });
    }

    /// The area is what turns an assigned literal into the path it names; without one the
    /// variable stays unknown, exactly as before.
    #[test]
    fn outside_an_area_an_assigned_variable_stays_unknown() {
        assert_eq!(
            judge_guard(
                "Bash",
                &serde_json::json!({ "command": r#"S=C:/Temp/pad; cargo test > "$S/gate.log""# }),
                Path::new("C:/Projects/repo")
            ),
            Some(JudgeGuard::OutsideTheRun)
        );
    }

    #[test]
    fn a_conversations_area_holds_the_main_checkout_and_its_worktrees() {
        let dir = std::env::temp_dir().join(format!("nucleos-area-{}", std::process::id()));
        let main = dir.join("repo");
        let mine = dir.join("repo-feature");
        let run = dir.join("repo-run");
        std::fs::create_dir_all(main.join(".git/worktrees/feature")).unwrap();
        std::fs::create_dir_all(main.join(".git/worktrees/run")).unwrap();
        std::fs::create_dir_all(&mine).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        let admin = |name: &str, worktree: &Path, head: &str| {
            let admin = main.join(".git/worktrees").join(name);
            std::fs::write(
                admin.join("gitdir"),
                format!("{}\n", worktree.join(".git").display()),
            )
            .unwrap();
            std::fs::write(admin.join("HEAD"), head).unwrap();
            std::fs::write(
                worktree.join(".git"),
                format!("gitdir: {}\n", admin.display()),
            )
            .unwrap();
        };
        admin("feature", &mine, "ref: refs/heads/fix/x\n");
        admin("run", &run, "ref: refs/heads/nucleos/run-9\n");

        let mut found = checkouts_of(&mine)
            .iter()
            .map(|path| fold_for_containment(&normalize_path(&path.to_string_lossy(), None)))
            .collect::<Vec<_>>();
        found.sort();
        let mut expected = [&main, &mine]
            .iter()
            .map(|path| fold_for_containment(&normalize_path(&path.to_string_lossy(), None)))
            .collect::<Vec<_>>();
        expected.sort();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(found, expected);
    }
}
