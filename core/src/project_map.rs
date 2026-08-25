//! The structure layer of a project's map: what modules exist, and what they import.
//!
//! **Pure, and that is what makes it callable from anywhere.** It takes a path and returns
//! data; it does not know what SQL is and does not know what HTTP is. The extraction slice
//! will need to call this from somewhere else entirely, and a module that only knows how to
//! read a tree can be called from there without being moved.
//!
//! **Nothing here is persisted.** The derivation runs on every read — decision 1 of the spec.
//! A stored map is a portrait, and `docs/funcionalidades.md` already demonstrated what
//! happens to those: it was three weeks old and already wrong about four modules.
//!
//! This slice reads Rust and TypeScript. Go, which the sidecars are written in, is not read —
//! and Go files therefore come back in [`Structure::unread`] rather than vanishing. A map that
//! pretends `sidecars/` does not exist is lying about the architecture; one that says "I
//! cannot read this" is not.

use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

/// A language this reader knows how to interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reader {
    Rust,
    Typescript,
}

/// Who reads this file, or `None` when nobody here does.
///
/// `None` is not a failure and is not a file to skip: it is what puts the path in `unread`.
pub fn reader_for(path: &str) -> Option<Reader> {
    // A `.d.ts` declares types that something else implements. It is nobody's module, and
    // counting it would put a permanently orphaned node on the map — noise that never resolves
    // no matter who looks at it.
    if path.ends_with(".d.ts") {
        return None;
    }
    if path.ends_with(".rs") {
        return Some(Reader::Rust);
    }
    if path.ends_with(".ts") || path.ends_with(".tsx") {
        return Some(Reader::Typescript);
    }
    None
}

/// Which modules of this crate a Rust file names.
///
/// Catches `use crate::x` and also a bare `crate::x::y(...)` in the middle of an expression,
/// because `http.rs` calls dozens of modules by full path without ever writing `use` for them
/// — and without this every route would be drawn with no edge to the module it serves, which
/// is half the graph missing.
///
/// **Deliberately not a parser.** A `crate::` inside a string literal or a comment counts as
/// an edge. The error that produces is one extra edge between two modules that already mention
/// each other by name — cheap, visible, and correctable by looking. The error a real parser
/// would avoid does not justify pulling `syn` into this slice.
pub fn rust_imports(source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (index, _) in source.match_indices("crate::") {
        let rest = &source[index + "crate::".len()..];

        // `use crate::{a, b};` — a group, and every name inside it counts.
        if let Some(inner) = rest.strip_prefix('{') {
            let close = match inner.find('}') {
                Some(at) => at,
                None => continue,
            };
            for part in inner[..close].split(',') {
                if let Some(name) = leading_ident(part.trim()) {
                    found.insert(name);
                }
            }
            continue;
        }

        if let Some(name) = leading_ident(rest) {
            found.insert(name);
        }
    }
    found
}

/// The identifier a piece of text starts with, or nothing.
fn leading_ident(text: &str) -> Option<String> {
    let name: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

/// Which files of this project a TypeScript module imports, by path without an extension.
///
/// Only **relative** specifiers count. `@tanstack/react-query` is a dependency, not a module
/// of this project, and drawing it would fill the map with nodes nobody here wrote.
///
/// The extension is deliberately left unresolved: `./client` could be `client.ts` or
/// `client.tsx`, and only something that has already walked the tree knows which. That
/// decision belongs to [`structure`], where the file list exists.
///
/// `path` is the module's path from the project root, joined with `/` on every platform, which
/// is the form [`structure`] hands over. Given a backslash path this finds no folder at all and
/// quietly resolves every import as if the file sat at the root — so the normalisation belongs
/// to the caller that walked the filesystem, and is not repeated here.
pub fn ts_imports(path: &str, source: &str) -> BTreeSet<String> {
    let folder = path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let mut found = BTreeSet::new();

    for quote in ['"', '\''] {
        for (index, _) in source.match_indices(quote) {
            let rest = &source[index + 1..];
            let close = match rest.find(quote) {
                Some(at) => at,
                None => continue,
            };
            let target = &rest[..close];
            if !target.starts_with('.') {
                continue;
            }
            // No specifier of any kind contains a space, which is the whole of what separates
            // a real one from an apostrophe in prose closing a span it never opened. Cheap
            // enough not to need a parser, and it closes the class rather than the example.
            if target.chars().any(char::is_whitespace) {
                continue;
            }
            if let Some(resolved) = join_relative(folder, target) {
                found.insert(resolved);
            }
        }
    }
    found
}

/// A relative specifier joined to the folder of whoever wrote it, or nothing when it names no
/// file of this project.
///
/// Nothing comes back in two ways, and only one of them is a climb. `../../..` from `a/b` runs
/// out of folder to pop, which is what stops a path outside the project from becoming a node
/// that could never match a file. But `..` from `a` lands exactly on the root, and a folder is
/// not a module either — that one ends with nothing left to name rather than with an underflow,
/// and is just as correctly not a node.
fn join_relative(folder: &str, target: &str) -> Option<String> {
    let mut parts: Vec<&str> = if folder.is_empty() {
        Vec::new()
    } else {
        folder.split('/').collect()
    };

    for piece in target.split('/') {
        match piece {
            "." | "" => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Whether the file says which decision it belongs to.
///
/// Today this is only *"it cites something"* — the citation carries no document identity, so
/// **which** decision is unknowable. That ambiguity is §8 of the spec and belongs to slice 6.
/// It is still worth measuring now: the difference between *declares something* and *declares
/// nothing at all* already separates the ~87 files that claim a purpose from the ~108 that
/// do not, and that split is the whole first slice.
pub fn cites_section(source: &str) -> bool {
    source.contains('§')
}

/// Whether a Rust module carries its own tests, which is where this repo puts them.
///
/// It looks for `#[cfg(test)]` and nothing else — not `#[test]`, not `mod tests`, not a
/// `tests/` directory beside the crate. That is not a shortcut but the actual convention here,
/// and looking for the other three would find files that do not exist.
///
/// Like everything else in this module it is a string search, so a `#[cfg(test)]` quoted inside
/// a string or shown in a doc-comment example counts. The cost is one module wrongly marked as
/// proved, in a file that is already talking about test configuration — visible to anyone who
/// opens it, and cheaper than the parser that would avoid it.
pub fn rust_has_tests(source: &str) -> bool {
    source.contains("#[cfg(test)]")
}

/// The path, without extension, where a TypeScript module's test would live.
///
/// This repo puts the test beside the file — `Meter.tsx` / `Meter.test.tsx`. Returning the
/// path rather than a boolean leaves the "does it exist?" question to the caller that already
/// holds the file list, so this stays a pure string operation with nothing to stub.
///
/// Handed a path that is already a test, this answers `Meter.test.test` — a file that cannot
/// exist. That is not guarded here, because the caller walking the tree has a better answer
/// than a guard would: a test is proof *about* a module and is not a module itself, so it never
/// reaches this function at all.
pub fn ts_test_sibling(path: &str) -> String {
    let stem = path
        .strip_suffix(".tsx")
        .or_else(|| path.strip_suffix(".ts"))
        .unwrap_or(path);
    format!("{stem}.test")
}

/// Folders that are never walked.
///
/// `target/` alone is 11.7 GB on this machine and nothing inside it was written by anybody.
/// `.ai/` is out because it is working material, not product.
const SKIP: &[&str] = &[
    ".git",
    ".ai",
    ".claude",
    ".agents",
    "target",
    "target-test",
    "node_modules",
    "dist",
    "build",
];

/// A file of the project, and what is known about it without asking any model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Module {
    /// Path relative to the root, always with forward slashes.
    pub path: String,
    pub reader: Reader,
    /// It cites a spec section. `false` is the *code nobody asked for* pile of §5.1.
    pub declares: bool,
    /// Something tests it.
    pub tested: bool,
}

/// One module imports another. Both ends exist — a dangling import is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Import {
    pub from: String,
    pub to: String,
}

/// The whole structure layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Structure {
    pub modules: Vec<Module>,
    pub imports: Vec<Import>,
    /// Files found that no reader here knows how to interpret (§11).
    pub unread: Vec<String>,
}

/// Whether a file is proof or a declaration *about* a module rather than a module itself.
///
/// `Meter.test.tsx` is how `Meter.tsx` proves it runs, and a `.d.ts` declares types somebody
/// else implements. Neither is a feature. Drawing them would double the shell's node count with
/// nodes that permanently declare nothing and are permanently untested — which is precisely the
/// pile §5.1 counts, so the noise would land inside the one number that matters.
///
/// Deliberately separate from [`reader_for`], which also refuses a `.d.ts`. That refusal means
/// *nobody here reads this*, and `structure` turns it into `unread`. These files are not unread:
/// nothing failed to read them. Two questions that happen to overlap on one extension are still
/// two questions, and collapsing them would make the map say "I cannot read this" about a file
/// it understands perfectly well.
fn about_a_module(path: &str) -> bool {
    path.ends_with(".d.ts") || path.ends_with(".test.ts") || path.ends_with(".test.tsx")
}

/// The first segment of a path, which is as near to *which crate is this* as a tree walk gets.
fn top_folder(path: &str) -> &str {
    path.split_once('/').map(|(head, _)| head).unwrap_or(path)
}

/// Walk the tree and assemble the structure.
///
/// Two passes, and it has to be two: an import can only be resolved once the file list is
/// known. Resolving while walking would make the answer depend on the order the filesystem
/// happened to hand back its entries, which is a graph that changes shape between reads for
/// no reason anybody could see.
pub fn structure(root: &Path) -> std::io::Result<Structure> {
    let mut files: Vec<String> = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();

    let present: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let mut modules = Vec::new();
    let mut unread = Vec::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();

    for path in &files {
        if about_a_module(path) {
            continue;
        }
        let Some(reader) = reader_for(path) else {
            unread.push(path.clone());
            continue;
        };
        let source = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        let tested = match reader {
            Reader::Rust => rust_has_tests(&source),
            Reader::Typescript => {
                let sibling = ts_test_sibling(path);
                present.contains(format!("{sibling}.ts").as_str())
                    || present.contains(format!("{sibling}.tsx").as_str())
            }
        };
        modules.push(Module {
            path: path.clone(),
            reader,
            declares: cites_section(&source),
            tested,
        });
        sources.insert(path.clone(), source);
    }

    // A Rust module is named by its file, and that is how `use crate::storage` finds
    // `core/src/storage.rs` without this module ever having to know where the crate root is.
    //
    // Keyed by top-level folder as well as by stem, because `crate::` never crosses a crate.
    // Today there is one Rust crate here and the extra key changes nothing. The day `sidecars/`
    // holds Rust, `main.rs` exists twice, and a flat map would quietly hand half the edges to
    // the wrong file — which is the one failure this module refuses everywhere else, because a
    // dropped edge is visible and a wrong one is not.
    let mut by_stem: BTreeMap<(&str, &str), &str> = BTreeMap::new();
    for module in &modules {
        if module.reader == Reader::Rust
            && let Some(stem) = module
                .path
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".rs"))
        {
            by_stem.insert((top_folder(&module.path), stem), module.path.as_str());
        }
    }

    // An edge may only land on a node the map draws. Resolving against every file on disk would
    // let an import find a test file, and an edge to something never shown is the same ghost the
    // dangling case refuses to invent — just harder to see, because one end of it is real.
    let drawn: BTreeSet<&str> = modules.iter().map(|m| m.path.as_str()).collect();

    let mut imports = Vec::new();
    for module in &modules {
        let source = sources.get(&module.path).map(String::as_str).unwrap_or("");
        match module.reader {
            Reader::Rust => {
                for name in rust_imports(source) {
                    if let Some(target) = by_stem.get(&(top_folder(&module.path), name.as_str()))
                        && *target != module.path
                    {
                        imports.push(Import {
                            from: module.path.clone(),
                            to: (*target).into(),
                        });
                    }
                }
            }
            Reader::Typescript => {
                for stem in ts_imports(&module.path, source) {
                    // The extension resolves only here, where the file list exists.
                    let candidates = [
                        format!("{stem}.ts"),
                        format!("{stem}.tsx"),
                        format!("{stem}/index.ts"),
                        format!("{stem}/index.tsx"),
                    ];
                    if let Some(target) = candidates.iter().find(|c| drawn.contains(c.as_str()))
                        && *target != module.path
                    {
                        imports.push(Import {
                            from: module.path.clone(),
                            to: target.clone(),
                        });
                    }
                }
            }
        }
    }
    imports.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    imports.dedup();

    Ok(Structure {
        modules,
        imports,
        unread,
    })
}

/// Every file below `dir`, by path relative to `root`.
fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if SKIP.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            collect(root, &path, out)?;
            continue;
        }
        // The same rule as for folders, and for the same reason. An `.eslintrc.ts` is
        // configuration, not a module, and counting it would drop it straight into the pile of
        // things that declare nothing — which is the one number this map exists to report.
        if name.starts_with('.') {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A toy tree, so no test depends on the shape of the real repository.
    ///
    /// Keyed by process as well as by name, the way `transcribe.rs` already keys its temp
    /// paths. This deletes before it creates, so two suites running at once on one machine
    /// would delete each other's fixtures mid-test and fail for a reason neither contains.
    fn scratch(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("nucleos-map-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("scratch");
        root
    }

    fn write(root: &std::path::Path, rel: &str, body: &str) {
        let full = root.join(rel);
        fs::create_dir_all(full.parent().expect("parent")).expect("dirs");
        fs::write(full, body).expect("write");
    }

    #[test]
    fn a_rust_file_and_a_typescript_file_are_read_by_different_readers() {
        assert_eq!(reader_for("core/src/agent.rs"), Some(Reader::Rust));
        assert_eq!(
            reader_for("shell/src/ui/Button.tsx"),
            Some(Reader::Typescript)
        );
        assert_eq!(
            reader_for("shell/src/data/keys.ts"),
            Some(Reader::Typescript)
        );
    }

    #[test]
    fn a_language_this_cannot_read_is_reported_and_never_guessed_at() {
        // The sidecars are Go and this slice does not read Go. Returning `None` is what puts
        // them in `unread` rather than making them disappear from the map — §11.
        assert_eq!(reader_for("sidecars/echo/main.go"), None);
        assert_eq!(reader_for("README.md"), None);
    }

    #[test]
    fn a_declaration_file_is_not_a_module() {
        // `vite-env.d.ts` is nobody's code; counting it would put a permanently orphaned node
        // on the map, which is noise that never resolves.
        assert_eq!(reader_for("shell/src/vite-env.d.ts"), None);
    }

    #[test]
    fn a_rust_module_imports_what_it_names_after_use_crate() {
        let source = r#"
use std::collections::BTreeMap;
use crate::storage;
use crate::token_efficiency::Baseline;
use crate::{budget, health};
"#;
        let found = rust_imports(source);
        assert!(found.contains("storage"));
        assert!(found.contains("token_efficiency"));
        assert!(found.contains("budget"));
        assert!(found.contains("health"));
        // `std` is not a module of this project and is not a node on the map.
        assert!(!found.contains("collections"));
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn a_crate_reference_inside_a_line_of_code_counts_too() {
        // `http.rs` calls `crate::project_readings::readings(...)` without ever writing `use`.
        // Ignoring that would leave every route without an edge to the module it serves.
        let source = "crate::project_readings::readings(&state.pool, &id).await";
        let found = rust_imports(source);
        assert!(found.contains("project_readings"));
    }

    #[test]
    fn a_module_named_more_than_once_still_appears_once() {
        let source = "use crate::storage;\nuse crate::storage::Thing;";
        assert_eq!(rust_imports(source).len(), 1);
    }

    #[test]
    fn a_glob_import_names_no_single_module_and_so_draws_no_edge() {
        // `use crate::*;` names everything and therefore nothing in particular. Expanding it
        // into an edge to every module in the crate would bury the graph under a fan that says
        // less than no edge at all does. Recorded here so the silence is a decision, not a gap.
        assert!(rust_imports("use crate::*;\n").is_empty());
    }

    #[test]
    fn a_typescript_module_resolves_a_relative_import_against_its_own_folder() {
        let source = r#"
import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { GraphNode } from "../data/workflow-graph";
"#;
        let found = ts_imports("shell/src/canvas/map-model.ts", source);
        // A package from node_modules is not a module of this project.
        assert!(!found.iter().any(|p| p.contains("react-query")));
        assert!(found.contains("shell/src/canvas/client"));
        assert!(found.contains("shell/src/data/workflow-graph"));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_parent_hop_cannot_climb_out_of_the_project() {
        // `../../../../elsewhere` is not a node on the map. Climbing out of the root returns
        // nothing rather than a path that could never match a module anyway.
        let found = ts_imports(
            "shell/src/main.tsx",
            "import x from \"../../../../elsewhere\";",
        );
        assert!(found.is_empty());
    }

    #[test]
    fn a_single_quoted_specifier_is_read_like_a_double_quoted_one() {
        // The shell writes both forms. A map that only saw one would be missing edges for a
        // reason no one looking at it could ever guess.
        let found = ts_imports(
            "shell/src/project/Workspace.tsx",
            "import x from './ModeMapa';",
        );
        assert!(found.contains("shell/src/project/ModeMapa"));
    }

    #[test]
    fn an_apostrophe_in_prose_never_becomes_a_path() {
        // An apostrophe with no partner turns the next one into a closing quote, and the
        // sentence caught between them starts with a dot by accident.
        let source = "// the students'./project is done, ask the teachers' opinion";
        assert!(ts_imports("shell/src/main.tsx", source).is_empty());
    }

    #[test]
    fn a_file_that_cites_a_section_declares_what_it_implements() {
        assert!(cites_section(
            "//! §6.4, and the argument for it is what the reader gets"
        ));
        assert!(cites_section(
            "/// Reserved by §14 and drawn by the inspector"
        ));
    }

    #[test]
    fn a_file_that_cites_nothing_is_the_pile_that_nobody_asked_for() {
        // This is not a defect of the file. It is the `code nobody asked for` category of
        // §5.1, and it is half the reason the map exists at all.
        assert!(!cites_section("use crate::storage;\n\npub fn thing() {}"));
    }

    #[test]
    fn a_rust_module_proves_itself_with_a_test_module_inside_it() {
        assert!(rust_has_tests(
            "#[cfg(test)]\nmod tests {\n    use super::*;\n}"
        ));
        assert!(!rust_has_tests("pub fn untested() {}"));
    }

    #[test]
    fn a_typescript_module_is_proved_by_the_sibling_beside_it() {
        assert_eq!(
            ts_test_sibling("shell/src/ui/Meter.tsx"),
            "shell/src/ui/Meter.test"
        );
        assert_eq!(
            ts_test_sibling("shell/src/data/keys.ts"),
            "shell/src/data/keys.test"
        );
    }

    #[test]
    fn the_structure_joins_a_module_to_the_one_it_imports() {
        let root = scratch("joins");
        write(
            &root,
            "core/src/a.rs",
            "//! §1 alfa\nuse crate::b;\n#[cfg(test)]\nmod t {}",
        );
        write(&root, "core/src/b.rs", "pub fn b() {}");

        let found = structure(&root).expect("structure");

        let a = found
            .modules
            .iter()
            .find(|m| m.path == "core/src/a.rs")
            .expect("a");
        assert!(a.declares, "it cites a section, so it declares something");
        assert!(a.tested, "it has cfg(test), so something proves it runs");

        let b = found
            .modules
            .iter()
            .find(|m| m.path == "core/src/b.rs")
            .expect("b");
        assert!(
            !b.declares,
            "it cites nothing — this is the code nobody asked for"
        );
        assert!(!b.tested);

        assert!(
            found
                .imports
                .iter()
                .any(|i| i.from == "core/src/a.rs" && i.to == "core/src/b.rs")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_import_of_something_that_is_not_there_is_dropped_rather_than_invented() {
        // `use crate::storage` in a project with no `storage.rs` must not become a ghost node:
        // a node with no file behind it is indistinguishable from a decision with no code, and
        // those two say exactly the opposite thing about the project.
        let root = scratch("dangling");
        write(&root, "core/src/a.rs", "use crate::nowhere;");

        let found = structure(&root).expect("structure");
        // Both halves, or the test proves nothing: an empty module list also has no imports,
        // and that is the opposite outcome wearing the same assertion.
        assert_eq!(found.modules.len(), 1);
        assert_eq!(found.modules[0].path, "core/src/a.rs");
        assert!(found.imports.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_language_nobody_reads_is_listed_instead_of_being_dropped() {
        let root = scratch("unread");
        write(&root, "sidecars/echo/main.go", "package main");

        let found = structure(&root).expect("structure");
        assert!(found.modules.is_empty());
        assert_eq!(found.unread, vec!["sidecars/echo/main.go".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_build_folder_is_never_walked() {
        // `target/` is 11.7 GB on this machine and none of it was written by anybody.
        let root = scratch("skips");
        write(&root, "target/debug/build.rs", "fn main() {}");
        write(&root, "node_modules/x/index.ts", "export const x = 1;");

        let found = structure(&root).expect("structure");
        assert!(found.modules.is_empty());
        assert!(found.unread.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_test_beside_a_module_is_proof_of_it_and_never_a_module_itself() {
        // `Meter.test.tsx` is how `Meter.tsx` proves it runs. Drawing it as its own node would
        // double the shell's count with nodes that permanently declare nothing — putting the
        // noise inside the one number this map exists to report.
        let root = scratch("proof");
        write(
            &root,
            "shell/src/ui/Meter.tsx",
            "export const Meter = () => null;",
        );
        write(
            &root,
            "shell/src/ui/Meter.test.tsx",
            "import { Meter } from './Meter';",
        );

        let found = structure(&root).expect("structure");

        assert_eq!(found.modules.len(), 1);
        assert_eq!(found.modules[0].path, "shell/src/ui/Meter.tsx");
        assert!(
            found.modules[0].tested,
            "the sibling beside it is the proof"
        );
        assert!(
            found.unread.is_empty(),
            "nothing failed to read it — it is just not a module"
        );
        assert!(
            found.imports.is_empty(),
            "the only import came from a file that is not a node"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dotfile_is_configuration_and_never_a_module() {
        // A folder starting with a dot is already skipped. A file starting with one was not,
        // so an `.eslintrc.ts` walked straight into the pile of things that declare nothing —
        // the single number this whole map exists to report.
        let root = scratch("dotfile");
        write(&root, "shell/.eslintrc.ts", "export default {};");

        let found = structure(&root).expect("structure");
        assert!(found.modules.is_empty());
        assert!(found.unread.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn two_files_with_the_same_name_in_different_trees_never_share_an_edge() {
        // `main.rs` exists in every crate there has ever been. A flat index of file stems lets
        // one tree's import land on another tree's file, and an edge pointing at the wrong
        // module is worse than no edge: nothing about it looks wrong.
        let root = scratch("stems");
        write(&root, "core/src/a.rs", "use crate::shared;");
        write(&root, "core/src/shared.rs", "pub fn s() {}");
        write(&root, "sidecars/tool/shared.rs", "pub fn s() {}");

        let found = structure(&root).expect("structure");

        assert_eq!(found.imports.len(), 1);
        assert_eq!(found.imports[0].from, "core/src/a.rs");
        assert_eq!(found.imports[0].to, "core/src/shared.rs");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_declaration_file_is_neither_a_module_nor_unread() {
        // `reader_for` already refuses it, but refusing it there would land it in `unread` —
        // and nothing failed to read it. Saying "I cannot read this" about a file this reader
        // understands perfectly well is the one thing §11 exists to prevent.
        let root = scratch("declaration");
        write(
            &root,
            "shell/src/vite-env.d.ts",
            "declare const injected: number;",
        );

        let found = structure(&root).expect("structure");
        assert!(found.modules.is_empty());
        assert!(found.unread.is_empty());

        let _ = fs::remove_dir_all(&root);
    }
}
