// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Navneet Joneja

//! Builds the C ABI wrapper around the vendored Signalsmith Stretch headers.

fn main() {
    println!("cargo:rerun-if-changed=cpp/stretch_wrapper.cpp");
    println!("cargo:rerun-if-changed=third_party");
    let mut b = cc::Build::new();
    b.cpp(true)
        .file("cpp/stretch_wrapper.cpp")
        .include("third_party")
        // The stretcher is ~10x slower unoptimized; always optimize it, even in debug builds.
        .opt_level(3)
        .warnings(false);
    if b.get_compiler().is_like_msvc() {
        b.flag("/std:c++17").flag("/EHsc");
    } else {
        b.flag("-std=c++14");
    }
    b.compile("shredlab_stretch");
}
