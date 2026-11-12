//! Regression tests for the differential-fuzz divergences. Each comment names the behaviour of
//! `[System.Management.Automation.Language.Parser]::ParseInput` (pwsh 7) the test mirrors.

use super::scanner_tests_common::{err, ok};

#[test]
fn s1_redirection_glued_to_a_word() {
    // pwsh: `r>e` is one bareword (a command named `r>e`), not `r` plus a redirection.
    err("v'''';r>e");
    err("& ''&&e>\"\"");
    err("a x>f");
}

#[test]
fn s2_backtick_blank_is_whitespace() {
    // pwsh: a backtick followed by a space or tab is skipped like whitespace at a token start.
    ok("t|` t{$_.m()} {} ", &["t", "t"]);
    ok("t'';` ", &["t''"]);
    ok("a `\tb", &["a"]);
}

#[test]
fn a1_stream_merge_forms() {
    // pwsh: `>&`, `>>&`, `1>&2`, `2>&2`, `2>&3` are "Missing file specification" / reserved errors.
    err("a >&2>1");
    err("e \"\">&1");
    err("m >&2");
    err("a 2>>&1");
    err("a 1>&2");
    err("a 2>&2");
    err("a 2>&3");
    // pwsh: only `*>&1` and `2>&1`..`6>&1` merge streams.
    ok("a 2>&1", &["a"]);
    ok("a *>&1", &["a"]);
    ok("a 3>&1", &["a"]);
}

#[test]
fn a1_stream_already_redirected() {
    // pwsh: "The output stream for this command is already redirected".
    err("a >f >g");
    err("a 2>f 2>g");
    // pwsh: different streams may each be redirected once.
    ok("a >f 2>g", &["a"]);
}

#[test]
fn a2_assignment_is_not_a_pipeline_element() {
    // pwsh: `a&&$v=1` and `a|$v=1` are parse errors; an assignment is a statement.
    err("l''&&$s=(t).e;");
    err("(1)|e|$v='';n");
    err("a||$v=1");
    // pwsh: an assignment may start a statement and take a pipeline on its right.
    ok("$v=a|b", &["a", "b"]);
    ok("a;$v=1", &["a"]);
}

#[test]
fn a3_invalid_variable_in_string() {
    // pwsh: `"${"` has no closing brace, `${}` is an empty variable name.
    err("s\"${\"");
    err("a ${}");
    // pwsh: `$:` and `$name:` must be followed by a name char.
    err("u\"$:/\"''");
    err("a \"$a:\"");
    err("a \"$a:/\"");
    // pwsh: `$a:b` is a drive-qualified variable; a lone `$` stays literal.
    ok("a \"$a:b\"", &["a"]);
    ok("a \"$\"", &["a"]);
}

#[test]
fn a4_expression_after_pipe() {
    // pwsh: "Expressions are only allowed as the first element of a pipeline".
    err("{''}|''|t");
    err("e (a\"\"|'')");
    err("''+4|''-'',''");
    err("a|1");
    err("a|(b)");
    // pwsh: after `&&` a new pipeline starts, so an expression is fine there.
    ok("a&&1", &["a"]);
}

#[test]
fn a4_unicode_escape() {
    // pwsh: `u must open `u{hex}` (1-6 hex digits, <= 10FFFF), otherwise the escape is invalid.
    err("switch(x){1{''}2{''}`u{''}}");
    err("a \"`u\"");
    err("a \"`u{110000}\"");
    // pwsh: `u{b} is a valid escape (b is a hex digit).
    ok("a `u{b}", &["a"]);
    ok("a \"`u{1F600}\"", &["a"]);
}
