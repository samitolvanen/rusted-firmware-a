// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::platform::ERRATA_LIST;
use core::{
    arch::naked_asm,
    mem::{offset_of, size_of},
};

/// A unique identifier for an erratum.
pub type ErratumID = u32;

/// The CVE number associated with an erratum, or 0 if none.
pub type CVE = u32;

/// Represents a CPU revision and variant.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct RevisionVariant {
    pub revision: u8,
    pub variant: u8,
}

impl RevisionVariant {
    /// Creates a new RevisionVariant.
    pub const fn new(revision: u8, variant: u8) -> Self {
        Self { revision, variant }
    }

    /// A sentinel value for errata that are not yet fixed.
    pub const NOT_FIXED: Self = Self::new(u8::MAX, u8::MAX);
}

/// Specifies when an erratum workaround should be applied.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum ErratumType {
    /// Apply the workaround at CPU reset, before the stack is set up.
    Reset,
    /// Apply the workaround at runtime by calling the function directly in the Platform
    /// implementation.
    Runtime,
}

/// # Safety
///
/// Check and workaround function implementations should be naked functions that don't require a
/// stack, and don't access memory.
pub unsafe trait Erratum {
    const ID: ErratumID;
    const CVE: CVE;
    const APPLY_ON: ErratumType;
    const FIXED_IN: RevisionVariant;
    const APPLY_FROM: RevisionVariant;

    /// Returns true if the erratum is to be applied for current CPU.
    extern "C" fn check() -> bool;
    /// Applies the workaround for a specific erratum.
    extern "C" fn workaround();
}

/// A C-compatible struct of function pointers for an erratum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct ErratumEntry {
    pub apply_on: ErratumType,
    pub fixed_in: RevisionVariant,
    pub apply_from: RevisionVariant,
    pub check: extern "C" fn() -> bool,
    pub workaround: extern "C" fn(),
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

/// Implements the logic for an erratum `check` function in assembly.
///
/// This macro generates an assembly implementation for the `check` function.
/// It reads the CPU's revision and variant from MIDR_EL1 and compares
/// it against `Self::APPLY_FROM` and `Self::FIXED_IN`. It handles both fixed
/// and unfixed errata by checking against `RevisionVariant::NOT_FIXED`.
///
/// The generated assembly returns `true` (1) if the erratum should be applied,
/// and `false` (0) otherwise, following the AArch64 C calling convention.
///
/// This should be used inside the `check` function of an `Erratum` implementation.
///
/// # Example
///
/// ```no_run
/// unsafe impl Erratum for MyErratum {
///     // ...
///     extern "C" fn check() -> bool {
///         implement_erratum_check!(RevisionVariant::new(0, 1), RevisionVariant::NOT_FIXED)
///     }
///     // ...
/// }
/// ```
macro_rules! implement_erratum_check {
    ($apply_from:expr, $fixed_in:expr) => {
        unsafe {
            naked_asm!(
                // Read MIDR_EL1
                "mrs x1, midr_el1",

                // Extract revision and variant from MIDR
                "and x2, x1, #{midr_revision_mask}", // x2: current revision
                "lsr x1, x1, #{midr_variant_offset}",
                "and x1, x1, #{midr_variant_mask}", // x1: current variant

                // Load APPLY_FROM revision and variant
                "mov w4, #{apply_from_revision}",
                "mov w3, #{apply_from_variant}",

                // Compare current revision and variant with APPLY_FROM
                "cmp x2, x4", // Compare current.revision with apply_from.revision
                "b.lt 3f",    // If less, condition not met, return false
                "b.gt 1f",    // If greater, condition met, check against FIXED_IN
                "cmp x1, x3", // If equal, compare current.variant with apply_from.variant
                "b.lt 3f",    // If less, condition not met, return false

                "1:", // Check if current < FIXED_IN
                "cmp x2, x6", // Compare current.revision with fixed_in.revision
                "b.lt 2f",    // If less, condition met, return true
                "b.gt 3f",    // If greater, condition not met, return false
                "cmp x1, x5", // If equal, compare current.variant with fixed_in.variant
                "b.lt 2f",    // If less, condition met, return true
                "b.ge 3f",    // If greater or equal, condition not met, return false

                // Condition (current.variant > APPLY_FROM.variant)
                //           or (current.revision < FIXED_IN) is true
                "2:",
                "mov x0, #1", // Return true
                "ret",

                "3:", // Return false
                "mov x0, #0",
                "ret",

                apply_from_revision = const Self::apply_from.revision as u32,
                apply_from_variant = const Self::apply_from.variant as u32,
                fixed_in_revision = const Self::fixed_in.revision as u32,
                fixed_in_variant = const Self::fixed_in.variant as u32,
                midr_revision_mask = MidrEl1::REVISION_MASK,
                midr_variant_offset = MidrEl1::VARIANT_SHIFT,
                midr_variant_mask = MidrEl1::VARIANT_MASK,

                // This assembly block is self-contained and performs a tail call
                options(nomem, nostack, noreturn)
            )
        }
    };
}
pub(crate) use implement_erratum_check;

/// This function iterates over the ERRATA_LIST, calling the check function on each Reset erratum
/// and then calling the workaround function if the check function returned true.
#[cfg(all(target_arch = "aarch64", not(test)))]
#[unsafe(naked)]
pub extern "C" fn apply_reset_errata() {
    naked_asm!(
        // Loop through the ERRATA slice.
        // Address of the beginning of ERRATA_LIST.
        "ldr x9, ={errata_list}",
        "ldr x10, =({errata_list} + {erratum_entry_size} * {errata_list_count})",
        "1:",
        "cmp  x9, x10",
        "b.eq 3f", // End of loop

        // Load apply_on field
        "ldr  w11, [x9, #{apply_on_offset}]",
        "cmp  w11, {reset_type}",
        "b.ne 2f", // Skip if not Reset type

        // Call check()
        "ldr  x11, [x9, #{check_offset}]",
        "blr  x11",
        "cbz  x0, 2f", // Skip if check() returns false

        // Call workaround()
        "ldr  x11, [x9, #{workaround_offset}]",
        "blr  x11",

        "2:",
        "add  x9, x9, #{entry_size}", // Next entry
        "b    1b",
        "3:",

        "ret",

        errata_list = sym ERRATA_LIST,
        erratum_entry_size = const size_of::<ErratumEntry>(),
        errata_list_count = const ERRATA_LIST.len(),
        apply_on_offset = const offset_of!(ErratumEntry, apply_on),
        check_offset = const offset_of!(ErratumEntry, check),
        workaround_offset = const offset_of!(ErratumEntry, workaround),
        reset_type = const ErratumType::Reset as u32,
        entry_size = const size_of::<ErratumEntry>(),
    );
}
