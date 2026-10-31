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
    names_self_governing(file_path, cwd)
}

/// PURE: whether `path` is one of the files whose contents are the policy, or lies under one of
/// the directories that are. The question `targets_self_governing_file` asks of a write tool's
/// `file_path`, and `segment_names_self_governing` of each word of a shell command.
pub(super) fn names_self_governing(path: &str, cwd: Option<&Path>) -> bool {
    let normalized = fold_for_match(&normalize_path(path, cwd));

    SELF_GOVERNING_FILES
        .iter()
        .any(|suffix| path_has_suffix(&normalized, suffix))
        || SELF_GOVERNING_DIRS
            .iter()
            .any(|dir| normalized.starts_with(dir) || normalized.contains(&format!("/{dir}")))
        || names_machine_settings(path)
}

/// PURE: whether any word of one shell command names a self-governing file or directory. See
/// [`names_self_governing`].
///
/// Asked before confinement widens a command, because confinement answers only "does this point
/// inside the workspace", and `.claude/hooks/ask_daemon.py` is inside it: `cp ./x
/// .claude/hooks/ask_daemon.py` was allowed for pointing nowhere else.
///
/// Every word is tried, not just the ones `confined_to_workspace` reads as paths, so a bare
/// `nucleos.tests.yaml` counts. Each is read twice — with only its quotes dropped (a Windows `\`
/// is a separator) and the way `flag_words` reads it (a POSIX `\` escapes) — and a word naming a
/// DIRECTORY itself (`./.claude/hooks`) counts, because a copy into it lands underneath. A value
/// is read on the right of an `=` and after a flag it is attached to, for the reasons
/// `confined_to_workspace` gives. Every extra reading can only refuse more.
pub(super) fn segment_names_self_governing(segment: &str, cwd: Option<&Path>) -> bool {
    shell_words(segment)
        .into_iter()
        .chain(flag_words(segment))
        .any(|word| {
            let value = word
                .split_once('=')
                .map_or(word.as_str(), |(_, value)| value);
            let value = if value.starts_with('-') {
                match value.find(['/', '\\', '.']) {
                    Some(start) => &value[start..],
                    None => return false,
                }
            } else {
                value
            };
            !value.is_empty()
                && (names_self_governing(value, cwd) || is_a_self_governing_dir(value, cwd))
        })
}

/// PURE: whether `path` IS a directory a self-governing file or directory sits in — `.claude`,
/// `.claude/hooks`, `.nucleos/workflows`, `.agents/skills` — rather than something under one. A
/// copy or a move into it lands the file underneath under its own name (`cp ./settings.json
/// ./.claude`), and a move of it takes everything inside along.
fn is_a_self_governing_dir(path: &str, cwd: Option<&Path>) -> bool {
    let normalized = fold_for_match(&normalize_path(path, cwd));
    let parents = SELF_GOVERNING_FILES
        .iter()
        .chain(SELF_GOVERNING_DIRS)
        .flat_map(|entry| {
            let entry = entry.trim_end_matches('/');
            entry.match_indices('/').map(move |(end, _)| &entry[..end])
        });
    let dirs = SELF_GOVERNING_DIRS
        .iter()
        .map(|dir| dir.trim_end_matches('/'));
    parents
        .chain(dirs)
        .any(|dir| path_has_suffix(&normalized, dir))
}

/// Spellings the shell or the editor turns into the home directory before the path is used.
/// Lowercase, because they are compared after `fold_for_match`.
pub(super) const HOME_SPELLINGS: &[&str] =
    &["~/", "$home/", "${home}/", "%userprofile%/", "%homepath%/"];

/// PURE: whether `raw` names this machine's own settings — `~/.nucleos/`, where the daemon reads
/// them.
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
///   one without is otherwise only `no-workspace`. Derived from [`crate::settings::documents`]
///   so another document cannot be added there and forgotten here.
///
/// The old `.ai/<file>` copies are not guarded any more: the startup migration that copied them
/// into `~/.nucleos/` is gone, so the daemon never reads one again and a write there decides
/// nothing.
///
/// **Each project's own state, by name, however it is rooted.** `autopilot.yaml` (the gate
/// command, and so what *green* means), `workflows.yaml`, `onboarded.yaml` (the marker
/// activation requires) and `materialized.yaml` with its per-worktree siblings under
/// `materialized/` (what the app last wrote of a workflow into a checkout, and so which of those
/// files it may overwrite) live in `~/.nucleos/projects/<id>/`
/// (`project/state.rs`). The home spelling is covered by the first answer above; the absolute one
/// is matched here as `.nucleos/projects/<any id>/<file>`, which a job worktree
/// (`.nucleos/worktrees/...`) never is.
pub(super) fn names_machine_settings(raw: &str) -> bool {
    let folded = fold_for_match(&raw.replace('\\', "/"));
    let under_home_settings = HOME_SPELLINGS.iter().any(|home| {
        folded
            .strip_prefix(home)
            .is_some_and(|rest| path_has_prefix(&normalize_path(rest, None), ".nucleos"))
    });
    let normalized = normalize_path(&folded, None);
    // `<file>.yaml.imported` is the file it came from: ask the same question of the original name.
    let imported_origin = normalized
        .strip_suffix(IMPORTED_SUFFIX)
        .filter(|origin| origin.ends_with(".yaml"));
    under_home_settings
        || names_settings_file(&normalized)
        || imported_origin.is_some_and(names_settings_file)
}

/// What the importer renames a consumed settings file to, next to where it found it
/// (`<name>.yaml` -> `<name>.yaml.imported`). Guarded like the original: the import only runs when a
/// document is absent, but `--settings-reset` re-opens that door, so a hand-edited one is a way in.
/// Only the `.nucleos/` copies are kept; the old `.ai/` ones are not read by anything.
pub(super) const IMPORTED_SUFFIX: &str = ".imported";

/// PURE: whether an already-normalised, case-folded path is one of the machine documents by file
/// name under `.nucleos/`, or one project's state file. Derived from the settings registry, so a
/// document added there cannot be forgotten here.
fn names_settings_file(normalized: &str) -> bool {
    crate::settings::documents().iter().any(|document| {
        document.kind == crate::settings::Kind::Machine
            && path_has_suffix(normalized, &format!(".nucleos/{}", document.origin_file))
    }) || names_project_state(normalized)
}

/// PURE: whether an already-normalised, case-folded path is one project's state file — under
/// `.nucleos/projects/<id>/`. See [`names_machine_settings`].
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
    in_state_dir || worktree_record
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

// ---- the NucleOS database and the files that replace it ---------------------------------------

/// Where the daemon keeps its SQLite database - `<data dir>/nucleos.db` - written as lowercase
/// path-segment runs that identify the data directory by how the OS and the shell spell it. Matched
/// ANYWHERE in a path, and only when a sibling name follows (see [`is_nucleos_data_sibling`]).
///
/// Each run is what `directories` builds for `ProjectDirs::from("dev", "nucleos", "NucleOS")`:
///
/// - Windows: `<LocalAppData>\<organization>\<application>\data`. The run is only the tail, so
///   `%LOCALAPPDATA%`, `$env:LOCALAPPDATA`, `~/AppData/Local` and a literal `C:\Users\<anyone>\...`
///   all reach it, and so does the tail a `Join-Path` leaves in one word.
/// - Linux: `$XDG_DATA_HOME` when absolute, else `~/.local/share`, then the application name.
///   A custom absolute `XDG_DATA_HOME` is caught by the resolved directory.
/// - macOS: `~/Library/Application Support/<qualifier>.<organization>.<application>`; the second
///   form is the shell-escaped `Application\ Support`, whose `\` has become a `/`. The last,
///   one-segment run is for that escaped form AFTER `shell_words` has split it: the second word
///   starts `Support/dev.nucleos.NucleOS/nucleos.db`, so no run beginning at `library` can match it.
///   `dev.nucleos.nucleos` is called nothing else, and a sibling must still follow.
const NUCLEOS_DATA_ROOTS: &[&[&str]] = &[
    &["nucleos", "nucleos", "data"],
    &[".local", "share", "nucleos"],
    &["$xdg_data_home", "nucleos"],
    &["${xdg_data_home}", "nucleos"],
    &["library", "application support", "dev.nucleos.nucleos"],
    &["library", "application", "support", "dev.nucleos.nucleos"],
    &["dev.nucleos.nucleos"],
];

pub(super) const NUCLEOS_DATA_CLASS: &str = "nucleos-data";
pub(super) const NUCLEOS_DATA_REASON: &str =
    "the NucleOS database and the files that replace it are the daemon's own and are denied";
pub(super) const NUCLEOS_SETTINGS_CLI_REASON: &str = "the daemon's settings command line edits the configuration database directly and is denied to an agent";

/// PURE: the names inside the data directory that are the database or replace it at start-up:
/// `backups` (a restore reads from it), everything beginning `nucleos.` (the database, its
/// `-wal`/`-shm`/`-journal`, and the restore markers, which `backup.rs` builds with
/// `with_extension` and so spells `nucleos.pending-restore` and so on), and the bare spellings the
/// design spec uses. `logs/` and the directory itself are deliberately not here.
fn is_nucleos_data_sibling(name: &str) -> bool {
    name == "backups"
        || name.starts_with("nucleos.")
        || matches!(
            name,
            ".pending-restore" | ".pending-restore-safety" | ".restore-new"
        )
}

/// PURE: a path as comparable segments - lowercased, `\` as `/`, `.`/`..` collapsed, a leading
/// `file:` scheme (a SQLite URI) and a `\\?\` prefix dropped.
fn nucleos_data_segments(raw: &str) -> Vec<String> {
    let folded = fold_for_match(&raw.replace('\\', "/"));
    let without_scheme = folded.strip_prefix("file:").unwrap_or(&folded);
    let mut segments: Vec<String> = normalize_path(without_scheme, None)
        .split('/')
        .map(|segment| segment.trim().to_owned())
        .collect();
    if segments.first().is_some_and(|segment| segment == "?") {
        segments.remove(0);
    }
    segments
}

/// PURE: whether `raw` names the NucleOS database, one of its `-wal`/`-shm` files, `backups/`, or a
/// restore marker, in the data directory this machine uses (`data_dirs`) or in any spelling of it
/// ([`NUCLEOS_DATA_ROOTS`]).
///
/// **Anchored, never basename-only.** `nucleos.db` alone, `core/testdata/nucleos.db` and a test's
/// temp directory match nothing; a root has to precede the name. `data_dirs` are absolute and
/// matched from the START of the path, so a relative override can never turn an ordinary relative
/// path into a hit. They are a parameter so a test does not depend on the machine it runs on.
///
/// Known residuals, seen and not fixed (the kernel-level containment is the sandbox spec's job): a
/// path built through a variable, a glob, `cd <data dir>` followed by a relative `nucleos.db`, a
/// Windows 8.3 short name, a link into the directory, and a `Join-Path` split across several words.
pub(super) fn names_nucleos_data_in(raw: &str, data_dirs: &[std::path::PathBuf]) -> bool {
    let segments = nucleos_data_segments(raw);
    let spelled = NUCLEOS_DATA_ROOTS.iter().any(|root| {
        segments.windows(root.len() + 1).any(|window| {
            (0..root.len()).all(|at| window[at] == root[at])
                && is_nucleos_data_sibling(&window[root.len()])
        })
    });
    spelled
        || data_dirs.iter().any(|dir| {
            let dir = dir.to_string_lossy();
            if !is_absolute_path(&dir.replace('\\', "/")) {
                return false;
            }
            let wanted = nucleos_data_segments(&dir);
            segments.len() > wanted.len()
                && segments[..wanted.len()] == wanted[..]
                && is_nucleos_data_sibling(&segments[wanted.len()])
        })
}

/// The directories this daemon's database can live in: what THIS process resolves
/// ([`crate::data_dir::data_dir`]) and the platform default ([`crate::data_dir::default_data_dir`]),
/// deduplicated. A resolver that fails contributes nothing; the static spellings still apply. The
/// environment is read each call and is the only reason this is not pure.
fn nucleos_data_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for dir in [
        crate::data_dir::data_dir(),
        crate::data_dir::default_data_dir(),
    ]
    .into_iter()
    .flatten()
    {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// PURE: the paths hiding in one shell word, split on the glue a shell puts between a path and the
/// rest (`>file`, `2>file`, `--db=file`, `(file)`, `a,b`, `$(...)/file`).
fn nucleos_data_pieces(word: &str) -> impl Iterator<Item = &str> {
    word.split(|character: char| {
        matches!(
            character,
            '<' | '>' | '&' | '|' | ';' | '(' | ')' | '`' | '=' | ','
        )
    })
    .filter(|piece| !piece.is_empty())
}

/// PURE: whether any word of a shell line names the NucleOS database or a sibling that replaces it.
/// The WHOLE line is scanned, not a segment: a refusal must not depend on where in a line it appears.
pub(super) fn line_names_nucleos_data_in(command: &str, data_dirs: &[std::path::PathBuf]) -> bool {
    shell_words(command)
        .iter()
        .any(|word| nucleos_data_pieces(word).any(|piece| names_nucleos_data_in(piece, data_dirs)))
}

pub(super) fn line_names_nucleos_data(command: &str) -> bool {
    line_names_nucleos_data_in(command, &nucleos_data_dirs())
}

/// Whether a write tool's target is the NucleOS database or a sibling. Both the raw spelling and
/// the one resolved against the run's workspace are asked: the first is what a home- or
/// variable-rooted path looks like, the second what a `..` out of the workspace looks like.
pub(super) fn writes_nucleos_data(tool_input: &Value, cwd: Option<&Path>) -> bool {
    let Some(file_path) = written_path(tool_input) else {
        return false;
    };
    let dirs = nucleos_data_dirs();
    names_nucleos_data_in(file_path, &dirs)
        || names_nucleos_data_in(&normalize_path(file_path, cwd), &dirs)
}

/// PURE: whether a path-or-glob names the database, for a tool that LOOKS INSIDE a directory.
///
/// `Grep` and `Glob` take a directory or a pattern, so the data directory itself, or
/// `<data dir>/**`, searches the database without spelling a sibling name. This asks one more
/// question: the literal directory part of the argument (before the first glob metacharacter, cut
/// back to a separator) with `nucleos.db` appended - would that file be refused? `logs/` is still
/// allowed, because `logs/nucleos.db` has no root followed by a sibling.
///
/// Known residual: the PARENT of the data directory, and a basename-only pattern, search the
/// database without naming it.
fn names_nucleos_data_under(argument: &str, data_dirs: &[std::path::PathBuf]) -> bool {
    let directory = match argument.find(['*', '?', '[', '{']) {
        None => argument,
        Some(at) => argument[..at]
            .rfind(['/', '\\'])
            .map_or("", |sep| &argument[..sep]),
    };
    names_nucleos_data_in(argument, data_dirs)
        || names_nucleos_data_in(
            &format!("{}/nucleos.db", directory.trim_end_matches(['/', '\\'])),
            data_dirs,
        )
}

/// Whether a READ tool's target is the database or a sibling: `Read.file_path`, `Grep.path` and
/// `Grep.glob`, `Glob.path` and `Glob.pattern`. `Grep.pattern` is a regex over file contents and is
/// never looked at. `Read` takes a file and is asked with the plain matcher; `Grep` and `Glob` look
/// inside a directory and are asked with [`names_nucleos_data_under`]. The MCP `project_cat` /
/// `project_grep` never reach `classify`, so nothing is added for them.
pub(super) fn reads_nucleos_data(tool_name: &str, tool_input: &Value, cwd: Option<&Path>) -> bool {
    let keys: &[&str] = match tool_name {
        "Read" => &["file_path"],
        "Grep" => &["path", "glob"],
        "Glob" => &["path", "pattern"],
        _ => return false,
    };
    let dirs = nucleos_data_dirs();
    let asks = |argument: &str| {
        if tool_name == "Read" {
            names_nucleos_data_in(argument, &dirs)
        } else {
            names_nucleos_data_under(argument, &dirs)
        }
    };
    keys.iter()
        .filter_map(|key| tool_input.get(*key).and_then(Value::as_str))
        .any(|argument| asks(argument) || asks(&normalize_path(argument, cwd)))
}

/// PURE: whether a shell line runs the daemon's own settings command line: a word whose last path
/// component is `nucleos-core` or `nucleos-core.exe` (bare, through a path, quoted, after `cd x &&`,
/// behind `FOO=1`, or as cargo's `-p nucleos-core`), and a word that is exactly `--settings-reset`
/// or `--settings-rollback` (or either followed by `=value`). Both halves are required. A deliberate
/// over-deny: `grep nucleos-core --settings-reset` matches too.
pub(super) fn line_runs_settings_cli(command: &str) -> bool {
    use crate::settings_cli::{RESET_FLAG, ROLLBACK_FLAG};
    let words = shell_words(command);
    let pieces = || words.iter().flat_map(|word| nucleos_data_pieces(word));
    let is_daemon_binary = |piece: &str| {
        let name = piece
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(piece)
            .to_ascii_lowercase();
        name.strip_suffix(".exe").unwrap_or(&name) == "nucleos-core"
    };
    pieces().any(is_daemon_binary)
        && pieces().any(|piece| piece == RESET_FLAG || piece == ROLLBACK_FLAG)
}
