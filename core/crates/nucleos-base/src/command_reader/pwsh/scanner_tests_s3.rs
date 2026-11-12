//! S3 RED: sub-expressions, parentheses, `@( )`, script blocks, hashtables, expressions vs commands.
//! Values fixed against the pwsh 7 parser.

use super::scanner_tests_common::{err, ok};

#[test]
fn s3_subexpression() {
    ok("$(a; b)", &["a", "b"]);
    ok("$(a)", &["a"]);
    ok("$(a | b; c)", &["a", "b", "c"]);
    ok("$(a) | b", &["a", "b"]);
    ok("$(a),$(b)", &["a", "b"]);
}

#[test]
fn s3_parentheses() {
    ok("(a | b)", &["a", "b"]);
    ok("(a) | b", &["a", "b"]);
    ok("((a))", &["a"]);
    ok("(a),(b)", &["a", "b"]);
    ok("(a) -and (b)", &["a", "b"]);
    ok("(a) + (b)", &["a", "b"]);
    ok("-not (a)", &["a"]);
    ok("!(a)", &["a"]);
}

#[test]
fn s3_semicolon_inside_parentheses_is_a_parse_error() {
    err("(a; b)");
    err("echo (a; b)");
}

#[test]
fn s3_array_subexpression() {
    ok("@(a)", &["a"]);
    ok("@(a; b)", &["a", "b"]);
    ok("echo @(a; b)", &["echo", "a", "b"]);
    ok("$x = @(a)", &["a"]);
    // A comma is an expression operator, not a command separator.
    ok("@(1,2)", &[]);
    err("@(a, b)");
}

#[test]
fn s3_foreach_object_script_block() {
    ok("1..5 | ForEach-Object { $_ * 2 }", &["ForEach-Object"]);
    ok("1..5|ForEach-Object{$_*2}", &["ForEach-Object"]);
    ok("a | % { b | c }", &["a", "%", "b", "c"]);
    ok("Where-Object { $_.x -gt 1 }", &["Where-Object"]);
}

#[test]
fn s3_call_and_dot_operators_with_blocks() {
    // The first element of `& { a }` is the script-block expression itself.
    ok("& { a }", &["{ a }", "a"]);
    ok(". { a }", &["{ a }", "a"]);
    ok("& (gcm x) arg", &["(gcm x)", "gcm"]);
    ok("& $cmd arg", &["$cmd"]);
}

#[test]
fn s3_bare_script_block() {
    ok("{ a }", &["a"]);
    ok("{ a } | b", &["a", "b"]);
    ok("echo {a}", &["echo", "a"]);
    ok("a ({b})", &["a", "b"]);
}

#[test]
fn s3_hashtable() {
    ok("@{a=1; b=(c)}", &["c"]);
    ok("@{a=1}", &[]);
    ok("@{a=1; b=2}", &[]);
    ok("@{a=$(b)}", &["b"]);
    ok("@{a=(b | c); d=(e)}", &["b", "c", "e"]);
    ok("@{a={ b }}", &["b"]);
    ok("@{ a = ( b ) }", &["b"]);
    ok("echo @{a=1}", &["echo"]);
    err("@{a=1");
}

#[test]
fn s3_unbalanced_delimiters_are_err() {
    err("(a");
    err("a)");
    err("{ a");
    err("a }");
    err("$(a");
}

#[test]
fn s3_depth_limit_does_not_overflow_the_stack() {
    err(&"(".repeat(10_000));
    err(&"$(".repeat(10_000));
    err(&"{".repeat(10_000));
}

#[test]
fn s3_expressions_are_not_commands() {
    ok("( 1 + 2 )", &[]);
    ok("$x.Method(1)", &[]);
    ok("[int]'3'", &[]);
    ok("$a -eq $b", &[]);
    ok("\"x\" -replace 'a','b'", &[]);
    ok("$h['k']", &[]);
    ok("$x[0]", &[]);
    ok("$h.x.y", &[]);
    ok("$x | a", &["a"]);
    ok("1 | a", &["a"]);
}

#[test]
fn s3_bareword_in_method_arguments_is_a_parse_error() {
    err("$x.Method(a)");
    ok("$x.Method($(a))", &["a"]);
}

#[test]
fn s3_expressions_around_commands() {
    ok("$a -eq (b)", &["b"]);
    ok("[int](a)", &["a"]);
    ok("$x[(a)]", &["a"]);
    ok("($x = a)", &["a"]);
}

#[test]
fn s3_sub_blocks_inside_arguments() {
    ok("echo $(a)", &["echo", "a"]);
    ok("echo $(a) $(b | c)", &["echo", "a", "b", "c"]);
    ok("echo (a)", &["echo", "a"]);
    ok("echo ((a))", &["echo", "a"]);
    ok("echo @(a)", &["echo", "a"]);
    ok("a -b (c)", &["a", "c"]);
    ok("a -b $(c) -d", &["a", "c"]);
    ok("a -b { c } -d", &["a", "c"]);
    ok("a -b @(c)", &["a", "c"]);
    ok("a (b) c", &["a", "b"]);
    ok("a -x:(b)", &["a", "b"]);
}

#[test]
fn s3_arguments_that_are_not_commands() {
    ok("echo -f x", &["echo"]);
    ok("echo a -f b", &["echo"]);
    ok("echo a,b", &["echo"]);
    ok("echo $x.y", &["echo"]);
    ok("echo $env:PATH", &["echo"]);
    ok("echo a.b", &["echo"]);
    ok("echo [a]", &["echo"]);
}

#[test]
fn s3_argument_followed_by_group_is_a_parse_error() {
    err("(a) b");
    err("(a)(b)");
    err("{a}b");
}
