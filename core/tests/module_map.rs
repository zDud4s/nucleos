use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The package this test is *running in*, checked against the one it was *compiled in*.
///
/// **`env!("CARGO_MANIFEST_DIR")` is baked at compile time, and with a shared target directory that
/// makes it a claim about a different checkout.** `CARGO_HOME/config.toml` on this machine points
/// every crate at `C:/Projects/.cargo-target`, so a `module_map-<hash>.exe` compiled inside a
/// worktree is reused by the main checkout whenever the hashes line up. That binary looks for
/// `AGENTS.md` under the *worktree's* `core/`, does not find it — no worktree has one — takes the
/// early return below, and reports PASS having checked nothing.
///
/// **Measured 2026-08-26, and it had already happened.** The shared `deps/` held two binaries at
/// once: `module_map-fbe301874ed541e2.exe` with `C:\Projects\nucleos\core` baked in, and
/// `module_map-71f5cbac06dd4edb.exe` with `C:\Projects\nucleos-conversas\core`. Which one cargo
/// runs is invisible from the output, so the gate was a coin flip that always looked green — and
/// `map_stamp.rs` landed with no row in the map while this test said everything was fine.
///
/// That is the exact shape of the false confidence the project-map feature exists to cure, sitting
/// inside this repository's own gate. So the mismatch is an ASSERTION and not a fallback: a test
/// that quietly reads another checkout's files is worse than one that refuses to run.
///
/// Cargo sets the working directory of an integration test to the package root, so
/// `current_dir` is the honest answer to *which checkout am I looking at*.
fn package_root() -> PathBuf {
    let built_in = Path::new(env!("CARGO_MANIFEST_DIR"));
    let running_in = std::env::current_dir().expect("the working directory should be readable");
    assert_eq!(
        built_in,
        running_in.as_path(),
        "this test binary was compiled in {} and is running in {} — a shared target directory has \
         handed this checkout a binary built somewhere else, so every path below would name the \
         other checkout's files. Touch this file to force a rebuild.",
        built_in.display(),
        running_in.display(),
    );
    running_in
}

#[test]
fn the_module_map_matches_the_files_on_disk() {
    let manifest_dir = package_root();
    let map_path = manifest_dir.join("AGENTS.md");

    // AGENTS.md is ignored, so its absence means this checkout does not carry the map to check.
    //
    // **This early return is why a stale map imitates a flake, and it cost a diagnosis.** The file
    // exists in the main checkout and in no worktree, so the same commit is GREEN wherever the gate
    // happens to run from a worktree and RED from the checkout — intermittent-looking, entirely
    // deterministic, and nothing in the output says which of the two just happened. A red here is
    // never a race: it is two modules added without a row, and the fix is to write the rows.
    //
    // **Only a worktree may take it, and that is now checked rather than assumed.** A worktree's
    // `.git` is a FILE pointing at the real directory; the main checkout's is a directory. So the
    // one place that must never skip this gate cannot: a main checkout missing the map fails here
    // instead of passing in silence. Without this the return is unfalsifiable — it is indeed
    // correct for a worktree, and it was also covering the case above.
    if !map_path.exists() {
        let git = manifest_dir
            .parent()
            .expect("the package root should have a parent")
            .join(".git");
        assert!(
            git.is_file(),
            "{} has no AGENTS.md and {} is not a worktree's .git file, so this is the main \
             checkout and the module map is simply missing. Skipping here would report a gate as \
             green that checked nothing.",
            manifest_dir.display(),
            git.display(),
        );
        return;
    }

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
/// not — see the note on the early return above. Twenty-two archived full runs, six of them at
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
    // `package_root` and not `env!`, for the reason it documents: `src/` exists in every worktree,
    // so this test would not have gone red on a binary built elsewhere — it would have read the
    // other checkout's modules and reported on those. Quieter than the map check's failure and the
    // same defect.
    let src = package_root().join("src");
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
