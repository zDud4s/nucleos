//! Owned-path globs: whether two branches' paths overlap, and whether a child's paths stay inside
//! its parent's (the L1 containment rule).

use super::{KIND_ROOT, Node, NodeError};

/// The literal text of a glob before its first wildcard, with `\` read as `/` and a leading `./`
/// dropped.
fn literal_prefix(glob: &str) -> String {
    let normalized = glob.replace('\\', "/");
    let trimmed = normalized.strip_prefix("./").unwrap_or(&normalized);
    let end = trimmed.find(['*', '?', '[', '{']).unwrap_or(trimmed.len());
    trimmed[..end].to_owned()
}

/// **Pure.** Whether two path globs may name a common file. Deliberately conservative: it compares
/// only the literal prefixes (the text before the first wildcard) and reports an overlap when one
/// is a prefix of the other, so an empty prefix overlaps everything.
pub fn globs_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (literal_prefix(a), literal_prefix(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// **Pure.** The first pair `(from a, from b)` of globs that overlap, if any.
pub fn sets_overlap(a: &[String], b: &[String]) -> Option<(String, String)> {
    for left in a {
        for right in b {
            if globs_overlap(left, right) {
                return Some((left.clone(), right.clone()));
            }
        }
    }
    None
}

/// **Pure.** A path glob in the one form containment compares: `\` read as `/`, empty and `.`
/// segments dropped (so `./a`, `a//b` and `a/b/` all clean up), and refused when it contains a `..`
/// segment, is absolute (`/x`, `C:/x`) or is left empty. Case is kept: containment is
/// case-sensitive, which on Windows can only make it refuse more, never less.
pub fn normalise_glob(raw: &str) -> Result<String, NodeError> {
    let unified = raw.trim().replace('\\', "/");
    if unified.starts_with('/') || unified.as_bytes().get(1) == Some(&b':') {
        return Err(NodeError::Invalid(format!(
            "owned path {raw:?} must be relative to the project root; specs and other files \
             under the project's state_dir are not owned by tree branches; the conductor \
             updates them"
        )));
    }
    let mut kept = Vec::new();
    for segment in unified.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                return Err(NodeError::Invalid(format!(
                    "owned path {raw:?} may not contain a \"..\" segment"
                )));
            }
            other => kept.push(other),
        }
    }
    if kept.is_empty() {
        return Err(NodeError::Invalid(format!(
            "owned path {raw:?} names no file or directory"
        )));
    }
    Ok(kept.join("/"))
}

/// Whether the text carries any glob metacharacter.
fn has_wildcard(text: &str) -> bool {
    text.contains(['*', '?', '[', '{'])
}

/// Whether one path segment matches a segment pattern: `*` is any run of characters, `?` exactly
/// one. Neither crosses a `/`, because the caller has already split on it.
fn segment_match(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((&'*', rest)) => (0..=text.len()).any(|skip| segment_match(rest, &text[skip..])),
        Some((&'?', rest)) => !text.is_empty() && segment_match(rest, &text[1..]),
        Some((literal, rest)) => text.first() == Some(literal) && segment_match(rest, &text[1..]),
    }
}

/// Whether a segment list matches a pattern segment list; a `**` segment takes zero or more whole
/// segments.
fn segments_match(pattern: &[&str], text: &[&str]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((head, rest)) if *head == "**" => {
            (0..=text.len()).any(|skip| segments_match(rest, &text[skip..]))
        }
        Some((head, rest)) => match text.split_first() {
            Some((first, tail)) => {
                let pattern_chars: Vec<char> = head.chars().collect();
                let text_chars: Vec<char> = first.chars().collect();
                segment_match(&pattern_chars, &text_chars) && segments_match(rest, tail)
            }
            None => false,
        },
    }
}

/// Whether `child` is `dir` itself or lies under it, at a segment boundary.
fn under_dir(child: &str, dir: &str) -> bool {
    child == dir
        || child
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// **Pure.** Whether every file the `child` glob can name is also named by the `parent` glob. Both
/// arguments must already be `normalise_glob` output. Conservative: when unsure, `false`.
///
/// - parent without wildcards: a file or a directory. The child is the parent itself or starts
///   with `<parent>/` (a segment boundary, so `core/srcx` is not inside `core/src`), and may carry
///   wildcards after that; the same rule as `<prefix>/**` below;
/// - parent `<prefix>/**` with a wildcard-free prefix: the child is the prefix itself, or starts
///   with `<prefix>/` (a segment boundary, so `core/srcx` is not inside `core/src/**`), and may
///   carry wildcards after that;
/// - any other wildcard form (`*.rs`, `a*/b`, `**`): the child must be literal and match the parent
///   glob, where `*` and `?` stay inside a segment and `**` crosses segments. A parent with `[` or
///   `{` contains nothing, because those forms are not understood here.
pub fn path_contained(child: &str, parent: &str) -> bool {
    if !has_wildcard(parent) {
        return under_dir(child, parent);
    }
    if let Some(prefix) = parent.strip_suffix("/**")
        && !has_wildcard(prefix)
    {
        return under_dir(child, prefix);
    }
    if has_wildcard(child) || parent.contains(['[', '{']) {
        return false;
    }
    let pattern: Vec<&str> = parent.split('/').collect();
    let text: Vec<&str> = child.split('/').collect();
    segments_match(&pattern, &text)
}

/// **Pure.** The L1 rule for a child opened under `parent`: every glob the child owns must be
/// contained in some glob the parent owns. A child with no globs (a reviewer) is always accepted.
/// A parent with no globs imposes no limit when it is the root, which owns everything, and is
/// otherwise only allowed children with no globs. A malformed child glob (`..`, absolute, blank) is
/// `Invalid`; a well-formed one outside the parent is `Conflict`.
pub(crate) fn check_containment(parent: &Node, child_paths: &[String]) -> Result<(), NodeError> {
    let children = child_paths
        .iter()
        .map(|path| normalise_glob(path))
        .collect::<Result<Vec<_>, _>>()?;
    if children.is_empty() {
        return Ok(());
    }
    if parent.owned_paths.is_empty() {
        if parent.kind == KIND_ROOT {
            return Ok(());
        }
        return Err(NodeError::Conflict(format!(
            "parent {:?} owns no paths, so it only takes children with no owned_paths",
            parent.title
        )));
    }
    // A parent glob that does not normalise contains nothing.
    let owned: Vec<String> = parent
        .owned_paths
        .iter()
        .filter_map(|path| normalise_glob(path).ok())
        .collect();
    for (raw, child) in child_paths.iter().zip(&children) {
        if !owned.iter().any(|glob| path_contained(child, glob)) {
            return Err(NodeError::Conflict(format!(
                "owned path {raw:?} is not contained in any owned path of parent {:?} ({})",
                parent.title,
                parent.owned_paths.join(", ")
            )));
        }
    }
    Ok(())
}
