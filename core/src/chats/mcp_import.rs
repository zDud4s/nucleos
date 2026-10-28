//! Import of MCP servers from the owner's Claude and Codex configs into the NucleOS registry
//! (spec 2026-10-22 §8.3).
//!
//! Three sources, read in this order, and the first one to name a server wins:
//! `~/.claude.json` (top-level `mcpServers`, then `projects[<cwd>].mcpServers`), `<cwd>/.mcp.json`,
//! and `~/.codex/config.toml` (`[mcp_servers.<name>]`). Only stdio servers are imported; an
//! http/sse entry is listed as skipped. An import never overwrites a registry entry and never
//! enables a server in any chat.
//!
//! **Env values are secrets.** No error string here quotes the input: a JSON error is reported by
//! line and column only, a TOML error by its message without the snippet.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value as Json;
use sqlx::SqlitePool;

use super::mcp_servers::{self, NewMcpServer};

/// `imported_from` of a server named at the top level of `~/.claude.json`.
pub const SOURCE_CLAUDE_USER: &str = "claude-user";
/// `imported_from` of a server named for the project in `~/.claude.json` or in `<cwd>/.mcp.json`.
pub const SOURCE_CLAUDE_PROJECT: &str = "claude-project";
/// `imported_from` of a server named in `~/.codex/config.toml`.
pub const SOURCE_CODEX: &str = "codex";

/// Where the three sources live; `None` means "do not read this source".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportPaths {
    /// `~/.claude.json`
    pub claude_json: Option<PathBuf>,
    /// `<cwd>/.mcp.json`
    pub mcp_json: Option<PathBuf>,
    /// `~/.codex/config.toml`
    pub codex_toml: Option<PathBuf>,
}

impl ImportPaths {
    /// The default locations under `home` and `cwd`; a missing one leaves its sources out.
    pub fn discover(home: Option<&Path>, cwd: Option<&Path>) -> Self {
        Self {
            claude_json: home.map(|h| h.join(".claude.json")),
            mcp_json: cwd.map(|c| c.join(".mcp.json")),
            codex_toml: home.map(|h| h.join(".codex").join("config.toml")),
        }
    }
}

/// The user's home directory: `USERPROFILE`, then `HOME`, ignoring empty values.
pub fn user_home() -> Option<PathBuf> {
    ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Why a server named in a source was not imported.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SkipReason {
    /// An http, sse or url server: the registry holds stdio servers only.
    HttpTransport,
    /// An earlier source already named it; `kept_from` is that source.
    Duplicate { kept_from: String },
    /// Not `[A-Za-z0-9_-]+`, or the reserved `nucleos`.
    InvalidName,
    /// A stdio entry without a string `command`.
    MissingCommand,
    /// `args` is not a string array, `env` not a string map, or the entry not an object.
    Malformed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Skipped {
    pub name: String,
    pub source: String,
    #[serde(flatten)]
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ImportReport {
    /// Written to the registry by this import.
    pub imported: Vec<String>,
    pub skipped: Vec<Skipped>,
    /// Named by a source but already in the registry: left as it was.
    pub already_present: Vec<String>,
    /// `"<file>: <error>"` for a source that exists but could not be read or parsed. Never
    /// contains a value from the file.
    pub unread: Vec<String>,
}

/// A server a source names, ready for the registry.
pub struct Candidate {
    pub server: NewMcpServer,
    pub source: &'static str,
}

type Parsed = (Vec<Candidate>, Vec<Skipped>);

fn skipped(name: &str, source: &str, reason: SkipReason) -> Skipped {
    Skipped {
        name: name.to_string(),
        source: source.to_string(),
        reason,
    }
}

fn json_error(error: &serde_json::Error) -> String {
    format!(
        "invalid JSON at line {} column {}",
        error.line(),
        error.column()
    )
}

/// One `mcpServers` entry of a Claude config.
fn json_entry(name: &str, value: &Json, source: &'static str) -> Result<Candidate, SkipReason> {
    if !mcp_servers::valid_name(name) {
        return Err(SkipReason::InvalidName);
    }
    let Some(entry) = value.as_object() else {
        return Err(SkipReason::Malformed);
    };
    let kind = entry.get("type").and_then(Json::as_str);
    if entry.contains_key("url") || matches!(kind, Some("http" | "sse" | "streamable-http")) {
        return Err(SkipReason::HttpTransport);
    }
    if !matches!(kind, None | Some("stdio")) {
        return Err(SkipReason::Malformed);
    }
    let command = match entry.get("command").and_then(Json::as_str) {
        Some(command) if !command.is_empty() => command.to_string(),
        _ => return Err(SkipReason::MissingCommand),
    };
    let args = match entry.get("args") {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or(SkipReason::Malformed)?,
        Some(_) => return Err(SkipReason::Malformed),
    };
    let env = match entry.get("env") {
        None | Some(Json::Null) => BTreeMap::new(),
        Some(Json::Object(vars)) => vars
            .iter()
            .map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
            .collect::<Option<BTreeMap<_, _>>>()
            .ok_or(SkipReason::Malformed)?,
        Some(_) => return Err(SkipReason::Malformed),
    };
    Ok(Candidate {
        server: NewMcpServer {
            name: name.to_string(),
            command,
            args,
            env,
            imported_from: Some(source.to_string()),
        },
        source,
    })
}

/// Every entry of one `mcpServers` object, appended to `out`. `servers` absent is no entry;
/// present but not an object is an error.
fn json_servers(
    servers: Option<&Json>,
    source: &'static str,
    out: &mut Parsed,
) -> Result<(), String> {
    match servers {
        None | Some(Json::Null) => Ok(()),
        Some(Json::Object(map)) => {
            for (name, value) in map {
                match json_entry(name, value, source) {
                    Ok(candidate) => out.0.push(candidate),
                    Err(reason) => out.1.push(skipped(name, source, reason)),
                }
            }
            Ok(())
        }
        Some(_) => Err("`mcpServers` is not an object".to_string()),
    }
}

/// Claude keys a project by its path with forward slashes on Windows (`C:/Projects/x`).
fn normalize_project(path: &str) -> String {
    let forward = path.replace('\\', "/");
    let trimmed = forward.trim_end_matches('/');
    let trimmed = if trimmed.is_empty() { "/" } else { trimmed };
    if cfg!(windows) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// `~/.claude.json`: the top-level `mcpServers` (`claude-user`), then the `mcpServers` of every
/// `projects` key that names `cwd` (`claude-project`).
pub fn parse_claude_json(text: &str, cwd: Option<&Path>) -> Result<Parsed, String> {
    let doc: Json = serde_json::from_str(text).map_err(|e| json_error(&e))?;
    let Some(top) = doc.as_object() else {
        return Err("not a JSON object".to_string());
    };
    let mut out: Parsed = (Vec::new(), Vec::new());
    json_servers(top.get("mcpServers"), SOURCE_CLAUDE_USER, &mut out)?;
    if let (Some(cwd), Some(Json::Object(projects))) = (cwd, top.get("projects")) {
        let wanted = normalize_project(&cwd.to_string_lossy());
        for (key, project) in projects {
            if normalize_project(key) == wanted {
                json_servers(project.get("mcpServers"), SOURCE_CLAUDE_PROJECT, &mut out)?;
            }
        }
    }
    Ok(out)
}

/// `<cwd>/.mcp.json`: its `mcpServers` (`claude-project`).
pub fn parse_mcp_json(text: &str) -> Result<Parsed, String> {
    let doc: Json = serde_json::from_str(text).map_err(|e| json_error(&e))?;
    let Some(top) = doc.as_object() else {
        return Err("not a JSON object".to_string());
    };
    let mut out: Parsed = (Vec::new(), Vec::new());
    json_servers(top.get("mcpServers"), SOURCE_CLAUDE_PROJECT, &mut out)?;
    Ok(out)
}

/// One `[mcp_servers.<name>]` table of a Codex config.
fn toml_entry(name: &str, item: &toml_edit::Item) -> Result<Candidate, SkipReason> {
    if !mcp_servers::valid_name(name) {
        return Err(SkipReason::InvalidName);
    }
    let Some(entry) = item.as_table_like() else {
        return Err(SkipReason::Malformed);
    };
    if entry.get("url").is_some() {
        return Err(SkipReason::HttpTransport);
    }
    let command = match entry.get("command").and_then(toml_edit::Item::as_str) {
        Some(command) if !command.is_empty() => command.to_string(),
        _ => return Err(SkipReason::MissingCommand),
    };
    let args = match entry.get("args") {
        None => Vec::new(),
        Some(item) => item
            .as_array()
            .ok_or(SkipReason::Malformed)?
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or(SkipReason::Malformed)?,
    };
    let env = match entry.get("env") {
        None => BTreeMap::new(),
        // An inline table `env = { .. }` and a `[mcp_servers.<name>.env]` subtable alike.
        Some(item) => item
            .as_table_like()
            .ok_or(SkipReason::Malformed)?
            .iter()
            .map(|(k, v)| v.as_str().map(|v| (k.to_string(), v.to_string())))
            .collect::<Option<BTreeMap<_, _>>>()
            .ok_or(SkipReason::Malformed)?,
    };
    Ok(Candidate {
        server: NewMcpServer {
            name: name.to_string(),
            command,
            args,
            env,
            imported_from: Some(SOURCE_CODEX.to_string()),
        },
        source: SOURCE_CODEX,
    })
}

/// `~/.codex/config.toml`: every `[mcp_servers.<name>]` table (`codex`).
pub fn parse_codex_toml(text: &str) -> Result<Parsed, String> {
    let doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| {
        // `message()` alone: the error's Display quotes the offending line, which can be a value.
        let message = e.message().replace('\n', "; ");
        match e.span() {
            Some(span) => {
                let start = span.start.min(text.len());
                let line = text.as_bytes()[..start]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count()
                    + 1;
                format!("invalid TOML at line {line}: {message}")
            }
            None => format!("invalid TOML: {message}"),
        }
    })?;
    let mut out: Parsed = (Vec::new(), Vec::new());
    let Some(servers) = doc.as_table().get("mcp_servers") else {
        return Ok(out);
    };
    let Some(servers) = servers.as_table_like() else {
        return Err("`mcp_servers` is not a table".to_string());
    };
    for (name, item) in servers.iter() {
        match toml_entry(name, item) {
            Ok(candidate) => out.0.push(candidate),
            Err(reason) => out.1.push(skipped(name, SOURCE_CODEX, reason)),
        }
    }
    Ok(out)
}

/// Keeps the first candidate per name in source order; a later one is `Duplicate { kept_from }`.
pub fn merge(sources: Vec<Parsed>) -> Parsed {
    let mut kept: Vec<Candidate> = Vec::new();
    let mut kept_from: HashMap<String, &'static str> = HashMap::new();
    let mut skips: Vec<Skipped> = Vec::new();
    for (candidates, skipped_here) in sources {
        skips.extend(skipped_here);
        for candidate in candidates {
            if let Some(first) = kept_from.get(&candidate.server.name) {
                skips.push(skipped(
                    &candidate.server.name,
                    candidate.source,
                    SkipReason::Duplicate {
                        kept_from: first.to_string(),
                    },
                ));
            } else {
                kept_from.insert(candidate.server.name.clone(), candidate.source);
                kept.push(candidate);
            }
        }
    }
    (kept, skips)
}

enum Source {
    ClaudeJson,
    McpJson,
    CodexToml,
}

/// Reads every existing source, merges them and adds each server the registry lacks. A missing
/// file is silent; an unreadable or malformed one is an `unread` line. Touches `mcp_servers` only:
/// nothing is enabled in any chat.
pub async fn import(
    pool: &SqlitePool,
    paths: &ImportPaths,
    cwd: Option<&Path>,
) -> sqlx::Result<ImportReport> {
    let mut report = ImportReport::default();
    let mut parsed: Vec<Parsed> = Vec::new();
    let sources = [
        (&paths.claude_json, Source::ClaudeJson),
        (&paths.mcp_json, Source::McpJson),
        (&paths.codex_toml, Source::CodexToml),
    ];
    for (path, source) in sources {
        let Some(path) = path else { continue };
        let text = match tokio::fs::read_to_string(path).await {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                report.unread.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        let result = match source {
            Source::ClaudeJson => parse_claude_json(&text, cwd),
            Source::McpJson => parse_mcp_json(&text),
            Source::CodexToml => parse_codex_toml(&text),
        };
        match result {
            Ok(one) => parsed.push(one),
            Err(error) => report.unread.push(format!("{}: {error}", path.display())),
        }
    }
    let (candidates, skips) = merge(parsed);
    report.skipped = skips;
    for candidate in candidates {
        let name = candidate.server.name.clone();
        if mcp_servers::insert_if_absent(pool, &candidate.server).await? {
            report.imported.push(name);
        } else {
            report.already_present.push(name);
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chats::{self, Brain};

    const SECRET: &str = "sk-SECRET-7f3a";

    fn names(candidates: &[Candidate]) -> Vec<&str> {
        candidates.iter().map(|c| c.server.name.as_str()).collect()
    }

    fn reason_of<'a>(skips: &'a [Skipped], name: &str) -> &'a SkipReason {
        &skips
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} was not skipped"))
            .reason
    }

    fn project_cwd() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\x\y" } else { "C:/x/y" })
    }

    #[test]
    fn claude_json_top_level_and_projects_cwd() {
        let text = r#"{
            "mcpServers": {
                "gh": {"type": "stdio", "command": "gh-mcp", "args": ["--stdio"],
                       "env": {"GH_TOKEN": "t"}}
            },
            "projects": {
                "C:/x/y": {"mcpServers": {"db": {"command": "db-mcp"}}},
                "C:/other": {"mcpServers": {"elsewhere": {"command": "e"}}}
            }
        }"#;
        let (candidates, skips) = parse_claude_json(text, Some(&project_cwd())).unwrap();
        assert!(skips.is_empty());
        assert_eq!(names(&candidates), vec!["gh", "db"]);
        let gh = &candidates[0];
        assert_eq!(gh.source, SOURCE_CLAUDE_USER);
        assert_eq!(gh.server.command, "gh-mcp");
        assert_eq!(gh.server.args, vec!["--stdio".to_string()]);
        assert_eq!(gh.server.env.get("GH_TOKEN").map(String::as_str), Some("t"));
        assert_eq!(gh.server.imported_from.as_deref(), Some(SOURCE_CLAUDE_USER));
        assert_eq!(candidates[1].source, SOURCE_CLAUDE_PROJECT);
        assert!(candidates[1].server.args.is_empty());

        // Without a cwd only the top level is read.
        let (top_only, _) = parse_claude_json(text, None).unwrap();
        assert_eq!(names(&top_only), vec!["gh"]);

        if cfg!(windows) {
            // Case and a trailing separator do not matter on Windows.
            let (cased, _) = parse_claude_json(text, Some(Path::new(r"c:\X\y\"))).unwrap();
            assert_eq!(names(&cased), vec!["gh", "db"]);
        } else {
            let (other, _) = parse_claude_json(text, Some(Path::new("c:/X/y"))).unwrap();
            assert_eq!(names(&other), vec!["gh"]);
        }
    }

    #[test]
    fn claude_json_http_entry_is_skipped() {
        let text = r#"{"mcpServers": {
            "remote": {"type": "http", "url": "https://example.com/mcp"},
            "events": {"type": "sse", "url": "https://example.com/sse"},
            "bare": {"url": "https://example.com/x"},
            "local": {"command": "local-mcp"}
        }}"#;
        let (candidates, skips) = parse_claude_json(text, None).unwrap();
        assert_eq!(names(&candidates), vec!["local"]);
        for name in ["remote", "events", "bare"] {
            assert_eq!(reason_of(&skips, name), &SkipReason::HttpTransport);
        }
        assert!(skips.iter().all(|s| s.source == SOURCE_CLAUDE_USER));
    }

    #[test]
    fn mcp_json_reads_mcp_servers() {
        let text = r#"{"mcpServers": {
            "fs": {"command": "npx", "args": ["-y", "fs-mcp", "."], "env": {"ROOT": "."}},
            "nocmd": {"args": ["x"]},
            "badargs": {"command": "x", "args": ["ok", 3]}
        }}"#;
        let (candidates, skips) = parse_mcp_json(text).unwrap();
        assert_eq!(names(&candidates), vec!["fs"]);
        assert_eq!(candidates[0].source, SOURCE_CLAUDE_PROJECT);
        assert_eq!(candidates[0].server.args, vec!["-y", "fs-mcp", "."]);
        assert_eq!(reason_of(&skips, "nocmd"), &SkipReason::MissingCommand);
        assert_eq!(reason_of(&skips, "badargs"), &SkipReason::Malformed);
        // No `mcpServers` at all is an empty source, not an error.
        let (none, none_skipped) = parse_mcp_json("{}").unwrap();
        assert!(none.is_empty() && none_skipped.is_empty());
    }

    #[test]
    fn codex_toml_reads_tables_inline_and_sub_env() {
        let text = r#"
model = "gpt-5"

[mcp_servers.inline]
command = "inline-mcp"
args = ["--a", "b"]
env = { API_KEY = "k1", REGION = "eu" }

[mcp_servers.sub]
command = "sub-mcp"

[mcp_servers.sub.env]
TOKEN = "k2"

[mcp_servers.remote]
url = "https://example.com/mcp"

[mcp_servers.nocmd]
args = ["x"]
"#;
        let (candidates, skips) = parse_codex_toml(text).unwrap();
        assert_eq!(names(&candidates), vec!["inline", "sub"]);
        let inline = &candidates[0].server;
        assert_eq!(inline.args, vec!["--a", "b"]);
        assert_eq!(inline.env.len(), 2);
        assert_eq!(inline.env.get("API_KEY").map(String::as_str), Some("k1"));
        assert_eq!(inline.imported_from.as_deref(), Some(SOURCE_CODEX));
        let sub = &candidates[1].server;
        assert_eq!(sub.env.get("TOKEN").map(String::as_str), Some("k2"));
        assert!(sub.args.is_empty());
        assert_eq!(reason_of(&skips, "remote"), &SkipReason::HttpTransport);
        assert_eq!(reason_of(&skips, "nocmd"), &SkipReason::MissingCommand);
        assert!(skips.iter().all(|s| s.source == SOURCE_CODEX));
        // A config without servers is an empty source.
        let (none, _) = parse_codex_toml("model = \"x\"\n").unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn malformed_json_and_toml_are_errors_without_values() {
        let json_inputs = [
            format!(r#"{{"mcpServers": {{"x": {{"command": "c", "env": {{"TOKEN": "{SECRET}"}}"#),
            format!(r#"{{"mcpServers": {{"x": {{"env": {{"TOKEN": "{SECRET}" "#),
            format!(r#"{{"mcpServers": {{"x": {{"env": {{"TOKEN": {SECRET}}}}}}}}}"#),
        ];
        for text in &json_inputs {
            let claude = parse_claude_json(text, None)
                .err()
                .expect("claude.json must fail");
            assert!(!claude.contains(SECRET), "a JSON error quoted the input");
            let mcp = parse_mcp_json(text).err().expect(".mcp.json must fail");
            assert!(!mcp.contains(SECRET), "a JSON error quoted the input");
        }
        let toml_inputs = [
            format!("[mcp_servers.x]\ncommand = \"c\"\nenv = {{ TOKEN = \"{SECRET}\" \n"),
            format!("[mcp_servers.x]\nTOKEN = {SECRET}\n"),
            format!("[mcp_servers.x]\nTOKEN = \"{SECRET}\"\nTOKEN = \"{SECRET}\"\n"),
            format!("[mcp_servers.x\nTOKEN = \"{SECRET}\"\n"),
        ];
        for text in &toml_inputs {
            let error = parse_codex_toml(text).err().expect("config.toml must fail");
            assert!(!error.contains(SECRET), "a TOML error quoted the input");
        }
        // Wrong shapes are errors too.
        assert!(parse_claude_json("[]", None).is_err());
        assert!(parse_mcp_json(r#"{"mcpServers": []}"#).is_err());
        assert!(parse_codex_toml("mcp_servers = 3\n").is_err());
    }

    #[test]
    fn merge_first_source_wins_and_lists_duplicates() {
        let claude = parse_claude_json(
            r#"{"mcpServers": {"gh": {"command": "from-claude"}},
                "projects": {"C:/x/y": {"mcpServers": {"gh": {"command": "from-project"}}}}}"#,
            Some(&project_cwd()),
        )
        .unwrap();
        let mcp_json = parse_mcp_json(r#"{"mcpServers": {"db": {"command": "db-json"}}}"#).unwrap();
        let codex = parse_codex_toml(
            "[mcp_servers.gh]\ncommand = \"from-codex\"\n[mcp_servers.db]\ncommand = \"db-codex\"\n\
             [mcp_servers.only]\ncommand = \"o\"\n",
        )
        .unwrap();
        let (kept, skips) = merge(vec![claude, mcp_json, codex]);
        assert_eq!(names(&kept), vec!["gh", "db", "only"]);
        assert_eq!(kept[0].server.command, "from-claude");
        assert_eq!(kept[1].server.command, "db-json");
        let duplicates: Vec<(&str, &str, &SkipReason)> = skips
            .iter()
            .map(|s| (s.name.as_str(), s.source.as_str(), &s.reason))
            .collect();
        let dup = |kept_from: &str| SkipReason::Duplicate {
            kept_from: kept_from.to_string(),
        };
        assert_eq!(
            duplicates,
            vec![
                ("gh", SOURCE_CLAUDE_PROJECT, &dup(SOURCE_CLAUDE_USER)),
                ("gh", SOURCE_CODEX, &dup(SOURCE_CLAUDE_USER)),
                ("db", SOURCE_CODEX, &dup(SOURCE_CLAUDE_PROJECT)),
            ]
        );
    }

    #[test]
    fn invalid_and_reserved_names_are_skipped() {
        let (candidates, skips) = parse_claude_json(
            r#"{"mcpServers": {
                "nucleos": {"command": "x"},
                "NucleOS": {"command": "x"},
                "a b": {"command": "x"},
                "a/b": {"command": "x"},
                "gh_": {"command": "x"},
                "a__b": {"command": "x"},
                "ok_1-2": {"command": "x"}
            }}"#,
            None,
        )
        .unwrap();
        assert_eq!(names(&candidates), vec!["ok_1-2"]);
        for name in ["nucleos", "NucleOS", "a b", "a/b", "gh_", "a__b"] {
            assert_eq!(reason_of(&skips, name), &SkipReason::InvalidName);
        }
        let (codex, codex_skips) = parse_codex_toml(
            "[mcp_servers.nucleos]\ncommand = \"x\"\n[mcp_servers.\"a.b\"]\ncommand = \"x\"\n",
        )
        .unwrap();
        assert!(codex.is_empty());
        assert_eq!(reason_of(&codex_skips, "nucleos"), &SkipReason::InvalidName);
        assert_eq!(reason_of(&codex_skips, "a.b"), &SkipReason::InvalidName);

        let (codex, codex_skips) = parse_codex_toml(
            "[mcp_servers.gh_]\ncommand = \"x\"\n[mcp_servers.a__b]\ncommand = \"x\"\n",
        )
        .unwrap();
        assert!(codex.is_empty());
        assert_eq!(reason_of(&codex_skips, "gh_"), &SkipReason::InvalidName);
        assert_eq!(reason_of(&codex_skips, "a__b"), &SkipReason::InvalidName);
    }

    #[tokio::test]
    async fn import_upserts_without_overwriting_and_enables_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let _chat = chats::create(&pool, Brain::Cloud, None).await.unwrap();
        // A temp home and project: the real ~/.claude.json and ~/.codex are never read.
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        std::fs::write(
            home.path().join(".claude.json"),
            r#"{"mcpServers": {
                "gh": {"command": "gh-new"},
                "fresh": {"command": "fresh-mcp", "env": {"TOKEN": "v"}},
                "remote": {"type": "http", "url": "https://example.com"}
            }}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        std::fs::write(
            home.path().join(".codex").join("config.toml"),
            format!("[mcp_servers.x\nTOKEN = \"{SECRET}\"\n"),
        )
        .unwrap();
        // `<project>/.mcp.json` does not exist.
        assert!(
            mcp_servers::insert_if_absent(
                &pool,
                &NewMcpServer {
                    name: "gh".to_string(),
                    command: "gh-mine".to_string(),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    imported_from: None,
                },
            )
            .await
            .unwrap()
        );

        let paths = ImportPaths::discover(Some(home.path()), Some(project.path()));
        assert_eq!(paths.mcp_json, Some(project.path().join(".mcp.json")));
        let report = import(&pool, &paths, Some(project.path())).await.unwrap();

        assert_eq!(report.imported, vec!["fresh".to_string()]);
        assert_eq!(report.already_present, vec!["gh".to_string()]);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].reason, SkipReason::HttpTransport);
        assert_eq!(
            report.unread.len(),
            1,
            "only the malformed codex file: {:?}",
            report.unread
        );
        assert!(report.unread[0].contains("config.toml"));
        assert!(!report.unread[0].contains(SECRET));

        let kept = mcp_servers::get(&pool, "gh").await.unwrap().unwrap();
        assert_eq!(kept.command, "gh-mine");
        assert_eq!(kept.imported_from, None);
        let fresh = mcp_servers::get(&pool, "fresh").await.unwrap().unwrap();
        assert_eq!(fresh.imported_from.as_deref(), Some(SOURCE_CLAUDE_USER));
        assert_eq!(fresh.env.get("TOKEN").map(String::as_str), Some("v"));

        let toggles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_mcp_servers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(toggles, 0);

        // A second import changes nothing.
        let again = import(&pool, &paths, Some(project.path())).await.unwrap();
        assert!(again.imported.is_empty());
        let mut present = again.already_present.clone();
        present.sort();
        assert_eq!(present, vec!["fresh".to_string(), "gh".to_string()]);
    }
}
