// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! FEAT_PAN introduces a bit to PSTATE. When the value of this PAN state bit is 1, any privileged
//! data access from EL1, or EL2 when the Effective value of HCR_EL2.E2H is 1, to a virtual memory
//! address that is accessible to data accesses at EL0, generates a Permission fault.

use super::CpuExtension;
use arm_sysregs::read_id_aa64mmfr1_el1;

pub struct Pan;

impl CpuExtension for Pan {
    fn is_present(&self) -> bool {
        read_id_aa64mmfr1_el1().is_feat_pan_present()
    }
}
