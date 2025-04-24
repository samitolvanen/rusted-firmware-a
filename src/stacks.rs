// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::{
    pagetable::{IdMap, map_region},
    platform::{Platform, PlatformImpl},
};
use aarch64_paging::paging::{Attributes, MemoryRegion, PAGE_SIZE};
use core::cell::UnsafeCell;

/// The number of bytes of stack space to reserve for each core.
///
/// This must by a multiple of `PAGE_SIZE`, so that stack guards can be page aligned.
const STACK_SIZE: usize = PAGE_SIZE * 2;

const _: () = assert!(STACK_SIZE % PAGE_SIZE == 0);
const _: () = assert!(STACK_SIZE % PlatformImpl::CACHE_WRITEBACK_GRANULE == 0);

/// The size in bytes of a stack plus a guard page.
const GUARDED_STACK_SIZE: usize = STACK_SIZE + PAGE_SIZE;

#[cfg(not(test))]
const STACKS_PTR: *const [Stack; PlatformImpl::CORE_COUNT] = &raw const PLATFORM_NORMAL_STACKS;

/// Fake stack region start for tests. This should be consistent with the layout in
/// `layout_fake.rs`.
#[cfg(test)]
const STACKS_PTR: *const [Stack; PlatformImpl::CORE_COUNT] = 0x10_0000 as _;

/// Stacks allocated for all cores.
#[unsafe(export_name = "platform_normal_stacks")]
#[unsafe(link_section = ".tzfw_normal_stacks")]
static PLATFORM_NORMAL_STACKS: [Stack; PlatformImpl::CORE_COUNT] =
    [const { Stack::new() }; PlatformImpl::CORE_COUNT];

#[repr(C, align(4096))]
struct Stack(UnsafeCell<[u8; GUARDED_STACK_SIZE]>);

impl Stack {
    /// Creates a new empty stack.
    const fn new() -> Self {
        Self(UnsafeCell::new([0; GUARDED_STACK_SIZE]))
    }
}

// SAFETY: &Stack only allows getting a pointer to the stack, which is in itself safe to do from any
// core. Actually accessing the stack from the wrong core is not safe.
unsafe impl Sync for Stack {}

/// Unmaps guard pages for all stacks from the given pagetable.
///
/// This must be called before the pagetable is activated.
pub fn unmap_stack_guards(idmap: &mut IdMap) {
    let stacks_start = STACKS_PTR as usize;
    for core in 0..PlatformImpl::CORE_COUNT {
        let stack_start = stacks_start + core * GUARDED_STACK_SIZE;
        map_region(
            idmap,
            &MemoryRegion::new(stack_start, stack_start + PAGE_SIZE),
            Attributes::empty(),
        );
    }
}

#[cfg(target_arch = "aarch64")]
mod asm {
    use super::*;
    use crate::debug::DEBUG;
    use core::arch::global_asm;

    global_asm!(
        include_str!("asm_macros_common.S"),

        // Returns a pointer to the top of the stack to use for current CPU.
        ".global	plat_get_my_stack",
            "func plat_get_my_stack",
            "mov	x10, x30",
            "bl	plat_my_core_pos",
            "adrp	x2, (platform_normal_stacks + {GUARDED_STACK_SIZE})",
            "add	x2, x2, :lo12:(platform_normal_stacks + {GUARDED_STACK_SIZE})",
            "mov x1, #{GUARDED_STACK_SIZE}",
            "madd x0, x0, x1, x2",
            "ret	x10",
        "endfunc plat_get_my_stack",

        // Initialises the stack pointer for the current CPU.
        ".global	plat_set_my_stack",
        "func plat_set_my_stack",
            "mov	x9, x30",
            "bl 	plat_get_my_stack",
            "mov	sp, x0",
            "ret	x9",
        "endfunc plat_set_my_stack",

        include_str!("asm_macros_common_purge.S"),

        DEBUG = const DEBUG as i32,
        GUARDED_STACK_SIZE = const GUARDED_STACK_SIZE,
    );
}
