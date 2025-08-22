//! Errata management framework.
//!
//! This module provides a framework for managing CPU errata.

use crate::platform::ERRATA_LIST;

/// A unique identifier for an erratum.
pub type ErratumID = usize;

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

// Constants for MIDR bit shifts and masks
const MIDR_REVISION_SHIFT: u32 = 0;
const MIDR_REVISION_BITS: u32 = 4;
const MIDR_VARIANT_SHIFT: u32 = 20;
const MIDR_VARIANT_BITS: u32 = 4;

// Offsets for RevisionVariant fields
const REVISION_VARIANT_REVISION_OFFSET: usize = core::mem::offset_of!(RevisionVariant, revision);
const REVISION_VARIANT_VARIANT_OFFSET: usize = core::mem::offset_of!(RevisionVariant, variant);

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
    pub fixed_in: RevisionVariant,
    pub apply_from: RevisionVariant,
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
///     const APPLY_FROM: RevisionVariant = RevisionVariant::new(0, 1);
///     const FIXED_IN: RevisionVariant = RevisionVariant::NOT_FIXED;
///
///     extern "C" fn check() -> bool {
///         implement_erratum_check!()
///     }
///     // ...
/// }
/// ```
macro_rules! implement_erratum_check {
    () => {
        unsafe {
            core::arch::asm!(
                // Read MIDR_EL1
                "mrs x1, midr_el1",

                // Extract revision and variant from MIDR
                "and x2, x1, #0xF", // x2: current revision
                "lsr x1, x1, #20",
                "and x1, x1, #0xF", // x1: current variant

                // Load APPLY_FROM revision and variant
                "mov w4, #{apply_from_revision}",
                "mov w3, #{apply_from_variant}",

                // Compare current revision and variant with APPLY_FROM
                "cmp x2, x4", // Compare current.revision with apply_from.revision
                "b.lt 0f",    // If less, condition not met, return false
                "b.gt 1f",    // If greater, condition met, check against FIXED_IN
                "cmp x1, x3", // If equal, compare current.variant with apply_from.variant
                "b.lt 0f",    // If less, condition not met, return false

                "1:", // Condition (current >= APPLY_FROM) is true, now check against FIXED_IN

                // Load FIXED_IN and NOT_FIXED revision and variant
                "mov w6, #{fixed_in_revision}",
                "mov w5, #{fixed_in_variant}",
                "mov w8, #{not_fixed_revision}",
                "mov w7, #{not_fixed_variant}",

                // Check if FIXED_IN is NOT_FIXED
                "cmp w6, w8",
                "b.ne 2f", // If revisions are not equal, it's not NOT_FIXED
                "cmp w5, w7",
                "b.ne 2f", // If variants are not equal, it's not NOT_FIXED

                // FIXED_IN is NOT_FIXED, and we already know current >= APPLY_FROM
                "mov x0, #1", // Return true
                "ret",

                "2:", // FIXED_IN is not NOT_FIXED, check if current < FIXED_IN
                "cmp x2, x6", // Compare current.revision with fixed_in.revision
                "b.lt 3f",    // If less, condition met, return true
                "b.gt 0f",    // If greater, condition not met, return false
                "cmp x1, x5", // If equal, compare current.variant with fixed_in.variant
                "b.lt 3f",    // If less, condition met, return true
                "b.ge 0f",    // If greater or equal, condition not met, return false

                "3:", // Condition (current < FIXED_IN) is true
                "mov x0, #1", // Return true
                "ret",

                "0:", // Return false
                "mov x0, #0",
                "ret",

                apply_from_revision = const Self::APPLY_FROM.revision as u32,
                apply_from_variant = const Self::APPLY_FROM.variant as u32,
                fixed_in_revision = const Self::FIXED_IN.revision as u32,
                fixed_in_variant = const Self::FIXED_IN.variant as u32,
                not_fixed_revision = const RevisionVariant::NOT_FIXED.revision as u32,
                not_fixed_variant = const RevisionVariant::NOT_FIXED.variant as u32,

                // This assembly block is self-contained and performs a tail call
                options(nomem, nostack, noreturn)
            )
        }
    };
}
pub(crate) use implement_erratum_check;

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
