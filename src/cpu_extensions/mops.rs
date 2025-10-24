// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! FEAT_MOPS provides instructions that perform a memory copy or memory set, and introduces Memory
//! Copy and Memory Set exceptions.

use super::CpuExtension;
use arm_sysregs::{HcrxEl2, read_hcrx_el2, read_id_aa64isar2_el1, write_hcrx_el2};

pub struct Mops;

impl CpuExtension for Mops {
    fn is_present(&self) -> bool {
        read_id_aa64isar2_el1().is_feat_mops_present()
    }

    fn init(&self) {
        // TODO: Check if HCX is enabled.
        write_hcrx_el2(read_hcrx_el2() | HcrxEl2::MSCEn);
    }
}
