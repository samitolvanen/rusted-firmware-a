// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use log::info;

use super::Cpu;

pub struct QemuMax;

/// Safety: The reset handler is implemented as a naked function and does not clobber any registers.
unsafe impl Cpu for QemuMax {
    const MIDR: u64 = 0x000f_0510;

    #[unsafe(naked)]
    extern "C" fn reset_handler() {
        core::arch::naked_asm!("ret");
    }

    fn power_down_level0() {
        info!("QemuMax::power_down_level0");
    }

    fn power_down_level1() {
        info!("QemuMax::power_down_level1");
    }
}
