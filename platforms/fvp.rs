// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use super::Builder;

pub struct FvpBuilder;

impl FvpBuilder {
    pub const PLAT_NAME: &str = "fvp";

    const BL2_BASE: u64 = 0x4060000;
    const BL31_BASE: u64 = 0x4003000;
    const BL31_SIZE: u64 = Self::BL2_BASE - Self::BL31_BASE;
}

impl Builder for FvpBuilder {
    fn bl31_base(&self) -> u64 {
        Self::BL31_BASE
    }

    fn bl31_size(&self) -> u64 {
        Self::BL31_SIZE
    }
}
