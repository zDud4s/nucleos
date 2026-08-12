fn main() {
    // `tauri::generate_context!()` (lib.rs) embeds the built frontend AT COMPILE TIME, and nothing
    // otherwise tells cargo so. Without these lines the crate looks fresh after a frontend rebuild:
    // `cargo build` prints `Finished` in a couple of seconds, touches nothing, and leaves an
    // executable serving the PREVIOUS UI. It is the worst shape a build failure can take, because
    // the build reports success — the only way to catch it is to notice the binary's timestamp did
    // not move, which is not something anyone checks.
    //
    // Watching the directory alone is not enough. Vite writes hashed bundles into `dist/assets/`,
    // so a content change lands in a SUBDIRECTORY and leaves `dist`'s own mtime untouched; the
    // recursion is what makes this fire. The directories are watched too, for files added or
    // removed rather than edited.
    //
    // `../dist` mirrors `build.frontendDist` in tauri.conf.json. A build script's working directory
    // is its package root, so this resolves to `shell/dist`, and the two have to be changed
    // together.
    watch(std::path::Path::new("../dist"));
    tauri_build::build()
}

/// Registers a path with cargo's rebuild triggers, and everything beneath it if it is a directory.
///
/// A missing `../dist` is silent on purpose: the crate cannot compile without it, and the error the
/// person needs is the one `generate_context!` gives about the frontend, not one from here about a
/// directory listing. `scripts/gates.sh` checks for it before building for the same reason.
fn watch(path: &std::path::Path) {
    println!("cargo:rerun-if-changed={}", path.display());
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        watch(&entry.path());
    }
}
