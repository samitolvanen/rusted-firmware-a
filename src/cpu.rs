// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::sysregs::read_midr_el1;

pub mod aem_generic;
pub mod cortex_a78;

/// The `Cpu` trait captures low level CPU specific operations.
pub trait Cpu {
    /// Main ID register value, only the 'Implementer' and 'PartNum' fields are used for identifying
    /// the `Cpu` implementation.
    const MIDR: u64;

    /// This function is called on CPU cold boot.
    /// # Safety
    ///
    /// The function must be implemented as a naked function. It should only clobber X0-X18, and X30
    /// registers. It must not use the stack, because it might be called at a point when the stack
    /// has not been configured.
    unsafe extern "C" fn reset_handler();

    /// Prepares for a power down that only affects power level 0.
    extern "C" fn power_down_level0();

    /// Prepares for a power down that affects power level 0 and 1.
    extern "C" fn power_down_level1();
}

/// Structure for storing MIDR value and CpuOps function pointers.
#[repr(C)]
#[derive(Debug)]
struct CpuOps {
    midr: u64,
    reset_handler: *const fn(),
    power_down_level0: *const fn(),
    power_down_level1: *const fn(),
}

impl CpuOps {
    /// Only use Implementer and PartNum fields.
    const MIDR_MASK: u64 = 0xff00_fff0;

    /// Check if the instance has an MIDR with matching Implementer and PartNum fields.
    fn has_matching_midr(&self, midr: u64) -> bool {
        (self.midr & Self::MIDR_MASK) == (midr & Self::MIDR_MASK)
    }

    /// Check if the instance is the last, empty one.
    fn end(&self) -> bool {
        self.midr == 0
    }
}

unsafe impl Send for CpuOps {}
unsafe impl Sync for CpuOps {}

impl CpuOps {
    /// Create new, empty instance.
    const fn new() -> Self {
        const NULL: *const fn() = core::ptr::null();

        Self {
            midr: 0,
            reset_handler: NULL,
            power_down_level0: NULL,
            power_down_level1: NULL,
        }
    }

    /// Create [CpuOps] from [Cpu] implementation.
    const fn from_cpu<T: Cpu>() -> Self {
        Self {
            midr: T::MIDR,
            reset_handler: T::reset_handler as *const fn(),
            power_down_level0: T::power_down_level0 as *const fn(),
            power_down_level1: T::power_down_level1 as *const fn(),
        }
    }
}

/// Calculates the count of specified Cpu types.
macro_rules! cpu_ops_count {
    ($cpu:ty) => { 1 };
    ($cpu:ty, $($cpus:ty),+) => {
        cpu_ops_count!($cpu) + cpu_ops_count!($($cpus),+)
    };
}

/// Declares the CPU_OPS array.
macro_rules! define_cpu_ops {
    ($($cpus:ty),+) => {
        const CPU_OPS_COUNT : usize = cpu_ops_count!($($cpus),+);
        static CPU_OPS : [CpuOps; CPU_OPS_COUNT + 1] = [
            $(CpuOps::from_cpu::<$cpus>()),*,
            CpuOps::new() // Final empty item
        ];
    }
}

// These functions can be implemented in assembly.
#[unsafe(no_mangle)]
pub extern "C" fn cpu_reset_handler() {
    let midr = read_midr_el1();

    let mut i = 0;
    loop {
        let ops = &CPU_OPS[i];

        if ops.end() {
            panic!("Unknown MIDR {midr:x}");
        } else if ops.has_matching_midr(midr) {
            let func: extern "C" fn() = unsafe { core::mem::transmute(ops.reset_handler) };
            (func)();
            break;
        }
        i += 1;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn cpu_power_down(level: u32) {
    let midr = read_midr_el1();

    let mut i = 0;
    loop {
        let ops = &CPU_OPS[i];

        if ops.end() {
            panic!("Unknown MIDR {midr:x}");
        } else if ops.has_matching_midr(midr) {
            let func_ptr = match level {
                0 => ops.power_down_level0,
                1 => ops.power_down_level1,
                _ => panic!("Invalid level"),
            };
            let func: extern "C" fn() = unsafe { core::mem::transmute(func_ptr) };
            (func)();
            break;
        }
        i += 1;
    }
}

// This goes into the platform implementation file.
use aem_generic::AemGeneric;
use cortex_a78::CortexA78;

define_cpu_ops!(AemGeneric, CortexA78);

#[cfg(test)]
mod test {
    use crate::sysregs::fake::SYSREGS;

    use super::*;

    #[test]
    fn test_reset_handler() {
        SYSREGS.lock().unwrap().midr_el1 = 0x410f_d0f0;
        cpu_reset_handler();
        cpu_power_down(0);
        cpu_power_down(1);
    }
}
