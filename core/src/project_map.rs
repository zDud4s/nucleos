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
}
