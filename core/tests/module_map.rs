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
