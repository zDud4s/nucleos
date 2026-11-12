//! §spec atlas-layers-uml 2c
//!
//! A project's key flows, DECLARED in `.nucleos/flows.yaml` and VERIFIED here — never inferred.
//!
//! Inferring a call chain without a parser is fragile, so the owner writes the chain as ordered
//! anchors (`participant`, `path`, `fn`) and this module only checks that each anchor still holds:
//! the function exists in its file, and its caller still names it. A diagram that drifted from the
//! code says so (`stale`, `broken_step`) instead of lying.
//!
//! **The body rule.** A call is looked for in the caller's *body*, taken from the same scanner
//! that finds the items ([`items::items`] for where a function starts, [`items::body_of`] for where
//! it ends), over the source masked by [`items::mask`]. Reusing that scanner means this module and
//! the item list can never disagree about where a function is, and masking stops a name inside a
//! comment or a string literal from counting as a call. A declaration with no brace body (a
//! TypeScript arrow with an expression body) runs to the start of the next item's line, or to the
//! end of the file.
//!
//! **Two optional extensions to a step**, because a linear list of direct calls cannot say the
//! rest. `via` absent is a direct call; `via: "<METHOD> /path"` is an HTTP hop, checked against the
//! caller's RAW text (the mask blanks string literals, and a route lives in one); `via: "async
//! ..."` is a queue hop, checked for existence only. `from` is the index of the calling step when
//! it is not the previous one — without it a flow could not express a return.
//!
//! **Stated limits.** A call is matched by NAME, not by resolution, so a trait-object call
//! (`executor.execute(`) matches any same-named method and can read as verified for a call that
//! goes elsewhere. Go has no reader here, so a Go step says `no reader for this language` — honest,
//! not a crash.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::map::Reader;
use crate::map::items::{self, Item};

/// Where a project declares its flows, relative to its root.
pub const FLOWS_FILE: &str = ".nucleos/flows.yaml";

/// The file's shape: the top level is the `flows:` key.
#[derive(Deserialize)]
struct FlowsFile {
    flows: Vec<DeclaredFlow>,
}

/// One flow as the owner wrote it.
#[derive(Debug, Clone, Deserialize)]
pub struct DeclaredFlow {
    pub name: String,
    pub steps: Vec<DeclaredStep>,
}

/// One anchor of a flow, as the owner wrote it.
#[derive(Debug, Clone, Deserialize)]
pub struct DeclaredStep {
    pub participant: String,
    pub path: String,
    #[serde(rename = "fn")]
    pub function: String,
    #[serde(default)]
    pub via: Option<String>,
    #[serde(default)]
    pub from: Option<usize>,
}

/// One anchor, after checking it against the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepCheck {
    pub participant: String,
    pub path: String,
    #[serde(rename = "fn")]
    pub function: String,
    pub via: Option<String>,
    /// The index of the step that calls this one; `None` for the first step.
    pub from: Option<usize>,
    /// Where the function is declared, 1-based, or `None` when it was not found.
    pub line: Option<usize>,
    pub ok: bool,
    pub reason: Option<String>,
}

/// One flow, after checking every step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowCheck {
    pub name: String,
    pub stale: bool,
    /// The first step that does not hold.
    pub broken_step: Option<usize>,
    pub steps: Vec<StepCheck>,
}

/// The answer for a whole project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Flows {
    pub file: String,
    /// Whether the project has a flows file at all; its absence is no flows, not an error.
    pub declared: bool,
    pub root: String,
    pub flows: Vec<FlowCheck>,
    /// Why the file could not be read, when it could not.
    pub error: Option<String>,
}

/// Parses the flows file, refusing a flow with no name or no steps.
pub fn parse(text: &str) -> Result<Vec<DeclaredFlow>, String> {
    let file: FlowsFile = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    for flow in &file.flows {
        if flow.name.trim().is_empty() {
            return Err("a flow has an empty name".to_string());
        }
        if flow.steps.is_empty() {
            return Err(format!("flow `{}` has no steps", flow.name));
        }
    }
    Ok(file.flows)
}

/// A step whose function was found: its file's text, and where its body sits in it.
struct Found {
    name: String,
    path: String,
    line: usize,
    /// The file's characters, comments and strings blanked.
    masked: Vec<char>,
    /// The file's characters as written.
    raw: Vec<char>,
    /// The body, as a char range into both of the above.
    span: (usize, usize),
}

/// Verifies every step of `flow`, reading files through `read` (a path relative to the root).
pub fn verify(flow: &DeclaredFlow, read: &dyn Fn(&str) -> Option<String>) -> FlowCheck {
    let mut found: Vec<Option<Found>> = Vec::new();
    let mut steps: Vec<StepCheck> = Vec::new();

    for (i, step) in flow.steps.iter().enumerate() {
        let (located, mut reason, line) = locate(step, read);
        let caller = if i > 0 {
            Some(step.from.unwrap_or(i - 1))
        } else {
            None
        };
        // A caller that did not resolve leaves nothing to check the link against: the step then
        // reports only its own existence, and the flow's broken step is the first failure.
        if let (None, Some(from)) = (&reason, caller) {
            reason = match found.get(from) {
                _ if from >= i => Some("from must name an earlier step".to_string()),
                Some(Some(parent)) => link_failure(step, parent),
                _ => None,
            };
        }
        steps.push(StepCheck {
            participant: step.participant.clone(),
            path: step.path.clone(),
            function: step.function.clone(),
            via: step.via.clone(),
            from: caller,
            line,
            ok: reason.is_none(),
            reason,
        });
        found.push(located);
    }

    let broken_step = steps.iter().position(|s| !s.ok);
    FlowCheck {
        name: flow.name.clone(),
        stale: broken_step.is_some(),
        broken_step,
        steps,
    }
}

/// Finds the step's function: what was found, why not, and the line it is declared on.
fn locate(
    step: &DeclaredStep,
    read: &dyn Fn(&str) -> Option<String>,
) -> (Option<Found>, Option<String>, Option<usize>) {
    let Some(source) = read(&step.path) else {
        let why = format!("{} could not be read inside the project", step.path);
        return (None, Some(why), None);
    };
    let scanned = items::items(&step.path, &source);
    let Some(reader) = scanned.reader else {
        return (None, Some("no reader for this language".to_string()), None);
    };
    let qualified = step.function.contains("::");
    let matches: Vec<&Item> = scanned
        .items
        .iter()
        .filter(|item| {
            if qualified {
                item.id == step.function
            } else {
                item.name == step.function
            }
        })
        .collect();
    let item = match matches.as_slice() {
        [] => {
            let why = format!("no fn {} in {}", step.function, step.path);
            return (None, Some(why), None);
        }
        [one] => *one,
        many => {
            let why = format!(
                "{} items are named {} in {}; qualify it as Type::{}",
                many.len(),
                step.function,
                step.path,
                step.function
            );
            return (None, Some(why), None);
        }
    };

    let masked = items::mask(&source, reader == Reader::Rust);
    let raw: Vec<char> = source.chars().collect();
    let span = body_span(&masked, &scanned.items, item);
    let found = Found {
        name: item.name.clone(),
        path: step.path.clone(),
        line: item.line,
        masked,
        raw,
        span,
    };
    (Some(found), None, Some(item.line))
}

/// Where `item`'s body sits, as a char range: its braces, or up to the next item's line.
fn body_span(masked: &[char], all: &[Item], item: &Item) -> (usize, usize) {
    let start = line_start(masked, item.line);
    let name: Vec<char> = item.name.chars().collect();
    let name_end = find_word(masked, start, masked.len(), &name)
        .map(|at| at + name.len())
        .unwrap_or(start);
    if let Some((open, close)) = items::body_of(masked, name_end) {
        return (open, close + 1);
    }
    let next = all
        .iter()
        .map(|other| other.line)
        .filter(|line| *line > item.line)
        .min()
        .map(|line| line_start(masked, line))
        .unwrap_or(masked.len());
    (name_end, next.max(name_end))
}

/// The char index where the 1-based `line` starts, or the end of the text when it has fewer.
fn line_start(chars: &[char], line: usize) -> usize {
    let mut seen = 1;
    for (i, c) in chars.iter().enumerate() {
        if seen >= line {
            return i;
        }
        if *c == '\n' {
            seen += 1;
        }
    }
    chars.len()
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The first place in `chars[from..to]` where `word` stands as a whole identifier.
fn find_word(chars: &[char], from: usize, to: usize, word: &[char]) -> Option<usize> {
    if word.is_empty() || to > chars.len() {
        return None;
    }
    let mut i = from;
    while i + word.len() <= to {
        if chars[i..i + word.len()] == *word {
            let before = i == 0 || !is_ident(chars[i - 1]);
            let after = chars.get(i + word.len()).is_none_or(|c| !is_ident(*c));
            if before && after {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Whether `chars[from..to]` calls `word`: a whole identifier, then `(`, `<` or `::<`.
fn calls(chars: &[char], from: usize, to: usize, word: &[char]) -> bool {
    let mut at = from;
    while let Some(hit) = find_word(chars, at, to, word) {
        let mut after = hit + word.len();
        while chars.get(after).is_some_and(|c| *c == ' ' || *c == '\t') {
            after += 1;
        }
        let rest = &chars[after.min(chars.len())..];
        let opens = rest.starts_with(&['(']) || rest.starts_with(&['<']);
        if opens || rest.starts_with(&[':', ':', '<']) {
            return true;
        }
        at = hit + word.len();
    }
    false
}

/// Why `step` is not reached from `caller`, or `None` when the link holds.
fn link_failure(step: &DeclaredStep, caller: &Found) -> Option<String> {
    let (from, to) = caller.span;
    match step.via.as_deref() {
        None => {
            let bare = step.function.rsplit("::").next().unwrap_or(&step.function);
            let word: Vec<char> = bare.chars().collect();
            if calls(&caller.masked, from, to, &word) {
                return None;
            }
            Some(format!(
                "{} is not called inside {} ({}:{})",
                bare, caller.name, caller.path, caller.line
            ))
        }
        Some(via) if via.trim_start().starts_with("async") => None,
        Some(via) => {
            let route = via.split_once(' ').map_or(via, |(_, rest)| rest).trim();
            let prefix = route.split('{').next().unwrap_or(route);
            let wanted: Vec<char> = prefix.chars().collect();
            let text = &caller.raw[from..to];
            if wanted.is_empty() || text.windows(wanted.len()).any(|w| w == wanted) {
                None
            } else {
                Some(format!("{} does not name {}", caller.name, prefix))
            }
        }
    }
}

/// Reads and verifies a project's flows file.
pub fn check(root: &Path) -> Flows {
    let answer = |declared: bool, flows: Vec<FlowCheck>, error: Option<String>| Flows {
        file: FLOWS_FILE.to_string(),
        declared,
        root: root.to_string_lossy().into_owned(),
        flows,
        error,
    };
    let text = match std::fs::read_to_string(root.join(FLOWS_FILE)) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return answer(false, Vec::new(), None);
        }
        Err(e) => return answer(true, Vec::new(), Some(e.to_string())),
    };
    let declared = match parse(&text) {
        Ok(flows) => flows,
        Err(message) => return answer(true, Vec::new(), Some(message)),
    };
    let read = |path: &str| {
        crate::inspect::resolved_within(root, path)
            .ok()
            .and_then(|file| std::fs::read_to_string(file).ok())
    };
    let checked = declared.iter().map(|flow| verify(flow, &read)).collect();
    answer(true, checked, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    /// A step with no `via` and no `from`: a direct call by the previous step.
    fn step(participant: &str, path: &str, function: &str) -> DeclaredStep {
        DeclaredStep {
            participant: participant.to_string(),
            path: path.to_string(),
            function: function.to_string(),
            via: None,
            from: None,
        }
    }

    fn flow(steps: Vec<DeclaredStep>) -> DeclaredFlow {
        DeclaredFlow {
            name: "demo".to_string(),
            steps,
        }
    }

    /// Verifies `flow` against in-memory files, so no test here touches the disk.
    fn verify_in(files: &[(&str, &str)], flow: &DeclaredFlow) -> FlowCheck {
        let files: HashMap<String, String> = files
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect();
        verify(flow, &|path: &str| files.get(path).cloned())
    }

    fn write(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const CHAIN_A: &str = "pub fn first() {\n    second();\n}\n";
    const CHAIN_B: &str = "pub fn second() {\n    third(1);\n}\npub fn third(_: u8) {}\n";

    #[test]
    fn an_absent_flows_file_is_no_flows_and_not_an_error() {
        let dir = tempfile::tempdir().unwrap();

        let found = check(dir.path());

        assert!(!found.declared);
        assert!(found.flows.is_empty());
        assert!(found.error.is_none());
    }

    #[test]
    fn a_malformed_flows_file_is_reported_in_the_answer() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), FLOWS_FILE, "flows: [this is: not, closed\n");

        let found = check(dir.path());

        assert!(found.declared);
        assert!(found.error.is_some(), "a malformed file must say so");
        assert!(found.flows.is_empty());
    }

    #[test]
    fn a_chain_of_direct_calls_verifies() {
        let declared = flow(vec![
            step("a", "a.rs", "first"),
            step("b", "b.rs", "second"),
            step("b", "b.rs", "third"),
        ]);

        let checked = verify_in(&[("a.rs", CHAIN_A), ("b.rs", CHAIN_B)], &declared);

        assert!(checked.steps.iter().all(|s| s.ok), "{:?}", checked.steps);
        assert!(checked.steps.iter().all(|s| s.line.is_some()));
        assert!(!checked.stale);
        assert_eq!(checked.broken_step, None);
        assert_eq!(checked.steps[1].line, Some(1));
        assert_eq!(checked.steps[2].line, Some(4));
        assert_eq!(checked.steps[1].from, Some(0));
        assert_eq!(checked.steps[0].from, None);
    }

    #[test]
    fn a_step_whose_fn_is_gone_marks_the_flow_stale_at_that_step() {
        let declared = flow(vec![
            step("a", "a.rs", "first"),
            step("b", "b.rs", "second"),
        ]);

        let checked = verify_in(
            &[("a.rs", CHAIN_A), ("b.rs", "pub fn other() {}\n")],
            &declared,
        );

        assert!(checked.stale);
        assert_eq!(checked.broken_step, Some(1));
        assert!(!checked.steps[1].ok);
        let reason = checked.steps[1].reason.as_deref().unwrap();
        assert!(reason.contains("second"), "{reason}");
        assert_eq!(checked.steps[1].line, None);
    }

    #[test]
    fn a_step_its_caller_never_calls_is_the_broken_step() {
        let declared = flow(vec![
            step("a", "a.rs", "first"),
            step("b", "b.rs", "second"),
        ]);

        let checked = verify_in(
            &[("a.rs", "pub fn first() {}\n"), ("b.rs", CHAIN_B)],
            &declared,
        );

        assert!(checked.stale);
        assert_eq!(checked.broken_step, Some(1));
        let reason = checked.steps[1].reason.as_deref().unwrap();
        assert!(
            reason.contains("first"),
            "the reason names the caller: {reason}"
        );
        // The fn itself exists, so the step still reports where it is.
        assert_eq!(checked.steps[1].line, Some(1));
    }

    #[test]
    fn a_name_inside_a_comment_or_string_is_not_a_call() {
        let caller = "pub fn first() {\n    // second();\n    let _ = \"second(\";\n}\n";
        let declared = flow(vec![
            step("a", "a.rs", "first"),
            step("b", "b.rs", "second"),
        ]);

        let checked = verify_in(&[("a.rs", caller), ("b.rs", CHAIN_B)], &declared);

        assert!(checked.stale, "{:?}", checked.steps);
        assert_eq!(checked.broken_step, Some(1));
    }

    #[test]
    fn an_http_hop_is_checked_by_the_route_path_in_the_callers_text() {
        let naming = "pub fn client() {\n    post(\"/vcs/land\");\n}\n";
        let silent = "pub fn client() {\n    post(\"/elsewhere\");\n}\n";
        let server = "pub fn land_worktree() {}\n";
        let mut hop = step("http", "b.rs", "land_worktree");
        hop.via = Some("POST /vcs/land".to_string());
        let declared = flow(vec![step("cli", "a.rs", "client"), hop]);

        let ok = verify_in(&[("a.rs", naming), ("b.rs", server)], &declared);
        let broken = verify_in(&[("a.rs", silent), ("b.rs", server)], &declared);

        assert!(!ok.stale, "{:?}", ok.steps);
        assert!(ok.steps[1].ok);
        assert!(broken.stale);
        assert_eq!(broken.broken_step, Some(1));
        let reason = broken.steps[1].reason.as_deref().unwrap();
        assert!(reason.contains("/vcs/land"), "{reason}");
    }

    #[test]
    fn an_async_hop_is_checked_for_existence_only() {
        let mut hop = step("drain", "b.rs", "second");
        hop.via = Some("async (queued row)".to_string());
        let declared = flow(vec![step("a", "a.rs", "first"), hop]);

        // The caller never mentions the callee, and that is the point of a queue.
        let present = verify_in(
            &[("a.rs", "pub fn first() {}\n"), ("b.rs", CHAIN_B)],
            &declared,
        );
        let absent = verify_in(
            &[("a.rs", "pub fn first() {}\n"), ("b.rs", "pub fn x() {}\n")],
            &declared,
        );

        assert!(!present.stale, "{:?}", present.steps);
        assert!(absent.stale);
        assert_eq!(absent.broken_step, Some(1));
    }

    #[test]
    fn from_names_a_caller_other_than_the_previous_step() {
        // `first` calls both `second` and `third`; `second` calls neither.
        let a = "pub fn first() {\n    second();\n    third();\n}\n";
        let b = "pub fn second() {}\npub fn third() {}\n";
        let mut back = step("b", "b.rs", "third");
        back.from = Some(0);
        let declared = flow(vec![
            step("a", "a.rs", "first"),
            step("b", "b.rs", "second"),
            back,
        ]);
        let files = [("a.rs", a), ("b.rs", b)];

        let checked = verify_in(&files, &declared);

        assert!(!checked.stale, "{:?}", checked.steps);
        assert_eq!(checked.steps[2].from, Some(0));

        // Without `from`, the previous step is the caller, and it never calls `third`.
        let mut without = declared.clone();
        without.steps[2].from = None;
        assert_eq!(verify_in(&files, &without).broken_step, Some(2));

        // A `from` that is not an earlier step breaks the step.
        let mut forward = declared.clone();
        forward.steps[1].from = Some(1);
        let checked = verify_in(&files, &forward);
        assert_eq!(checked.broken_step, Some(1));
        let reason = checked.steps[1].reason.as_deref().unwrap();
        assert!(reason.contains("earlier"), "{reason}");
    }

    #[test]
    fn an_ambiguous_bare_name_is_refused_until_qualified() {
        let caller = "pub fn first(a: A) {\n    a.run();\n}\n";
        let callee = "impl A {\n    pub fn run(&self) {}\n}\n\
                      impl B {\n    pub fn run(&self) {}\n}\n";
        let files = [("a.rs", caller), ("b.rs", callee)];

        let bare = verify_in(
            &files,
            &flow(vec![step("a", "a.rs", "first"), step("b", "b.rs", "run")]),
        );
        let qualified = verify_in(
            &files,
            &flow(vec![
                step("a", "a.rs", "first"),
                step("b", "b.rs", "A::run"),
            ]),
        );

        assert!(bare.stale);
        assert_eq!(bare.broken_step, Some(1));
        let reason = bare.steps[1].reason.as_deref().unwrap();
        assert!(reason.contains("Type::run"), "{reason}");
        assert!(!qualified.stale, "{:?}", qualified.steps);
        assert_eq!(qualified.steps[1].line, Some(2));
    }

    #[test]
    fn a_typescript_arrow_function_is_read_like_a_rust_fn() {
        let caller = "export const first = () => {\n  second();\n};\n";
        let callee = "export const second = () => {\n  return 1;\n};\n";
        let declared = flow(vec![
            step("a", "a.ts", "first"),
            step("b", "b.ts", "second"),
        ]);

        let checked = verify_in(&[("a.ts", caller), ("b.ts", callee)], &declared);
        assert!(!checked.stale, "{:?}", checked.steps);
        assert_eq!(checked.steps[1].line, Some(1));

        // The calls are checked too: an arrow that never calls it breaks the step.
        let idle = "export const first = () => {\n  return 0;\n};\n";
        let checked = verify_in(&[("a.ts", idle), ("b.ts", callee)], &declared);
        assert_eq!(checked.broken_step, Some(1));
    }

    #[test]
    fn a_step_path_outside_the_project_is_refused() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("project");
        write(outer.path(), "x.rs", "pub fn x() {}\n");
        write(
            &root,
            FLOWS_FILE,
            "flows:\n  - name: escape\n    steps:\n      \
             - { participant: x, path: ../x.rs, fn: x }\n",
        );

        let found = check(&root);

        assert!(found.error.is_none(), "{:?}", found.error);
        let step = &found.flows[0].steps[0];
        assert!(!step.ok);
        assert_eq!(
            step.line, None,
            "the file outside the project was never read"
        );
        assert!(found.flows[0].stale);
        assert_eq!(found.flows[0].broken_step, Some(0));
    }

    /// The checkout this test is *running in*, checked against the one it was *compiled in*.
    ///
    /// The same assertion `map::seam` and `core/tests/module_map.rs` make, for the same measured
    /// reason: with a shared target directory a binary compiled in another worktree gets reused,
    /// and a ground-truth test that silently reads another checkout's files is worse than one that
    /// refuses to run.
    fn repository() -> std::path::PathBuf {
        let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
        let running_in = std::env::current_dir().expect("the working directory should be readable");
        assert_eq!(
            built_in,
            running_in.as_path(),
            "this binary was compiled in {} and is running in {} — a shared target directory has \
             handed this checkout a binary built somewhere else. \
             Touch this file to force a rebuild.",
            built_in.display(),
            running_in.display(),
        );
        running_in
            // The package, `core/crates/nucleos-app`, sits three levels below the root.
            .ancestors()
            .nth(3)
            .expect("the package sits inside the repository")
            .to_path_buf()
    }

    /// The alarm: rename a function a declared flow names and this goes red, by design.
    #[test]
    fn this_repositorys_declared_flows_verify() {
        let found = check(&repository());

        assert!(
            found.declared,
            "{FLOWS_FILE} is missing from this repository"
        );
        assert!(found.error.is_none(), "{:?}", found.error);
        assert!(
            found.flows.iter().any(|f| f.name == "land"),
            "the `land` flow is not declared"
        );
        for flow in &found.flows {
            let broken = flow.broken_step.map(|i| &flow.steps[i]);
            assert!(
                !flow.stale,
                "flow `{}` is stale at step {:?}: {:?}",
                flow.name,
                flow.broken_step,
                broken.map(|s| (&s.path, &s.function, &s.reason)),
            );
        }
    }
}
