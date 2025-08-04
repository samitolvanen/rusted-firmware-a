// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::{
    aarch64::{disable_dcache, flush_all_caches, flush_level1_cache, flush_level2_cache},
    sysregs::{CacheLevel, CacheType, read_clidr_el1},
};

use super::Cpu;
use core::arch::naked_asm;

pub struct AemGeneric;

/// The AEM FVP requires cache maintenance operations on CPU/Cluster power down, because it does not
/// implement DynamIQ Shared Unit (DSU).
///
/// Safety: The reset handler is implemented as a naked function and does not clobber any registers.
unsafe impl Cpu for AemGeneric {
    const MIDR: u64 = 0x410f_d0f0;

    #[unsafe(naked)]
    extern "C" fn reset_handler() {
        naked_asm!("ret");
    }

    /// Disables data cache, flushes level 1 and also flushes level 2 cache if level 3 cache is
    /// present.
    fn power_down_level0() {
        // Safety: The following function calls flush the required levels of caches to avoid data
        // loss before powering down the core.
        // The exclusive memory accesses are predictable with AEM FVP's cache implementation.
        unsafe {
            disable_dcache();
        }

        // Safety: The exclusive memory accesses are predictable with AEM FVP's cache
        // implementation.
        unsafe {
            flush_level1_cache();

            if read_clidr_el1().ctype(CacheLevel::new(3)) != CacheType::NoCache {
                flush_level2_cache();
            }
        }
    }

    /// Disables data cache and flushes level 1, level 2 and level 3 caches if present.
    fn power_down_level1() {
        // Safety: The following function calls flush the required levels of caches to avoid data
        // loss before powering down the core and the cluster.
        // The exclusive memory accesses are predictable with AEM FVP's cache implementation.
        unsafe {
            disable_dcache();
        }

        // Safety: The exclusive memory accesses are predictable with AEM FVP's cache
        // implementation.
        unsafe {
            flush_all_caches();
        }
    }
}
