//! Reading ONE word for `command_reader::parse`: [`analyse`] walks the pieces brush-parser cut a
//! word into and returns its unquoted text, how it was quoted, whether it expands, whether it can
//! assign, and the command substitutions inside it. A piece the walk cannot place (a nested
//! command in a `${x:-…}`, arithmetic that can assign) is refused with its reason.

use brush_parser::ParserOptions;
use brush_parser::word::{self, WordPiece, WordPieceWithSource};

use super::hazards::{evaluates_a_variable, holds_a_reference, transforms};
use super::{MISALIGNED, Quoting, Site, Step, UNPARSEABLE_WORD, Word};

/// A word, analysed.
pub(super) struct Analysis {
    pub(super) word: Word,
    /// Substitutions in the word's own bytes.
    pub(super) sites: Vec<Site>,
    pub(super) assigns: bool,
}

pub(super) fn analyse(raw: &str, options: &ParserOptions) -> Result<Analysis, &'static str> {
    let pieces = word::parse(raw, options).map_err(|_| UNPARSEABLE_WORD)?;
    let mut collector = Collector::new(raw);
    for (index, piece) in pieces.iter().enumerate() {
        collector.piece(piece, false, index == 0)?;
    }
    Ok(collector.finish())
}

/// Collects the unquoted text of a word, how it was quoted and whether it expands.
pub(super) struct Collector<'a> {
    raw: &'a str,
    text: String,
    bare: bool,
    single: bool,
    double: bool,
    expands: bool,
    assigns: bool,
    pub(super) sites: Vec<Site>,
}

impl<'a> Collector<'a> {
    pub(super) fn new(raw: &'a str) -> Self {
        Collector {
            raw,
            text: String::new(),
            bare: false,
            single: false,
            double: false,
            expands: false,
            assigns: false,
            sites: Vec::new(),
        }
    }

    /// One piece of the word. `in_double` is set inside double quotes, where quoting is already
    /// accounted for and a glob character is only a character. Offsets are the word's own: the
    /// pieces inside a double-quoted sequence are positioned in the same string as the sequence.
    pub(super) fn piece(
        &mut self,
        piece: &WordPieceWithSource,
        in_double: bool,
        first: bool,
    ) -> Step {
        let raw = self.raw;
        let source = raw
            .get(piece.start_index..piece.end_index)
            .ok_or(MISALIGNED)?;
        match &piece.piece {
            WordPiece::Text(text) => {
                self.text.push_str(text);
                if !in_double {
                    self.bare = true;
                    if glob_like(text) || (first && text.starts_with('~')) {
                        self.expands = true;
                    }
                }
            }
            WordPiece::SingleQuotedText(text) | WordPiece::AnsiCQuotedText(text) => {
                self.single = true;
                self.text.push_str(text);
            }
            WordPiece::DoubleQuotedSequence(inner)
            | WordPiece::GettextDoubleQuotedSequence(inner) => {
                self.double = true;
                for part in inner {
                    self.piece(part, true, false)?;
                }
            }
            WordPiece::TildeExpansion(_) => {
                self.bare |= !in_double;
                self.expands = true;
                self.text.push_str(source);
            }
            WordPiece::ParameterExpansion(_) => {
                // brush keeps the default, alternate and pattern words of `${x:-…}` as raw strings,
                // so a substitution inside one is invisible to the AST: no site is recorded and no
                // placeholder appears. Refuse rather than recurse into text this walk cannot place.
                if ["$(", "`", "<(", ">("]
                    .iter()
                    .any(|opener| source.contains(opener))
                {
                    return Err("a parameter expansion runs a nested command");
                }
                // `${x@P}` expands prompt escapes (which run `$(…)` held in the value), `@E`
                // expands escapes and `@A` builds an assignment: each can run what a variable holds.
                // brush keeps nested words raw, so the operator is searched anywhere in the source,
                // not only at the end; over-refusing `${x:-@P}` is accepted.
                if transforms(source) {
                    return Err("a prompt or escape transformation can run what a variable holds");
                }
                if evaluates_a_variable(source) {
                    return Err("a parameter expansion evaluates a variable's value as code");
                }
                self.bare |= !in_double;
                self.expands = true;
                // `${S:=x}` and its relatives assign; anything with an `=` in it might.
                self.assigns |= source.contains('=');
                self.text.push_str(source);
            }
            WordPiece::CommandSubstitution(inner) => {
                self.substitution(piece, source, inner, false, in_double);
            }
            WordPiece::BackquotedCommandSubstitution(inner) => {
                self.substitution(piece, source, inner, true, in_double);
            }
            WordPiece::EscapeSequence(text) => {
                self.single |= !in_double;
                let unescaped = text.strip_prefix('\\').unwrap_or(text.as_str());
                self.text.push_str(unescaped);
            }
            WordPiece::ArithmeticExpression(expression) => {
                let value = &expression.value;
                if value.contains(['=', '`'])
                    || value.contains("++")
                    || value.contains("--")
                    || value.contains("$(")
                {
                    return Err("an arithmetic expansion can assign or run a command");
                }
                if holds_a_reference(value) {
                    return Err("arithmetic over a variable evaluates its value as code");
                }
                if transforms(value) {
                    return Err("a prompt or escape transformation can run what a variable holds");
                }
                self.bare |= !in_double;
                self.expands = true;
                self.text.push_str(source);
            }
        }
        Ok(())
    }

    fn substitution(
        &mut self,
        piece: &WordPieceWithSource,
        source: &str,
        inner: &str,
        backquoted: bool,
        in_double: bool,
    ) {
        self.bare |= !in_double;
        self.expands = true;
        self.text.push_str(source);
        self.sites.push(Site {
            range: piece.start_index..piece.end_index,
            inner: inner.to_owned(),
            backquoted,
        });
    }

    pub(super) fn finish(self) -> Analysis {
        let quoting = match (self.bare, self.single, self.double) {
            (_, false, false) => Quoting::Bare,
            (false, true, false) => Quoting::Single,
            (false, false, true) => Quoting::Double,
            _ => Quoting::Mixed,
        };
        Analysis {
            word: Word {
                text: self.text,
                quoting,
                expands: self.expands,
            },
            sites: self.sites,
            assigns: self.assigns,
        }
    }
}

/// Whether unquoted text holds something the shell expands by pattern: `*`, `?`, a bracket class,
/// a brace list or range, or the `(` of an extended glob. A lone `[` (the `test` program) is not.
fn glob_like(text: &str) -> bool {
    text.contains(['*', '?', '('])
        || text
            .find('[')
            .is_some_and(|open| text[open..].contains(']'))
        || text.match_indices('{').any(|(open, _)| {
            let rest = &text[open + 1..];
            rest.find('}')
                .is_some_and(|close| rest[..close].contains(',') || rest[..close].contains(".."))
        })
}
