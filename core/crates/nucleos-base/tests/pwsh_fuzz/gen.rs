//! Deterministic PRNG (xorshift64*) and a small grammar-based generator of PowerShell phrases.
//! Same seed, same phrases: the fuzz must be reproducible from the printed seed.

pub const MAX_LEN: usize = 4096;

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        let mut r = Rng(seed ^ 0x9E37_79B9_7F4A_7C15);
        if r.0 == 0 {
            r.0 = 0x2545_F491_4F6C_DD1D;
        }
        // Small seeds start with weak output; burn a few draws.
        for _ in 0..8 {
            r.next_u64();
        }
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish value in `0..n` (0 when `n == 0`).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            ((self.next_u64() >> 11) % n as u64) as usize
        }
    }

    pub fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }

    pub fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

/// Truncates at a char boundary so the case stays within `max` bytes.
pub fn cap(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

const NAMES: &[&str] = &[
    "git",
    "Get-ChildItem",
    "a-b",
    "'q uoted'",
    "\"x$(y)\"",
    "&",
    ".",
    "ls",
    "echo",
    "Write-Output",
    "cargo",
    "Get-Content",
    "npm",
    "Set-Location",
    "Get-Process",
    "Write-Host",
    "Select-String",
];
const PARAMS: &[&str] = &[
    "-Path", "-Force", "-n", "--flag", "-Name:x", "-Recurse", "-Filter", "-First",
];
const WORDS: &[&str] = &[
    "foo",
    "./bar",
    "a.txt",
    "*.rs",
    "1",
    "$true",
    "$x",
    "$env:PATH",
    "$_",
    "$_.Name",
    "C:\\tmp\\f",
    "a,b",
];
const REDIRECTS: &[&str] = &[
    " > out.txt",
    " >> log",
    " 2>&1",
    " 2> $null",
    " *> all.log",
    " 1>&2",
];
const CHAIN: &[&str] = &[" && ", " || ", "; ", " ?? "];
const BLOCK_CMDLETS: &[&str] = &["ForEach-Object", "Where-Object", "%", "?"];

fn cmd(r: &mut Rng, d: u32) -> String {
    let name = *r.pick(NAMES);
    let mut s = match name {
        "&" => "& 'tool'".to_string(),
        "." => ". ./x.ps1".to_string(),
        n => n.to_string(),
    };
    for _ in 0..r.below(4) {
        s.push(' ');
        s.push_str(&arg(r, d));
    }
    if r.chance(15) {
        s.push_str(r.pick(REDIRECTS));
    }
    s
}

fn arg(r: &mut Rng, d: u32) -> String {
    let n = if d > 0 { 13 } else { 8 };
    let e = d.saturating_sub(1);
    match r.below(n) {
        0 => r.pick(PARAMS).to_string(),
        1 => r.pick(WORDS).to_string(),
        2 => "'text here'".to_string(),
        3 => "\"a $x b\"".to_string(),
        4 => "'it''s'".to_string(),
        5 => "@'\nline $x\n'@".to_string(),
        6 => "@\"\nhere $x\n\"@".to_string(),
        7 => "\"`\"esc`\" `$z\"".to_string(),
        8 => format!("\"v=$({})\"", cmd(r, e)),
        9 => format!("$({})", pipeline(r, e)),
        10 => format!("({})", pipeline(r, e)),
        11 => format!("@({})", pipeline(r, e)),
        _ => format!("{{ {} }}", stmts(r, e)),
    }
}

fn pipeline(r: &mut Rng, d: u32) -> String {
    let mut s = cmd(r, d);
    for _ in 0..r.below(3) {
        s.push_str(" | ");
        s.push_str(&cmd(r, d));
    }
    if d > 0 && r.chance(25) {
        s.push_str(&format!(
            " | {} {{ {} }}",
            r.pick(BLOCK_CMDLETS),
            stmts(r, d - 1)
        ));
    }
    if r.chance(20) {
        s.push_str(r.pick(CHAIN));
        s.push_str(&cmd(r, d));
    }
    s
}

fn expr(r: &mut Rng, d: u32) -> String {
    match r.below(16) {
        0 => "1..5".into(),
        1 => "$x.M()".into(),
        2 => "[int]'3'".into(),
        3 => "'{0}' -f $a".into(),
        4 => "$s -replace 'a','b'".into(),
        5 => "$a ? 1 : 2".into(),
        6 => "$o?.p".into(),
        7 => "$a ?? 'd'".into(),
        8 => "$a -eq 1 -and $b".into(),
        9 => format!("$x.Y.Z($({}))", cmd(r, d.saturating_sub(1))),
        10 => "@(1,2,3)".into(),
        11 => "@{a=1;b=2}".into(),
        12 => "$a + $b * 2".into(),
        13 => "$x -match 'z'".into(),
        14 => "$a -gt 1".into(),
        _ => "-not $flag".into(),
    }
}

fn cond(r: &mut Rng, d: u32) -> String {
    if r.chance(25) {
        format!("({})", pipeline(r, d.saturating_sub(1)))
    } else {
        expr(r, d)
    }
}

fn block(r: &mut Rng, d: u32) -> String {
    format!("{{ {} }}", stmts(r, d))
}

fn stmts(r: &mut Rng, d: u32) -> String {
    let n = if d >= 3 {
        1 + r.below(2)
    } else {
        1 + r.below(3)
    };
    let mut out = String::new();
    for i in 0..n {
        if i > 0 {
            out.push_str(r.pick(&["; ", "\n"]));
        }
        out.push_str(&stmt(r, d));
    }
    out
}

fn stmt(r: &mut Rng, d: u32) -> String {
    let e = d.saturating_sub(1);
    let n = if d == 0 { 6 } else { 20 };
    match r.below(n) {
        0 | 1 => pipeline(r, d),
        2 => format!(
            "$v = {}",
            if r.chance(50) {
                pipeline(r, d)
            } else {
                expr(r, d)
            }
        ),
        3 => expr(r, d),
        4 => format!("{} # note", cmd(r, d)),
        5 => format!("<# c #> {}", cmd(r, d)),
        6 => {
            let mut s = format!("if ({}) {}", cond(r, e), block(r, e));
            if r.chance(40) {
                s.push_str(&format!(" elseif ({}) {}", cond(r, e), block(r, e)));
            }
            if r.chance(40) {
                s.push_str(&format!(" else {}", block(r, e)));
            }
            s
        }
        7 => format!(
            "foreach ($i in {}) {}",
            if r.chance(50) {
                "1..3".to_string()
            } else {
                format!("({})", pipeline(r, e))
            },
            block(r, e)
        ),
        8 => format!("for ($i = 0; $i -lt 3; $i++) {}", block(r, e)),
        9 => format!("while ({}) {}", cond(r, e), block(r, e)),
        10 => format!(
            "do {} {} ({})",
            block(r, e),
            if r.chance(50) { "while" } else { "until" },
            cond(r, e)
        ),
        11 => {
            let mut s = format!("try {} catch {}", block(r, e), block(r, e));
            if r.chance(50) {
                s.push_str(&format!(" finally {}", block(r, e)));
            }
            s
        }
        12 => format!(
            "function Foo-{} {{ param($a) {} }}",
            r.below(100),
            stmts(r, e)
        ),
        13 => format!(
            "switch{} ($x) {{ 1 {} 'a' {} default {} }}",
            if r.chance(30) { " -Regex" } else { "" },
            block(r, e),
            block(r, e),
            block(r, e)
        ),
        14 => format!("$v += {}", expr(r, d)),
        15 => "return".into(),
        16 => "throw 'x'".into(),
        17 => format!("{} 2>&1 | Out-Null", cmd(r, d)),
        18 => format!("& {{ {} }}", stmts(r, e)),
        _ => format!("$v = @({})", pipeline(r, e)),
    }
}

/// One generated phrase (depth <= 4, <= 4 KiB).
pub fn generate(r: &mut Rng) -> String {
    let d = 1 + r.below(4) as u32;
    let mut out = String::new();
    for i in 0..(1 + r.below(2)) {
        if i > 0 {
            out.push_str(r.pick(&["; ", "\n"]));
        }
        out.push_str(&stmts(r, d));
    }
    cap(out, MAX_LEN)
}
