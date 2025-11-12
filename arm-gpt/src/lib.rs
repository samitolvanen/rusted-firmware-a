// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

#![cfg_attr(not(test), no_std)]

use core::{fmt::Debug, ops::Range};

use arm_sysregs::rme::GpccEl3;
#[cfg(all(target_arch = "aarch64", not(test)))]
use arm_sysregs::rme::*;
use spin::mutex::SpinMutex;
use thiserror::Error;

pub use crate::table::GPIAccessType;
use crate::table::{
    ContigSize, LeafDescriptorType, Level0Descriptor, Level0Table, Level1Descriptor, Level1Table,
};

extern crate core;

mod table;

pub type PA = usize;

#[derive(Debug, Error, PartialEq, Eq)]
/// Errors returned when manipulating the [`GranuleProtection`] object.
pub enum Error {
    #[error("Tried to access uninitialized GranuleProtection")]
    GPTNotInitialized,
    #[error("GPT buffer has invalid size")]
    BadBuffer,
    #[error("Out of memory")]
    OutOfMemory,
}

macro_rules! mask {
    ($end:expr, $start:expr) => {
        (mask!($end) & !mask!($start))
    };
    ($len:expr) => {
        ((1 << $len) - 1)
    };
}
pub(crate) use mask;

fn range_intersection(a: &Range<usize>, b: &Range<usize>) -> Range<usize> {
    a.start.max(b.start)..a.end.min(b.end)
}

#[derive(Debug)]
struct GranuleProtectionState<'life, const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>
{
    level0: &'life mut Level0Table<L0_COUNT, L1_COUNT, PGS>,
    level1: &'life mut [Level1Table<L0_COUNT, L1_COUNT, PGS>],
    free: Option<usize>,
}

#[derive(Debug)]
/// Result of shattering a Block or Contiguous Descriptor.
struct ShatterResult {
    /// Whether the whole `set()` operation is completed.
    done: bool,
    /// Range of the descriptor which was shattered.
    ///
    /// Since the shatter operation rewrites the range overlapping between the range requested by
    /// `set()` and the descriptor range, the range specified by this field should be ignored by
    /// subsequent writes.
    processed_range: Range<usize>,
}

impl ShatterResult {
    /// Creates a [`ShatterResult`] for the descriptor `desc` and `range` requested by the `set()`
    /// operation.
    fn from_ranges(desc: Range<usize>, range: Range<usize>) -> Self {
        Self {
            done: desc.start <= range.start && range.end <= desc.end,
            processed_range: desc,
        }
    }
}

impl<'life, const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>
    GranuleProtectionState<'life, L0_COUNT, L1_COUNT, PGS>
{
    const PPS: usize = pps(L0_COUNT, L1_COUNT, PGS);
    const L0GPTSZ: usize = l0gptsz(L1_COUNT, PGS);

    fn l0_resolve(pa: PA) -> usize {
        Level0Table::<L0_COUNT, L1_COUNT, PGS>::resolve(pa)
    }

    fn l1_resolve(pa: PA) -> usize {
        Level1Table::<L0_COUNT, L1_COUNT, PGS>::resolve(pa)
    }

    /// Allocates a L1 table from the level 1 buffer, returning its index.
    ///
    /// The content of the table is left uninitialized.
    fn allocate_l1(&mut self) -> Result<usize, Error> {
        let Some(free) = self.free else {
            return Err(Error::OutOfMemory);
        };

        let (next, size) = self.level1[free].get_free_metadata();

        if size > 1 {
            self.level1[free + 1].set_free_metadata(next, size - 1);
            self.free = Some(free + 1);
        } else if next != u64::MAX {
            self.free = Some(next as usize);
        } else {
            self.free = None;
        }

        Ok(free)
    }

    /// Marks a L1 table as unused.
    fn free_l1(&mut self, l1: usize) {
        self.level1[l1].set_free_metadata(self.free.map(|f| f as u64).unwrap_or(u64::MAX), 1);

        self.free = Some(l1);
    }

    /// Writes the descriptors corresponding to the given range into the provided L1 table.
    ///
    /// The range must be fully contained within the L1 table, otherwise the function will panic.
    fn write_l1(
        l1: &mut Level1Table<L0_COUNT, L1_COUNT, PGS>,
        range: Range<PA>,
        gpi: GPIAccessType,
    ) {
        for (pa, size, ty) in Self::break_pa_range(range) {
            match ty {
                LeafDescriptorType::Block => {
                    panic!();
                }
                LeafDescriptorType::Granule => {
                    let l1_entry = &mut l1.entries[Self::l1_resolve(pa)];

                    let mut gran = if let Some(gran) = l1_entry.as_granule_mut() {
                        gran
                    } else {
                        *l1_entry = Level1Descriptor::granule(&[GPIAccessType::NoAccess; 16]);
                        l1_entry.as_granule_mut().unwrap()
                    };

                    let gran_idx = (pa >> PGS) & 0xF;
                    gran.set_gpi(gran_idx, gpi);
                }
                LeafDescriptorType::Contig => {
                    let l1_idx = Self::l1_resolve(pa);
                    let desc = Level1Descriptor::contig(ContigSize::from_shift(size), gpi);

                    l1.entries[l1_idx..l1_idx + (1 << (size - PGS - 4))].fill(desc);
                }
            }
        }
    }

    /// Shatters a descriptor to create space for a new region mapping.
    ///
    /// - `range`: The range where the new mapping will be inserted.
    /// - `at_end`: Whether the shatter operation should occur at the start or the end of the range.
    /// - `gpi`: The [`GPIAccessType`] for the new region mapping.
    ///
    /// The shatter operation will identify any overlapping Contiguous/Block descriptor at the start
    /// (or end, depending on `at_end`) of the requested range and break it down into smaller
    /// descriptors. Then, it will update the range overlapping between the requested and descriptor
    /// range to instead use the new provided `gpi`.
    ///
    /// If the requested range is fully contained within the descriptor, [`ShatterResult::done`]
    /// will indicate to the caller that nothing else needs to be done.
    fn shatter(
        &mut self,
        range: Range<PA>,
        at_end: bool,
        gpi: GPIAccessType,
    ) -> Result<ShatterResult, Error> {
        let pa = if at_end { range.end } else { range.start };

        let no_op = Ok(ShatterResult {
            done: false,
            processed_range: pa..pa,
        });

        // If the address is aligned with a L0 entry, no shatter is needed.
        if pa & mask!(Self::L0GPTSZ) == 0 {
            return no_op;
        }

        let l0_idx = Self::l0_resolve(pa);

        if let Some(block) = self.level0.entries[l0_idx].as_block() {
            let block_start = pa & !mask!(Self::L0GPTSZ);
            let block_end = block_start + (1 << Self::L0GPTSZ);
            let block_gpi = block.gpi();

            // Allocate a new L1 table where the new descriptors will be written.
            let l1_idx = self.allocate_l1()?;
            let l1 = &mut self.level1[l1_idx];

            // Update the whole Block Descriptor range to store the requested GPI in the requested range and the Block GPI in the rest. Since the Block range covers the full L1 table, after these operations the table is completely initialized.
            Self::write_l1(l1, block_start..range.start, block_gpi);
            Self::write_l1(l1, range.end..block_end, block_gpi);
            Self::write_l1(
                l1,
                range_intersection(&range, &(block_start..block_end)),
                gpi,
            );

            // Writes the new Table Descriptor.
            self.level0.entries[l0_idx] = Level0Descriptor::table(l1_idx, self.level1.as_ptr());

            return Ok(ShatterResult::from_ranges(block_start..block_end, range));
        } else if let Some(table) = self.level0.entries[l0_idx].as_table() {
            let l1 = table.next_idx(self.level1.as_ptr());
            let l1 = &mut self.level1[l1];

            if let Some(contig) = l1.entries[Self::l1_resolve(pa)].as_contig() {
                // If the address is at the start of the Contig, there is no need to shatter it as it will either get fully rewritten (if pa is range.start) or left untouched (if pa is range.end).
                if pa & mask!(contig.size().shift()) == 0 {
                    return no_op;
                }

                let contig_start = pa & !mask!(contig.size().shift());
                let contig_end = contig_start + contig.size().size();
                let contig_gpi = contig.gpi();

                Self::write_l1(l1, contig_start..range.start, contig_gpi);
                Self::write_l1(l1, range.end..contig_end, contig_gpi);
                Self::write_l1(
                    l1,
                    range_intersection(&range, &(contig_start..contig_end)),
                    gpi,
                );

                return Ok(ShatterResult::from_ranges(contig_start..contig_end, range));
            }
        }

        no_op
    }

    fn set(&mut self, range: Range<PA>, gpi: GPIAccessType) -> Result<(), Error> {
        let shatter_left = self.shatter(range.clone(), false, gpi)?;
        if shatter_left.done {
            return Ok(());
        }

        let shatter_right = self.shatter(range, true, gpi)?;

        let inner_range = shatter_left.processed_range.end..shatter_right.processed_range.start;

        self.write_descs(inner_range, gpi);

        Ok(())
    }

    /// Get the Level 1 table corresponding to the given PA. The PA must resolve to an Level 1 table.
    fn get_l1(&mut self, pa: PA) -> &mut Level1Table<L0_COUNT, L1_COUNT, PGS> {
        let l0_idx = Self::l0_resolve(pa);
        let l0_entry = self.level0.entries[l0_idx].as_table().unwrap();
        &mut self.level1[l0_entry.next_idx(self.level1.as_ptr())]
    }

    /// Writes the leaf descriptors for the given range.
    ///
    /// Level 1 tables allocation management must already have been done. This means that if the
    /// range requires to write
    /// -  an L1 descriptor in an invalid or block L0 entry, the function will panic;
    /// -  a block descriptor over a table entry, the table will be leaked.
    fn write_descs(&mut self, range: Range<PA>, gpi: GPIAccessType) {
        for (pa, size, ty) in Self::break_pa_range(range) {
            match ty {
                LeafDescriptorType::Block => {
                    let l0_idx = Self::l0_resolve(pa);
                    let l0_entry = &mut self.level0.entries[l0_idx];

                    let to_free = l0_entry
                        .as_table()
                        .map(|e| e.next_idx(self.level1.as_ptr()));

                    *l0_entry = Level0Descriptor::block(gpi);

                    if let Some(to_free) = to_free {
                        self.free_l1(to_free);
                    }
                }
                LeafDescriptorType::Granule => {
                    let l1 = self.get_l1(pa);
                    let l1_entry = &mut l1.entries[Self::l1_resolve(pa)];

                    let mut gran = if let Some(gran) = l1_entry.as_granule_mut() {
                        gran
                    } else {
                        *l1_entry = Level1Descriptor::granule(&[GPIAccessType::NoAccess; 16]);
                        l1_entry.as_granule_mut().unwrap()
                    };

                    let gran_idx = (pa >> PGS) & 0xF;
                    gran.set_gpi(gran_idx, gpi);
                }
                LeafDescriptorType::Contig => {
                    let l1 = self.get_l1(pa);
                    let l1_idx = Self::l1_resolve(pa);
                    let desc = Level1Descriptor::contig(ContigSize::from_shift(size), gpi);

                    l1.entries[l1_idx..l1_idx + (1 << (size - PGS - 4))].fill(desc);
                }
            }
        }
    }

    /// Breaks down a PA range into chunks such that their size can fit in exactly one descriptor.
    ///
    /// The returned iterator yields tuples with:
    ///  - the starting PA for the descriptor;
    ///  - the log2 of its size;
    ///  - the type of the descriptor matching the size.
    fn break_pa_range(pas: Range<PA>) -> impl Iterator<Item = (PA, usize, LeafDescriptorType)> {
        struct Iter<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize> {
            current: PA,
            end: PA,
        }

        impl<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize> Iterator
            for Iter<L0_COUNT, L1_COUNT, PGS>
        {
            type Item = (PA, usize, LeafDescriptorType);

            fn next(&mut self) -> Option<Self::Item> {
                if self.current >= self.end {
                    return None;
                }

                let l0gptsz: usize = l0gptsz(L1_COUNT, PGS);

                // To compute the next chunk, we must ensure that the range
                //   1. starts aligned with the chunk size (e.g. a 0x1000 chunk can only be
                //      constructed if the start of the range's alignment is >= 12);
                //   2. is large enough to contain the chunk.
                //
                // We first compute the alignment of the start and the maximum chunk size that the
                // range can contain. Both are computed in terms of bitshift.
                // Then, we take the minimum of both values, which correspond to the log2 of the
                // maximum chunk that can be constructed. Finally, this value is
                // rounded down to match the size of a descriptor.

                let rem_size_shift =
                    (u64::BITS - (self.end - self.current).leading_zeros() - 1) as usize;
                let current_alignment = self.current.trailing_zeros() as usize;
                let max_size = rem_size_shift.min(current_alignment);

                // Identifies which descriptor type best matches the chunk's size.
                //
                // TODO(perf): maybe the iterator could return a size larger than the descriptor's
                // given that it is a strict multiple of it (ensured since both sizes are power of
                // two). It would avoid having to do the whole computation multiple times for
                // contiguous same-sized descriptors.
                let (ty, size);
                if max_size >= l0gptsz {
                    // Create a block descriptor.
                    (ty, size) = (LeafDescriptorType::Block, l0gptsz);
                } else {
                    // Create a Contig descriptor and reduces the chunk size to match one of those
                    // allowed. If the chunk is to small, instead create a Granule descriptor.

                    if let Some(max) = ContigSize::allowed_shifts()
                        .filter(|s| *s <= max_size)
                        .max()
                    {
                        (ty, size) = (LeafDescriptorType::Contig, max);
                    } else {
                        (ty, size) = (LeafDescriptorType::Granule, PGS)
                    }
                }

                let next = (self.current, size, ty);

                self.current += 1 << size;

                Some(next)
            }
        }

        Iter::<L0_COUNT, L1_COUNT, PGS> {
            current: pas.start,
            end: pas.end,
        }
    }
}

/// Handle to manipulate Granule Protection Tables and related registers.
///
/// See [`declare_granule_protection`] for instantiating it.
///
/// Before any other operation, the [`GranuleProtection`] object must be initialized exactly once
/// with two backing buffers by the [`GranuleProtection::init`] function. Once the memory regions
/// are mapped using [`GranuleProtection::set`], [`GranuleProtection::enable`] can be used to update
/// the sytem registers accordingly.
pub struct GranuleProtection<'life, const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>(
    SpinMutex<Option<GranuleProtectionState<'life, L0_COUNT, L1_COUNT, PGS>>>,
);

impl<'life, const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>
    GranuleProtection<'life, L0_COUNT, L1_COUNT, PGS>
{
    const PPS: usize = pps(L0_COUNT, L1_COUNT, PGS);
    const L0GPTSZ: usize = l0gptsz(L1_COUNT, PGS);

    /// This function should never be used explicitly.
    ///
    /// See [`declare_granule_protection`] for instantiating a [`GranuleProtection`].
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self(SpinMutex::new(None))
    }

    /// Initializes the Granule Protection Table with two backing buffers:
    ///
    /// - `l0buf`, which must be of size `8 * (1 << (PPS - L0GPTSZ))`, will contain the Level 0
    ///   Table.
    /// - `l1buf`, whose size must be a multiple of `8 * (1 << (L0GPTSZ - (PGS + 4)))`, will contain
    ///   the Level 1 Tables.
    pub fn init(&self, l0buf: &'life mut [u8], l1buf: &'life mut [u8]) -> Result<(), Error> {
        let mut state = self.0.lock();

        // TODO: check if gpt is already initialized

        let gpt = GranuleProtectionState {
            level0: Level0Table::new(l0buf)?,
            level1: Level1Table::new_slice(l1buf)?,
            free: Some(0),
        };

        gpt.level1[0].set_free_metadata(u64::MAX, gpt.level1.len() as u64);

        *state = Some(gpt);

        Ok(())
    }

    /// Insert a new access control mapping into the GPT.
    ///
    /// - `range`: Range governed by the new mapping.
    /// - `gpi`: Access restriction to apply to the mapping.
    ///
    /// [init][`Self::init`] must be called before this function is used (see
    /// [`GranuleProtection`]), otherwise this function will panic.
    pub fn set(&self, range: Range<PA>, gpi: GPIAccessType) -> Result<(), Error> {
        self.0
            .lock()
            .as_mut()
            .ok_or(Error::GPTNotInitialized)?
            .set(range, gpi)
    }
}

#[macro_export]
/// Declares a static instance of [`GranuleProtection`], using the given size parameters.
macro_rules! declare_granule_protection {
    ($name:ident, PPS = $PPS:literal, L0GPTSZ = $L0GPTSZ:literal, PGS = $PGS:literal) => {
        static $name: $crate::GranuleProtection<
            'static,
            { 1 << ($PPS - $L0GPTSZ) },
            { 1 << ($L0GPTSZ - ($PGS + 4)) },
            $PGS,
        > = $crate::GranuleProtection::new();
    };
}

pub(crate) const fn pps(l1_count: usize, l0_count: usize, pgs: usize) -> usize {
    l0_count.trailing_zeros() as usize + l0gptsz(l1_count, pgs)
}
pub(crate) const fn l0gptsz(l1_count: usize, pgs: usize) -> usize {
    l1_count.trailing_zeros() as usize + pgs + 4
}

#[cfg(all(target_arch = "aarch64", not(test)))]
pub struct GpccConfig {
    pub appsaa: bool,
    pub nso: bool,
    pub tbgpcd: bool,
    pub gpcp: bool,
    pub sh: Shareability,
    pub orgn: Cacheability,
    pub irgn: Cacheability,
    pub spad: bool,
    pub nspad: bool,
    pub rlpad: bool,
}

#[cfg(all(target_arch = "aarch64", not(test)))]
impl GpccConfig {
    fn to_reg(&self) -> GpccEl3 {
        let mut reg = GpccEl3::empty();

        if self.appsaa {
            reg |= GpccEl3::APPSAA
        }

        if self.nso {
            reg |= GpccEl3::NSO
        }

        if self.tbgpcd {
            reg |= GpccEl3::TBGPCD
        }

        if self.gpcp {
            reg |= GpccEl3::GPCP
        }

        if self.spad {
            reg |= GpccEl3::SPAD
        }

        if self.nspad {
            reg |= GpccEl3::NSPAD
        }

        if self.rlpad {
            reg |= GpccEl3::RLPAD
        }

        reg.set_sh(self.sh);
        reg.set_orgn(self.orgn);
        reg.set_irgn(self.irgn);

        reg
    }
}

impl<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize> Debug
    for GranuleProtection<'_, L0_COUNT, L1_COUNT, PGS>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        struct Key {
            value: usize,
            width: usize,
            dashes: usize,
        }

        impl Debug for Key {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_fmt(format_args!(
                    "{}{:->dashes$}{:0width$x}",
                    if self.dashes == 0 { "0x" } else { "--" },
                    "",
                    self.value,
                    dashes = self.dashes,
                    width = self.width
                ))
            }
        }

        let lock = self.0.lock();
        let pps = pps(L1_COUNT, L0_COUNT, PGS);
        let l0gptsz: usize = l0gptsz(L1_COUNT, PGS);

        let mut map = f.debug_map();
        map.entries([("PPS", pps), ("L0GPTSZ", l0gptsz), ("PGS", PGS)]);

        if let Some(gpt) = lock.as_ref() {
            for (l0_idx, l0_entry) in gpt.level0.entries.iter().enumerate() {
                map.key(&Key {
                    value: l0_idx << l0gptsz,
                    width: pps.div_ceil(4),
                    dashes: 0,
                });

                if let Some(blk) = l0_entry.as_block() {
                    map.value(&blk);
                } else if let Some(t) = l0_entry.as_table() {
                    let l1 = &gpt.level1[t.next_idx(gpt.level1.as_ptr())];

                    map.value(&"Table");

                    let mut it = l1.entries.iter().enumerate();

                    while let Some((l1_idx, l1_entry)) = it.next() {
                        map.key(&Key {
                            value: l1_idx << (PGS + 4),
                            width: l0gptsz.div_ceil(4),
                            dashes: pps.div_ceil(4) - l0gptsz.div_ceil(4),
                        });

                        if let Some(contig) = l1_entry.as_contig() {
                            map.value(&contig);

                            for _ in 1..(1 << (contig.size().shift() - PGS - 4)) {
                                it.next();
                            }
                        } else if let Some(gran) = l1_entry.as_granule()
                            && !gran.is_empty()
                        {
                            map.value(&"Granule");

                            for i in 0..16 {
                                map.entry(
                                    &Key {
                                        value: i << PGS,
                                        width: PGS.div_ceil(4) + 1,
                                        dashes: pps.div_ceil(4) - (PGS.div_ceil(4) + 1),
                                    },
                                    &gran.gpi(i).unwrap(),
                                );
                            }
                        }
                    }
                } else {
                    map.value(&"Invalid");
                }
            }

            map.finish()
        } else {
            f.debug_struct("GranuleProtectionTable")
                .field("state", &"Uninitialized")
                .finish()
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        Error, GPIAccessType, GranuleProtection,
        table::{ContigSize, Level0Descriptor, Level1Descriptor, Level1Table},
    };

    /// Dynamically allocates a 'static buffer for `elems` entries of `size` bytes. The resulting
    /// buffer is aligned on `size`.
    fn align(slice: &mut [u8], size: usize, elems: usize) -> &mut [u8] {
        let ptr = slice.as_ptr() as usize;
        let start_ptr = (ptr & !(size - 1)) + size;
        let start = start_ptr - ptr;

        &mut slice[start..start + size * elems]
    }
    macro_rules! declare_local_granule_protection {
        ($name:ident, $PPS:literal, $L0GPTSZ:literal, $PGS:literal, $l1_size:literal) => {
            let $name: $crate::GranuleProtection<
                { 1 << ($PPS - $L0GPTSZ) },
                { 1 << ($L0GPTSZ - ($PGS + 4)) },
                $PGS,
            > = $crate::GranuleProtection::new();

            let mut l0 = vec![0; (8 << ($PPS - $L0GPTSZ)) * 2];
            let mut l0 = align(&mut l0, 8 << ($PPS - $L0GPTSZ), 1);

            let mut l1 = vec![0; (8 << ($L0GPTSZ - ($PGS + 4))) * ($l1_size + 1)];
            let mut l1 = align(&mut l1, 8 << ($L0GPTSZ - ($PGS + 4)), $l1_size);

            $name.init(&mut l0, &mut l1)?;
        };
    }

    enum L0Matcher<'a> {
        Block(GPIAccessType),
        Table(&'a [(usize, L1Matcher<'a>)]),
    }

    enum L1Matcher<'a> {
        Contig(ContigSize, GPIAccessType),
        GranulePart(&'a [(usize, GPIAccessType)]),
        Granule([GPIAccessType; 16]),
    }

    use L0Matcher::*;
    use L1Matcher::*;

    fn assert_gpt_eq<'a, const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>(
        gpt: &GranuleProtection<L0_COUNT, L1_COUNT, PGS>,
        expected: &'a [(usize, L0Matcher<'a>)],
        free_count: usize,
    ) {
        fn free_list_size<const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>(
            free: Option<usize>,
            level1: &[Level1Table<L0_COUNT, L1_COUNT, PGS>],
        ) -> usize {
            let Some(mut free) = free else {
                return 0;
            };

            let mut count = 0;

            while free != u64::MAX as usize {
                let (next, size) = level1[free].get_free_metadata();

                free = next as usize;
                count += size as usize;
            }

            count
        }

        fn match_l1<'a>(entry: &Level1Descriptor, matcher: &L1Matcher<'a>) {
            match matcher {
                Contig(size, gpi) => {
                    let contig = entry.as_contig().unwrap();

                    assert_eq!(contig.gpi(), *gpi);
                    assert_eq!(contig.size(), *size);
                }
                GranulePart(items) => {
                    let granule = entry.as_granule().unwrap();

                    for (i, gran) in *items {
                        assert_eq!(granule.gpi(*i).unwrap_or(GPIAccessType::Any), *gran)
                    }
                }
                Granule(items) => {
                    let granule = entry.as_granule().unwrap();

                    for (i, gran) in items.iter().enumerate() {
                        assert_eq!(granule.gpi(i).unwrap_or(GPIAccessType::Any), *gran)
                    }
                }
            }
        }

        fn match_l0<'a, const L0_COUNT: usize, const L1_COUNT: usize, const PGS: usize>(
            entry: &Level0Descriptor,
            level1: &[Level1Table<L0_COUNT, L1_COUNT, PGS>],
            matcher: &L0Matcher<'a>,
        ) {
            match matcher {
                Block(gpi) => {
                    let blk = entry.as_block().unwrap();

                    assert_eq!(blk.gpi(), *gpi)
                }
                Table(items) => {
                    let table = entry.as_table().unwrap();

                    let l1 = &level1[table.next_idx(level1.as_ptr())];

                    let mut l1_matcher_iter = items.iter().peekable();

                    let mut l1_idx = 0;

                    while l1_idx < l1.entries.len() {
                        let l1_entry = &l1.entries[l1_idx];

                        if l1_matcher_iter.peek().is_some_and(|(i, _)| *i == l1_idx) {
                            let (_, l1_matcher) = l1_matcher_iter.next().unwrap();

                            match_l1(l1_entry, l1_matcher);
                        } else {
                            assert!(
                                l1_entry
                                    .as_contig()
                                    .is_some_and(|e| e.gpi() == GPIAccessType::Any)
                                    || l1_entry.as_granule().is_some_and(|g| g.is_all())
                            );
                        }

                        let size = l1_entry
                            .as_contig()
                            .map(|c| c.size().size() >> (PGS + 4))
                            .unwrap_or(1);

                        // Check that no contig overlaps with the descriptor at `l1_idx`.
                        assert!(
                            l1.entries[l1_idx + 1..l1_idx + size]
                                .iter()
                                .all(|e| e == l1_entry)
                        );

                        l1_idx += size;
                    }
                }
            }
        }

        let lock = gpt.0.lock();
        let state = lock.as_ref().unwrap();

        let mut l0_matcher_iter = expected.iter().peekable();
        for (l0_idx, l0_entry) in state.level0.entries.iter().enumerate() {
            if l0_matcher_iter.peek().is_some_and(|(i, _)| *i == l0_idx) {
                let (_, l0_matcher) = l0_matcher_iter.next().unwrap();

                match_l0(l0_entry, state.level1, l0_matcher);
            } else {
                assert!(
                    l0_entry
                        .as_block()
                        .is_some_and(|b| b.gpi() == GPIAccessType::Any)
                );
            }
        }

        assert_eq!(free_list_size(state.free, state.level1), free_count);
    }

    #[test]
    fn one_block() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0..0x40000000, GPIAccessType::Root)?;

        assert_gpt_eq(&gpt, &[(0, Block(GPIAccessType::Root))], 32);

        Ok(())
    }

    #[test]
    fn one_contig() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0..0x20_0000, GPIAccessType::Root)?;

        assert_gpt_eq(
            &gpt,
            &[(
                0,
                Table(&[(0, Contig(ContigSize::MB2, GPIAccessType::Root))]),
            )],
            31,
        );

        Ok(())
    }

    #[test]
    fn one_granule() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0..0x1_0000, GPIAccessType::Root)?;

        assert_gpt_eq(
            &gpt,
            &[(0, Table(&[(0, GranulePart(&[(0, GPIAccessType::Root)]))]))],
            31,
        );

        Ok(())
    }

    #[test]
    fn multi_desc() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0x1c00_0000..0x8204_0000, GPIAccessType::Root)?;

        assert_gpt_eq(
            &gpt,
            &[
                (
                    0,
                    Table(&[
                        (448, Contig(ContigSize::MB32, GPIAccessType::Root)),
                        (480, Contig(ContigSize::MB32, GPIAccessType::Root)),
                        (512, Contig(ContigSize::MB512, GPIAccessType::Root)),
                    ]),
                ),
                (1, Block(GPIAccessType::Root)),
                (
                    2,
                    Table(&[
                        (0, Contig(ContigSize::MB32, GPIAccessType::Root)),
                        (
                            32,
                            GranulePart(&[
                                (0, GPIAccessType::Root),
                                (1, GPIAccessType::Root),
                                (2, GPIAccessType::Root),
                                (3, GPIAccessType::Root),
                            ]),
                        ),
                    ]),
                ),
            ],
            30,
        );

        Ok(())
    }

    #[test]
    fn multi_desc_break() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0..0xc000_0000, GPIAccessType::Secure)?;
        assert_gpt_eq(
            &gpt,
            &[
                (0, Block(GPIAccessType::Secure)),
                (1, Block(GPIAccessType::Secure)),
                (2, Block(GPIAccessType::Secure)),
            ],
            32,
        );

        gpt.set(0x2000_0000..0xa000_0000, GPIAccessType::Root)?;

        assert_gpt_eq(
            &gpt,
            &[
                (
                    0,
                    Table(&[
                        (0, Contig(ContigSize::MB512, GPIAccessType::Secure)),
                        (512, Contig(ContigSize::MB512, GPIAccessType::Root)),
                    ]),
                ),
                (1, Block(GPIAccessType::Root)),
                (
                    2,
                    Table(&[
                        (0, Contig(ContigSize::MB512, GPIAccessType::Root)),
                        (512, Contig(ContigSize::MB512, GPIAccessType::Secure)),
                    ]),
                ),
            ],
            30,
        );

        Ok(())
    }

    #[test]
    fn break_block() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0..0x4000_0000, GPIAccessType::Root)?;
        gpt.set(0x2000_0000..0x8000_0000, GPIAccessType::Realm)?;

        assert_gpt_eq(
            &gpt,
            &[
                (
                    0,
                    Table(&[
                        (0, Contig(ContigSize::MB512, GPIAccessType::Root)),
                        (512, Contig(ContigSize::MB512, GPIAccessType::Realm)),
                    ]),
                ),
                (1, Block(GPIAccessType::Realm)),
            ],
            31,
        );

        Ok(())
    }

    #[test]
    fn break_contig() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0..0x2000_0000, GPIAccessType::Root)?;
        gpt.set(0x4_0000..0x0200_0000, GPIAccessType::Realm)?;

        assert_gpt_eq(
            &gpt,
            &[(
                0,
                Table(&[
                    (
                        0,
                        Granule([
                            GPIAccessType::Root,
                            GPIAccessType::Root,
                            GPIAccessType::Root,
                            GPIAccessType::Root,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                            GPIAccessType::Realm,
                        ]),
                    ),
                    (1, Granule([GPIAccessType::Realm; 16])),
                    (2, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (4, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (6, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (8, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (10, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (12, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (14, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (16, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (18, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (20, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (22, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (24, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (26, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (28, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (30, Contig(ContigSize::MB2, GPIAccessType::Realm)),
                    (32, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (64, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (96, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (128, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (160, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (192, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (224, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (256, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (288, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (320, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (352, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (384, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (416, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (448, Contig(ContigSize::MB32, GPIAccessType::Root)),
                    (480, Contig(ContigSize::MB32, GPIAccessType::Root)),
                ]),
            )],
            31,
        );

        Ok(())
    }

    #[test]
    fn overwrite() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0x4000_0000..0x8204_0000, GPIAccessType::Root)?;

        assert_gpt_eq(
            &gpt,
            &[
                (1, Block(GPIAccessType::Root)),
                (
                    2,
                    Table(&[
                        (0, Contig(ContigSize::MB32, GPIAccessType::Root)),
                        (
                            32,
                            GranulePart(&[
                                (0, GPIAccessType::Root),
                                (1, GPIAccessType::Root),
                                (2, GPIAccessType::Root),
                                (3, GPIAccessType::Root),
                            ]),
                        ),
                    ]),
                ),
            ],
            31,
        );

        gpt.set(0..0xc000_0000, GPIAccessType::Realm)?;

        assert_gpt_eq(
            &gpt,
            &[
                (0, Block(GPIAccessType::Realm)),
                (1, Block(GPIAccessType::Realm)),
                (2, Block(GPIAccessType::Realm)),
            ],
            32,
        );

        Ok(())
    }

    #[test]
    fn reuse_freed_l1() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 32);

        gpt.set(0..0x20_0000, GPIAccessType::Root)?;
        assert_gpt_eq(
            &gpt,
            &[(
                0,
                Table(&[(0, Contig(ContigSize::MB2, GPIAccessType::Root))]),
            )],
            31,
        );

        gpt.set(0..0x4000_0000, GPIAccessType::Realm)?;
        assert_gpt_eq(&gpt, &[(0, Block(GPIAccessType::Realm))], 32);

        gpt.set(0x4000_0000..0x4020_0000, GPIAccessType::Secure)?;
        assert_gpt_eq(
            &gpt,
            &[
                (0, Block(GPIAccessType::Realm)),
                (
                    1,
                    Table(&[(0, Contig(ContigSize::MB2, GPIAccessType::Secure))]),
                ),
            ],
            31,
        );

        Ok(())
    }

    #[test]
    fn oom() -> Result<(), Error> {
        declare_local_granule_protection!(gpt, 35, 30, 16, 1);

        gpt.set(0..0x20_0000, GPIAccessType::Root)?;
        assert_gpt_eq(
            &gpt,
            &[(
                0,
                Table(&[(0, Contig(ContigSize::MB2, GPIAccessType::Root))]),
            )],
            0,
        );

        assert_eq!(
            gpt.set(0x4000_0000..0x4020_0000, GPIAccessType::Secure),
            Err(Error::OutOfMemory)
        );

        Ok(())
    }
}
