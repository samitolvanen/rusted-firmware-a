// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::{
    aarch64::{dsb_ish, dsb_sy, isb, tlbi_alle3},
    layout::{
        bl_code_base, bl_code_end, bl_ro_data_base, bl_ro_data_end, bl31_end, bl31_start, bss2_end,
        bss2_start,
    },
    platform::{Platform, PlatformImpl},
};
use aarch64_paging::{
    MapError, Mapping,
    mair::{Mair, MairAttribute, NormalMemory},
    paging::{
        Attributes, Constraints, MemoryRegion, PageTable, PhysicalAddress, Translation,
        TranslationRegime, VaRange, VirtualAddress,
    },
};
use arm_sysregs::{
    SctlrEl3, read_sctlr_el3, write_mair_el3, write_sctlr_el3, write_tcr_el3, write_ttbr0_el3,
};
use core::{
    fmt::{self, Debug, Formatter},
    mem::take,
    ptr::NonNull,
    arch::naked_asm,
};
use log::{debug, info, trace, warn};
use spin::{
    Once,
    mutex::{SpinMutex, SpinMutexGuard},
};

const ROOT_LEVEL: usize = 1;

// Indices of entries in the Memory Attribute Indirection Register.
const MAIR_IWTRWA_OWTRWA_NTR_INDEX: u8 = 0;
const MAIR_DEVICE_INDEX: u8 = 1;
const MAIR_NON_CACHEABLE_INDEX: u8 = 2;

// Values for MAIR entries.
const MAIR_DEVICE: MairAttribute = MairAttribute::DEVICE_NGNRE;

// Set write-through mode to ensure all written values are propagated to system memory.
// This guarantees correct Once and Mutex behavior before enabling the MMU.
const MAIR_IWTRWA_OWTRWA_NTR: MairAttribute = MairAttribute::normal(
    NormalMemory::WriteThroughTransientReadWriteAllocate,
    NormalMemory::WriteThroughTransientReadWriteAllocate,
);
const MAIR_NON_CACHEABLE: MairAttribute =
    MairAttribute::normal(NormalMemory::NonCacheable, NormalMemory::NonCacheable);

const MAIR: Mair = Mair::EMPTY
    .with_attribute(MAIR_DEVICE_INDEX, MAIR_DEVICE)
    .with_attribute(MAIR_IWTRWA_OWTRWA_NTR_INDEX, MAIR_IWTRWA_OWTRWA_NTR)
    .with_attribute(MAIR_NON_CACHEABLE_INDEX, MAIR_NON_CACHEABLE);

// Attribute values corresponding to the above MAIR indices.
const IWTRWA_OWTRWA_NTR: Attributes = Attributes::ATTRIBUTE_INDEX_0;
const DEVICE: Attributes = Attributes::ATTRIBUTE_INDEX_1;
const NON_CACHEABLE: Attributes = Attributes::ATTRIBUTE_INDEX_2;

/// Attribute bits which are RES1 for the EL3 translation regime, as we configure it.
///
/// From Arm ARM K.a, D8.3.1.2 Fig. D8-16: lower attributes AP\[1\] bit 6
/// and D8.4.1.2.1 Stage 1 data accesses using Direct permissions:
/// "For a stage 1 translation that supports one Exception level, AP\[1\] is RES1."
const EL3_RES1: Attributes = Attributes::USER;

/// Attribute bit for NSE aka Root state for FEAT_RME
///
/// From ARM DDI 0487K.a, D8-49 Stage 1 VMSAv8-64 Block and Page descriptor fields,
/// the NSE bit is aliased with the Not-global (nG) flag (bit 11).
const NSE: Attributes = Attributes::NON_GLOBAL;

/// Attributes used for all mappings.
///
/// We always set the access flag, as we don't manage access flag faults.
const BASE: Attributes = if cfg!(feature = "rme") {
    EL3_RES1
        .union(Attributes::ACCESSED)
        .union(Attributes::VALID)
        .union(NSE)
} else {
    EL3_RES1
        .union(Attributes::ACCESSED)
        .union(Attributes::VALID)
};

/// Attributes used for device mappings.
///
/// Device memory is always mapped as execute-never to avoid the possibility of a speculative
/// instruction fetch, which could be an issue if the memory region corresponds to a read-sensitive
/// peripheral.
/// Arm ARM K.a D8.6.2 "If a region is mapped as Device memory or Normal
/// Non-cacheable memory after all enabled translation stages, then the
/// region has an effective Shareability attribute of Outer Shareable."
/// Arm ARM K.a D8.4.1.2.3 bit 54 UXN/PXN/XN is the XN field at EL3:
/// "If the Effective value of XN is 1, then PrivExecute is removed."
pub const MT_DEVICE: Attributes = DEVICE.union(BASE).union(Attributes::UXN);

/// Attributes used for non-cacheable memory mappings.
#[allow(unused)]
pub const MT_NON_CACHEABLE: Attributes = NON_CACHEABLE.union(BASE);

/// Attributes used for regular memory mappings.
pub const MT_MEMORY: Attributes = IWTRWA_OWTRWA_NTR
    .union(BASE)
    .union(Attributes::INNER_SHAREABLE);

/// Attributes used for code (i.e. text) mappings.
pub const MT_CODE: Attributes = {
    let attrs = MT_MEMORY.union(Attributes::READ_ONLY);
    if cfg!(bti) {
        attrs.union(Attributes::GP)
    } else {
        attrs
    }
};

/// Attributes used for read-only data mappings.
pub const MT_RO_DATA: Attributes = MT_MEMORY
    .union(Attributes::READ_ONLY)
    .union(Attributes::UXN);

/// Attributes used for read-write data mappings.
#[allow(unused)]
pub const MT_RW_DATA: Attributes = MT_MEMORY.union(Attributes::UXN);

static PAGE_HEAP: SpinMutex<[PageTable; PlatformImpl::PAGE_HEAP_PAGE_COUNT]> =
    SpinMutex::new([PageTable::EMPTY; PlatformImpl::PAGE_HEAP_PAGE_COUNT]);
static PAGE_TABLE: Once<SpinMutex<IdMap>> = Once::new();

/// Initialises and enables the page table.
///
/// This should be called once early in startup, before anything else that depends on it.
pub fn init() {
    PAGE_TABLE.call_once(|| {
        let page_heap =
            SpinMutexGuard::leak(PAGE_HEAP.try_lock().expect("Page heap was already taken"));
        let mut idmap = init_page_table(page_heap);

        // SAFETY: We pass the root address of `idmap`, which has just been initialised with
        // appropriate mappings, and will remain valid forever.
        unsafe {
            setup_mmu_cfg(idmap.root_address());
        }
        idmap.mark_active();

        SpinMutex::new(idmap)
    });
}

/// Enables the MMU for a newly booted core, assuming the page table is already initialised.
pub fn enable() {
    // SAFETY: We pass the root address of the IdMap from `PAGE_TABLE`, which has previously been
    // initialised with appropriate mappings, and will remain valid forever.
    unsafe {
        setup_mmu_cfg(PAGE_TABLE.get().unwrap().lock().root_address());
    }
}

/// Creates the page table and maps initial regions needed for boot, including any platform-specific
/// regions.
fn init_page_table(pages: &'static mut [PageTable]) -> IdMap {
    let mut idmap = IdMap::new(pages);

    // If the BL32 entry point is in the middle of our memory range then something is misconfigured.
    let secure_entry_pc = PlatformImpl::secure_entry_point().pc;
    assert!(secure_entry_pc < bl31_start() || secure_entry_pc >= bl31_end());
    assert!(secure_entry_pc < bss2_start() || secure_entry_pc >= bss2_end());

    // Corresponds to `bl_regions` in C TF-A, `plat/arm/common/arm_bl31_setup.c`.
    // BL31_TOTAL
    map_region(
        &mut idmap,
        &MemoryRegion::new(bl31_start(), bl31_end()),
        MT_MEMORY,
    );
    // BL31_RO
    map_region(
        &mut idmap,
        &MemoryRegion::new(bl_code_base(), bl_code_end()),
        MT_CODE,
    );
    map_region(
        &mut idmap,
        &MemoryRegion::new(bl_ro_data_base(), bl_ro_data_end()),
        MT_RO_DATA,
    );
    let bss2_start = bss2_start();
    let bss2_end = bss2_end();
    if bss2_start != bss2_end {
        map_region(
            &mut idmap,
            &MemoryRegion::new(bss2_start, bss2_end),
            MT_RW_DATA,
        );
    }

    // Corresponds to `plat_regions` in C TF-A.
    PlatformImpl::map_extra_regions(&mut idmap);

    idmap
}

/// Adds the given region to the page table with the given attributes, logging it first.
pub fn map_region(idmap: &mut IdMap, region: &MemoryRegion, attributes: Attributes) {
    debug!("Mapping {region} as {attributes:?}.");
    idmap
        .map_range(region, attributes)
        .expect("Error mapping memory range");
}

#[unsafe(naked)]
pub extern "C" fn init_mmu_early() {
    naked_asm!(
        // Disable MMU and D$
        "mrs x1, sctlr_el3",
        "mov x2, 5", // M=C=0
        "orr x2, x2, 0x80000", // WXN=0
        "bic x1, x1, x2",
        "msr sctlr_el3, x1",
        "isb",

        // Clear TLB
        "tlbi alle3",
        "dsb sy",
        "isb",

        // TCR_EL3
        // T0SZ=25 (39b), IRGN0=11b, ORGN0=11b, SH0=11b
        "mov x1, (25UL << 0) | (3UL << 8) | (3UL << 10) | (3UL << 12)",
        // PS=101b (48b), TBI=1, IPS=01b, TBI0=1
        "mov x2, (1UL << 20) | (5UL << 16)",
        "orr x2, x2, (1UL << 32)",
        "orr x2, x2, (1UL << 37)",
        "orr x1, x1, x2",
        "msr tcr_el3, x1",

        // Set MAIR_EL3
        "mov x1, 0x0433",
        "movk x1, 0x44, lsl 16",
        "msr mair_el3, x1",

        // Set TTBR0_EL3
        "adr x1, __INIT_PT_START__",
        "mov x2, x1",
        "msr ttbr0_el3, x1",

        // Clear initial page tables
        "mov x3, 3 * 4096",
        "2: stp xzr, xzr, [x2], #16",
        "subs x3, x3, #16",
        "bne 2b",

        "add x2, x1, 4096",
        "add x3, x2, 4096",
        "adr x4, __TEXT_START__",

        // Calculate L1 PT entry address
        "lsr x5, x4, 30",
        "and x6, x5, 0x1ff",
        "lsl x6, x6, 3",
        "add x1, x1, x6",
        "orr x5, x2, 3", // Page table, valid
        "str x5, [x1]",

        // Calculate L2 PT entry address
        "lsr x5, x4, 21",
        "and x6, x5, 0x1ff",
        "lsl x6, x6, 3",
        "add x2, x2, x6",
        "orr x5, x3, 3", // Page table, valid
        "str x5, [x2]",

        // Calculate L3 PT entry address
        "lsr x5, x4, 12",
        "and x6, x5, 0x1ff",
        "lsl x6, x6, 3",
        "add x3, x3, x6",

        // Fill L3 table with page descriptors
        "adr x5, __BL31_END__",
        "orr x4, x4, (1UL << 10)",
        "orr x4, x4, 3", // Page descriptor, valid
        "3: add x6, x4, 4096",
        "stp x4, x6, [x3], #16",
        "add x4, x4, 8192",
        "cmp x4, x5",
        "blt 3b",

        // Enable MMU and D$
        "mrs x1, sctlr_el3",
        "orr x1, x1, 4",
        "orr x1, x1, 1",
        "msr sctlr_el3, x1",
        "isb",

        // Clear I$
        "ic iallu",
        "isb",
        "ret"
    );
}

/// # Safety
///
/// `root_address` must be the physical address of a valid page table which maps all the memory that
/// EL3 uses.
unsafe fn setup_mmu_cfg(root_address: PhysicalAddress) {
    let ttbr0 = root_address.0;
    let mut sctlr = read_sctlr_el3();

    tlbi_alle3();

    // SAFETY: We enable the MMU with valid and correct configuration parameters MAIR, TCR, and
    // TTBR0 (which is a valid address).
    unsafe {
        write_ttbr0_el3(ttbr0);
    }

    // Ensure all translation table writes have drained into memory, the TLB invalidation is
    // complete, and translation register writes are committed before enabling the MMU.
    dsb_ish();
    isb();

    // Enable WXN
    sctlr |= SctlrEl3::WXN;
    // SAFETY: `sctlr` is a valid and safe value for the EL3 system control register.
    // The translation table base register has been set to a valid address.
    unsafe {
        write_sctlr_el3(sctlr);
    }
    isb();
}

/// # Safety
///
/// Caller must guarantee that it is safe to disable the MMU at the time of calling this function.
pub unsafe fn disable_mmu_el3() {
    let mut sctlr_el3 = read_sctlr_el3();
    sctlr_el3.remove(SctlrEl3::C | SctlrEl3::M);
    // SAFETY: `sctlr` is a valid and safe value for the EL3 system control register. Caller
    // promises that we can safely disable the MMU.
    unsafe {
        write_sctlr_el3(sctlr_el3);
    }
    isb();
    dsb_sy();
}

struct IdTranslation {
    /// Pages which may be allocated for page tables but have not yet been.
    unused_pages: &'static mut [PageTable],
}

impl Debug for IdTranslation {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        f.debug_struct("IdTranslation")
            .field("unused_pages", &self.unused_pages.len())
            .finish()
    }
}

impl IdTranslation {
    fn virtual_to_physical(va: VirtualAddress) -> PhysicalAddress {
        // Physical address is the same as the virtual address because we are using identity mapping
        // everywhere.
        PhysicalAddress(va.0)
    }
}

impl Translation for IdTranslation {
    fn allocate_table(&mut self) -> (NonNull<PageTable>, PhysicalAddress) {
        let (table, rest) = take(&mut self.unused_pages)
            .split_first_mut()
            .expect("Failed to allocate page table");
        self.unused_pages = rest;

        let table = NonNull::from(table);
        (
            table,
            Self::virtual_to_physical(VirtualAddress(table.as_ptr() as usize)),
        )
    }

    unsafe fn deallocate_table(&mut self, page_table: NonNull<PageTable>) {
        warn!("Leaking page table allocation {page_table:?}");
    }

    fn physical_to_virtual(&self, page_table_pa: PhysicalAddress) -> NonNull<PageTable> {
        NonNull::new(page_table_pa.0 as *mut PageTable)
            .expect("Got physical address 0 for pagetable")
    }
}

#[derive(Debug)]
pub struct IdMap {
    mapping: Mapping<IdTranslation>,
}

impl IdMap {
    fn new(pages: &'static mut [PageTable]) -> Self {
        Self {
            mapping: Mapping::new(
                IdTranslation {
                    unused_pages: pages,
                },
                0,
                ROOT_LEVEL,
                TranslationRegime::El3,
                VaRange::Lower,
            ),
        }
    }

    fn map_range(&mut self, range: &MemoryRegion, flags: Attributes) -> Result<(), MapError> {
        let pa = IdTranslation::virtual_to_physical(range.start());
        self.mapping
            .map_range(range, pa, flags, Constraints::empty())
    }

    fn mark_active(&mut self) {
        self.mapping.mark_active();
    }

    fn root_address(&self) -> PhysicalAddress {
        self.mapping.root_address()
    }
}

#[cfg(all(target_arch = "aarch64", not(test)))]
mod asm {
    use crate::debug::DEBUG;
    use core::arch::global_asm;

    global_asm!(
        include_str!("asm_macros_common.S"),
        include_str!("cache_helpers.S"),
        include_str!("asm_macros_common_purge.S"),
        DEBUG = const DEBUG as i32,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_page_table() {
        assert_ne!(PlatformImpl::PAGE_HEAP_PAGE_COUNT, 0);

        let page_heap =
            SpinMutexGuard::leak(PAGE_HEAP.try_lock().expect("Page heap was already taken"));

        let mut idmap = init_page_table(page_heap);
        assert_ne!(idmap.root_address().0, 0);
        idmap.mark_active();
        // `aarch64-paging` will detect the dropped idmap and panic
        core::mem::forget(idmap);
    }
}
