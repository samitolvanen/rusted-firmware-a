// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! FEAT_HCX introduces the Extended Hypervisor Configuration Register, HCRX_EL2, that provides
//! configuration controls for virtualization in addition to those provided by HCR_EL2, including
//! defining whether various operations are trapped to EL2.

use super::CpuExtension;

use crate::context::{CpuContext, World};
#[cfg(feature = "sel2")]
use crate::{
    context::{CPU_DATA_CONTEXT_NUM, PerCoreState, PerWorld},
    platform::{Platform, PlatformImpl, exception_free},
};

#[cfg(feature = "sel2")]
use arm_sysregs::read_hcrx_el2;
use arm_sysregs::{HcrxEl2, ScrEl3, read_id_aa64mmfr1_el1, write_hcrx_el2};
#[cfg(feature = "sel2")]
use core::cell::RefCell;
#[cfg(feature = "sel2")]
use percore::{ExceptionLock, PerCore};

#[cfg(feature = "sel2")]
struct HcxCpuContext {
    hcrx_el2: HcrxEl2,
}

#[cfg(feature = "sel2")]
impl HcxCpuContext {
    const EMPTY: Self = Self {
        hcrx_el2: HcrxEl2::empty(),
    };
}

#[cfg(feature = "sel2")]
static HCX_CTX: PerCoreState<PerWorld<HcxCpuContext>> = PerCore::new(
    [const {
        ExceptionLock::new(RefCell::new(PerWorld(
            [HcxCpuContext::EMPTY; CPU_DATA_CONTEXT_NUM],
        )))
    }; PlatformImpl::CORE_COUNT],
);

pub struct Hcx;

impl CpuExtension for Hcx {
    fn is_present(&self) -> bool {
        read_id_aa64mmfr1_el1().is_feat_hcx_present()
    }

    fn init(&self) {
        // Initialize register HCRX_EL2 to all-zero.
        // As the value of HCRX_EL2 is UNKNOWN on reset, there is a chance that this can lead to
        // unexpected behavior in lower ELs that have not been updated since the introduction of
        // this feature if not properly initialized, especially when it comes to those bits that
        // enable/disable traps.
        write_hcrx_el2(HcrxEl2::empty());
    }

    fn configure_per_cpu(&self, _world: World, context: &mut CpuContext) {
        context.el3_state.scr_el3 |= ScrEl3::HXEN;
    }

    #[cfg(feature = "sel2")]
    fn save_context(&self, world: World) {
        if self.is_present() {
            exception_free(|token| {
                HCX_CTX.get().borrow_mut(token)[world].hcrx_el2 = read_hcrx_el2();
            })
        }
    }

    #[cfg(feature = "sel2")]
    fn restore_context(&self, world: World) {
        if self.is_present() {
            exception_free(|token| {
                write_hcrx_el2(HCX_CTX.get().borrow_mut(token)[world].hcrx_el2);
            })
        }
    }
}
