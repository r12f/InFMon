// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Riff
//
// Build script for infmon-frontend.
// Compiles the VAPI client C code and links against libvapiclient.

use std::path::PathBuf;

fn main() {
    // Find the generated VAPI header.
    // In the build tree it lives under build/generated/.
    // The CMake build must have already run to generate infmon.api.vapi.h.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let generated_dir = repo_root.join("build").join("generated");

    if !generated_dir.join("infmon.api.vapi.h").exists() {
        // If the VAPI header doesn't exist, skip C compilation.
        // This allows `cargo check` / `cargo test` to work without VPP.
        println!("cargo:warning=infmon.api.vapi.h not found — skipping VAPI FFI build");
        println!("cargo:warning=Stats client will use stub implementation");
        return;
    }

    // Check for libvapiclient
    let has_vapi = std::process::Command::new("pkg-config")
        .args(["--exists", "vapiclient"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !has_vapi {
        // Fallback: direct library check. Hardcoded to aarch64 because InFMon
        // exclusively targets NVIDIA BF3 (aarch64 DPU). The pkg-config path
        // above handles the general case; this is a last resort for build
        // environments where pkg-config is not configured.
        let lib_path = PathBuf::from("/usr/lib/aarch64-linux-gnu/libvapiclient.so");
        if !lib_path.exists() {
            println!("cargo:warning=libvapiclient not found — skipping VAPI FFI build");
            return;
        }
    }

    cc::Build::new()
        .file("src/vapi_ffi/infmon_vapi_client.c")
        .include(&generated_dir)
        .include("/usr/include")
        // Suppress warnings from VPP headers
        .flag("-Wno-pedantic")
        .flag("-Wno-unused-parameter")
        .flag("-Wno-sign-compare")
        .compile("infmon_vapi_client");

    println!("cargo:rustc-link-lib=dylib=vapiclient");
    println!("cargo:rustc-link-lib=dylib=vppinfra");
    println!("cargo:rustc-link-lib=dylib=vlibmemoryclient");
    println!("cargo:rustc-link-lib=dylib=svm");
    println!("cargo:rustc-cfg=feature=\"vapi\"");
    println!("cargo:rerun-if-changed=src/vapi_ffi/infmon_vapi_client.c");
    println!(
        "cargo:rerun-if-changed={}",
        generated_dir.join("infmon.api.vapi.h").display()
    );
}
