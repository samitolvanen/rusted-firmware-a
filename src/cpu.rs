// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

pub mod aem_generic;

use crate::{platform::CPU_OPS, sysregs::read_midr_el1};

/// The `Cpu` trait captures low level CPU specific operations.
/// # Safety
///
/// The `reset_handler` function must be implemented as a naked function. It should only clobber
/// X0-X18, and X30 registers. It must not use the stack, because it might be called at a point when
/// the stack has not been configured.
pub unsafe trait Cpu {
    /// Main ID register value, only the 'Implementer' and 'PartNum' fields are used for identifying
    /// the `Cpu` implementation.
    const MIDR: u64;

    /// This function is called on CPU cold boot.
    extern "C" fn reset_handler();

    /// Prepares for a power down that only affects power level 0.
    fn power_down_level0();

    /// Prepares for a power down that affects power level 0 and 1.
    fn power_down_level1();
}

/// Structure for storing MIDR value and `CpuOps` function pointers.
#[repr(C)]
#[derive(Debug)]
pub struct CpuOps {
    midr: u64,
    reset_handler: *const extern "C" fn(),
    power_down_level0: *const fn(),
    power_down_level1: *const fn(),
}

impl CpuOps {
    /// Only use Implementer and PartNum fields.
    const MIDR_MASK: u64 = 0xff00_fff0;
    //const SIZE: usize = size_of::<Self>();

    /// Check if the instance has an MIDR with matching Implementer and PartNum fields.
    fn has_matching_midr(&self, midr: u64) -> bool {
        (self.midr & Self::MIDR_MASK) == (midr & Self::MIDR_MASK)
    }

    /// Check if the instance is the last, empty one.
    fn end(&self) -> bool {
        self.midr == 0
    }
}

/// Safety:
/// Safety: All fields of `CpuOps` are immutable after initialization and contain constant values
/// and raw function pointers. The function pointers are assumed to point to valid static functions
/// for the lifetime of the application on all cores, making it safe to transfer ownership of a
/// `CpuOps` instance to another thread.
unsafe impl Send for CpuOps {}

/// Safety: All fields of `CpuOps` are immutable after initialization and contain constant values
/// and raw function pointers. There is no interior mutability or dependency on thread-local state,
/// so shared references to `CpuOps` can be safely accessed from multiple threads concurrently.
unsafe impl Sync for CpuOps {}

impl CpuOps {
    /// Create new, empty instance.
    pub const fn empty() -> Self {
        const NULL: *const fn() = core::ptr::null();

        Self {
            midr: 0,
            reset_handler: core::ptr::null(),
            power_down_level0: NULL,
            power_down_level1: NULL,
        }
    }

    /// Create [CpuOps] from [Cpu] implementation.
    pub const fn from_cpu<T: Cpu>() -> Self {
        Self {
            midr: T::MIDR & Self::MIDR_MASK,
            reset_handler: T::reset_handler as *const extern "C" fn(),
            power_down_level0: T::power_down_level0 as *const fn(),
            power_down_level1: T::power_down_level1 as *const fn(),
        }
    }
}

/// Calculates the count of specified Cpu types.
#[macro_export]
macro_rules! cpu_ops_count {
    ($cpu:ty) => { 1 };
    ($cpu:ty, $($cpus:ty),+) => {
        cpu_ops_count!($cpu) + cpu_ops_count!($($cpus),+)
    };
}

/// Declares the CPU_OPS array.
#[macro_export]
macro_rules! define_cpu_ops {
    ($($cpus:ty),+) => {
        const CPU_OPS_COUNT : usize = $crate::cpu_ops_count!($($cpus),+);
        pub static CPU_OPS : [$crate::cpu::CpuOps; CPU_OPS_COUNT + 1] = [
            $($crate::cpu::CpuOps::from_cpu::<$cpus>()),*,
            $crate::cpu::CpuOps::empty() // Final empty item
        ];
    }
}

#[cfg(test)]
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
            func();
            break;
        }
        i += 1;
    }
}

#[cfg(not(test))]
#[unsafe(naked)]
#[unsafe(no_mangle)]
pub extern "C" fn cpu_reset_handler() {
    core::arch::naked_asm!(
        "/* Read and mask MIDR_EL1 */
	    mrs	x2, midr_el1
        mov w3, ({midr_mask} & 0xffff)
        movk    w3, (({midr_mask} >> 16) & 0xffff), LSL 16
	    and	w2, w2, w3

        /* Get the CPU_OPS location */
        ldr x3, ={cpu_ops}

    1:
        /* Load the midr from the CPU_OPS */
        ldr w1, [x3, #{midr_offset}]

        /* Check end of list */
        cbz w1, 3f

        /* Check if MIDR matches the MIDR of this core */
	    cmp w1, w2
	    b.eq    2f

        /* Step to next CPU_OPS entry */
        add x3, x3, #{cpu_ops_size}
        b   1b

    2:
        /* Read and jump to reset handler function */
        ldr x1, [x3, #{reset_handler_offset}]
        br  x1

    3:
        /* The MIDR values was not found */
        b   el3_panic
        ",
        midr_mask = const CpuOps::MIDR_MASK,
        cpu_ops = sym CPU_OPS,
        cpu_ops_size = const core::mem::size_of::<CpuOps>(),
        midr_offset = const core::mem::offset_of!(CpuOps, midr),
        reset_handler_offset = const core::mem::offset_of!(CpuOps, reset_handler),
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn cpu_power_down(level: usize) {
    let midr = read_midr_el1();

    let mut i = 0;
    loop {
        let ops = &CPU_OPS[i];

        if ops.end() {
            return;
        } else if ops.has_matching_midr(midr) {
            let func_ptr = if level == 0 {
                ops.power_down_level0
            } else {
                ops.power_down_level1
            };
            let func: fn() = unsafe { core::mem::transmute(func_ptr) };
            func();
            break;
        }
        i += 1;
    }
}

#[cfg(test)]
mod test {
    use crate::sysregs::fake::SYSREGS;

    use super::*;

    #[test]
    fn test_reset_handler() {
        SYSREGS.lock().unwrap().midr_el1 = 0x1234_5678;
        cpu_reset_handler();
        cpu_power_down(0);
        cpu_power_down(1);
    }
}
