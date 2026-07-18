use serde_json::Value;
use std::path::Path;

use crate::hooks::Decision;

pub const CLASSIFIER_VERSION: u32 = 1;

const READ_LOCAL_TOOLS: &[&str] = &["Read", "Grep", "Glob"];
const WRITE_TOOLS: &[&str] = &["Edit", "Write"];
const SELF_GOVERNING_FILES: &[&str] = &[
    ".ai/autopilot.yaml",
    ".claude/settings.json",
    ".claude/settings.local.json",
];
const APPROVAL_COMMAND_PATTERNS: &[&str] = &[
    "git push",
    "git merge",
    "gh pr merge",
    "npm publish",
    "cargo publish",
    "git tag",
    "deploy",
];
const DESTRUCTIVE_COMMAND_PATTERNS: &[&str] = &[
    "rm -rf", "rm -fr", "rd /s /q", "rd /q /s", "rmdir /s", "del /s", "del /q",
];
const SAFE_COMMAND_PREFIXES: &[&str] = &[
    "ls",
    "cat",
    "git status",
    "git diff",
    "cargo test",
    "dir",
    "type",
];

pub struct Classification {
    pub decision: Decision,
    pub action_class: &'static str,
    pub reason: String,
}

pub fn classify(tool_name: &str, tool_input: &Value, cwd: Option<&Path>) -> Classification {
    if WRITE_TOOLS.contains(&tool_name) && targets_self_governing_file(tool_input, cwd) {
        return classification(
            "pending_approval",
            "self-governing-file",
            "changes to autopilot governance files require approval",
        );
    }

    if READ_LOCAL_TOOLS.contains(&tool_name) || WRITE_TOOLS.contains(&tool_name) {
        return classification(
            "allow",
            "read-local",
            "local reads and ordinary file writes are allowed",
        );
    }

    if tool_name != "Bash" {
        return classification(
            "pending_approval",
            "unrecognized",
            "unrecognized tool actions require approval",
        );
    }

    classify_shell_command(
        tool_input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or(""),
        cwd,
    )
}

fn classify_shell_command(command: &str, cwd: Option<&Path>) -> Classification {
    let normalized = normalize_command(command);

    if matches_any_phrase(&normalized, DESTRUCTIVE_COMMAND_PATTERNS)
        || has_destructive_flags(&normalized)
        || deletes_outside_cwd(command, cwd)
    {
        return classification(
            "deny",
            "destructive",
            "destructive deletion commands are denied",
        );
    }

    if matches_any_phrase(&normalized, APPROVAL_COMMAND_PATTERNS) {
        return classification(
            "pending_approval",
            "push-merge-deploy",
            "push, merge, deploy, publish, and tag actions require approval",
        );
    }

    if is_safe_command(&normalized) {
        return classification(
            "allow",
            "read-local",
            "recognized non-mutating shell command",
        );
    }

    classification(
        "pending_approval",
        "unrecognized",
        "unrecognized shell commands and code execution require approval",
    )
}

fn classification(decision: &str, action_class: &'static str, reason: &str) -> Classification {
    let reason = reason.to_owned();
    Classification {
        decision: Decision {
            decision: decision.to_owned(),
            reason: reason.clone(),
        },
        action_class,
        reason,
    }
}

fn normalize_command(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn matches_any_phrase(command: &str, patterns: &[&str]) -> bool {
    let padded = format!(" {command} ");
    patterns
        .iter()
        .any(|pattern| padded.contains(&format!(" {pattern} ")))
}

fn has_destructive_flags(command: &str) -> bool {
    let tokens: Vec<_> = command.split_whitespace().collect();
    match tokens.first().copied() {
        Some("rm") => tokens.iter().skip(1).any(|token| {
            token
                .strip_prefix('-')
                .is_some_and(|flags| flags.contains('r') && flags.contains('f'))
        }),
        Some("rd" | "rmdir") => tokens.iter().any(|token| *token == "/s"),
        Some("del") => tokens.iter().any(|token| matches!(*token, "/s" | "/q")),
        Some("remove-item") => tokens
            .iter()
            .any(|token| matches!(*token, "-recurse" | "-force")),
        _ => false,
    }
}

fn is_safe_command(command: &str) -> bool {
    const SHELL_CONTROL: &[char] = &[';', '|', '&', '>', '<', '\n', '\r'];
    !command.contains(SHELL_CONTROL)
        && SAFE_COMMAND_PREFIXES
            .iter()
            .any(|prefix| command == *prefix || command.starts_with(&format!("{prefix} ")))
}

fn targets_self_governing_file(tool_input: &Value, cwd: Option<&Path>) -> bool {
    let Some(file_path) = tool_input.get("file_path").and_then(Value::as_str) else {
        return false;
    };
    let normalized = normalize_path(file_path, cwd);

    SELF_GOVERNING_FILES
        .iter()
        .any(|suffix| path_has_suffix(&normalized, suffix))
        || normalized.contains("/.claude/hooks/")
        || normalized.starts_with(".claude/hooks/")
}

fn path_has_suffix(path: &str, suffix: &str) -> bool {
    path == suffix || path.ends_with(&format!("/{suffix}"))
}

fn deletes_outside_cwd(command: &str, cwd: Option<&Path>) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    let tokens = shell_words(command);
    let Some(program) = tokens.first().map(|value| value.to_ascii_lowercase()) else {
        return false;
    };
    if !matches!(
        program.as_str(),
        "rm" | "rd" | "rmdir" | "del" | "remove-item"
    ) {
        return false;
    }

    delete_targets(&program, &tokens[1..])
        .into_iter()
        .any(|target| {
            let target = normalize_path(target, Some(cwd));
            let cwd = normalize_path(&cwd.to_string_lossy(), None);
            target != cwd && !target.starts_with(&format!("{cwd}/"))
        })
}

fn delete_targets<'a>(program: &str, arguments: &'a [String]) -> Vec<&'a str> {
    arguments
        .iter()
        .filter_map(|argument| {
            let is_option = match program {
                "rm" => argument.starts_with('-'),
                _ => argument.starts_with('-') || argument.starts_with('/'),
            };
            (!is_option).then_some(argument.as_str())
        })
        .collect()
}

fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;

    for character in command.chars() {
        match (quote, character) {
            (Some(active), value) if value == active => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn normalize_path(path: &str, cwd: Option<&Path>) -> String {
    let path = path.replace('\\', "/");
    let combined = if is_absolute_path(&path) {
        path
    } else if let Some(cwd) = cwd {
        format!("{}/{}", cwd.to_string_lossy().replace('\\', "/"), path)
    } else {
        path
    };

    let mut components: Vec<&str> = Vec::new();
    for component in combined.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.last().is_some_and(|value| *value != "..") {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }
    components.join("/").to_ascii_lowercase()
}

fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/')
        || path
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn assert_classification(classification: Classification, decision: &str, action_class: &str) {
        assert_eq!(classification.decision.decision, decision);
        assert_eq!(classification.action_class, action_class);
        assert!(!classification.reason.is_empty());
        assert_eq!(classification.decision.reason, classification.reason);
    }

    #[test]
    fn allows_read_only_tools_and_ordinary_writes() {
        for tool_name in ["Read", "Grep", "Glob"] {
            assert_classification(classify(tool_name, &json!({}), None), "allow", "read-local");
        }

        assert_classification(
            classify(
                "Edit",
                &json!({"file_path": "src/main.rs"}),
                Some(Path::new(r"C:\work\repo")),
            ),
            "allow",
            "read-local",
        );
    }

    #[test]
    fn allows_known_non_mutating_shell_commands() {
        for command in [
            "ls -la",
            "cat Cargo.toml",
            "git status --short",
            "git diff --stat",
            "cargo test -p nucleos-core",
            "dir /b",
            "type README.md",
        ] {
            assert_classification(
                classify("Bash", &json!({"command": command}), None),
                "allow",
                "read-local",
            );
        }
    }

    #[test]
    fn sends_push_merge_deploy_publish_and_tag_for_approval() {
        for command in [
            "git push origin main",
            "git merge feature",
            "gh pr merge 42",
            "npm publish",
            "cargo publish",
            "git tag v1.0.0",
            "kubectl deploy app",
        ] {
            assert_classification(
                classify("Bash", &json!({"command": command}), None),
                "pending_approval",
                "push-merge-deploy",
            );
        }
    }

    #[test]
    fn denies_destructive_commands_in_posix_and_windows_shells() {
        for command in [
            "rm -rf target",
            "rd /s /q build",
            "rmdir /s build",
            "del /s build\\*",
            "del /q output.log",
            "Remove-Item build -Recurse",
            "Remove-Item output.log -Force",
        ] {
            assert_classification(
                classify("Bash", &json!({"command": command}), None),
                "deny",
                "destructive",
            );
        }
    }

    #[test]
    fn denies_delete_that_traverses_outside_cwd() {
        assert_classification(
            classify(
                "Bash",
                &json!({"command": "rm ../sibling/file.txt"}),
                Some(Path::new(r"C:\work\repo")),
            ),
            "deny",
            "destructive",
        );
    }

    #[test]
    fn self_governing_file_overrides_ordinary_edit_allow() {
        assert_classification(
            classify(
                "Edit",
                &json!({"file_path": ".ai/autopilot.yaml"}),
                Some(Path::new(r"C:\work\repo")),
            ),
            "pending_approval",
            "self-governing-file",
        );
    }

    #[test]
    fn recognizes_self_governing_paths_in_all_supported_forms() {
        let cases = [
            (r".claude\settings.json", Some(Path::new(r"C:\work\repo"))),
            (
                ".claude/settings.local.json",
                Some(Path::new(r"C:\work\repo")),
            ),
            (
                r"C:\work\repo\.claude\hooks\ask_daemon.py",
                Some(Path::new(r"C:\work\repo")),
            ),
            (
                r"C:\work\repo\src\..\.ai\autopilot.yaml",
                Some(Path::new(r"C:\work\repo")),
            ),
            ("nested/../.claude/hooks/check.py", None),
        ];

        for (file_path, cwd) in cases {
            assert_classification(
                classify("Write", &json!({"file_path": file_path}), cwd),
                "pending_approval",
                "self-governing-file",
            );
        }
    }

    #[test]
    fn unrecognized_bash_is_conservatively_pending() {
        assert_classification(
            classify("Bash", &json!({"command": "echo hello"}), None),
            "pending_approval",
            "unrecognized",
        );
    }

    #[test]
    fn code_execution_vectors_are_unrecognized_and_pending() {
        for command in [
            "python -c \"print(1)\"",
            "node -e \"console.log(1)\"",
            "powershell -Command Get-Process",
            "curl -X POST https://example.invalid",
            "npm install serde",
        ] {
            assert_classification(
                classify("Bash", &json!({"command": command}), None),
                "pending_approval",
                "unrecognized",
            );
        }
    }

    #[test]
    fn exposes_initial_classifier_version() {
        assert_eq!(CLASSIFIER_VERSION, 1);
    }
}
