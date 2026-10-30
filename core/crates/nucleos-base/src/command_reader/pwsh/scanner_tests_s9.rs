//! Regression tests for the fuzz counterexamples of round 7. Each comment names the behaviour of
//! `[System.Management.Automation.Language.Parser]::ParseInput` (pwsh 7) the test mirrors.

use super::scanner_tests_common::{err, ok};

#[test]
fn s9_string_sub_expression_ends_at_the_first_raw_paren() {
    // pwsh ends a `$( )` inside a string at the first raw `)` that balances the `(`, quotes and
    // here-strings ignored; a `)` inside a nested string therefore cuts the sub-expression short.
    err("echo \"$(o \")\" )\"");
    err("echo \"$(o '(' )\"");
    err("echo \"$(o \"`)\" )\"");
    err("foreach($i in(m \"$(t \"$\"`c`z\")\" @'\n'@ )\")){v ''''}");
}

#[test]
fn s9_nested_strings_cut_by_a_raw_paren_in_a_switch() {
    // pwsh: 6 errors, the `")"` inside the nested `$( )` closes the sub-expression early.
    err(
        "switch -Regex(x){1{v}''{(o@\"\n\"@ $(t (t@\"\n\"@ ) ) \"\"\"\")\nt\"`c`z\" \
         \"$(o'''' \"$\"`c`z\")\")\"''}}v",
    );
}

#[test]
fn s9_quoted_closing_parens_in_a_nested_call() {
    // pwsh: 5 errors, the string `")"` after a backtick word ends the sub-expression early.
    err("v((m \"$(o \" \"b\" \"`c`z\")\" \" \"`c`z\")\")\"))");
}

#[test]
fn s9_redirection_target_is_not_a_comma_list() {
    // pwsh: "MissingArgument" for a comma list as the redirection target.
    err("i >.,t");
    err("i >a,t");
    err("i 2>>a,b");
    ok("i >'a,b'", &["i"]);
}

#[test]
fn s9_ordinary_inputs_still_read() {
    ok("echo \"a $(ls) b\"", &["echo", "ls"]);
    ok("echo \"a $(f \"x\") b\"", &["echo", "f"]);
    ok("echo hi > log.txt", &["echo"]);
    ok("echo hi >> log.txt 2>&1", &["echo"]);
}
