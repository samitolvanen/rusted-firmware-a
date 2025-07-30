// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Build script for RF-A.

mod platforms;

use cc::Build;
use platforms::{PLATFORMS, get_builder};
use std::env;

fn build_libtfa(platform: &str) {
    let platform_builder = get_builder(platform).unwrap();

    // SAFETY: The build script is single-threaded.
    unsafe {
        env::set_var("CROSS_COMPILE", "aarch64-none-elf");
        env::set_var("CC", "clang");
    }

    let mut build = Build::new();
    if env::var("CARGO_FEATURE_RME").as_deref() == Ok("1") {
        build.define("ENABLE_RME", Some("1"));
    }
    build
        .define("CRASH_REPORTING", Some("1"))
        .define("ENABLE_ASSERTIONS", Some("1"))
        .include("include")
        .include("include/arch/aarch64")
        .include("include/lib/cpus/aarch64")
        .include("include/lib/el3_runtime/aarch64")
        .include("include/lib/libc")
        .include("include/plat/arm/common/aarch64")
        .file("bl31_entrypoint.S")
        .file("cpu_helpers.S")
        .file("cpu_data.S");

    if let Ok(debug) = env::var("DEBUG") {
        build.define("DEBUG", debug.as_str());
    }

    platform_builder.configure_build(&mut build).unwrap();

    build.compile("tfa");
}

fn setup_linker(platform: &String) {
    println!("cargo:rustc-link-arg=-Tbl31.ld");
    println!("cargo:rerun-if-changed=bl31.ld");

    // Select the linker scripts. bl31.ld is common to all platforms. It gets supplemented by the
    // platform linker script. Some platforms have multiple linker scripts, depending on the enabled
    // features.
    let linker_name_base = platform.clone();
    #[cfg(feature = "rme")]
    let linker_name_base = linker_name_base + "-rme";

    let vendor_path = env::var("CARGO_CFG_VENDOR_PATH").unwrap_or_default();

    let mut linker_path = std::path::PathBuf::from("platforms");
    if !vendor_path.is_empty() {
        linker_path.push(vendor_path);
    }
    linker_path.push(platform);
    let linker_file = format!("{}.ld", linker_name_base);
    linker_path.push(linker_file);

    let linker_script = linker_path.to_str().expect("Invalid linker path");

    println!("cargo:rustc-link-arg=-T{}", linker_script);
    println!("cargo:rerun-if-changed={}", linker_script);
}

fn main() {
    println!(
        "cargo::rustc-check-cfg=cfg(platform, values(\"{}\"))",
        PLATFORMS.join("\", \""),
    );
    println!("cargo::rustc-check-cfg=cfg(vendor_path)");

    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "none" {
        let platform = env::var("CARGO_CFG_PLATFORM").expect("Missing platform name");

        build_libtfa(&platform);

        setup_linker(&platform);
    }
}
