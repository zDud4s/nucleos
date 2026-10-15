//! Write and deletion guard predicates.

use super::*;

/// PURE: the file a write tool is aiming at, whichever key that tool uses to name it.
///
/// `Edit` and `Write` say `file_path`; `NotebookEdit` says `notebook_path`. Four guards used to
/// read the first key inline, one copy each, and a copy that does not know a key returns `false`
/// -- which every one of those call sites reads as a verdict rather than as an absence. One reader
/// so the four cannot come to disagree, and so that admitting the next write tool is a line here
/// instead of four edits somebody does three of.
///
/// First match wins and the order is not meaningful: no tool sends both keys, and one that did
/// would be a tool this file has not reasoned about.
pub(super) fn written_path(tool_input: &Value) -> Option<&str> {
    ["file_path", "notebook_path"]
        .iter()
        .find_map(|key| tool_input.get(key).and_then(Value::as_str))
}

pub(super) fn targets_self_governing_file(tool_input: &Value, cwd: Option<&Path>) -> bool {
    let Some(file_path) = written_path(tool_input) else {
        return false;
    };
    let normalized = fold_for_match(&normalize_path(file_path, cwd));

    SELF_GOVERNING_FILES
        .iter()
        .any(|suffix| path_has_suffix(&normalized, suffix))
        || SELF_GOVERNING_DIRS
            .iter()
            .any(|dir| normalized.starts_with(dir) || normalized.contains(&format!("/{dir}")))
        || names_machine_settings(file_path)
}

/// Spellings the shell or the editor turns into the home directory before the path is used.
/// Lowercase, because they are compared after `fold_for_match`.
pub(super) const HOME_SPELLINGS: &[&str] =
    &["~/", "$home/", "${home}/", "%userprofile%/", "%homepath%/"];

/// PURE: whether `raw` names this machine's own settings — `~/.nucleos/`, where the daemon reads
/// them, or the old `.ai/<file>` it copies them from once.
///
/// **Why this is its own check rather than nine more lines in `SELF_GOVERNING_FILES`.** Those files
/// moved out of the project and into the home directory, and a suffix list is matched against a
/// path glued onto the run's workspace — so `~/.nucleos/github.yaml` arrived here as
/// `<workspace>/~/.nucleos/github.yaml`, lay "inside" the workspace, and was an ordinary allowed
/// write. The same was true of `$HOME/...` and `%USERPROFILE%\...`. Two answers, both needed:
///
/// - **Anything under a home-spelled `.nucleos/`** is refused whole: every file there is the
///   owner's (the settings, the council's roster, the workflow library), none is a run's to write.
///   Only a HOME spelling is matched, because `.nucleos/` as a bare path segment is also where this
///   daemon puts job worktrees (`<repo>/.nucleos/worktrees/job-26/...`) and every write inside one
///   would otherwise ask.
/// - **Each settings file by name, however it is rooted** — an absolute `C:/Users/me/.nucleos/...`
///   included, which on a run with a workspace is already refused as `outside-workspace` and on
///   one without is otherwise only `no-workspace`. Derived from [`crate::machine_config::SETTINGS`]
///   so a tenth file cannot be added there and forgotten here.
///
/// **The old `.ai/<file>` is still guarded, and that is not nostalgia.** At every start the daemon
/// copies `<working directory>/.ai/<file>` into `~/.nucleos/` for any file the latter lacks, so a
/// run that could write `.ai/github.yaml` in the daemon's own checkout would, on a machine that
/// never had one, be writing the owner's GitHub policy one restart later.
///
/// **Each project's own state, by name, however it is rooted.** `autopilot.yaml` (the gate
/// command, and so what *green* means), `workflows.yaml`, `onboarded.yaml` (the marker
/// activation requires) and `materialized.yaml` with its per-worktree siblings under
/// `materialized/` (what the app last wrote of a workflow into a checkout, and so which of those
/// files it may overwrite) live in `~/.nucleos/projects/<id>/`
/// (`project/state.rs`). The home spelling is covered by the first answer above; the absolute one
/// is matched here as `.nucleos/projects/<any id>/<file>`, which a job worktree
/// (`.nucleos/worktrees/...`) never is. The old `.ai/workflows.yaml` is guarded for the reason the
/// old settings files are: startup copies it into a project's state directory when that has none.
/// (`.ai/autopilot.yaml` was always in [`SELF_GOVERNING_FILES`].)
pub(super) fn names_machine_settings(raw: &str) -> bool {
    let folded = fold_for_match(&raw.replace('\\', "/"));
    let under_home_settings = HOME_SPELLINGS.iter().any(|home| {
        folded
            .strip_prefix(home)
            .is_some_and(|rest| path_has_prefix(&normalize_path(rest, None), ".nucleos"))
    });
    let normalized = normalize_path(&folded, None);
    under_home_settings
        || crate::machine_config::SETTINGS.iter().any(|setting| {
            path_has_suffix(&normalized, &format!(".nucleos/{}", setting.path))
                || path_has_suffix(&normalized, &format!(".ai/{}", setting.path))
        })
        || names_project_state(&normalized)
}

/// PURE: whether an already-normalised, case-folded path is one project's state file — under
/// `.nucleos/projects/<id>/` — or the old `.ai/workflows.yaml` it is migrated from. See
/// [`names_machine_settings`].
pub(super) fn names_project_state(normalized: &str) -> bool {
    let parts: Vec<&str> = normalized.split('/').collect();
    let in_state_dir = crate::project::state::ALL_FILES.iter().any(|file| {
        matches!(
            parts.as_slice(),
            [.., ".nucleos", "projects", id, name] if !id.is_empty() && name == file
        )
    });
    let worktree_record = matches!(
        parts.as_slice(),
        [.., ".nucleos", "projects", id, dir, name]
            if !id.is_empty()
                && *dir == crate::project::state::WORKTREE_RECORDS_DIR
                && !name.is_empty()
    );
    in_state_dir
        || worktree_record
        || path_has_suffix(
            normalized,
            &format!(".ai/{}", crate::project::state::PINS_FILE),
        )
}

/// PURE: whether any word of one shell command names this machine's settings. See
/// [`names_machine_settings`].
///
/// `--flag=value` is split for the reason `confined_to_workspace` splits it: the path is on the
/// right of the `=`.
pub(super) fn segment_names_machine_settings(segment: &str) -> bool {
    shell_words(segment).iter().any(|word| {
        let candidate = word
            .split_once('=')
            .map_or(word.as_str(), |(_, value)| value);
        names_machine_settings(candidate)
    })
}

pub(super) fn targets_file_that_runs_on_next_command(
    tool_input: &Value,
    cwd: Option<&Path>,
) -> bool {
    let Some(file_path) = written_path(tool_input) else {
        return false;
    };
    let normalized = fold_for_match(&normalize_path(file_path, cwd));

    EXECUTES_ON_NEXT_COMMAND_FILES
        .iter()
        .any(|suffix| path_has_suffix(&normalized, suffix))
        || EXECUTES_ON_NEXT_COMMAND_DIRS
            .iter()
            .any(|dir| normalized.starts_with(dir) || normalized.contains(&format!("/{dir}")))
}

pub(super) fn writes_outside_cwd(tool_input: &Value, cwd: Option<&Path>) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    let Some(file_path) = written_path(tool_input) else {
        return false;
    };

    let target = fold_for_containment(&normalize_path(file_path, Some(cwd)));
    let cwd = fold_for_containment(&normalize_path(&cwd.to_string_lossy(), None));
    !super::area::inside(&target, &cwd)
}

/// PURE: splits the first whitespace-delimited token (quotes respected) off `raw`, returning the
/// token's unquoted text and the raw remainder with its leading whitespace trimmed.
pub(super) fn split_first_token(raw: &str) -> Option<(String, &str)> {
    let raw = raw.trim_start();
    if raw.is_empty() {
        return None;
    }
    let mut quote: Option<char> = None;
    let mut end = raw.len();
    for (index, ch) in raw.char_indices() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                } else if ch.is_whitespace() {
                    end = index;
                    break;
                }
            }
        }
    }
    let token = shell_words(&raw[..end]).into_iter().next()?;
    Some((token, raw[end..].trim_start()))
}

/// PURE: the command a `python <path>/scripts/heavy.py [options] [--] <argv>` line runs, when the
/// script lies OUTSIDE the cwd workspace (the main checkout's broker, which a run worktree cannot
/// edit). A broker inside the workspace is a file the run could have rewritten, so it is no
/// wrapper. Only the known options are unwrapped; anything else yields `None`.
pub(super) fn heavy_wrapper_inner(segment: &str, cwd: Option<&Path>) -> Option<String> {
    let (program, rest) = split_first_token(segment)?;
    let program = program_name(&program).to_ascii_lowercase();
    if !(program.starts_with("python") || program == "py") {
        return None;
    }
    let (script, mut rest) = split_first_token(rest)?;
    let normalized = normalize_path(&script, cwd);
    if !path_has_suffix(&normalized, "scripts/heavy.py") {
        return None;
    }
    // Only the project's own main checkout holds the trusted broker; a heavy.py anywhere else is
    // arbitrary code. Unresolved main checkout (or no cwd) fails safe: no unwrap.
    let cwd = cwd?;
    let target = fold_for_containment(&normalized);
    let root = fold_for_containment(&normalize_path(&cwd.to_string_lossy(), None));
    if target == root || target.starts_with(&format!("{root}/")) {
        return None;
    }
    let main = main_checkout_of(cwd)?;
    let broker = fold_for_containment(&normalize_path(&format!("{main}/scripts/heavy.py"), None));
    if target != broker {
        return None;
    }
    loop {
        let (token, after) = split_first_token(rest)?;
        if token == "--" {
            return (!after.is_empty()).then(|| after.to_owned());
        }
        if matches!(
            token.as_str(),
            "--prio" | "--agent" | "--kind" | "--wait-max"
        ) {
            let (_, after_value) = split_first_token(after)?;
            rest = after_value;
            continue;
        }
        if ["--prio=", "--agent=", "--kind=", "--wait-max="]
            .iter()
            .any(|p| token.starts_with(p))
        {
            rest = after;
            continue;
        }
        if token.starts_with('-') {
            return None;
        }
        // PowerShell may swallow `--`: the first token after the options starts the argv. A bare
        // word such as `hold-worktree` is a broker subcommand, not a command to unwrap.
        if token == "hold-worktree" {
            return None;
        }
        return Some(rest.to_owned());
    }
}

/// Best-effort: the main checkout of the repository `cwd` lies in. Walks up to the first ancestor
/// with a `.git` entry; a directory means that ancestor is the main checkout, a file reading
/// `gitdir: <main>/.git/worktrees/<name>` means `<main>`. Any failure is `None`.
pub(super) fn main_checkout_of(cwd: &Path) -> Option<String> {
    let dir = cwd.ancestors().find(|a| a.join(".git").exists())?;
    let dot_git = dir.join(".git");
    if dot_git.is_dir() {
        return Some(normalize_path(&dir.to_string_lossy(), None));
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim()
        .replace('\\', "/");
    let index = gitdir.rfind("/.git/")?;
    let main = &gitdir[..index];
    (!main.is_empty()).then(|| normalize_path(main, None))
}

pub(super) fn path_has_suffix(path: &str, suffix: &str) -> bool {
    path == suffix || path.ends_with(&format!("/{suffix}"))
}

/// PURE: whether `path` IS `prefix` or lies underneath it.
///
/// The mirror of `path_has_suffix`, and the separator inside the `starts_with` is the whole of it.
/// A bare `starts_with` would let the prefix `core/mig` reach `core/migrations` — a directory
/// nobody named, refused by a rule about a directory that need not even exist.
pub(super) fn path_has_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

/// Whether this project wrote down a refusal that covers this write.
///
/// **`path_has_prefix` and deliberately not `matches_command_prefix`.** That one separates on a
/// SPACE, because a command prefix is a run of whole words; a path has no spaces to separate on, so
/// `migrations` would match the directory entry itself and nothing inside it. That is a restriction
/// which silently is not one — the owner reads a refusal and the daemon enforces nothing — and it
/// is exactly the shape `http.rs`'s `checked_denials` doc refuses to build.
///
/// **Both sides go through `normalize_path` against the same `cwd`, and that is what makes a
/// RELATIVE prefix mean something.** A stored `migrations` is joined onto the workspace, so it
/// names THIS project's `migrations/` and not any path anywhere that happens to end in it — which
/// is what a suffix match would have given, and it would have refused a `vendor/x/migrations/y.sql`
/// nobody was thinking about. An absolute prefix is left where it is, by `normalize_path` itself.
///
/// `fold_for_containment` on both sides and not `fold_for_match`: this is a containment question
/// about a real filesystem, so the fold has to be the filesystem's own — the same one
/// `writes_outside_cwd` asks a few lines up about the same `file_path`. It is also why
/// `project_policy::fold_path_prefix` does not fold case at storage time: the case question is
/// answered here, once, by the side that knows the answer.
pub(super) fn write_denied_by_project(
    tool_name: &str,
    tool_input: &Value,
    cwd: Option<&Path>,
    rules: &crate::project::policy::ShellRules,
) -> bool {
    // A project that declared no write refusals cannot enter this block at all — the same
    // structural non-regression the shell side's empty-list guard buys, and for the same reason: no
    // walk, no allocation, and no verdict this can possibly change.
    if rules.deny_writes.is_empty() {
        return false;
    }
    let Some(file_path) = written_path(tool_input) else {
        return false;
    };
    let target = fold_for_containment(&normalize_path(file_path, cwd));

    rules.deny_writes.iter().any(|(tool, prefix)| {
        tool == tool_name
            && path_has_prefix(&target, &fold_for_containment(&normalize_path(prefix, cwd)))
    })
}

/// Whether the command deletes something outside the run's workspace.
///
/// Read from every position and with the program's directory stripped, for the same reason
/// `has_destructive_flags` is: anchoring on the first token whole meant `rm ../../secrets` was
/// denied and `/bin/rm ../../secrets` — the same delete, escaping the same workspace — was not
/// recognised as a delete at all.
pub(super) fn deletes_outside_cwd(command: &str, cwd: Option<&Path>) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    // Tokens kept as written. Only the program name is case-folded, to match the list below; the
    // delete TARGETS go to `fold_for_containment`, which folds a path only where the filesystem
    // does — folding them here would widen the workspace behind its back.
    let tokens = shell_words(command);
    let workspace = fold_for_containment(&normalize_path(&cwd.to_string_lossy(), None));

    tokens.iter().enumerate().any(|(index, token)| {
        let lowered = token.to_ascii_lowercase();
        let program = program_name(&lowered);
        if !matches!(
            program,
            "rm" | "rd" | "rmdir" | "del" | "erase" | "remove-item" | "ri"
        ) {
            return false;
        }
        delete_targets(program, &tokens[index + 1..])
            .into_iter()
            .any(|target| {
                // Expanded by the shell before the delete runs, to the home directory or another
                // place this cannot see. `normalize_path` would read `~/x` and `$HOME/x` as
                // relative and land them inside the workspace, so they are outside by fiat.
                if expands_to_somewhere_else(target) {
                    return true;
                }
                let target = fold_for_containment(&normalize_path(target, Some(cwd)));
                target != workspace && !target.starts_with(&format!("{workspace}/"))
            })
    })
}

/// PURE: whether a delete target names its place through the shell rather than literally — the
/// home directory (`~`, `$HOME`, `${HOME}`, `$env:USERPROFILE`, `%USERPROFILE%`) or any other
/// environment variable. Leading `(` and `{` are stepped over, as `program_name` does.
pub(super) fn expands_to_somewhere_else(target: &str) -> bool {
    let target = target.trim_start_matches(['(', '{']).to_ascii_lowercase();
    target.starts_with('~')
        || target.starts_with("$home")
        || target.starts_with("${home}")
        || target.starts_with("$env:")
        || target.starts_with("${env:")
        || (target.starts_with('%') && target[1..].contains('%'))
}

/// The arguments of a delete that name what it deletes.
///
/// A `/`-prefixed argument is a switch only where cmd gives the program that switch: `rd /s /q`,
/// `del /f /s /q /p /a[:attrs]`. Treating every `/x` as a switch let `del /c/Users/me/x` and
/// `Remove-Item /etc/x` — which has no slash switches at all — delete outside the workspace with
/// their target never looked at.
pub(super) fn delete_targets<'a>(program: &str, arguments: &'a [String]) -> Vec<&'a str> {
    arguments
        .iter()
        .filter_map(|argument| {
            let lowered = argument.to_ascii_lowercase();
            let is_cmd_switch = match program {
                "rd" | "rmdir" => matches!(lowered.as_str(), "/s" | "/q"),
                "del" | "erase" => {
                    matches!(lowered.as_str(), "/p" | "/f" | "/s" | "/q" | "/a")
                        || lowered.starts_with("/a:")
                }
                _ => false,
            };
            let is_option = argument.starts_with('-') || is_cmd_switch;
            (!is_option).then_some(argument.as_str())
        })
        .collect()
}
