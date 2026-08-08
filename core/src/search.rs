//! Turning a person's words into an FTS5 query.
//!
//! One function, and it is here rather than in a caller because it had been written twice —
//! `runs.rs` and `web.rs` each carried a copy, identical and independent. Two copies of a sanitiser
//! is the arrangement where one of them is fixed and the other is not, and `sidecar.rs` already
//! records the general form of that lesson: "the copy that drifts is always the one nobody is
//! reading". A third caller is the moment to stop having copies.

/// Quotes each word so FTS5 reads a search box as words rather than as syntax.
///
/// Everything a person types is a term. `NEAR`, `*`, `:`, `^`, `-` and parentheses all mean
/// something to FTS5, so a query typed by a person — or worse, composed by a model out of a page it
/// just read — is a small injection surface into the index. Quoting every term as a literal removes
/// the whole category. The cost is that nobody can write an FTS expression on purpose; the benefit
/// is that nobody can write one by accident either.
///
/// Quotes are STRIPPED rather than doubled, and the two callers this was consolidated from
/// disagreed on exactly that. `unicode61` treats every non-alphanumeric character as a separator, so
/// a quote is never a token and stripping it cannot change which rows match. Doubling preserves a
/// character that can never be searched for, and turns an input of nothing but quotes into a phrase
/// containing no tokens — a query FTS5 has no good answer for. Stripping turns the same input into
/// the empty string, which callers already handle.
///
/// Returns an empty string for input with no searchable words. Callers must treat that as "no FTS
/// clause" rather than passing it to `MATCH`, which errors on an empty query.
pub(crate) fn fts_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(|term| term.replace('"', ""))
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::fts_query;

    #[test]
    fn every_word_becomes_a_quoted_term() {
        assert_eq!(fts_query("hello world"), "\"hello\" \"world\"");
    }

    /// The reason this function exists. Each of these is valid FTS5 syntax and none of them is what
    /// the person meant.
    #[test]
    fn operators_are_treated_as_words() {
        assert_eq!(fts_query("cats AND dogs"), "\"cats\" \"AND\" \"dogs\"");
        assert_eq!(fts_query("re-run"), "\"re-run\"");
        assert_eq!(fts_query("wild*"), "\"wild*\"");
        assert_eq!(fts_query("^start"), "\"^start\"");
    }

    #[test]
    fn a_quote_cannot_close_the_quoting() {
        assert_eq!(fts_query("say \"hi\""), "\"say\" \"hi\"");
    }

    /// The degenerate input that made stripping the right choice over doubling: quotes alone leave
    /// no term at all, and must reach the caller as "no FTS clause" rather than as a phrase FTS5
    /// cannot evaluate.
    #[test]
    fn quotes_alone_leave_nothing_to_search_for() {
        assert_eq!(fts_query("\"\"\""), "");
        assert_eq!(fts_query("\" \" \""), "");
    }

    /// Empty input must be distinguishable by the caller, because `MATCH ''` is an error rather than
    /// a query that matches nothing.
    #[test]
    fn wordless_input_produces_an_empty_query() {
        assert_eq!(fts_query(""), "");
        assert_eq!(fts_query("   \t\n "), "");
    }
}
