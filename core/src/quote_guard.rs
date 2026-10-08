//! Whether a text repeats a run of words from any of a set of sources.
//!
//! Spec `.ai/specs/2026-10-07-fronteira-prompts-design.md` §3. The distiller asks this about every
//! new learning against the owner's text that went into its dossier, so that a quotation reaches no
//! prompt without the owner's yes. It catches quotation only, never paraphrase: deterministic,
//! cheap, and testable in a table. PURE: knows no notes, no store, no SQL.

use std::collections::HashSet;

/// A run of this many consecutive normalised words shared with a source is a quotation.
pub const QUOTE_WORDS: usize = 8;
/// A source shorter than `QUOTE_WORDS` counts only when it is repeated whole, and only from this
/// many words up: with less, a match is noise.
pub const QUOTE_MIN_WORDS: usize = 4;

/// The Latin letters Portuguese and English use, reduced to their base letter.
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        other => other,
    }
}

/// Lowercased, diacritics folded, every non-alphanumeric character a separator.
pub fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars().flat_map(char::to_lowercase).map(fold) {
        if c.is_alphanumeric() {
            current.push(c);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// True when `text` repeats `QUOTE_WORDS` consecutive words of some source, or the whole of a
/// source of `QUOTE_MIN_WORDS..QUOTE_WORDS` words.
pub fn quotes(text: &str, sources: &[&str]) -> bool {
    let text = words(text);
    sources.iter().any(|source| {
        let source = words(source);
        if source.len() < QUOTE_MIN_WORDS {
            return false;
        }
        let width = source.len().min(QUOTE_WORDS);
        if text.len() < width {
            return false;
        }
        let windows: HashSet<&[String]> = source.windows(width).collect();
        text.windows(width).any(|w| windows.contains(w))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "the staging database is restored from the friday backup every week";

    #[test]
    fn the_table() {
        let cases: &[(&str, &str, &[&str], bool)] = &[
            (
                "eight shared words quote",
                "We learned the staging database is restored from the friday backup.",
                &[NOTE],
                true,
            ),
            (
                "seven shared words do not",
                "Note: the staging database is restored from the monday dump.",
                &[NOTE],
                false,
            ),
            (
                "case, accents and punctuation do not hide it",
                "THE STAGING DATABASE — is restored, from the Fríday backup!",
                &[NOTE],
                true,
            ),
            (
                "a short source repeated whole quotes",
                "Remember: never deploy on Fridays, ever.",
                &["never deploy on fridays"],
                true,
            ),
            (
                "a short source repeated in part does not",
                "never deploy on mondays",
                &["never deploy on fridays"],
                false,
            ),
            (
                "a source under the minimum never counts",
                "use the cache",
                &["use the cache"],
                false,
            ),
            (
                "no sources, no quotation",
                "the staging database is restored from the friday backup",
                &[],
                false,
            ),
            (
                "an empty source is not a quotation",
                "anything at all here",
                &["", "   "],
                false,
            ),
            (
                "any one source is enough",
                "the staging database is restored from the friday backup",
                &["unrelated words only here", NOTE],
                true,
            ),
        ];
        for (name, text, sources, expected) in cases {
            assert_eq!(quotes(text, sources), *expected, "{name}");
        }
    }

    #[test]
    fn words_fold_and_split() {
        assert_eq!(words("Ação, já-feita!"), vec!["acao", "ja", "feita"]);
        assert!(words("  ...  ").is_empty());
    }
}
