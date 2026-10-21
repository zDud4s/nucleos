//! Claude Code agent files generated from agent packages (plan C1).
//!
//! Every installed package is exposed to interactive Claude Code sessions as
//! `<claude_home>/agents/<name>.md`, the user-level agents directory (the same place hand-made
//! agents such as `worker` live). The highest installed version of each name wins.
//!
//! # Ownership is decided by a record, not by the marker
//!
//! The first body line of a generated file is a marker comment, but it only warns humans.
//! `<data_dir>/agents/harness-claude.json` records the hash of every file written. Per file:
//!
//! - absent: write and record;
//! - present and unchanged since written: overwrite when the content differs;
//! - present and hand-edited: keep, report `Edited`;
//! - present and unrecorded (hand-made): keep, report `Collision`; never adopted, even when it
//!   carries the marker;
//! - recorded but its package is gone: delete when unchanged, else keep and report `EditedOrphan`.
//!
//! An unparsable record refuses the whole run.
//!
//! # Tools
//!
//! The `tools` line comes from [`translate`] for [`Engine::Claude`]. A package whose translation is
//! empty is not generated (`NoTools`): an omitted `tools` line means "inherit everything" in Claude
//! Code, which would silently widen the agent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::capabilities::{Engine, translate};
use super::package::{ContextSource, Package, PackageRef, highest_version, load_installed};
use super::prompt::{ContextRefs, ContextRender, build_prefix};
use nucleos_base::agent::Capability;

/// Start of the first body line of every generated file.
pub const MARKER_PREFIX: &str = "<!-- nucleos-generated:";

/// Names never generated: the hand-made `worker` and the `nucleos-dev` cockpits (`wf-*`).
fn reserved(name: &str) -> bool {
    name == "worker" || name.starts_with("wf-")
}

/// The Claude Code config directory: `config_dir` (`CLAUDE_CONFIG_DIR`) when set and non-empty,
/// else `<home>/.claude`. Pure so a test needs no environment.
pub fn claude_home_from(config_dir: Option<&str>, home: Option<PathBuf>) -> Option<PathBuf> {
    match config_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => home.map(|home| home.join(".claude")),
    }
}

/// `<claude home>/agents`, from the real environment.
pub fn claude_agents_dir() -> Option<PathBuf> {
    let config = std::env::var("CLAUDE_CONFIG_DIR").ok();
    let home = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf());
    claude_home_from(config.as_deref(), home).map(|dir| dir.join("agents"))
}

/// Where the hash record lives: `<data_dir>/agents/harness-claude.json`.
pub fn record_path(data_dir: &Path) -> PathBuf {
    data_dir.join("agents").join("harness-claude.json")
}

/// A double-quoted YAML scalar: `\` and `"` escaped, line breaks folded to spaces.
fn yaml_quote(text: &str) -> String {
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let escaped = folded.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Where the project sources are read from by an interactive session. Paths are relative to the
/// repository root, except the two state files, which live under the project's `state_dir`.
fn context_refs(pkg: &Package) -> ContextRefs {
    let mut refs = ContextRefs::default();
    for source in &pkg.context {
        match source {
            ContextSource::ProjectInstructions => {
                refs.instructions = Some(PathBuf::from("CLAUDE.md"));
            }
            ContextSource::ProjectMemory => {
                refs.memory = Some(PathBuf::from("<state_dir>/memory.md"));
            }
            ContextSource::ProjectDecisions => {
                refs.decisions = Some(PathBuf::from("<state_dir>/decisions.md"));
            }
        }
    }
    refs
}

/// The full text of `<name>.md` for Claude Code. Pure given the installed package files.
pub fn render_claude_agent(pkg: &Package) -> Result<String, super::capabilities::Unsupported> {
    let tools = translate(Engine::Claude, &pkg.capabilities)?;
    // NEEDS A REAL-SESSION CHECK: `mcp__nucleos__*` is assumed to be accepted in a subagent's
    // `tools` line (plan C1, open question Q3).
    let tools_line = tools.claude_allowed_tools.join(", ");
    let can_read = pkg.capabilities.contains(&Capability::FsRead);
    let refs = if can_read {
        context_refs(pkg)
    } else {
        ContextRefs::default()
    };
    let needs_state_note = can_read
        && pkg.context.iter().any(|s| {
            matches!(
                s,
                ContextSource::ProjectMemory | ContextSource::ProjectDecisions
            )
        });
    let prefix = build_prefix(pkg, &ContextRender::Refs(refs), &pkg.output_md);

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", pkg.reference.name));
    out.push_str(&format!("description: {}\n", yaml_quote(&pkg.description)));
    out.push_str(&format!("tools: {tools_line}\n"));
    out.push_str("---\n");
    out.push_str(&format!(
        "{MARKER_PREFIX} {}; edit the package, not this file; regenerated by the daemon -->\n\n",
        pkg.reference
    ));
    out.push_str(prefix.trim_end());
    if needs_state_note {
        out.push_str(
            "\n\n`<state_dir>` is the `state_dir` field of `.nucleos/workflow/project.json` \
             in the repository; skip those files silently when it is absent.",
        );
    }
    out.push('\n');
    Ok(out)
}

/// What a sync did to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Written,
    Replaced,
    Unchanged,
    Removed,
    /// Generated before, hand-edited since: kept.
    Edited,
    /// A hand-made file of the same name: kept.
    Collision,
    /// A reserved name: never generated.
    Reserved,
    /// The package grants no tools: not generated.
    NoTools,
    /// Orphaned and hand-edited: kept.
    EditedOrphan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub file: String,
    pub package_ref: Option<String>,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub entries: Vec<Entry>,
}

impl Report {
    /// One log line: the count of each outcome that is not `Unchanged`.
    pub fn summary(&self) -> String {
        let label = |o: Outcome| match o {
            Outcome::Written => "written",
            Outcome::Replaced => "replaced",
            Outcome::Unchanged => "unchanged",
            Outcome::Removed => "removed",
            Outcome::Edited => "kept (hand-edited)",
            Outcome::Collision => "kept (hand-made file of that name)",
            Outcome::Reserved => "reserved",
            Outcome::NoTools => "skipped (no tools)",
            Outcome::EditedOrphan => "kept (orphaned but hand-edited)",
        };
        let mut counts: Vec<(Outcome, usize)> = Vec::new();
        for entry in &self.entries {
            match counts.iter_mut().find(|(o, _)| *o == entry.outcome) {
                Some((_, n)) => *n += 1,
                None => counts.push((entry.outcome, 1)),
            }
        }
        let parts: Vec<String> = counts
            .iter()
            .filter(|(o, _)| *o != Outcome::Unchanged)
            .map(|(o, n)| format!("{n} {}", label(*o)))
            .collect();
        if parts.is_empty() {
            format!("claude agents: {} unchanged", self.entries.len())
        } else {
            format!("claude agents: {}", parts.join(", "))
        }
    }
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Record {
    #[serde(default)]
    files: BTreeMap<String, Recorded>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Recorded {
    package_ref: String,
    hash: String,
}

fn read_record(path: &Path) -> Result<Record, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not a valid record: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Record::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn save_record(path: &Path, record: &Record) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Keep only the highest version of each name, sorted by name.
fn latest_per_name(packages: &[Package]) -> Vec<&Package> {
    let mut best: BTreeMap<&str, &Package> = BTreeMap::new();
    for pkg in packages {
        let slot = best.entry(pkg.reference.name.as_str()).or_insert(pkg);
        if pkg.reference.version_key() > slot.reference.version_key() {
            *slot = pkg;
        }
    }
    best.into_values().collect()
}

/// Bring `target_dir` in line with `packages`, following the rules in the module docs.
pub fn sync_claude_agents(
    packages: &[Package],
    target_dir: &Path,
    record: &Path,
) -> Result<Report, String> {
    sync_claude_agents_keeping(packages, &BTreeSet::new(), target_dir, record)
}

/// As `sync_claude_agents`, but the files in `keep` are never treated as orphans (their package
/// is installed yet could not be loaded).
fn sync_claude_agents_keeping(
    packages: &[Package],
    keep: &BTreeSet<String>,
    target_dir: &Path,
    record: &Path,
) -> Result<Report, String> {
    let mut rec = read_record(record)?;
    std::fs::create_dir_all(target_dir).map_err(|e| format!("{}: {e}", target_dir.display()))?;
    let mut report = Report::default();
    let mut wanted: BTreeSet<String> = BTreeSet::new();

    for pkg in latest_per_name(packages) {
        let name = &pkg.reference.name;
        let file = format!("{name}.md");
        let package_ref = Some(pkg.reference.to_string());
        let mut push = |outcome| {
            report.entries.push(Entry {
                file: file.clone(),
                package_ref: package_ref.clone(),
                outcome,
            })
        };
        if reserved(name) {
            push(Outcome::Reserved);
            continue;
        }
        let tools = translate(Engine::Claude, &pkg.capabilities).map_err(|e| e.to_string())?;
        if tools.claude_allowed_tools.is_empty() {
            push(Outcome::NoTools);
            continue;
        }
        wanted.insert(file.clone());
        let text = render_claude_agent(pkg).map_err(|e| e.to_string())?;
        let path = nucleos_base::inspect::safe_write_target(target_dir, &file)
            .map_err(|e| format!("{file}: unsafe target ({e:?})"))?;
        let write = |rec: &mut Record| -> Result<(), String> {
            std::fs::write(&path, &text).map_err(|e| format!("{}: {e}", path.display()))?;
            rec.files.insert(
                file.clone(),
                Recorded {
                    package_ref: pkg.reference.to_string(),
                    hash: crate::workflows::hash_of(text.as_bytes()),
                },
            );
            save_record(record, rec)
        };
        if !path.exists() {
            write(&mut rec)?;
            push(Outcome::Written);
            continue;
        }
        let current = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        match rec.files.get(&file) {
            None => push(Outcome::Collision),
            Some(recorded) if recorded.hash != crate::workflows::hash_of(&current) => {
                push(Outcome::Edited)
            }
            Some(_) if current == text.as_bytes() => push(Outcome::Unchanged),
            Some(_) => {
                write(&mut rec)?;
                push(Outcome::Replaced);
            }
        }
    }

    // Deletion pass: recorded files whose package is no longer exposed.
    let orphans: Vec<(String, Recorded)> = rec
        .files
        .iter()
        .filter(|(file, _)| !wanted.contains(*file) && !keep.contains(*file))
        .map(|(file, recorded)| (file.clone(), recorded.clone()))
        .collect();
    for (file, recorded) in orphans {
        let package_ref = Some(recorded.package_ref.clone());
        let path = nucleos_base::inspect::safe_write_target(target_dir, &file)
            .map_err(|e| format!("{file}: unsafe target ({e:?})"))?;
        let outcome = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                rec.files.remove(&file);
                save_record(record, &rec)?;
                continue;
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
            Ok(bytes) if crate::workflows::hash_of(&bytes) == recorded.hash => {
                std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                rec.files.remove(&file);
                save_record(record, &rec)?;
                Outcome::Removed
            }
            Ok(_) => Outcome::EditedOrphan,
        };
        report.entries.push(Entry {
            file,
            package_ref,
            outcome,
        });
    }
    Ok(report)
}

/// What `load_highest` found under the agents home.
struct Loaded {
    packages: Vec<Package>,
    /// `<name>.md` of every package whose directory exists but did not load: still present, so
    /// its generated file must survive the orphan pass.
    broken: BTreeSet<String>,
}

/// The highest installed version of every package under `agents_home`. A package that fails to
/// load is reported through `tracing` and recorded in `broken`. A missing home is empty; an
/// unreadable one is an error, so the caller does not delete files on a failed listing.
fn load_highest(agents_home: &Path) -> Result<Loaded, String> {
    let entries = match std::fs::read_dir(agents_home) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                packages: Vec::new(),
                broken: BTreeSet::new(),
            });
        }
        Err(e) => return Err(format!("{}: {e}", agents_home.display())),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        // `install.rs` stages under `<home>/.staging`; a dot directory is never a package.
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    let mut loaded = Loaded {
        packages: Vec::new(),
        broken: BTreeSet::new(),
    };
    for name in &names {
        let result = match highest_version(agents_home, name) {
            Some(reference) => {
                let reference: PackageRef = reference;
                load_installed(agents_home, &reference).map_err(|e| e.to_string())
            }
            None => Err("no installed version".to_string()),
        };
        match result {
            Ok(pkg) => loaded.packages.push(pkg),
            Err(error) => {
                tracing::warn!(agent = %name, %error, "agent package failed to load; keeping its generated file");
                loaded.broken.insert(format!("{name}.md"));
            }
        }
    }
    Ok(loaded)
}

/// Generate the Claude Code agent files for everything installed under `agents_home`, into the
/// user-level agents directory, recording hashes under `data_dir`.
pub fn sync_installed(agents_home: &Path, data_dir: &Path) -> Result<Report, String> {
    let target = claude_agents_dir().ok_or_else(|| {
        "no Claude config directory (no CLAUDE_CONFIG_DIR and no home)".to_string()
    })?;
    sync_installed_into(agents_home, &target, data_dir)
}

fn sync_installed_into(
    agents_home: &Path,
    target: &Path,
    data_dir: &Path,
) -> Result<Report, String> {
    let loaded = load_highest(agents_home)?;
    sync_claude_agents_keeping(
        &loaded.packages,
        &loaded.broken,
        target,
        &record_path(data_dir),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg_in(root: &Path, name: &str, version: &str, caps: &[Capability]) -> Package {
        let dir = root.join(name).join(version);
        std::fs::create_dir_all(dir.join("knowledge/ref")).unwrap();
        Package {
            reference: PackageRef {
                name: name.into(),
                version: version.into(),
            },
            description: "Reviews code.".into(),
            capabilities: caps.to_vec(),
            context: Vec::new(),
            inject: Vec::new(),
            on_demand: Vec::new(),
            body: "BODY\n".into(),
            output_md: "OUTPUT\n".into(),
            root: dir,
        }
    }

    fn pkg(name: &str, caps: &[Capability]) -> Package {
        let tmp = std::env::temp_dir().join("nucleos-harness-test-unused");
        pkg_in(&tmp, name, "1.0.0", caps)
    }

    /// Frontmatter lines between the two `---`.
    fn front(text: &str) -> Vec<&str> {
        text.lines().skip(1).take_while(|l| *l != "---").collect()
    }

    struct Dirs {
        _tmp: tempfile::TempDir,
        target: PathBuf,
        record: PathBuf,
    }

    fn dirs() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        Dirs {
            target: tmp.path().join("claude").join("agents"),
            record: tmp
                .path()
                .join("data")
                .join("agents")
                .join("harness-claude.json"),
            _tmp: tmp,
        }
    }

    const READ: &[Capability] = &[Capability::FsRead];

    #[test]
    fn render_frontmatter_has_only_name_description_tools() {
        let text = render_claude_agent(&pkg("reviewer", READ)).unwrap();
        let keys: Vec<&str> = front(&text)
            .iter()
            .map(|l| l.split(':').next().unwrap())
            .collect();
        assert_eq!(keys, ["name", "description", "tools"]);
    }

    #[test]
    fn render_tools_follow_the_claude_column() {
        let caps = [Capability::FsRead, Capability::Web, Capability::MemoryRead];
        let text = render_claude_agent(&pkg("reviewer", &caps)).unwrap();
        assert!(
            text.contains("tools: Read, Glob, Grep, WebSearch, WebFetch, mcp__nucleos__*\n"),
            "{text}"
        );
    }

    /// Write a loadable installed package `<home>/reviewer/1.0.0`, returning its directory.
    fn write_loadable(home: &Path) -> PathBuf {
        let dir = home.join("reviewer").join("1.0.0");
        std::fs::create_dir_all(dir.join("knowledge/ref")).unwrap();
        std::fs::write(
            dir.join("agent.md"),
            "---\nname: reviewer\nversion: 1.0.0\ndescription: D.\ncapabilities: [fs.read]\n\
             context: []\nknowledge:\n  inject: [knowledge/inject.md]\n  on_demand: [knowledge/ref/*.md]\n---\nB\n",
        )
        .unwrap();
        std::fs::write(dir.join("output.md"), "OUT\n").unwrap();
        std::fs::write(dir.join("knowledge/inject.md"), "I\n").unwrap();
        dir
    }

    #[test]
    fn a_package_that_fails_to_load_keeps_its_generated_file() {
        let d = dirs();
        let home = d._tmp.path().join("agents-home");
        let root = write_loadable(&home);
        let data = d._tmp.path().join("data");
        let report = sync_installed_into(&home, &d.target, &data).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Written, "{report:?}");
        // Break it: the directory exists, the load fails, the file stays.
        std::fs::write(root.join("agent.md"), "not a package").unwrap();
        let report = sync_installed_into(&home, &d.target, &data).unwrap();
        assert!(report.entries.is_empty(), "{report:?}");
        assert!(d.target.join("reviewer.md").exists());
        // Removing the package directory makes it a real orphan.
        std::fs::remove_dir_all(home.join("reviewer")).unwrap();
        let report = sync_installed_into(&home, &d.target, &data).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Removed);
    }

    #[test]
    fn the_installers_staging_directory_is_not_a_package() {
        let d = dirs();
        let home = d._tmp.path().join("agents-home");
        write_loadable(&home);
        // `install.rs` stages under `<home>/.staging/<name>/<version>` and leaves the directory.
        std::fs::create_dir_all(home.join(".staging/reviewer/1.0.0")).unwrap();
        let loaded = load_highest(&home).unwrap();
        assert!(loaded.broken.is_empty(), "{:?}", loaded.broken);
        assert_eq!(loaded.packages.len(), 1);
    }

    #[test]
    fn an_unreadable_agents_home_deletes_nothing() {
        let d = dirs();
        let data = d._tmp.path().join("data");
        let home = d._tmp.path().join("agents-home");
        write_loadable(&home);
        sync_installed_into(&home, &d.target, &data).unwrap();
        assert!(d.target.join("reviewer.md").exists());
        // A file where the directory should be: read_dir fails with a non-NotFound error.
        let not_a_dir = d._tmp.path().join("agents-file");
        std::fs::write(&not_a_dir, "x").unwrap();
        assert!(sync_installed_into(&not_a_dir, &d.target, &data).is_err());
        assert!(d.target.join("reviewer.md").exists());
    }

    #[test]
    fn sync_skips_an_agent_with_no_tools() {
        let d = dirs();
        let report = sync_claude_agents(&[pkg("mute", &[])], &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::NoTools);
        assert!(!d.target.join("mute.md").exists());
    }

    #[test]
    fn render_description_is_yaml_escaped() {
        let mut p = pkg("reviewer", READ);
        p.description = "Says \"hi\": a\\b\nsecond line".into();
        let text = render_claude_agent(&p).unwrap();
        assert!(
            text.contains("description: \"Says \\\"hi\\\": a\\\\b second line\"\n"),
            "{text}"
        );
    }

    #[test]
    fn render_body_order_is_prompt_knowledge_index_context_output() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = pkg_in(tmp.path(), "reviewer", "1.0.0", READ);
        std::fs::write(p.root.join("knowledge/a.md"), "INJECTED\n").unwrap();
        std::fs::write(p.root.join("knowledge/ref/x.md"), "Title of x\n").unwrap();
        p.inject = vec!["knowledge/a.md".into()];
        p.on_demand = vec!["knowledge/ref/*.md".into()];
        p.context = vec![ContextSource::ProjectInstructions];
        let text = render_claude_agent(&p).unwrap();
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle}\n{text}"))
        };
        let order = [
            at("BODY"),
            at("# Knowledge"),
            at("# Reference index"),
            at("# Project context"),
            at("# Required output format"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    }

    #[test]
    fn render_inlines_inject_and_indexes_on_demand_by_absolute_path_and_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        let mut p = pkg_in(tmp.path(), "reviewer", "1.0.0", READ);
        std::fs::write(p.root.join("knowledge/a.md"), "INJECTED\n").unwrap();
        std::fs::write(p.root.join("knowledge/ref/x.md"), "\nTitle of x\nmore\n").unwrap();
        p.inject = vec!["knowledge/a.md".into()];
        p.on_demand = vec!["knowledge/ref/*.md".into()];
        let text = render_claude_agent(&p).unwrap();
        assert!(text.contains("INJECTED"));
        let line = format!(
            "{} — Title of x",
            p.root.join("knowledge/ref/x.md").display()
        );
        assert!(text.contains(&line), "{line}\n{text}");
        assert!(!text.contains("more"));
    }

    #[test]
    fn render_context_pointers_only_for_declared_kinds() {
        let mut p = pkg("reviewer", READ);
        p.context = vec![ContextSource::ProjectInstructions];
        let text = render_claude_agent(&p).unwrap();
        assert!(text.contains("CLAUDE.md"));
        assert!(!text.contains("memory.md") && !text.contains("decisions.md"));
        p.context = vec![
            ContextSource::ProjectMemory,
            ContextSource::ProjectDecisions,
        ];
        let text = render_claude_agent(&p).unwrap();
        assert!(text.contains("<state_dir>/memory.md"));
        assert!(text.contains("<state_dir>/decisions.md"));
        assert!(!text.contains("CLAUDE.md"));
    }

    #[test]
    fn render_omits_context_section_without_fs_read() {
        let mut p = pkg("reviewer", &[Capability::Web]);
        p.context = vec![
            ContextSource::ProjectInstructions,
            ContextSource::ProjectMemory,
        ];
        let text = render_claude_agent(&p).unwrap();
        assert!(!text.contains("Project context"));
        assert!(!text.contains("CLAUDE.md") && !text.contains("<state_dir>"));
    }

    #[test]
    fn render_is_deterministic_and_ends_with_one_newline() {
        let p = pkg("reviewer", READ);
        let a = render_claude_agent(&p).unwrap();
        assert_eq!(a, render_claude_agent(&p).unwrap());
        assert!(a.ends_with('\n') && !a.ends_with("\n\n"));
        assert!(!a.contains('\r'));
    }

    #[test]
    fn render_carries_the_marker_with_name_and_version() {
        let text = render_claude_agent(&pkg("reviewer", READ)).unwrap();
        let body_first = text
            .lines()
            .skip_while(|l| *l != "---")
            .skip(1)
            .skip_while(|l| *l != "---")
            .nth(1)
            .unwrap();
        assert!(body_first.starts_with(MARKER_PREFIX), "{body_first}");
        assert!(body_first.contains("reviewer@1.0.0"));
    }

    #[test]
    fn sync_writes_and_records_a_new_agent() {
        let d = dirs();
        let p = pkg("reviewer", READ);
        let report = sync_claude_agents(std::slice::from_ref(&p), &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Written);
        let written = std::fs::read_to_string(d.target.join("reviewer.md")).unwrap();
        assert_eq!(written, render_claude_agent(&p).unwrap());
        let rec = read_record(&d.record).unwrap();
        let recorded = &rec.files["reviewer.md"];
        assert_eq!(recorded.package_ref, "reviewer@1.0.0");
        assert_eq!(recorded.hash, crate::workflows::hash_of(written.as_bytes()));
    }

    #[test]
    fn sync_is_idempotent() {
        let d = dirs();
        let p = [pkg("reviewer", READ)];
        sync_claude_agents(&p, &d.target, &d.record).unwrap();
        let report = sync_claude_agents(&p, &d.target, &d.record).unwrap();
        assert!(
            report
                .entries
                .iter()
                .all(|e| e.outcome == Outcome::Unchanged)
        );
    }

    #[test]
    fn sync_regenerates_on_a_version_bump() {
        let d = dirs();
        sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let bumped = pkg_in(tmp.path(), "reviewer", "1.1.0", READ);
        let report = sync_claude_agents(&[bumped], &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Replaced);
        let text = std::fs::read_to_string(d.target.join("reviewer.md")).unwrap();
        assert!(text.contains("reviewer@1.1.0"));
        assert_eq!(
            read_record(&d.record).unwrap().files["reviewer.md"].package_ref,
            "reviewer@1.1.0"
        );
    }

    #[test]
    fn sync_exposes_only_the_highest_installed_version() {
        let d = dirs();
        let tmp = tempfile::tempdir().unwrap();
        let old = pkg_in(tmp.path(), "reviewer", "1.9.0", READ);
        let new = pkg_in(tmp.path(), "reviewer", "1.10.0", READ);
        let report = sync_claude_agents(&[new, old], &d.target, &d.record).unwrap();
        assert_eq!(report.entries.len(), 1);
        let text = std::fs::read_to_string(d.target.join("reviewer.md")).unwrap();
        assert!(text.contains("reviewer@1.10.0"));
    }

    #[test]
    fn sync_never_overwrites_a_hand_edited_generated_file() {
        let d = dirs();
        let p = [pkg("reviewer", READ)];
        sync_claude_agents(&p, &d.target, &d.record).unwrap();
        let file = d.target.join("reviewer.md");
        std::fs::write(&file, "my edit\n").unwrap();
        let report = sync_claude_agents(&p, &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Edited);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "my edit\n");
    }

    #[test]
    fn sync_leaves_a_hand_made_file_alone() {
        let d = dirs();
        std::fs::create_dir_all(&d.target).unwrap();
        std::fs::write(d.target.join("reviewer.md"), "mine\n").unwrap();
        let report = sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Collision);
        assert_eq!(
            std::fs::read_to_string(d.target.join("reviewer.md")).unwrap(),
            "mine\n"
        );
    }

    #[test]
    fn sync_never_adopts_a_file_with_the_marker() {
        let d = dirs();
        std::fs::create_dir_all(&d.target).unwrap();
        let marked = format!("---\nname: reviewer\n---\n{MARKER_PREFIX} reviewer@1.0.0 -->\n");
        std::fs::write(d.target.join("reviewer.md"), &marked).unwrap();
        let report = sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Collision);
        assert_eq!(
            std::fs::read_to_string(d.target.join("reviewer.md")).unwrap(),
            marked
        );
        assert!(read_record(&d.record).unwrap().files.is_empty());
    }

    #[test]
    fn sync_reserves_worker_and_wf_names() {
        let d = dirs();
        let packages = [pkg("worker", READ), pkg("wf-coder", READ)];
        let report = sync_claude_agents(&packages, &d.target, &d.record).unwrap();
        assert!(
            report
                .entries
                .iter()
                .all(|e| e.outcome == Outcome::Reserved)
        );
        assert!(!d.target.join("worker.md").exists());
        assert!(!d.target.join("wf-coder.md").exists());
    }

    #[test]
    fn sync_deletes_the_file_when_the_package_is_gone() {
        let d = dirs();
        sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record).unwrap();
        let report = sync_claude_agents(&[], &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::Removed);
        assert!(!d.target.join("reviewer.md").exists());
        assert!(read_record(&d.record).unwrap().files.is_empty());
    }

    #[test]
    fn sync_keeps_an_edited_orphan() {
        let d = dirs();
        sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record).unwrap();
        std::fs::write(d.target.join("reviewer.md"), "edited\n").unwrap();
        let report = sync_claude_agents(&[], &d.target, &d.record).unwrap();
        assert_eq!(report.entries[0].outcome, Outcome::EditedOrphan);
        assert!(d.target.join("reviewer.md").exists());
        assert!(
            read_record(&d.record)
                .unwrap()
                .files
                .contains_key("reviewer.md")
        );
    }

    #[test]
    fn sync_refuses_an_unparsable_record_and_writes_nothing() {
        let d = dirs();
        std::fs::create_dir_all(d.record.parent().unwrap()).unwrap();
        std::fs::write(&d.record, "{ not json").unwrap();
        let result = sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record);
        assert!(result.is_err());
        assert!(!d.target.join("reviewer.md").exists());
    }

    #[test]
    fn sync_creates_the_target_dir() {
        let d = dirs();
        assert!(!d.target.exists());
        sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record).unwrap();
        assert!(d.target.join("reviewer.md").is_file());
    }

    #[test]
    fn sync_writes_nothing_outside_the_target() {
        let d = dirs();
        sync_claude_agents(&[pkg("reviewer", READ)], &d.target, &d.record).unwrap();
        let siblings: Vec<_> = std::fs::read_dir(d.target.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(siblings, ["agents"]);
        let files: Vec<_> = std::fs::read_dir(&d.target).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn claude_home_honours_claude_config_dir() {
        let home = Some(PathBuf::from("/home/u"));
        assert_eq!(
            claude_home_from(Some("/cfg"), home.clone()),
            Some(PathBuf::from("/cfg"))
        );
        assert_eq!(
            claude_home_from(Some(""), home.clone()),
            Some(PathBuf::from("/home/u").join(".claude"))
        );
        assert_eq!(
            claude_home_from(None, home),
            Some(PathBuf::from("/home/u").join(".claude"))
        );
        assert_eq!(claude_home_from(None, None), None);
    }

    #[test]
    fn summary_names_what_changed() {
        let report = Report {
            entries: vec![
                Entry {
                    file: "a.md".into(),
                    package_ref: None,
                    outcome: Outcome::Written,
                },
                Entry {
                    file: "b.md".into(),
                    package_ref: None,
                    outcome: Outcome::Unchanged,
                },
            ],
        };
        assert_eq!(report.summary(), "claude agents: 1 written");
    }
}
