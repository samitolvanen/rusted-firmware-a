// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Controls lower EL access to Trace filter control registers.

use super::CpuExtension;

use crate::context::{CpuContext, World};

use arm_sysregs::{IdAa64dfr0El1, MdcrEl3, read_id_aa64dfr0_el1};

pub struct TraceFiltering;

impl CpuExtension for TraceFiltering {
    fn is_present(&self) -> bool {
        !(read_id_aa64dfr0_el1() & IdAa64dfr0El1::TRACE_FILT_MASK).is_empty()
    }

    fn configure_per_cpu(&self, world: World, ctx: &mut CpuContext) {
        match world {
            World::NonSecure => {
                // Allow access of trace filter control registers from NS-EL2
                // and NS-EL1 when NS-EL2 is implemented but not used
                ctx.el3_state.mdcr_el3 -= MdcrEl3::STE;
            }
            World::Secure => {
                // Trace prohibited in Secure state unless overridden by the
                // IMPLEMENTATION DEFINED authentication interface.
                ctx.el3_state.mdcr_el3 -= MdcrEl3::TTRF;
            }
            #[cfg(feature = "rme")]
            World::Realm => {
                // Trace prohibited in Realm state, unless overridden by the
                // IMPLEMENTATION DEFINED authentication interface.
                ctx.el3_state.mdcr_el3 |= CptrEl3::RLTE;
            }
        }
    }
}
