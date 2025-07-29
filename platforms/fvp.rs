// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use super::Builder;

pub struct FvpBuilder;

impl Builder for FvpBuilder {}

impl FvpBuilder {
    pub const PLAT_NAME: &str = "fvp";
}
