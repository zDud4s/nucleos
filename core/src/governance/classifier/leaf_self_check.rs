//! The self-check a tree leaf may run before closing, each form wrapped by
//! `python scripts/heavy.py`: the formatter, the invariant checker and (only with the setting on)
//! a crate check. PURE form checks plus one comparison of the scripts with the main checkout's
//! copies.

use std::path::Path;

use super::shell_shape::program_name;
use super::write_guards::{heavy_wrapper_arguments, main_checkout_of, split_first_token};

/// PURE: whether `command` is exactly one of the forms a leaf run may self-check with, each wrapped
/// by `python scripts/heavy.py`: `cargo fmt -p <crate>`, `python scripts/check_invariants.py`, and
/// (only with `self_check`) `cargo check -p <crate> --tests`. Anything with a shell metacharacter,
/// a quote or another shape is `false`. Says nothing about whether the scripts are trustworthy:
/// see `leaf_self_check_scripts_intact`.
pub fn leaf_self_check_form(command: &str, self_check: bool) -> bool {
    // The harmless dressing a leaf puts around the call: a `timeout N` prefix, a `2>&1` fold, a
    // `| tail -N` / `| head -N` cut and the `; echo rc=$?` / `; git status` tail. Each is parsed as
    // a whole piece, never as a stray metacharacter, so the core below still sees none.
    let mut pieces = command.split(';');
    let main = pieces.next().unwrap_or("");
    let dressing_ok = pieces.all(|piece| {
        let words: Vec<&str> = piece.split_whitespace().collect();
        matches!(
            words.as_slice(),
            ["echo", "rc=$?"] | ["git", "status"] | ["git", "status", "--short" | "-s"]
        )
    });
    if !dressing_ok {
        return false;
    }
    let is_count = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit());
    let mut stages = main.split('|');
    let core = stages.next().unwrap_or("");
    let cut_ok = match (stages.next(), stages.next()) {
        (None, _) => true,
        (Some(cut), None) => {
            let words: Vec<&str> = cut.split_whitespace().collect();
            match words.as_slice() {
                ["tail" | "head", n] => n.strip_prefix('-').is_some_and(is_count),
                ["tail" | "head", "-n", n] => is_count(n),
                _ => false,
            }
        }
        _ => false,
    };
    if !cut_ok {
        return false;
    }
    let mut core_words: Vec<&str> = core.split_whitespace().collect();
    if core_words.last() == Some(&"2>&1") {
        core_words.pop();
    }
    if core_words.first() == Some(&"timeout") && core_words.get(1).is_some_and(|n| is_count(n)) {
        core_words.drain(..2);
    }
    leaf_self_check_core(&core_words.join(" "), self_check)
}

/// PURE: the bare wrapped form, with no dressing around it. See `leaf_self_check_form`.
fn leaf_self_check_core(command: &str, self_check: bool) -> bool {
    const FORBIDDEN: &[char] = &[
        ';', '|', '&', '<', '>', '$', '`', '\n', '\r', '(', ')', '{', '}', '*', '?', '~', '!', '#',
        '\'', '"',
    ];
    if command.contains(FORBIDDEN) {
        return false;
    }
    let script_is = |token: &str, name: &str| {
        let token = token.replace('\\', "/");
        token.strip_prefix("./").unwrap_or(&token) == name
    };
    let is_python = |token: &str| {
        let program = program_name(token).to_ascii_lowercase();
        program.starts_with("python") || program == "py"
    };
    let Some((program, rest)) = split_first_token(command) else {
        return false;
    };
    let Some((script, rest)) = split_first_token(rest) else {
        return false;
    };
    if !is_python(&program) || !script_is(&script, "scripts/heavy.py") {
        return false;
    }
    let Some(wrapped) = heavy_wrapper_arguments(rest) else {
        return false;
    };
    let words: Vec<&str> = wrapped.split_whitespace().collect();
    let crate_name_ok = |name: &str| {
        !name.starts_with('-')
            && !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    match words.as_slice() {
        ["cargo", "fmt", "-p", name] => crate_name_ok(name),
        ["cargo", "check", "-p", name, "--tests"] => self_check && crate_name_ok(name),
        [py, script] => is_python(py) && script_is(script, "scripts/check_invariants.py"),
        _ => false,
    }
}

/// Whether the scripts a self-check line would run are byte-identical to the main checkout's. The
/// worktree's copy is a file the run could have rewritten, so it is trusted only when it equals the
/// copy the run cannot touch. Any failure to resolve or read is `false`.
pub fn leaf_self_check_scripts_intact(cwd: &Path, command: &str) -> bool {
    let Some(main) = main_checkout_of(cwd) else {
        return false;
    };
    let Some(tree) = cwd.ancestors().find(|a| a.join(".git").exists()) else {
        return false;
    };
    let (main_scripts, tree_scripts) = (Path::new(&main).join("scripts"), tree.join("scripts"));
    let mut names = vec!["heavy.py".to_owned()];
    if command.contains("check_invariants") {
        names.push("check_invariants.py".to_owned());
    }
    if let Ok(entries) = std::fs::read_dir(&main_scripts) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("heavy_") && name.ends_with(".py") && !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names.iter().all(|name| {
        match (
            std::fs::read(main_scripts.join(name)),
            std::fs::read(tree_scripts.join(name)),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    })
}
