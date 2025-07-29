// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Build script for RF-A.

mod platforms;

use platforms::{PLATFORMS, get_builder};
use std::env;

fn setup_linker(platform: &String) {
    println!("cargo:rustc-link-arg=-Tbl31.ld");
    println!("cargo:rerun-if-changed=bl31.ld");

    // Select the linker scripts. bl31.ld is common to all platforms. It gets supplemented by the
    // platform linker script. Some platforms have multiple linker scripts, depending on the enabled
    // features.
    let linker_name = platform.clone();
    #[cfg(feature = "rme")]
    let linker_name = linker_name + "-rme";

    let linker_name = format!("platforms/{}/{}.ld", platform, linker_name);
    println!("cargo:rustc-link-arg=-T{}", linker_name);
    println!("cargo:rerun-if-changed={}", linker_name);
}

fn main() {
    println!(
        "cargo::rustc-check-cfg=cfg(platform, values(\"{}\"))",
        PLATFORMS.join("\", \""),
    );

    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "none" {
        let platform = env::var("CARGO_CFG_PLATFORM").expect("Missing platform name");

        let builder = get_builder(&platform).unwrap();

        setup_linker(&platform);

        builder.configure_build().unwrap();
    }
}
