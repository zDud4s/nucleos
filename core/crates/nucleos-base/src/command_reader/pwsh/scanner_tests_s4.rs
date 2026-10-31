//! S4 RED: assignments and statement keywords. Values fixed against the pwsh 7 parser.
//! Malformed keyword statements are parse errors in the oracle, hence `err`.

use super::scanner_tests_common::{err, ok};

#[test]
fn s4_assignments() {
    ok("$x = a", &["a"]);
    ok("$x = (a | b)", &["a", "b"]);
    ok("$x += a", &["a"]);
    ok("$x -= 1", &[]);
    ok("$a, $b = 1, 2", &[]);
    ok("$env:PATH = 'x'", &[]);
    ok("[int]$x = f", &["f"]);
    ok("$x = if (c) { a } else { b }", &["c", "a", "b"]);
    ok("$x = a | b", &["a", "b"]);
    ok("$x = $(a; b)", &["a", "b"]);
    ok("$x++", &[]);
    ok("$h['k'] = a", &["a"]);
    ok("$x.y = a", &["a"]);
    ok("$x = $y = a", &["a"]);
    ok("$x = 1; a", &["a"]);
    ok("$x = a; b", &["a", "b"]);
    ok("$x = \"v: $(a)\"", &["a"]);
    ok("$x = @(a)", &["a"]);
    ok("$x = @{ k = (a) }", &["a"]);
}

#[test]
fn s4_if() {
    ok("if (a) { b }", &["a", "b"]);
    ok("if (a) { b } else { c }", &["a", "b", "c"]);
    ok(
        "if (a) { b } elseif (c) { d } else { e }",
        &["a", "b", "c", "d", "e"],
    );
    ok(
        "IF (a) { b } ELSEIF (c) { d } ELSE { e }",
        &["a", "b", "c", "d", "e"],
    );
    ok("if ($x -eq 1) { a }", &["a"]);
    ok("if (a -and b) { c }", &["a", "c"]);
    ok("if (a)\n{ b }\nelse\n{ c }", &["a", "b", "c"]);
    ok("if ($x) { a } else { if ($y) { b } }", &["a", "b"]);
    ok("if (a | b) { c }", &["a", "b", "c"]);
    ok("if (!(a)) { b }", &["a", "b"]);
    ok("if (a) { b }; c", &["a", "b", "c"]);
}

#[test]
fn s4_if_malformed() {
    err("if (a)");
    err("if a { b }");
    err("if (a { b }");
    err("if () { b }");
    err("if (a) { b } elseif { c }");
    err("if (a) { b } else");
    err("if (a) { b");
    // Oracle: a stray `elseif` is an ordinary command name here.
    ok("elseif (a) { b }", &["elseif", "a", "b"]);
}

#[test]
fn s4_foreach() {
    ok("foreach ($i in a) { b }", &["a", "b"]);
    ok("foreach ($i in 1..3) { a $i }", &["a"]);
    ok("foreach ($i in (a | b)) { c }", &["a", "b", "c"]);
    ok("FOREACH ($i in a) { b }", &["a", "b"]);
    ok("foreach ($i in a)\n{\n b\n}", &["a", "b"]);
    ok("foreach ($i in $x) { }", &[]);
    ok("foreach ($i in a) { b | c }", &["a", "b", "c"]);
}

#[test]
fn s4_foreach_malformed() {
    err("foreach ($i in a)");
    err("foreach ($i a) { b }");
    err("foreach ($i in a { b }");
    err("foreach () { b }");
    err("foreach ($i in a) { b");
}

#[test]
fn s4_for() {
    // Oracle: a bareword on the right of `-lt` is a parse error (the plan guessed `["b"]`).
    err("for ($i=0; $i -lt n; $i++) { b }");
    ok("for ($i = 0; $i -lt (a); $i++) { b }", &["a", "b"]);
    ok("for (;;) { a }", &["a"]);
    ok("for ($i=0; $i -lt 3; $i++) { a; b }", &["a", "b"]);
    ok("for ($i = a; $i -lt 3; $i++) { b }", &["a", "b"]);
}

#[test]
fn s4_for_malformed() {
    err("for ($i=0; $i -lt n) { b }");
    err("for (;;)");
    err("for ($i=0; $i -lt n; $i++ { b }");
}

#[test]
fn s4_while() {
    ok("while (a) { b }", &["a", "b"]);
    ok("while ($i -lt 3) { a; $i++ }", &["a"]);
    ok("WHILE (a -and b) { c }", &["a", "c"]);
    ok("while ($true) { a; break }", &["a"]);
}

#[test]
fn s4_do_loops() {
    ok("do { a } while (b)", &["b", "a"]);
    ok("do { a } until (b)", &["b", "a"]);
    ok("do { a; b } while ($i -lt 3)", &["a", "b"]);
    ok("DO { a } WHILE (b)", &["b", "a"]);
    ok("do { a }\nwhile (b)", &["b", "a"]);
}

#[test]
fn s4_do_malformed() {
    err("do { a }");
    err("do { a } while");
    err("do { a } while (b");
    err("do { a } until");
}

#[test]
fn s4_switch() {
    ok("switch (a) { 1 { b } default { c } }", &["a", "b", "c"]);
    ok("switch ($x) { 'a' { b } 'c' { d } }", &["b", "d"]);
    ok("switch -Regex ($x) { \"a\" { b } }", &["b"]);
    ok("switch ($x) { default { a } }", &["a"]);
    ok("switch ($x) { 1 { a } 2 { b } }", &["a", "b"]);
    // Oracle: an empty switch body is a parse error.
    err("switch (a) { }");
    ok("SWITCH (a) { 1 { b } DEFAULT { c } }", &["a", "b", "c"]);
    ok("switch ($x) { { $_ -gt 1 } { a } }", &["a"]);
}

#[test]
fn s4_switch_malformed() {
    err("switch (a)");
    err("switch (a) { 1 { b }");
    err("switch { 1 { b } }");
}

#[test]
fn s4_try() {
    ok(
        "try { a } catch [Type] { b } finally { c }",
        &["a", "b", "c"],
    );
    ok("try { a } catch { b }", &["a", "b"]);
    ok("try { a } finally { b }", &["a", "b"]);
    ok("try { a } catch [A],[B] { b }", &["a", "b"]);
    ok("TRY { a } CATCH { b } FINALLY { c }", &["a", "b", "c"]);
    ok("try { a }\ncatch { b }\nfinally { c }", &["a", "b", "c"]);
    ok("try { a } catch [Type] { b } catch { c }", &["a", "b", "c"]);
}

#[test]
fn s4_try_malformed() {
    err("try { a }");
    err("try a catch { b }");
    err("try { a } catch");
    err("try { a } catch { b");
    err("try { a } catch [Type { b }");
}

#[test]
fn s4_function() {
    ok("function f { a }", &["a"]);
    ok("function f($p) { a }", &["a"]);
    ok("function f { param($x) a }", &["a"]);
    ok("function f { param($x, $y) a $x }", &["a"]);
    ok("FUNCTION f { a }", &["a"]);
    ok("function Get-Foo { a | b }", &["a", "b"]);
    ok("function f { b }; f", &["b", "f"]);
    ok("function f($p = (a)) { b }", &["a", "b"]);
    ok("function f { [CmdletBinding()] param($x) a }", &["a"]);
    ok("param($x)", &[]);
    ok("param($x, $y = (a))", &["a"]);
    ok(
        "function f { begin { a } process { b } end { c } }",
        &["a", "b", "c"],
    );
}

#[test]
fn s4_function_malformed() {
    err("function f");
    err("function { a }");
    err("function f { a");
    err("function f($p { a }");
    err("param(");
}

#[test]
fn s4_flow_keywords() {
    ok("return a", &["a"]);
    ok("return", &[]);
    ok("return (a)", &["a"]);
    ok("throw a", &["a"]);
    ok("throw", &[]);
    ok("exit", &[]);
    ok("exit 1", &[]);
    ok("break", &[]);
    ok("continue", &[]);
    ok("exit a", &["a"]);
    ok("return a | b", &["a", "b"]);
    ok("throw \"x $(a)\"", &["a"]);
    ok("return $x; a", &["a"]);
    ok("if (a) { return }", &["a"]);
    ok(
        "foreach ($i in a) { if (b) { continue } c }",
        &["a", "b", "c"],
    );
    ok("while (a) { break }", &["a"]);
}

#[test]
fn s4_out_of_subset() {
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("trap { a }");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("trap [Type] { a }");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("filter f { a }");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("class C { }");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("class C { [void] M() { a } }");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("enum E { A; B }");
    err("workflow w { a }");
    err("data d { a }");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("using namespace System");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("#requires -Version 7");
    err("using module M");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err("configuration c { }");
}

#[test]
fn s4_label_loops() {
    // The oracle accepts this; the closed subset deliberately refuses it.
    err(":outer foreach ($i in a) { b }");
    // The oracle accepts this; the closed subset deliberately refuses it.
    err(":outer while (a) { break outer }");
}

#[test]
fn s4_keyword_as_argument() {
    ok("echo if", &["echo"]);
    ok("echo foreach", &["echo"]);
    ok("echo while", &["echo"]);
    ok("echo function", &["echo"]);
    ok("echo return", &["echo"]);
    ok("echo try", &["echo"]);
    ok("echo else", &["echo"]);
    ok("echo switch", &["echo"]);
    ok("echo throw", &["echo"]);
    ok("echo param", &["echo"]);
    ok("echo break continue", &["echo"]);
    ok("a -if b", &["a"]);
    ok("a | echo for", &["a", "echo"]);
    ok("a if", &["a"]);
    ok("a; echo do", &["a", "echo"]);
}

#[test]
fn s4_foreach_object_parallel() {
    ok("ForEach-Object -Parallel { a }", &["ForEach-Object", "a"]);
    ok(
        "1..3 | ForEach-Object -Parallel { a $_ }",
        &["ForEach-Object", "a"],
    );
    ok(
        "a | ForEach-Object -Parallel { b | c } -ThrottleLimit 2",
        &["a", "ForEach-Object", "b", "c"],
    );
    ok("a | % -Parallel { b }", &["a", "%", "b"]);
    ok(
        "a | ForEach-Object -Parallel { b } | c",
        &["a", "ForEach-Object", "b", "c"],
    );
}
