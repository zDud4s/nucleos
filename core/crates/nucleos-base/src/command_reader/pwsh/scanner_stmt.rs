//! Scanner statements: statement lists, chains, pipelines, simple commands, redirections and the
//! bracketed groups (`( )`, `$( )`, `@( )`, `{ }`, `@{ }`). Expressions live in `scanner_expr`.

use super::scanner::{Res, Scanner};
use super::scanner_lex::{is_dq, is_inline_ws, is_sq};

/// Words the scanner refuses as a command name: statement forms it does not model, or that only
/// parse as statements. (`else`, `elseif`, `catch`, `finally`, `until` are ordinary names alone.)
const KEYWORDS: &[&str] = &[
    "if",
    "foreach",
    "for",
    "while",
    "do",
    "switch",
    "try",
    "function",
    "filter",
    "param",
    "return",
    "throw",
    "exit",
    "break",
    "continue",
    "trap",
    "class",
    "enum",
    "workflow",
    "data",
    "using",
    "begin",
    "process",
    "end",
    "dynamicparam",
    "configuration",
    "parallel",
    "sequence",
    "inlinescript",
    "from",
    "define",
    "var",
];

/// Words read as statements by `keyword_statement`.
const HANDLED: &[&str] = &[
    "if",
    "foreach",
    "for",
    "while",
    "do",
    "switch",
    "try",
    "function",
    "param",
    "return",
    "throw",
    "exit",
    "break",
    "continue",
    "begin",
    "process",
    "end",
    "dynamicparam",
];

/// Statement keywords that may also stand on the right of an assignment.
const VALUE_OK: &[&str] = &["if", "foreach", "for", "while", "do", "switch", "try"];

/// Switch options that take no argument.
const SWITCH_FLAGS: &[&str] = &["regex", "wildcard", "exact", "casesensitive"];

/// What `keyword_statement` found.
pub(super) enum Stmt {
    /// Not a keyword statement: read it as a pipeline.
    NotKw,
    /// A complete block statement; no terminator is needed.
    Done,
    /// A flow statement (`return`...); the usual terminator check follows.
    Flow,
}

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | '{' | ';' | ')' | '}' | '|' | '&' | '#' | '[')
}

impl Scanner {
    pub(super) fn expect(&mut self, c: char) -> Res {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected {c:?} at {}", self.pos))
        }
    }

    /// Statements up to EOF or a closing `)` / `}` (left for the caller to consume).
    pub(super) fn statement_list(&mut self) -> Res {
        loop {
            self.skip_nl()?;
            match self.peek() {
                None | Some(')' | '}') => return Ok(()),
                Some(';') => {
                    self.pos += 1;
                    continue;
                }
                Some(_) => {}
            }
            match self.keyword_statement(false)? {
                Stmt::Done => continue,
                Stmt::Flow => {}
                Stmt::NotKw => self.chain()?,
            }
            self.skip_inline()?;
            match self.peek() {
                None | Some(';' | '\n' | '\r' | ')' | '}') => {}
                Some('&') if self.peek_at(1) != Some('&') => self.background()?,
                Some(c) => return Err(format!("unexpected {c:?} at {}", self.pos)),
            }
        }
    }

    /// A single `&` ends a statement (background job).
    fn background(&mut self) -> Res {
        self.pos += 1;
        match self.peek() {
            None | Some(';' | ')' | '}') => Ok(()),
            Some(c) if c.is_whitespace() => Ok(()),
            Some(_) => Err("`&` glued to a word".to_string()),
        }
    }

    /// `pipeline (&& | ||) pipeline ...`
    fn chain(&mut self) -> Res {
        self.pipeline_from(true)?;
        loop {
            self.skip_inline()?;
            let both = (self.peek(), self.peek_at(1));
            if matches!(both, (Some('&'), Some('&')) | (Some('|'), Some('|'))) {
                self.pos += 2;
                self.skip_nl()?;
                // pwsh: an assignment after `&&` / `||` is a parse error.
                self.pipeline_from(false)?;
            } else {
                return Ok(());
            }
        }
    }

    pub(super) fn pipeline(&mut self) -> Res {
        self.pipeline_from(true)
    }

    /// `assign_ok`: whether the first element may be an assignment.
    fn pipeline_from(&mut self, assign_ok: bool) -> Res {
        self.element(true, assign_ok)?;
        loop {
            self.skip_inline()?;
            if self.peek() == Some('|') && self.peek_at(1) != Some('|') {
                self.pos += 1;
                self.skip_nl()?;
                self.element(false, false)?;
            } else {
                return Ok(());
            }
        }
    }

    /// One pipeline element: a command or an expression, decided by its first character.
    fn element(&mut self, first: bool, assign_ok: bool) -> Res {
        self.skip_inline()?;
        let next = self.peek_at(1);
        match self.peek() {
            None => Err("expected a command".to_string()),
            Some('&' | '.') if matches!(next, Some(w) if is_inline_ws(w)) => self.call_operator(),
            Some('$' | '[' | '(' | '{' | '"' | '\'' | '!' | '-' | '+' | '0'..='9') => {
                self.expression_element(first, assign_ok)
            }
            // `.5` is a number; any other `.` word is a command name.
            Some('.') if matches!(next, Some(d) if d.is_ascii_digit()) => {
                self.expression_element(first, assign_ok)
            }
            Some(c) if is_sq(c) || is_dq(c) => self.expression_element(first, assign_ok),
            Some('@') if matches!(next, Some('(' | '{' | '\'' | '"')) => {
                self.expression_element(first, assign_ok)
            }
            Some('@') => Err("splatting is not supported".to_string()),
            Some(_) => self.command(),
        }
    }

    fn expression_element(&mut self, first: bool, assign_ok: bool) -> Res {
        // pwsh: "Expressions are only allowed as the first element of a pipeline".
        if !first {
            return Err("expression after `|`".to_string());
        }
        self.allow_assign = assign_ok;
        self.expression()?;
        let mut seen = 0u8;
        while self.at_redirect() {
            self.redirect(&mut seen)?;
            self.skip_inline()?;
        }
        Ok(())
    }

    fn at_redirect(&self) -> bool {
        match self.peek() {
            Some('>') => true,
            Some('*') => self.peek_at(1) == Some('>'),
            _ => false,
        }
    }

    /// `& target args` / `. target args`: the name is the target's source text.
    fn call_operator(&mut self) -> Res {
        self.pos += 1;
        self.skip_inline()?;
        let idx = self.names.len();
        let start = self.pos;
        self.call_target()?;
        let text = self.text(start, self.pos);
        self.names.insert(idx, text);
        self.args()
    }

    /// The target of `&` / `.`: exactly one string, group, variable or bareword, then a boundary.
    /// Text glued behind it makes pwsh split the word differently, so it is refused.
    fn call_target(&mut self) -> Res {
        match self.peek() {
            Some(c) if is_sq(c) => self.single_quoted()?,
            Some(c) if is_dq(c) => self.double_quoted()?,
            Some('(') => self.paren_group()?,
            Some('{') => self.block_group()?,
            Some('$') => match self.peek_at(1) {
                Some('(') => self.sub_expression()?,
                Some('{') => self.braced_variable()?,
                _ => self.variable()?,
            },
            _ => {
                let start = self.pos;
                while let Some(c) = self.peek() {
                    if c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')' | '}') {
                        break;
                    }
                    if is_sq(c)
                        || is_dq(c)
                        || matches!(c, '$' | '(' | '{' | '@' | ',' | '<' | '>' | '[' | '`')
                    {
                        return Err("unsupported call target".to_string());
                    }
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err("expected a call target".to_string());
                }
            }
        }
        match self.peek() {
            None => Ok(()),
            Some(c) if c.is_whitespace() || matches!(c, ';' | '|' | '&' | ')' | '}') => Ok(()),
            Some(_) => Err("text glued to a call target".to_string()),
        }
    }

    fn command(&mut self) -> Res {
        let start = self.pos;
        let idx = self.names.len();
        self.token(true)?;
        if self.names.len() != idx {
            return Err("sub-expression inside a command name".to_string());
        }
        let text = self.text(start, self.pos);
        if text.starts_with(':') {
            return Err("labels are not supported".to_string());
        }
        // pwsh: a lone `.` is a parse error; `.$x` / `."x"` / `.@` are dot-sourcing without a blank.
        let mut ch = text.chars();
        if ch.next() == Some('.')
            && matches!(ch.next(), None | Some('$' | '(' | '{' | '"' | '\'' | '@'))
        {
            return Err("dot-source without a blank".to_string());
        }
        if text.contains(['[', ']']) {
            return Err("bracket in a command name".to_string());
        }
        if KEYWORDS.contains(&text.to_lowercase().as_str()) {
            return Err(format!("statement keyword {text:?} not supported"));
        }
        self.names.push(text);
        self.args()
    }

    /// Arguments, parameters and redirections of a command, up to a statement/pipeline boundary.
    fn args(&mut self) -> Res {
        let mut seen = 0u8;
        loop {
            self.skip_inline()?;
            match self.peek() {
                None | Some('\n' | '\r' | ';' | '|' | '&' | ')' | '}') => return Ok(()),
                Some('>') => self.redirect(&mut seen)?,
                Some('*') if self.peek_at(1) == Some('>') => self.redirect(&mut seen)?,
                Some(d) if d.is_ascii_digit() && self.peek_at(1) == Some('>') => {
                    self.redirect(&mut seen)?
                }
                Some(_) => self.token(false)?,
            }
        }
    }

    /// `>`, `>>`, `2>`, `*>`, `2>&1`, each followed by a target word. `seen` holds the streams
    /// this command already redirects (bit 0 = `*`, bit n = stream n).
    fn redirect(&mut self, seen: &mut u8) -> Res {
        // pwsh: a redirection glued to the previous word is part of that word, not an operator.
        if matches!(self.prev(), Some(c) if !c.is_whitespace()) {
            return Err("redirection glued to a word".to_string());
        }
        let explicit = matches!(self.peek(), Some(c) if c == '*' || c.is_ascii_digit());
        let stream = match self.peek() {
            Some('*') => 0,
            // Streams 1-6 only: `0>`/`7>`.. are plain words to pwsh.
            Some(d) if explicit => match d.to_digit(10) {
                Some(n @ 1..=6) => n as u8,
                _ => return Err("not a stream number".to_string()),
            },
            _ => 1,
        };
        if explicit {
            self.pos += 1;
        }
        self.expect('>')?;
        let append = self.peek() == Some('>');
        if append {
            self.pos += 1;
        }
        // pwsh: "The output/error stream for this command is already redirected".
        let bit = 1u8 << stream;
        if *seen & bit != 0 || (*seen != 0 && (stream == 0 || *seen & 1 != 0)) {
            return Err("stream already redirected".to_string());
        }
        *seen |= bit;
        if self.peek() == Some('&') {
            // pwsh: only `*>&1` and `2>&1`..`6>&1` merge; `>&`, `>>&`, `1>&2`, `2>&2`, `2>&3`.. are
            // parse errors ("Missing file specification" / "reserved for future use").
            if !explicit || append || stream == 1 || self.peek_at(1) != Some('1') {
                return Err("bad stream merge".to_string());
            }
            self.pos += 2;
            return match self.peek() {
                None | Some(';' | '|' | '&' | ')' | '}') => Ok(()),
                Some(c) if c.is_whitespace() => Ok(()),
                Some(_) => Err("text glued to a stream merge".to_string()),
            };
        }
        self.skip_inline()?;
        // pwsh: a comma list as the target ("MissingArgument") is a parse error.
        let commas = self.commas;
        self.token(false)?;
        if self.commas != commas {
            return Err("comma in a redirection target".to_string());
        }
        Ok(())
    }

    /// `( pipeline )`: no `;` and no chains inside.
    pub(super) fn paren_group(&mut self) -> Res {
        self.pos += 1;
        self.nested(Self::paren_body)
    }

    fn paren_body(&mut self) -> Res {
        self.skip_nl()?;
        self.pipeline()?;
        self.skip_nl()?;
        self.expect(')')
    }

    /// `$( statements )`, positioned at the `$`.
    pub(super) fn sub_expression(&mut self) -> Res {
        self.pos += 2;
        self.nested(Self::list_paren_body)
    }

    /// `@( statements )`, positioned at the `@`.
    pub(super) fn array_group(&mut self) -> Res {
        self.sub_expression()
    }

    fn list_paren_body(&mut self) -> Res {
        self.statement_list()?;
        self.expect(')')
    }

    /// `{ statements }`
    pub(super) fn block_group(&mut self) -> Res {
        self.pos += 1;
        self.nested(Self::block_body)
    }

    fn block_body(&mut self) -> Res {
        self.statement_list()?;
        self.expect('}')
    }

    /// `@{ key = value; ... }`, positioned at the `@`.
    pub(super) fn hashtable(&mut self) -> Res {
        self.pos += 2;
        self.nested(Self::hash_body)
    }

    fn hash_body(&mut self) -> Res {
        loop {
            self.skip_nl()?;
            match self.peek() {
                Some('}') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(';') => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            self.hash_key()?;
            self.skip_inline()?;
            self.expect('=')?;
            self.skip_nl()?;
            self.pipeline()?;
            self.skip_inline()?;
            match self.peek() {
                Some(';' | '\n' | '\r' | '}') => {}
                _ => return Err("bad hashtable entry".to_string()),
            }
        }
    }

    fn hash_key(&mut self) -> Res {
        match self.peek() {
            Some(c) if is_sq(c) => self.single_quoted(),
            Some(c) if is_dq(c) => self.double_quoted(),
            Some(c) if c.is_alphanumeric() || c == '_' => {
                while matches!(self.peek(), Some(c) if c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
                {
                    self.pos += 1;
                }
                Ok(())
            }
            _ => Err("bad hashtable key".to_string()),
        }
    }

    /// ASCII letters at the current position (lowercased) and the char right after them.
    fn letters_ahead(&self) -> (String, Option<char>) {
        let mut w = String::new();
        let mut i = 0;
        while let Some(c) = self.peek_at(i) {
            if !c.is_ascii_alphabetic() {
                break;
            }
            w.push(c.to_ascii_lowercase());
            i += 1;
        }
        (w, self.peek_at(i))
    }

    /// A whole lowercase word (letters then a boundary) at the current position, if any.
    fn word_ahead(&self) -> Option<String> {
        let (w, follow) = self.letters_ahead();
        match follow {
            _ if w.is_empty() => None,
            None => Some(w),
            Some(c) if is_boundary(c) => Some(w),
            Some(_) => None,
        }
    }

    /// Reads a keyword statement at the current position, if there is one. `value_ctx` is the right
    /// side of an assignment, where only the block statements are allowed.
    pub(super) fn keyword_statement(&mut self, value_ctx: bool) -> Res<Stmt> {
        if self.peek() == Some('[') {
            return if value_ctx {
                Ok(Stmt::NotKw)
            } else {
                self.attributed_param()
            };
        }
        let (word, follow) = self.letters_ahead();
        if !HANDLED.contains(&word.as_str()) {
            return Ok(Stmt::NotKw);
        }
        match follow {
            None => {}
            Some(c) if is_boundary(c) => {}
            Some(c) if matches!(c, '$' | '@') || is_sq(c) || is_dq(c) => {
                return Err(format!("text glued to keyword {word:?}"));
            }
            Some(_) => return Ok(Stmt::NotKw),
        }
        if value_ctx && !VALUE_OK.contains(&word.as_str()) {
            return Err(format!("keyword {word:?} as a value"));
        }
        self.pos += word.len();
        match word.as_str() {
            "if" => self.if_stmt()?,
            "while" => {
                self.header()?;
                self.body()?;
            }
            "for" => {
                self.skip_inline()?;
                self.expect('(')?;
                self.nested(Self::for_header)?;
                self.body()?;
            }
            "foreach" => {
                self.skip_inline()?;
                self.expect('(')?;
                self.nested(Self::foreach_header)?;
                self.body()?;
            }
            "do" => self.do_stmt()?,
            "switch" => self.switch_stmt()?,
            "try" => self.try_stmt()?,
            "function" => self.function_stmt()?,
            "param" => {
                self.skip_inline()?;
                self.expect('(')?;
                self.nested(Self::param_list)?;
            }
            "begin" | "process" | "end" | "dynamicparam" => {
                self.skip_inline()?;
                if self.peek() != Some('{') {
                    return Err(format!("{word:?} needs a block"));
                }
                self.block_group()?;
            }
            "return" | "throw" | "exit" => {
                self.skip_inline()?;
                if !matches!(self.peek(), None | Some('\n' | '\r' | ';' | ')' | '}')) {
                    self.pipeline()?;
                }
                return Ok(Stmt::Flow);
            }
            _ => {
                // break, continue: a label operand is not supported.
                self.skip_inline()?;
                if !matches!(self.peek(), None | Some('\n' | '\r' | ';' | ')' | '}')) {
                    return Err(format!("operand after {word:?}"));
                }
                return Ok(Stmt::Flow);
            }
        }
        Ok(Stmt::Done)
    }

    /// Right-hand side of an assignment: a block statement or a pipeline.
    pub(super) fn assigned_value(&mut self) -> Res {
        match self.keyword_statement(true)? {
            Stmt::NotKw => self.pipeline(),
            _ => Ok(()),
        }
    }

    /// `( pipeline )` header of if/elseif/while/until.
    fn header(&mut self) -> Res {
        self.skip_inline()?;
        if self.peek() != Some('(') {
            return Err("expected '('".to_string());
        }
        self.paren_group()
    }

    /// A `{ }` statement block, newlines allowed before it.
    fn body(&mut self) -> Res {
        self.skip_nl()?;
        if self.peek() != Some('{') {
            return Err("expected '{'".to_string());
        }
        self.block_group()
    }

    fn if_stmt(&mut self) -> Res {
        self.header()?;
        self.body()?;
        loop {
            let save = self.pos;
            self.skip_nl()?;
            match self.word_ahead().as_deref() {
                Some("elseif") => {
                    self.pos += 6;
                    self.header()?;
                    self.body()?;
                }
                Some("else") => {
                    self.pos += 4;
                    return self.body();
                }
                _ => {
                    self.pos = save;
                    return Ok(());
                }
            }
        }
    }

    /// `init; condition; step )`, after the `(`.
    fn for_header(&mut self) -> Res {
        self.for_part()?;
        self.expect(';')?;
        self.for_part()?;
        self.expect(';')?;
        self.for_part()?;
        self.expect(')')
    }

    fn for_part(&mut self) -> Res {
        self.skip_nl()?;
        if !matches!(self.peek(), Some(';' | ')')) {
            self.pipeline()?;
            self.skip_nl()?;
        }
        Ok(())
    }

    /// `$var in pipeline )`, after the `(`.
    fn foreach_header(&mut self) -> Res {
        self.skip_nl()?;
        if self.peek() != Some('$') || self.peek_at(1) == Some('{') {
            return Err("expected a loop variable".to_string());
        }
        self.variable()?;
        self.skip_nl()?;
        if self.word_ahead().as_deref() != Some("in") {
            return Err("expected 'in'".to_string());
        }
        self.pos += 2;
        self.skip_nl()?;
        self.pipeline()?;
        self.skip_nl()?;
        self.expect(')')
    }

    /// `do { } while|until ( )`: the oracle lists the condition's commands before the body's.
    fn do_stmt(&mut self) -> Res {
        let idx = self.names.len();
        self.body()?;
        let mid = self.names.len();
        self.skip_nl()?;
        match self.word_ahead().as_deref() {
            Some("while" | "until") => self.pos += 5,
            _ => return Err("expected 'while' or 'until'".to_string()),
        }
        self.header()?;
        self.names[idx..].rotate_left(mid - idx);
        Ok(())
    }

    fn switch_stmt(&mut self) -> Res {
        loop {
            self.skip_inline()?;
            if self.peek() != Some('-') {
                break;
            }
            self.pos += 1;
            let start = self.pos;
            while matches!(self.peek(), Some(c) if c.is_ascii_alphabetic()) {
                self.pos += 1;
            }
            let flag = self.text(start, self.pos).to_lowercase();
            if !SWITCH_FLAGS.contains(&flag.as_str()) {
                return Err(format!("unsupported switch option -{flag}"));
            }
        }
        self.header()?;
        self.skip_nl()?;
        self.expect('{')?;
        self.nested(Self::switch_body)
    }

    /// Clauses `condition { body }` up to the closing `}`; at least one is required.
    fn switch_body(&mut self) -> Res {
        let mut clauses = 0;
        loop {
            self.skip_nl()?;
            if self.peek() == Some('}') {
                if clauses == 0 {
                    return Err("empty switch".to_string());
                }
                self.pos += 1;
                return Ok(());
            }
            if self.peek() == Some('{') {
                self.block_group()?;
            } else if self.word_ahead().as_deref() == Some("default") {
                self.pos += 7;
            } else {
                self.token(true)?;
            }
            self.body()?;
            clauses += 1;
        }
    }

    fn try_stmt(&mut self) -> Res {
        self.body()?;
        let mut handlers = 0;
        loop {
            let save = self.pos;
            self.skip_nl()?;
            match self.word_ahead().as_deref() {
                Some("catch") => {
                    self.pos += 5;
                    self.catch_types()?;
                    self.body()?;
                    handlers += 1;
                }
                Some("finally") => {
                    self.pos += 7;
                    return self.body();
                }
                _ => {
                    self.pos = save;
                    break;
                }
            }
        }
        if handlers == 0 {
            return Err("try needs catch or finally".to_string());
        }
        Ok(())
    }

    /// Optional `[Type], [Type]` list after `catch`.
    fn catch_types(&mut self) -> Res {
        self.skip_inline()?;
        if self.peek() != Some('[') {
            return Ok(());
        }
        loop {
            self.type_literal()?;
            self.skip_inline()?;
            if self.peek() != Some(',') {
                return Ok(());
            }
            self.pos += 1;
            self.skip_inline()?;
        }
    }

    /// `function name [( params )] { body }`, after the keyword.
    fn function_stmt(&mut self) -> Res {
        self.skip_inline()?;
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err("function needs a name".to_string());
        }
        self.skip_inline()?;
        if self.peek() == Some('(') {
            self.pos += 1;
            self.nested(Self::param_list)?;
        }
        self.body()
    }

    /// `[Attr] [Attr()] param(...)`; anything else starting with `[` is left to the expression reader.
    fn attributed_param(&mut self) -> Res<Stmt> {
        let save = self.pos;
        if self.attributes() && self.word_ahead().as_deref() == Some("param") {
            self.pos += 5;
            self.skip_inline()?;
            self.expect('(')?;
            self.nested(Self::param_list)?;
            return Ok(Stmt::Done);
        }
        self.pos = save;
        Ok(Stmt::NotKw)
    }

    /// One or more `[Name]` / `[Name()]` attributes; false (position unspecified) if the text is not.
    fn attributes(&mut self) -> bool {
        let mut seen = false;
        loop {
            if self.skip_nl().is_err() || self.peek() != Some('[') {
                return seen;
            }
            self.pos += 1;
            let start = self.pos;
            while matches!(self.peek(), Some(c) if c.is_alphanumeric() || matches!(c, '_' | '.')) {
                self.pos += 1;
            }
            if self.pos == start {
                return false;
            }
            if self.peek() == Some('(') && self.peek_at(1) == Some(')') {
                self.pos += 2;
            }
            if self.peek() != Some(']') {
                return false;
            }
            self.pos += 1;
            seen = true;
        }
    }
}
