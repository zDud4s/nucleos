//! The detectors of `command_reader::parse`: PURE tests of a value the shell would evaluate as
//! code (a substitution beside arithmetic, a variable reference, an indirect or substring
//! `${…}`, a `@P`/`@E`/`@A` transformation, `test`/`[` operands that could carry `-v`) or of a
//! line two readers could disagree on (a bare carriage return, brackets nested past the limit).
//! Each one answers from its text alone and over-refuses on purpose.

use super::{MAX_BRACKET_DEPTH, Quoting, Word};

/// PURE: whether the line holds ANY command substitution (`$(` not immediately followed by `(`, or
/// a backtick) AND ANY arithmetic context (`((` anywhere, `$[`, a `${` expansion holding `[` or `:`
/// before its closing `}`, or a shell-name character immediately followed by `[`). Arithmetic
/// evaluates a variable's value as code, and that evaluation runs a `$(…)` in an array subscript,
/// so a value a substitution produced must not reach one. Not keyed on `NAME=`: a quoted
/// `NAME="$(…)"` or a `for v in $(…)` list reaches arithmetic the same way. Coarse on purpose:
/// over-refusal is accepted.
pub(super) fn substitution_beside_arithmetic(command: &str) -> bool {
    let bytes = command.as_bytes();
    let is_name = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let has_substitution = bytes.iter().enumerate().any(|(at, byte)| match byte {
        b'`' => true,
        b'$' => bytes.get(at + 1) == Some(&b'(') && bytes.get(at + 2) != Some(&b'('),
        _ => false,
    });
    if !has_substitution {
        return false;
    }
    if command.contains("((") || command.contains("$[") {
        return true;
    }
    if bytes
        .windows(2)
        .any(|pair| is_name(pair[0]) && pair[1] == b'[')
    {
        return true;
    }
    command.match_indices("${").any(|(at, _)| {
        let inside = &command[at + 2..];
        let inside = inside.split('}').next().unwrap_or(inside);
        inside.contains('[') || inside.contains(':')
    })
}

/// PURE: whether the text holds an ASCII letter or `_` (it names a variable) or a `$` (it expands
/// to something that may, such as the special parameter `$-`, which is `hBc` under `bash -c`).
/// Arithmetic evaluates a name's value recursively, so either one makes the text unreadable.
pub(super) fn holds_a_reference(text: &str) -> bool {
    text.bytes()
        .any(|byte| byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$')
}

/// PURE: whether the operands of `test`/`[` (the program word and a closing `]` already taken off)
/// can be read. A word is dynamic when it expands, or is not quoted yet holds `{`. With no dynamic
/// word the operands are fixed text. With one, the command is readable only in these shapes: one
/// double-quoted operand; a literal unary operator other than `-v`/`-R` and a double-quoted
/// operand; or a literal binary operator in the middle with the other two literal or
/// double-quoted. Anything else could carry the operator (`-v`) through an expansion. An operand
/// that expands and holds `@` (`"$@"`, `"${a[@]}"`) is several operands and fits no shape.
pub(super) fn test_operands_are_readable(operands: &[Word]) -> bool {
    let dynamic = |word: &Word| {
        word.expands
            || (!matches!(word.quoting, Quoting::Double | Quoting::Single)
                && word.text.contains('{'))
    };
    if !operands.iter().any(dynamic) {
        return true;
    }
    // An expansion holding `@` (`"$@"`, `"${a[@]}"`) is several operands, so it fits no shape.
    if operands
        .iter()
        .any(|word| word.expands && word.text.contains('@'))
    {
        return false;
    }
    let double = |word: &Word| word.quoting == Quoting::Double;
    let literal = |word: &Word| !dynamic(word);
    match operands {
        [only] => double(only),
        [operator, operand] => {
            literal(operator)
                && double(operand)
                && operator.text.len() == 2
                && operator.text.starts_with('-')
                && operator.text.as_bytes()[1].is_ascii_alphabetic()
                && !matches!(operator.text.as_str(), "-v" | "-R")
        }
        [left, operator, right] => {
            literal(operator)
                && matches!(
                    operator.text.as_str(),
                    "=" | "=="
                        | "!="
                        | "<"
                        | ">"
                        | "-eq"
                        | "-ne"
                        | "-lt"
                        | "-le"
                        | "-gt"
                        | "-ge"
                        | "-nt"
                        | "-ot"
                        | "-ef"
                )
                && (literal(left) || double(left))
                && (literal(right) || double(right))
        }
        _ => false,
    }
}

/// PURE: whether a `${…}` source makes the shell evaluate a variable's VALUE as arithmetic or as a
/// reference: an indirect expansion (`${!y}`), a subscript that is not only digits, `@` or `*`
/// (`${a[y]}`, `${#a[i]}`), or a substring offset/length naming a variable (`${x:y}`). A `:` right
/// after the name or subscript and followed by `-`, `=`, `+` or `?` is a default/alternate operator,
/// not an offset, and stays readable.
pub(super) fn evaluates_a_variable(source: &str) -> bool {
    let Some(inside) = source.strip_prefix("${") else {
        return false;
    };
    if inside.starts_with('!') {
        return true;
    }
    let inside = inside.strip_suffix('}').unwrap_or(inside);
    let inside = inside.strip_prefix('#').unwrap_or(inside);
    let name_end = inside
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(inside.len());
    // A special parameter (`@`, `*`, `?`, `$`…) is one character.
    let name_end = if name_end == 0 {
        inside.chars().next().map_or(0, char::len_utf8)
    } else {
        name_end
    };
    let mut rest = &inside[name_end..];
    if let Some(after) = rest.strip_prefix('[') {
        let Some(close) = after.find(']') else {
            return true;
        };
        if !after[..close]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'@' || byte == b'*')
        {
            return true;
        }
        rest = &after[close + 1..];
    }
    match rest.strip_prefix(':') {
        Some(offset) if !offset.starts_with(['-', '=', '+', '?']) => holds_a_reference(offset),
        _ => false,
    }
}

/// PURE: whether the line holds a `\r` that is not immediately followed by `\n`.
pub(super) fn has_bare_carriage_return(command: &str) -> bool {
    let bytes = command.as_bytes();
    bytes
        .iter()
        .enumerate()
        .any(|(at, byte)| *byte == b'\r' && bytes.get(at + 1) != Some(&b'\n'))
}

/// Whether the text holds a `@P`, `@E` or `@A` transformation anywhere, nested ones included.
pub(super) fn transforms(source: &str) -> bool {
    ["@P", "@E", "@A"]
        .iter()
        .any(|operator| source.contains(operator))
}

pub(super) fn brackets_nest_too_deeply(command: &str) -> bool {
    let mut depth = 0_usize;
    command.chars().any(|c| {
        match c {
            '(' | '{' => depth += 1,
            ')' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        depth > MAX_BRACKET_DEPTH
    })
}
