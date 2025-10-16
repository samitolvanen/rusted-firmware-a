// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use core::{
    fmt::{Debug, Display},
    ops::{Deref, DerefMut, Index, IndexMut},
    ptr::{slice_from_raw_parts, slice_from_raw_parts_mut},
    slice::SliceIndex,
};

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Utility trait used to manipulate lists stored alongside with the [`RmmBootManifest`].
pub trait ManifestList {
    type Item: IntoBytes + FromBytes;

    /// Address of the start of the list.
    fn ptr(&self) -> u64;
    /// Number of elements present in the list.
    fn size(&self) -> u64;

    /// If present, return a mutable reference to the checksum field of this list.
    fn checksum(&mut self) -> Option<&mut u64>;
    /// Computes the sum of all the bytes referenced by this list.
    fn compute_sum_of_bytes(&self) -> u64;
    /// Computes the expected checksum of this list.
    fn compute_checksum(&self) -> u64 {
        if self.size() == 0 {
            0
        } else {
            u64::MAX - self.compute_sum_of_bytes() + 1
        }
    }

    /// Returns the content of the list as an immutable slice.
    fn as_slice(&self) -> &[Self::Item] {
        if self.size() == 0 {
            return &[];
        }

        // Safety: assuming that the initial `ManifestList` is valid, then the area pointed to is an
        // array of `self.size()` elements and lives for the same lifetime as the whole
        // manifest.
        //
        // `self.ptr()` can be 0 if and only if `self.size()` is 0, which is handled by the
        // condition above.
        unsafe { &*slice_from_raw_parts(self.ptr() as *const Self::Item, self.size() as usize) }
    }

    /// Returns the content of the list as a mutable slice, wrapped in `ChecksumWrapper`. This
    /// wrapping ensures that the checksum is correctly updated when the wrapper is dropped.
    fn as_slice_mut(&mut self) -> ChecksumWrapper<'_, Self>
    where
        Self: core::marker::Sized,
    {
        ChecksumWrapper { list: self }
    }

    /// The size in bytes of the content of the list.
    fn byte_size(&self) -> u64 {
        self.size() * size_of::<Self::Item>() as u64
    }
}

pub struct ChecksumWrapper<'a, T>
where
    T: ManifestList,
{
    list: &'a mut T,
}

impl<'a, T> Deref for ChecksumWrapper<'a, T>
where
    T: ManifestList,
{
    type Target = [T::Item];

    fn deref(&self) -> &Self::Target {
        self.list.as_slice()
    }
}

impl<'a, T> DerefMut for ChecksumWrapper<'a, T>
where
    T: ManifestList,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        if self.list.size() == 0 {
            return &mut [];
        }

        // Safety: assuming that the initial `ManifestList` is valid, then the area pointed to is an
        // array of `self.list.size()` elements and lives for the same lifetime as the whole
        // manifest.
        //
        // `self.list.ptr()` can be 0 if and only if `self.list.size()` is 0, which is handled by
        // the condition above.
        unsafe {
            &mut *slice_from_raw_parts_mut(
                self.list.ptr() as *mut T::Item,
                self.list.size() as usize,
            )
        }
    }
}

impl<'a, T, Idx> IndexMut<Idx> for ChecksumWrapper<'a, T>
where
    Idx: SliceIndex<[T::Item]>,
    T: ManifestList,
{
    fn index_mut(&mut self, index: Idx) -> &mut Self::Output {
        &mut self.deref_mut()[index]
    }
}

impl<'a, T, Idx> Index<Idx> for ChecksumWrapper<'a, T>
where
    Idx: SliceIndex<[T::Item]>,
    T: ManifestList,
{
    type Output = Idx::Output;

    fn index(&self, index: Idx) -> &Self::Output {
        &self.list.as_slice()[index]
    }
}
impl<'a, T> Drop for ChecksumWrapper<'a, T>
where
    T: ManifestList,
{
    fn drop(&mut self) {
        if self.list.checksum().is_some() {
            let new_checksum = self.list.compute_checksum();
            *self.list.checksum().unwrap() = new_checksum;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
/// The RMM-EL3 Boot Manifest v0.5 structure contains platform boot information passed from EL3 to
/// RMM. The size of the Boot Manifest is 160 bytes.
pub struct RmmBootManifest {
    /// Boot Manifest version.
    pub version: RmmBootManifestVersion,
    /// Reserved, set to 0.
    pub padding: [u8; 4],
    /// Pointer to Platform Data section.
    pub plat_data: u64,
    /// NS DRAM Layout Info structure.
    pub plat_dram: RmmMemoryBankList,
    /// List of consoles available to RMM.
    pub plat_console: RmmConsoleList,
    /// Device non-coherent ranges Info structure.
    pub plat_ncoh_region: RmmMemoryBankList,
    /// Device coherent ranges Info structure.
    pub plat_coh_region: RmmMemoryBankList,
    /// List of SMMUs available to RMM (from Boot Manifest v0.5).
    pub plat_smmu: RmmSmmuList,
    /// List of PCIe root complexes available to RMM (from Boot Manifest v0.5).
    pub plat_root_complex: RmmRootComplexList,
}

impl RmmBootManifest {
    /// Creates a new boot manifest in the given memory region.
    ///
    /// Field containing pointers to lists (such as `plat_dram`) will be initialized with the
    /// given size and the elements of the list will be stored in the provided buffer after the
    /// `RmmBootManifest` itself. The element themselves will be initialized from zeroed-out memory.
    pub fn new<'a>(
        buf: &'a mut [u8],
        dram_size: usize,
        console_size: usize,
        ncoh_region_size: usize,
        coh_region_size: usize,
        smmu_size: usize,
        root_complex_size: &'_ [&'_ [usize]],
    ) -> &'a mut Self {
        macro_rules! create_list {
            ($field:expr, $size:expr, $ptr:expr, checksum = true) => {
                create_list!($field, $size, $ptr, checksum = false);

                if $size != 0 {
                    let new_cs = $field.compute_checksum();
                    *$field.checksum().unwrap() = new_cs;
                }
            };
            ($field:expr, $size:expr, $ptr:expr, checksum = false) => {{
                if $size != 0 {
                    $field.num_entries = $size;
                    $field.entries_ptr = $ptr;
                    $ptr += $field.byte_size();
                }
            }};
        }

        // Checks that the buffer is exactly one page.
        let ptr = buf.as_ptr() as u64;
        assert!(ptr & 0xFFF == 0);
        assert!(buf.len() == 0x1000);

        buf.fill(0);
        let s = Self::mut_from_prefix(buf).unwrap().0;

        s.version = RMM_BOOT_MANIFEST_VERSION;

        let mut ptr = ptr + size_of::<Self>() as u64;

        // Initializes all the lists in the manifest with the requested size, leaving the entries
        // unitialized.
        create_list!(s.plat_dram, dram_size as u64, ptr, checksum = true);
        create_list!(s.plat_console, console_size as u64, ptr, checksum = true);
        create_list!(
            s.plat_ncoh_region,
            ncoh_region_size as u64,
            ptr,
            checksum = true
        );
        create_list!(
            s.plat_coh_region,
            coh_region_size as u64,
            ptr,
            checksum = true
        );
        create_list!(s.plat_smmu, smmu_size as u64, ptr, checksum = true);

        // Recursively create the root complex list and the lists it references.
        {
            create_list!(
                s.plat_root_complex,
                root_complex_size.len() as u64,
                ptr,
                checksum = false
            );
            let mut plat_root_complex = s.plat_root_complex.as_slice_mut();

            for (complex_idx, complex_size) in root_complex_size.iter().enumerate() {
                create_list!(
                    plat_root_complex[complex_idx],
                    complex_size.len() as u32,
                    ptr,
                    checksum = false
                );
                let mut port_slice = plat_root_complex[complex_idx].as_slice_mut();

                for (port_idx, port_size) in complex_size.iter().enumerate() {
                    create_list!(
                        port_slice[port_idx],
                        *port_size as u32,
                        ptr,
                        checksum = false
                    );
                }
            }
        }

        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Immutable, FromBytes, IntoBytes)]
/// Versioning information used in the RMM boot process.
pub struct RmmBootManifestVersion(pub u32);
impl RmmBootManifestVersion {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self((major as u32 & 0x7fff) << 16 | minor as u32)
    }

    pub const fn major(&self) -> u16 {
        (self.0 >> 16 & 0x7fff) as u16
    }

    pub const fn minor(&self) -> u16 {
        (self.0 & 0xffff) as u16
    }
}

impl Display for RmmBootManifestVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!("v{}.{}", self.major(), self.minor()))
    }
}

/// Current version of the `RmmBootManifest` struct.
pub const RMM_BOOT_MANIFEST_VERSION: RmmBootManifestVersion = RmmBootManifestVersion::new(0, 5);

/// As `zerocopy` does not support aligned generic structs, this macro is used to generate the
/// different list that the manifest contains.
macro_rules! make_rmm_list {
    ($name:ident, $elems:ty) => {
        #[derive(Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
        #[repr(C)] // Packed required as limitation of the zerocopy crate.
        #[doc = concat!(
                            "A checksummed pointer to a list of `",
                            stringify!($name),
                            "` elements, stored alongside with the original's `RmmBootManifest`."
                        )]
        pub struct $name {
            num_entries: u64,
            entries_ptr: u64,
            checksum: u64,
        }

        impl Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                let num = self.num_entries;
                let ptr = self.entries_ptr;
                let cs = self.checksum;

                f.debug_struct("RmmList")
                    .field("num_entries", &num)
                    .field("entries_ptr", &ptr)
                    .field("checksum", &cs)
                    .field("entries", &self.as_slice())
                    .finish()
            }
        }

        impl ManifestList for $name {
            type Item = $elems;

            fn checksum(&mut self) -> Option<&mut u64> {
                Some(&mut self.checksum)
            }

            fn compute_sum_of_bytes(&self) -> u64 {
                self.num_entries
                    + self.entries_ptr
                    + <[u64]>::ref_from_bytes(self.as_slice().as_bytes())
                        .unwrap()
                        .iter()
                        .copied()
                        .sum::<u64>()
            }

            fn ptr(&self) -> u64 {
                self.entries_ptr
            }

            fn size(&self) -> u64 {
                self.num_entries
            }
        }
    };
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
/// Memory Bank structure contains information about each memory bank/device region.
pub struct RmmMemoryBank {
    /// Base address.
    pub base: u64,
    /// Size of memory bank/device region in bytes.
    pub size: u64,
}
make_rmm_list!(RmmMemoryBankList, RmmMemoryBank);

#[derive(Debug, Copy, Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
/// Console Info structure contains information about each Console available to RMM.
pub struct RmmConsoleInfo {
    /// Console Base address.
    pub base: u64,
    /// Num of pages to map for console MMIO.
    pub map_pages: u64,
    /// Name of console.
    pub name: [u8; 8],
    /// UART clock (in Hz) for console.
    pub clk_in_hz: u64,
    /// Baud rate.
    pub baud_rate: u64,
    /// Additional flags (reserved, MBZ).
    pub flags: u64,
}
make_rmm_list!(RmmConsoleList, RmmConsoleInfo);

#[derive(Debug, Copy, Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
/// SMMU Info structure contains information about each SMMU available to RMM.
pub struct RmmSmmuInfo {
    /// SMMU Base address.
    pub smmu_base: u64,
    /// SMMU Realm Pages base address.
    pub smmu_r_base: u64,
}
make_rmm_list!(RmmSmmuList, RmmSmmuInfo);

#[derive(Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
pub struct RmmRootComplexList {
    num_entries: u64,
    /// Root Complex Info structure version.
    pub rc_info_version: RmmBootManifestVersion,
    /// Reserved, set to 0.
    pub padding: u32,
    entries_ptr: u64,
    checksum: u64,
}

impl Debug for RmmRootComplexList {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RmmRootComplexList")
            .field("num_entries", &self.num_entries)
            .field("rc_info_version", &self.rc_info_version)
            .field("padding", &self.padding)
            .field("entries_ptr", &self.entries_ptr)
            .field("checksum", &self.checksum)
            .field("entries", &self.as_slice())
            .finish()
    }
}

impl ManifestList for RmmRootComplexList {
    type Item = RmmRootComplexInfo;
    fn checksum(&mut self) -> Option<&mut u64> {
        Some(&mut self.checksum)
    }
    fn ptr(&self) -> u64 {
        self.entries_ptr
    }
    fn size(&self) -> u64 {
        self.num_entries
    }

    fn compute_sum_of_bytes(&self) -> u64 {
        self.num_entries
            + ((self.rc_info_version.0 as u64) << 32 | self.padding as u64)
            + self.entries_ptr
            + self
                .as_slice()
                .iter()
                .map(|s| s.compute_sum_of_bytes())
                .sum::<u64>()
    }
}

#[derive(Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
/// Root Complex Info structure contains information about each PCIe root complex available to RMM.
pub struct RmmRootComplexInfo {
    /// PCIe ECAM Base address.
    pub ecam_base: u64,
    /// PCIe segment identifier.
    pub segment: u8,
    /// Reserved, set to 0.
    pub padding: [u8; 3],
    num_entries: u32,
    entries_ptr: u64,
}

impl Debug for RmmRootComplexInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RmmRootComplexInfo")
            .field("ecam_base", &self.ecam_base)
            .field("segment", &self.segment)
            .field("padding", &self.padding)
            .field("num_entries", &self.num_entries)
            .field("entries_ptr", &self.entries_ptr)
            .field("entries", &self.as_slice())
            .finish()
    }
}
impl ManifestList for RmmRootComplexInfo {
    type Item = RmmRootPortInfo;

    fn ptr(&self) -> u64 {
        self.entries_ptr
    }

    fn size(&self) -> u64 {
        self.num_entries as u64
    }

    fn checksum(&mut self) -> Option<&mut u64> {
        None
    }

    fn compute_sum_of_bytes(&self) -> u64 {
        self.ecam_base
            + ((u32::from_le_bytes([
                self.segment,
                self.padding[0],
                self.padding[1],
                self.padding[2],
            ]) as u64)
                << 32
                | self.num_entries as u64)
            + self.entries_ptr
            + self
                .as_slice()
                .iter()
                .map(|s| s.compute_sum_of_bytes())
                .sum::<u64>()
    }
}

#[derive(Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
/// Root Complex Info structure contains information about each root port in PCIe root complex.
pub struct RmmRootPortInfo {
    /// Root Port identifier.
    pub root_port_id: u16,
    /// Reserved, set to 0.
    pub padding: u16,
    num_entries: u32,
    entries_ptr: u64,
}

impl Debug for RmmRootPortInfo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RmmRootPortInfo")
            .field("root_port_id", &self.root_port_id)
            .field("padding", &self.padding)
            .field("num_entries", &self.num_entries)
            .field("entries_ptr", &self.entries_ptr)
            .field("entries", &self.as_slice())
            .finish()
    }
}
impl ManifestList for RmmRootPortInfo {
    type Item = BdfMappingInfo;

    fn ptr(&self) -> u64 {
        self.entries_ptr
    }

    fn size(&self) -> u64 {
        self.num_entries as u64
    }

    fn checksum(&mut self) -> Option<&mut u64> {
        None
    }

    fn compute_sum_of_bytes(&self) -> u64 {
        ((self.root_port_id as u64) << 48 | (self.padding as u64) << 32 | self.num_entries as u64)
            + self.entries_ptr
            + <[u64]>::ref_from_bytes(self.as_slice().as_bytes())
                .unwrap()
                .iter()
                .copied()
                .sum::<u64>()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Immutable, FromBytes, IntoBytes, KnownLayout)]
#[repr(C)]
/// BDF Mapping Info structure contains information about each Device-Bus-Function (BDF) mapping for
/// PCIe root port.
pub struct BdfMappingInfo {
    /// Base of BDF mapping (inclusive).
    pub mapping_base: u16,
    /// Top of BDF mapping (exclusive).
    pub mapping_top: u16,
    /// Mapping offset, as per Arm Base System Architecture:
    /// StreamID = RequesterID[N-1:0] + (1<<N)*Constant_B.
    pub mapping_off: u16,
    /// SMMU index in [`plat_smmu`][RmmBootManifest::plat_smmu] array.
    pub smmu_idx: u16,
}
