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
    arch::asm,
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
// TODO: is the constraint above still required?
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
    // Assert that the MMU is not yet enabled:
    assert!(!read_sctlr_el3().contains(SctlrEl3::M));

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

#[repr(align(4096))]
struct InitPageTables {
    l1: [u64; 512],
    l2: [u64; 512],
    l3: [u64; 512],
}

#[unsafe(no_mangle)]
static mut INIT_PT: InitPageTables =
    InitPageTables { l1: [0; 512], l2: [0; 512], l3: [0; 512] };

pub extern "C" fn init_mmu_early() {
    let sctlr = SctlrEl3::A | SctlrEl3::SA | SctlrEl3::I;
    let tcr = (1 << 20) // TBI
        | (0b101 << 16) // 48 bit physical address size (256 TiB).
        | (3 << 12)     // SH0
        | (3 << 10)     // ORGN0
        | (3 << 8)      // IRGN0
        | (64 - 39);    // Size offset is 2**39 bytes (512 GiB).

    // Assert that the MMU is not yet enabled:
    assert!(!read_sctlr_el3().contains(SctlrEl3::M));

    // SAFETY: Set appropriate configurations to TCR_EL3, MAIR_EL3, SCTLR_EL3
    // which might have retained bit settings from earlier bootloader stages.
    // Set TTBR0 to point to a valid root page table address.
    unsafe {
        write_mair_el3(MAIR.0);
        write_tcr_el3(tcr);
        write_sctlr_el3(sctlr);

        let ttbr = &raw const INIT_PT.l1 as usize;
        write_ttbr0_el3(ttbr);
    }

    // Invalidate all instruction caches
    unsafe {
        asm!(
            // Clear I$
            "ic iallu",
            "isb",
        );
    }

    // Clear TLB such that it doesn't contain stale entries for when we
    // re-enable MMU with new mapping.
    tlbi_alle3();
    isb();

    // Build L1/L2/L3 tables to cover the whole BL31 image
    // with identity mapping.
    let image_start = bl31_start();
    let l1_index = (image_start >> 30) & 0x1ff;

    // SAFETY: l2_base raw pointer is inferred from INIT_PT.l2 which is a valid
    // address decided by the linker.
    // l1_index is limited to 0..511 range which matches INIT_PT.l1 array bounds.
    unsafe {
        let l2_base = &raw const INIT_PT.l2 as u64;
        INIT_PT.l1[l1_index] = l2_base | 3;
    }

    let l2_index = (image_start >> 21) & 0x1ff;

    // SAFETY: l3_base raw pointer is inferred from INIT_PT.l3 which is a valid
    // address decided by the linker.
    // l2_index is limited to 0..511 range which matches INIT_PT.l2 array bounds.
    unsafe {
        let l3_base = &raw const INIT_PT.l3 as u64;
        INIT_PT.l2[l2_index] = l3_base | 3;
    }

    let l3_index_start = (image_start >> 12) & 0x1ff;
    let l3_index_end = (bl31_end() >> 12) & 0x1ff;

    //TODO: check image does not cross a 2MB (or 1GB) boundary.

    let mut image_address = image_start as u64;

    for l3_index in l3_index_start..l3_index_end {
        let mut l3_desc = image_address;

        #[cfg(feature = "rme")]
        {
            // NSE
            l3_desc |= (1 << 11);
        }

        // AF, Inner shareable, AttrIndex=0 (Normal WRT), Pagetable, Valid
        l3_desc |= (1 << 10) | (3 << 8) | (0 << 2) | 3;

        // SAFETY: l3_index is limited to 0..511 range which matches INIT_PT.l3
        // array bounds.
        unsafe {
            INIT_PT.l3[l3_index] = l3_desc;
        }

        image_address += 4096;
    }

    // Ensure all translation table writes have drained into memory, the TLB invalidation is
    // complete, and translation register writes are committed before enabling the MMU.
    dsb_ish();
    isb();

    // SAFETY: Enable MMU and D$, system configuration and page tables are set is
    // a known good way.
    unsafe {
        write_sctlr_el3(sctlr | SctlrEl3::C | SctlrEl3::M);
    }
    isb();
}

/// # Safety
///
/// `root_address` must be the physical address of a valid page table which maps all the memory that
/// EL3 uses.
unsafe fn setup_mmu_cfg(root_address: PhysicalAddress) {
    let tcr = (1 << 20) // TBI
        | (0b101 << 16) // 48 bit physical address size (256 TiB).
        | (3 << 12)     // SH0
        | (3 << 10)     // ORGN0
        | (3 << 8)      // IRGN0
        | (64 - 39);    // Size offset is 2**39 bytes (512 GiB).
    let sctlr = SctlrEl3::WXN
        | SctlrEl3::A
        | SctlrEl3::SA
        | SctlrEl3::I
        | SctlrEl3::C
        | SctlrEl3::M;
    let ttbr0 = root_address.0;

    tlbi_alle3();

    // SAFETY: init_mmu_early has set valid and correct configuration parameters MAIR, TCR.
    // TTBR0 (which is a valid address) is swapped to runtime page tables.
    unsafe {
        write_mair_el3(MAIR.0);
        write_tcr_el3(tcr);
        write_ttbr0_el3(ttbr0);
    }

    // Ensure all translation table writes have drained into memory, the TLB invalidation is
    // complete, and translation register writes are committed before enabling the MMU.
    dsb_ish();
    isb();

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
