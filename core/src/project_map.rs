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
use std::collections::BTreeSet;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
