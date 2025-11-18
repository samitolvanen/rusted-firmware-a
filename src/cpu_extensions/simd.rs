// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! SIMD support.

use super::CpuExtension;

use crate::{
    context::{CPU_DATA_CONTEXT_NUM, PerCoreState, PerWorld, PerWorldContext, World},
    platform::{Platform, PlatformImpl, exception_free},
};

use arm_sysregs::CptrEl3;
use core::{arch::asm, cell::RefCell};
use percore::{ExceptionLock, PerCore};

#[repr(C)]
struct SimdCpuContext {
    vectors: [u128; 32],
    fpsr: u64,
    fpcr: u64,
}

impl SimdCpuContext {
    const EMPTY: Self = Self {
        vectors: [0; 32],
        fpsr: 0,
        fpcr: 0,
    };
}

static SIMD_CTX: PerCoreState<PerWorld<SimdCpuContext>> = PerCore::new(
    [const {
        ExceptionLock::new(RefCell::new(PerWorld(
            [SimdCpuContext::EMPTY; CPU_DATA_CONTEXT_NUM],
        )))
    }; PlatformImpl::CORE_COUNT],
);

fn fpregs_state_save(regs: &mut SimdCpuContext) {
    let fpsr_value;
    let fpcr_value;

    // SAFETY: This asm does not access memory.
    unsafe {
        asm!(
            ".arch_extension fp",
            "mrs {fpsr_value}, fpsr",
            "mrs {fpcr_value}, fpcr",
            ".arch_extension nofp",
            fpsr_value = out(reg) fpsr_value,
            fpcr_value = out(reg) fpcr_value,
        );
    }

    regs.fpsr = fpsr_value;
    regs.fpcr = fpcr_value;
}

fn fpregs_state_restore(regs: &SimdCpuContext) {
    // SAFETY: This asm does not access memory.
    unsafe {
        asm!(
            ".arch_extension fp",
            "msr fpsr, {fpsr_value}",
            "msr fpcr, {fpcr_value}",
            ".arch_extension nofp",
            fpsr_value = in(reg) regs.fpsr,
            fpcr_value = in(reg) regs.fpcr,
        );
    }
}

/// Saves the 32 SIMD/FP (Q) registers in the SimdRegs structure
/// using the optimized ARM64 STP (Store Pair) instruction with post-indexing.
fn fpregs_context_save(regs: &mut SimdCpuContext) {
    // Get a mutable pointer to the start of the vector storage.
    let dest = regs.vectors.as_mut_ptr();

    // The instructions save 32 bytes (Qx and Qy) per line and advance the pointer by 32.
    // SAFETY: `dest` is a 16B aligned valid pointer to a 32 * 32 byte array.
    unsafe {
        asm!(
            ".arch_extension fp",
            "stp q0, q1, [{dest}], #32",
            "stp q2, q3, [{dest}], #32",
            "stp q4, q5, [{dest}], #32",
            "stp q6, q7, [{dest}], #32",
            "stp q8, q9, [{dest}], #32",
            "stp q10, q11, [{dest}], #32",
            "stp q12, q13, [{dest}], #32",
            "stp q14, q15, [{dest}], #32",
            "stp q16, q17, [{dest}], #32",
            "stp q18, q19, [{dest}], #32",
            "stp q20, q21, [{dest}], #32",
            "stp q22, q23, [{dest}], #32",
            "stp q24, q25, [{dest}], #32",
            "stp q26, q27, [{dest}], #32",
            "stp q28, q29, [{dest}], #32",
            "stp q30, q31, [{dest}], #32",
            ".arch_extension nofp",
            dest = in(reg) dest,
        );
    }
}

/// Restores the 32 SIMD/FP (Q) registers from the SimdRegs structure
/// using the optimized ARM64 LDP (Load Pair) instruction with post-indexing.
#[cfg(target_arch = "aarch64")]
fn fpregs_context_restore(regs: &SimdCpuContext) {
    // Get a pointer to the start of the vector storage.
    let src = regs.vectors.as_ptr();

    // The instructions load 32 bytes (Qx and Qy) per line and advance the pointer by 32.
    // SAFETY: `src` is a 16B aligned valid pointer to a 32 * 32 byte array.
    unsafe {
        asm!(
            ".arch_extension fp",
            "ldp q0, q1, [{src}], #32",
            "ldp q2, q3, [{src}], #32",
            "ldp q4, q5, [{src}], #32",
            "ldp q6, q7, [{src}], #32",
            "ldp q8, q9, [{src}], #32",
            "ldp q10, q11, [{src}], #32",
            "ldp q12, q13, [{src}], #32",
            "ldp q14, q15, [{src}], #32",
            "ldp q16, q17, [{src}], #32",
            "ldp q18, q19, [{src}], #32",
            "ldp q20, q21, [{src}], #32",
            "ldp q22, q23, [{src}], #32",
            "ldp q24, q25, [{src}], #32",
            "ldp q26, q27, [{src}], #32",
            "ldp q28, q29, [{src}], #32",
            "ldp q30, q31, [{src}], #32",
            ".arch_extension nofp",
            src = in(reg) src,
        );
    }
}

pub struct Simd;

impl CpuExtension for Simd {
    fn is_present(&self) -> bool {
        true
    }

    fn configure_per_world(&self, _world: World, ctx: &mut PerWorldContext) {
        ctx.cptr_el3 -= CptrEl3::TFP;
    }

    fn save_context(&self, world: World) {
        exception_free(|token| {
            let ctx = &mut SIMD_CTX.get().borrow_mut(token)[world];

            fpregs_context_save(ctx);
            fpregs_state_save(ctx);
        })
    }

    fn restore_context(&self, world: World) {
        exception_free(|token| {
            let ctx = &SIMD_CTX.get().borrow_mut(token)[world];

            fpregs_state_restore(ctx);
            fpregs_context_restore(ctx);
        })
    }
}
