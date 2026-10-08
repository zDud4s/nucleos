//! PURE: which programs are tests, builds, lints, mutations, sleeps, commits and reverts (the base
//! lists of `devtime.yaml` plus a project's extras), which subagent roles an `agentType` names, and
//! which paths are external or code. No I/O, no SQL, no rule: it answers "what is this program" and
//! the families decide what that means.
//!
//! Matching follows the spec §5 "Comandos de teste e build" and "Papéis de subagente": every list
//! comes from `DevtimeRulesConfig`, so a new test runner or subagent role is a config line.

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

/// The words of a command or a pattern: lower-cased, each reduced to its basename (after the last
/// `/` or `\`) and stripped of `.exe`, so `C:\tools\Cargo.EXE test` and `cargo test` read the same.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            let lower = word.to_lowercase();
            let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
            base.strip_suffix(".exe").unwrap_or(base).to_string()
        })
        .collect()
}

/// Whether `pattern` names `program`: both are split into words, lower-cased, reduced to their
/// basename and stripped of `.exe`; a leading interpreter on the program is dropped when the pattern
/// does not start with one (so `bash scripts/gates.sh` matches `scripts/gates.sh`); the pattern's
/// words must then be a prefix of the program's. An empty pattern matches nothing.
pub fn matches(pattern: &str, program: &str, interpreters: &[String]) -> bool {
    let pattern = words(pattern);
    let mut program = words(program);
    if pattern.is_empty() {
        return false;
    }
    let is_interpreter = |word: &str| {
        interpreters
            .iter()
            .any(|interpreter| interpreter.to_lowercase() == word)
    };
    if program
        .first()
        .is_some_and(|first| is_interpreter(first.as_str()))
        && !is_interpreter(pattern[0].as_str())
    {
        program.remove(0);
    }
    program.len() >= pattern.len() && program[..pattern.len()] == pattern[..]
}

/// Whether any pattern of `patterns` names `program` (see [`matches`]).
pub fn matches_any(patterns: &[String], program: &str, interpreters: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| matches(pattern, program, interpreters))
}

/// Test, then Build, then Lint; the first match wins. `None` when nothing matches.
pub fn classify(cmds: &CommandSet, program: &str) -> Option<CmdClass> {
    [
        (&cmds.test, CmdClass::Test),
        (&cmds.build, CmdClass::Build),
        (&cmds.lint, CmdClass::Lint),
    ]
    .into_iter()
    .find(|(list, _)| matches_any(list, program, &cmds.interpreters))
    .map(|(_, class)| class)
}

/// Case-insensitive glob where `*` matches any run of characters (including none, and including `/`)
/// and every other character matches itself.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let (mut p, mut t) = (0, 0);
    // The last `*` seen, and where in the text its current expansion ends.
    let mut star: Option<usize> = None;
    let mut resume = 0;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some(at) = star {
            p = at + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// A subagent's role by case-insensitive glob (`*` only) over its `agentType`; the reviewer patterns
/// are tried first, then the implementer ones. `None` when nothing matches (spec §5: an `agentType`
/// with no role takes no part in the role rules).
pub fn role_of(agent_type: &str, roles: &DevtimeRolesConfig) -> Option<Role> {
    let any = |patterns: &[String]| {
        patterns
            .iter()
            .any(|pattern| !pattern.is_empty() && glob_match(pattern, agent_type))
    };
    if any(&roles.reviewer) {
        Some(Role::Reviewer)
    } else if any(&roles.implementer) {
        Some(Role::Implementer)
    } else {
        None
    }
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

    /// A path matching any `external` glob, case-insensitively and with `\` read as `/`.
    pub fn is_external(&self, path: &str) -> bool {
        let path = path.replace('\\', "/");
        self.external
            .iter()
            .any(|pattern| !pattern.is_empty() && glob_match(&pattern.replace('\\', "/"), &path))
    }

    /// A path whose extension (after the last `.` of its last component, case-insensitively) is in
    /// `code_extensions`. A leading dot in a configured extension is ignored.
    pub fn is_code(&self, path: &str) -> bool {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let Some((stem, extension)) = name.rsplit_once('.') else {
            return false;
        };
        !stem.is_empty()
            && self
                .code_extensions
                .iter()
                .any(|code| code.trim_start_matches('.').eq_ignore_ascii_case(extension))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds_for(cfg: &DevtimeRulesConfig, project: &str) -> CommandSet {
        CommandSet::for_project(cfg, project)
    }

    #[test]
    fn base_and_project_extras_classify_and_unknown_stays_none() {
        let mut cfg = DevtimeRulesConfig::default();
        let base = cmds_for(&cfg, "p1");
        assert_eq!(classify(&base, "cargo test"), Some(CmdClass::Test));
        assert_eq!(
            classify(&base, "bash scripts/gates.sh"),
            None,
            "a project script is no test until the project says so"
        );
        assert_eq!(classify(&base, "ls"), None);
        assert_eq!(classify(&base, "make test"), Some(CmdClass::Test));
        assert_eq!(classify(&base, "make"), Some(CmdClass::Build));
        assert_eq!(classify(&base, "cargo clippy"), Some(CmdClass::Lint));
        assert_eq!(classify(&base, "cargo build"), Some(CmdClass::Build));
        assert_eq!(
            classify(&base, "C:/tools/Cargo.exe test"),
            Some(CmdClass::Test),
            "basename and .exe are ignored"
        );

        cfg.commands.per_project.insert(
            "p1".to_string(),
            crate::config::DevtimeProjectCommands {
                test: vec!["scripts/gates.sh".to_string()],
                ..Default::default()
            },
        );
        let p1 = cmds_for(&cfg, "p1");
        assert_eq!(
            classify(&p1, "bash scripts/gates.sh"),
            Some(CmdClass::Test),
            "the interpreter is dropped before the match"
        );
        assert_eq!(classify(&p1, "scripts/gates.sh"), Some(CmdClass::Test));
        assert_eq!(
            classify(&p1, "cargo test"),
            Some(CmdClass::Test),
            "the base list stays"
        );
        let p2 = cmds_for(&cfg, "p2");
        assert_eq!(
            classify(&p2, "bash scripts/gates.sh"),
            None,
            "another project does not get p1's extras"
        );
    }

    #[test]
    fn matching_is_by_word_prefix() {
        let interpreters = vec!["bash".to_string(), "python".to_string()];
        assert!(matches("cargo test", "cargo test", &interpreters));
        assert!(matches("cargo", "cargo test", &interpreters));
        assert!(!matches("cargo test", "cargo", &interpreters));
        assert!(!matches("cargo test", "cargo testing", &interpreters));
        assert!(!matches("", "cargo", &interpreters));
        assert!(matches("Start-Sleep", "start-sleep", &interpreters));
        // A pattern that itself starts with an interpreter keeps the program's.
        assert!(matches("python -m", "python -m", &interpreters));
        assert!(!matches("pytest", "python", &interpreters));
    }

    #[test]
    fn roles_match_by_glob_reviewer_first() {
        let roles = DevtimeRolesConfig::default();
        assert_eq!(
            role_of("superpowers:code-reviewer", &roles),
            Some(Role::Reviewer)
        );
        assert_eq!(role_of("wf-executor", &roles), Some(Role::Implementer));
        assert_eq!(role_of("general-purpose", &roles), Some(Role::Implementer));
        assert_eq!(role_of("Explore", &roles), None);
        assert_eq!(
            role_of("wf-executor-reviewer", &roles),
            Some(Role::Reviewer),
            "the reviewer patterns are tried first"
        );
        assert_eq!(
            role_of("WF-EXECUTOR", &roles),
            Some(Role::Implementer),
            "case-insensitive"
        );
    }

    #[test]
    fn external_and_code_paths() {
        let matcher = PathMatcher::new(&DevtimePathsConfig::default());
        assert!(matcher.is_external("C:\\Users\\x\\AppData\\Local\\Temp\\a.txt"));
        assert!(matcher.is_external("/tmp/run/out.txt"));
        assert!(matcher.is_external("C:/Users/x/scratchpad/notes.md"));
        assert!(matcher.is_external("logs/build.LOG"));
        assert!(matcher.is_external("dump.output"));
        assert!(!matcher.is_external("core/src/main.rs"));
        assert!(!matcher.is_external("docs/template.md"));

        assert!(matcher.is_code("core/src/main.rs"));
        assert!(matcher.is_code("shell\\src\\App.TSX"));
        assert!(matcher.is_code("scripts/gates.sh"));
        assert!(!matcher.is_code("README.md"));
        assert!(!matcher.is_code("Makefile"));
        assert!(!matcher.is_code(".rs"), "a dotfile has no extension");
        assert!(!matcher.is_code("src/rs"));

        assert!(glob_match("*review*", "Code-Reviewer"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
        assert!(glob_match("*", ""));
        assert!(glob_match("exact", "EXACT"));
        assert!(!glob_match("exact", "exacts"));
    }
}
