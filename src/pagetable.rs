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
    cell::UnsafeCell,
    fmt::{self, Debug, Formatter},
    mem::{MaybeUninit, take},
    ptr::NonNull,
};
use log::{debug, info, trace, warn};

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

pub static VIRTUAL_MEMORY_MAPPING: VirtualMemoryMapping = VirtualMemoryMapping::new();

/// Once-like entity for handling virtual memory mapping related global objects.
///
/// Using exclusive load/store instructions before enabling the MMU and caches is not permitted,
/// because it can introduce unpredictable behavior according to the Arm Architecture Reference
/// Manual. For this reason, Once, SpinMutex, etc. cannot be used when intializing the page tables.
/// The functions of this object can only be called before enabling the MMU, either for initializing
/// the page tables on the primary core on boot, or for configuring and enabling virtual memory
/// mapping on secondary cores (or on warm boot).
pub struct VirtualMemoryMapping {
    page_heap: UnsafeCell<[PageTable; PlatformImpl::PAGE_HEAP_PAGE_COUNT]>,
    idmap: UnsafeCell<MaybeUninit<IdMap>>,
    initialized: UnsafeCell<bool>,
}

impl VirtualMemoryMapping {
    /// Creates new instance.
    pub const fn new() -> Self {
        Self {
            page_heap: UnsafeCell::new([PageTable::EMPTY; PlatformImpl::PAGE_HEAP_PAGE_COUNT]),
            idmap: UnsafeCell::new(MaybeUninit::zeroed()),
            initialized: UnsafeCell::new(false),
        }
    }

    /// Initializes the page tables and enables virtual memory mapping.
    ///
    /// Panics if the MMU is enabled or the `idmap` is initialized.
    ///
    /// # Safety
    ///
    /// This function must be called once early in startup on the primary core, before anything else
    /// that depends on virtual memory mapping and caches. The MMU must be disabled when this
    /// function is called.
    pub unsafe fn init_and_enable(&self) {
        assert!(!read_sctlr_el3().contains(SctlrEl3::M));
        assert!(!self.is_initialized());

        // Safety: It is a valid pointer, and this is the only location where it is dereferenced.
        // The reference is then passed to `idmap`, which shares the same lifetime as `page_heap`.
        let pages = unsafe { &mut *self.page_heap.get() };

        // Safety: It is valid and aligned pointer. The caller promises that this function is called
        // once on primary core init, so this is the only place with mutable access to the variable.
        let idmap_uninit = unsafe { &mut *self.idmap.get() };

        idmap_uninit.write(init_page_table(pages));
        self.mark_initialzed();

        // Safety: The object has been initialized by an earlier step of this function.
        let idmap = unsafe { idmap_uninit.assume_init_ref() };

        // Safety: We pass the root address of `idmap`, which has just been initialised with
        // appropriate mappings, and will remain valid forever.
        unsafe {
            setup_mmu_cfg(idmap.root_address());
        }

        trace!("Page table: {idmap:?}");

        info!("Marking page table as active");
        idmap.mark_active();
    }

    /// Enables virtual memory mapping.
    ///
    /// Panics if the MMU is enabled or the `idmap` is not initialized.
    ///
    /// # Safety
    ///
    /// The function must be called with MMU disabled.
    pub unsafe fn enable(&self) {
        assert!(!read_sctlr_el3().contains(SctlrEl3::M));
        assert!(self.is_initialized());

        // Safety: It is a valid and aligned pointer and `is_initialized()` guarantees that `idmap`
        // has been initialized.
        let idmap = unsafe { (&*self.idmap.get()).assume_init_ref() };

        // Safety: We pass the root address of `idmap`, which has been initialised with appropriate
        // mappings, and will remain valid forever.
        unsafe {
            setup_mmu_cfg(idmap.root_address());
        }
    }

    /// Check whether self.idmap is initialized. Panics of the MMU is active.
    fn is_initialized(&self) -> bool {
        assert!(!read_sctlr_el3().contains(SctlrEl3::M));

        // Safety: It is a valid and aligned pointer and it has been initialized by `new()`. The
        // variable is only accessed when the MMU is disabled, so it always reads the system memory.
        unsafe { core::ptr::read(self.initialized.get()) }
    }

    /// Marks the self.idmap as initialized. Panics of the MMU is active.
    fn mark_initialzed(&self) {
        assert!(!read_sctlr_el3().contains(SctlrEl3::M));

        // Safety: It is a valid and aligned pointer. The variable is only accessed when the MMU is disabled,
        unsafe { core::ptr::write_volatile(self.initialized.get(), true) };

        isb();
        dsb_sy();
    }
}

// Safety: The callers of the functions of this object promise that the MMU is off, so all memory
// accesses are device memory accesses. `init_and_enable()` only called once on the primary core and
// this is the only function that can write the object. `enable()` only uses immutable reference
// to the internal objects and validates if they have been initialized properly. `initialized` only
// changes from `false` to `true` once, and this prevent `enable()` to use an invalid state.
unsafe impl Sync for VirtualMemoryMapping {}

/// Initialises and enables the page table.
///
/// # Safety
///
/// This function must be called once early in startup on the primary core, before anything else
/// that depends on virtual memory mapping and caches. The MMU must be disabled when this
/// function is called.
pub unsafe fn init() {
    // Safety: The same safety requirements are propagated to the caller.
    unsafe {
        VIRTUAL_MEMORY_MAPPING.init_and_enable();
    }
}

/// Enables the MMU for a newly booted core, assuming the page table is already initialised.
///
/// # Safety
///
/// The function must be called with MMU disabled.
pub unsafe fn enable() {
    // Safety: The same safety requirements are propagated to the caller.
    unsafe {
        VIRTUAL_MEMORY_MAPPING.enable();
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
    assert!(!sctlr.contains(SctlrEl3::M));

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

    fn mark_active(&self) {
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
    use crate::sysregs::fake::SYSREGS;

    use super::*;

    #[test]
    fn vmm_init_and_enable() {
        SYSREGS.lock().unwrap().sctlr_el3 = SctlrEl3::empty();

        let vmm = VirtualMemoryMapping::new();

        unsafe {
            vmm.init_and_enable();
        }

        // Act like a different core on boot.
        SYSREGS.lock().unwrap().sctlr_el3 = SctlrEl3::empty();
        unsafe {
            vmm.enable();
        }
    }

    #[test]
    #[should_panic]
    fn vmm_init_with_mmu() {
        SYSREGS.lock().unwrap().sctlr_el3 = SctlrEl3::M;

        let vmm = VirtualMemoryMapping::new();

        unsafe {
            vmm.init_and_enable();
        }
    }

    #[test]
    #[should_panic]
    fn vmm_init_twice() {
        SYSREGS.lock().unwrap().sctlr_el3 = SctlrEl3::empty();

        let vmm = VirtualMemoryMapping::new();

        unsafe {
            vmm.init_and_enable();
            vmm.init_and_enable();
        }
    }

    #[test]
    #[should_panic]
    fn vmm_enable_with_mmu() {
        SYSREGS.lock().unwrap().sctlr_el3 = SctlrEl3::M;

        let vmm = VirtualMemoryMapping::new();

        unsafe {
            vmm.enable();
        }
    }

    #[test]
    #[should_panic]
    fn vmm_enable_not_initialized() {
        SYSREGS.lock().unwrap().sctlr_el3 = SctlrEl3::empty();

        let vmm = VirtualMemoryMapping::new();

        unsafe {
            vmm.enable();
        }
    }
}
