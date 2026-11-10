//! On macOS, link clang's compiler runtime (`libclang_rt.osx.a`).
//!
//! ONNX Runtime 1.31's prebuilt library has CoreML code built from
//! Objective-C++ whose `@available` checks call `___isPlatformVersionAtLeast`.
//! A C or Objective-C program gets that from the compiler runtime the clang
//! driver links by default, but rustc links with `-nodefaultlibs`. Rust's std
//! carries its own copy, which is why a debug build links, but a release build
//! with LTO keeps only what Rust code uses, so nothing defined it and the
//! macOS release failed to link. The runtime's copy is only pulled in when the
//! symbol is otherwise missing, so a debug build is unchanged.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CC");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let clang = std::env::var("CC")
        .ok()
        .filter(|cc| !cc.trim().is_empty())
        .unwrap_or_else(|| "clang".to_owned());
    let runtime_dir = Command::new(&clang)
        .arg("-print-resource-dir")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join("lib/darwin"));
    match runtime_dir {
        Some(dir) if dir.join("libclang_rt.osx.a").is_file() => {
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        _ => println!(
            "cargo:warning=libclang_rt.osx.a not found through `{clang} -print-resource-dir`; \
             a release build may fail to link ONNX Runtime (___isPlatformVersionAtLeast)"
        ),
    }
}
