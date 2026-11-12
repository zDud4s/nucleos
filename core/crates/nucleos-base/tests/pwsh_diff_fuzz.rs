//! Differential fuzz: the pure-Rust PowerShell scanner against the real pwsh Parser.
//!
//! `pwsh_diff_fuzz` needs pwsh 7 and is `#[ignore]`d; the generator tests below do not.
//! Env: NUCLEOS_FUZZ_N (default 10000), NUCLEOS_FUZZ_SEED (default: wall clock ms, always
//! printed), NUCLEOS_PWSH_EXE (else `engine::default_exe()`).
#![allow(dead_code)]

#[path = "pwsh_fuzz/gen.rs"]
mod generator;
#[path = "pwsh_fuzz/mutate.rs"]
mod mutate;
#[path = "pwsh_fuzz/oracle.rs"]
mod oracle;

use generator::{Rng, generate};
use nucleos_base::command_reader::pwsh::{command_names, engine};
use oracle::{Oracle, OracleOut};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

const SEEDS: &str = include_str!("pwsh_fuzz/seeds.txt");

enum Verdict {
    Agree,
    /// Scanner said `Err`; `oracle_rejected` when the Parser also refused the text.
    Unreadable {
        oracle_rejected: bool,
    },
    Silent {
        scanner: Vec<String>,
        parser: Vec<String>,
    },
    AcceptedInvalid {
        scanner: Vec<String>,
        parser_err: String,
    },
    Panic(String),
}

impl Verdict {
    fn is_error(&self) -> bool {
        matches!(
            self,
            Verdict::Silent { .. } | Verdict::AcceptedInvalid { .. } | Verdict::Panic(_)
        )
    }
    fn same_kind(&self, other: &Verdict) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
    fn describe(&self) -> String {
        match self {
            Verdict::Silent { scanner, parser } => {
                format!("SILENT scanner={scanner:?} parser={parser:?}")
            }
            Verdict::AcceptedInvalid {
                scanner,
                parser_err,
            } => {
                format!("ACCEPTED_INVALID scanner={scanner:?} parser=rejected({parser_err})")
            }
            Verdict::Panic(m) => format!("SCANNER_PANIC {m}"),
            _ => "ok".into(),
        }
    }
}

fn classify(src: &str, oracle: &mut Oracle) -> Verdict {
    let scanned = match catch_unwind(AssertUnwindSafe(|| command_names(src))) {
        Ok(r) => r,
        Err(p) => {
            let m = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic>".into());
            return Verdict::Panic(m);
        }
    };
    match (scanned, oracle.names(src)) {
        (Err(_), OracleOut::Rejected(_)) => Verdict::Unreadable {
            oracle_rejected: true,
        },
        (Err(_), OracleOut::Names(_)) => Verdict::Unreadable {
            oracle_rejected: false,
        },
        (Ok(a), OracleOut::Names(b)) if a == b => Verdict::Agree,
        (Ok(a), OracleOut::Names(b)) => Verdict::Silent {
            scanner: a,
            parser: b,
        },
        (Ok(a), OracleOut::Rejected(e)) => Verdict::AcceptedInvalid {
            scanner: a,
            parser_err: e,
        },
    }
}

/// Simple delta-debugging: drop lines, then chars, while `fails` keeps holding. Bounded by `budget` probes.
fn minimize<F: FnMut(&str) -> bool>(src: &str, mut fails: F) -> String {
    let mut budget = 1500usize;
    let mut cur = src.to_string();
    // Line pass.
    loop {
        let lines: Vec<&str> = cur.split('\n').collect();
        if lines.len() <= 1 {
            break;
        }
        let mut shrunk = None;
        for i in 0..lines.len() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let cand: Vec<&str> = lines
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, l)| *l)
                .collect();
            let cand = cand.join("\n");
            if fails(&cand) {
                shrunk = Some(cand);
                break;
            }
        }
        match shrunk {
            Some(c) => cur = c,
            None => break,
        }
    }
    // Char pass.
    let mut chars: Vec<char> = cur.chars().collect();
    let mut i = 0;
    while i < chars.len() && budget > 0 {
        budget -= 1;
        let cand: String = chars
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, c)| *c)
            .collect();
        if fails(&cand) {
            chars.remove(i);
        } else {
            i += 1;
        }
    }
    chars.into_iter().collect()
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

#[test]
#[ignore = "needs pwsh 7; run via the gate command"]
fn pwsh_diff_fuzz() {
    let n = env_u64("NUCLEOS_FUZZ_N").unwrap_or(10_000) as usize;
    let seed = env_u64("NUCLEOS_FUZZ_SEED").unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1)
    });
    println!("seed={seed}");
    let exe = std::env::var_os("NUCLEOS_PWSH_EXE")
        .map(PathBuf::from)
        .or_else(engine::default_exe)
        .expect("pwsh 7 not found: set NUCLEOS_PWSH_EXE (the fuzz gate must not pass silently without an oracle)");
    let mut oracle = Oracle::new(exe);
    let seeds = mutate::load_seeds(SEEDS);
    assert!(!seeds.is_empty(), "seeds.txt is empty");
    let mut rng = Rng::new(seed);

    let (mut agree, mut unreadable, mut oracle_rejected) = (0usize, 0usize, 0usize);
    let (mut silent, mut accepted_invalid, mut panics) = (0usize, 0usize, 0usize);
    // Up to 20 counterexamples per kind, so one common kind cannot crowd out a rare one.
    let mut examples: Vec<String> = Vec::new();
    let mut shown = [0usize; 3];

    for _ in 0..n {
        let src = if rng.chance(60) {
            generate(&mut rng)
        } else {
            mutate::mutate(&mut rng, &seeds)
        };
        let v = classify(&src, &mut oracle);
        match &v {
            Verdict::Agree => agree += 1,
            Verdict::Unreadable {
                oracle_rejected: rej,
            } => {
                unreadable += 1;
                if *rej {
                    oracle_rejected += 1;
                }
            }
            Verdict::Silent { .. } => silent += 1,
            Verdict::AcceptedInvalid { .. } => accepted_invalid += 1,
            Verdict::Panic(_) => panics += 1,
        }
        let slot = match &v {
            Verdict::Silent { .. } => 0,
            Verdict::AcceptedInvalid { .. } => 1,
            _ => 2,
        };
        if v.is_error() && shown[slot] < 20 {
            shown[slot] += 1;
            let min = minimize(&src, |cand| classify(cand, &mut oracle).same_kind(&v));
            let mv = classify(&min, &mut oracle);
            examples.push(format!(
                "src={min:?}\n  original={src:?}\n  {}",
                mv.describe()
            ));
        }
    }

    let coverage = if agree + unreadable == 0 {
        0.0
    } else {
        agree as f64 / (agree + unreadable) as f64
    };
    println!("N={n}");
    println!("agree={agree}");
    println!("unreadable={unreadable}");
    println!("oracle_rejected={oracle_rejected}");
    println!("silent_errors={silent}");
    println!("accepted_invalid={accepted_invalid}");
    println!("scanner_panics={panics}");
    println!("oracle_restarts={}", oracle.restarts);
    println!("coverage={:.1}%", coverage * 100.0);
    if coverage < 0.5 {
        println!("WARNING: coverage below 50%: a scanner that always says Err passes trivially");
    }
    for (i, e) in examples.iter().enumerate() {
        println!("counterexample #{}: {e}", i + 1);
    }
    assert!(
        silent + accepted_invalid + panics == 0,
        "fuzz found {silent} silent errors, {accepted_invalid} accepted-invalid, {panics} scanner panics (seed={seed})"
    );
}

fn phrases(seed: u64, k: usize) -> Vec<String> {
    let mut r = Rng::new(seed);
    (0..k).map(|_| generate(&mut r)).collect()
}

#[test]
fn fuzz_generator_is_deterministic() {
    assert_eq!(phrases(42, 200), phrases(42, 200));
    assert_ne!(phrases(42, 200), phrases(43, 200));
}

#[test]
fn fuzz_generator_mutation_is_deterministic() {
    let seeds = mutate::load_seeds(SEEDS);
    assert!(
        seeds.len() >= 40,
        "expected ~40 seed lines, got {}",
        seeds.len()
    );
    let run = |s: u64| {
        let mut r = Rng::new(s);
        (0..200)
            .map(|_| mutate::mutate(&mut r, &seeds))
            .collect::<Vec<_>>()
    };
    assert_eq!(run(7), run(7));
    assert!(run(7).iter().all(|m| m.len() <= 4096));
}

#[test]
fn fuzz_generator_emits_varied_cases() {
    const MARKERS: &[&str] = &[
        "if ",
        "elseif",
        "else ",
        "foreach",
        "for (",
        "while",
        "do {",
        "try",
        "catch",
        "finally",
        "function",
        "switch",
        "&&",
        "||",
        "??",
        "-replace",
        "-f ",
        "?.",
        "ForEach-Object",
        "Where-Object",
        "@(",
        "$(",
        "<#",
        "2>&1",
        ">>",
        "@'",
        "@\"",
        "..",
        " ? ",
        "#",
    ];
    let all = phrases(1234, 1000).join("\n");
    let seen: Vec<&&str> = MARKERS.iter().filter(|m| all.contains(**m)).collect();
    assert!(
        seen.len() >= 20,
        "only {} distinct markers in 1000 phrases: {seen:?}",
        seen.len()
    );
    assert!(phrases(1234, 1000).iter().all(|p| p.len() <= 4096));
}
