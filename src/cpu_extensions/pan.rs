// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! FEAT_PAN introduces a bit to PSTATE. When the value of this PAN state bit is 1, any privileged
//! data access from EL1, or EL2 when the Effective value of HCR_EL2.E2H is 1, to a virtual memory
//! address that is accessible to data accesses at EL0, generates a Permission fault.

use super::CpuExtension;
use crate::exceptions::is_tge_enabled;
use arm_sysregs::{
    ExceptionLevel, SctlrEl1, SctlrEl2, Spsr, read_id_aa64mmfr1_el1, read_sctlr_el1, read_sctlr_el2,
};

pub struct Pan;

impl CpuExtension for Pan {
    fn is_present(&self) -> bool {
        read_id_aa64mmfr1_el1().is_feat_pan_present()
    }

    fn set_spsr_exception_ret(
        &self,
        new_spsr: &mut Spsr,
        old_spsr: Spsr,
        target_el: ExceptionLevel,
    ) {
        *new_spsr |= old_spsr & Spsr::PAN;

        if self.is_present() {
            match target_el {
                ExceptionLevel::El1 => {
                    if !read_sctlr_el1().contains(SctlrEl1::SPAN) {
                        *new_spsr |= Spsr::PAN;
                    }
                }
                ExceptionLevel::El2 => {
                    if !read_sctlr_el2().contains(SctlrEl2::SPAN) && is_tge_enabled() {
                        *new_spsr |= Spsr::PAN;
                    }
                }
                _ => {}
            }
        }
    }
}
