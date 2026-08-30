// §spec mapa-do-projeto
//! The seam between the núcleo and the shell, read from both sides.
//!
//! **§16.4 gives L0 the question *where is the boundary?*, and until this existed the map answered
//! it with an absence.** The three sides of this product share no source file: nothing in `core/`
//! imports anything in `shell/`, and nothing could — a Rust file cannot import a TypeScript module
//! and the import resolver refuses to look outside a folder. Drawn as boxes with no line between
//! them that reads as independence, which is not what was measured. The boundary is real, it is
//! HTTP, and this module is the map learning to read it.
//!
//! Two lists, taken from opposite ends and compared:
//!
//! | Measured over this repository, 2026-08-30 | |
//! |---|---:|
//! | routes the daemon registers | **208** |
//! | call sites in the shell | 229 |
//! | calls that match a served route | 223 |
//! | calls whose path is partly built at run time | 4 |
//! | **calls matching no route at all** | **0** |
//! | call sites handing in a path from elsewhere | 2 |
//! | served routes no screen calls | 24 |
//!
//! **Zero is the answer worth having, and it is only worth having because it can change.** A shell
//! calling `GET /projects/{id}/mapp` compiles, ships, and fails at run time in front of whoever
//! opened that screen. Nothing in this repository checked it before now.
//!
//! **This is a scan and not a parser**, for [`crate::project_map::rust_imports`]'s reason, and its
//! blind spots are counted rather than hidden. Three of them earned their place by producing a
//! confidently wrong answer during construction:
//!
//! - **Test routers are not the boundary.** `auth.rs` registers 66 routes and every one is a
//!   fixture inside `#[cfg(test)] mod tests`; `hooks.rs` and `runs.rs` have their own. Scanning
//!   every `.route(` in every file said this daemon serves `/secret`. So a `#[cfg(test)] mod`
//!   block is skipped whole, and after that every route found is in `http.rs` — 208 of them,
//!   which is exactly what `http.rs` registers outside its own tests.
//! - **A comment is not code.** `auth.rs` explains itself with `` `.route("/files", …)` `` inside a
//!   doc comment, and a text search reported that as a served route. The scan runs over
//!   [`crate::map_items::mask`], which blanks comments and string *contents* while leaving every
//!   character where it was — so a hit is real code, and the literal is read back out of the
//!   untouched source at the same index.
//! - **A hole glued to a segment is not a whole segment.** `` `/email/queue${query}` `` reads as
//!   `/email/queue{}`, and matching that against `/email/{id}` segment by segment succeeds — the
//!   instrument reporting, with confidence, that a screen listing the mail queue was fetching one
//!   message. A call with a partial segment now matches nothing directly: it is reported as
//!   *computed*, with every route it could be, and the reader decides.
//!
//! **A project with only one of the two halves is told so rather than accused.** A backend that is
//! not axum registers nothing this scan recognises, and comparing calls against an empty list would
//! paint every request in the shell red — a screen of findings about a project that works. §11 makes
//! the same concession to a project with no specs.
//!
//! **The one blind spot that cannot be closed cheaply is stated instead.** Two call sites hand
//! `apiFetch` a path built elsewhere — `useRuns` binds it one line above, and `useWorkflowChange`
//! takes it from its caller — so the four workflow routes only those callers reach are reported as
//! *nothing calls this*. Widening the rule to any string literal starting with `/` was
//! tried and measured: it collected 453 literals and called 118 of them unmatched, nearly all of
//! them the app's own router paths (`/fleet`, `/waiting`, `/chats/{}`). That is a worse answer
//! wearing more numbers, so the narrow rule stands and [`Seam::opaque`] carries the cost of it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::map_items::{mask, match_brace};
use crate::project_map::{Module, Reader};

/// A place in a file, for a call this scan could say nothing about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Site {
    pub file: String,
    pub line: usize,
}

/// One call from the shell to the daemon, with its path as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Call {
    /// The path with every interpolated expression replaced by `{}`, and the query string dropped.
    pub path: String,
    pub file: String,
    pub line: usize,
}

/// A call whose path is partly assembled at run time.
///
/// **Reported apart from a match and apart from a miss, because it is neither.**
/// `` `/email/queue${query}` `` is a path this scan cannot finish reading: what the expression
/// appends may be a query string, another segment, or nothing. Folding it into the matches would
/// claim a route was reached that may not be; folding it into the misses would report a bug in
/// working code. So it carries its candidates and says which they are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Computed {
    pub path: String,
    pub file: String,
    pub line: usize,
    /// Routes this call could be, empty when none fits under any reading.
    pub candidates: Vec<String>,
}

/// The boundary between the two sides of the product, from both ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct Seam {
    /// Every route path the daemon registers outside its own tests, sorted and deduplicated.
    ///
    /// **This is the literal answer to §16.4's question.** The boundary is not a folder and not a
    /// convention: it is this list.
    pub served: Vec<String>,
    /// Call sites found, including the ones that could not be read.
    pub calls: usize,
    /// Calls matching exactly one served route, read end to end with no guesswork.
    pub matched: usize,
    pub computed: Vec<Computed>,
    /// Calls matching no served route under any reading. **The finding this module exists for.**
    pub unmatched: Vec<Call>,
    /// Call sites handed a path built somewhere else, so nothing could be said about them.
    ///
    /// Counted and listed rather than dropped, because a route only these reach is reported in
    /// [`Seam::uncalled`] as though nothing wanted it. The number beside that list is what tells a
    /// reader how much to trust it.
    pub opaque: Vec<Site>,
    /// Served routes no call here reaches — **and *no screen* is not *nothing*.**
    ///
    /// The hooks, the webhook and the browser's own verbs are called by agents and sidecars over
    /// the same HTTP, which this scan does not read. Reading this list as dead code would delete
    /// working routes.
    pub uncalled: Vec<String>,
}

/// Read both ends of the boundary and compare them.
///
/// Takes the modules [`crate::project_map::structure`] already found rather than walking the tree
/// again — one definition of *which files are this project*, for §16.3's fourth reason.
pub fn seam(root: &Path, modules: &[Module]) -> std::io::Result<Seam> {
    let mut served: BTreeSet<String> = BTreeSet::new();
    let mut calls: Vec<(String, Site)> = Vec::new();
    let mut opaque: Vec<Site> = Vec::new();

    for module in modules {
        let source = std::fs::read_to_string(root.join(&module.path)).unwrap_or_default();
        match module.reader {
            Reader::Rust => served.extend(routes(&source)),
            Reader::Typescript => {
                let found = requests(&source);
                for (path, line) in found.paths {
                    calls.push((
                        path,
                        Site {
                            file: module.path.clone(),
                            line,
                        },
                    ));
                }
                for line in found.opaque {
                    opaque.push(Site {
                        file: module.path.clone(),
                        line,
                    });
                }
            }
        }
    }

    let total = calls.len() + opaque.len();

    // **Nothing to compare against is not the same as nothing matching**, and this repository is a
    // bad place to notice the difference because it always has both halves. A project whose backend
    // is not axum registers no route here, and every call the shell makes would come back as a
    // route nobody serves — a screen of red about a project that is working perfectly. §11 makes
    // the same concession to a project with no specs: say what could not be read, rather than
    // report the absence of an input as a finding about the code.
    if served.is_empty() {
        return Ok(Seam {
            served: Vec::new(),
            calls: total,
            matched: 0,
            computed: Vec::new(),
            unmatched: Vec::new(),
            opaque,
            uncalled: Vec::new(),
        });
    }

    let shapes: BTreeMap<&str, Vec<String>> = served
        .iter()
        .map(|route| (route.as_str(), segments(route)))
        .collect();

    let mut matched = 0usize;
    let mut computed = Vec::new();
    let mut unmatched = Vec::new();
    let mut reached: BTreeSet<String> = BTreeSet::new();

    for (path, site) in &calls {
        let asked = segments(path);
        let partial = asked.iter().any(|part| part != "{}" && part.contains("{}"));
        if !partial {
            let hits: Vec<&str> = shapes
                .iter()
                .filter(|(_, shape)| fits(&asked, shape))
                .map(|(route, _)| *route)
                .collect();
            if !hits.is_empty() {
                matched += 1;
                reached.extend(hits.into_iter().map(str::to_string));
                continue;
            }
            unmatched.push(Call {
                path: path.clone(),
                file: site.file.clone(),
                line: site.line,
            });
            continue;
        }
        let candidates: Vec<String> = shapes
            .iter()
            .filter(|(_, shape)| fits(&without_the_glue(&asked), shape))
            .map(|(route, _)| (*route).to_string())
            .collect();
        reached.extend(candidates.iter().cloned());
        computed.push(Computed {
            path: path.clone(),
            file: site.file.clone(),
            line: site.line,
            candidates,
        });
    }

    let uncalled = served
        .iter()
        .filter(|route| !reached.contains(route.as_str()))
        .cloned()
        .collect();

    Ok(Seam {
        served: served.into_iter().collect(),
        calls: total,
        matched,
        computed,
        unmatched,
        opaque,
        uncalled,
    })
}

/// The route paths one Rust file registers, outside its own tests.
///
/// Every `.route("…")` in real code. The literal is read from the untouched source at the index the
/// masked copy found the quote at, which is what the mask preserving positions buys.
pub fn routes(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let masked = mask(source, true);
    let fixtures = test_blocks(&masked);
    let mut found = Vec::new();
    let needle: Vec<char> = ".route(".chars().collect();
    let mut at = 0;
    while let Some(hit) = find(&masked, &needle, at) {
        at = hit + needle.len();
        if fixtures.iter().any(|(from, to)| hit >= *from && hit <= *to) {
            continue;
        }
        let Some(open) = masked[at..].iter().position(|c| *c == '"').map(|i| at + i) else {
            break;
        };
        // Only whitespace may sit between the paren and the literal. Anything else means this
        // `.route(` was handed something computed and the next quote in the file belongs to
        // somebody else entirely.
        if masked[at..open].iter().any(|c| !c.is_whitespace()) {
            continue;
        }
        let Some(close) = masked[open + 1..]
            .iter()
            .position(|c| *c == '"')
            .map(|i| open + 1 + i)
        else {
            break;
        };
        found.push(chars[open + 1..close].iter().collect::<String>());
        at = close + 1;
    }
    found
}

/// What one TypeScript file asks the daemon for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Requests {
    /// Paths with their line numbers, interpolations written `{}`.
    pub paths: Vec<(String, usize)>,
    /// Lines of call sites whose path is not a literal.
    pub opaque: Vec<usize>,
}

/// Every `apiFetch` / `apiText` / `apiBlob` call in one file, and what it asks for.
///
/// **The three names are the whole of the shell's side of the boundary**, and that is a property of
/// `data/client.ts` rather than an assumption: `DAEMON_URL` is joined to a path in exactly one
/// place, and these are the three exported wrappers over it. A fourth wrapper would go unread here,
/// which is the same class of gap as the one [`Seam::opaque`] counts.
pub fn requests(source: &str) -> Requests {
    let chars: Vec<char> = source.chars().collect();
    let masked = mask(source, false);
    let mut found = Requests::default();
    for name in ["apiFetch", "apiText", "apiBlob"] {
        let needle: Vec<char> = name.chars().collect();
        let mut at = 0;
        while let Some(hit) = find(&masked, &needle, at) {
            at = hit + needle.len();
            // The name and not the tail of a longer one. `myApiFetch("/x")` contains `apiFetch`
            // followed by a paren and a literal, and without this it would put somebody else's
            // function's argument on the daemon's boundary. Nothing in this repository is named
            // that way today, which is exactly why the guard is written now rather than after.
            if hit > 0 && part_of_a_name(masked[hit - 1]) {
                continue;
            }
            let mut i = at;
            // An optional type argument, which may nest.
            if masked.get(i) == Some(&'<') {
                let mut depth = 0;
                while i < masked.len() {
                    if masked[i] == '<' {
                        depth += 1;
                    } else if masked[i] == '>' {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    i += 1;
                }
            }
            while masked.get(i).is_some_and(|c| c.is_whitespace()) {
                i += 1;
            }
            if masked.get(i) != Some(&'(') {
                continue;
            }
            i += 1;
            while masked.get(i).is_some_and(|c| c.is_whitespace()) {
                i += 1;
            }
            // `apiFetch<T>(path: string, init?: RequestInit)` is where these functions are
            // DECLARED, not a place one is called, and the three declarations in `data/client.ts`
            // were being counted as call sites nobody could read. That inflated the one number
            // whose whole job is to say how much the *nothing calls this* list is worth — a wrong
            // answer inside the field that exists to admit ignorance. A first argument that is a
            // name followed by a colon is a parameter list; a call passing a variable is a name
            // followed by `,` or `)`, and stays counted.
            if declares_its_parameters(&masked, i) {
                continue;
            }
            let line = masked[..i.min(masked.len())]
                .iter()
                .filter(|c| **c == '\n')
                .count()
                + 1;
            match masked.get(i) {
                Some('"') => {
                    let Some(close) = masked[i + 1..]
                        .iter()
                        .position(|c| *c == '"')
                        .map(|k| i + 1 + k)
                    else {
                        continue;
                    };
                    found
                        .paths
                        .push((chars[i + 1..close].iter().collect::<String>(), line));
                }
                Some('`') => match template(&chars, i) {
                    Some(path) => found.paths.push((path, line)),
                    None => found.opaque.push(line),
                },
                _ => found.opaque.push(line),
            }
        }
    }
    found.paths.sort_by_key(|(_, line)| *line);
    found.opaque.sort_unstable();
    found
}

/// Whether a character could be part of a JavaScript identifier.
fn part_of_a_name(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Whether what follows an opening paren is a parameter list rather than an argument.
///
/// `(path: string, …)` declares the function; `(path, …)` calls it with one. The whole test is the
/// colon after the first name.
fn declares_its_parameters(masked: &[char], from: usize) -> bool {
    let mut i = from;
    if !masked.get(i).copied().is_some_and(part_of_a_name) {
        return false;
    }
    while masked.get(i).copied().is_some_and(part_of_a_name) {
        i += 1;
    }
    while masked.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    masked.get(i) == Some(&':')
}

/// A template literal read out of the untouched source, with every `${…}` written `{}`.
///
/// Read from the real characters and not the masked copy on purpose: the mask blanks a string's
/// contents, which is precisely the text wanted here. Brace depth carries the expression, so a
/// nested object literal survives; a backtick *inside* the expression does not, and that call comes
/// back as opaque rather than as a wrong path.
fn template(chars: &[char], open: usize) -> Option<String> {
    let mut out = String::new();
    let mut i = open + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '`' => return Some(out),
            '$' if chars.get(i + 1) == Some(&'{') => {
                let mut depth = 1;
                i += 2;
                while i < chars.len() && depth > 0 {
                    match chars[i] {
                        '{' => depth += 1,
                        '}' => depth -= 1,
                        '`' => return None,
                        _ => {}
                    }
                    i += 1;
                }
                out.push_str("{}");
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None
}

/// The spans of every `#[cfg(test)] mod …` block, which is where fixture routers live.
///
/// Only a `mod`, because that is the shape the exclusion is sound for: an attribute on a single
/// item has no block to measure, and hunting for the next brace would swallow the rest of the file.
/// A route registered inside a `#[cfg(test)] fn` would still be counted, and this repository has
/// none.
fn test_blocks(masked: &[char]) -> Vec<(usize, usize)> {
    let needle: Vec<char> = "#[cfg(test)]".chars().collect();
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(hit) = find(masked, &needle, at) {
        at = hit + needle.len();
        let mut i = at;
        while masked.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        let rest: String = masked[i..masked.len().min(i + 8)].iter().collect();
        if !(rest.starts_with("mod ") || rest.starts_with("pub mod ")) {
            continue;
        }
        let Some(open) = masked[i..].iter().position(|c| *c == '{').map(|k| i + k) else {
            continue;
        };
        if let Some(close) = match_brace(masked, open) {
            spans.push((hit, close));
        }
    }
    spans
}

/// A path split into segments, with the query string dropped and route parameters made anonymous.
///
/// `/projects/{id}/map` and `/projects/{}/map` come out the same, which is the point: one side
/// names its parameters and the other cannot.
fn segments(path: &str) -> Vec<String> {
    path.split('?')
        .next()
        .unwrap_or("")
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                "{}".to_string()
            } else {
                part.to_string()
            }
        })
        .collect()
}

/// Whether a call's segments could be this route's.
///
/// A `{}` on either side stands for one whole segment and nothing else. **Never for part of one**:
/// letting `queue{}` match `{}` is what had this module reporting that the mail queue screen
/// fetched a single message.
fn fits(asked: &[String], route: &[String]) -> bool {
    asked.len() == route.len()
        && asked.iter().zip(route).all(|(a, b)| {
            if a != "{}" && a.contains("{}") {
                // Half a segment matches nothing, not even a route parameter. The check lives here
                // rather than only at the call site so it cannot be forgotten by the next caller.
                return false;
            }
            a == b || a == "{}" || b == "{}"
        })
}

/// The same path read as though a trailing glued expression were a query string.
///
/// `/email/queue{}` becomes `/email/queue`, which is what `` `/email/queue${query}` `` means every
/// time in this repository — and the reason that reading is offered as a candidate rather than
/// taken as the answer is that nothing lexical can tell it from a segment being appended.
fn without_the_glue(asked: &[String]) -> Vec<String> {
    let mut trimmed: Vec<String> = asked.to_vec();
    if let Some(last) = trimmed.last_mut()
        && last.contains("{}")
    {
        *last = last.replace("{}", "");
    }
    trimmed.retain(|part| !part.is_empty());
    trimmed
}

/// The first index at or after `from` where `needle` sits in `hay`.
fn find(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checkout this test is *running in*, checked against the one it was *compiled in*.
    ///
    /// The same assertion `core/tests/module_map.rs` makes and for the same measured reason: with a
    /// shared target directory a binary compiled in another worktree gets reused here, and a
    /// ground-truth test that silently reads another checkout's files is worse than one that
    /// refuses to run.
    fn repository() -> std::path::PathBuf {
        let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory should be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "this binary was compiled in {} and is running in {} — a shared target directory has              handed this checkout a binary built somewhere else. Touch this file to force a rebuild.",
            built_in.display(),
            running_in.display(),
        );
        running_in
            .parent()
            .expect("the package sits inside the repository")
            .to_path_buf()
    }

    /// **The gate this module exists to be**, run against the repository it ships in.
    ///
    /// A screen calling a route the daemon does not serve compiles, ships, and fails in front of
    /// whoever opened it. Nothing here checked that before this test.
    ///
    /// **The two size assertions are not padding.** A scan that silently found nothing would report
    /// zero unmatched calls and pass, which is this feature's own failure mode — the instrument
    /// built to catch a confident wrong answer giving one. So the counts are asserted first, and
    /// only then the emptiness.
    #[test]
    fn no_screen_in_this_repository_asks_for_a_route_the_daemon_does_not_serve() {
        let root = repository();
        let structure = crate::project_map::structure(&root).expect("the tree should be readable");
        let found = seam(&root, &structure.modules).expect("the sources should be readable");
        assert!(
            found.served.len() > 100,
            "only {} routes found — the scan has stopped reading the router, and every conclusion              below it is worthless",
            found.served.len(),
        );
        assert!(
            found.matched > 100,
            "only {} calls matched of {} — the scan has stopped reading the shell",
            found.matched,
            found.calls,
        );
        assert!(
            found.unmatched.is_empty(),
            "the shell asks for routes this daemon does not serve: {:?}",
            found.unmatched,
        );
        // Every call site lands in exactly one pile. A call that fell out of all four would be one
        // this module read and then said nothing about, which is the one outcome its whole shape is
        // built to prevent.
        assert_eq!(
            found.calls,
            found.matched + found.computed.len() + found.unmatched.len() + found.opaque.len(),
            "every call site belongs to exactly one pile"
        );
    }

    #[test]
    fn a_project_whose_backend_is_not_read_here_is_told_so_and_not_accused() {
        // Comparing calls against an empty list would paint every request in a non-axum project red
        // — a screen of findings about a project that works.
        let root = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(root.path().join("shell/src")).expect("a folder");
        std::fs::write(
            root.path().join("shell/src/x.ts"),
            "export const load = () => apiFetch<Thing[]>(\"/things\");
",
        )
        .expect("a file");
        let structure = crate::project_map::structure(root.path()).expect("the tree");
        let found = seam(root.path(), &structure.modules).expect("the sources");
        assert!(found.served.is_empty());
        assert_eq!(found.calls, 1);
        assert!(
            found.unmatched.is_empty(),
            "a call cannot miss a route in a project where no route was found"
        );
    }

    #[test]
    fn a_registered_route_is_found_with_its_path() {
        let found = routes("let app = Router::new().route(\"/status\", get(status));");
        assert_eq!(found, vec!["/status".to_string()]);
    }

    #[test]
    fn a_route_registered_in_a_test_module_is_not_the_boundary() {
        // `auth.rs` registers 66 routes and every one is a fixture. Counting them said this daemon
        // serves `/secret`, which is a confident wrong answer about what the product exposes.
        let source = "
fn app() -> Router { Router::new().route(\"/real\", get(h)) }

#[cfg(test)]
mod tests {
    fn fake() -> Router { Router::new().route(\"/secret\", get(h)) }
}
";
        assert_eq!(routes(source), vec!["/real".to_string()]);
    }

    #[test]
    fn a_route_written_inside_a_comment_is_not_registered() {
        // `auth.rs` explains itself with one, and a text search believed it.
        let source =
            "/// `.route(\"/files\", get(get_files))`, so a list of paths alone would\nfn a() {}";
        assert!(routes(source).is_empty());
    }

    #[test]
    fn a_route_handed_something_computed_is_skipped_rather_than_given_a_stranger_s_path() {
        // Taking the next quote in the file would attach an unrelated literal to this call.
        let source = "let app = Router::new().route(prefix, get(h));\nlet name = \"/elsewhere\";";
        assert!(routes(source).is_empty());
    }

    #[test]
    fn a_plain_call_gives_its_path_and_its_line() {
        let found = requests("\n\nqueryFn: () => apiFetch<Agent[]>(\"/agents\"),");
        assert_eq!(found.paths, vec![("/agents".to_string(), 3)]);
        assert!(found.opaque.is_empty());
    }

    #[test]
    fn an_interpolated_segment_becomes_a_hole() {
        let found = requests("apiFetch<Job>(`/jobs/${id ?? -1}`)");
        assert_eq!(found.paths, vec![("/jobs/{}".to_string(), 1)]);
    }

    #[test]
    fn a_call_handed_a_path_from_elsewhere_is_counted_and_not_guessed_at() {
        // `useRuns` binds the path one line above; `useWorkflowChange` takes it from its caller.
        let found = requests("const path = `/runs`;\nreturn apiFetch<Row[]>(path);");
        assert!(found.paths.is_empty());
        assert_eq!(found.opaque, vec![2]);
    }

    #[test]
    fn where_the_client_declares_these_functions_is_not_a_call_site() {
        // `data/client.ts` declares all three, and counting them as unreadable calls inflated the
        // one number whose job is to say how much the *nothing calls this* list is worth.
        let found = requests(
            "export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {}",
        );
        assert!(found.paths.is_empty());
        assert!(
            found.opaque.is_empty(),
            "a declaration is not a call this scan failed to read"
        );
    }

    #[test]
    fn a_function_whose_name_merely_ends_in_one_of_these_is_somebody_else_s() {
        // Nothing here is named this way today, which is why the guard is written now.
        let found = requests("myApiFetch(\"/not-the-daemon\");");
        assert!(found.paths.is_empty());
        assert!(found.opaque.is_empty());
    }

    #[test]
    fn a_path_inside_a_comment_is_not_a_call() {
        let found = requests("// apiFetch<Thing>(\"/ghost\")\nconst x = 1;");
        assert!(found.paths.is_empty());
        assert!(found.opaque.is_empty());
    }

    #[test]
    fn the_query_string_is_not_part_of_the_path() {
        assert_eq!(segments("/jobs?live=true"), vec!["jobs".to_string()]);
    }

    #[test]
    fn a_named_parameter_and_an_interpolation_are_the_same_shape() {
        assert!(fits(&segments("/email/{}"), &segments("/email/{id}")));
    }

    #[test]
    fn a_hole_stands_for_a_whole_segment_and_never_for_part_of_one() {
        // `/email/queue${query}` matching `/email/{id}` had this module reporting that the screen
        // listing the mail queue was fetching one message.
        assert!(!fits(&segments("/email/queue{}"), &segments("/email/{id}")));
    }

    #[test]
    fn a_shorter_path_never_matches_a_longer_route() {
        assert!(!fits(
            &segments("/projects/{}"),
            &segments("/projects/{id}/map")
        ));
    }

    #[test]
    fn a_trailing_expression_read_as_a_query_string_gives_back_the_route_before_it() {
        assert_eq!(
            without_the_glue(&segments("/email/queue{}")),
            vec!["email".to_string(), "queue".to_string()]
        );
    }
}
