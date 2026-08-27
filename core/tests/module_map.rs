use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[test]
fn the_module_map_matches_the_files_on_disk() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let map_path = manifest_dir.join("AGENTS.md");

    // No early return for an absent map any more, and its removal is the point rather than tidying.
    // AGENTS.md used to be gitignored, so this test skipped itself wherever the file happened not
    // to be — green in the one checkout that held it, red in every worktree, off the same commit,
    // with nothing in the output saying which of the two had just happened. That reads as a flake
    // and is not one; the diagnosis cost real time. The file is tracked now (see `.gitignore`), so
    // its absence is a checkout somebody broke, and `read_to_string` below says so loudly instead
    // of this passing quietly.

    let mapped_files = fs::read_to_string(&map_path)
        .expect("the module map should be readable")
        .lines()
        .filter_map(|line| {
            let field = line.strip_prefix("| `")?.split_once("` |")?.0;
            field.ends_with(".rs").then_some(field.to_owned())
        })
        .collect::<BTreeSet<_>>();

    let source_files = fs::read_dir(manifest_dir.join("src"))
        .expect("the source directory should be readable")
        .map(|entry| entry.expect("source directory entries should be readable"))
        .filter_map(|entry| {
            entry
                .file_type()
                .expect("source file types should be readable")
                .is_file()
                .then_some(entry.file_name().to_string_lossy().into_owned())
        })
        .filter(|file_name| file_name.ends_with(".rs"))
        .collect::<BTreeSet<_>>();

    let on_disk_not_in_map = source_files
        .difference(&mapped_files)
        .cloned()
        .collect::<Vec<_>>();
    let in_map_not_on_disk = mapped_files
        .difference(&source_files)
        .cloned()
        .collect::<Vec<_>>();

    assert!(
        on_disk_not_in_map.is_empty() && in_map_not_on_disk.is_empty(),
        "on disk but not in the map: {on_disk_not_in_map:?}\nin the map but not on disk: {in_map_not_on_disk:?}"
    );
}

/// **The process-wide worktree root is only ever set through something that puts it back.**
///
/// `worktree_root` returns `NUCLEOS_WORKTREE_ROOT` verbatim when it is set, and only falls back to a
/// sibling of the project root when it is not. So a test that sets it with a bare `set_var` and
/// never restores it does not affect only itself: every test that runs afterwards in the same
/// process and provisions a worktree WITHOUT naming a root of its own inherits that one — usually a
/// `TempDir` that has since been deleted. They then share a root, and two of them creating a job
/// with the same id collide on `nucleos/job-<id>`.
///
/// The failure that would produce reads as a bug in worktree adoption and is not one.
///
/// **Corrected 2026-08-22: this said it "arrives about once in a dozen full runs", and that rate
/// was never measured.** It was inferred while hunting a red that was assumed intermittent and was
/// not — the map above used to skip itself wherever the file was absent, which is what made a
/// deterministic red look like a race. Twenty-two archived full runs, six of them at
/// `--test-threads=32`, have never produced this failure. The hazard is real and reads straight off
/// `worktree_root`; how often it would bite is unknown.
///
/// Each module that needs the variable carries its own `WorktreeRootEnv` guard, which records the
/// previous value and restores it on drop — two `set_var` calls, one in `set` and one in `drop`. So
/// the rule this checks is arithmetic: a file may name the variable twice if it defines the guard,
/// and not at all otherwise. Taking the lock is a separate obligation and a separate check; this one
/// is only about putting the value back.
///
/// A text check because there is no type to hang it on: `std::env::set_var` is available to every
/// module and the thing being forbidden is calling it directly.
#[test]
fn nothing_sets_the_worktree_root_without_restoring_it() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();

    for entry in fs::read_dir(&src).expect("the source directory should be readable") {
        let entry = entry.expect("source directory entries should be readable");
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".rs") {
            continue;
        }
        let text = fs::read_to_string(entry.path()).expect("a source file should be readable");
        // BOTH verbs, and counting `remove_var` is not symmetry for its own sake: the guard's
        // `drop` restores by SETTING the old value or by REMOVING it when there was none, so a
        // module that only ever removed the variable would have zero `set_var` and slip past this
        // check entirely. One had exactly that — a bare `remove_var` left behind in `runs.rs`
        // when the guard went in — and this test called the module clean.
        let sets = text.matches(r#"set_var("NUCLEOS_WORKTREE_ROOT""#).count();
        let removes = text
            .matches(r#"remove_var("NUCLEOS_WORKTREE_ROOT""#)
            .count();
        if sets == 0 && removes == 0 {
            continue;
        }
        // Two sets and one remove: `set` writes once, `drop` writes once or removes once.
        let guards = text.contains("struct WorktreeRootEnv");
        if !guards || sets > 2 || removes > 1 {
            offenders.push(format!(
                "{name}: {sets} set(s), {removes} remove(s), guard present: {guards}"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "the worktree root is set somewhere that does not restore it: {offenders:?}\n\
         Use that module's `WorktreeRootEnv::set`, which records the previous value and puts it \
         back on drop."
    );
}
