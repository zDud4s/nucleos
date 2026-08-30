//! §spec mapa-do-projeto
//!
//! What a single file declares, and which of those declarations use each other.
//!
//! **The step below the file, and the reason the ladder needed one.** The map draws the project as
//! a matrix of communities and a community as a graph of files. A file is where that stops, and a
//! file is not where the question stops: *está como eu queria?* is answered by looking at the thing
//! that was built, and the thing that was built is a function with a name.
//!
//! **The instrument is the layered drawing, and this time the measurement says so rather than
//! ruling it out.** One level up the argument ran the other way: the núcleo is 5.9 dependencies a
//! file, node-link drawings stop reading near 2.5, and that is why the project as a whole is a
//! matrix. Inside a file the same measurement comes back **0.90 references an item at the median
//! and 1.65 at the 90th percentile — and of 272 files, not one is over the 2.6 the drawing
//! refuses at**. What forces a matrix upstairs is absent downstairs, so the picture that could not
//! be drawn for the project draws cleanly for the file.
//!
//! Measured over this repository, 2026-08-30:
//!
//! | | |
//! |---|---:|
//! | files read | 272 |
//! | items | 5622 |
//! | references between them | 7188 |
//! | items a file, median / p90 / max | 13 / 45 / 385 |
//! | files drawable under both thresholds | **243 of 272** |
//! | files refused, all of them on size alone | 29 |
//! | items carrying a doc comment | 3833 |
//! | items reachable from outside their file | 2335 |
//!
//! The 29 refusals are `http.rs` and its kind, and they refuse for the reason the level above
//! refuses: 385 boxes is not a picture. They come back with the numbers that decided it, which is
//! the one thing a drawing that cannot be drawn can still honestly say.
//!
//! **Deliberately not a parser, exactly as [`crate::project_map::rust_imports`] is not one.** That
//! decision is argued in full there and the argument has not changed: pulling `syn` in would buy
//! precision this screen cannot spend, and would buy nothing at all for TypeScript, which would
//! still need a second parser with a second set of failures. What *is* new here is that the scan
//! has to survive being wrong in a visible way rather than an invisible one, so every limit this
//! reader has is named in [`Items::missed`] and drawn on the screen beside the picture.
//!
//! **What this reader still does not see, said rather than discovered later.** A destructuring
//! declaration — `const { theme } = config` — names things this finds no name for, and a
//! multi-name one — `const a = 1, b = 2` — is read as `a` alone. A class field holding an arrow,
//! `handle = (e) => {}`, is not a method here. None of these is counted into [`Items::missed`],
//! which is the honest shape of the gap: that count covers what the scan knowingly walked past,
//! and not what it never recognised at all. They are written down here so the next reader finds
//! them stated rather than measuring them again.
//!
//! **The mask preserves positions, and that is the one thing this module does that its neighbour
//! does not.** `project_map::without_comments` collapses a comment to a single space because it
//! only ever asks *does this text contain `crate::`*. Here every answer is a location — which line
//! an item starts on, which span its body covers — so a mask that shortened the text would report
//! item positions that drift further from the truth the better a file is documented. This one
//! replaces a masked character with a space and a masked newline with a newline, so the copy is
//! the same length as the original and a char index means the same thing in both.

use crate::project_map::{Reader, reader_for};
use serde::Serialize;

/// What kind of thing a declaration is, at the resolution a reader cares about.
///
/// **Coarser than either language's own grammar, on purpose.** Rust separates `struct`, `enum`,
/// `union` and `type`; TypeScript separates `interface` and `type`. On a drawing they are one
/// shape — *this names a shape of data* — and four legends nobody reads is worse than one word
/// that is true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A free function: `fn`, `function`, or a `const` bound to an arrow.
    Function,
    /// A function inside an `impl`, a `trait` or a `class` — the `Type::method` of the id.
    Method,
    /// `struct`, `enum`, `union`, `type`, `interface`, `class`.
    Shape,
    /// `const`, `static`, and a TypeScript `const` that is not a function.
    Constant,
    /// A `mod` with a body. A `mod x;` declares a file and is [`crate::project_map`]'s business.
    Module,
    /// A `trait` — the shape of a contract rather than of data.
    Contract,
}

/// One thing a file declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    /// Unique within the file: `name`, or `Type::method` for anything with a container.
    ///
    /// **Qualified because bare names collide and the collision is silent.** A file with two
    /// `impl` blocks that both define `new` has two different functions called `new`, and a graph
    /// keyed on the bare name would draw one box with both of their edges — a wrong answer that
    /// looks exactly like a right one, which is the single failure this map exists to refuse.
    pub id: String,
    /// What a box is labelled with: the bare name, without its container.
    pub name: String,
    /// The type, trait or class this belongs to, or `None` at the top level.
    pub container: Option<String>,
    pub kind: ItemKind,
    /// `pub` in any of its forms, or `export` in TypeScript.
    ///
    /// **One flag and not the full lattice.** Rust has `pub`, `pub(crate)`, `pub(super)` and
    /// `pub(in path)`; the question this screen asks is *can anything outside this file reach it*,
    /// and all four answer yes. A drawing that distinguished them would be reporting a fact about
    /// Rust rather than a fact about the project.
    pub exported: bool,
    /// A doc comment sits immediately above it — `///`, `//!`, `/** */`, attributes skipped.
    ///
    /// **Adjacency is the whole test, because in both languages it is the whole rule.** A blank
    /// line between a comment and a declaration means the comment documents nothing, and counting
    /// it would report a file as explained when it is not.
    pub documented: bool,
    /// 1-based, and it is the line the declaration starts on rather than the line its doc does.
    pub line: usize,
}

/// One declaration names another, inside the file that declares both.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Reference {
    pub from: String,
    pub to: String,
}

/// Everything one file says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Items {
    pub path: String,
    /// `None` when nobody here reads this language — and then `items` is empty rather than absent.
    pub reader: Option<Reader>,
    pub items: Vec<Item>,
    /// Which of them use which, within this file only.
    pub references: Vec<Reference>,
    /// What this reader knows it did not catch, in the reader's words.
    ///
    /// **On the wire and on the screen, because a level that hides its own blind spots is the
    /// §16.5 rule broken one floor down.** *Descer um nível pode esconder detalhe; nunca pode
    /// esconder uma costura.* A file whose picture is four boxes when it defines forty things has
    /// told the owner something false about their own code, and the only thing standing between
    /// this drawing and that is a sentence saying which forty it could not see.
    pub missed: Vec<String>,
}

/// Everything `path` declares, and which of those things use each other.
///
/// **One file, and never a walk.** The level above already knows which files exist; asking this to
/// find them too would be a second answer to a question [`crate::project_map::structure`] has
/// already answered, free to disagree with it about a project whose folder moved in between.
pub fn items(path: &str, source: &str) -> Items {
    let reader = reader_for(path);
    let (found, missed) = match reader {
        Some(Reader::Rust) => rust_items(source),
        Some(Reader::Typescript) => ts_items(source),
        None => (Vec::new(), Vec::new()),
    };
    let references = references(source, &found, reader == Some(Reader::Rust));
    Items {
        path: path.to_string(),
        reader,
        items: found.into_iter().map(|entry| entry.item).collect(),
        references,
        missed,
    }
}

/// The source with comments and string contents blanked, and every position left where it was.
///
/// A masked character becomes a space and a masked newline stays a newline, so the result has the
/// same length and the same line breaks as the input. That is what lets the scan report a line
/// number by counting newlines in the masked copy and have it be true of the real file.
///
/// It has to know what a string is for [`crate::project_map`]'s reason — a `//` inside a URL
/// literal would otherwise swallow the rest of its line — and it blanks the *contents* of strings
/// as well, which that function does not, because here a `fn` inside a string literal would become
/// a box on somebody's screen.
///
/// Raw strings are handled, block comments nest, and template literals are strings. The one case
/// left is a `'"'` character literal, which opens a string here and closes it at the next quote:
/// the same gap [`crate::project_map`] declares, in the same words, and for the same reason it is
/// cheaper to say so than to teach this the difference between a character literal and a lifetime.
fn mask(source: &str, rust: bool) -> Vec<char> {
    let chars: Vec<char> = source.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;

    // Blank one character, keeping a newline so line numbers survive.
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };

    while i < chars.len() {
        let here = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');

        // `r"..."` / `r#"..."#` — text all the way through, hashes and all.
        if here == 'r' && (next == '"' || next == '#') {
            let mut hashes = 0;
            let mut at = i + 1;
            while chars.get(at) == Some(&'#') {
                hashes += 1;
                at += 1;
            }
            if chars.get(at) == Some(&'"') {
                for c in &chars[i..=at] {
                    out.push(blank(*c));
                }
                at += 1;
                while at < chars.len() {
                    let closes = chars[at] == '"'
                        && (1..=hashes).all(|step| chars.get(at + step) == Some(&'#'));
                    if closes {
                        for c in &chars[at..=at + hashes] {
                            out.push(blank(*c));
                        }
                        at += hashes + 1;
                        break;
                    }
                    out.push(blank(chars[at]));
                    at += 1;
                }
                i = at;
                continue;
            }
        }

        if here == '"' || here == '\'' || here == '`' {
            let quote = here;
            // A Rust lifetime — `'a` — is not a string, and treating it as one would blank the
            // rest of the file from the first generic parameter onwards.
            //
            // **Only in Rust, and the asymmetry is the whole point.** TypeScript has no
            // lifetimes and does have 'single quoted strings', which open with a letter
            // exactly as a lifetime does. Asking this question of a TypeScript file answered
            // *lifetime* for every such string, left its contents unmasked, and turned a
            // `function` inside one into a box on somebody's screen — a declaration that does
            // not exist, drawn as confidently as one that does. Measured when it was fixed:
            // **61 of this repository's 5587 declarations were never declared at all**, and
            // every one of them came from a single-quoted string.
            if rust && quote == '\'' && is_lifetime(&chars, i) {
                out.push(here);
                i += 1;
                continue;
            }

            // **In JavaScript a quoted string cannot contain a newline, and that rule is the whole
            // defence against JSX.** `<h1>What's new</h1>` is prose, not a string, and reading it
            // as one blanked the rest of the file — every later declaration gone, `missed` empty,
            // nothing to notice it by. Silent loss is the one answer this module may not give. So a
            // `'` or `"` that reaches the end of its line never opened a string, and the text is
            // put back exactly as it was. A backtick is left alone: a template literal spans lines
            // on purpose, and so does a Rust string.
            let bounded = !rust && quote != '`';
            let opened_out = out.len();
            let opened_at = i;
            let mut closed = false;

            out.push(here);
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    out.push(blank(chars[i]));
                    if let Some(&escaped) = chars.get(i + 1) {
                        out.push(blank(escaped));
                    }
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    out.push(quote);
                    i += 1;
                    closed = true;
                    break;
                }
                if bounded && chars[i] == '\n' {
                    break;
                }
                out.push(blank(chars[i]));
                i += 1;
            }

            if bounded && !closed {
                out.truncate(opened_out);
                out.extend_from_slice(&chars[opened_at..i]);
            }
            continue;
        }

        if here == '/' && next == '/' {
            while i < chars.len() && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }

        if here == '/' && next == '*' {
            let mut depth = 1;
            out.push(' ');
            out.push(' ');
            i += 2;
            while i < chars.len() && depth > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                } else {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
            continue;
        }

        out.push(here);
        i += 1;
    }
    out
}

/// Whether the quote at `at` opens a Rust lifetime rather than a character literal.
///
/// `'a` and `'static` are followed by an identifier and never closed. A real character literal is
/// one character (or an escape) and then another quote — so the test is what sits two along.
fn is_lifetime(chars: &[char], at: usize) -> bool {
    if chars.get(at + 1).is_some_and(|c| *c == '\\') {
        return false;
    }
    match (chars.get(at + 1), chars.get(at + 2)) {
        (Some(c), Some('\'')) if *c != '\'' => false,
        (Some(c), _) if c.is_ascii_alphabetic() || *c == '_' => true,
        _ => false,
    }
}

/// The 1-based line a char index sits on.
fn line_at(masked: &[char], at: usize) -> usize {
    masked[..at.min(masked.len())]
        .iter()
        .filter(|c| **c == '\n')
        .count()
        + 1
}

/// The identifier starting at `at`, and where it ends.
fn ident_at(chars: &[char], at: usize) -> Option<(String, usize)> {
    let mut end = at;
    while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
        end += 1;
    }
    if end == at {
        None
    } else {
        Some((chars[at..end].iter().collect(), end))
    }
}

/// The next index at or after `at` that is not whitespace.
fn skip_space(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    at
}

/// Whether a word starting at `at` is a whole word rather than the tail of a longer one.
fn word_start(chars: &[char], at: usize) -> bool {
    at == 0 || !(chars[at - 1].is_alphanumeric() || chars[at - 1] == '_')
}

/// Whether the declaration at `at` has a doc comment immediately above it.
///
/// Reads the **original** source and not the mask, because the mask has by construction erased the
/// thing this is looking for. Attribute lines are skipped — `#[derive(...)]` between a doc comment
/// and its item is normal and does not detach it — and so are plain `//` lines, which the language
/// ignores entirely. A blank line is not skipped, because in both languages a blank line is exactly
/// what detaches a comment from what follows it.
///
/// **An attribute that rustfmt wrapped is still one attribute**, and its continuation lines start
/// with none of the markers above. Walking up counts the brackets it has closed and not yet
/// reopened, so a `)]` on its own line carries the walk past `Clone,` and `Debug,` to the `#[derive(`
/// that owns them. Without that, every long `#[derive(...)]` or `#[serde(...)]` in this repository
/// reported its item as undocumented — understating the one number this level leads with.
///
/// **A plain `//` above a declaration is not documentation, and that is a deliberate line rather
/// than an oversight.** `///` is what the language itself prints and what somebody outside the file
/// can read; a note to whoever edits the body next is a different thing serving a different reader.
/// Counting both would make this number mean *somebody wrote something near here*, which nobody
/// needs.
fn documented_above(source: &[char], at: usize) -> bool {
    let mut line_end = start_of_line(source, at);
    // How many brackets the lines walked so far have closed without reopening: above zero, we are
    // inside a wrapped attribute and whatever the line looks like is a continuation of it.
    let mut unclosed = 0i32;
    // A doc comment is adjacent by definition, so an unbounded walk can only find something that
    // is not one.
    for _ in 0..24 {
        if line_end == 0 {
            return false;
        }
        let above_end = line_end - 1;
        let above_start = start_of_line(source, above_end);
        let line: String = source[above_start..above_end].iter().collect();
        let line = line.trim();

        if unclosed > 0 {
            unclosed += closed_minus_opened(line);
            unclosed = unclosed.max(0);
            line_end = above_start;
            continue;
        }
        if line.is_empty() {
            return false;
        }
        if line.starts_with("///") || line.starts_with("//!") || line.starts_with("/**") {
            return true;
        }
        // A closing `*/` is the last line of a block comment, which is a doc comment in
        // TypeScript whenever it opened with `/**`. Cheap to accept, and the false positive is a
        // plain `/* ... */` above a declaration, which is documentation by any reading anyway.
        if line.ends_with("*/") {
            return true;
        }
        if line.starts_with('#') || line.starts_with('@') || line.starts_with("//") {
            line_end = above_start;
            continue;
        }
        let balance = closed_minus_opened(line);
        if balance > 0 {
            unclosed = balance;
            line_end = above_start;
            continue;
        }
        return false;
    }
    false
}

/// How many more brackets a line closes than it opens. Braces are deliberately not counted: a `}`
/// above a declaration is the end of the previous one, and never the tail of its attributes.
fn closed_minus_opened(line: &str) -> i32 {
    line.chars().fold(0, |sum, c| match c {
        ')' | ']' => sum + 1,
        '(' | '[' => sum - 1,
        _ => sum,
    })
}

/// The index of the first character of the line `at` sits on.
fn start_of_line(source: &[char], at: usize) -> usize {
    let mut start = at.min(source.len());
    while start > 0 && source[start - 1] != '\n' {
        start -= 1;
    }
    start
}

/// A declaration found mid-scan, before it is known whether it has a body.
struct Found {
    item: Item,
    /// Where its body opens and closes, when it has one.
    body: Option<(usize, usize)>,
}

/// What `impl Display for Foo` is an impl *of*.
///
/// The self type and never the trait: a method of `impl Display for Foo` belongs to `Foo`, and
/// filing it under `Display` would scatter one type's methods across as many containers as it
/// implements traits. Generics are dropped and the last path segment wins, so `impl<T>
/// crate::a::Foo<T>` is `Foo`.
///
/// **`None` is a refusal and the caller must report it, not swallow it.** A self type this cannot
/// name — `impl Trait for (A, B)`, or for `[u8; 4]` — used to drop the whole block along with
/// every method in it, and say nothing. Silence about a block of code is the one answer this
/// module is not allowed to give.
fn impl_target(header: &str) -> Option<String> {
    // `impl<T: Clone> Holder<T>` opens with the parameter list rather than the type, and taking
    // the text before the first `<` would then take nothing at all.
    //
    // The `>` of a `->` inside that list is not a closing bracket. `impl<F: Fn(u8) -> u8>
    // Runner<F>` cut there, and the container came out as `u8`.
    let header = header.trim_start();
    let header = if header.starts_with('<') {
        let mut depth = 0i32;
        let mut cut = None;
        let mut previous = ' ';
        for (at, c) in header.char_indices() {
            match c {
                '<' => depth += 1,
                '>' if previous != '-' && previous != '=' => {
                    depth -= 1;
                    if depth == 0 {
                        cut = Some(at + c.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
            previous = c;
        }
        &header[cut?..]
    } else {
        header
    };
    let body = header
        .split_once(" for ")
        .map(|(_, tail)| tail)
        .unwrap_or(header);

    // A self type may be written through a reference, a lifetime, a `mut`, or `dyn`, and none of
    // those is its name. `impl Display for &'a mut Foo` is an impl on `Foo`.
    let mut body = body.trim();
    loop {
        let trimmed = body
            .strip_prefix('&')
            .or_else(|| body.strip_prefix("mut "))
            .or_else(|| body.strip_prefix("dyn "))
            .or_else(|| {
                body.strip_prefix('\'')
                    .map(|rest| rest.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_'))
            });
        match trimmed {
            Some(rest) if rest.trim_start() != body => body = rest.trim_start(),
            _ => break,
        }
    }

    let body = body.split('<').next().unwrap_or(body);
    let body = body.rsplit("::").next().unwrap_or(body);
    let name: String = body
        .trim()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

/// From the name of a declaration, the `{` that opens its body — or nothing when it has none.
///
/// Stops at a `;` outside every bracket, which is what a `struct Foo;`, a `const X: u8 = 1;` and a
/// trait method with no default all end with. Brackets are counted so that a `{` inside a
/// parameter's default or a `where` clause's bound is not mistaken for the body.
fn body_of(masked: &[char], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    let (mut paren, mut angle, mut square) = (0i32, 0i32, 0i32);
    while i < masked.len() {
        match masked[i] {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => square += 1,
            ']' => square -= 1,
            // `->` and `=>` are not an angle bracket closing, and `<` in a comparison is not one
            // opening. Only counted while no other bracket is open, which is where a generic
            // parameter list actually appears.
            '<' if paren == 0 && square == 0 && masked.get(i + 1) != Some(&'<') => angle += 1,
            '>' if angle > 0
                && masked.get(i.wrapping_sub(1)) != Some(&'-')
                && masked.get(i.wrapping_sub(1)) != Some(&'=') =>
            {
                angle -= 1
            }
            ';' if paren <= 0 && square <= 0 => return None,
            '{' if paren <= 0 && square <= 0 => {
                let close = match_brace(masked, i)?;
                return Some((i, close));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The `}` closing the `{` at `open`, counting nested braces.
fn match_brace(masked: &[char], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < masked.len() {
        match masked[i] {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether `pub` (or `pub(crate)`, or `export`) sits immediately before `at`.
fn exported_before(masked: &[char], at: usize, word: &str) -> bool {
    let mut end = at;
    // Step back over whitespace, and over a `(crate)` / `(super)` / `(in path)` qualifier.
    loop {
        while end > 0 && masked[end - 1].is_whitespace() {
            end -= 1;
        }
        if end > 0 && masked[end - 1] == ')' {
            let mut depth = 0;
            while end > 0 {
                end -= 1;
                match masked[end] {
                    ')' => depth += 1,
                    '(' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        break;
    }
    let mut start = end;
    while start > 0 && (masked[start - 1].is_alphanumeric() || masked[start - 1] == '_') {
        start -= 1;
    }
    let previous: String = masked[start..end].iter().collect();
    previous == word
}

/// The keywords that open a Rust item, and what kind each one makes.
const RUST_KEYWORDS: &[(&str, ItemKind)] = &[
    ("fn", ItemKind::Function),
    ("struct", ItemKind::Shape),
    ("enum", ItemKind::Shape),
    ("union", ItemKind::Shape),
    ("type", ItemKind::Shape),
    ("trait", ItemKind::Contract),
    ("const", ItemKind::Constant),
    ("static", ItemKind::Constant),
    ("mod", ItemKind::Module),
];

/// Words that may stand between an item's keyword and its name.
///
/// **`const fn zero()` is a function called `zero` and not a constant called `fn`**, which is what
/// taking the very next word gave. The phantom was bad on its own and worse downstream: an item
/// named `fn` collects an edge from every body in the file that contains the word.
const RUST_MODIFIERS: &[&str] = &["mut", "unsafe", "async", "extern", "default"];

/// The name a Rust declaration actually gives, and the kind it settles on.
///
/// Walks the modifiers and the second keyword — `pub const fn`, `static mut`, `pub async unsafe
/// fn` — and lets the LAST keyword decide the kind, because that is the one that says what is
/// being declared. `extern "C" fn` puts a string in the way, and the mask left its quotes in
/// place, so a quote is stepped over rather than read.
fn rust_declared(
    masked: &[char],
    mut at: usize,
    mut kind: ItemKind,
) -> Option<(ItemKind, String, usize)> {
    for _ in 0..6 {
        at = skip_space(masked, at);
        if masked.get(at) == Some(&'"') {
            let mut end = at + 1;
            while end < masked.len() && masked[end] != '"' {
                end += 1;
            }
            at = end + 1;
            continue;
        }
        let (word, end) = ident_at(masked, at)?;
        if let Some((_, next)) = RUST_KEYWORDS.iter().find(|(keyword, _)| *keyword == word) {
            kind = *next;
            at = end;
            continue;
        }
        if RUST_MODIFIERS.contains(&word.as_str()) {
            at = end;
            continue;
        }
        return Some((kind, word, end));
    }
    None
}

/// What a Rust file declares, at the top level and one level inside every `impl` and `trait`.
///
/// **Two levels and not every level, which is a choice about the reader rather than about Rust.**
/// A function defined inside another function is a detail of that function's implementation; it
/// has no caller outside it, and drawing it would put a box on the screen whose only possible edge
/// is the one it is already inside. The count of what was skipped that way is reported rather than
/// dropped, because *four boxes for a forty-item file* is exactly the lie this level must not tell.
fn rust_items(source: &str) -> (Vec<Found>, Vec<String>) {
    let original: Vec<char> = source.chars().collect();
    let masked = mask(source, true);
    let mut found: Vec<Found> = Vec::new();
    let mut nested = 0usize;
    let mut unnamed = 0usize;

    let mut i = 0;
    while i < masked.len() {
        if !word_start(&masked, i) {
            i += 1;
            continue;
        }
        let Some((word, after)) = ident_at(&masked, i) else {
            i += 1;
            continue;
        };

        if word == "impl" {
            // Everything between `impl` and the body is the header the target is read from.
            let Some((open, close)) = body_of(&masked, after) else {
                i = after;
                continue;
            };
            let header: String = masked[after..open].iter().collect();
            match impl_target(&header) {
                Some(target) => {
                    let (inner, skipped) = rust_members(&original, &masked, open, close, &target);
                    found.extend(inner);
                    nested += skipped;
                }
                // A self type this reader cannot name — a tuple, a slice — used to take its whole
                // block with it in silence. It is counted now, so the drawing says a block is
                // missing instead of looking complete.
                None => unnamed += 1,
            }
            i = close + 1;
            continue;
        }

        let Some(kind) = RUST_KEYWORDS
            .iter()
            .find(|(keyword, _)| *keyword == word)
            .map(|(_, kind)| *kind)
        else {
            i = after;
            continue;
        };

        // `type` inside a `where` clause, a `const` in a generic parameter list, an `fn` inside a
        // function pointer type: all are the keyword without a declaration behind it. The test is
        // that a real declaration is followed by a name.
        let Some((kind, name, name_end)) = rust_declared(&masked, after, kind) else {
            i = after;
            continue;
        };

        // `mod x;` names a file and belongs to the level above, which already draws it as a box.
        let body = body_of(&masked, name_end);
        if kind == ItemKind::Module && body.is_none() {
            i = name_end;
            continue;
        }

        // A trait's own methods are items exactly as an impl's are, and for the same reason: they
        // are what somebody calls. Collected here rather than counted as nested functions, which
        // they are not — and which is what the `missed` line used to call them.
        if kind == ItemKind::Contract
            && let Some((open, close)) = body
        {
            let (inner, skipped) = rust_members(&original, &masked, open, close, &name);
            found.extend(inner);
            nested += skipped;
        }

        found.push(Found {
            item: Item {
                id: name.clone(),
                name,
                container: None,
                kind,
                exported: exported_before(&masked, i, "pub"),
                documented: documented_above(&original, i),
                line: line_at(&masked, i),
            },
            body,
        });
        i = match body {
            Some((open, close)) => {
                // A `fn` inside a `fn`, and equally a `fn` inside a `mod` body: neither is drawn,
                // and both are counted rather than lost. A trait's are neither — they were just
                // collected above.
                if kind != ItemKind::Contract {
                    nested += nested_fns(&masked, open, close);
                }
                close + 1
            }
            None => name_end,
        };
    }

    let (mut found, mut missed) = finish(found, nested, "nested function", "nested functions");
    if unnamed > 0 {
        missed.push(format!(
            "{unnamed} impl block{} whose type this reader could not name",
            if unnamed == 1 { "" } else { "s" }
        ));
    }
    found.sort_by_key(|entry| entry.item.line);
    (found, missed)
}

/// The `fn`s directly inside one `impl` or `trait` body, filed under the type they belong to.
fn rust_members(
    original: &[char],
    masked: &[char],
    open: usize,
    close: usize,
    container: &str,
) -> (Vec<Found>, usize) {
    let mut found = Vec::new();
    let mut skipped = 0usize;
    let mut i = open + 1;

    while i < close {
        if !word_start(masked, i) {
            i += 1;
            continue;
        }
        let Some((word, after)) = ident_at(masked, i) else {
            i += 1;
            continue;
        };
        if word != "fn" {
            i = after;
            continue;
        }
        let name_at = skip_space(masked, after);
        let Some((name, name_end)) = ident_at(masked, name_at) else {
            i = after;
            continue;
        };
        let body = body_of(masked, name_end).filter(|(_, end)| *end <= close);
        found.push(Found {
            item: Item {
                id: format!("{container}::{name}"),
                name,
                container: Some(container.to_string()),
                kind: ItemKind::Method,
                exported: exported_before(masked, i, "pub"),
                documented: documented_above(original, i),
                line: line_at(masked, i),
            },
            body,
        });
        match body {
            Some((inner_open, inner_close)) => {
                skipped += nested_fns(masked, inner_open, inner_close);
                i = inner_close + 1;
            }
            None => i = name_end,
        }
    }
    (found, skipped)
}

/// How many `fn`s hide inside a body, so the number can be reported rather than lost.
fn nested_fns(masked: &[char], open: usize, close: usize) -> usize {
    let mut count = 0;
    let mut i = open + 1;
    while i < close && i < masked.len() {
        if word_start(masked, i)
            && let Some((word, after)) = ident_at(masked, i)
        {
            // A declaration names something; `render: fn(&str) -> String` is a type and names
            // nothing. Counting it printed a wrong sentence under a right picture, which is worse
            // than no sentence, because `missed` is meant to be read.
            if word == "fn" && ident_at(masked, skip_space(masked, after)).is_some() {
                count += 1;
            }
            i = after;
            continue;
        }
        i += 1;
    }
    count
}

/// The words that may stand between `export` and the keyword, or open a class member.
const TS_MODIFIERS: &[&str] = &[
    "export",
    "default",
    "async",
    "declare",
    "abstract",
    "public",
    "private",
    "protected",
    "static",
    "readonly",
];

/// Whether a keyword at `at` opens a statement, rather than sitting in the middle of something.
///
/// **This is what separates a declaration from prose, and in a `.tsx` file that is not a hair to
/// split.** JSX text lives at brace depth zero exactly as a top-level declaration does, so
/// `<p>Every function must return a value</p>` offered `function` in the same position a real one
/// occupies, and minted an item called `must`. A statement begins after a `;`, a `{`, a `}`, a line
/// break, or one of the modifiers that may precede it; prose does not.
fn starts_statement(masked: &[char], at: usize) -> bool {
    let mut end = at;
    // Spaces and tabs only: a newline is itself an answer, and consuming it would lose it.
    while end > 0 && (masked[end - 1] == ' ' || masked[end - 1] == '\t') {
        end -= 1;
    }
    if end == 0 {
        return true;
    }
    match masked[end - 1] {
        '\n' | '\r' | ';' | '{' | '}' => return true,
        c if c.is_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    let mut start = end;
    while start > 0 && (masked[start - 1].is_alphanumeric() || masked[start - 1] == '_') {
        start -= 1;
    }
    let word: String = masked[start..end].iter().collect();
    TS_MODIFIERS.contains(&word.as_str())
}

/// Whether what follows a name is what follows a declaration's name.
///
/// The second half of the defence above, and the half that catches prose starting a line. A
/// declared name is followed by `(`, `<`, `=`, `{`, `:`, `;` or `,` — or by `extends` /
/// `implements`, which is how a class names its parent. In `function must return a value` the word
/// after the name is another word, and no declaration reads like that.
fn looks_declared(masked: &[char], name_end: usize) -> bool {
    let at = skip_space(masked, name_end);
    match masked.get(at) {
        Some('(') | Some('<') | Some('=') | Some('{') | Some(':') | Some(';') | Some(',') => true,
        Some(_) => matches!(
            ident_at(masked, at).as_ref().map(|(word, _)| word.as_str()),
            Some("extends") | Some("implements")
        ),
        None => false,
    }
}

/// The name a TypeScript declaration gives, and the kind it settles on.
///
/// `const enum Colour` is the pair that matters here: the last keyword decides, exactly as it does
/// in Rust, and taking the word after the first one produced a constant called `enum`.
fn ts_declared(
    masked: &[char],
    mut at: usize,
    mut kind: ItemKind,
) -> Option<(ItemKind, String, usize)> {
    for _ in 0..4 {
        at = skip_space(masked, at);
        let (word, end) = ident_at(masked, at)?;
        if let Some((_, next)) = TS_KEYWORDS.iter().find(|(keyword, _)| *keyword == word) {
            kind = *next;
            at = end;
            continue;
        }
        if TS_MODIFIERS.contains(&word.as_str()) {
            at = end;
            continue;
        }
        return Some((kind, word, end));
    }
    None
}

/// The keywords that open a TypeScript declaration, and what kind each one makes.
///
/// `const` is [`ItemKind::Constant`] here and is corrected to [`ItemKind::Function`] once the
/// right-hand side is known, because in this shell most functions are `const f = () => {}` and
/// filing them all as constants would put the whole of `shell/src` in the wrong shape.
const TS_KEYWORDS: &[(&str, ItemKind)] = &[
    ("function", ItemKind::Function),
    ("class", ItemKind::Shape),
    ("interface", ItemKind::Shape),
    ("type", ItemKind::Shape),
    ("enum", ItemKind::Shape),
    ("const", ItemKind::Constant),
    ("let", ItemKind::Constant),
    ("var", ItemKind::Constant),
];

/// What a TypeScript file declares, at the top level and one level inside every `class`.
///
/// **Only at brace depth zero, which is what makes a `const` a declaration rather than a local.**
/// `const rows = ...` inside a component is a variable; the identical text at the top of the file
/// is a module constant. Nothing separates them but where they sit, so depth is tracked rather
/// than inferred, and every local skipped that way is counted into [`Items::missed`].
fn ts_items(source: &str) -> (Vec<Found>, Vec<String>) {
    let original: Vec<char> = source.chars().collect();
    let masked = mask(source, false);
    let mut found: Vec<Found> = Vec::new();
    let mut locals = 0usize;
    let mut depth = 0i32;
    let mut i = 0;

    while i < masked.len() {
        match masked[i] {
            '{' => {
                depth += 1;
                i += 1;
                continue;
            }
            '}' => {
                depth -= 1;
                i += 1;
                continue;
            }
            _ => {}
        }
        if !word_start(&masked, i) {
            i += 1;
            continue;
        }
        let Some((word, after)) = ident_at(&masked, i) else {
            i += 1;
            continue;
        };
        let Some(kind) = TS_KEYWORDS
            .iter()
            .find(|(keyword, _)| *keyword == word)
            .map(|(_, kind)| *kind)
        else {
            i = after;
            continue;
        };
        // Prose is not a declaration, and in a `.tsx` file it sits in the same place one does.
        if !starts_statement(&masked, i) {
            i = after;
            continue;
        }
        // `const enum Colour` is a shape called `Colour`, not a constant called `enum`.
        let Some((kind, name, name_end)) = ts_declared(&masked, after, kind) else {
            i = after;
            continue;
        };
        if !looks_declared(&masked, name_end) {
            i = after;
            continue;
        }
        if depth != 0 {
            locals += 1;
            i = name_end;
            continue;
        }

        let body = ts_body(&masked, name_end);
        // `export const x = () => {}` is a function by every reading that matters, and the arrow
        // is the only thing that says so.
        let kind = if kind == ItemKind::Constant && is_arrow(&masked, name_end) {
            ItemKind::Function
        } else {
            kind
        };
        let exported = exported_before(&masked, i, "export") || after_export(&masked, i);

        if word == "class"
            && let Some((open, close)) = body
        {
            found.extend(ts_methods(&original, &masked, open, close, &name));
        }

        found.push(Found {
            item: Item {
                id: name.clone(),
                name,
                container: None,
                kind,
                exported,
                documented: documented_above(&original, i),
                line: line_at(&masked, i),
            },
            body,
        });
        // Deliberately not past the body: what is inside it is where the locals are, and the
        // count of those is the only thing standing between this picture and a file that looks
        // simpler than it is.
        i = name_end;
    }

    finish(
        found,
        locals,
        "declaration local to a function body",
        "declarations local to a function body",
    )
}

/// The methods directly inside one `class` body.
///
/// **A method has no keyword**, which is the whole difficulty: it is a name, a parameter list and
/// a body — and so is a call. The two things that separate them are that a method sits at the
/// class's own brace depth, and that `if (…) {` and its family are keywords rather than names.
fn ts_methods(
    original: &[char],
    masked: &[char],
    open: usize,
    close: usize,
    container: &str,
) -> Vec<Found> {
    const NOT_A_METHOD: &[&str] = &[
        "if", "for", "while", "switch", "catch", "return", "typeof", "new", "await", "do", "else",
    ];
    let mut found = Vec::new();
    let mut depth = 0i32;
    let mut i = open;

    while i < close && i < masked.len() {
        match masked[i] {
            '{' => {
                depth += 1;
                i += 1;
                continue;
            }
            '}' => {
                depth -= 1;
                i += 1;
                continue;
            }
            _ => {}
        }
        if depth != 1 || !word_start(masked, i) {
            i += 1;
            continue;
        }
        let Some((name, name_end)) = ident_at(masked, i) else {
            i += 1;
            continue;
        };
        if NOT_A_METHOD.contains(&name.as_str()) {
            i = name_end;
            continue;
        }
        let after_name = skip_space(masked, name_end);
        if masked.get(after_name) != Some(&'(') {
            i = name_end;
            continue;
        }
        let Some((body_open, body_close)) = ts_body(masked, name_end) else {
            i = name_end;
            continue;
        };
        if body_close > close {
            i = name_end;
            continue;
        }
        found.push(Found {
            item: Item {
                id: format!("{container}.{name}"),
                name,
                container: Some(container.to_string()),
                kind: ItemKind::Method,
                exported: false,
                documented: documented_above(original, i),
                line: line_at(masked, i),
            },
            body: Some((body_open, body_close)),
        });
        i = body_close + 1;
    }
    found
}

/// Whether what follows a TypeScript declaration's name is an arrow function.
///
/// Looks past a type annotation and a parameter list for the `=>` that makes it one, and stops at
/// the `;` or the `{` that ends the statement — so an arrow later in the file cannot reach back
/// and reclassify a plain constant.
fn is_arrow(masked: &[char], from: usize) -> bool {
    let mut i = from;
    let mut paren = 0i32;
    while i < masked.len() {
        match masked[i] {
            '(' => paren += 1,
            ')' => paren -= 1,
            '=' if masked.get(i + 1) == Some(&'>') && paren <= 0 => return true,
            ';' if paren <= 0 => return false,
            '{' if paren <= 0 => return false,
            _ => {}
        }
        i += 1;
    }
    false
}

/// The body of a TypeScript declaration: the `{` that opens it, or nothing.
///
/// Unlike Rust's, an `=` can stand between the name and the body — `const f = () => { … }` — and a
/// `;` before any `{` means there is no body at all.
fn ts_body(masked: &[char], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    let (mut paren, mut square) = (0i32, 0i32);
    while i < masked.len() {
        match masked[i] {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => square += 1,
            ']' => square -= 1,
            ';' if paren <= 0 && square <= 0 => return None,
            '{' if paren <= 0 && square <= 0 => {
                let close = match_brace(masked, i)?;
                return Some((i, close));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether `export` opens the statement this declaration is part of.
///
/// `export default function foo` and `export async function foo` both put words between the two,
/// which [`exported_before`] alone would miss. Walks back over the words allowed to sit there and
/// stops at anything else.
fn after_export(masked: &[char], at: usize) -> bool {
    const BETWEEN: &[&str] = &["default", "async", "abstract", "declare"];
    let mut end = at;
    for _ in 0..4 {
        while end > 0 && masked[end - 1].is_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && (masked[start - 1].is_alphanumeric() || masked[start - 1] == '_') {
            start -= 1;
        }
        let word: String = masked[start..end].iter().collect();
        if word == "export" {
            return true;
        }
        if !BETWEEN.contains(&word.as_str()) {
            return false;
        }
        end = start;
    }
    false
}

/// Sort what was found, drop the duplicates, and say what was left out.
///
/// **Sorted by line and never by name**, because the order a file declares things in is a fact
/// about the file, and it is the order the owner will read the list in when they go looking for
/// the box they just clicked.
///
/// A duplicate id is possible and is not an error: two `impl` blocks for one type can each define
/// `fmt`, and Rust allows it when they implement different traits. The first wins, and the loss is
/// reported rather than silently merged.
fn finish(
    mut found: Vec<Found>,
    skipped: usize,
    one: &str,
    many: &str,
) -> (Vec<Found>, Vec<String>) {
    found.sort_by_key(|entry| entry.item.line);

    let mut seen = std::collections::BTreeSet::new();
    let mut collisions = 0usize;
    found.retain(|entry| {
        if seen.insert(entry.item.id.clone()) {
            true
        } else {
            collisions += 1;
            false
        }
    });

    let mut missed = Vec::new();
    if skipped > 0 {
        let noun = if skipped == 1 { one } else { many };
        missed.push(format!("{skipped} {noun} not drawn"));
    }
    if collisions > 0 {
        missed.push(format!(
            "{collisions} declaration{} share a name with one already drawn",
            if collisions == 1 { "" } else { "s" }
        ));
    }
    (found, missed)
}

/// Which declarations name which others, inside the one file that holds them all.
///
/// **A name inside a body, and nothing cleverer.** This is the same trade
/// [`crate::project_map::rust_imports`] makes one level up, and it fails the same way: a local
/// variable that happens to share a name with a function of this file draws an edge that is not
/// there. What buys that back is the scope — both ends are in the same file, so an invented edge
/// joins two things the reader can see at once and check, rather than two modules they cannot.
///
/// **A method is reached through `self`, `Self` or its type, and never by its bare name.** Inside
/// `Foo::a`, a bare `b` is whatever `b` is at the top level; `self.b()` is `Foo::b`. Without that
/// distinction every type with a method called `new` would collect an edge from every function
/// that constructs anything at all.
///
/// A declaration never references itself. Recursion is real and is a property of one box, not a
/// line from a box to itself — and a self-loop is the one edge a layered drawing cannot place.
fn references(source: &str, found: &[Found], rust: bool) -> Vec<Reference> {
    let masked = mask(source, rust);
    let mut by_name: std::collections::BTreeMap<&str, Vec<&Item>> =
        std::collections::BTreeMap::new();
    for entry in found {
        by_name
            .entry(&entry.item.name)
            .or_default()
            .push(&entry.item);
    }

    let mut edges = std::collections::BTreeSet::new();
    for entry in found {
        let Some((open, close)) = entry.body else {
            continue;
        };
        let from = &entry.item;
        let mut i = open + 1;
        while i < close && i < masked.len() {
            if !word_start(&masked, i) {
                i += 1;
                continue;
            }
            let Some((word, after)) = ident_at(&masked, i) else {
                i += 1;
                continue;
            };
            let owner = qualifier(&masked, i, from.container.as_deref());
            if let Some(target) = resolve(&by_name, &word, owner.as_deref())
                && target.id != from.id
            {
                edges.insert(Reference {
                    from: from.id.clone(),
                    to: target.id.clone(),
                });
            }
            i = after;
        }
    }
    edges.into_iter().collect()
}

/// What sits before the name at `at` as a qualifier: the type of a `Foo::bar` or `foo.bar`.
///
/// `self` and `Self` resolve to the container of whatever is being scanned, which is what makes
/// `self.helper()` inside `Foo::run` an edge to `Foo::helper` rather than to a free `helper`.
fn qualifier(masked: &[char], at: usize, container: Option<&str>) -> Option<String> {
    let mut end = at;
    // Step back over the `::` or `.` that would make this a qualified name.
    if end >= 2 && masked[end - 1] == ':' && masked[end - 2] == ':' {
        end -= 2;
    } else if end >= 1 && masked[end - 1] == '.' {
        end -= 1;
    } else {
        return None;
    }
    let mut start = end;
    while start > 0 && (masked[start - 1].is_alphanumeric() || masked[start - 1] == '_') {
        start -= 1;
    }
    let word: String = masked[start..end].iter().collect();
    if word.is_empty() {
        return None;
    }
    if word == "self" || word == "Self" || word == "this" {
        return container.map(str::to_string);
    }
    Some(word)
}

/// The declaration a name refers to, given what qualified it.
///
/// A qualified name matches only a member of that container; an unqualified one matches only a
/// top-level declaration. That is the rule that keeps `Foo::new` and `Bar::new` apart, and it is
/// why an unqualified `new` matches neither of them rather than guessing.
fn resolve<'a>(
    by_name: &std::collections::BTreeMap<&str, Vec<&'a Item>>,
    name: &str,
    owner: Option<&str>,
) -> Option<&'a Item> {
    let candidates = by_name.get(name)?;
    match owner {
        Some(container) => candidates
            .iter()
            .find(|item| item.container.as_deref() == Some(container))
            .copied(),
        None => {
            let mut top = candidates.iter().filter(|item| item.container.is_none());
            let first = top.next()?;
            // Two top-level declarations with one name cannot both be meant, and picking either
            // would be a coin toss drawn as a fact.
            if top.next().is_some() {
                None
            } else {
                Some(*first)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(found: &Items) -> Vec<&str> {
        found.items.iter().map(|item| item.id.as_str()).collect()
    }

    fn edges(found: &Items) -> Vec<String> {
        found
            .references
            .iter()
            .map(|edge| format!("{} -> {}", edge.from, edge.to))
            .collect()
    }

    fn item<'a>(found: &'a Items, id: &str) -> &'a Item {
        found
            .items
            .iter()
            .find(|item| item.id == id)
            .unwrap_or_else(|| panic!("no item {id} in {:?}", ids(found)))
    }

    #[test]
    fn a_top_level_function_is_an_item_with_its_visibility_and_its_doc() {
        let found = items(
            "core/src/a.rs",
            "/// What it does.\npub fn open() {}\n\nfn shut() {}\n",
        );
        assert_eq!(ids(&found), vec!["open", "shut"]);
        assert!(item(&found, "open").exported);
        assert!(item(&found, "open").documented);
        assert!(!item(&found, "shut").exported);
        assert!(!item(&found, "shut").documented);
    }

    #[test]
    fn the_line_is_the_declarations_own_and_not_its_docs() {
        let found = items("core/src/a.rs", "\n/// One.\n/// Two.\nfn here() {}\n");
        assert_eq!(item(&found, "here").line, 4);
    }

    #[test]
    fn an_attribute_between_a_doc_and_its_item_does_not_detach_it() {
        let found = items(
            "core/src/a.rs",
            "/// Documented.\n#[derive(Debug)]\n#[serde(rename_all = \"snake_case\")]\nstruct Kept;\n",
        );
        assert!(item(&found, "Kept").documented);
    }

    #[test]
    fn a_blank_line_between_a_comment_and_an_item_detaches_it() {
        let found = items("core/src/a.rs", "/// Not about it.\n\nfn alone() {}\n");
        assert!(!item(&found, "alone").documented);
    }

    #[test]
    fn a_method_is_named_for_the_type_it_belongs_to() {
        let found = items(
            "core/src/a.rs",
            "struct Door;\n\nimpl Door {\n    pub fn open(&self) {}\n    fn shut(&self) {}\n}\n",
        );
        assert_eq!(ids(&found), vec!["Door", "Door::open", "Door::shut"]);
        assert_eq!(item(&found, "Door::open").name, "open");
        assert_eq!(
            item(&found, "Door::open").container.as_deref(),
            Some("Door")
        );
        assert_eq!(item(&found, "Door::open").kind, ItemKind::Method);
    }

    #[test]
    fn a_trait_impl_files_its_methods_under_the_type_and_never_the_trait() {
        let found = items(
            "core/src/a.rs",
            "struct Door;\n\nimpl std::fmt::Display for Door {\n    fn fmt(&self) {}\n}\n",
        );
        assert!(ids(&found).contains(&"Door::fmt"));
        assert!(!ids(&found).contains(&"Display::fmt"));
    }

    #[test]
    fn a_generic_impl_is_still_the_type_it_is_for() {
        let found = items(
            "core/src/a.rs",
            "impl<T: Clone> crate::holder::Holder<T> {\n    fn get(&self) {}\n}\n",
        );
        assert_eq!(ids(&found), vec!["Holder::get"]);
    }

    #[test]
    fn two_types_may_each_have_a_new_and_they_stay_apart() {
        let found = items(
            "core/src/a.rs",
            "impl A {\n    fn new() {}\n}\nimpl B {\n    fn new() {}\n}\n",
        );
        assert_eq!(ids(&found), vec!["A::new", "B::new"]);
    }

    #[test]
    fn a_function_inside_a_function_is_not_drawn_and_is_counted() {
        let found = items(
            "core/src/a.rs",
            "impl A {\n    fn outer() {\n        fn inner() {}\n    }\n}\n",
        );
        assert_eq!(ids(&found), vec!["A::outer"]);
        assert_eq!(found.missed, vec!["1 nested function not drawn"]);
    }

    #[test]
    fn a_mod_naming_a_file_is_the_level_above_and_a_mod_with_a_body_is_here() {
        let found = items(
            "core/src/a.rs",
            "mod elsewhere;\n\nmod here {\n    fn deep() {}\n}\n",
        );
        assert_eq!(ids(&found), vec!["here"]);
        assert_eq!(item(&found, "here").kind, ItemKind::Module);
    }

    #[test]
    fn a_keyword_inside_a_string_or_a_comment_is_not_an_item() {
        let found = items(
            "core/src/a.rs",
            "// fn commented() {}\nconst SQL: &str = \"fn quoted() {}\";\n/* fn blocked() {} */\nfn real() {}\n",
        );
        assert_eq!(ids(&found), vec!["SQL", "real"]);
    }

    #[test]
    fn a_lifetime_does_not_open_a_string_and_swallow_the_rest_of_the_file() {
        let found = items(
            "core/src/a.rs",
            "fn borrow<'a>(text: &'a str) -> &'a str {\n    text\n}\n\nfn after() {}\n",
        );
        assert_eq!(ids(&found), vec!["borrow", "after"]);
    }

    #[test]
    fn a_raw_string_is_text_all_the_way_through() {
        let found = items(
            "core/src/a.rs",
            "const Q: &str = r#\"fn hidden() {} \"# ;\nfn seen() {}\n",
        );
        assert_eq!(ids(&found), vec!["Q", "seen"]);
    }

    #[test]
    fn one_declaration_calling_another_is_an_edge() {
        let found = items(
            "core/src/a.rs",
            "fn helper() {}\n\nfn caller() {\n    helper();\n}\n",
        );
        assert_eq!(edges(&found), vec!["caller -> helper"]);
    }

    #[test]
    fn a_method_reached_through_self_belongs_to_its_own_type() {
        let found = items(
            "core/src/a.rs",
            "fn helper() {}\n\nimpl Door {\n    fn helper(&self) {}\n    fn run(&self) {\n        self.helper();\n    }\n}\n",
        );
        assert!(edges(&found).contains(&"Door::run -> Door::helper".to_string()));
        assert!(!edges(&found).contains(&"Door::run -> helper".to_string()));
    }

    #[test]
    fn recursion_is_a_property_of_one_box_and_never_a_line_to_itself() {
        let found = items(
            "core/src/a.rs",
            "fn walk(depth: u32) {\n    if depth > 0 {\n        walk(depth - 1);\n    }\n}\n",
        );
        assert!(edges(&found).is_empty());
    }

    #[test]
    fn an_unqualified_name_that_two_declarations_answer_to_draws_neither() {
        let found = items(
            "core/src/a.rs",
            "impl A {\n    fn go() {}\n}\nimpl B {\n    fn go() {}\n}\nfn caller() {\n    go();\n}\n",
        );
        assert!(edges(&found).is_empty());
    }

    #[test]
    fn a_typescript_export_is_an_item_and_an_arrow_is_a_function() {
        let found = items(
            "shell/src/a.ts",
            "export const shout = (word: string) => word;\nexport const LIMIT = 4;\nfunction quiet() {}\n",
        );
        assert_eq!(ids(&found), vec!["shout", "LIMIT", "quiet"]);
        assert_eq!(item(&found, "shout").kind, ItemKind::Function);
        assert_eq!(item(&found, "LIMIT").kind, ItemKind::Constant);
        assert!(item(&found, "shout").exported);
        assert!(!item(&found, "quiet").exported);
    }

    #[test]
    fn a_single_quoted_typescript_string_is_a_string_and_not_a_lifetime() {
        // The mask asks whether a quote opens a lifetime, and in Rust it sometimes does. Asked of
        // TypeScript the answer was yes for every string beginning with a letter, so the contents
        // were never blanked and a keyword inside one became a box. A declaration that does not
        // exist, drawn exactly like one that does, is the failure this whole map refuses.
        let found = items(
            "shell/src/a.ts",
            "const SQL = 'function ghost() {}';\nexport function real() {}\n",
        );
        assert_eq!(ids(&found), vec!["SQL", "real"]);
    }

    #[test]
    fn a_typescript_shape_is_a_shape_whichever_word_declares_it() {
        let found = items(
            "shell/src/a.ts",
            "export interface Row {\n  id: string;\n}\nexport type Key = string;\n",
        );
        assert_eq!(item(&found, "Row").kind, ItemKind::Shape);
        assert_eq!(item(&found, "Key").kind, ItemKind::Shape);
    }

    #[test]
    fn a_local_is_not_a_declaration_and_the_count_says_so() {
        let found = items(
            "shell/src/a.ts",
            "export function draw() {\n  const rows = 3;\n  const cols = 4;\n  return rows * cols;\n}\n",
        );
        assert_eq!(ids(&found), vec!["draw"]);
        assert_eq!(
            found.missed,
            vec!["2 declarations local to a function body not drawn"]
        );
    }

    #[test]
    fn export_default_and_export_async_are_still_exports() {
        let found = items(
            "shell/src/a.ts",
            "export default function main() {}\nexport async function later() {}\n",
        );
        assert!(item(&found, "main").exported);
        assert!(item(&found, "later").exported);
    }

    #[test]
    fn a_class_method_is_named_for_its_class() {
        let found = items(
            "shell/src/a.ts",
            "export class Gate {\n  open() {\n    return 1;\n  }\n}\n",
        );
        assert!(ids(&found).contains(&"Gate.open"));
        assert_eq!(item(&found, "Gate.open").kind, ItemKind::Method);
    }

    #[test]
    fn a_modifier_is_never_the_name_of_the_thing_it_modifies() {
        // `const` opens a declaration and `const fn` opens a different one. Taking the word after
        // the keyword as the name gave an item literally called `fn`, lost `zero` entirely, and
        // then drew an edge to that phantom from every body containing the word.
        let found = items(
            "core/src/a.rs",
            "pub const fn zero() -> u8 {\n    0\n}\n\nstatic mut COUNT: u32 = 0;\n",
        );
        assert_eq!(ids(&found), vec!["zero", "COUNT"]);
        assert_eq!(item(&found, "zero").kind, ItemKind::Function);
        assert!(item(&found, "zero").exported);
        assert_eq!(item(&found, "COUNT").kind, ItemKind::Constant);
    }

    #[test]
    fn a_typescript_const_enum_is_the_shape_it_names() {
        let found = items("shell/src/a.ts", "export const enum Colour {\n  Red,\n}\n");
        assert_eq!(ids(&found), vec!["Colour"]);
        assert_eq!(item(&found, "Colour").kind, ItemKind::Shape);
        assert!(item(&found, "Colour").exported);
    }

    #[test]
    fn an_apostrophe_in_prose_does_not_swallow_the_rest_of_the_file() {
        // A quote in JSX text opened a string that never closed, so everything after it was
        // blanked and every later declaration vanished — with `missed` empty, because nothing
        // noticed. Silent loss is the one failure this module's own header says it must not have.
        // In TypeScript a quoted string cannot contain a newline, and that is the whole fix.
        let found = items(
            "shell/src/a.tsx",
            "export function Title() {\n  return <h1>What's new</h1>;\n}\nexport function Footer() {}\n",
        );
        assert_eq!(ids(&found), vec!["Title", "Footer"]);
    }

    #[test]
    fn prose_that_happens_to_contain_a_keyword_is_not_a_declaration() {
        // `function` inside JSX text sits at brace depth zero exactly as a real declaration does.
        // What separates them is that a declaration begins a statement and prose does not.
        let found = items(
            "shell/src/a.tsx",
            "export const Note = () => <p>Every function must return a value</p>;\n",
        );
        assert_eq!(ids(&found), vec!["Note"]);
    }

    #[test]
    fn an_impl_for_a_reference_still_belongs_to_the_type_it_is_for() {
        // `take_while` stopped on the `&` and produced no target at all, so the whole block was
        // dropped — no items, and nothing said so.
        let found = items(
            "core/src/a.rs",
            "impl std::fmt::Display for &Foo {\n    fn fmt(&self) {}\n}\n",
        );
        assert_eq!(ids(&found), vec!["Foo::fmt"]);
    }

    #[test]
    fn a_bound_that_returns_does_not_become_the_container() {
        // The `>` of `-> u8` closed the generic list early, leaving `u8` as the self type.
        let found = items(
            "core/src/a.rs",
            "impl<F: Fn(u8) -> u8> Runner<F> {\n    fn run(&self) {}\n}\n",
        );
        assert_eq!(ids(&found), vec!["Runner::run"]);
    }

    #[test]
    fn a_traits_own_methods_are_items_the_way_an_impls_are() {
        // Three doc comments promised this and only `impl` delivered it. A trait's methods were
        // counted as "nested functions", which they are not.
        let found = items(
            "core/src/a.rs",
            "pub trait Door {\n    fn open(&self);\n    fn shut(&self) {}\n}\n",
        );
        assert_eq!(ids(&found), vec!["Door", "Door::open", "Door::shut"]);
        assert_eq!(item(&found, "Door::open").kind, ItemKind::Method);
        assert!(found.missed.is_empty());
    }

    #[test]
    fn a_wrapped_attribute_does_not_detach_a_doc_comment() {
        // rustfmt breaks any long `#[derive(...)]` across lines, and the continuation lines start
        // with neither `#` nor `//`. The walk upwards stopped there and reported a documented item
        // as undocumented — understating the one number this level leads with.
        let found = items(
            "core/src/a.rs",
            "/// Documented.\n#[derive(\n    Debug,\n    Clone,\n)]\npub struct Kept;\n",
        );
        assert!(item(&found, "Kept").documented);
    }

    #[test]
    fn a_function_pointer_in_a_field_is_not_a_function_that_was_hidden() {
        // `missed` is printed beside the drawing, so a number that counts types as functions is a
        // wrong sentence under a right picture.
        let found = items(
            "core/src/a.rs",
            "pub struct Table {\n    render: fn(&str) -> String,\n}\n",
        );
        assert_eq!(ids(&found), vec!["Table"]);
        assert!(found.missed.is_empty());
    }

    #[test]
    fn a_language_nobody_here_reads_comes_back_empty_and_says_who_read_it() {
        let found = items("sidecars/echo/main.go", "func main() {}\n");
        assert_eq!(found.reader, None);
        assert!(found.items.is_empty());
        assert!(found.references.is_empty());
    }

    #[test]
    fn a_file_that_declares_nothing_is_no_items_rather_than_no_answer() {
        let found = items("core/src/a.rs", "use crate::other;\n");
        assert_eq!(found.reader, Some(Reader::Rust));
        assert!(found.items.is_empty());
        assert!(found.missed.is_empty());
    }
}
