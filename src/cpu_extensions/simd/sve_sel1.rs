// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! SVE context management for when Secure EL2 is not enabled.

use crate::{
    context::{CPU_DATA_CONTEXT_NUM, PerCoreState, PerWorld, World},
    platform::{Platform, PlatformImpl, exception_free},
};
use core::{arch::asm, cell::RefCell};
use percore::{ExceptionLock, PerCore};

#[repr(C)]
pub struct SveCpuContext {
    vectors: [[u128; 16]; 32], // TODO: [u128; 16] is the MAX capacity. Allow platforms to adjust.
    // predicates: [u128; 256 / 8], // TODO: should be [u128; SVE_VECTOR_LEN_BYTES / 8]
    // ffr: [u8; 256 / 8], // TODO: should be [u8; SVE_VECTOR_LEN_BYTES / 8]
    fpsr: u64,
    fpcr: u64,
}

impl SveCpuContext {
    const EMPTY: Self = Self {
        vectors: [[u128; 16]; 32],
        fpsr: 0,
        fpcr: 0,
    };

    /// Saves the 32 SIMD/FP (Q) registers using the optimized ARM64 STP (Store Pair) instruction
    /// with post-indexing and saves the FP state registers.
    pub fn save(&mut self) {
        // Get a mutable pointer to the start of the vector storage.
        let dest = self.vectors.as_mut_ptr();
        let fpsr_value;
        let fpcr_value;

        // SAFETY: `dest` is a 16B aligned valid pointer to a 32 * 32 byte array.
        unsafe {
            asm!(
                // The instructions save 32 bytes (Qx and Qy) per line and advance the pointer by
                // 32 bytes.
                ".arch_extension fp",

                ".arch_extension sve;"
                "str z0, [{dest}, #0, MUL VL];"
                "str z1, [{dest}, #1, MUL VL];"
                "str z2, [{dest}, #2, MUL VL];"
                "str z3, [{dest}, #3, MUL VL];"
                "str z4, [{dest}, #4, MUL VL];"
                "str z5, [{dest}, #5, MUL VL];"
                "str z6, [{dest}, #6, MUL VL];"
                "str z7, [{dest}, #7, MUL VL];"
                "str z8, [{dest}, #8, MUL VL];"
                "str z9, [{dest}, #9, MUL VL];"
                "str z10, [{dest}, #10, MUL VL];"
                "str z11, [{dest}, #11, MUL VL];"
                "str z12, [{dest}, #12, MUL VL];"
                "str z13, [{dest}, #13, MUL VL];"
                "str z14, [{dest}, #14, MUL VL];"
                "str z15, [{dest}, #15, MUL VL];"
                "str z16, [{dest}, #16, MUL VL];"
                "str z17, [{dest}, #17, MUL VL];"
                "str z18, [{dest}, #18, MUL VL];"
                "str z19, [{dest}, #19, MUL VL];"
                "str z20, [{dest}, #20, MUL VL];"
                "str z21, [{dest}, #21, MUL VL];"
                "str z22, [{dest}, #22, MUL VL];"
                "str z23, [{dest}, #23, MUL VL];"
                "str z24, [{dest}, #24, MUL VL];"
                "str z25, [{dest}, #25, MUL VL];"
                "str z26, [{dest}, #26, MUL VL];"
                "str z27, [{dest}, #27, MUL VL];"
                "str z28, [{dest}, #28, MUL VL];"
                "str z29, [{dest}, #29, MUL VL];"
                "str z30, [{dest}, #30, MUL VL];"
                "str z31, [{dest}, #31, MUL VL];"
                ".arch_extension nosve"

                "mrs {fpsr_value}, fpsr",
                "mrs {fpcr_value}, fpcr",
                ".arch_extension nofp",
                // inout because stp instructions advance the pointer.
                dest = inout(reg) dest => _,
                fpsr_value = out(reg) fpsr_value,
                fpcr_value = out(reg) fpcr_value,
            );
        }

        self.fpsr = fpsr_value;
        self.fpcr = fpcr_value;
    }

    /// Restores the 32 SIMD/FP (Q) registers using the optimized ARM64 LDP (Load Pair) instruction
    /// with post-indexing and restores the FP state registers.
    pub fn restore(&self) {
        // Get a pointer to the start of the vector storage.
        let src = self.vectors.as_ptr();

        // SAFETY: `src` is a 16B aligned valid pointer to a 32 * 32 byte array.
        unsafe {
            asm!(
                // The instructions load 32 bytes (Qx and Qy) per line and advance the pointer by
                // 32 bytes.
                ".arch_extension fp",

                ".arch_extension sve;"
                "ldr z0, [{src}, #0, MUL VL];"
                "ldr z1, [{src}, #1, MUL VL];"
                "ldr z2, [{src}, #2, MUL VL];"
                "ldr z3, [{src}, #3, MUL VL];"
                "ldr z4, [{src}, #4, MUL VL];"
                "ldr z5, [{src}, #5, MUL VL];"
                "ldr z6, [{src}, #6, MUL VL];"
                "ldr z7, [{src}, #7, MUL VL];"
                "ldr z8, [{src}, #8, MUL VL];"
                "ldr z9, [{src}, #9, MUL VL];"
                "ldr z10, [{src}, #10, MUL VL];"
                "ldr z11, [{src}, #11, MUL VL];"
                "ldr z12, [{src}, #12, MUL VL];"
                "ldr z13, [{src}, #13, MUL VL];"
                "ldr z14, [{src}, #14, MUL VL];"
                "ldr z15, [{src}, #15, MUL VL];"
                "ldr z16, [{src}, #16, MUL VL];"
                "ldr z17, [{src}, #17, MUL VL];"
                "ldr z18, [{src}, #18, MUL VL];"
                "ldr z19, [{src}, #19, MUL VL];"
                "ldr z20, [{src}, #20, MUL VL];"
                "ldr z21, [{src}, #21, MUL VL];"
                "ldr z22, [{src}, #22, MUL VL];"
                "ldr z23, [{src}, #23, MUL VL];"
                "ldr z24, [{src}, #24, MUL VL];"
                "ldr z25, [{src}, #25, MUL VL];"
                "ldr z26, [{src}, #26, MUL VL];"
                "ldr z27, [{src}, #27, MUL VL];"
                "ldr z28, [{src}, #28, MUL VL];"
                "ldr z29, [{src}, #29, MUL VL];"
                "ldr z30, [{src}, #30, MUL VL];"
                "ldr z31, [{src}, #31, MUL VL];"
                ".arch_extension nosve"

                "msr fpsr, {fpsr_value}",
                "msr fpcr, {fpcr_value}",
                ".arch_extension nofp",
                // inout because ldp instructions advance the pointer.
                src = inout(reg) src => _,
                fpsr_value = in(reg) self.fpsr,
                fpcr_value = in(reg) self.fpcr,
            );
        }
    }
}

pub static SVE_CTX: PerCoreState<PerWorld<SveCpuContext>> = PerCore::new(
    [const {
        ExceptionLock::new(RefCell::new(PerWorld(
            [SveCpuContext::EMPTY; CPU_DATA_CONTEXT_NUM],
        )))
    }; PlatformImpl::CORE_COUNT],
);
