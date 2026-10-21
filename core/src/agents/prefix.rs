//! The Codex prefix of an agent package, generated to a file per agent (spec 2026-10-20 section 6,
//! plan C2).
//!
//! An interactive `codex exec` node has no daemon behind it, so the pieces the daemon would compute
//! at launch are computed ahead of time and written to `<out_dir>/<name>.json`: the stable prompt
//! prefix (one assembler, [`build_prefix`], with the project context as file REFERENCES so the file
//! stays valid while `memory.md` changes), the sandbox and the web-search switch
//! ([`translate`] for [`Engine::Codex`], never a second table), and the capabilities Codex cannot
//! enforce.
//!
//! # Ceiling limitation
//!
//! On Codex, capabilities are a WRITE CEILING, not an exact tool list. `read-only` still allows
//! shell reads, and `workspace-write` gives write and shell together. `memory.read` and
//! `mcp.nucleos` need the daemon's MCP server, which an interactive node does not have: they are
//! listed in `dropped` and do not change the sandbox. The generated file says so in `ceiling_note`.
//!
//! # Files are local
//!
//! The output directory is a parameter; the CLI passes `<state_dir>/agent-prefix` ([`prefix_dir_for`]),
//! a local place outside the repository. A file is
//! rewritten only when its `content_sha256` changed, and a file this module generated for an agent
//! that is no longer installed is removed. Files without our `generated_by` marker are left alone.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use nucleos_base::agent::Capability;
use sha2::{Digest, Sha256};

use super::capabilities::{CodexSandbox, Engine, Unsupported, translate};
use super::package::{Package, highest_version, load_installed};
use super::prompt::{ContextRefs, ContextRender, build_prefix};

/// The value of `generated_by` in every file this module writes.
pub const GENERATED_BY: &str = "nucleos agents-sync";

/// What the generated file tells a reader about the limits of the sandbox.
pub const CEILING_NOTE: &str = "capabilities are a write ceiling on Codex, not an exact tool \
list: read-only still allows shell reads, workspace-write gives write and shell together; \
memory.read and mcp.nucleos need the daemon MCP server and are not enforced here";

/// The generated prefix of one agent at one version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexPrefix {
    pub name: String,
    pub version: String,
    pub description: String,
    /// The value `sandbox_mode` takes: `read-only` or `workspace-write`.
    pub sandbox: &'static str,
    pub web_search: bool,
    /// Capabilities the package declares that Codex cannot enforce.
    pub dropped: Vec<&'static str>,
    pub prefix: String,
    pub content_sha256: String,
}

/// The prefix of `pkg` for Codex. Pure: it reads the package's knowledge files and nothing else.
pub fn codex_prefix(pkg: &Package, refs: &ContextRefs) -> Result<CodexPrefix, Unsupported> {
    let tools = translate(Engine::Codex, &pkg.capabilities)?;
    let sandbox = tools
        .codex_sandbox
        .unwrap_or(CodexSandbox::ReadOnly)
        .as_str();
    let dropped: Vec<&'static str> = [Capability::MemoryRead, Capability::McpNucleos]
        .into_iter()
        .filter(|cap| pkg.capabilities.contains(cap))
        .map(Capability::as_str)
        .collect();
    let prefix = build_prefix(pkg, &ContextRender::Refs(refs.clone()), &pkg.output_md);
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    hasher.update([0u8]);
    hasher.update(sandbox.as_bytes());
    hasher.update([0u8]);
    hasher.update([u8::from(tools.codex_web_search)]);
    Ok(CodexPrefix {
        name: pkg.reference.name.clone(),
        version: pkg.reference.version.clone(),
        description: pkg.description.clone(),
        sandbox,
        web_search: tools.codex_web_search,
        dropped,
        prefix,
        content_sha256: format!("{:x}", hasher.finalize()),
    })
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("a string always serializes")
}

/// The file content: schema 1, keys in a fixed order, a trailing newline.
pub fn render_json(p: &CodexPrefix) -> String {
    let dropped = p
        .dropped
        .iter()
        .map(|cap| quoted(cap))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\n  \"schema\": 1,\n  \"generated_by\": {},\n  \"name\": {},\n  \"version\": {},\n  \
         \"description\": {},\n  \"sandbox\": {},\n  \"web_search\": {},\n  \"dropped\": [{}],\n  \
         \"ceiling_note\": {},\n  \"prefix\": {},\n  \"content_sha256\": {}\n}}\n",
        quoted(GENERATED_BY),
        quoted(&p.name),
        quoted(&p.version),
        quoted(&p.description),
        quoted(p.sandbox),
        p.web_search,
        dropped,
        quoted(CEILING_NOTE),
        quoted(&p.prefix),
        quoted(&p.content_sha256),
    )
}

/// The project files a prefix may point at, only those that exist: instructions are `AGENTS.md`,
/// else `CLAUDE.md`; memory and decisions are `<state_dir>/memory.md` and `<state_dir>/decisions.md`
/// when the workflow pointer names a state directory, else the legacy `<root>/.ai/` copies.
pub fn context_refs_for(project_root: &Path) -> ContextRefs {
    let existing = |path: PathBuf| path.is_file().then_some(path);
    let state = |file: &str| {
        state_dir_for(project_root)
            .and_then(|dir| existing(dir.join(file)))
            .or_else(|| existing(project_root.join(".ai").join(file)))
    };
    ContextRefs {
        instructions: existing(project_root.join("AGENTS.md"))
            .or_else(|| existing(project_root.join("CLAUDE.md"))),
        memory: state("memory.md"),
        decisions: state("decisions.md"),
    }
}

/// What one sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Agents whose file was created or rewritten.
    pub written: Vec<String>,
    /// Agents whose file already had the same hash.
    pub unchanged: Vec<String>,
    /// Agents whose file was removed because the agent is no longer installed.
    pub removed: Vec<String>,
    /// `(agent, message)` for each agent that could not be processed.
    pub errors: Vec<(String, String)>,
}

/// The project root a workflow project pointer governs: the nearest ancestor of `start` (itself
/// included) that holds `.nucleos/workflow/project.json`.
pub fn project_root_from(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".nucleos/workflow/project.json").is_file())
        .map(Path::to_path_buf)
}

/// `<state_dir>/agent-prefix` for the project at `project_root`, with `state_dir` read from the
/// `state_dir` key of `<project_root>/.nucleos/workflow/project.json`. `None` when the pointer is
/// missing, unreadable or names no state directory.
pub fn prefix_dir_for(project_root: &Path) -> Option<PathBuf> {
    state_dir_for(project_root).map(|dir| dir.join("agent-prefix"))
}

/// The `state_dir` key of `<project_root>/.nucleos/workflow/project.json`, if any.
fn state_dir_for(project_root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(project_root.join(".nucleos/workflow/project.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let state_dir = value.get("state_dir")?.as_str().filter(|s| !s.is_empty())?;
    Some(PathBuf::from(state_dir))
}

/// Sync the prefixes of the project at `project_root` into its `<state_dir>/agent-prefix`.
/// `None` when the project has no workflow pointer, so there is nowhere to write.
pub fn sync_for_project(agents_home: &Path, project_root: &Path) -> Option<(PathBuf, SyncReport)> {
    let out = prefix_dir_for(project_root)?;
    let report = sync_codex_prefixes(agents_home, project_root, &out);
    Some((out, report))
}

/// The `(generated_by, name, content_sha256)` of an existing prefix file, if it parses.
fn read_existing(path: &Path) -> Option<(String, String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let field = |key: &str| value.get(key)?.as_str().map(str::to_owned);
    Some((
        field("generated_by")?,
        field("name").unwrap_or_default(),
        field("content_sha256").unwrap_or_default(),
    ))
}

/// Generate `<out_dir>/<name>.json` for the highest installed version of every agent under
/// `agents_home`. Per-agent failures are collected and the others still written.
pub fn sync_codex_prefixes(agents_home: &Path, project_root: &Path, out_dir: &Path) -> SyncReport {
    let mut report = SyncReport::default();
    let refs = context_refs_for(project_root);
    let mut names = BTreeSet::new();
    match std::fs::read_dir(agents_home) {
        Ok(entries) => {
            for entry in entries.flatten() {
                // `install.rs` stages under `<home>/.staging`; a dot directory is never a package.
                if entry.path().is_dir()
                    && let Ok(name) = entry.file_name().into_string()
                    && !name.starts_with('.')
                {
                    names.insert(name);
                }
            }
        }
        // No agents installed at all: sweeping the generated prefixes is correct.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Unreadable is not empty: skip the sweep, or every generated prefix would be deleted.
        Err(e) => {
            report.errors.push((
                "<agents home>".into(),
                format!("{}: {e}", agents_home.display()),
            ));
            return report;
        }
    }

    for name in &names {
        let mut fail = |message: String| report.errors.push((name.clone(), message));
        let Some(reference) = highest_version(agents_home, name) else {
            fail("no installed version".into());
            continue;
        };
        let pkg = match load_installed(agents_home, &reference) {
            Ok(pkg) => pkg,
            Err(error) => {
                fail(error.to_string());
                continue;
            }
        };
        let generated = match codex_prefix(&pkg, &refs) {
            Ok(generated) => generated,
            Err(error) => {
                fail(error.to_string());
                continue;
            }
        };
        let path = out_dir.join(format!("{name}.json"));
        if read_existing(&path)
            .is_some_and(|(by, _, hash)| by == GENERATED_BY && hash == generated.content_sha256)
        {
            report.unchanged.push(name.clone());
            continue;
        }
        let result = std::fs::create_dir_all(out_dir)
            .and_then(|()| std::fs::write(&path, render_json(&generated)));
        match result {
            Ok(()) => report.written.push(name.clone()),
            Err(error) => fail(format!("{}: {error}", path.display())),
        }
    }

    if let Ok(entries) = std::fs::read_dir(out_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some((by, _, _)) = read_existing(&path) else {
                continue;
            };
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            if by == GENERATED_BY && !names.contains(&stem) && std::fs::remove_file(&path).is_ok() {
                report.removed.push(stem);
            }
        }
    }
    report.removed.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::package::PackageRef;
    use crate::agents::prompt::{ProjectContext, build_prompt};

    /// Write `<home>/<name>/<version>/` with the given capabilities, context sources and body.
    fn write_package(
        home: &Path,
        name: &str,
        version: &str,
        caps: &str,
        context: &str,
        body: &str,
    ) {
        let dir = home.join(name).join(version);
        std::fs::create_dir_all(dir.join("knowledge/ref")).unwrap();
        let agent = format!(
            "---\nname: {name}\nversion: {version}\ndescription: Desc of {name}.\n\
             capabilities: [{caps}]\ncontext: [{context}]\n\
             knowledge:\n  inject: [knowledge/inject.md]\n  on_demand: [knowledge/ref/*.md]\n---\n{body}\n"
        );
        std::fs::write(dir.join("agent.md"), agent).unwrap();
        std::fs::write(dir.join("output.md"), "OUTPUT-SHAPE\n").unwrap();
        std::fs::write(dir.join("knowledge/inject.md"), "INJECT-TEXT\n").unwrap();
        std::fs::write(dir.join("knowledge/ref/a.md"), "REF-HEAD\nmore\n").unwrap();
    }

    fn load(home: &Path, name: &str, version: &str) -> Package {
        load_installed(
            home,
            &PackageRef {
                name: name.into(),
                version: version.into(),
            },
        )
        .unwrap()
    }

    /// A package named `x` with `caps` and no context, built in a fresh temp home.
    fn prefix_for(caps: &str) -> CodexPrefix {
        let home = tempfile::tempdir().unwrap();
        // `on_demand` needs fs.read, so the helper always declares it; tests that care about
        // the sandbox add the capability under test next to it.
        write_package(home.path(), "x", "1.0.0", caps, "", "BODY");
        let pkg = load(home.path(), "x", "1.0.0");
        codex_prefix(&pkg, &ContextRefs::default()).unwrap()
    }

    #[test]
    fn sandbox_read_only_for_fs_read_only() {
        let p = prefix_for("fs.read");
        assert_eq!(p.sandbox, "read-only");
        assert!(!p.web_search);
    }

    #[test]
    fn sandbox_read_only_for_empty_and_mcp_caps() {
        for caps in [
            "fs.read, mcp.nucleos",
            "fs.read, memory.read",
            "fs.read, web",
        ] {
            assert_eq!(prefix_for(caps).sandbox, "read-only", "{caps}");
        }
        // No fs/shell/web capability at all: translate leaves the sandbox open, we pin read-only.
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("e/1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.md"),
            "---\nname: e\nversion: 1.0.0\ndescription: d\ncapabilities: []\n---\nB\n",
        )
        .unwrap();
        std::fs::write(dir.join("output.md"), "O\n").unwrap();
        let pkg = load(home.path(), "e", "1.0.0");
        let p = codex_prefix(&pkg, &ContextRefs::default()).unwrap();
        assert_eq!(p.sandbox, "read-only");
        let p = prefix_for("fs.read, mcp.nucleos");
        assert_eq!(p.sandbox, "read-only");
    }

    #[test]
    fn sandbox_workspace_write_for_shell() {
        assert_eq!(prefix_for("fs.read, shell").sandbox, "workspace-write");
    }

    #[test]
    fn sandbox_workspace_write_for_fs_write() {
        assert_eq!(prefix_for("fs.read, fs.write").sandbox, "workspace-write");
    }

    #[test]
    fn web_capability_enables_web_search_only_when_declared() {
        assert!(prefix_for("fs.read, web").web_search);
        assert!(!prefix_for("fs.read, fs.write").web_search);
    }

    #[test]
    fn memory_and_mcp_caps_listed_in_dropped_not_in_sandbox() {
        let p = prefix_for("fs.read, memory.read, mcp.nucleos");
        assert_eq!(p.dropped, vec!["memory.read", "mcp.nucleos"]);
        assert_eq!(p.sandbox, "read-only");
        assert!(prefix_for("fs.read, fs.write").dropped.is_empty());
    }

    #[test]
    fn prefix_has_layers_in_spec_order() {
        let home = tempfile::tempdir().unwrap();
        write_package(
            home.path(),
            "x",
            "1.0.0",
            "fs.read",
            "project.instructions, project.memory, project.decisions",
            "BODY-TEXT",
        );
        let pkg = load(home.path(), "x", "1.0.0");
        let refs = ContextRefs {
            instructions: Some(PathBuf::from("/p/AGENTS.md")),
            memory: Some(PathBuf::from("/p/.ai/memory.md")),
            decisions: Some(PathBuf::from("/p/.ai/decisions.md")),
        };
        let p = codex_prefix(&pkg, &refs).unwrap();
        let installed = pkg.root.join("knowledge/ref/a.md").display().to_string();
        let order: Vec<usize> = [
            "BODY-TEXT",
            "INJECT-TEXT",
            installed.as_str(),
            "/p/AGENTS.md",
            "/p/.ai/memory.md",
            "/p/.ai/decisions.md",
            "OUTPUT-SHAPE",
        ]
        .iter()
        .map(|needle| {
            p.prefix
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing in {}", p.prefix))
        })
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    }

    #[test]
    fn prefix_omits_loadout_and_task_layers() {
        let p = prefix_for("fs.read");
        assert!(!p.prefix.contains("# Task"));
        assert!(!p.prefix.to_lowercase().contains("loadout"));
        assert!(p.prefix.ends_with("OUTPUT-SHAPE"));
    }

    #[test]
    fn context_is_refs_not_inlined() {
        let home = tempfile::tempdir().unwrap();
        write_package(
            home.path(),
            "x",
            "1.0.0",
            "fs.read",
            "project.memory",
            "BODY",
        );
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".ai")).unwrap();
        let big = format!("MEMORY-MARKER {}", "m".repeat(50_000));
        std::fs::write(project.path().join(".ai/memory.md"), &big).unwrap();
        let refs = context_refs_for(project.path());
        let pkg = load(home.path(), "x", "1.0.0");
        let p = codex_prefix(&pkg, &refs).unwrap();
        assert!(!p.prefix.contains("MEMORY-MARKER"));
        assert!(p.prefix.len() < 5_000);
        assert!(
            p.prefix.contains(
                &project
                    .path()
                    .join(".ai")
                    .join("memory.md")
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn context_refs_only_for_declared_sources() {
        let home = tempfile::tempdir().unwrap();
        write_package(
            home.path(),
            "x",
            "1.0.0",
            "fs.read",
            "project.memory",
            "BODY",
        );
        let refs = ContextRefs {
            instructions: Some(PathBuf::from("/p/AGENTS.md")),
            memory: Some(PathBuf::from("/p/.ai/memory.md")),
            decisions: Some(PathBuf::from("/p/.ai/decisions.md")),
        };
        let p = codex_prefix(&load(home.path(), "x", "1.0.0"), &refs).unwrap();
        assert!(p.prefix.contains("/p/.ai/memory.md"));
        assert!(!p.prefix.contains("decisions.md"));
        assert!(!p.prefix.contains("AGENTS.md"));
    }

    #[test]
    fn prefix_matches_build_prompt_prefix() {
        // One assembler: with no context declared, the prefix is exactly what `build_prompt`
        // assembles when its per-run layers (loadout, task) are empty.
        let home = tempfile::tempdir().unwrap();
        write_package(home.path(), "x", "1.0.0", "fs.read", "", "BODY");
        let pkg = load(home.path(), "x", "1.0.0");
        let p = codex_prefix(&pkg, &ContextRefs::default()).unwrap();
        let direct = build_prefix(
            &pkg,
            &ContextRender::Refs(ContextRefs::default()),
            &pkg.output_md,
        );
        assert_eq!(p.prefix, direct);
        let prompt = build_prompt(&pkg, &ProjectContext::default(), "", &pkg.output_md, "");
        assert_eq!(p.prefix, prompt);
        assert!(p.prefix.contains("BODY") && p.prefix.contains("INJECT-TEXT"));
    }

    #[test]
    fn content_sha256_changes_with_sandbox_and_body() {
        let base = prefix_for("fs.read");
        // The prefix embeds the install path of the on-demand files, so a fresh temp home gives
        // another hash; stability is only promised for the same installed package.
        let same_home = tempfile::tempdir().unwrap();
        write_package(same_home.path(), "x", "1.0.0", "fs.read", "", "BODY");
        let first = codex_prefix(
            &load(same_home.path(), "x", "1.0.0"),
            &ContextRefs::default(),
        )
        .unwrap();
        let second = codex_prefix(
            &load(same_home.path(), "x", "1.0.0"),
            &ContextRefs::default(),
        )
        .unwrap();
        assert_eq!(first.content_sha256, second.content_sha256);
        assert_ne!(
            base.content_sha256,
            prefix_for("fs.read, fs.write").content_sha256
        );
        assert_ne!(
            base.content_sha256,
            prefix_for("fs.read, web").content_sha256
        );
        let home = tempfile::tempdir().unwrap();
        write_package(home.path(), "x", "1.0.0", "fs.read", "", "OTHER BODY");
        let other =
            codex_prefix(&load(home.path(), "x", "1.0.0"), &ContextRefs::default()).unwrap();
        assert_ne!(base.content_sha256, other.content_sha256);
        assert_eq!(base.content_sha256.len(), 64);
    }

    #[test]
    fn render_json_is_stable_and_round_trips() {
        let mut p = prefix_for("fs.read, memory.read");
        p.prefix = "line \"one\"\nC:\\path\\x\ttab".into();
        let text = render_json(&p);
        assert_eq!(text, render_json(&p));
        assert!(text.ends_with("}\n"));
        let keys = [
            "schema",
            "generated_by",
            "name",
            "version",
            "description",
            "sandbox",
            "web_search",
            "dropped",
            "ceiling_note",
            "prefix",
            "content_sha256",
        ];
        let at: Vec<usize> = keys
            .iter()
            .map(|key| text.find(&format!("\"{key}\":")).unwrap())
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["schema"], 1);
        assert_eq!(value["generated_by"], GENERATED_BY);
        assert_eq!(value["name"], "x");
        assert_eq!(value["sandbox"], "read-only");
        assert_eq!(value["web_search"], false);
        assert_eq!(value["dropped"], serde_json::json!(["memory.read"]));
        assert_eq!(value["prefix"], p.prefix.as_str());
        assert_eq!(value["content_sha256"], p.content_sha256.as_str());
        assert_eq!(value["ceiling_note"], CEILING_NOTE);
    }

    #[test]
    fn context_refs_prefers_agents_md_over_claude_md() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("CLAUDE.md"), "c").unwrap();
        let refs = context_refs_for(project.path());
        assert_eq!(refs.instructions, Some(project.path().join("CLAUDE.md")));
        assert_eq!(refs.memory, None);
        assert_eq!(refs.decisions, None);
        std::fs::write(project.path().join("AGENTS.md"), "a").unwrap();
        let refs = context_refs_for(project.path());
        assert_eq!(refs.instructions, Some(project.path().join("AGENTS.md")));
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn write_pointer(project: &Path, state: &Path) {
        let dir = project.join(".nucleos/workflow");
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({ "state_dir": state.to_string_lossy() });
        std::fs::write(dir.join("project.json"), body.to_string()).unwrap();
    }

    #[test]
    fn prefix_dir_is_agent_prefix_under_the_pointer_state_dir() {
        let project = tempfile::tempdir().unwrap();
        assert_eq!(prefix_dir_for(project.path()), None);
        let state = project.path().join("st");
        write_pointer(project.path(), &state);
        assert_eq!(
            prefix_dir_for(project.path()),
            Some(state.join("agent-prefix"))
        );
        let nested = project.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(
            project_root_from(&nested),
            Some(project.path().to_path_buf())
        );
    }

    #[test]
    fn sync_for_project_writes_into_the_state_dir() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let state = project.path().join("st");
        write_pointer(project.path(), &state);
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "A");
        let (out, report) = sync_for_project(home.path(), project.path()).unwrap();
        assert_eq!(out, state.join("agent-prefix"));
        assert_eq!(report.written, vec!["alpha"]);
        assert!(out.join("alpha.json").is_file());
    }

    #[test]
    fn memory_and_decisions_come_from_the_state_dir_before_the_legacy_copy() {
        let project = tempfile::tempdir().unwrap();
        let state = project.path().join("st");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(project.path().join(".ai")).unwrap();
        write_pointer(project.path(), &state);
        std::fs::write(state.join("memory.md"), "m").unwrap();
        std::fs::write(project.path().join(".ai").join("memory.md"), "old").unwrap();
        std::fs::write(project.path().join(".ai").join("decisions.md"), "old").unwrap();
        let refs = context_refs_for(project.path());
        assert_eq!(refs.memory, Some(state.join("memory.md")));
        // No state copy: the legacy file is still pointed at.
        assert_eq!(
            refs.decisions,
            Some(project.path().join(".ai").join("decisions.md"))
        );
    }

    #[test]
    fn sync_for_project_without_a_pointer_does_nothing() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "A");
        assert!(sync_for_project(home.path(), project.path()).is_none());
    }

    #[test]
    fn sync_writes_one_file_per_agent_at_highest_version() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let out = project.path().join("state/agent-prefix");
        write_package(home.path(), "alpha", "1.9.0", "fs.read", "", "OLD");
        write_package(home.path(), "alpha", "1.10.0", "fs.read", "", "NEW");
        write_package(home.path(), "beta", "2.0.0", "fs.read, shell", "", "B");
        let report = sync_codex_prefixes(home.path(), project.path(), &out);
        assert_eq!(report.written, vec!["alpha", "beta"]);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let alpha = read_json(&out.join("alpha.json"));
        assert_eq!(alpha["version"], "1.10.0");
        assert!(alpha["prefix"].as_str().unwrap().contains("NEW"));
        assert_eq!(
            read_json(&out.join("beta.json"))["sandbox"],
            "workspace-write"
        );
    }

    #[test]
    fn the_installers_staging_directory_is_not_a_package() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let out = project.path().join("out");
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "A");
        std::fs::create_dir_all(home.path().join(".staging/alpha/1.0.0")).unwrap();
        let report = sync_codex_prefixes(home.path(), project.path(), &out);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.written, vec!["alpha"]);
    }

    #[test]
    fn unreadable_agents_home_keeps_generated_prefixes() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let out = project.path().join("out");
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "B");
        sync_codex_prefixes(home.path(), project.path(), &out);
        // A file where the directory should be: read_dir fails with a non-NotFound error.
        let not_a_dir = project.path().join("agents-file");
        std::fs::write(&not_a_dir, "x").unwrap();
        let report = sync_codex_prefixes(&not_a_dir, project.path(), &out);
        assert!(report.removed.is_empty());
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(out.join("alpha.json").exists());
        // A missing home is genuinely empty: the sweep runs.
        let report = sync_codex_prefixes(&project.path().join("nope"), project.path(), &out);
        assert_eq!(report.removed, vec!["alpha"]);
    }

    #[test]
    fn sync_skips_rewrite_when_hash_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let out = project.path().join("out");
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "B");
        sync_codex_prefixes(home.path(), project.path(), &out);
        let file = out.join("alpha.json");
        let before = std::fs::metadata(&file).unwrap().modified().unwrap();
        let report = sync_codex_prefixes(home.path(), project.path(), &out);
        assert_eq!(report.unchanged, vec!["alpha"]);
        assert!(report.written.is_empty());
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            before
        );
        // A real change is written.
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "CHANGED");
        let report = sync_codex_prefixes(home.path(), project.path(), &out);
        assert_eq!(report.written, vec!["alpha"]);
    }

    #[test]
    fn sync_removes_files_of_agents_that_no_longer_exist() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let out = project.path().join("out");
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "B");
        write_package(home.path(), "gone", "1.0.0", "fs.read", "", "B");
        sync_codex_prefixes(home.path(), project.path(), &out);
        std::fs::write(out.join("foreign.json"), "{\"name\": \"foreign\"}").unwrap();
        std::fs::write(out.join("notes.txt"), "keep").unwrap();
        std::fs::remove_dir_all(home.path().join("gone")).unwrap();
        let report = sync_codex_prefixes(home.path(), project.path(), &out);
        assert_eq!(report.removed, vec!["gone"]);
        assert!(!out.join("gone.json").exists());
        assert!(out.join("alpha.json").exists());
        assert!(out.join("foreign.json").exists());
        assert!(out.join("notes.txt").exists());
    }

    #[test]
    fn sync_collects_per_agent_errors_and_writes_the_others() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let out = project.path().join("out");
        write_package(home.path(), "alpha", "1.0.0", "fs.read", "", "B");
        write_package(home.path(), "broken", "1.0.0", "fs.read", "", "B");
        std::fs::write(home.path().join("broken/1.0.0/agent.md"), "no frontmatter").unwrap();
        std::fs::write(home.path().join("stray-file"), "x").unwrap();
        let report = sync_codex_prefixes(home.path(), project.path(), &out);
        assert_eq!(report.written, vec!["alpha"]);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].0, "broken");
        assert!(out.join("alpha.json").exists());
        assert!(!out.join("broken.json").exists());
    }
}
