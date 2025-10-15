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
    sysregs::{
        SctlrEl3, read_sctlr_el3, write_mair_el3, write_sctlr_el3, write_tcr_el3, write_ttbr0_el3,
    },
};
use aarch64_paging::{
    MapError, Mapping,
    mair::{Mair, MairAttribute, NormalMemory},
    paging::{
        Attributes, Constraints, MemoryRegion, PageTable, PhysicalAddress, Translation,
        TranslationRegime, VaRange, VirtualAddress,
    },
};
use core::{
    fmt::{self, Debug, Formatter},
    mem::take,
    ptr::NonNull,
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
pub const MT_CODE: Attributes = MT_MEMORY.union(Attributes::READ_ONLY);

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

#[unsafe(no_mangle)]
static mut PAGE_TABLE_ADDR: usize = 0;

/// Initialises and enables the page table.
///
/// This should be called once early in startup, before anything else that depends on it.
pub fn init() {
    PAGE_TABLE.call_once(|| {
        let page_heap =
            SpinMutexGuard::leak(PAGE_HEAP.try_lock().expect("Page heap was already taken"));
        let mut idmap = init_page_table(page_heap);

        trace!("Page table: {idmap:?}");

        info!("Setting MMU config");
        unsafe {
            // Expose page table address, this is still written with device attributes.
            PAGE_TABLE_ADDR = idmap.root_address().0;
        }

        // SAFETY: We pass the root address of `idmap`, which has just been initialised with
        // appropriate mappings, and will remain valid forever.
        unsafe {
            setup_mmu_cfg(idmap.root_address());
        }
        info!("Marking page table as active");
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

/// # Safety
///
/// `root_address` must be the physical address of a valid page table which maps all the memory that
/// EL3 uses.
unsafe fn setup_mmu_cfg(root_address: PhysicalAddress) {
    let tcr = (0b101 << 16) // 48 bit physical address size (256 TiB).
        | (64 - 39); // Size offset is 2**39 bytes (512 GiB).
    let ttbr0 = root_address.0;

    let mut sctlr = read_sctlr_el3();
    // Assert that the MMU is not yet enabled:
    //assert!(!sctlr.contains(SctlrEl3::M));

    tlbi_alle3();
    // SAFETY: We enable the MMU with valid and correct configuration parameters MAIR, TCR, and
    // TTBR0 (which is a valid address).
    unsafe {
        write_mair_el3(MAIR.0);
        write_tcr_el3(tcr);
        write_ttbr0_el3(ttbr0);
    }

    // Ensure all translation table writes have drained into memory, the TLB invalidation is
    // complete, and translation register writes are committed before enabling the MMU.
    dsb_ish();
    isb();

    sctlr |= SctlrEl3::M | SctlrEl3::C | SctlrEl3::WXN;
    // SAFETY: `sctlr` is a valid and safe value for the EL3 system control register. At this point,
    // the MMU is turned off (as `assert!`ed above), the translation table base register has been
    // set to a valid address, and we are about to turn the MMU on with a safe configuration
    // (`SctlrEl3::C | SctlrEl3::WXN`).
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

pub mod early_page_tables {
    use crate::{platform::EARLY_PAGE_TABLE_RANGES, sysregs::SctlrEl3};

    #[derive(Debug)]
    #[repr(C)]
    pub struct DescriptorRange {
        /// Index of the descriptor in the flattened page table array.
        index: usize,
        /// This field is split into two by the granule size mask. The upper part is the step value
        /// that is added to each descriptor value. The lower part is the count of the consecutive
        /// block descriptors.
        /// It is 0 for table descriptors.
        step_count: usize,
        /// Descriptor base value. For table descriptor it contains the offset of the next level table
        /// in the page table array.
        value: usize,
    }

    pub const fn build_ranges<const N: usize>(
        regions: &[(Range<usize>, usize)],
    ) -> ([DescriptorRange; N], usize) {
        let mut ranges = [const {
            DescriptorRange {
                index: 0,
                value: 0,
                step_count: 0,
            }
        }; N];

        let (entry_count, _) =
            build_page_table_ranges(regions, &mut ranges, 0, 0, 0, 0x4000_0000, 4, 4096);

        (ranges, entry_count)
    }

    /// const usize min
    const fn min(a: usize, b: usize) -> usize {
        if a < b { a } else { b }
    }

    /// const usize max
    const fn max(a: usize, b: usize) -> usize {
        if a > b { a } else { b }
    }

    /// const overlap check between two `Range<usize>`.
    const fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
        max(a.start, b.start) < min(a.end, b.end)
    }

    /// Builds DescriptorRanges from memory regions recursively.
    ///
    /// * regions: input memory regions and their attributes
    /// * ranges: output ranges
    /// * output_index: next index to be used in ranges
    /// * start_entry: index of the first descriptor in the flattened page table array
    /// * block_base_address: base VA of the current level page table
    /// * block_size: size of the blocks in the current level page table
    /// * descriptor_count: descriptor size at the current level page table
    /// * granule_size: granule size
    const fn build_page_table_ranges(
        regions: &[(Range<usize>, usize)],
        ranges: &mut [DescriptorRange],
        mut output_index: usize,
        start_entry: usize,
        block_base_address: usize,
        block_size: usize,
        descriptor_count: usize,
        granule_size: usize,
    ) -> (usize, usize) {
        let mut used_entry_count = granule_size / 8;
        let mut descriptor_index = 0;

        // Looping the descriptors of the current table.
        loop {
            if descriptor_index >= descriptor_count {
                break;
            }

            let block = (block_base_address + descriptor_index * block_size)
                ..(block_base_address + (descriptor_index + 1) * block_size);

            // Loop each address range and check if the descriptor
            let mut region_index = 0;
            loop {
                if region_index >= regions.len() {
                    break;
                }

                let range = &regions[region_index].0;
                let attr = regions[region_index].1;

                if range.start <= block.start && block.end <= range.end {
                    // The block is fully covered by a region, insert block descriptor.

                    // Calculate description repetition count.
                    let repeat_count = min(
                        (range.end - block.end) / block_size,
                        descriptor_count - descriptor_index,
                    );

                    let lsb = if granule_size == block_size {
                        0b11
                    } else {
                        0b01
                    };

                    ranges[output_index] = DescriptorRange {
                        index: (descriptor_index + start_entry) * 8,
                        step_count: block_size | (repeat_count + 1),
                        value: block.start | attr | lsb,
                    };

                    descriptor_index += repeat_count;
                    output_index += 1;
                    break;
                } else if overlaps(&block, range) {
                    // There's a region that overlaps with the block but with a smaller granule, make it
                    // into a table descriptor.
                    let next_descriptor_count = granule_size / 8;
                    assert!(block_size / next_descriptor_count >= granule_size);

                    ranges[output_index] = DescriptorRange {
                        index: (descriptor_index + start_entry) * 8,
                        step_count: 0,
                        value: ((start_entry + used_entry_count) * 8) | 0b11,
                    };

                    // Create next level table.
                    let (new_entry_count, new_output_index) = build_page_table_ranges(
                        regions,
                        ranges,
                        output_index + 1,
                        start_entry + used_entry_count,
                        block.start,
                        block_size / next_descriptor_count,
                        next_descriptor_count,
                        granule_size,
                    );

                    output_index = new_output_index;
                    used_entry_count += new_entry_count;
                    break;
                }

                region_index += 1;
            }

            descriptor_index += 1;
        }
        (used_entry_count, output_index)
    }

    /// Calculates the necessary DescriptorRanges for a given region list in const time.
    pub const fn get_range_entry_count(
        regions: &[(Range<usize>, usize)],
        block_base_address: usize,
        block_size: usize,
        descriptor_count: usize,
        granule_size: usize,
    ) -> usize {
        let mut count = 0;

        let mut descriptor_index = 0;
        loop {
            if descriptor_index >= descriptor_count {
                break;
            }

            let block = (block_base_address + descriptor_index * block_size)
                ..(block_base_address + (descriptor_index + 1) * block_size);

            let mut region_index = 0;
            loop {
                if region_index >= regions.len() {
                    break;
                }

                let range = &regions[region_index].0;

                if range.start <= block.start && block.end <= range.end {
                    // The block is fully covered by a region, insert block descriptor.

                    // Calculate description repetition count.
                    descriptor_index += min(
                        (range.end - block.end) / block_size,
                        descriptor_count - descriptor_index,
                    );

                    count += 1;
                    break;
                } else if overlaps(&block, range) {
                    // There's a region that overlaps with the block but with a smaller granule, make it
                    // into a table descriptor.
                    let next_descriptor_count = granule_size / 8;
                    assert!(block_size / next_descriptor_count >= granule_size);

                    count += get_range_entry_count(
                        regions,
                        block.start,
                        block_size / next_descriptor_count,
                        next_descriptor_count,
                        granule_size,
                    ) + 1;
                    break;
                }

                region_index += 1;
            }

            descriptor_index += 1;
        }
        count
    }

    macro_rules! define_early_mapping {
        ($regions:expr) => {
            pub static EARLY_PAGE_TABLE_RANGES: (
                [$crate::pagetable::early_page_tables::DescriptorRange;
                    $crate::pagetable::early_page_tables::get_range_entry_count(
                        &$regions,
                        0,
                        0x4000_0000,
                        4,
                        4096,
                    )],
                usize,
            ) = $crate::pagetable::early_page_tables::build_ranges(&$regions);
        };
    }

    use core::ops::Range;

    pub(crate) use define_early_mapping;

    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    pub extern "C" fn init_early_page_tables() {
        core::arch::naked_asm!(
            "/* x0 = RANGES start */
            ldr	x0, ={ranges}

            /* x1 = RANGES end */
            ldr	x1, =({ranges} + ({ranges_size} * {ranges_count}))

            /* x2 = table base address */
            ldr x2, =early_page_table_start

        1:
            /* x3 = range.index and x4 = range.step_count, x5 = range.value */
            ldp x3, x4, [x0, #{index_step_count_offset}];
            ldr x5, [x0, #{value_offset}]

            /* If step_count is zero, the entry is a table descriptor. */
            cbz	x4, 3f

            /* Block descriptors */

            /* x4 = step, x6 = index + (count * 8) */
            and x6, x4, #{count_mask}
            sub x4, x4, x6
            lsl x6, x6, #3
            add x6, x3, x6

        2:
            /* Block descriptor loop */

            /* *(table_base + index) = range.value */
            str x5, [x2, x3]

            /* index += 8 */
            add x3, x3, #8

            /* index != end_index */
            cmp x3, x6
            b.eq 4f

            /* range.value += step */
            add x5, x5, x4
            b 2b

        3:
            /* Table descriptor */

            /* *(table_base + index) = table_base + range.value */
            add x5, x2, x5
            str x5, [x2, x3]

        4:
            /* range += sizeof(DescriptorRange) */
            add	x0, x0, #{ranges_size}

            /* Check end of list */
            cmp	x0, x1
            b.ne	1b

            /* Instruction and data barrier */
            isb
            dsb sy

            ret",
            ranges = sym EARLY_PAGE_TABLE_RANGES,
            ranges_size = const core::mem::size_of::<DescriptorRange>(),
            ranges_count = const EARLY_PAGE_TABLE_RANGES.0.len(),
            index_step_count_offset = const core::mem::offset_of!(DescriptorRange, index),
            value_offset = const core::mem::offset_of!(DescriptorRange, value),
            count_mask = const 0xfff,
        );
    }

    #[cfg(target_arch = "aarch64")]
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    pub extern "C" fn enable_early_mmu() {
        core::arch::naked_asm!(
            "tlbi alle3

            ldr x0, ={mair}
            msr mair_el3, x0

            ldr x0, ={tcr}
            msr tcr_el3, x0

            ldr x0, =early_page_table_start
            msr ttbr0_el3, x0

            dsb ish
            isb

            mrs x1, sctlr_el3

            ldr x0, ={sctlr_set}
            orr x1, x1, x0
            ldr x0, ={sctlr_clear}
            and x1, x1, x0

            msr sctlr_el3, x1

            isb
            ret",
            mair = const super::MAIR.0,
            tcr = const {
                (0b101 << 16) // 48 bit physical address size (256 TiB).
            | (64 - 32) // Size offset is 2**32 bytes (4 GiB).
            },
            sctlr_set = const { SctlrEl3::M.bits() | SctlrEl3::C.bits() },
            sctlr_clear = const { !SctlrEl3::WXN.bits() },
        )
    }
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
