//! TypeScript declaration scanning for the project map.
//! This keeps the language-specific reader separate from the shared item graph logic.

use super::*;

/// The words that may stand between `export` and the keyword, or open a class member.
const TS_MODIFIERS: &[&str] = &[
    "export",
    "default",
    "async",
    "declare",
    "abstract",
    "public",
    "private",
    "protected",
    "static",
    "readonly",
];

/// Whether a keyword at `at` opens a statement, rather than sitting in the middle of something.
///
/// **This is what separates a declaration from prose, and in a `.tsx` file that is not a hair to
/// split.** JSX text lives at brace depth zero exactly as a top-level declaration does, so
/// `<p>Every function must return a value</p>` offered `function` in the same position a real one
/// occupies, and minted an item called `must`. A statement begins after a `;`, a `{`, a `}`, a line
/// break, or one of the modifiers that may precede it; prose does not.
fn starts_statement(masked: &[char], at: usize) -> bool {
    let mut end = at;
    // Spaces and tabs only: a newline is itself an answer, and consuming it would lose it.
    while end > 0 && (masked[end - 1] == ' ' || masked[end - 1] == '\t') {
        end -= 1;
    }
    if end == 0 {
        return true;
    }
    match masked[end - 1] {
        '\n' | '\r' | ';' | '{' | '}' => return true,
        c if c.is_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    let mut start = end;
    while start > 0 && (masked[start - 1].is_alphanumeric() || masked[start - 1] == '_') {
        start -= 1;
    }
    let word: String = masked[start..end].iter().collect();
    TS_MODIFIERS.contains(&word.as_str())
}

/// Whether what follows a name is what follows a declaration's name.
///
/// The second half of the defence above, and the half that catches prose starting a line. A
/// declared name is followed by `(`, `<`, `=`, `{`, `:`, `;` or `,` — or by `extends` /
/// `implements`, which is how a class names its parent. In `function must return a value` the word
/// after the name is another word, and no declaration reads like that.
fn looks_declared(masked: &[char], name_end: usize) -> bool {
    let at = skip_space(masked, name_end);
    match masked.get(at) {
        Some('(') | Some('<') | Some('=') | Some('{') | Some(':') | Some(';') | Some(',') => true,
        Some(_) => matches!(
            ident_at(masked, at).as_ref().map(|(word, _)| word.as_str()),
            Some("extends") | Some("implements")
        ),
        None => false,
    }
}

/// The name a TypeScript declaration gives, and the kind it settles on.
///
/// `const enum Colour` is the pair that matters here: the last keyword decides, exactly as it does
/// in Rust, and taking the word after the first one produced a constant called `enum`.
fn ts_declared(
    masked: &[char],
    mut at: usize,
    mut kind: ItemKind,
) -> Option<(ItemKind, String, usize)> {
    for _ in 0..4 {
        at = skip_space(masked, at);
        let (word, end) = ident_at(masked, at)?;
        if let Some((_, next)) = TS_KEYWORDS.iter().find(|(keyword, _)| *keyword == word) {
            kind = *next;
            at = end;
            continue;
        }
        if TS_MODIFIERS.contains(&word.as_str()) {
            at = end;
            continue;
        }
        return Some((kind, word, end));
    }
    None
}

/// The keywords that open a TypeScript declaration, and what kind each one makes.
///
/// `const` is [`ItemKind::Constant`] here and is corrected to [`ItemKind::Function`] once the
/// right-hand side is known, because in this shell most functions are `const f = () => {}` and
/// filing them all as constants would put the whole of `shell/src` in the wrong shape.
const TS_KEYWORDS: &[(&str, ItemKind)] = &[
    ("function", ItemKind::Function),
    ("class", ItemKind::Shape),
    ("interface", ItemKind::Shape),
    ("type", ItemKind::Shape),
    ("enum", ItemKind::Shape),
    ("const", ItemKind::Constant),
    ("let", ItemKind::Constant),
    ("var", ItemKind::Constant),
];

/// What a TypeScript file declares, at the top level and one level inside every `class`.
///
/// **Only at brace depth zero, which is what makes a `const` a declaration rather than a local.**
/// `const rows = ...` inside a component is a variable; the identical text at the top of the file
/// is a module constant. Nothing separates them but where they sit, so depth is tracked rather
/// than inferred, and every local skipped that way is counted into [`Items::missed`].
pub(super) fn ts_items(source: &str) -> (Vec<Found>, Vec<String>) {
    let original: Vec<char> = source.chars().collect();
    let masked = mask(source, false);
    let mut found: Vec<Found> = Vec::new();
    let mut locals = 0usize;
    let mut depth = 0i32;
    let mut i = 0;

    while i < masked.len() {
        match masked[i] {
            '{' => {
                depth += 1;
                i += 1;
                continue;
            }
            '}' => {
                depth -= 1;
                i += 1;
                continue;
            }
            _ => {}
        }
        if !word_start(&masked, i) {
            i += 1;
            continue;
        }
        let Some((word, after)) = ident_at(&masked, i) else {
            i += 1;
            continue;
        };
        let Some(kind) = TS_KEYWORDS
            .iter()
            .find(|(keyword, _)| *keyword == word)
            .map(|(_, kind)| *kind)
        else {
            i = after;
            continue;
        };
        // Prose is not a declaration, and in a `.tsx` file it sits in the same place one does.
        if !starts_statement(&masked, i) {
            i = after;
            continue;
        }
        // `const enum Colour` is a shape called `Colour`, not a constant called `enum`.
        let Some((kind, name, name_end)) = ts_declared(&masked, after, kind) else {
            i = after;
            continue;
        };
        if !looks_declared(&masked, name_end) {
            i = after;
            continue;
        }
        if depth != 0 {
            locals += 1;
            i = name_end;
            continue;
        }

        let body = ts_body(&masked, name_end);
        // `export const x = () => {}` is a function by every reading that matters, and the arrow
        // is the only thing that says so.
        let kind = if kind == ItemKind::Constant && is_arrow(&masked, name_end) {
            ItemKind::Function
        } else {
            kind
        };
        let exported = exported_before(&masked, i, "export") || after_export(&masked, i);

        if word == "class"
            && let Some((open, close)) = body
        {
            found.extend(ts_methods(&original, &masked, open, close, &name));
        }

        found.push(Found {
            item: Item {
                id: name.clone(),
                name,
                container: None,
                kind,
                exported,
                documented: documented_above(&original, i),
                line: line_at(&masked, i),
            },
            body,
        });
        // Deliberately not past the body: what is inside it is where the locals are, and the
        // count of those is the only thing standing between this picture and a file that looks
        // simpler than it is.
        i = name_end;
    }

    finish(
        found,
        locals,
        "declaration local to a function body",
        "declarations local to a function body",
    )
}

/// The methods directly inside one `class` body.
///
/// **A method has no keyword**, which is the whole difficulty: it is a name, a parameter list and
/// a body — and so is a call. The two things that separate them are that a method sits at the
/// class's own brace depth, and that `if (…) {` and its family are keywords rather than names.
fn ts_methods(
    original: &[char],
    masked: &[char],
    open: usize,
    close: usize,
    container: &str,
) -> Vec<Found> {
    const NOT_A_METHOD: &[&str] = &[
        "if", "for", "while", "switch", "catch", "return", "typeof", "new", "await", "do", "else",
    ];
    let mut found = Vec::new();
    let mut depth = 0i32;
    let mut i = open;

    while i < close && i < masked.len() {
        match masked[i] {
            '{' => {
                depth += 1;
                i += 1;
                continue;
            }
            '}' => {
                depth -= 1;
                i += 1;
                continue;
            }
            _ => {}
        }
        if depth != 1 || !word_start(masked, i) {
            i += 1;
            continue;
        }
        let Some((name, name_end)) = ident_at(masked, i) else {
            i += 1;
            continue;
        };
        if NOT_A_METHOD.contains(&name.as_str()) {
            i = name_end;
            continue;
        }
        let after_name = skip_space(masked, name_end);
        if masked.get(after_name) != Some(&'(') {
            i = name_end;
            continue;
        }
        let Some((body_open, body_close)) = ts_body(masked, name_end) else {
            i = name_end;
            continue;
        };
        if body_close > close {
            i = name_end;
            continue;
        }
        found.push(Found {
            item: Item {
                id: format!("{container}.{name}"),
                name,
                container: Some(container.to_string()),
                kind: ItemKind::Method,
                exported: false,
                documented: documented_above(original, i),
                line: line_at(masked, i),
            },
            body: Some((body_open, body_close)),
        });
        i = body_close + 1;
    }
    found
}

/// Whether what follows a TypeScript declaration's name is an arrow function.
///
/// Looks past a type annotation and a parameter list for the `=>` that makes it one, and stops at
/// the `;` or the `{` that ends the statement — so an arrow later in the file cannot reach back
/// and reclassify a plain constant.
fn is_arrow(masked: &[char], from: usize) -> bool {
    let mut i = from;
    let mut paren = 0i32;
    while i < masked.len() {
        match masked[i] {
            '(' => paren += 1,
            ')' => paren -= 1,
            '=' if masked.get(i + 1) == Some(&'>') && paren <= 0 => return true,
            ';' if paren <= 0 => return false,
            '{' if paren <= 0 => return false,
            _ => {}
        }
        i += 1;
    }
    false
}

/// The body of a TypeScript declaration: the `{` that opens it, or nothing.
///
/// Unlike Rust's, an `=` can stand between the name and the body — `const f = () => { … }` — and a
/// `;` before any `{` means there is no body at all.
fn ts_body(masked: &[char], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    let (mut paren, mut square) = (0i32, 0i32);
    while i < masked.len() {
        match masked[i] {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => square += 1,
            ']' => square -= 1,
            ';' if paren <= 0 && square <= 0 => return None,
            '{' if paren <= 0 && square <= 0 => {
                let close = match_brace(masked, i)?;
                return Some((i, close));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether `export` opens the statement this declaration is part of.
///
/// `export default function foo` and `export async function foo` both put words between the two,
/// which [`exported_before`] alone would miss. Walks back over the words allowed to sit there and
/// stops at anything else.
fn after_export(masked: &[char], at: usize) -> bool {
    const BETWEEN: &[&str] = &["default", "async", "abstract", "declare"];
    let mut end = at;
    for _ in 0..4 {
        while end > 0 && masked[end - 1].is_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (masked[start - 1].is_alphanumeric() || masked[start - 1] == '_') {
            start -= 1;
        }
        let word: String = masked[start..end].iter().collect();
        if word == "export" {
            return true;
        }
        if !BETWEEN.contains(&word.as_str()) {
            return false;
        }
        end = start;
    }
    false
}
