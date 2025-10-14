// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! SVE Extension

// /// The number of contexts to store for each CPU core, one per security state.
// const CPU_DATA_CONTEXT_NUM: usize = if cfg!(feature = "rme") { 3 } else { 2 };

use super::CpuExtension;

use crate::{
    context::{CpuContext, PerCoreState, PerWorld, PerWorldContext, World},
    platform::{Platform, PlatformImpl},
    sysregs::{CptrEl3, ScrEl3},
};
use core::cell::RefCell;
use log::info;
use percore::{ExceptionLock, PerCore};

#[repr(C, align(16))]
pub struct SveCpuContext {
    /// FFR and each of predicates is one-eigth of the SVE vector length
    predicates: [[u8; 16]; PlatformImpl::SVE_VECTOR_LEN / 64],
    ffr: [u8; PlatformImpl::SVE_VECTOR_LEN / 8],
    /// SMCCCv1.3 FID[16] hint bit state recorded on EL3 entry
    hint: bool,
}

impl SveCpuContext {
    const EMPTY: Self = Self {
        predicates: [[0 as u8; 16]; PlatformImpl::SVE_VECTOR_LEN / 64],
        ffr: [0; PlatformImpl::SVE_VECTOR_LEN / 8],
        hint: false,
    };
}  

static SVE_CTX: PerCoreState<PerWorld<SveCpuContext>> = PerCore::new(
    [const {
        ExceptionLock::new(RefCell::new(PerWorld([
            SveCpuContext::EMPTY,
            SveCpuContext::EMPTY,
        ])))
    }; PlatformImpl::CORE_COUNT],
);

/// Returns a raw pointer to the CPU context of the given world on the current core.
fn sve_world_context(world: World) -> *mut SveCpuContext {
    // SAFETY: Getting the `CpuContext` pointer from a `CpuState` pointer requires the `CpuState`
    // pointer to be valid. We know that this is always true, because we get it from
    // `CPU_STATE.get().as_ptr()`. We avoid creating any intermediate references by accessing the
    // field of the `PerWorld` directly rather than using the `IndexMut` implementation.
    unsafe { &raw mut (&mut (*SVE_CTX.get().as_ptr()))[world] }
}

/// Scalable Vector Extension
pub struct Sve;

// const ZCR_EL3_LEN_MASK: u32 = 4;

// impl Sve {
//     pub const fn new() -> Self {
//         // Check if `VECTOR_LEN` fits in `ZCR_EL3_LEN_MASK`.
//         assert!(
//             PlatformImpl::SVE_VECTOR_LEN % 128 == 0
//                 && PlatformImpl::SVE_VECTOR_LEN < 128 * (1 << ZCR_EL3_LEN_MASK),
//             "Invalid SVE vector length"
//         );
//     }
// }

impl CpuExtension for Sve {
    fn configure_per_world(&self, _world: World, ctx: &mut PerWorldContext) {
        ctx.cptr_el3 |= CptrEl3::EZ;
        ctx.cptr_el3 &= !CptrEl3::TFP;
        // TODO: I think zcr_el3 should not be per world.
        ctx.zcr_el3 = (PlatformImpl::SVE_VECTOR_LEN / 128 - 1) as u64;
    }

    fn save_context(&self, _world: World) {
        // TODO
    }

    fn restore_context(&self, _world: World) {
        // TODO
    }
}
