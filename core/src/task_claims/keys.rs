//! Canonical task keys (spec task-claims section 1, "Chaves").
//!
//! A fixed shape per kind -- no free text -- is what lets the daemon deduplicate: two agents that
//! describe the same failing test write the same key. Pure string work, no filesystem access, so it
//! behaves the same for a Windows path typed on a POSIX box in a test.
//!
//! | key | use |
//! |---|---|
//! | `master-red` | one live incident per project |
//! | `test:<path>::<name>` | one failing test |
//! | `file:<path>` | a bug localised in a file |
//! | `spec-item:<spec>#<n>` | an item of a spec checklist |
//! | `topic:<slug>` | last resort |

use std::path::Path;

/// A normalised key. Only [`normalize`] and the store (reading a row back) build one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct TaskKey(pub(super) String);

impl TaskKey {
    /// The one incident key per project.
    pub fn master_red() -> Self {
        Self(super::MASTER_RED_KEY.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The part before the first `:` (the whole key for `master-red`).
    pub fn prefix(&self) -> &str {
        self.0.split_once(':').map_or(self.0.as_str(), |(prefix, _)| prefix)
    }

    /// The file the key points at, for the kinds that have one (`test`, `file`, `spec-item`).
    pub fn path(&self) -> Option<&str> {
        let (prefix, body) = self.0.split_once(':')?;
        match prefix {
            "test" => body.split_once("::").map(|(path, _)| path),
            "file" => Some(body),
            "spec-item" => body.split_once('#').map(|(path, _)| path),
            _ => None,
        }
    }
}

impl std::fmt::Display for TaskKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    Empty,
    UnknownPrefix(String),
    Malformed { prefix: String, expected: &'static str },
    OutsideRoot(String),
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "empty task key"),
            Self::UnknownPrefix(prefix) => write!(
                formatter,
                "unknown task key prefix '{prefix}' (use master-red, test:, file:, spec-item: or topic:)"
            ),
            Self::Malformed { prefix, expected } => {
                write!(formatter, "malformed '{prefix}' key: expected {expected}")
            }
            Self::OutsideRoot(path) => write!(formatter, "path '{path}' is outside the project root"),
        }
    }
}

impl std::error::Error for KeyError {}

fn malformed(prefix: &str, expected: &'static str) -> KeyError {
    KeyError::Malformed { prefix: prefix.to_owned(), expected }
}

/// Normalises `raw`: lowercase prefix, `\` to `/`, absolute paths made relative to `project_root`,
/// `.`/`..` resolved lexically, slug in `[a-z0-9-]`. Unknown prefixes are refused.
pub fn normalize(raw: &str, project_root: &Path) -> Result<TaskKey, KeyError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(KeyError::Empty);
    }
    let (prefix, body) = match raw.split_once(':') {
        Some((prefix, body)) => (prefix.trim().to_ascii_lowercase(), body.trim()),
        None => (raw.to_ascii_lowercase(), ""),
    };
    let root = root_text(project_root);
    let key = match prefix.as_str() {
        "master-red" => {
            if !body.is_empty() {
                return Err(malformed("master-red", "no body (the shas live in the detail)"));
            }
            super::MASTER_RED_KEY.to_owned()
        }
        "file" => format!("file:{}", normalize_path_in(body, &root)?),
        "test" => {
            let (path, name) = body
                .split_once("::")
                .ok_or_else(|| malformed("test", "test:<path>::<name>"))?;
            let name = name.trim();
            if name.is_empty() {
                return Err(malformed("test", "test:<path>::<name> with a non-empty name"));
            }
            format!("test:{}::{name}", normalize_path_in(path.trim(), &root)?)
        }
        "spec-item" => {
            let (spec, index) = body
                .split_once('#')
                .ok_or_else(|| malformed("spec-item", "spec-item:<spec>#<n>"))?;
            let index: u32 = index
                .trim()
                .parse()
                .map_err(|_| malformed("spec-item", "a numeric item index after '#'"))?;
            format!("spec-item:{}#{index}", normalize_path_in(spec.trim(), &root)?)
        }
        "topic" => format!("topic:{}", slug(body).ok_or_else(|| malformed("topic", "a slug with letters or digits"))?),
        other => return Err(KeyError::UnknownPrefix(other.to_owned())),
    };
    Ok(TaskKey(key))
}

/// A path as the claims store keeps it: relative to the project root, `/`-separated, no `.`/`..`.
/// Also used for `guard_paths` and `hint_paths` coming from the agents.
pub fn normalize_path(raw: &str, project_root: &Path) -> Result<String, KeyError> {
    normalize_path_in(raw, &root_text(project_root))
}

fn root_text(root: &Path) -> String {
    let text = root.to_string_lossy().replace('\\', "/");
    strip_verbatim(&text).trim_end_matches('/').to_owned()
}

fn strip_verbatim(text: &str) -> &str {
    text.strip_prefix("//?/").unwrap_or(text)
}

fn is_drive(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn normalize_path_in(raw: &str, root: &str) -> Result<String, KeyError> {
    let slashed = raw.trim().replace('\\', "/");
    let text = strip_verbatim(&slashed);
    if text.is_empty() {
        return Err(malformed("path", "a non-empty path"));
    }
    let relative = if text.starts_with('/') || is_drive(text) {
        strip_root(text, root).ok_or_else(|| KeyError::OutsideRoot(raw.to_owned()))?
    } else {
        text
    };
    let mut parts: Vec<&str> = Vec::new();
    for segment in relative.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(KeyError::OutsideRoot(raw.to_owned()));
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return Err(malformed("path", "a path below the project root"));
    }
    Ok(parts.join("/"))
}

/// `text` without the `root` prefix, when it is under it. Windows roots compare ignoring ASCII case;
/// a POSIX root is exact. `root-other` is not under `root`: the next character must be `/` or the end.
fn strip_root<'a>(text: &'a str, root: &str) -> Option<&'a str> {
    if root.is_empty() {
        return None;
    }
    let head = text.get(..root.len())?;
    let same = if is_drive(root) { head.eq_ignore_ascii_case(root) } else { head == root };
    if !same {
        return None;
    }
    let rest = &text[root.len()..];
    (rest.is_empty() || rest.starts_with('/')).then_some(rest)
}

fn slug(body: &str) -> Option<String> {
    let mut out = String::new();
    for character in body.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            out.push(character);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn key(raw: &str) -> Result<String, KeyError> {
        normalize(raw, Path::new("/repo")).map(|k| k.as_str().to_owned())
    }

    #[test]
    fn the_prefix_is_lowercased_and_master_red_takes_no_body() {
        assert_eq!(key("MASTER-RED").unwrap(), "master-red");
        assert_eq!(key("  master-red  ").unwrap(), "master-red");
        assert!(matches!(key("master-red:abc123"), Err(KeyError::Malformed { .. })));
    }

    #[test]
    fn unknown_prefixes_and_empty_keys_are_refused() {
        assert_eq!(key("bug:foo"), Err(KeyError::UnknownPrefix("bug".to_owned())));
        assert_eq!(key("nocolon"), Err(KeyError::UnknownPrefix("nocolon".to_owned())));
        assert_eq!(key("   "), Err(KeyError::Empty));
    }

    #[test]
    fn paths_become_relative_slash_separated_and_lexically_resolved() {
        assert_eq!(key("file:/repo/core/a.rs").unwrap(), "file:core/a.rs");
        assert_eq!(key("File:.\\core\\\\a.rs").unwrap(), "file:core/a.rs");
        assert_eq!(key("file:core/../core/a.rs").unwrap(), "file:core/a.rs");
        assert!(matches!(key("file:/elsewhere/a.rs"), Err(KeyError::OutsideRoot(_))));
        assert!(matches!(key("file:../a.rs"), Err(KeyError::OutsideRoot(_))));
        assert!(matches!(key("file:/repo"), Err(KeyError::Malformed { .. })), "the root is not a file");
    }

    #[test]
    fn windows_roots_match_ignoring_case_and_the_verbatim_prefix() {
        let root = Path::new("C:\\Projects\\repo");
        let got = |raw: &str| normalize(raw, root).map(|k| k.as_str().to_owned());
        assert_eq!(got("file:C:\\Projects\\repo\\core\\src\\a.rs").unwrap(), "file:core/src/a.rs");
        assert_eq!(got("file:c:\\projects\\REPO\\x.rs").unwrap(), "file:x.rs");
        assert_eq!(got("file://?/C:/Projects/repo/x.rs").unwrap(), "file:x.rs");
        assert!(matches!(got("file:C:\\Projects\\repo-other\\x.rs"), Err(KeyError::OutsideRoot(_))));
    }

    #[test]
    fn test_keys_keep_the_test_name_exactly_and_normalize_only_the_path() {
        assert_eq!(
            key("TEST:core\\src\\a.rs::mod_x::It_Works").unwrap(),
            "test:core/src/a.rs::mod_x::It_Works"
        );
        assert!(matches!(key("test:core/a.rs"), Err(KeyError::Malformed { .. })));
        assert!(matches!(key("test:core/a.rs::  "), Err(KeyError::Malformed { .. })));
    }

    #[test]
    fn spec_item_keys_carry_a_numeric_index() {
        assert_eq!(key("spec-item:.ai/specs/x.md#03").unwrap(), "spec-item:.ai/specs/x.md#3");
        assert!(matches!(key("spec-item:x.md#a"), Err(KeyError::Malformed { .. })));
        assert!(matches!(key("spec-item:x.md"), Err(KeyError::Malformed { .. })));
    }

    #[test]
    fn topic_slugs_are_lowercase_ascii_with_single_dashes() {
        assert_eq!(key("topic:Fix the CI!!  now").unwrap(), "topic:fix-the-ci-now");
        assert!(matches!(key("topic:!!!"), Err(KeyError::Malformed { .. })));
    }

    #[test]
    fn a_key_knows_its_prefix_and_its_path() {
        let test = normalize("test:core/a.rs::x", Path::new("/repo")).unwrap();
        assert_eq!((test.prefix(), test.path()), ("test", Some("core/a.rs")));
        let file = normalize("file:core/a.rs", Path::new("/repo")).unwrap();
        assert_eq!((file.prefix(), file.path()), ("file", Some("core/a.rs")));
        let red = TaskKey::master_red();
        assert_eq!((red.prefix(), red.path()), ("master-red", None));
        let spec = normalize("spec-item:s.md#2", Path::new("/repo")).unwrap();
        assert_eq!(spec.path(), Some("s.md"));
    }

    #[test]
    fn normalize_path_serves_guard_and_hint_paths_too() {
        assert_eq!(normalize_path("\\repo\\core\\a.rs", Path::new("/repo")).unwrap(), "core/a.rs");
        assert_eq!(normalize_path("core/a.rs", Path::new("/repo")).unwrap(), "core/a.rs");
    }
}
