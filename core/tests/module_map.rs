use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[test]
fn the_module_map_matches_the_files_on_disk() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let map_path = manifest_dir.join("AGENTS.md");

    // AGENTS.md is ignored, so its absence means this checkout does not carry the map to check.
    if !map_path.exists() {
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
/// The failure that produces reads as a bug in worktree adoption, arrives about once in a dozen full
/// runs depending on the order the thread pool happened to pick, and is neither. It cost this
/// repository a diagnosis it could not reproduce.
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
        let sets = text.matches(r#"set_var("NUCLEOS_WORKTREE_ROOT""#).count();
        if sets == 0 {
            continue;
        }
        let guards = text.contains("struct WorktreeRootEnv");
        if !guards || sets > 2 {
            offenders.push(format!(
                "{name}: {sets} bare set(s), guard present: {guards}"
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
