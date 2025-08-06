// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::aarch64::{disable_dcache, flush_all_caches, flush_level1_cache};
use super::Cpu;

pub struct QemuMax;

/// Safety: The reset handler is implemented as a naked function and does not clobber any registers.
unsafe impl Cpu for QemuMax {
    const MIDR: u64 = 0x000f_0510;

    #[unsafe(naked)]
    extern "C" fn reset_handler() {
        core::arch::naked_asm!("ret");
    }

    /// Disables data cache and flushes level 1 cache.
    fn power_down_level0() {
        // Safety: The following function calls flush the required levels of caches to avoid data
        // loss before powering down the core.
        unsafe {
            disable_dcache();
        }
        flush_level1_cache();
    }

    /// Disables data cache and flushes all cache levels.
    fn power_down_level1() {
        // Safety: The following function calls flush the required levels of caches to avoid data
        // loss before powering down the core.
        unsafe {
            disable_dcache();
        }
        flush_all_caches();
    }
}
