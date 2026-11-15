//! Guard: no two files in `core/migrations/` share a numeric prefix.
//!
//! Two branches can each add `0305_*.sql`; git merges them cleanly (different file names) and then
//! sqlx fails every DB test with an opaque error. This test fails first, naming the files and the
//! next free number. It reads only file names, so it needs no database.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// The numeric prefix of a migration file name (`0305_x.sql` -> `305`), if it has one.
fn prefix(name: &str) -> Option<u64> {
    let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !name.ends_with(".sql") {
        return None;
    }
    digits.parse().ok()
}

/// One message per prefix shared by two or more names, plus the next free number.
fn collisions(names: &[String]) -> Vec<String> {
    let mut by_prefix: BTreeMap<u64, Vec<&str>> = BTreeMap::new();
    let mut max = 0;
    for name in names {
        if let Some(p) = prefix(name) {
            max = max.max(p);
            by_prefix.entry(p).or_default().push(name);
        }
    }
    by_prefix
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(p, files)| {
            format!(
                "migration number {p:04} is used by {}; renumber one (next free number: {:04})",
                files.join(", "),
                max + 1
            )
        })
        .collect()
}

fn migrations_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    cwd.ancestors()
        .map(|a| a.join("migrations"))
        .find(|d| d.is_dir())
}

#[test]
fn collisions_name_the_files_and_the_next_free_number() {
    let names: Vec<String> = [
        "0304_a.sql",
        "0305_decision_answered_obsolete.sql",
        "0305_proposal_culprit.sql",
    ]
    .map(String::from)
    .to_vec();
    let found = collisions(&names);
    assert_eq!(found.len(), 1);
    assert!(found[0].contains("0305_decision_answered_obsolete.sql"));
    assert!(found[0].contains("0305_proposal_culprit.sql"));
    assert!(found[0].contains("next free number: 0306"));
}

#[test]
fn distinct_numbers_have_no_collision() {
    let names = ["0001_a.sql", "0002_b.sql", "notes.txt"].map(String::from);
    assert!(collisions(&names).is_empty());
}

#[test]
fn migration_numbers_are_unique() {
    let Some(dir) = migrations_dir() else {
        panic!("no migrations/ directory found above the working directory");
    };
    let names: Vec<String> = fs::read_dir(&dir)
        .expect("migrations/ should be readable")
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let found = collisions(&names);
    assert!(found.is_empty(), "{}", found.join("\n"));
}
