//! Regression tests for the fuzz counterexamples of round 8. Each comment names the behaviour of
//! `[System.Management.Automation.Language.Parser]::ParseInput` (pwsh 7) the test mirrors.

use super::scanner_tests_common::{err, ok};

#[test]
fn s10_escaped_quote_in_a_nested_string_is_refused() {
    // pwsh: 2 errors (missing string terminator, missing `)`): inside the `$( )` of a double-quoted
    // string, a nested string does not treat `` `" `` as an escaped quote.
    err("echo \"$(m \"a`\"b\")\"");
    err("echo \"$(m \"`c`\"z\")\"");
}

#[test]
fn s10_fuzz_cases_with_a_backtick_quote_word() {
    // Fuzz accepted-invalid cases, each pwsh: 2 errors.
    err("echo \"$(m\nt\nm\"`c`\"z\"\nv)\"");
    err("echo \"$(x\nv\no@\"\n\"@\nt\nt@\"\n\"@\nt\"`c`z\"\no''''\ns\"`c`\"z\"\nv)\"");
    err("echo \"$(v\nm\no\ne\"`c`\"z\"'''')\"");
}

#[test]
fn s10_escapes_outside_a_sub_expression_still_read() {
    ok("echo \"a`\"b\"", &["echo"]);
    ok("echo \"$(m 'a`\"b')\"", &["echo", "m"]);
    ok("echo \"$(m \"a`$b\")\"", &["echo", "m"]);
}
