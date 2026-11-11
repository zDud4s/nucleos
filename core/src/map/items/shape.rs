//! §spec 2026-10-10-atlas-layers-uml (passo 4)
//!
//! What a Rust `struct` or `enum` is made of, and which paths a file `use`s: the two extra facts
//! the class/module diagram needs beyond what [`super::items`] already says.
//!
//! Same discipline as the rest of `map/items`: it reads the **masked** copy of the source, so a
//! comment or a string never produces a member, and it is not a parser — a member list is a
//! comma-split of a body, and a `use` tree is flattened by a token walk. What it cannot read it
//! leaves out rather than invents.

use super::{body_of, ident_at, line_at, skip_space, word_start};
use serde::Serialize;

/// Whether a member is a struct field or an enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberKind {
    Field,
    Variant,
}

/// One field of a struct, or one variant of an enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Member {
    /// The id of the [`super::Item`] this belongs to: the struct's or the enum's name.
    pub owner: String,
    /// The field's name (`0`, `1`… for a tuple struct) or the variant's.
    pub name: String,
    pub kind: MemberKind,
    /// A field's type as written, whitespace collapsed. A variant's payload as written —
    /// `(u8, String)` or `{ a: u8 }` — and `None` for a unit variant. `None` for a variant with a
    /// discriminant (`A = 1`) too: the number is not a type.
    pub ty: Option<String>,
    /// A field written `pub` in any form. Always `false` for a variant.
    pub exported: bool,
    /// 1-based line of the member's name.
    pub line: usize,
}

/// One path a file `use`s, flattened: `use a::{b, c::D};` is `a::b` and `a::c::D`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Use {
    /// As written, minus any `as` rename; a glob keeps its `*` (`super::*`).
    pub path: String,
    /// 1-based line of the `use` keyword.
    pub line: usize,
}

/// Every `struct` and `enum` body in the file, as a flat list of members.
///
/// Scans the whole file, so a type declared inside a function is read too; the caller keeps only
/// the members whose owner is an item it actually draws.
pub(super) fn shape_members(masked: &[char]) -> Vec<Member> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < masked.len() {
        if !word_start(masked, i) {
            i += 1;
            continue;
        }
        let Some((word, after)) = ident_at(masked, i) else {
            i += 1;
            continue;
        };
        i = after;
        let is_enum = word == "enum";
        if !is_enum && word != "struct" {
            continue;
        }
        let Some((owner, name_end)) = ident_at(masked, skip_space(masked, after)) else {
            continue;
        };
        i = name_end;

        let mut at = skip_space(masked, name_end);
        if masked.get(at) == Some(&'<') {
            at = skip_space(masked, skip_angle(masked, at));
        }
        let (open, close, tuple) = if !is_enum && masked.get(at) == Some(&'(') {
            match close_of(masked, at, '(', ')') {
                Some(close) => (at, close, true),
                None => continue,
            }
        } else if let Some((open, close)) = body_of(masked, name_end) {
            (open, close, false)
        } else {
            continue;
        };

        for (index, (start, end)) in split_top(masked, open + 1, close).into_iter().enumerate() {
            let at = skip_attributes(masked, skip_space(masked, start), end);
            if at >= end {
                continue;
            }
            let member = if is_enum {
                variant(masked, &owner, at, end)
            } else {
                field(masked, &owner, at, end, tuple.then_some(index))
            };
            out.extend(member);
        }
    }
    out
}

fn field(
    masked: &[char],
    owner: &str,
    at: usize,
    end: usize,
    tuple_index: Option<usize>,
) -> Option<Member> {
    let (at, exported) = skip_visibility(masked, at, end);
    let (name, ty_from, name_at) = match tuple_index {
        Some(index) => (index.to_string(), at, at),
        None => {
            let (name, name_end) = ident_at(masked, at)?;
            let colon = skip_space(masked, name_end);
            if masked.get(colon) != Some(&':') {
                return None;
            }
            (name, colon + 1, at)
        }
    };
    let ty = collapse(masked, ty_from, end);
    if ty.is_empty() {
        return None;
    }
    Some(Member {
        owner: owner.to_string(),
        name,
        kind: MemberKind::Field,
        ty: Some(ty),
        exported,
        line: line_at(masked, name_at),
    })
}

fn variant(masked: &[char], owner: &str, at: usize, end: usize) -> Option<Member> {
    let (name, name_end) = ident_at(masked, at)?;
    let rest = collapse(masked, name_end, end);
    let ty = (!rest.is_empty() && !rest.starts_with('=')).then_some(rest);
    Some(Member {
        owner: owner.to_string(),
        name,
        kind: MemberKind::Variant,
        ty,
        exported: false,
        line: line_at(masked, at),
    })
}

/// The text of `[from, to)` with every run of whitespace made one space, and trimmed.
fn collapse(masked: &[char], from: usize, to: usize) -> String {
    let text: String = masked[from..to.min(masked.len())].iter().collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Past any `#[...]` attributes starting at `at`.
fn skip_attributes(masked: &[char], mut at: usize, end: usize) -> usize {
    while at < end && masked[at] == '#' {
        let open = skip_space(masked, at + 1);
        if masked.get(open) != Some(&'[') {
            break;
        }
        match close_of(masked, open, '[', ']') {
            Some(close) => at = skip_space(masked, close + 1),
            None => break,
        }
    }
    at
}

/// Past a leading `pub` / `pub(crate)` / `pub(in path)`, and whether there was one.
fn skip_visibility(masked: &[char], at: usize, end: usize) -> (usize, bool) {
    let Some((word, after)) = ident_at(masked, at) else {
        return (at, false);
    };
    if word != "pub" || after > end {
        return (at, false);
    }
    let next = skip_space(masked, after);
    if masked.get(next) == Some(&'(')
        && let Some(close) = close_of(masked, next, '(', ')')
    {
        return (skip_space(masked, close + 1), true);
    }
    (next, true)
}

/// The index of the bracket closing the one at `open`.
fn close_of(masked: &[char], open: usize, opener: char, closer: char) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in masked.iter().enumerate().skip(open) {
        if *c == opener {
            depth += 1;
        } else if *c == closer {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The index just past the `>` closing the `<` at `open`. `->` and `=>` do not close it.
fn skip_angle(masked: &[char], open: usize) -> usize {
    let mut depth = 0i32;
    let mut i = open;
    while i < masked.len() {
        match masked[i] {
            '<' => depth += 1,
            '>' if i > 0 && masked[i - 1] != '-' && masked[i - 1] != '=' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    masked.len()
}

/// The `[start, end)` ranges of a body's comma-separated entries, commas inside any bracket or
/// generic list not splitting.
fn split_top(masked: &[char], from: usize, to: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut round, mut angle) = (0i32, 0i32);
    let mut start = from;
    for (i, c) in masked.iter().enumerate().take(to).skip(from) {
        match *c {
            '(' | '[' | '{' => round += 1,
            ')' | ']' | '}' => round -= 1,
            '<' => angle += 1,
            '>' if angle > 0 && i > 0 && masked[i - 1] != '-' && masked[i - 1] != '=' => angle -= 1,
            ',' if round == 0 && angle == 0 => {
                out.push((start, i));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push((start, to));
    out
}

#[derive(Debug, PartialEq)]
enum Tok {
    Ident(String),
    Sep,
    Open,
    Close,
    Comma,
    Star,
}

/// Every `use` declaration in the file, flattened to one path per leaf.
pub(super) fn rust_uses(masked: &[char]) -> Vec<Use> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < masked.len() {
        if !word_start(masked, i) {
            i += 1;
            continue;
        }
        let Some((word, after)) = ident_at(masked, i) else {
            i += 1;
            continue;
        };
        if word != "use" {
            i = after;
            continue;
        }
        let Some(semi) = (after..masked.len()).find(|at| masked[*at] == ';') else {
            break;
        };
        let line = line_at(masked, i);
        let tokens = tokenize(&masked[after..semi]);
        let mut pos = 0;
        let mut paths = Vec::new();
        walk(&tokens, &mut pos, &[], &mut paths);
        out.extend(paths.into_iter().map(|path| Use { path, line }));
        i = semi + 1;
    }
    out
}

fn tokenize(chars: &[char]) -> Vec<Tok> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' => tokens.push(Tok::Open),
            '}' => tokens.push(Tok::Close),
            ',' => tokens.push(Tok::Comma),
            '*' => tokens.push(Tok::Star),
            ':' => {
                tokens.push(Tok::Sep);
                if chars.get(i + 1) == Some(&':') {
                    i += 1;
                }
            }
            c if c.is_alphanumeric() || c == '_' => {
                let (word, end) = ident_at(chars, i).expect("an identifier starts here");
                i = end;
                if word == "as" {
                    // The rename is not part of the path.
                    let next = skip_space(chars, i);
                    if let Some((_, end)) = ident_at(chars, next) {
                        i = end;
                    }
                } else {
                    tokens.push(Tok::Ident(word));
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    tokens
}

/// Walks one comma-separated list of a `use` tree, pushing a flattened path for each leaf and
/// returning after the `}` that ends the list (or at the end of input).
fn walk(tokens: &[Tok], pos: &mut usize, prefix: &[String], out: &mut Vec<String>) {
    loop {
        let mut path = prefix.to_vec();
        let (mut leaf, mut nested) = (false, false);
        while let Some(token) = tokens.get(*pos) {
            match token {
                Tok::Ident(word) => {
                    if word != "self" {
                        path.push(word.clone());
                    }
                    leaf = true;
                }
                Tok::Star => {
                    path.push("*".to_string());
                    leaf = true;
                }
                Tok::Sep => {}
                Tok::Open => {
                    *pos += 1;
                    walk(tokens, pos, &path, out);
                    nested = true;
                    break;
                }
                Tok::Comma | Tok::Close => break,
            }
            *pos += 1;
        }
        if leaf && !nested && !path.is_empty() {
            out.push(path.join("::"));
        }
        match tokens.get(*pos) {
            Some(Tok::Comma) => *pos += 1,
            Some(Tok::Close) => {
                *pos += 1;
                return;
            }
            _ => return,
        }
    }
}
