//! Decision conditions (spec motor-de-workflows 4.3 / 4.5). Pure: no IO.
//!
//! Grammar:
//! ```text
//! condition  := comparison ( JOINER comparison )*      JOINER is all "and" or all "or"
//! comparison := key op value
//! op         := "==" | "!=" | "in"
//! value      := word | "quoted" | 'quoted'              for == and !=
//!             | "[" value ("," value)* "]"             for in only
//! key        := [A-Za-z_][A-Za-z0-9_.-]*               at least one '.' (namespace.key)
//! word       := [A-Za-z0-9_.+/-]+                      not one of and/or/in
//! ```
//! Evaluation is strict: a comparison on a key nobody published is `Unpublished`
//! (also under `!=`), and any unpublished comparison makes the whole condition
//! `Unpublished`, whatever the other comparisons say.

use super::facts::Facts;

/// The phrase of the refusal for `and` mixed with `or` in one condition.
const MIX_PHRASE: &str = "cannot mix `and` with `or` (split it into two decision nodes)";

/// A parsed condition. Opaque outside this module except through `keys()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    joiner: Joiner,
    comparisons: Vec<Comparison>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Joiner {
    Single,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Comparison {
    key: String,
    op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Eq(String),
    Ne(String),
    In(Vec<String>),
}

/// The outcome of evaluating a condition against the published facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eval {
    True,
    False,
    /// The first key (source order) read by the condition that no one published.
    Unpublished {
        key: String,
    },
}

impl Condition {
    /// Every key the condition reads, in source order, duplicates kept.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.comparisons.iter().map(|c| c.key.as_str())
    }

    /// Every comparison as its key and the literal value(s) it compares against, in
    /// source order: one value for `==`/`!=`, the whole list for `in`. Literals are
    /// the strings the evaluator compares (a quoted value yields its unquoted text;
    /// bare numbers and booleans are their source text).
    pub fn comparisons(&self) -> Vec<(&str, Vec<String>)> {
        self.comparisons
            .iter()
            .map(|c| {
                let values = match &c.op {
                    Op::Eq(v) | Op::Ne(v) => vec![v.clone()],
                    Op::In(vs) => vs.clone(),
                };
                (c.key.as_str(), values)
            })
            .collect()
    }
}

// ---------------------------------------------------------------- tokenizer

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Word,
    Quoted,
    EqEq,
    Ne,
    LBracket,
    RBracket,
    Comma,
}

#[derive(Debug, Clone)]
struct Tok {
    kind: Kind,
    /// Source text of the token (for quoted values: the unquoted content).
    text: String,
    /// What to show in an error.
    shown: String,
    /// 1-based character column.
    col: usize,
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '/' | '-')
}

fn fail(src: &str, msg: &str, shown: &str, col: usize) -> String {
    format!("condition `{src}`: {msg} (token `{shown}` at column {col})")
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let col = i + 1;
        let simple = |kind: Kind, s: &str| Tok {
            kind,
            text: s.to_string(),
            shown: s.to_string(),
            col,
        };
        if c.is_whitespace() {
            i += 1;
        } else if c == '[' {
            toks.push(simple(Kind::LBracket, "["));
            i += 1;
        } else if c == ']' {
            toks.push(simple(Kind::RBracket, "]"));
            i += 1;
        } else if c == ',' {
            toks.push(simple(Kind::Comma, ","));
            i += 1;
        } else if c == '=' {
            if chars.get(i + 1) == Some(&'=') {
                toks.push(simple(Kind::EqEq, "=="));
                i += 2;
            } else {
                return Err(fail(
                    src,
                    "unknown operator, use `==`, `!=` or `in`",
                    "=",
                    col,
                ));
            }
        } else if c == '!' {
            if chars.get(i + 1) == Some(&'=') {
                toks.push(simple(Kind::Ne, "!="));
                i += 2;
            } else {
                return Err(fail(
                    src,
                    "unknown operator, use `==`, `!=` or `in`",
                    "!",
                    col,
                ));
            }
        } else if c == '"' || c == '\'' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                j += 1;
            }
            if j >= chars.len() {
                let rest: String = chars[i..].iter().collect();
                return Err(fail(src, "unterminated quoted value", &rest, col));
            }
            let inner: String = chars[i + 1..j].iter().collect();
            let shown: String = chars[i..=j].iter().collect();
            toks.push(Tok {
                kind: Kind::Quoted,
                text: inner,
                shown,
                col,
            });
            i = j + 1;
        } else if is_word_char(c) {
            let mut j = i;
            while j < chars.len() && is_word_char(chars[j]) {
                j += 1;
            }
            let w: String = chars[i..j].iter().collect();
            toks.push(Tok {
                kind: Kind::Word,
                text: w.clone(),
                shown: w,
                col,
            });
            i = j;
        } else {
            let s = c.to_string();
            let msg = if matches!(c, '(' | ')') {
                "parentheses are not supported"
            } else {
                "unexpected character or unknown operator"
            };
            return Err(fail(src, msg, &s, col));
        }
    }
    Ok(toks)
}

// ------------------------------------------------------------------- parser

fn is_reserved(w: &str) -> bool {
    matches!(w, "and" | "or" | "in")
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn err_at(&self, msg: &str) -> String {
        match self.peek() {
            Some(t) => fail(self.src, msg, &t.shown, t.col),
            None => {
                let col = self.src.chars().count() + 1;
                fail(self.src, msg, "<end>", col)
            }
        }
    }

    fn key(&mut self) -> Result<String, String> {
        let Some(t) = self.peek() else {
            return Err(self.err_at("expected a key like `namespace.key`"));
        };
        if t.kind != Kind::Word || is_reserved(&t.text) {
            return Err(self.err_at("expected a key like `namespace.key`"));
        }
        let w = t.text.clone();
        let mut cs = w.chars();
        let first_ok = cs
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        let rest_ok = cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if !first_ok || !rest_ok || !w.contains('.') {
            return Err(self.err_at("a key must look like `namespace.key`"));
        }
        self.pos += 1;
        Ok(w)
    }

    /// One scalar value: a bare word (not reserved) or a quoted string.
    fn scalar(&mut self) -> Result<String, String> {
        let Some(t) = self.peek() else {
            return Err(self.err_at("expected a value"));
        };
        match t.kind {
            Kind::Quoted => {
                let v = t.text.clone();
                self.pos += 1;
                Ok(v)
            }
            Kind::Word if !is_reserved(&t.text) => {
                let v = t.text.clone();
                self.pos += 1;
                Ok(v)
            }
            Kind::Word => Err(self.err_at("a reserved word cannot be a bare value, quote it")),
            Kind::LBracket => Err(self.err_at("a list is only allowed after `in`")),
            _ => Err(self.err_at("expected a value")),
        }
    }

    fn list(&mut self) -> Result<Vec<String>, String> {
        match self.peek() {
            Some(t) if t.kind == Kind::LBracket => self.pos += 1,
            _ => return Err(self.err_at("`in` needs a list like `[a, b]`")),
        }
        let mut out = Vec::new();
        loop {
            match self.peek() {
                Some(t) if t.kind == Kind::LBracket => {
                    return Err(self.err_at("nested lists are not supported"));
                }
                Some(t) if t.kind == Kind::RBracket => {
                    return Err(self.err_at("expected a value (empty list or trailing comma)"));
                }
                _ => {}
            }
            out.push(self.scalar()?);
            match self.peek() {
                Some(t) if t.kind == Kind::Comma => self.pos += 1,
                Some(t) if t.kind == Kind::RBracket => {
                    self.pos += 1;
                    return Ok(out);
                }
                _ => return Err(self.err_at("expected `,` or `]`")),
            }
        }
    }

    fn comparison(&mut self) -> Result<Comparison, String> {
        let key = self.key()?;
        let op = match self.peek() {
            Some(t) if t.kind == Kind::EqEq => {
                self.pos += 1;
                Op::Eq(self.scalar()?)
            }
            Some(t) if t.kind == Kind::Ne => {
                self.pos += 1;
                Op::Ne(self.scalar()?)
            }
            Some(t) if t.kind == Kind::Word && t.text == "in" => {
                self.pos += 1;
                Op::In(self.list()?)
            }
            _ => return Err(self.err_at("expected an operator: `==`, `!=` or `in`")),
        };
        Ok(Comparison { key, op })
    }
}

/// Parse a condition. Errors quote the offending token and its 1-based column.
pub fn parse(src: &str) -> Result<Condition, String> {
    if src.trim().is_empty() {
        return Err("condition is empty".to_string());
    }
    let toks = tokenize(src)?;
    let mut p = Parser { src, toks, pos: 0 };
    let mut comparisons = vec![p.comparison()?];
    let mut joiner = Joiner::Single;
    while let Some(t) = p.peek() {
        let this = match (t.kind == Kind::Word).then_some(t.text.as_str()) {
            Some("and") => Joiner::And,
            Some("or") => Joiner::Or,
            _ => return Err(p.err_at("expected `and`, `or` or the end of the condition")),
        };
        if joiner != Joiner::Single && joiner != this {
            return Err(p.err_at(MIX_PHRASE));
        }
        joiner = this;
        p.pos += 1;
        comparisons.push(p.comparison()?);
    }
    Ok(Condition {
        joiner,
        comparisons,
    })
}

// --------------------------------------------------------------- evaluation

/// Evaluate against the published facts (strict about unpublished keys).
pub fn evaluate(c: &Condition, facts: &Facts) -> Eval {
    let mut results = Vec::with_capacity(c.comparisons.len());
    for cmp in &c.comparisons {
        let Some(fact) = facts.get(&cmp.key) else {
            return Eval::Unpublished {
                key: cmp.key.clone(),
            };
        };
        results.push(match &cmp.op {
            Op::Eq(v) => fact == v,
            Op::Ne(v) => fact != v,
            Op::In(vs) => vs.iter().any(|v| v == fact),
        });
    }
    let truth = match c.joiner {
        Joiner::Single => results[0],
        Joiner::And => results.iter().all(|r| *r),
        Joiner::Or => results.iter().any(|r| *r),
    };
    if truth { Eval::True } else { Eval::False }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(pairs: &[(&str, &str)]) -> Facts {
        let mut f = Facts::new();
        for (k, v) in pairs {
            f.insert(*k, *v);
        }
        f
    }

    fn eval(src: &str, pairs: &[(&str, &str)]) -> Eval {
        evaluate(&parse(src).expect("parses"), &facts(pairs))
    }

    fn unpublished(key: &str) -> Eval {
        Eval::Unpublished {
            key: key.to_string(),
        }
    }

    #[test]
    fn the_acceptance_test_rule_6_parses_and_evaluates_without_parentheses() {
        let src = "triage.risk == elevated or triage.size in [medium, large]";
        let table = [
            ("elevated", "small", Eval::True),
            ("elevated", "large", Eval::True),
            ("elevated", "medium", Eval::True),
            ("normal", "large", Eval::True),
            ("normal", "medium", Eval::True),
            ("normal", "small", Eval::False),
            ("normal", "Large", Eval::False),
            ("Elevated", "small", Eval::False),
        ];
        for (risk, size, want) in table {
            let got = eval(src, &[("triage.risk", risk), ("triage.size", size)]);
            assert_eq!(got, want, "risk={risk} size={size}");
        }
    }

    #[test]
    fn and_or_mixed_in_one_condition_is_refused() {
        for src in [
            "a.x == 1 and a.y == 2 or a.z == 3",
            "a.x == 1 or a.y == 2 and a.z == 3",
        ] {
            let err = parse(src).unwrap_err();
            assert!(err.contains(MIX_PHRASE), "{err}");
            assert!(
                err.contains("and") && err.contains("or") && err.contains("mix"),
                "{err}"
            );
        }
    }

    #[test]
    fn and_alone_and_or_alone_chains_of_three_are_accepted() {
        let all = "a.x == 1 and a.y == 2 and a.z == 3";
        let any = "a.x == 1 or a.y == 2 or a.z == 3";
        let none = [("a.x", "0"), ("a.y", "0"), ("a.z", "0")];
        let one = [("a.x", "0"), ("a.y", "2"), ("a.z", "0")];
        let every = [("a.x", "1"), ("a.y", "2"), ("a.z", "3")];
        assert_eq!(eval(all, &every), Eval::True);
        assert_eq!(eval(all, &one), Eval::False);
        assert_eq!(eval(any, &one), Eval::True);
        assert_eq!(eval(any, &none), Eval::False);
    }

    #[test]
    fn a_table_of_malformed_conditions_is_refused() {
        let table: &[(&str, &str)] = &[
            ("a.b in medium", "`medium`"),
            ("a.b == [x]", "`[`"),
            ("a.b in []", "`]`"),
            ("a.b in [x,]", "`]`"),
            ("a.b in [,x]", "`,`"),
            ("a.b in [x,,y]", "`,`"),
            ("a.b in [[x]]", "`[`"),
            ("a.b in [x", "`<end>`"),
            ("a.b in [x y]", "`y`"),
            ("(a.b == x)", "`(`"),
            ("a.b == x)", "`)`"),
            ("a.b = x", "`=`"),
            ("a.b ! x", "`!`"),
            ("a.b < x", "`<`"),
            ("a.b >= x", "`>`"),
            ("a.b ~ x", "`~`"),
            ("a.b == ", "`<end>`"),
            ("a.b == x and", "`<end>`"),
            ("a.b == x a.c == y", "`a.c`"),
            ("a.b == x and or c.d == y", "`or`"),
            ("a.b x", "`x`"),
            ("plain == x", "`plain`"),
            ("a.b == and", "`and`"),
            ("a.b == \"x", "`\"x`"),
            ("\"\"", "`\"\"`"),
            ("", "condition is empty"),
            ("   ", "condition is empty"),
        ];
        for (src, needle) in table {
            let err = parse(src).expect_err(src);
            assert!(err.contains(needle), "{src:?} -> {err}");
        }
        let err = parse("a.b == x a.c == y").unwrap_err();
        assert!(err.contains("column 10"), "{err}");
    }

    #[test]
    fn quoted_values_may_hold_spaces_and_keywords() {
        assert_eq!(
            eval("a.b == \"needs review\"", &[("a.b", "needs review")]),
            Eval::True
        );
        assert_eq!(eval("a.b == 'and'", &[("a.b", "and")]), Eval::True);
        assert_eq!(eval("a.b in [\"x y\", z]", &[("a.b", "x y")]), Eval::True);
        assert_eq!(eval("a.b in [\"x y\", z]", &[("a.b", "z")]), Eval::True);
        assert_eq!(eval("a.b == \"\"", &[("a.b", "")]), Eval::True);
        assert_eq!(eval("a.b == \"\"", &[("a.b", "x")]), Eval::False);
    }

    #[test]
    fn not_equal_is_true_when_the_key_is_published_and_different() {
        let table = [
            ("a.b == x", "x", Eval::True),
            ("a.b == x", "y", Eval::False),
            ("a.b != x", "y", Eval::True),
            ("a.b != x", "x", Eval::False),
            ("a.b in [x, y]", "y", Eval::True),
            ("a.b in [x, y]", "z", Eval::False),
        ];
        for (src, fact, want) in table {
            assert_eq!(eval(src, &[("a.b", fact)]), want, "{src} with {fact}");
        }
        for src in ["a.b == x", "a.b != x", "a.b in [x, y]"] {
            assert_eq!(eval(src, &[]), unpublished("a.b"), "{src}");
        }
    }

    #[test]
    fn an_unpublished_key_names_the_first_missing_key_in_source_order() {
        let src = "a.x == 1 or b.y == 2";
        assert_eq!(eval(src, &[]), unpublished("a.x"));
        assert_eq!(eval(src, &[("a.x", "1")]), unpublished("b.y"));
        assert_eq!(eval(src, &[("b.y", "2")]), unpublished("a.x"));
    }

    #[test]
    fn bare_true_compares_as_the_string_true() {
        let src = "plan.done != true";
        assert_eq!(eval(src, &[("plan.done", "true")]), Eval::False);
        assert_eq!(eval(src, &[("plan.done", "false")]), Eval::True);
        assert_eq!(eval(src, &[]), unpublished("plan.done"));
    }

    #[test]
    fn comparison_is_case_sensitive() {
        assert_eq!(eval("a.b == elevated", &[("a.b", "Elevated")]), Eval::False);
        assert_eq!(eval("a.b in [x]", &[("a.b", "X")]), Eval::False);
        assert_eq!(eval("A.b == x", &[("a.b", "x")]), unpublished("A.b"));
    }

    #[test]
    fn whitespace_around_tokens_is_free() {
        for src in ["a.b==x", "  a.b  ==  x  ", "a.b!=y", "a.b in[x,y]"] {
            assert!(parse(src).is_ok(), "{src}");
        }
        assert_eq!(eval("a.b==x", &[("a.b", "x")]), Eval::True);
        assert_eq!(eval("  a.b  ==  x  ", &[("a.b", "x")]), Eval::True);
    }

    #[test]
    fn keys_lists_every_key_read_in_order() {
        let c = parse("b.y == 1 or a.x in [p, q] or b.y != 2").unwrap();
        assert_eq!(c.keys().collect::<Vec<_>>(), ["b.y", "a.x", "b.y"]);
    }

    #[test]
    fn comparisons_yield_each_key_with_its_literals() {
        let c = parse("a.x == \"needs review\" or a.y in [p, 3] or a.z != true").unwrap();
        assert_eq!(
            c.comparisons(),
            vec![
                ("a.x", vec!["needs review".to_string()]),
                ("a.y", vec!["p".to_string(), "3".to_string()]),
                ("a.z", vec!["true".to_string()]),
            ]
        );
    }
}
