//! The config directory of one unit (a run or a chat): where the hook script, the settings file the
//! CLI is pointed at with `--settings`, and the MCP config live (sandbox plan D4 and D12).
//!
//! It is outside the working tree on purpose. The tree is what the unit writes; a hook or a
//! settings file the unit could rewrite is a hook it controls. Everything here is written through
//! the crossing, with the config dir as the only grant.

use std::path::{Path, PathBuf};

use crate::sandbox::write_crossing;

/// The base every unit config dir sits under: `<data dir>/units`. Under test, a directory of the
/// OS temp dir, so no test writes into the developer's own data directory.
fn base() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("nucleos-unit-config-test-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        crate::data_dir::data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("nucleos"))
            .join("units")
    }
}

/// The text a unit dir is keyed by: one spelling per directory. The daemon names the same tree as
/// `C:\x\tree`, `C:/x/tree`, `c:/x/tree/` or `\\?\C:\x\tree` depending on where the path came
/// from, and a key over the raw text gave each spelling its own config dir, so a hook wired under
/// one was missing under another. Separators become `/`, a verbatim prefix and a trailing `/` go,
/// and on Windows (a case-insensitive filesystem) the text is lowercased. Lexical only: the dir
/// may not exist yet, and canonicalizing would resolve a junction the unit is not meant to see.
fn key_text(unit_dir: &Path) -> String {
    let mut text = unit_dir.to_string_lossy().replace('\\', "/");
    if let Some(rest) = text.strip_prefix("//?/") {
        text = rest.to_string();
    }
    while text.len() > 1 && text.ends_with('/') {
        text.pop();
    }
    if cfg!(windows) {
        text = text.to_lowercase();
    }
    text
}

/// A stable, filesystem-safe key for a unit's working directory (FNV-1a over [`key_text`]).
fn key(unit_dir: &Path) -> String {
    let text = key_text(unit_dir);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // From the folded text too, so two spellings also agree on the readable half of the key.
    let name = text.rsplit('/').next().unwrap_or_default().to_string();
    let name: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    format!("{name}-{hash:016x}")
}

/// The config dir of the unit working in `unit_dir`. Pure: nothing is created.
///
/// Named `.claude` on purpose: it is the unit's `CLAUDE_CONFIG_DIR`, and the hook scripts inside it
/// keep the `.claude/hooks/...` tail every detector (`HOOK_SCRIPT`, the stop timer, the timeout
/// raiser) already matches on, so none of them needs to learn a second layout. The name is the
/// Claude adapter's own (`unit_config_dir_name`); [`unit_config_dir_for`] asks any other engine's.
pub fn unit_config_dir(unit_dir: &Path) -> PathBuf {
    base().join(key(unit_dir)).join(claude_dir_name())
}

/// The config dir of the unit working in `unit_dir`, for `engine`: the directory name is what that
/// engine's adapter declares. `None` for an engine outside the registry.
pub fn unit_config_dir_for(engine: &str, unit_dir: &Path) -> Option<PathBuf> {
    let name = crate::engines::adapters::execution_of(engine)?.unit_config_dir_name();
    Some(base().join(key(unit_dir)).join(name))
}

fn claude_dir_name() -> &'static str {
    use crate::engines::adapters::ExecutionAdapter;
    crate::engines::adapters::claude::engine::ClaudeEngine.unit_config_dir_name()
}

/// `path` as the forward-slash text a hook command carries, which `python` reads on every platform
/// and which keeps the `.claude/hooks/...` tail matchable on Windows.
pub fn command_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The settings file the CLI is pointed at with `--settings`.
pub fn settings_file(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// The hook script the settings name. Absolute in the command, so it never resolves into the tree.
pub fn hook_script(config_dir: &Path) -> PathBuf {
    config_dir.join("hooks").join("ask_daemon.py")
}

/// The MCP config of a chat or run, `name` being its file name.
pub fn mcp_file(config_dir: &Path, name: &str) -> PathBuf {
    config_dir.join("mcp").join(name)
}

/// The config dir of a chat or council, keyed by its id (any string: the key is sanitized and
/// hashed, so a hostile id cannot name a path).
pub fn chat_config_dir(id: &str) -> PathBuf {
    base()
        .join(format!("chat-{}", key(Path::new(id))))
        .join(claude_dir_name())
}

/// Writes an MCP config at `path` (made by [`mcp_file`]): the config dir is the grandparent, and
/// is the only grant of the crossing. The file holds registry env values, which are secrets.
pub fn write_mcp(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let config = path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("{} is not an MCP config path", path.display()))?;
    write_in(config, path, bytes)
}

/// Creates the config dir if needed and writes `bytes` to `path` inside it, through the crossing.
pub fn write_in(config_dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not make {}: {error}", parent.display()))?;
    write_crossing(path, &[config_dir.to_path_buf()], bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_config_dir_is_stable_and_never_inside_the_tree() {
        let tree = tempfile::TempDir::new().unwrap();
        let first = unit_config_dir(tree.path());
        assert_eq!(first, unit_config_dir(tree.path()));
        assert!(!first.starts_with(tree.path()));
        let other = tempfile::TempDir::new().unwrap();
        assert_ne!(first, unit_config_dir(other.path()));
    }

    /// The tree job asks about `C:/x/repo` for a root the test (or the runner) wired as `C:\x\repo`;
    /// both spellings must find the one config dir, or the hook is "not wired" where it is.
    #[test]
    fn every_spelling_of_a_unit_dir_shares_one_config_dir() {
        let one = unit_config_dir(Path::new(r"C:\x\repo"));
        assert_eq!(one, unit_config_dir(Path::new("C:/x/repo")));
        assert_eq!(one, unit_config_dir(Path::new("C:/x/repo/")));
        assert_eq!(one, unit_config_dir(Path::new(r"\\?\C:\x\repo")));
        if cfg!(windows) {
            assert_eq!(one, unit_config_dir(Path::new("c:/x/Repo")));
        }
        assert_ne!(one, unit_config_dir(Path::new("C:/x/repo2")));
    }

    #[test]
    fn write_in_refuses_a_path_outside_the_config_dir() {
        let config = tempfile::TempDir::new().unwrap();
        let elsewhere = tempfile::TempDir::new().unwrap();
        let target = elsewhere.path().join("x.json");
        assert!(write_in(config.path(), &target, b"{}").is_err());
        assert!(!target.exists());
        let inside = config.path().join("a").join("x.json");
        write_in(config.path(), &inside, b"{}").unwrap();
        assert_eq!(std::fs::read(&inside).unwrap(), b"{}");
    }
}
