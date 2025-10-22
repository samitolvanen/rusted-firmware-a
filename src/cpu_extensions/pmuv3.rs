// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use arm_sysregs::{
    IdAa64dfr0El1, MdcrEl3, Pmcr, read_id_aa64dfr0_el1, read_pmcr_el0, write_pmcr_el0,
};

use crate::{context::CpuContext, cpu_extensions::CpuExtension};

pub struct MultiThreadedPmu;

impl CpuExtension for MultiThreadedPmu {
    fn is_present(&self) -> bool {
        read_id_aa64dfr0_el1().is_feat_mtpmu_present()
    }

    fn configure_per_cpu(&self, _world: crate::context::World, ctx: &mut CpuContext) {
        ctx.el3_state.mdcr_el3 -= MdcrEl3::MTPMU;
    }
}

/// Initialise PMCR_EL0 setting all fields rather than relying
/// on hw. Some fields are architecturally UNKNOWN on reset.
pub fn init() {
    // PMCR_EL0.DP: Set to one so that the cycle counter,
    // PMCCNTR_EL0 does not count when event counting is prohibited.
    // Necessary on PMUv3 <= p7 where MDCR_EL3.{SCCD,MCCD} are not
    // available
    //
    // PMCR_EL0.X: Set to zero to disable export of events.
    //
    // PMCR_EL0.C: Set to one to reset PMCCNTR_EL0 to zero.
    //
    // PMCR_EL0.P: Set to one to reset each event counter PMEVCNTR<n>_EL0 to
    //  zero.
    //
    // PMCR_EL0.E: Set to zero to disable cycle and event counters.
    let mut pmcr_el0 = read_pmcr_el0();
    pmcr_el0 |= Pmcr::DP | Pmcr::C | Pmcr::P;
    pmcr_el0 -= Pmcr::X | Pmcr::E;
    write_pmcr_el0(pmcr_el0);
}

pub fn configure_per_cpu(ctx: &mut CpuContext) {
    #[cfg(feature = "sel2")]
    {
        // TODO
        // 	mdcr_el2_val = read_el2_ctx_common(get_el2_sysregs_ctx(ctx), mdcr_el2);
        // 	mdcr_el2_val = init_mdcr_el2_hpmn(mdcr_el2_val);
        // 	write_el2_ctx_common(get_el2_sysregs_ctx(ctx), mdcr_el2, mdcr_el2_val);
    }

    // MDCR_EL3.MPMX: Set to zero to not affect event counters (when
    // SPME = 0).
    //
    // MDCR_EL3.MCCD: Set to one so that cycle counting by PMCCNTR_EL0 is
    //  prohibited in EL3. This bit is RES0 in versions of the
    //  architecture with FEAT_PMUv3p7 not implemented.
    //
    // MDCR_EL3.SCCD: Set to one so that cycle counting by PMCCNTR_EL0 is
    //  prohibited in Secure state. This bit is RES0 in versions of the
    //  architecture with FEAT_PMUv3p5 not implemented.
    //
    // MDCR_EL3.SPME: Set to zero so that event counting is prohibited in
    //  Secure state (and explicitly EL3 with later revisions). If ARMv8.2
    //  Debug is not implemented this bit does not have any effect on the
    //  counters unless there is support for the implementation defined
    //  authentication interface ExternalSecureNoninvasiveDebugEnabled().
    //
    // The SPME/MPMX combination is a little tricky. Below is a small
    // summary if another combination is ever needed:
    // SPME | MPMX | secure world |   EL3
    // -------------------------------------
    //   0  |  0   |    disabled  | disabled
    //   1  |  0   |    enabled   | enabled
    //   0  |  1   |    enabled   | disabled
    //   1  |  1   |    enabled   | disabled only for counters 0 to
    //                              MDCR_EL2.HPMN - 1. Enabled for the rest
    //
    // MDCR_EL3.TPM: Set to zero so that EL0, EL1, and EL2 System register
    //  accesses to all Performance Monitors registers do not trap to EL3.
    ctx.el3_state.mdcr_el3 |= MdcrEl3::SCCD | MdcrEl3::MCCD;
    ctx.el3_state.mdcr_el3 -= MdcrEl3::MPMX | MdcrEl3::SPME | MdcrEl3::TPM;
}
