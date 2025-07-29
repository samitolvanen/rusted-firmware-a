// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use super::Builder;

pub struct QemuBuilder;

impl Builder for QemuBuilder {}

impl QemuBuilder {
    pub const PLAT_NAME: &str = "qemu";
}
