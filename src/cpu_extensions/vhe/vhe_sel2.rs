// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! VHE context management for when Secure EL2 is enabled.

use crate::{
    context::{CPU_DATA_CONTEXT_NUM, PerCoreState, PerWorld, World},
    platform::{Platform, PlatformImpl, exception_free},
};
use arm_sysregs::{read_contextidr_el2, read_ttbr1_el2, write_contextidr_el2, write_ttbr1_el2};
use core::cell::RefCell;
use percore::{ExceptionLock, PerCore};

struct VheCpuContext {
    contextidr_el2: u64,
    ttbr1_el2: u64,
}

impl VheCpuContext {
    const EMPTY: Self = Self {
        contextidr_el2: 0,
        ttbr1_el2: 0,
    };
}

static VHE_CTX: PerCoreState<PerWorld<VheCpuContext>> = PerCore::new(
    [const {
        ExceptionLock::new(RefCell::new(PerWorld(
            [VheCpuContext::EMPTY; CPU_DATA_CONTEXT_NUM],
        )))
    }; PlatformImpl::CORE_COUNT],
);

pub(super) fn save_context(world: World) {
    exception_free(|token| {
        let mut ctx = VHE_CTX.get().borrow_mut(token);

        ctx[world].contextidr_el2 = read_contextidr_el2();
        ctx[world].ttbr1_el2 = read_ttbr1_el2();
    })
}

pub(super) fn restore_context(world: World) {
    exception_free(|token| {
        let ctx = VHE_CTX.get().borrow_mut(token);

        write_contextidr_el2(ctx[world].contextidr_el2);
        write_ttbr1_el2(ctx[world].ttbr1_el2);
    })
}
