//! Embeds `templates/` and `static/` into the binary, so the panel needs no
//! files beside it at run time.

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("readable directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let mut code = String::new();
    for (name, dir, macro_name, kind) in [
        ("TEMPLATES", "templates", "include_str", "&str"),
        ("ASSETS", "static", "include_bytes", "&[u8]"),
    ] {
        let base = root.join(dir);
        println!("cargo:rerun-if-changed={}", base.display());
        writeln!(code, "pub static {name}: &[(&str, {kind})] = &[").unwrap();
        for path in files(&base) {
            let relative = path
                .strip_prefix(&base)
                .expect("inside the directory")
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            writeln!(
                code,
                "    ({relative:?}, {macro_name}!({:?})),",
                path.display().to_string()
            )
            .unwrap();
        }
        writeln!(code, "];").unwrap();
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("out dir")).join("embedded.rs");
    fs::write(out, code).expect("writable OUT_DIR");
}
