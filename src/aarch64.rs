// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

#[cfg(target_arch = "aarch64")]
use core::arch::asm;
use core::ops::RangeInclusive;

use crate::sysregs::{
    CacheLevel, CacheType, CsselrEl1, read_ccsidr_el1, read_clidr_el1, read_id_aa64mmfr2_el1,
    write_csselr_el1,
};

/// Issues a full system (`sy`) data synchronization barrier (`dsb`) instruction.
pub fn dsb_sy() {
    // SAFETY: `dsb` does not violate safe Rust guarantees.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("dsb sy", options(nostack));
    }
}

/// Issues a data synchronization barrier (`dsb`) instruction that applies to the inner shareable
/// domain (`ish`).
pub fn dsb_ish() {
    // SAFETY: `dsb` does not violate safe Rust guarantees.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("dsb ish", options(nostack));
    }
}

/// Issues an instruction synchronization barrier (`isb`) instruction.
pub fn isb() {
    // SAFETY: `isb` does not violate safe Rust guarantees.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("isb", options(nostack));
    }
}

/// Causes an event to be signaled to all cores within a multiprocessor system.
pub fn sev() {
    // SAFETY: `sev` does not violate safe Rust guarantees.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("sev", options(nostack));
    }
}

/// Issues a translation lookaside buffer invalidate (`tlbi`) instruction that invalidates all TLB
/// entries for EL3 (`alle3`).
pub fn tlbi_alle3() {
    // SAFETY: `tlbi` does not violate safe Rust guarantees.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("tlbi alle3", options(nostack));
    }
}

/// Wait For Interrupt is a hint instruction that indicates that the PE can enter a low-power state
/// and remain there until a wakeup event occurs.
pub fn wfi() {
    // SAFETY: `wfi` does not violate safe Rust guarantees.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        asm!("wfi", options(nostack));
    }
}

/// The cache size descriptor.
#[derive(Debug)]
pub struct CacheSize {
    /// (Number of sets in cache) - 1, therefore a value of 0 indicates 1 set in the cache. The
    /// number of sets does not have to be a power of 2.
    pub num_sets: u32,
    /// (Associativity of cache) - 1, therefore a value of 0 indicates an associativity of 1. The
    /// associativity does not have to be a power of 2.
    pub associativity: u32,
    /// (Log2(Number of bytes in cache line)) - 4
    pub line_size_log: u32,
}

/// Reads the cache size description of the given level of cache.
pub fn read_cache_size(level: CacheLevel) -> CacheSize {
    // Select cache level
    write_csselr_el1(CsselrEl1::new(false, level, false));
    isb();

    // Read and extract the fields of CCSIDR_EL1 according to the presence of CCIDX feature.
    let ccsidr = read_ccsidr_el1();

    let (num_sets, associativity) = if read_id_aa64mmfr2_el1().has_64_bit_ccsidr_el1() {
        (
            ((ccsidr >> 32) & 0x00ff_ffff) as u32,
            ((ccsidr >> 3) & 0x001f_ffff) as u32,
        )
    } else {
        (
            ((ccsidr >> 13) & 0x0000_7fff) as u32,
            ((ccsidr >> 3) & 0x0000_03ff) as u32,
        )
    };

    let line_size_log = (ccsidr & 0b111) as u32;

    CacheSize {
        num_sets,
        associativity,
        line_size_log,
    }
}

/// Disables data cache.
///
/// # Safety
///
/// The caller code must ensure that cache coherency is handled manually afterwards, so other PEs
/// have the correct view of shared data. This may invole flushing the caches and exiting cache
/// coherency.
/// Calling this function between a LoadExcl/StoreExcl sequence is not permitted, as modifying
/// memory attributes during this window can lead to unpredictable behavior (see 'B2.12.5
/// Load-Exclusive and Store-Exclusive Instruction Usage Restrictions'). The only exception is
/// when the function is restricted to architectures that guarantee predictable behavior for
/// exclusive accesses.
pub unsafe fn disable_dcache() {
    // Safety: Only disabling cache in SCTLR_EL3 and the caller must ensure that the necessary cache
    // maintenance is done afterwards.
    unsafe {
        use crate::sysregs::{SctlrEl3, read_sctlr_el3, write_sctlr_el3};

        write_sctlr_el3(read_sctlr_el3() - SctlrEl3::C);
    }

    isb();
}

/// Flushes a range of cache levels.
///
/// # Safety
///
/// Calling this function between a LoadExcl/StoreExcl sequence is not permitted, as doing cache
/// maintenance operations during this window can lead to unpredictable behavior (see 'B2.12.5
/// Load-Exclusive and Store-Exclusive Instruction Usage Restrictions'). The only exception is
/// when the function is restricted to architectures that guarantee predictable behavior for
/// exclusive accesses.
pub unsafe fn flush_cache_levels(levels: RangeInclusive<u8>) {
    let clidr_el1 = read_clidr_el1();

    for level_num in levels {
        let level = CacheLevel::new(level_num);

        // Check if the cache level is implemented.
        match clidr_el1.ctype(level) {
            CacheType::NoCache | CacheType::InstructionOnly => continue,
            _ => {}
        };

        // Read cache size and calculate way and set iteration variables.
        let cache_size = read_cache_size(level);

        let assoc_shift = cache_size.associativity.leading_zeros() as u64;
        let ways_aligned = u64::from(cache_size.associativity << assoc_shift);
        let way_step = 1 << assoc_shift;
        let line_length = 1 << (cache_size.line_size_log + 4);
        let max_set_num = u64::from(cache_size.num_sets << (cache_size.line_size_log + 4));

        dsb_sy();

        #[allow(unused)]
        for way in (0..=ways_aligned).step_by(way_step) {
            let value = (u64::from(level) << 1) | way;

            for set in (0..=max_set_num).step_by(line_length) {
                // Safety: The inline assembly invokes the 'Data or unified Cache line Clean and Invalidate by Set/Way'
                // instruction using an argument that is based on the reported cache configuration.
                // The caller ensures that the function is not called in an exclusive access window, or it is safe to do
                // so on the given architecture.
                #[cfg(target_arch = "aarch64")]
                unsafe {
                    asm!("dc cisw, {}", options(nostack), in(reg) value | set);
                }
            }
        }
    }

    write_csselr_el1(CsselrEl1::empty());
    dsb_sy();
    isb();
}

/// Flushes level 1 cache.
///
/// # Safety
///
/// Calling this function between a LoadExcl/StoreExcl sequence is not permitted, as doing cache
/// maintenance operations during this window can lead to unpredictable behavior (see 'B2.12.5
/// Load-Exclusive and Store-Exclusive Instruction Usage Restrictions'). The only exception is
/// when the function is restricted to architectures that guarantee predictable behavior for
/// exclusive accesses.
pub unsafe fn flush_level1_cache() {
    // Safety: The same safety conditions are required by the caller.
    unsafe {
        flush_cache_levels(1..=1);
    }
}

/// Flushes level 2 cache.
///
/// # Safety
///
/// Calling this function between a LoadExcl/StoreExcl sequence is not permitted, as doing cache
/// maintenance operations during this window can lead to unpredictable behavior (see 'B2.12.5
/// Load-Exclusive and Store-Exclusive Instruction Usage Restrictions'). The only exception is
/// when the function is restricted to architectures that guarantee predictable behavior for
/// exclusive accesses.
pub unsafe fn flush_level2_cache() {
    // Safety: The same safety conditions are required by the caller.
    unsafe {
        flush_cache_levels(2..=2);
    }
}

/// Flushes level 1, level 2 and level 3 caches.
///
/// # Safety
///
/// Calling this function between a LoadExcl/StoreExcl sequence is not permitted, as doing cache
/// maintenance operations during this window can lead to unpredictable behavior (see 'B2.12.5
/// Load-Exclusive and Store-Exclusive Instruction Usage Restrictions'). The only exception is
/// when the function is restricted to architectures that guarantee predictable behavior for
/// exclusive accesses.
pub unsafe fn flush_all_caches() {
    // Safety: The same safety conditions are required by the caller.
    unsafe {
        flush_cache_levels(1..=3);
    }
}
