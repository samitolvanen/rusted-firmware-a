// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! SIMD support.

#[cfg(not(feature = "sel2"))]
mod simd_sel1;

use super::CpuExtension;

use crate::{
    context::{CPU_DATA_CONTEXT_NUM, PerCoreState, PerWorld, PerWorldContext, World},
    platform::{Platform, PlatformImpl, exception_free},
};

use arm_sysregs::CptrEl3;
use core::{arch::asm, cell::RefCell};
use percore::{ExceptionLock, PerCore};

/// Enables FP/SIMD register access for all worlds.
///
/// If `sel2` is enabled, S-EL2 is responsible for FP/SIMD context management.
/// Otherwise, the context management is performed in EL3.
pub struct Simd;

impl CpuExtension for Simd {
    fn is_present(&self) -> bool {
        // FP is mandatory.
        true
    }

    fn configure_per_world(&self, _world: World, ctx: &mut PerWorldContext) {
        // Allow FP/SIMD register accesses in every World.
        ctx.cptr_el3 -= CptrEl3::TFP;
    }

    #[cfg(not(feature = "sel2"))]
    fn save_context(&self, world: World) {
        exception_free(|token| {
            let ctx = &mut simd_sel1::SIMD_CTX.get().borrow_mut(token)[world];

            ctx.save();
        })
    }

    #[cfg(not(feature = "sel2"))]
    fn restore_context(&self, world: World) {
        exception_free(|token| {
            let ctx = &simd_sel1::SIMD_CTX.get().borrow_mut(token)[world];

            ctx.restore();
        })
    }
}

/// TODO: SVE is a superset of FP. What to do if SVE is supported by platform but not enabled by hardware? fallback to FP?
pub struct Sve;

impl CpuExtension for Sve {
    fn is_present(&self) -> bool {
        read_id_aa64pfr0_el1().is_feat_sve_present()
    }

    fn configure_per_world(&self, _world: World, ctx: &mut PerWorldContext) {
        // Allow SVE register access in every world.
        ctx.cptr_el3 |= CptrEl3::EZ;
    }

    #[cfg(not(feature = "sel2"))]
    fn save_context(&self, world: World) {
        // exception_free(|token| {
        //     let ctx = &mut simd_sel1::SIMD_CTX.get().borrow_mut(token)[world];

        //     ctx.save();
        // })
    }

    #[cfg(not(feature = "sel2"))]
    fn restore_context(&self, world: World) {
        // exception_free(|token| {
        //     let ctx = &simd_sel1::SIMD_CTX.get().borrow_mut(token)[world];

        //     ctx.restore();
        // })
    }
}
