//! Closed-subset PowerShell scanner. Reads the first word of every simple command; anything it
//! does not recognise is an `Err` (fail closed) and falls through to the resident parser.
//!
//! Layout: this file holds the shared state and the entry point, `scanner_lex` the lexing
//! (words, strings, here-strings, comments), `scanner_stmt` statements, groups and expressions.

/// Result type shared by the scanner modules.
pub(super) type Res<T = ()> = Result<T, String>;

/// Maximum nesting of `(`, `$(`, `@(`, `{`, `@{`, `[`: deeper input is refused, never recursed into.
const MAX_DEPTH: usize = 64;

/// Larger inputs are refused outright (bytes).
pub(super) const MAX_INPUT: usize = 256 * 1024;

pub(super) struct Scanner {
    chars: Vec<char>,
    pub(super) pos: usize,
    depth: usize,
    /// Whether the next `expression()` may be an assignment; consumed (reset to true) on entry.
    pub(super) allow_assign: bool,
    pub(super) names: Vec<String>,
    /// How many `$( )` embedded in a double-quoted string the scanner is inside.
    pub(super) dq_sub: usize,
    /// How many list commas `token` has consumed so far (a redirect target may not use one).
    pub(super) commas: usize,
}

impl Scanner {
    fn new(src: &str) -> Self {
        Scanner {
            chars: src.chars().collect(),
            pos: 0,
            depth: 0,
            allow_assign: true,
            names: Vec::new(),
            dq_sub: 0,
            commas: 0,
        }
    }

    pub(super) fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    pub(super) fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    /// The char just before the current position.
    pub(super) fn prev(&self) -> Option<char> {
        self.pos
            .checked_sub(1)
            .and_then(|i| self.chars.get(i).copied())
    }

    /// Whether the raw `(`/`)` chars of `from..to` (a `$( )` span, quotes and comments ignored)
    /// close the first paren exactly at the last char, `to - 1`.
    pub(super) fn raw_parens_close_at_end(&self, from: usize, to: usize) -> bool {
        let mut depth = 0usize;
        for (i, c) in self.chars[from..to]
            .iter()
            .enumerate()
            .map(|(i, c)| (from + i, c))
        {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return i + 1 == to;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Source text between two char positions.
    pub(super) fn text(&self, from: usize, to: usize) -> String {
        self.chars[from..to].iter().collect()
    }

    /// Runs `f` one nesting level deeper; refuses past `MAX_DEPTH`.
    pub(super) fn nested(&mut self, f: fn(&mut Self) -> Res) -> Res {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("nesting too deep".to_string());
        }
        let r = f(self);
        self.depth -= 1;
        r
    }
}

/// First word of every simple command, document order. Anything not recognised is `Err`.
pub(super) fn command_names(src: &str) -> Result<Vec<String>, String> {
    if src.len() > MAX_INPUT {
        return Err("input too large".to_string());
    }
    if src.contains('\0') {
        return Err("NUL byte".to_string());
    }
    let mut s = Scanner::new(src);
    s.statement_list()?;
    if let Some(c) = s.peek() {
        return Err(format!("unexpected {c:?} at {}", s.pos));
    }
    Ok(s.names)
}
