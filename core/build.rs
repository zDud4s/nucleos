//! Embeds every file under `assets/agents/` so the daemon can install the agent packages.
//! Directory-driven: a new package or file needs no code change, only a rebuild.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/agents");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/agents");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let mut code = String::from("pub static EMBEDDED: &[(&str, &[u8])] = &[\n");
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        // Resolved against CARGO_MANIFEST_DIR at compile time, never an absolute path baked in
        // here: every checkout and worktree shares one target dir and cargo reuses this output
        // across them, so an absolute path pointed a later build at a worktree since deleted.
        let path = format!("/assets/agents/{rel}");
        writeln!(
            code,
            "    ({rel:?}, include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {path:?}))),"
        )
        .unwrap();
    }
    code.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("agents_embedded.rs");
    std::fs::write(out, code).unwrap();
}
