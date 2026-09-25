//! Builds Atelier Core (`plugins/atelier-core`) into the program: every file in the
//! folder becomes an entry of `CORE_FILES` (its path inside the folder, and its text),
//! which `plugin::core` loads like any plugin folder.

use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/atelier-core");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut found = Vec::new();
    files(&root, &mut found);
    found.sort();
    let mut code = String::from("pub static CORE_FILES: &[(&str, &str)] = &[\n");
    for path in &found {
        let name = path.strip_prefix(&root).expect("inside the folder").to_string_lossy().replace('\\', "/");
        code.push_str(&format!("    ({name:?}, include_str!({:?})),\n", path.to_string_lossy()));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("core_files.rs"), code).expect("write core_files.rs");
}
