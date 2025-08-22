//! Errata management framework.
//!
//! This module provides a framework for managing CPU errata.

use crate::platform::ERRATA_LIST;

/// A unique identifier for an erratum.
pub type ErratumID = usize;

/// The CVE number associated with an erratum, or 0 if none.
pub type CVE = u32;

/// The error type for erratum operations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ErratumError;

/// Specifies when an erratum workaround should be applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum ErratumType {
    /// Apply the workaround at CPU reset, before the stack is set up.
    Reset,
    /// Apply the workaround at runtime.
    Runtime,
}

/// SAFETY: Check and workaround function implementations should be naked functions that don't
/// require a stack, and don't access memory.
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct ErratumEntry {
    pub apply_on: ErratumType,
    pub fixed_in: FIXED_IN,
    pub apply_from: APPLY_FROM,
    pub check: extern "C" fn() -> bool,
    pub workaround: unsafe extern "C" fn(),
}

impl ErratumEntry {
    /// Creates an ErratumEntry struct from an implementation of the Erratum trait.
    pub const fn from_erratum<T: Erratum>() -> Self {
        Self {
            apply_on: T::APPLY_ON,
            fixed_in: T::FIXED_IN,
            apply_from: T::APPLY_FROM,
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

/// # Safety
///
/// This function is unsafe because it is a naked function, serving as a custom
/// reset vector handler for applying CPU errata workarounds in the TF-A environment.
///
/// 1.  Naked Function: It omits the standard Rust function prologue and epilogue. It must
///     manually manage the program flow and register state, ensuring adherence to the TF-A
///     reset vector calling convention for subsequent boot stages.
/// 2.  Register and Memory: It performs direct register manipulation (x9, x10, x11, x2, x0, w11)
///     and relies on raw pointer arithmetic using constant offsets to iterate over the
///     `ERRATA_LIST` array. Incorrect offsets or list structure assumptions could corrupt memory.
/// 3.  External Calls: It calls external `check()` and `workaround()` functions via function
///     pointers loaded into x11. The correctness of these functions and their adherence to
///     the AArch64 C calling convention (e.g., input in x0) are critical for safety.
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
        apply_on_offset = const core::mem::offset_of!(ErratumEntry, apply_on),
        check_offset = const core::mem::offset_of!(ErratumEntry, check),
        workaround_offset = const core::mem::offset_of!(ErratumEntry, workaround),
        reset_type = const ErratumType::Reset as u32,
        entry_size = const size_of::<ErratumEntry>(),
    );
}
