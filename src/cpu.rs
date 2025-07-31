// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::sysregs::read_midr_el1;

pub mod aem_generic;
pub mod cortex_a78;

pub trait Cpu {
    const MIDR: u64;

    extern "C" fn reset_handler();
    extern "C" fn power_down_level0();
    extern "C" fn power_down_level1();
}

/// Function pointer that implements Send and Sync.
#[repr(C)]
#[derive(Debug)]
struct CpuOpsFunction(*const fn());

unsafe impl Send for CpuOpsFunction {}
unsafe impl Sync for CpuOpsFunction {}

/// Structure for storing CpuOps function pointers.
#[repr(C)]
#[derive(Debug)]
struct CpuOps {
    midr: u64,
    reset_handler: CpuOpsFunction,
    power_down_level0: CpuOpsFunction,
    power_down_level1: CpuOpsFunction,
}

impl CpuOps {
    const fn new() -> Self {
        const NULL: CpuOpsFunction = CpuOpsFunction(core::ptr::null());

        Self {
            midr: 0,
            reset_handler: NULL,
            power_down_level0: NULL,
            power_down_level1: NULL,
        }
    }

    /// Create CpuOps from Cpu implementation.
    const fn from_cpu<T: Cpu>() -> Self {
        Self {
            midr: T::MIDR,
            reset_handler: CpuOpsFunction(T::reset_handler as *const fn()),
            power_down_level0: CpuOpsFunction(T::power_down_level0 as *const fn()),
            power_down_level1: CpuOpsFunction(T::power_down_level1 as *const fn()),
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

/// Fills the $var CpuOps array with the details of the specified Cpus.
macro_rules! cpu_ops {
    ($var:expr, $i:expr, $func:ident, $cpu:ty) => {
        $var[$i] = CpuOps::from_cpu::<$cpu>();
    };
    ($var:expr, $i:expr, $func:ident, $cpu:ty, $($cpus:ty),+) => {
        cpu_ops!($var, $i, $func, $cpu);
        cpu_ops!($var, $i + 1, $func, $($cpus),+)
    };
}

/// Declares the CPU_OPS array.
macro_rules! define_cpu_ops {
    ($($cpus:ty),+) => {
        const CPU_OPS_COUNT : usize = cpu_ops_count!($($cpus),+);
        static CPU_OPS : [CpuOps; CPU_OPS_COUNT + 1] = {
           let mut temp = [const{ CpuOps::new() }; CPU_OPS_COUNT + 1];
           cpu_ops!(temp, 0, power_down, $($cpus),+);
           temp
        };
    }
}

// These functions can be implemented in assembly.
#[unsafe(no_mangle)]
pub extern "C" fn reset_handler() {
    let midr = read_midr_el1();

    let mut i = 0;
    loop {
        let ops = &CPU_OPS[i];

        if ops.midr == 0 {
            panic!("Unknown MIDR {midr:x}");
        } else if ops.midr == midr {
            let func: extern "C" fn() = unsafe { core::mem::transmute(ops.reset_handler.0) };
            (func)();
            break;
        }
        i += 1;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn power_down(level: u32) {
    let midr = read_midr_el1();

    let mut i = 0;
    loop {
        let ops = &CPU_OPS[i];

        if ops.midr == 0 {
            panic!("Unknown MIDR {midr:x}");
        } else if ops.midr == midr {
            let func_ptr = match level {
                0 => ops.power_down_level0.0,
                1 => ops.power_down_level1.0,
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
        reset_handler();
    }
}
