//! Regression tests for the fuzz counterexamples of round 5. Each comment names the behaviour of
//! `[System.Management.Automation.Language.Parser]::ParseInput` (pwsh 7) the test mirrors.

use super::scanner_tests_common::{err, ok};

#[test]
fn s8_call_target_is_one_word() {
    // pwsh: `& ''g` calls the string `''`; the glued `g` is a separate element. The scanner refuses it.
    err("& ''g");
    err("& ''e");
    err(". e('','')");
    err("& \"\u{201C}e");
    err("& '',");
    ok("& 'C:\\x y\\t.exe' a", &["'C:\\x y\\t.exe'"]);
    ok("& $cmd a", &["$cmd"]);
    ok(". .\\s.ps1", &[".\\s.ps1"]);
}

#[test]
fn s8_dot_forms() {
    // pwsh: a lone `.` is a parse error; `.$x` is dot-sourcing `$x`, not a command named `.$x`.
    err(".");
    err(".\n");
    err(".$");
    err(".$x");
    // pwsh: `.5` is a number, so `.1|h{}` is an expression followed by the command `h`.
    ok(".1|h{}", &["h"]);
    ok(".5 | x", &["x"]);
    // pwsh: `]t` is a command named `]`, not `]t`.
    err("]t");
    err("(1)|]t");
}

#[test]
fn s8_commas() {
    // pwsh: a leading or trailing comma is a missing array element.
    err("o , ");
    err(". ,");
    err("t|t ,");
    err("t ,{h}");
    err("i >,");
    err("t \"$(t ,)\"");
    err("echo a,");
    err("echo a,,b");
    // pwsh: a comma inside a `-parameter` word is an error, unless it is a negative number.
    err("t -t,''");
    err("g --,");
    ok("echo -1,2", &["echo"]);
    // pwsh: a plain list is fine.
    ok("echo a, b", &["echo"]);
    ok("echo a,b", &["echo"]);
    // pwsh: `[,]` is not a type.
    err("[,]''+4");
}

#[test]
fn s8_bare_at_sign() {
    // pwsh: `@-` is not a splat, a quote glued to `@` is an invalid token.
    err("d 2>1 @-");
    err("@-''");
    err("n\"\" @-''");
    err("echo \"\"@");
    ok("ssh user@host", &["ssh"]);
}

#[test]
fn s8_bare_dollar_and_braced_names() {
    // pwsh: `$:` must be followed by a name char; `${{}` and `${:}` are invalid variable names.
    err("d$:\\");
    err("n\"${{}\"''");
    err("echo ${{}");
    err("echo ${:}");
    ok("echo ${env:PATH}", &["echo"]);
}

#[test]
fn s8_backtick_blank_needs_a_blank_before_it() {
    // pwsh: a backtick escaping a blank right after an operand ends the expression.
    err("foreach($f` in t){}");
    err("e{$_-gt0` }");
    ok("a;` b", &["a", "b"]);
}

#[test]
fn s8_doubled_quote_inside_a_string_sub_expression() {
    // pwsh rejects a `""` pair inside the `$( )` of a double-quoted string in these shapes.
    err("foreach($i in(m \"$(t \"$\"`c`z\"\" '' )\")){v ''''}");
    err("v((m \"$(o \"\"\"\"`c`z\"\"\"\"`c`z\"\")\"))");
    err(
        "switch -Regex(x){1{v}''{(o\"\" $(t (t\"\") ) \"\"\"\")\nt\"`c`z\" \"$(o'''' \"$\"`c`z\"\")\"''}}v",
    );
    // pwsh: the same string at the top level is fine.
    ok("t \"$\"`c`z\"\"", &["t"]);
    ok("echo \"a $(b \"c $(d)\")\"", &["echo", "b", "d"]);
}
