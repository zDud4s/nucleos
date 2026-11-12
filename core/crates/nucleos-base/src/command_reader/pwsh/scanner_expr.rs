//! Scanner expressions: operands, operators, member access, indexing, assignments. Only the
//! commands nested in groups are collected; a bareword where an operand is expected is an `Err`.

use super::scanner::{Res, Scanner};
use super::scanner_lex::{is_dq, is_sq};

/// Operators written `-word` that take two operands.
const BINARY_WORDS: &[&str] = &[
    "eq",
    "ne",
    "gt",
    "ge",
    "lt",
    "le",
    "like",
    "notlike",
    "match",
    "notmatch",
    "replace",
    "contains",
    "notcontains",
    "in",
    "notin",
    "is",
    "isnot",
    "as",
    "and",
    "or",
    "xor",
    "band",
    "bor",
    "bxor",
    "shl",
    "shr",
    "join",
    "split",
    "f",
];

/// Comparison operators that also exist with a `c` (case-sensitive) or `i` prefix.
const CASE_WORDS: &[&str] = &[
    "eq",
    "ne",
    "gt",
    "ge",
    "lt",
    "le",
    "like",
    "notlike",
    "match",
    "notmatch",
    "replace",
    "contains",
    "notcontains",
    "split",
];

/// Operators written `-word` that take one operand.
const UNARY_WORDS: &[&str] = &["not", "bnot", "join", "split"];

fn is_binary_word(w: &str) -> bool {
    BINARY_WORDS.contains(&w)
        || (w.starts_with(['c', 'i']) && w.get(1..).is_some_and(|rest| CASE_WORDS.contains(&rest)))
}

enum Operand {
    /// A finished value; the flag says whether it can be assigned to.
    Value(bool),
    /// A type literal that casts whatever operand follows.
    Cast,
    /// A unary operator: an operand still follows.
    Prefix,
}

impl Scanner {
    /// Lowercased letters after a `-` at the current position (`-eq` gives `eq`).
    fn dash_word(&self) -> String {
        let mut w = String::new();
        let mut i = 1;
        while let Some(c) = self.peek_at(i) {
            if !c.is_ascii_alphabetic() {
                break;
            }
            w.push(c.to_ascii_lowercase());
            i += 1;
        }
        w
    }

    /// Length of an assignment operator at the current position, if any.
    fn assign_op_len(&self) -> Option<usize> {
        match (self.peek(), self.peek_at(1), self.peek_at(2)) {
            (Some('='), _, _) => Some(1),
            (Some('+' | '-' | '*' | '/' | '%'), Some('='), _) => Some(2),
            (Some('?'), Some('?'), Some('=')) => Some(3),
            _ => None,
        }
    }

    /// An expression, up to a statement/pipeline boundary or a closing bracket (left unconsumed).
    /// An assignment hands the right-hand side to the pipeline scanner.
    pub(super) fn expression(&mut self) -> Res {
        let assign_ok = std::mem::replace(&mut self.allow_assign, true);
        let mut expect_operand = true;
        let mut assignable = true;
        loop {
            self.skip_inline()?;
            if expect_operand {
                match self.operand()? {
                    Operand::Value(a) => {
                        assignable &= a;
                        expect_operand = false;
                    }
                    Operand::Cast => {}
                    Operand::Prefix => assignable = false,
                }
                continue;
            }
            let next = self.peek_at(1);
            match self.peek() {
                None | Some('|' | ';' | '\n' | '\r' | ')' | '}' | ']' | '&' | '>') => return Ok(()),
                Some('*') if next == Some('>') => return Ok(()),
                Some(',') => {
                    self.pos += 1;
                    self.skip_nl()?;
                    expect_operand = true;
                }
                _ if self.assign_op_len().is_some() => {
                    let len = self.assign_op_len().unwrap_or(1);
                    if !assignable {
                        return Err("assignment to a non-variable".to_string());
                    }
                    // pwsh: an assignment is a statement, not a pipeline element after `|`/`&&`/`||`.
                    if !assign_ok {
                        return Err("assignment inside a pipeline".to_string());
                    }
                    self.pos += len;
                    self.skip_nl()?;
                    return self.assigned_value();
                }
                Some('+') if next == Some('+') => {
                    self.pos += 2;
                    assignable = false;
                }
                Some('-') if next == Some('-') => {
                    self.pos += 2;
                    assignable = false;
                }
                Some('+' | '*' | '/' | '%') => {
                    self.pos += 1;
                    expect_operand = true;
                    assignable = false;
                }
                Some('?') if next == Some('?') => {
                    self.pos += 2;
                    expect_operand = true;
                    assignable = false;
                }
                Some('.') if next == Some('.') => {
                    self.pos += 2;
                    expect_operand = true;
                    assignable = false;
                }
                Some('-') => {
                    let word = self.dash_word();
                    if word.is_empty() {
                        self.pos += 1;
                    } else if is_binary_word(&word) {
                        self.pos += 1 + word.len();
                    } else {
                        return Err(format!("unknown operator -{word}"));
                    }
                    expect_operand = true;
                    assignable = false;
                }
                Some(c) => return Err(format!("unexpected {c:?} in expression at {}", self.pos)),
            }
        }
    }

    fn operand(&mut self) -> Res<Operand> {
        let next = self.peek_at(1);
        let assignable = match self.peek() {
            Some('$') => match next {
                Some('(') => {
                    self.sub_expression()?;
                    false
                }
                Some('{') => {
                    self.braced_variable()?;
                    true
                }
                _ => {
                    self.variable()?;
                    true
                }
            },
            Some('[') => {
                self.type_literal()?;
                if self.peek() == Some(':') && self.peek_at(1) == Some(':') {
                    true
                } else {
                    return Ok(Operand::Cast);
                }
            }
            Some(c) if is_sq(c) => {
                self.single_quoted()?;
                false
            }
            Some(c) if is_dq(c) => {
                self.double_quoted()?;
                false
            }
            Some('@') => {
                match next {
                    Some('(') => self.array_group()?,
                    Some('{') => self.hashtable()?,
                    Some('\'' | '"') => self.here_string()?,
                    _ => return Err("splatting is not supported".to_string()),
                }
                false
            }
            Some('(') => {
                self.paren_group()?;
                false
            }
            Some('{') => {
                self.block_group()?;
                false
            }
            Some('0'..='9') => {
                self.number();
                false
            }
            // `.5`: only plain digits, then a boundary.
            Some('.') if matches!(next, Some(d) if d.is_ascii_digit()) => {
                self.pos += 1;
                while matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    self.pos += 1;
                }
                let end = matches!(
                    self.peek(),
                    None | Some(' ' | '\t' | '\n' | '\r' | ';' | '|' | '&' | ')' | '}' | ']' | ',')
                );
                if !end {
                    return Err("unsupported number".to_string());
                }
                false
            }
            Some('!') => {
                self.pos += 1;
                return Ok(Operand::Prefix);
            }
            Some('-' | '+') => return self.prefix_sign(),
            _ => return Err(format!("expected an operand at {}", self.pos)),
        };
        self.postfix()?;
        Ok(Operand::Value(assignable))
    }

    /// Unary `-not`, `-bnot`, `-join`, `-split`, or a sign in front of a number, variable or group.
    fn prefix_sign(&mut self) -> Res<Operand> {
        if self.peek() == Some('-') {
            let word = self.dash_word();
            if !word.is_empty() {
                if UNARY_WORDS.contains(&word.as_str()) {
                    self.pos += 1 + word.len();
                    return Ok(Operand::Prefix);
                }
                return Err(format!("unknown prefix operator -{word}"));
            }
        }
        match self.peek_at(1) {
            Some(c) if c.is_ascii_digit() || c == '$' || c == '(' => {
                self.pos += 1;
                Ok(Operand::Prefix)
            }
            _ => Err("bad unary operator".to_string()),
        }
    }

    /// `$name`, `$env:name`, `$_`, `$$`, `$?`, `$^`; positioned at the `$`.
    pub(super) fn variable(&mut self) -> Res {
        self.pos += 1;
        if matches!(self.peek(), Some('$' | '?' | '^')) {
            self.pos += 1;
            return Ok(());
        }
        let start = self.pos;
        loop {
            match self.peek() {
                Some(c) if c.is_alphanumeric() || c == '_' => self.pos += 1,
                Some(':')
                    if self.pos > start
                        && matches!(self.peek_at(1), Some(d) if d.is_alphanumeric() || d == '_') =>
                {
                    self.pos += 1;
                }
                _ => break,
            }
        }
        if self.pos == start {
            return Err("empty variable name".to_string());
        }
        Ok(())
    }

    /// Integer, hex, real and suffixed numbers; `1..5` stops before the range operator.
    fn number(&mut self) {
        let word = |s: &mut Self| {
            while matches!(s.peek(), Some(c) if c.is_ascii_alphanumeric() || c == '_') {
                s.pos += 1;
            }
        };
        word(self);
        if self.peek() == Some('.') && matches!(self.peek_at(1), Some(d) if d.is_ascii_digit()) {
            self.pos += 1;
            word(self);
        }
    }

    /// `[type]`, with nested brackets for generics and arrays; positioned at the `[`.
    pub(super) fn type_literal(&mut self) -> Res {
        let start = self.pos;
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        // pwsh rejects `[,]`, `[a.]`, `[int,]`...: names and separators must alternate.
        if !word(self.peek_at(1)) {
            return Err(format!("bad type literal at {start}"));
        }
        let mut depth = 0usize;
        loop {
            match self.peek() {
                Some('[') => depth += 1,
                Some(']') => depth = depth.saturating_sub(1),
                Some(c) if c.is_alphanumeric() || matches!(c, '_' | ' ') => {}
                Some('.' | '+') if word(self.prev()) && word(self.peek_at(1)) => {}
                Some(',') => {
                    let before = self.text(start, self.pos);
                    let prev = before.chars().rev().find(|c| *c != ' ');
                    let mut i = 1;
                    while self.peek_at(i) == Some(' ') {
                        i += 1;
                    }
                    let next = self.peek_at(i);
                    if !(word(prev) || prev == Some(']')) || !(word(next) || next == Some('[')) {
                        return Err(format!("bad type literal at {start}"));
                    }
                }
                _ => return Err(format!("bad type literal at {start}")),
            }
            self.pos += 1;
            if depth == 0 {
                break;
            }
        }
        if self.pos - start <= 2 {
            return Err("empty type literal".to_string());
        }
        Ok(())
    }

    /// Parameter list after the `(`, up to and including the `)`: optional `[type]`s, a variable and
    /// an optional single-operand default per parameter.
    pub(super) fn param_list(&mut self) -> Res {
        let mut after_comma = false;
        loop {
            self.skip_nl()?;
            if self.peek() == Some(')') {
                if after_comma {
                    return Err("trailing comma in parameters".to_string());
                }
                self.pos += 1;
                return Ok(());
            }
            while self.peek() == Some('[') {
                self.type_literal()?;
                self.skip_nl()?;
            }
            if self.peek() != Some('$') || self.peek_at(1) == Some('{') {
                return Err("expected a parameter variable".to_string());
            }
            self.variable()?;
            self.skip_nl()?;
            if self.peek() == Some('=') {
                self.pos += 1;
                self.skip_nl()?;
                self.default_value()?;
                self.skip_nl()?;
            }
            match self.peek() {
                Some(',') => {
                    self.pos += 1;
                    after_comma = true;
                }
                Some(')') => after_comma = false,
                _ => return Err("bad parameter list".to_string()),
            }
        }
    }

    /// A parameter default: one operand (casts and unary operators included), no binary operators.
    fn default_value(&mut self) -> Res {
        loop {
            self.skip_inline()?;
            if let Operand::Value(_) = self.operand()? {
                return Ok(());
            }
        }
    }

    /// Member access, static access, method calls and indexing glued to an operand.
    fn postfix(&mut self) -> Res {
        loop {
            match (self.peek(), self.peek_at(1)) {
                (Some('.'), n) if n != Some('.') => {
                    self.pos += 1;
                    self.member()?;
                }
                (Some(':'), Some(':')) => {
                    self.pos += 2;
                    self.member()?;
                }
                (Some('['), _) => {
                    self.pos += 1;
                    self.nested(Self::index_body)?;
                }
                _ => return Ok(()),
            }
        }
    }

    fn member(&mut self) -> Res {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_alphanumeric() || c == '_') {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("expected a member name".to_string());
        }
        if self.peek() == Some('(') {
            self.pos += 1;
            self.nested(Self::call_body)?;
        }
        Ok(())
    }

    fn index_body(&mut self) -> Res {
        self.skip_nl()?;
        self.expression()?;
        self.skip_nl()?;
        self.expect(']')
    }

    /// Method-call arguments: expressions only, so a bareword argument is refused.
    fn call_body(&mut self) -> Res {
        self.skip_nl()?;
        if self.peek() == Some(')') {
            self.pos += 1;
            return Ok(());
        }
        self.expression()?;
        self.skip_nl()?;
        self.expect(')')
    }
}
