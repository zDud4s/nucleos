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
    tauri_build::build();
    manifest_the_test_binaries();
    shadow_the_default_manifest();
}

/// Hands the Windows resource `tauri_build` just built to the TEST binaries as well.
///
/// `tauri_build` emits it as `cargo:rustc-link-arg-bins`, which is exactly what it says: the bins
/// get it, and the crate's lib test binary — the one `cargo test` runs first — gets nothing. That
/// resource carries the application manifest, and the manifest is what binds comctl32 **version 6**
/// instead of the 5.82 in System32. So the moment anything in the link graph reaches a v6-only
/// export the test binary dies at load with `STATUS_ENTRYPOINT_NOT_FOUND`, before `main`, with no
/// test having run and nothing on screen but a hex code.
///
/// That is not hypothetical and it is not about any one line of code. Measured here on 2026-09-20:
/// the notch work added a reference to `core::ptr::drop_glue::<Result<(), String>>`, rustc resolved
/// it to the copy in tauri's own codegen unit 04 — `-Zshare-generics` reuses an upstream
/// instantiation rather than making a local one — and pulling that object in dragged
/// `tray_icon` → `muda` → `TaskDialogIndirect` and `SetWindowSubclass` behind it (read out of
/// `ld -Map`, chain by chain). Nothing about that chain is visible in the source, and no review
/// could have caught it: the same drop glue exists on either side of the change, and which crate's
/// copy is used is a codegen-unit accident. `notch_set_mode`'s comment records the same failure
/// arriving by a different accident — an `async` command — which is the tell that the fault is
/// here, in a test binary built without the manifest every other binary gets.
///
/// The instruction is the unsuffixed `cargo:rustc-link-arg`, deliberately, and the two obvious
/// alternatives do not work. `-tests` is refused outright — cargo answers "the package does not
/// have a test target", because that suffix means the `tests/` directory and this crate keeps its
/// tests in their modules. `-bins` is the one `tauri_build` already emitted, and the whole problem.
///
/// So every linked target is handed the resource, the bins included — and handing them the same
/// one twice costs nothing, because it is an ARCHIVE: the linker pulls a member once, whichever
/// path names it. Measured rather than assumed, on 2026-09-20: `target/debug/shell.exe` carries
/// three `RT_MANIFEST` entries built from this build script and exactly three built from the one
/// without these lines, while the lib test binary goes from none to one. The
/// `.rsrc merge failure` warning is older than this line and unchanged by it. And the condition
/// itself is asserted rather than assumed, by `this_test_binary_carries_the_application_manifest`
/// in `lib.rs`: it reads the running test binary and fails by name if the manifest goes missing
/// again.
fn manifest_the_test_binaries() {
    let Ok(out) = std::env::var("OUT_DIR") else {
        return;
    };
    let resource = std::path::Path::new(&out).join("libresource.a");
    if resource.exists() {
        println!("cargo:rustc-link-arg={}", resource.display());
    }
}

/// Keeps mingw's fallback manifest out of every link, so the application's is the only one.
///
/// On windows-gnu, gcc's startfile spec appends `default-manifest.o` to every link it drives, a
/// fallback for programs that bring no manifest of their own. This one does: the resource above
/// carries it, as `RT_MANIFEST` ID 1, language 1033, and so does mingw's file. GNU ld met the two
/// with a warning (`.rsrc merge failure: multiple non-default manifests`) and kept going. Rust
/// 1.97's windows-gnu links with its bundled `rust-lld` (`-fuse-ld=lld` on the gcc line), and lld
/// refuses outright: `duplicate resource: type MANIFEST (ID 24)/name ID 1/language 1033`, naming
/// `libresource.a` and mingw 16.1's `default-manifest.o`. Measured on 2026-10-22: that error is
/// what stopped `cargo test -p shell --lib` from linking, and a two-line C program reproduces it
/// with nothing of this crate's in it.
///
/// gcc finds `default-manifest.o` through its startfile search, and a `-B` directory is searched
/// first. So the build drops a file of that name into `OUT_DIR` — an empty archive, which a linker
/// reads and takes nothing from — and points gcc at it. Measured the same day on the C program:
/// the link succeeds, and the executable carries exactly one manifest, the one it brought. The
/// instruction is unsuffixed for the reason `manifest_the_test_binaries` gives, so the bins drop
/// the duplicate too. Any other target never asks for the file, so nothing is written there.
fn shadow_the_default_manifest() {
    let windows_gnu = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu");
    let Ok(out) = std::env::var("OUT_DIR") else {
        return;
    };
    if !windows_gnu {
        return;
    }
    let dir = std::path::Path::new(&out).join("no-default-manifest");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if std::fs::write(dir.join("default-manifest.o"), b"!<arch>\n").is_ok() {
        println!("cargo:rustc-link-arg=-B{}", dir.display());
    }
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
