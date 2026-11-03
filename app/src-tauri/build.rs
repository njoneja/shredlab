// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

use std::process::Command;

fn main() {
    // The commit the build came from, for the startup log line (About gets
    // the same from vite.config.ts). Empty outside a git checkout.
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    println!("cargo:rustc-env=SHREDLAB_COMMIT={commit}");
    // Rebuild when HEAD moves: a commit, checkout or reset rewrites the ref
    // files or packed-refs, and HEAD itself on a branch switch. Missing
    // paths are skipped: Cargo would rerun this script on every build.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for f in ["HEAD", "refs/heads", "packed-refs"] {
            let path = format!("{dir}/{f}");
            if std::path::Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
    tauri_build::build()
}
