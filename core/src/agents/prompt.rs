//! The prompt of a catalogue agent, assembled from the most stable layer to the most volatile so the
//! provider's prompt cache keeps the longest prefix (spec 2026-10-20 section 6):
//!
//! 1. the body of `agent.md`;
//! 2. `knowledge.inject`, whole;
//! 3. an index of `knowledge.on_demand`: installed absolute path and first line of each file;
//! 4. the declared `context` (project instructions, memory, decisions), under
//!    [`AGENT_CONTEXT_BUDGET_CHARS`], cut from the tail with a marker;
//! 5. the output shape (`output.md`, or the caller's contract);
//! 6. the loadout block, as the caller resolved it;
//! 7. the task.
//!
//! Layers 1 to 4 plus the output shape form [`build_prefix`], the part that is identical across
//! runs of one package; the spec lists the loadout before the output shape, but the prefix must be
//! contiguous for engines that cache it, so the output shape moved up one place.

use std::path::{Path, PathBuf};

use super::package::{ContextSource, Package};

/// The most characters the declared project context (layer 4, all sources together) may take.
pub const AGENT_CONTEXT_BUDGET_CHARS: usize = 24_000;

/// What the caller read of the project: only the sources a package declares are used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectContext {
    /// `CLAUDE.md` / `AGENTS.md` of the project.
    pub instructions: Option<String>,
    /// The project's memory file under `<state_dir>`.
    pub memory: Option<String>,
    /// The project's decisions file under `<state_dir>`.
    pub decisions: Option<String>,
}

/// `text` cut to its first `limit` characters, with a marker saying how much was dropped. Text that
/// fits is returned as it is.
pub(crate) fn cut_tail(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    let kept: String = text.chars().take(limit).collect();
    format!(
        "{kept}\n[... cut: {} characters omitted ...]",
        count - limit
    )
}

/// One index line: the file's absolute path and its first non-blank line.
fn index_line(path: &Path, content: &str) -> String {
    let first = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(empty)");
    format!("- {} — {first}", path.display())
}

/// Every file under `root`, as `/`-separated paths relative to it, sorted.
fn files_under(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => walk(&entry.path(), &relative, out),
                Ok(kind) if kind.is_file() => out.push(relative),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out.sort();
    out
}

/// Layer 3: the files `on_demand` globs match, indexed. Empty when there are none.
fn on_demand_index(root: &Path, globs: &[String]) -> String {
    let files = files_under(root);
    let mut lines: Vec<String> = Vec::new();
    for relative in &files {
        if globs
            .iter()
            .any(|glob| crate::models::route_advice::glob_match(glob, relative))
        {
            let path: PathBuf = root.join(relative);
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            lines.push(index_line(&path, &content));
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "Reference files, read the ones you need:\n{}",
        lines.join("\n")
    )
}

/// Layer 2: each injected file, whole.
fn injected(root: &Path, paths: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for relative in paths {
        match std::fs::read_to_string(root.join(relative)) {
            Ok(content) => parts.push(content.trim_end().to_owned()),
            Err(error) => {
                tracing::warn!(file = %relative, %error, "an injected agent file could not be read");
            }
        }
    }
    parts.join("\n\n")
}

/// Layer 4: the declared sources in their declared order, sharing one budget. A source that finds
/// the budget spent is dropped; one that overruns it is cut from the tail.
fn project_context(pkg: &Package, ctx: &ProjectContext) -> String {
    let mut remaining = AGENT_CONTEXT_BUDGET_CHARS;
    let mut parts: Vec<String> = Vec::new();
    for source in &pkg.context {
        let (label, text) = match source {
            ContextSource::ProjectInstructions => ("Project instructions", &ctx.instructions),
            ContextSource::ProjectMemory => ("Project memory", &ctx.memory),
            ContextSource::ProjectDecisions => ("Project decisions", &ctx.decisions),
        };
        let Some(text) = text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        if remaining == 0 {
            break;
        }
        let cut = cut_tail(text, remaining);
        remaining = remaining.saturating_sub(text.chars().count());
        parts.push(format!("## {label}\n{cut}"));
    }
    parts.join("\n\n")
}

/// Joins the non-empty layers, each under its label, in the order given.
fn assemble(layers: &[(&str, String)]) -> String {
    layers
        .iter()
        .filter(|(_, text)| !text.trim().is_empty())
        .map(|(label, text)| {
            if label.is_empty() {
                text.trim_end().to_owned()
            } else {
                format!("# {label}\n{}", text.trim_end())
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Where an engine that reads files itself finds the project sources, so layer 4 can name them
/// instead of inlining them (the prefix then stays byte-stable while the files change).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextRefs {
    pub instructions: Option<PathBuf>,
    pub memory: Option<PathBuf>,
    pub decisions: Option<PathBuf>,
}

/// How layer 4 is rendered: the text inline (under the budget), or references to the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextRender {
    Inline(ProjectContext),
    Refs(ContextRefs),
}

/// Layer 4 as references: one line per declared source that has a path.
fn context_refs(pkg: &Package, refs: &ContextRefs) -> String {
    let lines: Vec<String> = pkg
        .context
        .iter()
        .filter_map(|source| {
            let (label, path) = match source {
                ContextSource::ProjectInstructions => ("Project instructions", &refs.instructions),
                ContextSource::ProjectMemory => ("Project memory", &refs.memory),
                ContextSource::ProjectDecisions => ("Project decisions", &refs.decisions),
            };
            path.as_ref()
                .map(|path| format!("- {label}: {}", path.display()))
        })
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    format!("Read these files when you need them:\n{}", lines.join("\n"))
}

/// The stable head of the prompt: layers 1 to 4 and the output shape. Everything that varies per
/// run (loadout, task) comes after it, so a provider's prompt cache keeps this whole prefix.
pub fn build_prefix(pkg: &Package, ctx: &ContextRender, output: &str) -> String {
    let project = match ctx {
        ContextRender::Inline(ctx) => project_context(pkg, ctx),
        ContextRender::Refs(refs) => context_refs(pkg, refs),
    };
    assemble(&[
        ("", pkg.body.clone()),
        ("Knowledge", injected(&pkg.root, &pkg.inject)),
        (
            "Reference index",
            on_demand_index(&pkg.root, &pkg.on_demand),
        ),
        ("Project context", project),
        ("Required output format", output.to_owned()),
    ])
}

/// The prompt of a package agent, stable to volatile: layers 1 to 4, the output shape, the loadout
/// block, the task. `output` is the shape text sent to the model (`output.md`, or the caller's
/// contract). The output shape (spec layer 6) sits before the loadout (layer 5) so that the prefix
/// is contiguous; the loadout is per run, the output shape is per package.
pub fn build_prompt(
    pkg: &Package,
    ctx: &ProjectContext,
    loadout_block: &str,
    output: &str,
    task: &str,
) -> String {
    let prefix = build_prefix(pkg, &ContextRender::Inline(ctx.clone()), output);
    let tail = assemble(&[("", loadout_block.to_owned()), ("Task", task.to_owned())]);
    [prefix, tail]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::package::PackageRef;

    fn package(root: &Path) -> Package {
        Package {
            reference: PackageRef {
                name: "reviewer".into(),
                version: "1.0.0".into(),
            },
            description: "d".into(),
            capabilities: Vec::new(),
            context: vec![
                ContextSource::ProjectInstructions,
                ContextSource::ProjectMemory,
            ],
            inject: vec!["knowledge/checklist.md".into()],
            on_demand: vec!["knowledge/ref/*.md".into()],
            body: "BODY".into(),
            output_md: String::new(),
            root: root.to_path_buf(),
        }
    }

    #[test]
    fn a_cut_keeps_the_head_and_says_how_much_went() {
        assert_eq!(cut_tail("abc", 3), "abc");
        assert_eq!(
            cut_tail("abcdef", 2),
            "ab\n[... cut: 4 characters omitted ...]"
        );
        assert_eq!(cut_tail("ééé", 1), "é\n[... cut: 2 characters omitted ...]");
    }

    #[test]
    fn an_index_line_is_the_path_and_the_first_non_blank_line() {
        let line = index_line(Path::new("/a/b.md"), "\n\n  Title here \nmore");
        assert_eq!(line, "- /a/b.md — Title here");
        assert!(index_line(Path::new("/a/c.md"), "").ends_with("(empty)"));
    }

    #[test]
    fn layers_come_stable_to_volatile_and_empty_ones_are_omitted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("knowledge/ref")).unwrap();
        std::fs::write(dir.path().join("knowledge/checklist.md"), "CHECKLIST\n").unwrap();
        std::fs::write(dir.path().join("knowledge/ref/a.md"), "\nREFHEAD\nbody").unwrap();
        std::fs::write(dir.path().join("knowledge/ref/b.txt"), "no").unwrap();
        let ctx = ProjectContext {
            instructions: Some("INSTR".into()),
            memory: None,
            decisions: Some("DECIDED".into()),
        };
        let prompt = build_prompt(&package(dir.path()), &ctx, "LOADOUT", "OUTPUT", "TASK");
        let at = |needle: &str| {
            prompt
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        let order = [
            at("BODY"),
            at("CHECKLIST"),
            at("REFHEAD"),
            at("INSTR"),
            at("OUTPUT"),
            at("LOADOUT"),
            at("TASK"),
        ];
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{prompt}");
        assert!(!prompt.contains("DECIDED"), "undeclared context stays out");
        assert!(
            !prompt.contains("b.txt"),
            "an unmatched file is not indexed"
        );
        assert!(prompt.contains(&dir.path().join("knowledge/ref/a.md").display().to_string()));
        assert!(
            !prompt.contains("Project memory"),
            "an absent source leaves no heading"
        );
    }

    #[test]
    fn the_prompt_is_the_prefix_then_loadout_then_task() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ProjectContext {
            instructions: Some("INSTR".into()),
            memory: None,
            decisions: None,
        };
        let pkg = package(dir.path());
        let prefix = build_prefix(&pkg, &ContextRender::Inline(ctx.clone()), "OUTPUT");
        let prompt = build_prompt(&pkg, &ctx, "LOADOUT", "OUTPUT", "TASK");
        assert_eq!(prompt, format!("{prefix}\n\nLOADOUT\n\n# Task\nTASK"));
        // The prefix does not depend on the per-run parts.
        assert!(!prefix.contains("LOADOUT") && !prefix.contains("TASK"));
        assert!(prefix.ends_with("OUTPUT"));
    }

    #[test]
    fn references_name_the_declared_files_instead_of_inlining() {
        let dir = tempfile::tempdir().unwrap();
        let refs = ContextRefs {
            instructions: Some(PathBuf::from("/p/AGENTS.md")),
            memory: Some(PathBuf::from("/p/.ai/memory.md")),
            decisions: Some(PathBuf::from("/p/.ai/decisions.md")),
        };
        let prefix = build_prefix(&package(dir.path()), &ContextRender::Refs(refs), "OUTPUT");
        assert!(prefix.contains("- Project instructions: /p/AGENTS.md"));
        assert!(prefix.contains("- Project memory: /p/.ai/memory.md"));
        assert!(
            !prefix.contains("decisions.md"),
            "undeclared source stays out"
        );
        let none = build_prefix(
            &package(dir.path()),
            &ContextRender::Refs(ContextRefs::default()),
            "O",
        );
        assert!(!none.contains("Project context"));
    }

    #[test]
    fn the_context_budget_cuts_the_tail_with_a_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut pkg = package(dir.path());
        pkg.inject.clear();
        pkg.on_demand.clear();
        let ctx = ProjectContext {
            instructions: Some("x".repeat(AGENT_CONTEXT_BUDGET_CHARS - 10)),
            memory: Some(format!("{}TAILEND", "m".repeat(100))),
            decisions: None,
        };
        let layer = project_context(&pkg, &ctx);
        assert!(layer.contains("## Project memory"));
        assert!(layer.contains("[... cut:"), "{}", layer.len());
        assert!(!layer.contains("TAILEND"));
        // 10 characters of budget were left for the memory.
        assert!(layer.contains("\nmmmmmmmmmm\n[... cut: 97 characters omitted ...]"));
    }
}
