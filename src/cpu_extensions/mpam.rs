// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Memory System Resource Partitioning and Monitoring (MPAM) extension
//!
//! MPAM extends the ability for software to co-manage runtime resource allocation of memory system
//! components such as caches,interconnects, and memory controllers. These are resources that
//! otherwise would not be controllable by software at this granularity.
//!
//! MPAM achieves this by enabling supervisory software such as an OS or Hypervisor to assign a
//! unique partition identifier to instruction accesses and data memory accesses for each VM or
//! application. These uniquely assigned partition identifiers accompany the memory accesses
//! throughout their lifetime in the memory system. Memory system components use partition
//! identifiers to configure the allocation of resources to a particular VM or application.

#[cfg(feature = "sel2")]
mod mpam_sel2;

use super::CpuExtension;
use crate::context::World;
use arm_sysregs::{Mpam3El3, read_id_aa64pfr0_el1, write_mpam3_el3};

pub struct Mpam;

impl CpuExtension for Mpam {
    fn is_present(&self) -> bool {
        read_id_aa64pfr0_el1().is_feat_mpam_present()
    }

    fn init(&self) {
        // C TFA does this in `configure_per_world` and MPAM3_EL3 is restored before ERET but
        // then it is never modified when entering EL3.
        // TODO: Double-check if we can configure it here once for simplicity.
        write_mpam3_el3(Mpam3El3::MPAMEN)
    }

    #[cfg(feature = "sel2")]
    fn save_context(&self, world: World) {
        if self.is_present() {
            mpam_sel2::save_context(world);
        }
    }

    #[cfg(feature = "sel2")]
    fn restore_context(&self, world: World) {
        if self.is_present() {
            mpam_sel2::restore_context(world);
        }
    }
}
