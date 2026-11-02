//! macOS: ONNX Runtime's CoreML code uses `@available`, which needs
//! `___isPlatformVersionAtLeast` from clang's runtime. Rust's own builtins do
//! not provide it in LTO (release) builds, so link `libclang_rt.osx.a`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = Command::new("xcrun")
        .args(["clang", "--print-runtime-dir"])
        .output()
        .expect("xcrun clang (Xcode Command Line Tools) is required on macOS");
    let dir = String::from_utf8(out.stdout).expect("clang runtime dir is not UTF-8");
    println!("cargo:rustc-link-search=native={}", dir.trim());
    println!("cargo:rustc-link-lib=static=clang_rt.osx");
}
