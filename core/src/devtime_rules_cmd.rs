//! PURE: which programs are tests, builds, lints, mutations, sleeps, commits and reverts (the base
//! lists of `devtime.yaml` plus a project's extras), which subagent roles an `agentType` names, and
//! which paths are external or code. No I/O, no SQL, no rule: it answers "what is this program" and
//! the families decide what that means.
//!
//! The recognition functions are filled in by a later chunk; until then they answer "nothing".

use crate::config::{DevtimePathsConfig, DevtimeRolesConfig, DevtimeRulesConfig};
use crate::devtime_rules::Role;

/// What kind of check a shell call is. A program matching none of them is simply not classified, and
/// stays out of every rule that needs a check (spec §5, "fica fora").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdClass {
    Test,
    Build,
    Lint,
}

/// The program lists in force for one project: the base lists plus that project's extras for test,
/// build and lint.
#[derive(Debug, Clone, Default)]
pub struct CommandSet {
    pub test: Vec<String>,
    pub build: Vec<String>,
    pub lint: Vec<String>,
    pub mutating: Vec<String>,
    pub sleep: Vec<String>,
    pub commit: Vec<String>,
    pub revert_with_paths: Vec<String>,
    pub revert_pathless: Vec<String>,
    /// First words that run a script rather than name a program.
    pub interpreters: Vec<String>,
    pub shell_tools: Vec<String>,
    pub edit_tools: Vec<String>,
    pub read_tools: Vec<String>,
    pub search_tools: Vec<String>,
}

impl CommandSet {
    /// The base lists, with `per_project[project_id]` appended to test, build and lint.
    pub fn for_project(cfg: &DevtimeRulesConfig, project_id: &str) -> Self {
        let commands = &cfg.commands;
        let extra = commands.per_project.get(project_id);
        fn with_extra(base: &[String], extra: Option<&[String]>) -> Vec<String> {
            let mut all = base.to_vec();
            all.extend(extra.unwrap_or_default().iter().cloned());
            all
        }
        Self {
            test: with_extra(&commands.test, extra.map(|e| e.test.as_slice())),
            build: with_extra(&commands.build, extra.map(|e| e.build.as_slice())),
            lint: with_extra(&commands.lint, extra.map(|e| e.lint.as_slice())),
            mutating: commands.mutating.clone(),
            sleep: commands.sleep.clone(),
            commit: commands.commit.clone(),
            revert_with_paths: commands.revert_with_paths.clone(),
            revert_pathless: commands.revert_pathless.clone(),
            interpreters: commands.interpreters.clone(),
            shell_tools: commands.shell_tools.clone(),
            edit_tools: commands.edit_tools.clone(),
            read_tools: commands.read_tools.clone(),
            search_tools: commands.search_tools.clone(),
        }
    }
}

/// Whether `pattern` names `program`: both are split into words, lower-cased, reduced to their
/// basename and stripped of `.exe`; a leading interpreter on the program is dropped when the pattern
/// does not start with one (so `bash scripts/gates.sh` matches `scripts/gates.sh`); the pattern's
/// words must then be a prefix of the program's. Stub: false.
pub fn matches(_pattern: &str, _program: &str, _interpreters: &[String]) -> bool {
    false
}

/// Test, then Build, then Lint; the first match wins. `None` when nothing matches. Stub: `None`.
pub fn classify(_cmds: &CommandSet, _program: &str) -> Option<CmdClass> {
    None
}

/// A subagent's role by case-insensitive glob (`*` only) over its `agentType`; the reviewer patterns
/// are tried first, then the implementer ones. `None` when nothing matches. Stub: `None`.
pub fn role_of(_agent_type: &str, _roles: &DevtimeRolesConfig) -> Option<Role> {
    None
}

/// Which paths are outside the project's work (scratch, logs) and which are code.
#[derive(Debug, Clone, Default)]
pub struct PathMatcher {
    pub external: Vec<String>,
    pub code_extensions: Vec<String>,
}

impl PathMatcher {
    pub fn new(cfg: &DevtimePathsConfig) -> Self {
        Self {
            external: cfg.external.clone(),
            code_extensions: cfg.code_extensions.clone(),
        }
    }

    /// A path matching any `external` glob, case-insensitively and with `\` read as `/`. Stub: false.
    pub fn is_external(&self, _path: &str) -> bool {
        false
    }

    /// A path whose extension is in `code_extensions`. Stub: false.
    pub fn is_code(&self, _path: &str) -> bool {
        false
    }
}
