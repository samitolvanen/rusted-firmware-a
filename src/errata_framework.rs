//! Errata management framework.
//!
//! This module provides a framework for managing CPU errata.

use crate::{platform::ERRATA_LIST};

/// A unique identifier for an erratum.
pub type ErratumID = usize;

/// The CVE number associated with an erratum, or 0 if none.
pub type CVE = usize;

/// The error type for erratum operations.
#[derive(Debug)]
pub enum ErratumError {
    /// Applying the workaround failed.
    ApplyFailed,
}

/// Specifies when an erratum workaround should be applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum ErratumType {
    /// Apply the workaround at CPU reset, before the stack is set up.
    Reset,
    /// Apply the workaround at runtime.
    Runtime,
}

/// TODO: comments later
pub unsafe trait Erratum {
    const ID: ErratumID;
    const CVE: CVE;
    const APPLY_ON: ErratumType;
    const FIXED_IN: RevisionVariant;
    const APPLY_FROM: RevisionVariant;
    extern "C" fn check() -> bool;
    unsafe extern "C" fn workaround();
}

/// A C-compatible struct of function pointers for an erratum.
#[repr(C)]
pub struct ErratumEntry {
    pub id: ErratumID,
    pub cve: CVE,
    pub apply_on: ErratumType,
    pub fixed_in: RevisionVariant: FIXED_IN,
    pub apply_from: RevisionVariant: APPLY_FROM,
    pub check: extern "C" fn() -> bool,
    pub workaround: unsafe extern "C" fn(),
}

impl ErratumEntry {
    pub const fn from_erratum<T: Erratum>() -> Self {
        Self {
            id: T::ID,
            cve: T::CVE,
            apply_on: T::APPLY_ON,
            fixed_in: T::FIXED_IN,
            apply_from, T::APPLY_FROM,
            check: T::check,
            workaround: T::workaround,
        }
    }
}

/// Calculates the count of specified Erratum types.
macro_rules! errata_count {
    () => { 0 };
    ($erratum:ty) => { 1 };
    ($erratum:ty, $($errata:ty),+) => {
        $crate::errata_framework::errata_count!($erratum) + $crate::errata_framework::errata_count!($($errata),+)
    };
}
pub(crate) use errata_count;

/// Declares the ERRATA_LIST array.
macro_rules! define_errata_list {
    ($($erratum:ty),*) => {
        pub static ERRATA_LIST : [$crate::errata_framework::ErratumEntry; $crate::errata_framework::errata_count!($($erratum),*)] = [
            $($crate::errata_framework::ErratumEntry::from_erratum::<$erratum>()),*
        ];
    }
}
pub(crate) use define_errata_list;

#[unsafe(naked)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn apply_reset_errata() {
    core::arch::naked_asm!(
        // Loop through the ERRATA slice.
        // Address of the beginning of ERRATA_LIST.
        "ldr x9, ={errata_list}",
        "ldr x10, =({errata_list} + {erratum_entry_size} * {errata_list_count})",
        "2:",
        "cmp  x9, x10",
        "b.eq 3f", // End of loop

        // Load apply_on field
        "ldr  w11, [x9, #{apply_on_offset}]",
        "cmp  w11, {reset_type}",
        "b.ne 1f", // Skip if not Reset type

        // Read MIDR and load CPU Revision and Variant to x0
        // Revision is [3:0] of MIDR, Variant is [23:20]
        "mrs x2, midr_el1",
        "and x0, x2, #0xF0_000F",

        // Call check()
        "ldr  x11, [x9, #{check_offset}]",
        "blr  x11",
        "cbz  x0, 1f", // Skip if check() returns false

        // Call workaround()
        "ldr  x11, [x9, #{workaround_offset}]",
        "blr  x11",

        "1:",
        "add  x9, x9, #{entry_size}", // Next entry
        "b    2b",
        "3:",

        "ret",

        errata_list = sym ERRATA_LIST,
        erratum_entry_size = const core::mem::size_of::<ErratumEntry>(),
        errata_list_count = const ERRATA_LIST.len(),
        apply_on_offset = const core::mem::offset_of!(crate::errata_framework::ErratumEntry, apply_on),
        check_offset = const core::mem::offset_of!(crate::errata_framework::ErratumEntry, check),
        workaround_offset = const core::mem::offset_of!(crate::errata_framework::ErratumEntry, workaround),
        reset_type = const ErratumType::Reset as u32,
        entry_size = const size_of::<crate::errata_framework::ErratumEntry>(),
    );
}
