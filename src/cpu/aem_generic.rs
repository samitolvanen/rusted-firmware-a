// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use log::info;

use super::Cpu;

pub struct AemGeneric;

impl Cpu for AemGeneric {
    const MIDR: u64 = 0x410f_d0f0;

    // Naked functions can work too.
    #[unsafe(naked)]
    extern "C" fn reset_handler() {
        core::arch::naked_asm!("ret");
    }

    extern "C" fn power_down_level0() {
        info!("AemGeneric::power_down_level0");
    }

    extern "C" fn power_down_level1() {
        info!("AemGeneric::power_down_level1");
    }
}
