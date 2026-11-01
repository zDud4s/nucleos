//! S2 RED: pipelines, chains, separators, redirections. Values fixed against the pwsh 7 parser.

use super::scanner_tests_common::{err, ok};

#[test]
fn s2_pipeline() {
    ok("a | b | c", &["a", "b", "c"]);
    ok("a|b", &["a", "b"]);
    ok("echo a|b", &["echo", "b"]);
}

#[test]
fn s2_semicolon_sequence() {
    ok("a; b", &["a", "b"]);
    ok("a;b", &["a", "b"]);
    ok("echo a;echo b", &["echo", "echo"]);
    ok("echo 'a;b'", &["echo"]);
}

#[test]
fn s2_pipeline_chains() {
    ok("a && b", &["a", "b"]);
    ok("a || b", &["a", "b"]);
    ok("a && b || c", &["a", "b", "c"]);
    ok("a&&b", &["a", "b"]);
    ok("a||b", &["a", "b"]);
    ok("a | b && c", &["a", "b", "c"]);
}

#[test]
fn s2_null_coalescing_and_ternary_are_expressions() {
    ok("$x ?? 'd'", &[]);
    // `$x ? a : b` is a parse error in the oracle.
    err("$x ? a : b");
}

#[test]
fn s2_double_semicolon_is_accepted_by_the_parser() {
    ok("a;;b", &["a", "b"]);
}

#[test]
fn s2_dangling_operators_are_err() {
    err("a |");
    err("a &&");
    err("a ||");
    err("&& b");
    err("a ; | b");
    err("a | ; b");
}

#[test]
fn s2_newline_separates_statements() {
    ok("a\nb", &["a", "b"]);
    ok("a\n\nb", &["a", "b"]);
    ok("a\r\n\r\nb", &["a", "b"]);
}

#[test]
fn s2_trailing_and_lone_semicolon() {
    ok("a;", &["a"]);
    ok(";", &[]);
}

#[test]
fn s2_redirections_are_not_commands() {
    ok("a > out.txt", &["a"]);
    ok("a >> out.txt", &["a"]);
    ok("a *> all.txt", &["a"]);
    ok("a 2>&1", &["a"]);
    ok("a 2>&1 | b", &["a", "b"]);
    ok("a 2> err.txt | b", &["a", "b"]);
}

#[test]
fn s2_input_redirection_is_a_parse_error() {
    err("a < in.txt");
}

#[test]
fn s2_background_operator() {
    ok("a &", &["a"]);
    ok("a & b", &["a", "b"]);
    ok("a | b &", &["a", "b"]);
}
