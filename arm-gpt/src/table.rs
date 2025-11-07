// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use core::fmt::Debug;

use num_enum::TryFromPrimitive;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use crate::{Error, PA, l0gptsz, mask, pps};

macro_rules! field {
    ($end:literal : $start:literal, $name:ident, $ty:ty) => {
        #[doc = "Return the [`"]
        #[doc = stringify!($ty)]
        #[doc = "`] of this descriptor."]
        pub fn $name(&self) -> $ty {
            ((self.0.0 >> $start) & mask!($end - $start))
                .try_into()
                .unwrap()
        }
    };
}

#[derive(Debug)]
pub enum LeafDescriptorType {
    Block,
    Granule,
    Contig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u64)]
/// Access control restriction that can be applied to a memory region in the GPT.
pub enum GPIAccessType {
    /// Access granted to no World.
    NoAccess = 0b0000,
    /// Access granted to Secure World only.
    Secure = 0b1000,
    /// Access granted to NonSecure World only.
    NonSecure = 0b1001,
    /// Access granted to Root World only.
    Root = 0b1010,
    /// Access granted to Realm World only.
    Realm = 0b1011,
    /// Access granted to all Worlds.
    Any = 0b1111,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive)]
#[repr(u64)]
/// Possible sizes of a Contiguous Descriptor.
pub enum ContigSize {
    MB2 = 0b01,
    MB32 = 0b10,
    MB512 = 0b11,
}

impl ContigSize {
    pub const VALUES: [Self; 3] = [Self::MB2, Self::MB32, Self::MB512];

    pub fn allowed_shifts() -> impl Iterator<Item = usize> {
        Self::VALUES.iter().map(|v| v.shift())
    }

    /// Returns the bitshift corresponding to this size's alignment.
    pub const fn shift(&self) -> usize {
        match self {
            ContigSize::MB2 => 21,
            ContigSize::MB32 => 25,
            ContigSize::MB512 => 29,
        }
    }

    /// Returns the [`ContigSize`] corresponding to the given alignment.
    pub const fn from_shift(shift: usize) -> Self {
        match shift {
            21 => ContigSize::MB2,
            25 => ContigSize::MB32,
            29 => ContigSize::MB512,
            _ => panic!(),
        }
    }

    /// The size in bytes.
    pub const fn size(&self) -> usize {
        1 << self.shift()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, FromBytes, IntoBytes, KnownLayout, Immutable,
)]
#[repr(transparent)]
pub(crate) struct Level0Descriptor(pub(crate) u64);

impl Level0Descriptor {
    /// Tries to create a view of this Level 0 Descriptor as a Table Descriptor.
    ///
    /// Returns a [`TableDescriptorRef`] referencing `self`, or [`None`] if self does not contain
    /// a Table Descriptor.
    pub const fn as_table<'a>(&'a self) -> Option<TableDescriptorRef<'a>> {
        if self.0 & !mask!(52, 12) == 0b0011 {
            Some(TableDescriptorRef(self))
        } else {
            None
        }
    }

    /// Creates a Table Descriptor referencing the L1 table at `idx` in the `l1` buffer.
    pub fn table<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>(
        idx: usize,
        l1: *const Level1Table<L0_COUNT, L1_COUNT, PGS>,
    ) -> Self {
        {
            let ptr = idx * size_of::<Level1Table<L0_COUNT, L1_COUNT, PGS>>() + l1 as usize;
            Self(0b0011 | ptr as u64)
        }
    }

    /// Tries to create a view of this Level 0 Descriptor as a Block Descriptor.
    ///
    /// Returns a [`BlockDescriptorRef`] referencing `self`, or [`None`] if self does not contain
    /// a Block Descriptor.
    pub const fn as_block<'a>(&'a self) -> Option<BlockDescriptorRef<'a>> {
        if self.0 & !mask!(8, 4) == 0b0001 {
            Some(BlockDescriptorRef(self))
        } else {
            None
        }
    }

    /// Creates a Block Descriptor with the given [`GPIAccessType`].
    pub const fn block(gpi: GPIAccessType) -> Self {
        Self(0b0001 | (gpi as u64 & mask!(8 - 4)) << 4)
    }
}

/// View of a [`Level0Descriptor`] as a Table Descriptor.
pub(crate) struct TableDescriptorRef<'a>(&'a Level0Descriptor);

impl<'a> TableDescriptorRef<'a> {
    /// Returns the index of the table referenced by this descriptor within the provided L1 buffer.
    pub fn next_idx<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>(
        &self,
        l1: *const Level1Table<L0_COUNT, L1_COUNT, PGS>,
    ) -> usize {
        let ptr = self.0.0 as usize & !0b0011;
        (ptr - l1 as usize) / size_of::<Level1Table<L0_COUNT, L1_COUNT, PGS>>()
    }
}

/// View of a [`Level0Descriptor`] as a Block Descriptor.
pub(crate) struct BlockDescriptorRef<'a>(&'a Level0Descriptor);

impl<'a> BlockDescriptorRef<'a> {
    field!(8:4, gpi, GPIAccessType);
}

impl Debug for BlockDescriptorRef<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!("Block ({:?})", self.gpi(),))
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, FromBytes, IntoBytes, KnownLayout, Immutable,
)]
#[repr(transparent)]
pub(crate) struct Level1Descriptor(u64);

impl Level1Descriptor {
    /// Tries to create a view of this Level 1 Descriptor as a Contig Descriptor.
    ///
    /// Returns a [`ContiguousDescriptorRef`] referencing `self`, or [`None`] if self does not contain
    /// a Contig Descriptor.
    pub const fn as_contig<'a>(&'a self) -> Option<ContiguousDescriptorRef<'a>> {
        if self.0 & !(mask!(10, 8) | mask!(8, 4)) == 0b0001 {
            Some(ContiguousDescriptorRef(self))
        } else {
            None
        }
    }

    /// Creates a Contiguous Descriptor form the given size and gpi.
    pub const fn contig(size: ContigSize, gpi: GPIAccessType) -> Self {
        Self(0b0001 | (size as u64 & mask!(10 - 8)) << 8 | (gpi as u64 & mask!(8 - 4)) << 4)
    }

    /// Tries to create a view of this Level 1 Descriptor as a Granule Descriptor.
    ///
    /// Returns a [`GranuleDescriptorRef`] referencing `self`, or [`None`] if self does not contain
    /// a Granule Descriptor.
    pub fn as_granule<'a>(&'a self) -> Option<GranuleDescriptorRef<'a>> {
        if GPIAccessType::try_from(self.0 & 0xF).is_ok() {
            Some(GranuleDescriptorRef(self))
        } else {
            None
        }
    }

    /// Tries to create a view of this Level 1 Descriptor as a Granule Descriptor.
    ///
    /// Returns a [`GranuleDescriptorRefMut`] mutably referencing `self`, or [`None`] if self does not
    /// contain a Granule Descriptor.
    pub fn as_granule_mut<'a>(&'a mut self) -> Option<GranuleDescriptorRefMut<'a>> {
        if GPIAccessType::try_from(self.0 & 0xF).is_ok() {
            Some(GranuleDescriptorRefMut(self))
        } else {
            None
        }
    }

    /// Creates a Granule Descriptor from the given [`GPIAccessType`]s.
    pub const fn granule(gpis: &[GPIAccessType; 16]) -> Self {
        let mut s = Self(0);
        let mut granule = GranuleDescriptorRefMut(&mut s);

        let mut i = 0;
        while i < 16 {
            granule.set_gpi(i, gpis[i]);
            i += 1;
        }

        s
    }
}

/// View of a [`Level1Descriptor`] as a Contiguous Descriptor.
pub(crate) struct ContiguousDescriptorRef<'a>(&'a Level1Descriptor);

impl ContiguousDescriptorRef<'_> {
    field!(10:8, size, ContigSize);
    field!(8:4, gpi, GPIAccessType);
}

impl Debug for ContiguousDescriptorRef<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!(
            "Contiguous ({:?}, {:?})",
            self.size(),
            self.gpi(),
        ))
    }
}

/// View of a [`Level1Descriptor`] as a Granule Descriptor.
pub(crate) struct GranuleDescriptorRef<'a>(&'a Level1Descriptor);

impl<'a> GranuleDescriptorRef<'a> {
    /// Returns the [`GPIAccessType`] corresponding to the granule at index `idx`.
    pub fn gpi(&self, idx: usize) -> Option<GPIAccessType> {
        assert!(idx < 16);

        let start = idx * 4;

        ((self.0.0 >> start) & 0xF).try_into().ok()
    }

    /// Whether all Granules are mapped with [`GPIAccessType::NoAccess`].
    pub fn is_empty(&self) -> bool {
        (0..16).all(|idx| self.gpi(idx).is_none_or(|v| v == GPIAccessType::NoAccess))
    }
}

/// Mutable view of a [`Level1Descriptor`] as a Granule Descriptor.
pub(crate) struct GranuleDescriptorRefMut<'a>(&'a mut Level1Descriptor);

impl GranuleDescriptorRefMut<'_> {
    /// Updates the [`GPIAccessType`] for the granule at index `idx`.
    pub const fn set_gpi(&mut self, idx: usize, value: GPIAccessType) -> bool {
        assert!(idx < 16);

        let start = idx * 4;
        let value = value as u64;

        self.0.0 = (self.0.0 & !(0xF << start)) | ((value & 0xF) << start);

        true
    }

    /// Returns the [`GPIAccessType`] corresponding to the granule at index `idx`.
    pub fn gpi(&self, idx: usize) -> Option<GPIAccessType> {
        GranuleDescriptorRef(self.0).gpi(idx)
    }
}

#[derive(Debug, FromBytes, IntoBytes, KnownLayout, Immutable, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct Level0Table<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize> {
    pub entries: [Level0Descriptor; L0_COUNT],
}

impl<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>
    Level0Table<L0_COUNT, L1_COUNT, PGS>
{
    const PPS: usize = pps(L0_COUNT, L1_COUNT, PGS);
    const L0GPTSZ: usize = l0gptsz(L1_COUNT, PGS);

    /// Creates a Level 0 table in the given buffer.
    ///
    /// The table is initialized with full access granted to every world.
    pub fn new(buf: &mut [u8]) -> Result<&mut Self, Error> {
        let table = Level0Table::mut_from_bytes(buf).map_err(|_| Error::BadBuffer)?;

        table
            .entries
            .fill(Level0Descriptor::block(GPIAccessType::Any));

        Ok(table)
    }

    // Retreive the index referencing the given PA in the table.
    pub fn resolve(pa: PA) -> usize {
        (pa & mask!(Self::PPS)) >> Self::L0GPTSZ
    }
}

#[derive(Debug, FromBytes, IntoBytes, KnownLayout, Immutable, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct Level1Table<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize> {
    pub entries: [Level1Descriptor; L1_COUNT],
}

impl<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>
    Level1Table<L0_COUNT, L1_COUNT, PGS>
{
    const L0GPTSZ: usize = l0gptsz(L1_COUNT, PGS);

    /// Creates a new uninitialized array of Level 1 tables.
    pub fn new_slice(buf: &mut [u8]) -> Result<&mut [Self], Error> {
        if !buf.len().is_multiple_of(size_of::<Self>()) {
            return Err(Error::BadBuffer);
        }

        <[Self]>::mut_from_bytes(buf).map_err(|_| Error::BadBuffer)
    }

    // Retreive the index referencing the given PA in the table.
    pub fn resolve(pa: PA) -> usize {
        (pa & mask!(Self::L0GPTSZ)) >> (PGS + 4)
    }

    /// Returns the free list metadata.
    ///
    /// - `0`: Index of the next block of the free list.
    /// - `1`: Size (in L1 tables) of this block.
    ///
    /// The values are store in the two first descriptors of the table. The return values are
    /// undefined if this table is allocated.
    pub fn get_free_metadata(&self) -> (u64, u64) {
        (self.entries[0].0, self.entries[1].0)
    }

    /// Sets the free list metadata for a block starting on this L1 table.
    pub fn set_free_metadata(&mut self, next: u64, size: u64) {
        self.entries[0] = Level1Descriptor(next);
        self.entries[1] = Level1Descriptor(size);
    }
}

#[cfg(test)]
mod test {

    use crate::{
        Error, GPIAccessType,
        table::{ContigSize, Level0Descriptor, Level0Table, Level1Descriptor, Level1Table},
    };

    #[test]
    fn as_block_valid() {
        assert!(
            Level0Descriptor(0b0001)
                .as_block()
                .is_some_and(|b| b.gpi() == GPIAccessType::NoAccess)
        );

        assert!(
            Level0Descriptor(0b1111_0001)
                .as_block()
                .is_some_and(|b| b.gpi() == GPIAccessType::Any)
        );
    }

    #[test]
    fn as_block_invalid() {
        assert!(Level0Descriptor(0).as_block().is_none());
        assert!(Level0Descriptor(0b11_0000_0001).as_block().is_none());
    }

    #[test]
    fn create_block() {
        assert_eq!(
            Level0Descriptor::block(GPIAccessType::Realm),
            Level0Descriptor(0b1011_0001)
        );
    }

    #[test]
    fn as_table_valid() {
        assert!(Level0Descriptor(0x0001_dead_beef_0003).as_table().is_some());
    }

    #[test]
    fn as_table_invalid() {
        assert!(Level0Descriptor(0x0001_dead_beef_0001).as_table().is_none());
        assert!(Level0Descriptor(0x0001_dead_beef_0005).as_table().is_none());
        assert!(Level0Descriptor(0x1001_dead_beef_0003).as_table().is_none());
    }

    #[test]
    fn as_table_idx() {
        type L1 = Level1Table<32, 1024, 16>;

        let table_ptr = 0x1000_0000_0000usize;

        let desc = Level0Descriptor(0x1000_0000_2003).as_table().unwrap();
        assert_eq!(desc.next_idx(table_ptr as *const L1), 1);

        let desc = Level0Descriptor(0x1000_0001_0003).as_table().unwrap();
        assert_eq!(desc.next_idx(table_ptr as *const L1), 8);
    }

    #[test]
    fn create_table() {
        type L1 = Level1Table<32, 1024, 16>;

        let table_ptr = 0x1000_0000_0000usize;

        assert_eq!(
            Level0Descriptor::table(5, table_ptr as *const L1),
            Level0Descriptor(0x1000_0000_a003)
        );
    }

    #[test]
    fn as_contig_valid() {
        let desc = Level1Descriptor(0b11_1001_0001).as_contig().unwrap();

        assert_eq!(desc.size(), ContigSize::MB512);
        assert_eq!(desc.gpi(), GPIAccessType::NonSecure);
    }

    #[test]
    fn as_contig_invalid() {
        assert!(Level1Descriptor(0b11_1001_0000).as_contig().is_none());
        assert!(Level1Descriptor(0b1111_0000_0001).as_contig().is_none());
    }

    #[test]
    fn create_contig() {
        assert_eq!(
            Level1Descriptor::contig(ContigSize::MB2, GPIAccessType::Realm),
            Level1Descriptor(0b01_1011_0001)
        );
    }

    #[test]
    fn as_granule_valid() {
        let gpi = Level1Descriptor(0xB09F).as_granule().unwrap();

        assert_eq!(gpi.gpi(0), Some(GPIAccessType::Any));
        assert_eq!(gpi.gpi(1), Some(GPIAccessType::NonSecure));
        assert_eq!(gpi.gpi(2), Some(GPIAccessType::NoAccess));
        assert_eq!(gpi.gpi(3), Some(GPIAccessType::Realm));
    }

    #[test]
    fn as_granule_mut_valid() {
        let mut desc = Level1Descriptor(0xB09F);
        let gpi = desc.as_granule_mut().unwrap();

        assert_eq!(gpi.gpi(0), Some(GPIAccessType::Any));
        assert_eq!(gpi.gpi(1), Some(GPIAccessType::NonSecure));
        assert_eq!(gpi.gpi(2), Some(GPIAccessType::NoAccess));
        assert_eq!(gpi.gpi(3), Some(GPIAccessType::Realm));
    }

    #[test]
    fn as_granule_invalid() {
        assert!(Level1Descriptor(1).as_granule().is_none());
        assert!(Level1Descriptor(1).as_granule_mut().is_none());
    }

    #[test]
    fn granule_set() {
        let mut desc = Level1Descriptor(0xB09F);
        let mut gpi = desc.as_granule_mut().unwrap();

        gpi.set_gpi(7, GPIAccessType::Root);
        assert_eq!(gpi.0.0, 0x0000_0000_A000_B09F);

        gpi.set_gpi(1, GPIAccessType::Secure);
        gpi.set_gpi(14, GPIAccessType::Secure);
        assert_eq!(gpi.0.0, 0x0800_0000_A000_B08F);
    }

    #[test]
    fn create_granule() {
        assert_eq!(
            Level1Descriptor::granule(&[GPIAccessType::Secure; 16]).0,
            0x8888_8888_8888_8888
        );

        assert_eq!(
            Level1Descriptor::granule(&[
                GPIAccessType::NonSecure,
                GPIAccessType::Root,
                GPIAccessType::Any,
                GPIAccessType::Secure,
                GPIAccessType::NonSecure,
                GPIAccessType::Root,
                GPIAccessType::Any,
                GPIAccessType::Secure,
                GPIAccessType::NonSecure,
                GPIAccessType::Root,
                GPIAccessType::Any,
                GPIAccessType::Secure,
                GPIAccessType::NonSecure,
                GPIAccessType::Root,
                GPIAccessType::Any,
                GPIAccessType::Secure,
            ])
            .0,
            0x8FA9_8FA9_8FA9_8FA9
        );
    }

    #[test]
    fn granule_non_empty() {
        macro_rules! assert_non_empty {
            ($e:expr) => {
                assert!($e.as_granule().is_some_and(|v| !v.is_empty()))
            };
        }

        assert_non_empty!(Level1Descriptor::granule(&[GPIAccessType::Any; 16]));
        assert_non_empty!(Level1Descriptor(0xF000));
    }

    #[test]
    fn granule_empty() {
        macro_rules! assert_empty {
            ($e:expr) => {
                assert!($e.as_granule().is_none_or(|v| v.is_empty()))
            };
        }

        assert_empty!(Level1Descriptor::granule(&[GPIAccessType::NoAccess; 16]));
        assert_empty!(Level1Descriptor(0x3));
        assert_empty!(Level1Descriptor(0x30));
    }

    #[test]
    fn new_l0() {
        type L0 = Level0Table<32, 1024, 16>;
        const SIZE: usize = size_of::<L0>();

        let mut buf = [0u8; 3 * SIZE];

        let ptr = buf.as_ptr() as usize;
        let start_ptr = (ptr & !(SIZE - 1)) + SIZE;
        let start = start_ptr - ptr;
        let aligned = &mut buf[start..start + SIZE];

        let l0 = L0::new(aligned).unwrap();

        assert!(
            l0.entries
                .iter()
                .all(|e| *e == Level0Descriptor::block(GPIAccessType::Any))
        );
    }

    #[test]
    fn new_l0_init() {
        type L0 = Level0Table<32, 1024, 16>;
        const SIZE: usize = size_of::<L0>();

        let mut buf = [0u8; 3 * SIZE];

        buf[0..8].copy_from_slice(&Level0Descriptor::block(GPIAccessType::Root).0.to_le_bytes());

        let ptr = buf.as_ptr() as usize;
        let start_ptr = (ptr & !(SIZE - 1)) + SIZE;
        let start = start_ptr - ptr;
        let aligned = &mut buf[start..start + SIZE];

        let l0 = L0::new(aligned).unwrap();

        assert!(
            l0.entries
                .iter()
                .all(|e| *e == Level0Descriptor::block(GPIAccessType::Any))
        );
    }

    #[test]
    fn new_l0_invalid_buffer() {
        type L0 = Level0Table<32, 1024, 16>;
        const SIZE: usize = size_of::<L0>();

        let mut buf = [0u8; 3 * SIZE];

        let ptr = buf.as_ptr() as usize;
        let start_ptr = (ptr & !(SIZE - 1)) + SIZE;
        let start = start_ptr - ptr;
        let aligned = &mut buf[start..];

        // Buffer too small.
        assert_eq!(L0::new(&mut aligned[..(SIZE - 1)]), Err(Error::BadBuffer));

        // Buffer misaligned.
        assert_eq!(L0::new(&mut aligned[1..(SIZE + 1)]), Err(Error::BadBuffer));
    }

    #[test]
    fn new_l1() {
        type L1 = Level1Table<32, 1024, 16>;
        const SIZE: usize = size_of::<L1>();

        let mut buf = [0u8; 10 * SIZE];

        let ptr = buf.as_ptr() as usize;
        let start_ptr = (ptr & !(SIZE - 1)) + SIZE;
        let start = start_ptr - ptr;
        let aligned = &mut buf[start..start + 8 * SIZE];

        let l0 = L1::new_slice(aligned).unwrap();

        assert_eq!(l0.len(), 8);
    }

    #[test]
    fn new_l1_invalid_buffer() {
        type L1 = Level1Table<32, 1024, 16>;
        const SIZE: usize = size_of::<L1>();

        let mut buf = [0u8; 10 * SIZE];

        let ptr = buf.as_ptr() as usize;
        let start_ptr = (ptr & !(SIZE - 1)) + SIZE;
        let start = start_ptr - ptr;
        let aligned = &mut buf[start..];

        // Buffer not a multiple of SIZE.
        assert_eq!(
            L1::new_slice(&mut aligned[..8 * SIZE - 1]),
            Err(Error::BadBuffer)
        );

        // Buffer misaligned.
        assert_eq!(
            L1::new_slice(&mut aligned[1..8 * SIZE + 1]),
            Err(Error::BadBuffer)
        );
    }

    #[test]
    fn l1_get_free_metadata() {
        type L1 = Level1Table<32, 1024, 16>;

        let mut l1 = L1 {
            entries: [Level1Descriptor(0); 1024],
        };

        l1.entries[0].0 = 32;
        l1.entries[1].0 = 64;

        assert_eq!(l1.get_free_metadata(), (32, 64))
    }

    #[test]
    fn l1_set_free_metadata() {
        type L1 = Level1Table<32, 1024, 16>;

        let mut l1 = L1 {
            entries: [Level1Descriptor(0); 1024],
        };

        l1.set_free_metadata(32, 64);

        assert_eq!(l1.entries[0].0, 32);
        assert_eq!(l1.entries[1].0, 64);
    }

    #[test]
    fn l0_resolve() {
        assert_eq!(
            Level0Table::<{ 1 << 5 }, { 1 << 10 }, 16>::resolve(0xdead_beef),
            0x3
        );

        assert_eq!(
            Level0Table::<{ 1 << 12 }, { 1 << 20 }, 16>::resolve(0xf_1234_dead_beef),
            0xf12
        );
    }

    #[test]
    fn l1_resolve() {
        assert_eq!(
            Level1Table::<{ 1 << 5 }, { 1 << 10 }, 16>::resolve(0xdead_beef),
            0x1ea
        );

        assert_eq!(
            Level1Table::<{ 1 << 12 }, { 1 << 20 }, 16>::resolve(0xf_1234_dead_beef),
            0x34_dea
        );
    }

    #[test]
    #[should_panic]
    fn contig_invalid_shift() {
        ContigSize::from_shift(30);
    }
}
