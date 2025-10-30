// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Controls Virtualization host extensions.

#[cfg(feature = "sel2")]
mod vhe_sel2;

use super::CpuExtension;

use crate::context::{CpuContext, World};

use arm_sysregs::{IdAa64mmfr1El1, read_id_aa64mmfr1_el1};

pub struct VirtualizationHost;

impl CpuExtension for VirtualizationHost {
    fn is_present(&self) -> bool {
        read_id_aa64mmfr1_el1().contains(IdAa64mmfr1El1::VHE)
    }

    #[cfg(feature = "sel2")]
    fn save_context(&self, world: World) {
        vhe_sel2::save_context(world);
    }

    #[cfg(feature = "sel2")]
    fn restore_context(&self, world: World) {
        vhe_sel2::restore_context(world);
    }
}
