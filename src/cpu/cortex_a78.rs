// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use super::Cpu;
use log::info;

pub struct CortexA78;

impl Cpu for CortexA78 {
    const MIDR: u64 = 0x410f_d410;

    extern "C" fn reset_handler() {
        info!("CortexA78::reset_handler");
    }

    extern "C" fn power_down_level0() {
        info!("CortexA78::power_down_level0");
    }

    extern "C" fn power_down_level1() {
        info!("CortexA78::power_down_level1");
    }
}
