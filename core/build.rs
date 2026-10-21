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
        let abs = file.to_string_lossy().replace('\\', "/");
        writeln!(code, "    ({rel:?}, include_bytes!({abs:?})),").unwrap();
    }
    code.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("agents_embedded.rs");
    std::fs::write(out, code).unwrap();
}
