//! §spec workflow-auto-afinacao
//!
//! Reads and rewrites ONE scalar of a bundle's YAML state file (`project.yaml`, `models.yaml`)
//! in place. `serde_yaml` would drop every comment of those files, so the edit is a line scan:
//! only the scalar token changes; the key, the indentation, a trailing comment and the line ending
//! stay as they were. Pure, string in and string out; the caller does the I/O.
//!
//! A key is 1 or 2 dotted segments. Only plain `key: scalar` lines are edited; block scalars, flow
//! collections, anchors and tags are refused, as are duplicate keys and multi-document files.
//! After a write the result is parsed again and must read back as the written value.

use std::fmt;

use serde_json::Value;

/// Why a file could not be read or edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    /// The value at the key is not a plain single-line scalar.
    NotAScalar(String),
    /// The file, the key or the edited result does not parse as expected.
    Unparseable(String),
    /// The key occurs twice at one level, or the file holds several documents.
    Ambiguous(String),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAScalar(m) => write!(f, "not a plain scalar: {m}"),
            Self::Unparseable(m) => write!(f, "unparseable: {m}"),
            Self::Ambiguous(m) => write!(f, "ambiguous: {m}"),
        }
    }
}

impl std::error::Error for FileError {}

struct Line<'a> {
    /// The line without its ending.
    body: &'a str,
    /// `"\n"`, `"\r\n"` or `""` (last line without a newline).
    ending: &'a str,
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    text.split_inclusive('\n')
        .map(|raw| {
            if let Some(body) = raw.strip_suffix("\r\n") {
                Line {
                    body,
                    ending: "\r\n",
                }
            } else if let Some(body) = raw.strip_suffix('\n') {
                Line { body, ending: "\n" }
            } else {
                Line {
                    body: raw,
                    ending: "",
                }
            }
        })
        .collect()
}

fn indent(body: &str) -> usize {
    body.len() - body.trim_start_matches(' ').len()
}

fn is_blank_or_comment(body: &str) -> bool {
    let t = body.trim();
    t.is_empty() || t.starts_with('#')
}

fn segments(key: &str) -> Result<Vec<&str>, FileError> {
    let segs: Vec<&str> = key.split('.').collect();
    if segs.is_empty() || segs.len() > 2 || segs.iter().any(|s| s.is_empty()) {
        return Err(FileError::Unparseable(format!("bad key `{key}`")));
    }
    Ok(segs)
}

/// The value text after `name:` when the line is `<indent>name:<rest>`.
fn key_rest<'a>(body: &'a str, name: &str, at_indent: usize) -> Option<&'a str> {
    if indent(body) != at_indent {
        return None;
    }
    let after = body[at_indent..].strip_prefix(name)?;
    let after = after.trim_start_matches([' ', '\t']);
    let rest = after.strip_prefix(':')?;
    if rest.is_empty() || rest.starts_with([' ', '\t']) {
        Some(rest)
    } else {
        None
    }
}

fn check_single_document(lines: &[Line<'_>]) -> Result<(), FileError> {
    let mut seen_content = false;
    for l in lines {
        if (l.body.starts_with("---") || l.body.starts_with("...")) && seen_content {
            return Err(FileError::Ambiguous("multi-document file".into()));
        }
        if !is_blank_or_comment(l.body) && !l.body.starts_with("---") && !l.body.starts_with('%') {
            seen_content = true;
        }
    }
    Ok(())
}

/// Byte range (inside `rest`) of the scalar token, or `NotAScalar`.
fn scalar_span(rest: &str) -> Result<(usize, usize), FileError> {
    let start = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let tail = &rest[start..];
    let first = tail.chars().next();
    let bad = |why: &str| FileError::NotAScalar(why.to_owned());
    let end = match first {
        None => return Err(bad("empty value")),
        Some('#') => return Err(bad("empty value")),
        Some('|' | '>') => return Err(bad("block scalar")),
        Some('[' | '{') => return Err(bad("flow collection")),
        Some('&' | '*') => return Err(bad("anchor or alias")),
        Some('!') => return Err(bad("tagged value")),
        Some(q @ ('"' | '\'')) => {
            let b = tail.as_bytes();
            let mut i = 1;
            let close = loop {
                if i >= b.len() {
                    return Err(bad("unterminated quoted scalar"));
                }
                if q == '"' && b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == q as u8 {
                    if q == '\'' && b.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    break i + 1;
                }
                i += 1;
            };
            let after = tail[close..].trim_start_matches([' ', '\t']);
            if !(after.is_empty() || after.starts_with('#')) {
                return Err(bad("text after a quoted scalar"));
            }
            close
        }
        Some(_) => {
            let mut end = tail.len();
            for (i, _) in tail.match_indices('#') {
                if i > 0 && matches!(tail.as_bytes()[i - 1], b' ' | b'\t') {
                    end = i;
                    break;
                }
            }
            tail[..end].trim_end().len()
        }
    };
    Ok((start, start + end))
}

/// A located key: the line index, plus the scalar span inside that line's body when it holds one.
struct Found {
    line: usize,
    /// Byte range of the scalar token in the line body.
    span: (usize, usize),
}

enum Lookup {
    Found(Found),
    /// The key is absent; `parent` is the line of an existing first segment (2-segment keys).
    Absent {
        parent: Option<usize>,
    },
}

/// Whether the line after `idx` continues a multi-line scalar (more indented text).
fn continues_scalar(lines: &[Line<'_>], idx: usize, key_indent: usize) -> bool {
    lines[idx + 1..]
        .iter()
        .find(|l| !is_blank_or_comment(l.body))
        .is_some_and(|l| indent(l.body) > key_indent)
}

fn locate(lines: &[Line<'_>], segs: &[&str]) -> Result<Lookup, FileError> {
    let tops: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| key_rest(l.body, segs[0], 0).is_some())
        .map(|(i, _)| i)
        .collect();
    if tops.len() > 1 {
        return Err(FileError::Ambiguous(format!("`{}` occurs twice", segs[0])));
    }
    let Some(&top) = tops.first() else {
        return Ok(Lookup::Absent { parent: None });
    };
    let top_rest = key_rest(lines[top].body, segs[0], 0).unwrap_or("");
    let found =
        |idx: usize, key_indent: usize, rest: &str, offset: usize| -> Result<Lookup, FileError> {
            let (s, e) = scalar_span(rest)?;
            if continues_scalar(lines, idx, key_indent) {
                return Err(FileError::NotAScalar("multi-line scalar".into()));
            }
            Ok(Lookup::Found(Found {
                line: idx,
                span: (offset + s, offset + e),
            }))
        };
    if segs.len() == 1 {
        let offset = lines[top].body.len() - top_rest.len();
        return found(top, 0, top_rest, offset);
    }
    // Two segments: the first must be a block header with no inline value.
    let inline = top_rest.trim();
    if !(inline.is_empty() || inline.starts_with('#')) {
        return Err(FileError::NotAScalar(format!(
            "`{}` is not a block mapping",
            segs[0]
        )));
    }
    let mut child_indent = None;
    let mut matches = Vec::new();
    for (i, l) in lines.iter().enumerate().skip(top + 1) {
        if is_blank_or_comment(l.body) {
            continue;
        }
        let ind = indent(l.body);
        if ind == 0 {
            break;
        }
        let ci = *child_indent.get_or_insert(ind);
        if ind == ci && key_rest(l.body, segs[1], ci).is_some() {
            matches.push((i, ci));
        }
    }
    if matches.len() > 1 {
        return Err(FileError::Ambiguous(format!(
            "`{}.{}` occurs twice",
            segs[0], segs[1]
        )));
    }
    match matches.first() {
        None => Ok(Lookup::Absent { parent: Some(top) }),
        Some(&(idx, ci)) => {
            let rest = key_rest(lines[idx].body, segs[1], ci).unwrap_or("");
            let offset = lines[idx].body.len() - rest.len();
            found(idx, ci, rest, offset)
        }
    }
}

fn yaml_to_json(token: &str) -> Result<Value, FileError> {
    let y: serde_yaml::Value =
        serde_yaml::from_str(token).map_err(|e| FileError::Unparseable(e.to_string()))?;
    serde_json::to_value(y).map_err(|e| FileError::Unparseable(e.to_string()))
}

/// The scalar at the dotted `key`; `None` when the key (or its parent) is absent.
pub fn read(text: &str, key: &str) -> Result<Option<Value>, FileError> {
    let segs = segments(key)?;
    let lines = split_lines(text);
    check_single_document(&lines)?;
    match locate(&lines, &segs)? {
        Lookup::Absent { .. } => Ok(None),
        Lookup::Found(f) => {
            let (s, e) = f.span;
            yaml_to_json(&lines[f.line].body[s..e]).map(Some)
        }
    }
}

fn render(value: &Value) -> Result<String, FileError> {
    match value {
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => {
            if n.is_f64() {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if !f.is_finite() {
                    return Err(FileError::Unparseable("non-finite number".into()));
                }
                let mut s = format!("{f}");
                if !s.contains(['.', 'e', 'E']) {
                    s.push_str(".0");
                }
                Ok(s)
            } else {
                Ok(n.to_string())
            }
        }
        Value::String(s) => {
            // The bundle's scripts read these files with PyYAML (YAML 1.1), where words
            // serde_yaml (YAML 1.2) keeps as strings still parse as booleans or null.
            let yaml11_word = matches!(
                s.to_ascii_lowercase().as_str(),
                "y" | "n" | "yes" | "no" | "on" | "off" | "true" | "false" | "null" | "~"
            );
            let plain = !s.is_empty()
                && !yaml11_word
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
                && matches!(
                    serde_yaml::from_str::<serde_yaml::Value>(s),
                    Ok(serde_yaml::Value::String(ref r)) if r == s
                );
            if plain {
                Ok(s.clone())
            } else {
                serde_json::to_string(s).map_err(|e| FileError::Unparseable(e.to_string()))
            }
        }
        other => Err(FileError::NotAScalar(format!("cannot write {other}"))),
    }
}

/// The same text with only that scalar replaced, or inserted when the key is absent.
pub fn write(text: &str, key: &str, value: &Value) -> Result<String, FileError> {
    let segs = segments(key)?;
    let lines = split_lines(text);
    check_single_document(&lines)?;
    let token = render(value)?;
    let eol = lines
        .iter()
        .find(|l| !l.ending.is_empty())
        .map_or("\n", |l| l.ending);

    let out = match locate(&lines, &segs)? {
        Lookup::Found(f) => {
            let mut s = String::with_capacity(text.len() + token.len());
            for (i, l) in lines.iter().enumerate() {
                if i == f.line {
                    s.push_str(&l.body[..f.span.0]);
                    s.push_str(&token);
                    s.push_str(&l.body[f.span.1..]);
                } else {
                    s.push_str(l.body);
                }
                s.push_str(l.ending);
            }
            s
        }
        Lookup::Absent { parent: Some(top) } => {
            // Insert after the last real line of the block (or right after the header when empty).
            let mut last = top;
            let mut child_indent = 2;
            let mut seen_child = false;
            for (i, l) in lines.iter().enumerate().skip(top + 1) {
                if is_blank_or_comment(l.body) {
                    continue;
                }
                let ind = indent(l.body);
                if ind == 0 {
                    break;
                }
                if !seen_child {
                    child_indent = ind;
                    seen_child = true;
                }
                last = i;
            }
            let mut s = String::with_capacity(text.len() + token.len() + 16);
            for (i, l) in lines.iter().enumerate() {
                s.push_str(l.body);
                if i == last {
                    s.push_str(if l.ending.is_empty() { eol } else { l.ending });
                    s.push_str(&format!("{}{}: {token}", " ".repeat(child_indent), segs[1]));
                    s.push_str(l.ending);
                } else {
                    s.push_str(l.ending);
                }
            }
            s
        }
        Lookup::Absent { parent: None } => {
            let mut s = text.to_owned();
            if !s.is_empty() && !s.ends_with('\n') {
                s.push_str(eol);
            }
            if segs.len() == 1 {
                s.push_str(&format!("{}: {token}{eol}", segs[0]));
            } else {
                s.push_str(&format!("{}:{eol}  {}: {token}{eol}", segs[0], segs[1]));
            }
            s
        }
    };

    match read(&out, key) {
        Ok(Some(back)) if &back == value => Ok(out),
        Ok(other) => Err(FileError::Unparseable(format!(
            "edited file reads back {other:?}, not {value}"
        ))),
        Err(e) => Err(e),
    }
    .and_then(|out| {
        serde_yaml::from_str::<serde_yaml::Value>(&out)
            .map(|_| out)
            .map_err(|e| FileError::Unparseable(e.to_string()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SAMPLE: &str = "\
# project settings
name: demo

tree:
  # how many branches run at once
  max_live_branches: 6  # owner
  links: [a, b]
  mode: fast

parallel_execute:
  worker: { tool: claude, model: opus }  # flow mapping
  max_parallel: 3 # trailing
  label: \"x # y\"

max_parallel: 9
";

    #[test]
    fn read_finds_a_two_segment_scalar_and_ignores_the_trailing_comment() {
        assert_eq!(
            read(SAMPLE, "tree.max_live_branches").unwrap(),
            Some(json!(6))
        );
        assert_eq!(
            read(SAMPLE, "parallel_execute.max_parallel").unwrap(),
            Some(json!(3))
        );
        assert_eq!(
            read(SAMPLE, "parallel_execute.label").unwrap(),
            Some(json!("x # y"))
        );
        assert_eq!(read(SAMPLE, "name").unwrap(), Some(json!("demo")));
    }

    #[test]
    fn read_of_an_absent_key_or_parent_is_none() {
        assert_eq!(read(SAMPLE, "tree.nope").unwrap(), None);
        assert_eq!(read(SAMPLE, "nothing.here").unwrap(), None);
        assert_eq!(read(SAMPLE, "nothing").unwrap(), None);
    }

    #[test]
    fn write_replaces_only_the_scalar_and_keeps_comment_indent_and_neighbours() {
        assert_eq!(
            write(SAMPLE, "tree.max_live_branches", &json!(8)).unwrap(),
            SAMPLE.replace("6  # owner", "8  # owner")
        );
        assert_eq!(
            write(SAMPLE, "parallel_execute.max_parallel", &json!(5)).unwrap(),
            SAMPLE.replace("3 # trailing", "5 # trailing")
        );
        assert_eq!(
            write(SAMPLE, "parallel_execute.label", &json!("ab")).unwrap(),
            SAMPLE.replace("\"x # y\"", "ab")
        );
    }

    #[test]
    fn write_keeps_crlf_line_endings() {
        let crlf = SAMPLE.replace('\n', "\r\n");
        let out = write(&crlf, "tree.max_live_branches", &json!(8)).unwrap();
        assert_eq!(out, crlf.replace("6  # owner", "8  # owner"));
        let out = write(&crlf, "tree.added", &json!(true)).unwrap();
        assert!(out.contains("  added: true\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn write_inserts_a_missing_child_at_the_end_of_an_existing_block() {
        let out = write(SAMPLE, "tree.depth", &json!(2)).unwrap();
        assert_eq!(
            out,
            SAMPLE.replace("  mode: fast\n", "  mode: fast\n  depth: 2\n")
        );
        let empty = "tree:\nother: 1\n";
        assert_eq!(
            write(empty, "tree.depth", &json!(2)).unwrap(),
            "tree:\n  depth: 2\nother: 1\n"
        );
    }

    #[test]
    fn write_appends_a_missing_parent_block_and_adds_the_final_newline() {
        assert_eq!(
            write("a: 1", "tree.depth", &json!(2)).unwrap(),
            "a: 1\ntree:\n  depth: 2\n"
        );
        assert_eq!(write("a: 1\n", "b", &json!(0.5)).unwrap(), "a: 1\nb: 0.5\n");
        assert_eq!(write("", "b", &json!(1.0)).unwrap(), "b: 1.0\n");
    }

    #[test]
    fn write_refuses_block_flow_anchor_and_tagged_values_and_changes_nothing() {
        for bad in [
            "k: |\n  text\n",
            "k: >\n  text\n",
            "k: [1, 2]\n",
            "k: { a: 1 }\n",
            "k: &a 1\n",
            "k: *a\n",
            "k: !!str 1\n",
            "k: word\n  continued\n",
            "k:\n  nested: 1\n",
        ] {
            let err = write(bad, "k", &json!(1)).unwrap_err();
            assert!(matches!(err, FileError::NotAScalar(_)), "{bad:?}: {err}");
        }
        assert!(matches!(
            write(SAMPLE, "tree.links", &json!(1)),
            Err(FileError::NotAScalar(_))
        ));
        assert!(matches!(
            write(SAMPLE, "parallel_execute.worker", &json!(1)),
            Err(FileError::NotAScalar(_))
        ));
    }

    #[test]
    fn write_refuses_duplicate_keys_and_multi_document_files() {
        assert!(matches!(
            write("a: 1\na: 2\n", "a", &json!(3)),
            Err(FileError::Ambiguous(_))
        ));
        assert!(matches!(
            write("t:\n  a: 1\n  a: 2\n", "t.a", &json!(3)),
            Err(FileError::Ambiguous(_))
        ));
        assert!(matches!(
            write("a: 1\n---\nb: 2\n", "a", &json!(3)),
            Err(FileError::Ambiguous(_))
        ));
        // A leading document marker is a single document.
        assert!(write("---\na: 1\n", "a", &json!(3)).is_ok());
    }

    #[test]
    fn write_quotes_a_string_that_would_parse_as_another_type() {
        for (s, quoted) in [
            ("yes", "\"yes\""),
            ("123", "\"123\""),
            ("null", "\"null\""),
            ("true", "\"true\""),
            ("a b", "\"a b\""),
            ("", "\"\""),
        ] {
            let out = write("k: x\n", "k", &json!(s)).unwrap();
            assert_eq!(out, format!("k: {quoted}\n"));
        }
        assert_eq!(
            write("k: x\n", "k", &json!("fast-1")).unwrap(),
            "k: fast-1\n"
        );
    }

    #[test]
    fn a_key_with_a_dot_in_a_different_block_is_not_confused() {
        // `max_parallel` exists at the top level and inside `parallel_execute`.
        assert_eq!(
            read(SAMPLE, "parallel_execute.max_parallel").unwrap(),
            Some(json!(3))
        );
        assert_eq!(read(SAMPLE, "max_parallel").unwrap(), Some(json!(9)));
        let out = write(SAMPLE, "max_parallel", &json!(10)).unwrap();
        assert_eq!(out, SAMPLE.replace("max_parallel: 9", "max_parallel: 10"));
        assert_eq!(read(SAMPLE, "tree.max_parallel").unwrap(), None);
    }

    #[test]
    fn the_result_round_trips_through_serde_yaml() {
        for (key, v) in [
            ("tree.max_live_branches", json!(12)),
            ("tree.ratio", json!(0.25)),
            ("tree.on", json!(false)),
            ("tree.mode", json!("slow")),
            ("fresh.child", json!(1)),
        ] {
            let out = write(SAMPLE, key, &v).unwrap();
            assert_eq!(read(&out, key).unwrap(), Some(v));
            serde_yaml::from_str::<serde_yaml::Value>(&out).unwrap();
        }
    }
}
