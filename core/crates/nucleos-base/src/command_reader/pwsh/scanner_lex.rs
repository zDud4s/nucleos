//! Scanner lexing: words, strings, here-strings, comments, backticks.

use super::scanner::{Res, Scanner};

/// Whitespace that does not end a line.
pub(super) fn is_inline_ws(c: char) -> bool {
    c != '\n' && c != '\r' && c.is_whitespace()
}

/// Single-quote class: PowerShell reads the curly single quotes as real quotes.
pub(super) fn is_sq(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

/// Double-quote class: PowerShell reads the curly double quotes as real quotes.
pub(super) fn is_dq(c: char) -> bool {
    matches!(c, '"' | '\u{201C}' | '\u{201D}' | '\u{201E}')
}

/// Whether the chars after a `#` spell `requires` (such a comment is refused: it changes how the file runs).
fn is_requires(rest: &[Option<char>]) -> bool {
    "requires"
        .chars()
        .zip(rest)
        .all(|(w, c)| c.is_some_and(|c| c.to_ascii_lowercase() == w))
}

impl Scanner {
    /// Skips blanks, backtick line continuations and comments; stops at a newline.
    pub(super) fn skip_inline(&mut self) -> Res {
        loop {
            match self.peek() {
                Some(c) if is_inline_ws(c) => self.pos += 1,
                Some('`') => match self.peek_at(1) {
                    Some('\n') => self.pos += 2,
                    Some('\r') => {
                        self.pos += 2;
                        if self.peek() == Some('\n') {
                            self.pos += 1;
                        }
                    }
                    None => return Err("trailing backtick".to_string()),
                    // pwsh skips a backtick followed by a space/tab like plain blanks at a token start.
                    // Glued to a word or operand it is an escape the scanner does not model.
                    Some(' ' | '\t') => {
                        if !matches!(self.prev(), None | Some(';' | '|' | '&' | '(' | '{'))
                            && !matches!(self.prev(), Some(p) if p.is_whitespace())
                        {
                            return Err("backtick blank glued to a word".to_string());
                        }
                        self.pos += 2;
                    }
                    Some(w) if is_inline_ws(w) => return Err("backtick before blank".to_string()),
                    Some(_) => return Ok(()),
                },
                Some('#') => {
                    let rest: Vec<Option<char>> = (1..=8).map(|i| self.peek_at(i)).collect();
                    if is_requires(&rest) {
                        return Err("#requires comment".to_string());
                    }
                    while !matches!(self.peek(), None | Some('\n' | '\r')) {
                        self.pos += 1;
                    }
                }
                Some('<') if self.peek_at(1) == Some('#') => {
                    self.pos += 2;
                    loop {
                        match self.peek() {
                            None => return Err("unterminated block comment".to_string()),
                            Some('#') if self.peek_at(1) == Some('>') => {
                                self.pos += 2;
                                break;
                            }
                            Some(_) => self.pos += 1,
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Like `skip_inline`, but newlines are skipped too.
    pub(super) fn skip_nl(&mut self) -> Res {
        loop {
            self.skip_inline()?;
            if matches!(self.peek(), Some('\n' | '\r')) {
                self.pos += 1;
            } else {
                return Ok(());
            }
        }
    }

    pub(super) fn single_quoted(&mut self) -> Res {
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_string()),
                Some(c) if is_sq(c) => {
                    self.pos += 1;
                    if matches!(self.peek(), Some(d) if is_sq(d)) {
                        self.pos += 1;
                    } else {
                        return Ok(());
                    }
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Double-quoted string; `$( )` inside is scanned for commands.
    pub(super) fn double_quoted(&mut self) -> Res {
        let open = self.pos;
        self.pos += 1;
        loop {
            match self.peek() {
                None => return Err("unterminated string".to_string()),
                Some('`') => self.backtick_escape()?,
                Some(c) if is_dq(c) => {
                    // pwsh rejects (or reads differently) an empty or doubled-quote string inside the
                    // `$( )` of another double-quoted string: refused, never modelled.
                    if self.dq_sub > 0
                        && (self.pos == open + 1 || matches!(self.peek_at(1), Some(d) if is_dq(d)))
                    {
                        return Err("empty or doubled quote in a string sub-expression".to_string());
                    }
                    self.pos += 1;
                    if matches!(self.peek(), Some(d) if is_dq(d)) {
                        self.pos += 1;
                    } else {
                        return Ok(());
                    }
                }
                Some('$') => self.dollar_in_string()?,
                Some(_) => self.pos += 1,
            }
        }
    }

    /// `@'...'@` or `@"..."@`, positioned at the `@`. The terminator counts only at line start.
    pub(super) fn here_string(&mut self) -> Res {
        let quote = self.peek_at(1).unwrap_or('\'');
        self.pos += 2;
        while matches!(self.peek(), Some(c) if is_inline_ws(c)) {
            self.pos += 1;
        }
        match self.peek() {
            Some('\n') => self.pos += 1,
            Some('\r') => {
                self.pos += 1;
                if self.peek() == Some('\n') {
                    self.pos += 1;
                }
            }
            _ => return Err("here-string header needs a newline".to_string()),
        }
        let expandable = quote == '"';
        let mut line_start = true;
        loop {
            match self.peek() {
                None => return Err("unterminated here-string".to_string()),
                Some(c) if line_start && c == quote && self.peek_at(1) == Some('@') => {
                    self.pos += 2;
                    return Ok(());
                }
                Some('`') if expandable => {
                    self.backtick_escape()?;
                    line_start = false;
                }
                Some('$') if expandable => {
                    self.dollar_in_string()?;
                    line_start = false;
                }
                Some(c) => {
                    line_start = c == '\n' || c == '\r';
                    self.pos += 1;
                }
            }
        }
    }

    /// `${...}` variable, positioned at the `$`.
    pub(super) fn braced_variable(&mut self) -> Res {
        self.pos += 2;
        // pwsh: "An empty ${} variable reference was found".
        if self.peek() == Some('}') {
            return Err("empty ${}".to_string());
        }
        loop {
            match self.peek() {
                None => return Err("unterminated ${".to_string()),
                Some('}') => {
                    // pwsh: a name ending in `:` is "Variable reference is not valid".
                    if self.prev() == Some(':') {
                        return Err("`${name:}`".to_string());
                    }
                    self.pos += 1;
                    return Ok(());
                }
                // pwsh: an unescaped `{` inside `${ }` is a parse error.
                Some('{') => return Err("`{` inside `${}`".to_string()),
                Some('`') => self.pos += 2,
                Some(_) => self.pos += 1,
            }
        }
    }

    /// A backtick at the current position and the char it escapes. pwsh: `` `u `` must open a valid
    /// `` `u{hex} `` (1-6 hex digits, at most 0x10FFFF), otherwise "The Unicode escape sequence is not valid".
    pub(super) fn backtick_escape(&mut self) -> Res {
        if self.peek_at(1) != Some('u') {
            self.pos += 2;
            return Ok(());
        }
        if self.peek_at(2) != Some('{') {
            return Err("bad unicode escape".to_string());
        }
        let mut i = 3;
        let mut value: u32 = 0;
        while let Some(d) = self.peek_at(i).and_then(|c| c.to_digit(16)) {
            value = value * 16 + d;
            i += 1;
            if i > 9 {
                return Err("unicode escape too long".to_string());
            }
        }
        if !(4..=9).contains(&i) || self.peek_at(i) != Some('}') || value > 0x10FFFF {
            return Err("bad unicode escape".to_string());
        }
        self.pos += i + 1;
        Ok(())
    }

    /// A `$` inside an expandable string, positioned at the `$`. pwsh: `${` needs a closing brace;
    /// `$name:` and `$:` must be followed by a name char ("Variable reference is not valid").
    fn dollar_in_string(&mut self) -> Res {
        let is_name =
            |c: char| c.is_ascii_alphanumeric() || c == '_' || (!c.is_ascii() && c.is_alphabetic());
        match self.peek_at(1) {
            Some('(') => {
                // pwsh finds the end of a `$( )` inside a string by counting raw parens, quotes and
                // comments ignored, then parses what is between: if that end is not where the
                // scanner closed the sub-expression the two readings differ, so it is refused.
                let start = self.pos;
                self.dq_sub += 1;
                let r = self.sub_expression();
                self.dq_sub -= 1;
                r?;
                if !self.raw_parens_close_at_end(start + 1, self.pos) {
                    return Err("unbalanced raw parens in a string sub-expression".to_string());
                }
                return Ok(());
            }
            Some('{') => return self.braced_variable(),
            Some('$' | '?' | '^') => {
                self.pos += 2;
                return Ok(());
            }
            Some(':') => {
                if !matches!(self.peek_at(2), Some(c) if is_name(c) || c == '?') {
                    return Err("`$:` not followed by a name".to_string());
                }
                self.pos += 2;
                while matches!(self.peek(), Some(c) if is_name(c) || c == '?') {
                    self.pos += 1;
                }
                return Ok(());
            }
            Some(c) if is_name(c) => self.pos += 1,
            _ => {
                self.pos += 1;
                return Ok(());
            }
        }
        while matches!(self.peek(), Some(c) if is_name(c)) {
            self.pos += 1;
        }
        if self.peek() == Some(':') {
            match self.peek_at(1) {
                Some(c) if is_name(c) || c == '?' => {
                    self.pos += 1;
                    while matches!(self.peek(), Some(c) if is_name(c) || c == '?') {
                        self.pos += 1;
                    }
                }
                _ => return Err("`$name:` not followed by a name".to_string()),
            }
        }
        Ok(())
    }

    /// One command-line word: bare text, strings, variables and nested groups glued together.
    /// A name (`is_name`) ends at `(`/`{` and may not contain a comma.
    pub(super) fn token(&mut self, is_name: bool) -> Res {
        let start = self.pos;
        // pwsh: a comma inside a `-parameter` word (unless it is a negative number) is a parse error.
        let dash_param =
            self.peek() == Some('-') && !matches!(self.peek_at(1), Some(d) if d.is_ascii_digit());
        let mut after_group = false;
        while let Some(c) = self.peek() {
            if after_group
                && !(c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')' | '}' | ',' | '>'))
            {
                return Err("text glued to a group".to_string());
            }
            after_group = false;
            match c {
                c if c.is_whitespace() => break,
                ';' | '|' | '&' | ')' | '}' => break,
                // pwsh reads `r>e` as one bareword (`a>&1` even splits oddly): a redirection glued
                // to a word is refused, never modelled.
                '>' => return Err("redirection glued to a word".to_string()),
                '<' => return Err("input redirection".to_string()),
                '(' | '{' if is_name && self.pos > start => break,
                '(' => {
                    self.paren_group()?;
                    after_group = true;
                }
                '{' => {
                    self.block_group()?;
                    after_group = true;
                }
                c if is_sq(c) => self.single_quoted()?,
                c if is_dq(c) => self.double_quoted()?,
                '`' => match self.peek_at(1) {
                    None => return Err("trailing backtick".to_string()),
                    Some('\r') => {
                        self.pos += 2;
                        if self.peek() == Some('\n') {
                            self.pos += 1;
                        }
                    }
                    Some(_) => self.backtick_escape()?,
                },
                '$' => match self.peek_at(1) {
                    Some('(') => {
                        self.sub_expression()?;
                        after_group = true;
                    }
                    Some('{') => self.braced_variable()?,
                    Some('$' | '?' | '^' | ':') => self.dollar_in_string()?,
                    Some(n)
                        if n.is_ascii_alphanumeric()
                            || n == '_'
                            || (!n.is_ascii() && n.is_alphabetic()) =>
                    {
                        self.dollar_in_string()?
                    }
                    // pwsh reads a lone `$` in odd places differently (`.$`): refused.
                    _ => return Err("bare `$`".to_string()),
                },
                // A quote, group or `]` right before an `@` changes how pwsh splits the word.
                '@' if self.pos > start
                    && matches!(self.prev(), Some(p) if is_sq(p) || is_dq(p) || matches!(p, ')' | '}' | ']')) =>
                {
                    return Err("`@` glued to a quote or group".to_string());
                }
                '@' if self.pos == start
                    && !matches!(self.peek_at(1), Some('(' | '{' | '\'' | '"' | '_'))
                    && !matches!(self.peek_at(1), Some(n) if n.is_alphabetic()) =>
                {
                    return Err("bare `@`".to_string());
                }
                '@' => match self.peek_at(1) {
                    Some('(') => {
                        self.array_group()?;
                        after_group = true;
                    }
                    Some('{') => {
                        self.hashtable()?;
                        after_group = true;
                    }
                    Some('\'' | '"') => {
                        self.here_string()?;
                        after_group = true;
                    }
                    _ => self.pos += 1,
                },
                ',' if is_name => return Err("comma in a command name".to_string()),
                ',' => {
                    self.commas += 1;
                    if self.pos == start || dash_param {
                        return Err("misplaced comma".to_string());
                    }
                    self.pos += 1;
                    self.skip_nl()?;
                    // pwsh: a trailing comma, or two in a row, is a missing array element.
                    if matches!(
                        self.peek(),
                        None | Some(';' | '|' | '&' | ')' | '}' | '>' | ',')
                    ) {
                        return Err("trailing comma".to_string());
                    }
                }
                _ => self.pos += 1,
            }
        }
        if self.pos == start {
            return Err("expected a word".to_string());
        }
        if self.text(start, self.pos) == "--%" {
            return Err("stop-parsing token".to_string());
        }
        Ok(())
    }
}
