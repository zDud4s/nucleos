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
//!
//! Not reading a language is still not an excuse to lose what it said out loud. A file nobody
//! here can interpret may still name a `§`, and [`Structure::foreign`] carries those sections
//! without promoting the file to a module — because *I cannot read this, and it claims §9* and
//! *nothing in this repository claims §9* are opposite answers, and only one of them is true of
//! the 77 Go files that name a section.

use crate::map_join::{Citation, citations};
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
///
/// **A `§spec` header counts as the gesture, and that is the right answer rather than a leak.**
/// The marker [`crate::map_join::declaration`] reads is built on the same sign this searches for,
/// so a file that declares its document without citing a numbered section anywhere comes back
/// `declares: true, cites: []` — the shape this doc's neighbour on [`Module::cites`] already
/// describes for a bare `§`, and the honest one: a file saying *my sections belong to that
/// document* has gestured at a section even if it never numbered one. Nothing in this tree is in
/// that state, since a header is only worth writing above citations.
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

/// Extensions read for citations alone, by a map that cannot read the language itself.
///
/// **An allowlist and not "whatever landed in `unread`"**, and the difference is the whole
/// safety of it. `unread` is every file this map could not interpret, which in this tree includes
/// a `.woff2`, an `.onnx` and an `.icns` — all three of which match `§` as a raw byte pattern
/// without containing a citation, or text, at all. `read_to_string` fails on them and
/// `unwrap_or_default()` would swallow that failure into an empty string, so a denylist would be
/// correct here by luck rather than by design. Naming the extensions makes the binaries
/// unreachable instead of merely harmless.
///
/// **`.md` is absent on purpose, and must stay absent.** A design spec cites its own sections
/// constantly — a heading *is* a citation — so reading specs for citations would report every
/// decision as claimed by the very document that decided it. That is not a performance choice; it
/// is the difference between a map and a mirror. The specs in `.ai/` are already out of the walk,
/// so what this actually keeps out today is `AGENTS.md`, `THREAT_MODEL.md` and their kin: prose
/// *about* the code, citing sections it discusses rather than implements. Both readings say the
/// same thing — a document that talks about a section is not code that implements one.
const FOREIGN: &[&str] = &[
    ".go", ".sql", ".css", ".sh", ".py", ".toml", ".yaml", ".yml", ".js", ".mjs", ".cjs", ".jsx",
];

/// Whether a file nobody reads is still worth scanning for the sections it names.
fn foreign_source(path: &str) -> bool {
    FOREIGN.iter().any(|extension| path.ends_with(extension))
}

/// Whether this map reads a § out of this path at all.
///
/// **One predicate for the two callers that have to agree, and they are a walk apart.**
/// [`citing_files`] applies it to the working tree; [`crate::map_orphan`] applies it to paths git
/// prints out of the history, where no file exists to read. Spelling the rule twice would let the
/// guard report a loss in a file the structure layer never looked at — a claim about a language
/// this map does not scan, arriving under the one word (`Lost`) that is supposed to mean
/// something definite.
pub fn scanned(path: &str) -> bool {
    reader_for(path).is_some() || foreign_source(path)
}

/// A file of the project, and what is known about it without asking any model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Module {
    /// Path relative to the root, always with forward slashes.
    pub path: String,
    pub reader: Reader,
    /// It cites a spec section. `false` is the *code nobody asked for* pile of §5.1.
    pub declares: bool,
    /// The sections this module names, at full resolution — **including the ones only its test
    /// names**.
    ///
    /// **Deliberately alongside `declares` and not instead of it**, and the two can disagree on
    /// purpose. `declares` is *this file gestures at a section*; `cites` is *these are the sections
    /// it names*. A file holding a bare `§` with no number is `declares: true, cites: []` — and that
    /// difference is a fact worth being able to count, not a bug to normalize away. Since the test
    /// sibling is folded in, the disagreement also runs the other way: a module that cites nothing
    /// itself but whose test names `§9.2` is `declares: false, cites: [9.2]`. `declares` stays the
    /// file's own gesture, because it is what the §5.1 *code nobody asked for* count is built on
    /// and quietly widening that count would change a shipped number without saying so.
    ///
    /// **A test's citation is its module's claim, and the symmetry is the reason.** A Rust module
    /// keeps its tests in the same file, so a `§` inside `#[cfg(test)]` has always landed here for
    /// free; the convention for TypeScript puts them in a sibling, so without reading it the same
    /// declaration would answer differently in the two languages. That is not a property of the
    /// decision being declared — it is a property of where each language happens to keep its
    /// tests, and a map that reports it as a difference about the code is wrong about the code.
    /// Whoever later reads the sibling read as a special case for TypeScript and removes it should
    /// know it is the opposite: it is what stops one.
    ///
    /// **And a file-level anchor declaration reopens that asymmetry on a new axis, which is stated
    /// here because the reader alone cannot close it.** [`crate::map_join::declaration`] is a
    /// property of the `&str` it is read from, so a `§spec` header governs its own file and no
    /// other. A Rust module gets the right answer for free — its tests are in the same file, under
    /// the same header — while a TypeScript module's sibling test is a *different* file, and its
    /// citations fold in still bare however carefully the module declared. The symmetry this field
    /// exists to keep would then hold for the section numbers and break for the documents.
    ///
    /// The fix is not code and must not become code: the sweep that writes the headers writes one
    /// into `Fleet.test.tsx` too, since a test file is a file and the marker costs it one comment
    /// line. Passing the module's declaration down into the sibling's read would mean a second
    /// entry point taking a default, which is the second mechanism [`crate::map_join::citations`]
    /// refuses to grow. **Measured, so the size of the gap is known rather than feared:** modules
    /// here name 76 distinct sections and test files 14, of which exactly one — `§9.2` in
    /// `shell/src/pages/Fleet.test.tsx` — is named by no module at all. The other 13 arrive twice,
    /// and [`crate::map_join::evidence`] takes the strongest of a file's rows, so the module's own
    /// declared citation wins and the bare copy costs nothing.
    ///
    /// The alternative — crediting the citation to the test file as a node of its own — is the one
    /// [`about_a_module`] already refuses, and for a reason that has not changed: it would double
    /// the shell's node count with nodes that permanently declare nothing and are permanently
    /// untested, which is noise inside the single number this map exists to report.
    ///
    /// **Small today and stated as a number, because an adjective would age worse.** Across this
    /// tree modules name 76 distinct sections and test files name 14, of which exactly one — `§9.2`
    /// in `shell/src/pages/Fleet.test.tsx` — is named by no module at all. One section is a thin
    /// reason to write code; the gap growing silently every time somebody tests what they named,
    /// with nothing that would ever announce it, is not.
    pub cites: Vec<Citation>,
    /// Something tests it.
    pub tested: bool,
}

/// One module imports another. Both ends exist — a dangling import is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Import {
    pub from: String,
    pub to: String,
}

/// A file that names a section in a language no reader here understands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Foreign {
    pub path: String,
    pub cites: Vec<Citation>,
}

/// The whole structure layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Structure {
    pub modules: Vec<Module>,
    pub imports: Vec<Import>,
    /// Files found that no reader here knows how to interpret (§11).
    pub unread: Vec<String>,
    /// Sections named by files no reader here understands. **Not modules**: nothing here can say
    /// what a Go file imports or whether anything tests it, and calling one a module would be the
    /// collapse this map refuses everywhere else.
    ///
    /// Kept at all because without them the junction's *declared, with no code* is a lie. 77 Go
    /// files in `sidecars/` name a `§`, and a decision one of them implements would otherwise be
    /// reported as unclaimed — a confident wrong answer about a whole language.
    pub foreign: Vec<Foreign>,
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
    let mut foreign = Vec::new();
    let mut sources: BTreeMap<String, String> = BTreeMap::new();

    for path in &files {
        if about_a_module(path) {
            continue;
        }
        let Some(reader) = reader_for(path) else {
            // `unread` counts every file nobody read, exactly as it did before this list existed,
            // and `foreign` is an extra reading of a subset — never a filter on it. Moving a Go
            // file out of `unread` because its sections were recovered would shrink the count §11
            // reports, which is the one number that says what this map cannot see.
            unread.push(path.clone());
            if foreign_source(path) {
                let source = std::fs::read_to_string(root.join(path)).unwrap_or_default();
                let cites: Vec<Citation> = citations(&source).into_iter().collect();
                // An entry with no citation is a row that says nothing the `unread` line beside
                // it does not already say.
                if !cites.is_empty() {
                    foreign.push(Foreign {
                        path: path.clone(),
                        cites,
                    });
                }
            }
            continue;
        };
        let source = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        let mut cites = citations(&source);
        let tested = match reader {
            Reader::Rust => rust_has_tests(&source),
            Reader::Typescript => {
                let stem = ts_test_sibling(path);
                let proof = [format!("{stem}.ts"), format!("{stem}.tsx")]
                    .into_iter()
                    .find(|candidate| present.contains(candidate.as_str()));
                // The sibling's sections are this module's claim — see `Module::cites`. A Rust
                // module gets this free because its tests share its file; doing it here is what
                // keeps the two languages answering the same question.
                //
                // Merged as sets and collected once, so `§4` named by both files is one row and
                // the order is the section order rather than the order the walk happened to
                // reach the two files in. `files` is sorted for exactly that reason, and reading
                // a second file per module must not be the thing that reintroduces the wobble.
                if let Some(proof) = &proof {
                    let proved_by = std::fs::read_to_string(root.join(proof)).unwrap_or_default();
                    cites.extend(citations(&proved_by));
                }
                proof.is_some()
            }
        };
        modules.push(Module {
            path: path.clone(),
            reader,
            declares: cites_section(&source),
            cites: cites.into_iter().collect(),
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
        foreign,
    })
}

/// Every file this map reads a `§` out of, which is deliberately **not** [`Structure::modules`].
///
/// Three groups, and the two a sweep over the modules alone loses in silence are the second and
/// the third:
///
/// 1. **The modules** — whatever [`reader_for`] names.
/// 2. **Their test siblings.** `Fleet.test.tsx` is [`about_a_module`], so [`structure`] never
///    makes it a module — and folds its citations into `Fleet.tsx`'s [`Module::cites`] all the
///    same, which is what keeps the two languages answering one question. But a declaration is
///    read out of the file it was written in, so a header on `Fleet.tsx` governs nothing the
///    sibling wrote: sweep the modules alone and every TypeScript module comes out
///    half-declared, with nothing on screen saying why. That is the asymmetry `Module::cites`
///    exists to prevent, reopened one level up. [`reader_for`] says yes to a `.test.tsx` — it is
///    [`structure`] that subtracts them — so they arrive here for free, and this paragraph is
///    here to say that the subtraction must not be copied.
/// 3. **The foreign files** — [`Structure::foreign`], whose citations reach the junction exactly
///    as a module's do. 77 Go files under `sidecars/` name a `§`, and a header is worth as much
///    on one of those as anywhere else.
///
/// A `.d.ts` is in none of the three, which is the second group's argument in reverse: nothing
/// folds its citations anywhere, so a header written on one would govern nothing.
///
/// **A file naming no `§` at all is left out**, by [`cites_section`]'s test rather than by a
/// second one, so this list and [`Module::declares`] cannot drift apart about what counts as
/// naming a section. A file that cannot be read is left out too, and reads here as naming
/// nothing — the same answer [`structure`] gives it, where an unreadable file becomes an empty
/// source rather than a failed walk.
///
/// **Paths and not sources**, so the caller reads each file when it gets to it. A list of 211
/// file bodies held at once to save a second `read_to_string` is memory spent on a walk that
/// happens once per sweep.
// Not reached from `main` yet, and the two halves of that are in two different tasks: the
// sweep that walks this list is the harness in `map_anchor`'s tests, and the applier that
// acts on what it proposes is the next commit. Scoped to the non-test build exactly as
// `map_anchor`'s own crate-level suppression is, so the lint stays live under `cfg(test)`,
// where the test below exercises every branch of it. The instruction, not a description:
// DELETE THESE TWO LINES with the change that gives this a production caller.
#[cfg_attr(not(test), allow(dead_code))]
pub fn citing_files(root: &Path) -> std::io::Result<Vec<String>> {
    let mut files: Vec<String> = Vec::new();
    collect(root, root, &mut files)?;
    files.sort();
    files.retain(|path| {
        scanned(path)
            && std::fs::read_to_string(root.join(path)).is_ok_and(|source| cites_section(&source))
    });
    Ok(files)
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

    #[test]
    fn a_module_that_names_a_section_carries_it() {
        let root = scratch("cites");
        write(
            &root,
            "core/src/a.rs",
            // `§7,` and not `§7 is`: a lowercase word after the number is a slug
            // *candidate* whatever it means in English, so `is` would come back as
            // `Some("is")`. That is `map_join`'s decision and not a defect — it hands the
            // join every candidate rather than the ones it liked the look of — but it is
            // not what this test is about.
            "//! §6.4 workspace-de-projeto — four kinds\n/// and §7, the other one\n",
        );

        let found = structure(&root).expect("structure");
        let a = found
            .modules
            .iter()
            .find(|m| m.path == "core/src/a.rs")
            .expect("a");

        assert!(a.declares, "it names sections, so it gestures at one");
        assert_eq!(
            a.cites,
            vec![
                Citation {
                    section: "6.4".to_string(),
                    named: Some("workspace-de-projeto".to_string()),
                },
                Citation {
                    section: "7".to_string(),
                    named: None,
                },
            ],
            "both sections, at the resolution the join needs, in the set's own order"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bare_paragraph_mark_declares_without_citing_anything() {
        // `declares` and `cites` are two questions and they may honestly disagree. A file holding
        // a `§` with no number after it gestures at a section without naming one, and that is a
        // fact worth being able to count rather than a disagreement to normalize away: it is the
        // difference between a file that forgot the number and one that never claimed anything.
        let root = scratch("bare");
        write(&root, "core/src/a.rs", "//! the § symbol, and no number\n");

        let found = structure(&root).expect("structure");
        let a = found
            .modules
            .iter()
            .find(|m| m.path == "core/src/a.rs")
            .expect("a");

        assert!(a.declares, "the sign is there");
        assert!(a.cites.is_empty(), "and it names nothing");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_go_file_naming_a_section_is_foreign_and_not_a_module() {
        // 77 Go files in `sidecars/` name a section. Without collecting them a decision one of
        // them implements is reported as having no code at all — a confident wrong answer about
        // a whole language, which is the one failure this map refuses everywhere else.
        let root = scratch("foreign-go");
        write(
            &root,
            "sidecars/echo/main.go",
            "// §9 echo — the sidecar contract\npackage main\n",
        );

        let found = structure(&root).expect("structure");

        assert!(
            found.modules.is_empty(),
            "nothing here can say what a Go file imports, so it is not a module"
        );
        assert_eq!(
            found.unread,
            vec!["sidecars/echo/main.go".to_string()],
            "`foreign` answers a different question and must not shrink this count"
        );
        assert_eq!(found.foreign.len(), 1);
        assert_eq!(found.foreign[0].path, "sidecars/echo/main.go");
        assert_eq!(
            found.foreign[0].cites,
            vec![Citation {
                section: "9".to_string(),
                named: Some("echo".to_string()),
            }]
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_spec_is_never_read_for_citations_because_it_cites_itself() {
        // A design document cites its own sections on every heading. Reading one for citations
        // would report every decision as claimed by the very document that decided it — a map
        // that has become a mirror. Still `unread`, because it is still a file nobody read.
        let root = scratch("spec");
        write(
            &root,
            "docs/design.md",
            r###"# Design

## §4.1 A decisão
§4.1 is decided here, and §6.4 workspace-de-projeto follows from it.

## §7 A outra
§7 too.
"###,
        );

        let found = structure(&root).expect("structure");

        assert!(
            found.foreign.is_empty(),
            "a document that cites itself claims nothing"
        );
        assert_eq!(found.unread, vec!["docs/design.md".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_foreign_file_with_no_citation_is_not_listed() {
        // An entry with an empty `cites` is a row that says nothing. `unread` already counts the
        // file; repeating it here with nothing attached is noise in the one list whose whole
        // purpose is to carry sections.
        let root = scratch("foreign-silent");
        write(&root, "sidecars/echo/quiet.go", "package main\n");

        let found = structure(&root).expect("structure");

        assert_eq!(found.unread, vec!["sidecars/echo/quiet.go".to_string()]);
        assert!(found.foreign.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_typescript_module_is_credited_with_what_its_test_names() {
        // The sibling names a section the module itself never does. Crediting it to the module
        // is what a Rust module already gets for free, and the alternative — a node for the test
        // file — is the one `about_a_module` refuses.
        let root = scratch("credited-ts");
        write(
            &root,
            "shell/src/pages/Fleet.tsx",
            "//! §4, the fleet page
",
        );
        write(
            &root,
            "shell/src/pages/Fleet.test.tsx",
            "// §9.2, and §4, both named here
import { Fleet } from './Fleet';
",
        );

        let found = structure(&root).expect("structure");
        let fleet = found
            .modules
            .iter()
            .find(|m| m.path == "shell/src/pages/Fleet.tsx")
            .expect("fleet");

        assert!(fleet.tested, "the sibling beside it is the proof");
        assert_eq!(
            fleet.cites,
            vec![
                Citation {
                    section: "4".to_string(),
                    named: None,
                },
                Citation {
                    section: "9.2".to_string(),
                    named: None,
                },
            ],
            "§4 once and not twice, and sorted — not in the order the walk reached the two files"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rust_module_is_credited_with_what_its_own_test_module_names() {
        // The symmetry the TypeScript merge exists to restore, asserted rather than assumed.
        // Rust puts its tests in the same file, so `citations` has always picked these up and
        // nothing defended that. If this ever stops being true the merge next door becomes a
        // special case for one language, which is precisely what it must never be.
        let root = scratch("credited-rs");
        write(
            &root,
            "core/src/a.rs",
            "pub fn a() {}

#[cfg(test)]
mod tests {
    // §9.2, proved right here
}
",
        );

        let found = structure(&root).expect("structure");
        let a = found
            .modules
            .iter()
            .find(|m| m.path == "core/src/a.rs")
            .expect("a");

        assert!(a.tested);
        assert_eq!(
            a.cites,
            vec![Citation {
                section: "9.2".to_string(),
                named: None,
            }],
            "a section named only inside `#[cfg(test)]` is still this module's claim"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_test_file_is_credited_to_its_module_and_appears_in_no_list_itself() {
        // Being credited is not being drawn. The citation moves to the module; the file stays
        // absent from every list this map returns — a node for it would permanently declare
        // nothing and be permanently untested, landing noise inside the §5.1 count.
        let root = scratch("credited-nowhere");
        write(
            &root,
            "shell/src/pages/Fleet.tsx",
            "export const Fleet = 1;
",
        );
        write(
            &root,
            "shell/src/pages/Fleet.test.tsx",
            "// §9.2, named only by the proof
",
        );

        let found = structure(&root).expect("structure");

        assert_eq!(found.modules.len(), 1);
        assert_eq!(found.modules[0].path, "shell/src/pages/Fleet.tsx");
        assert_eq!(
            found.modules[0].cites,
            vec![Citation {
                section: "9.2".to_string(),
                named: None,
            }]
        );
        assert!(
            !found.modules[0].declares,
            "`declares` stays the file's own gesture, and the file itself gestured at nothing"
        );
        assert!(
            found.unread.is_empty(),
            "nothing failed to read it — it is proof about a module"
        );
        assert!(
            found.foreign.is_empty(),
            "and it is no foreign language either"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// The document the `§spec` fixtures below declare, and **this repository does not have it.**
    ///
    /// `citations` is deliberately not a parser, so a `§spec` line written inside a string literal
    /// here would be read as a declaration of *this file* when the map walks this tree — and this
    /// file carries citations of its own. A real slug would hand them all that document and move
    /// the junction's counts on the one commit whose safety argument is that they cannot move.
    ///
    /// **Which is why the fixtures interpolate this constant instead of spelling the marker out**:
    /// the source text then never holds a whole declaration. The fictional slug is the second
    /// defence and the durable one, since the next fixture written as a plain literal loses the
    /// first without anything saying so. Named so that anyone who meets it on a real map reads
    /// what it is instead of going looking for the document.
    const FIXTURE_SLUG: &str = "documento-de-fixture";

    #[test]
    fn a_module_that_declares_its_spec_hands_the_slug_to_every_bare_citation() {
        // §8's *quem declara a âncora é o código*, read off a file by the real walk rather than by
        // calling the lexer directly. The declaration has to survive `structure`'s read for the
        // junction ever to see it, and a unit test of `citations` alone passes just as happily
        // with that wiring absent.
        let root = scratch("declared");
        write(
            &root,
            "core/src/a.rs",
            &format!(
                "//! §spec {FIXTURE_SLUG}\n\
                 //! what §7, together with §9.2, is for\n\
                 /// and §6.4 workspace-de-projeto, which is decided elsewhere\n"
            ),
        );

        let found = structure(&root).expect("structure");
        let a = found
            .modules
            .iter()
            .find(|m| m.path == "core/src/a.rs")
            .expect("a");

        assert_eq!(
            a.cites,
            vec![
                Citation {
                    section: "6.4".to_string(),
                    named: Some("workspace-de-projeto".to_string()),
                },
                Citation {
                    section: "7".to_string(),
                    named: Some(FIXTURE_SLUG.to_string()),
                },
                Citation {
                    section: "9.2".to_string(),
                    named: Some(FIXTURE_SLUG.to_string()),
                },
            ],
            "the header is the default, and the one citation that wrote its own slug keeps it"
        );
        assert!(
            a.declares,
            "the sign is there, in the header as much as anywhere"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_declaration_governs_its_own_file_and_not_the_sibling_test_that_proves_it() {
        // **The asymmetry `Module::cites` warns about, pinned so that it can only change on
        // purpose.** A declaration is a property of the text it was read from, so a Rust module
        // covers its own `#[cfg(test)]` for free and a TypeScript module does not cover a sibling
        // in another file. The sibling's `§9.2` folds in as this module's claim — that part is
        // unchanged and is what stops the two languages answering differently — and it folds in
        // bare.
        //
        // The fix belongs to the sweep that writes the headers, which writes one into the test
        // file too: a test file is a file, and the marker costs it one comment line. Teaching
        // `structure` to pass the module's declaration down instead would mean a second entry
        // point into `citations` taking a default, and one question would have two answers.
        let root = scratch("declared-sibling");
        write(
            &root,
            "shell/src/pages/Fleet.tsx",
            &format!("// §spec {FIXTURE_SLUG}\n// the page, and what §7, exactly, is for\n"),
        );
        write(
            &root,
            "shell/src/pages/Fleet.test.tsx",
            "// §9.2, named only by the proof\n",
        );

        let found = structure(&root).expect("structure");

        assert_eq!(found.modules.len(), 1);
        assert_eq!(
            found.modules[0].cites,
            vec![
                Citation {
                    section: "7".to_string(),
                    named: Some(FIXTURE_SLUG.to_string()),
                },
                Citation {
                    section: "9.2".to_string(),
                    named: None,
                },
            ],
            "the header reached its own file's citation and not the sibling's"
        );

        let _ = fs::remove_dir_all(&root);
    }
    #[test]
    fn the_files_a_sweep_asks_about_are_not_the_modules_it_draws() {
        // The list a disambiguating sweep has to walk is wider than `modules` in two directions,
        // and both are easy to miss because `structure` is the obvious thing to iterate over.
        //
        // The fixtures below name only sections this file already cites, deliberately: a `§` in a
        // literal here is a citation of THIS module — `citations` is not a parser — so a fixture
        // inventing a new number would put this file into the anchor set of every document that
        // has one, and move a junction count from a test.
        let root = scratch("citing");
        write(
            &root,
            "shell/src/fleet/Fleet.tsx",
            "// what §5.1 asks for\n",
        );
        // Never a module — `about_a_module` subtracts it — and yet its citations are counted as
        // `Fleet.tsx`'s. A declaration would have to be written in ITS text, so leaving it out of
        // the sweep leaves the module it proves half-declared.
        write(
            &root,
            "shell/src/fleet/Fleet.test.tsx",
            "// and §7 is what the test pins\n",
        );
        write(
            &root,
            "sidecars/echo/main.go",
            "// §9 echo — the sidecar contract\npackage main\n",
        );
        // Read by nobody and folded into nothing, so a header here would govern no citation at all.
        write(&root, "shell/src/data/wire.d.ts", "// §4 is mentioned\n");
        // Names no section, so it is a question worth nobody's money.
        write(&root, "core/src/quiet.rs", "//! nothing is claimed here\n");

        let found = structure(&root).expect("structure");
        assert_eq!(
            found
                .modules
                .iter()
                .map(|module| module.path.as_str())
                .collect::<Vec<_>>(),
            vec!["core/src/quiet.rs", "shell/src/fleet/Fleet.tsx"],
            "the drawn map is the narrower list, which is the whole reason for this function"
        );

        assert_eq!(
            citing_files(&root).expect("citing files"),
            vec![
                "shell/src/fleet/Fleet.test.tsx".to_string(),
                "shell/src/fleet/Fleet.tsx".to_string(),
                "sidecars/echo/main.go".to_string(),
            ]
        );

        let _ = fs::remove_dir_all(&root);
    }
}
