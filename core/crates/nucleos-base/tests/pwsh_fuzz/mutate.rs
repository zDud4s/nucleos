//! Seed-line mutator: 1-3 random edits of a real line, or a concatenation of two lines.

use crate::generator::{MAX_LEN, Rng, cap};

const INSERTS: &[&str] = &[
    "'", "\"", "`", "$", "(", ")", "{", "}", "[", "]", "|", "&", ";", "#", "<", ">", "@", "?", ":",
    ",", ".", "=", "\n", "\r\n", "--%", "<#", "#>", "\u{201C}", "\u{00A0}", "\0",
];
const JOINERS: &[&str] = &[";", "|", "&&", "\n"];
const SWAPS: &[(char, char)] = &[
    ('\'', '"'),
    ('"', '\''),
    ('(', '{'),
    (')', '}'),
    ('{', '('),
    ('}', ')'),
    ('[', '('),
    (']', ')'),
    ('|', ';'),
    (';', '|'),
    ('&', '|'),
];

/// Parses `seeds.txt` into non-empty lines.
pub fn load_seeds(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn one_edit(r: &mut Rng, c: &mut Vec<char>) {
    let len = c.len();
    match r.below(9) {
        0 if len > 0 => {
            c.remove(r.below(len));
        }
        1 if len > 0 => {
            let i = r.below(len);
            let ch = c[i];
            c.insert(i, ch);
        }
        2 if len > 1 => {
            let i = r.below(len - 1);
            c.swap(i, i + 1);
        }
        3 if len > 0 => {
            // "Byte" flip: toggle the low bit of an ASCII char.
            let i = r.below(len);
            if c[i].is_ascii() {
                c[i] = ((c[i] as u8) ^ 1) as char;
            }
        }
        4 => {
            let i = r.below(len + 1);
            for (k, ch) in r.pick(INSERTS).chars().enumerate() {
                c.insert(i + k, ch);
            }
        }
        5 if len > 1 => c.truncate(1 + r.below(len - 1)),
        6 if len > 0 => {
            // Swap one delimiter for another, at a random occurrence.
            let at: Vec<usize> = (0..len)
                .filter(|&i| SWAPS.iter().any(|s| s.0 == c[i]))
                .collect();
            if !at.is_empty() {
                let i = at[r.below(at.len())];
                if let Some(s) = SWAPS.iter().find(|s| s.0 == c[i]) {
                    c[i] = s.1;
                }
            }
        }
        7 if len > 0 => {
            // Delete a short run.
            let i = r.below(len);
            let n = (1 + r.below(4)).min(len - i);
            c.drain(i..i + n);
        }
        _ => {
            let i = r.below(len + 1);
            c.insert(i, *r.pick(&['\'', '"', '$', '(', '{', '|', '`']));
        }
    }
}

/// One mutated case derived from `seeds` (non-empty), at most 4 KiB.
pub fn mutate(r: &mut Rng, seeds: &[String]) -> String {
    let mut s = r.pick(seeds).clone();
    if r.chance(15) {
        s.push_str(r.pick(JOINERS));
        s.push_str(r.pick(seeds));
    }
    let mut c: Vec<char> = s.chars().collect();
    for _ in 0..(1 + r.below(3)) {
        one_edit(r, &mut c);
    }
    cap(c.into_iter().collect(), MAX_LEN)
}
