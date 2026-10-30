//! S1 RED: skeleton, words, strings, comments. Expected values fixed against
//! `System.Management.Automation.Language.Parser` (pwsh 7): `CommandElements[0].Extent.Text`.

use super::scanner_tests_common::{err, ok};

#[test]
fn s1_bare_words() {
    ok("Get-ChildItem", &["Get-ChildItem"]);
    ok("git status", &["git"]);
    ok("a -b -c d", &["a"]);
    ok(r".\tool.exe x", &[r".\tool.exe"]);
    ok(r"C:\Tools\x.exe y", &[r"C:\Tools\x.exe"]);
}

#[test]
fn s1_call_operator_target_keeps_its_quotes() {
    ok(r"& 'C:\x\tool.exe' a", &[r"'C:\x\tool.exe'"]);
    ok("& 'git' status", &["'git'"]);
    ok("& \"git\" status", &["\"git\""]);
}

#[test]
fn s1_quoted_text_at_statement_start_is_a_parse_error() {
    // The parser reads `'git'` as an expression, then chokes on `status`.
    err("'git' status");
}

#[test]
fn s1_single_quoted_strings() {
    ok("echo 'a ''b'''", &["echo"]);
    ok("echo 'a;b'", &["echo"]);
    err("echo 'abc");
}

#[test]
fn s1_double_quoted_strings() {
    ok("echo \"a `\" b\"", &["echo"]);
    ok("echo \"a \"\" b\"", &["echo"]);
    ok("echo \"a|b\"", &["echo"]);
    ok("echo \"a\nb\"", &["echo"]);
    err("echo \"abc");
}

#[test]
fn s1_subexpression_inside_double_quotes() {
    ok("echo \"x $(Get-Date) y\"", &["echo", "Get-Date"]);
    ok("echo \"x $(a; b) $(c)\"", &["echo", "a", "b", "c"]);
    ok("echo \"a$(b)c\" d", &["echo", "b"]);
    ok("echo \"a $(b \"c $(d)\")\"", &["echo", "b", "d"]);
    ok("echo \"`$(a)\"", &["echo"]);
    ok("echo '$(a)'", &["echo"]);
    err("echo \"x $(a\"");
}

#[test]
fn s1_single_quoted_here_string() {
    ok("echo @'\nx $(a) '@\n'@\nb", &["echo", "b"]);
    ok("echo @'\na\r\n'@\r\nb", &["echo", "b"]);
    ok("echo @'\n'@", &["echo"]);
    ok("echo @'\n'@\nb", &["echo", "b"]);
}

#[test]
fn s1_here_string_terminator_only_at_line_start() {
    ok("echo @'\na '@ b\n'@", &["echo"]);
    ok("echo @\"\na\n \"@ x\n\"@\nb", &["echo", "b"]);
}

#[test]
fn s1_expandable_here_string() {
    ok(
        "echo @\"\nx $(Get-Date) y\n\"@\nb",
        &["echo", "Get-Date", "b"],
    );
    ok("echo @\"\na \"\" b `\" c\n\"@", &["echo"]);
}

#[test]
fn s1_unterminated_here_string_is_err() {
    err("echo @'\nnever closed");
    err("echo @\"\nnever");
}

#[test]
fn s1_line_comment() {
    ok("a # comment\nb", &["a", "b"]);
    ok("echo a #c", &["echo"]);
    ok("echo \"a # b\"", &["echo"]);
}

#[test]
fn s1_hash_inside_a_word_is_not_a_comment() {
    ok("echo a#b", &["echo"]);
}

#[test]
fn s1_block_comment() {
    ok("<# c\nc #> a", &["a"]);
    ok("a <# x #> b", &["a"]);
    ok("<# a\nb #>\nc", &["c"]);
    // Block comments do not nest: the first `#>` closes it.
    ok("<# a <# b #> c", &["c"]);
}

#[test]
fn s1_unterminated_block_comment_is_err() {
    err("<# never");
}

#[test]
fn s1_backtick_continues_and_escapes() {
    ok("a `\nb", &["a"]);
    ok("a\n`\nb", &["a", "b"]);
    ok("echo a`\nb", &["echo"]);
    ok("echo a` b", &["echo"]);
    ok("echo a`;b", &["echo"]);
    ok("echo 'a'`\nb", &["echo"]);
}

#[test]
fn s1_trailing_backtick_is_err() {
    // The parser accepts it; the scanner fails closed on purpose.
    err("echo a`");
}

#[test]
fn s1_empty_blank_and_comment_only_input() {
    ok("", &[]);
    ok("   ", &[]);
    ok("\n", &[]);
    ok("# only comment", &[]);
}

#[test]
fn s1_crlf_and_lone_cr_end_lines() {
    ok("a\r\nb", &["a", "b"]);
    ok("a\rb", &["a", "b"]);
}

#[test]
fn s1_stop_parsing_token_is_err() {
    err("a --% b");
    err("echo --%  x");
}
