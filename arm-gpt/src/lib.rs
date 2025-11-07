// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

#![no_std]

use spin::mutex::SpinMutex;
use thiserror::Error;

pub use crate::table::GPIAccessType;
use crate::table::{Level0Table, Level1Table};

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

#[derive(Debug)]
pub struct GranuleProtectionState<
    'life,
    const L0_COUNT: usize,
    const L1_COUNT: usize,
    const PGS: usize,
> {
    level0: &'life mut Level0Table<L0_COUNT, L1_COUNT, PGS>,
    level1: &'life mut [Level1Table<L0_COUNT, L1_COUNT, PGS>],
    free: Option<usize>,
}

/// Handle to manipulate Granule Protection Tables and related registers.
///
/// See [`declare_granule_protection`] for instantiating it.
///
/// Before any other operation, the [`GranuleProtection`] object must be initialized exactly once
/// with two backing buffers by the [`GranuleProtection::init`] function. After that, the memory
/// regions can be mapped using [`GranuleProtection::set`].
#[derive(Debug)]
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
}

#[macro_export]
/// Declares a static instance of [`GranuleProtection`], using the given size parameters.
macro_rules! declare_granule_protection {
    ($name:ident, $PPS:literal, $L0GPTSZ:literal, $PGS:literal) => {
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
