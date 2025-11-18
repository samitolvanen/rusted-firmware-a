// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Tests SIMD context switch.

use crate::framework::{
    TestHelperProxy, TestHelperRequest, TestHelperResponse, normal_world_test, secure_world_test,
};
use log::{debug, info};

type SimdVectors = [u128; 32];

/// Initial state of SIMD context before any FP operations.
const EMPTY_SIMD_REGS: SimdVectors = [0_u128; 32];

/// Returns current state of SIMD vector registers.
fn read_simd() -> SimdVectors {
    let mut regs = EMPTY_SIMD_REGS;

    // The instructions save 32 bytes (Qx and Qy) per line and advance the pointer by 32.
    // SAFETY: `dest` is a 16B aligned valid pointer to a 32 * 32 byte array.
    unsafe {
        core::arch::asm!(
            ".arch_extension fp",
            "stp q0, q1, [{dest}], #32",
            "stp q2, q3, [{dest}], #32",
            "stp q4, q5, [{dest}], #32",
            "stp q6, q7, [{dest}], #32",
            "stp q8, q9, [{dest}], #32",
            "stp q10, q11, [{dest}], #32",
            "stp q12, q13, [{dest}], #32",
            "stp q14, q15, [{dest}], #32",
            "stp q16, q17, [{dest}], #32",
            "stp q18, q19, [{dest}], #32",
            "stp q20, q21, [{dest}], #32",
            "stp q22, q23, [{dest}], #32",
            "stp q24, q25, [{dest}], #32",
            "stp q26, q27, [{dest}], #32",
            "stp q28, q29, [{dest}], #32",
            "stp q30, q31, [{dest}], #32",
            ".arch_extension nofp",
            dest = in(reg) regs.as_mut_ptr(),
        );
    }

    regs
}

/// Overwrites SIMD vector registers with provided values.
fn overwrite_simd(regs: &SimdVectors) {
    // The instructions load 32 bytes (Qx and Qy) per line and advance the pointer by 32.
    // SAFETY: `src` is a 16B aligned valid pointer to a 32 * 32 byte array.
    unsafe {
        core::arch::asm!(
            ".arch_extension fp",
            "ldp q0, q1, [{src}], #32",
            "ldp q2, q3, [{src}], #32",
            "ldp q4, q5, [{src}], #32",
            "ldp q6, q7, [{src}], #32",
            "ldp q8, q9, [{src}], #32",
            "ldp q10, q11, [{src}], #32",
            "ldp q12, q13, [{src}], #32",
            "ldp q14, q15, [{src}], #32",
            "ldp q16, q17, [{src}], #32",
            "ldp q18, q19, [{src}], #32",
            "ldp q20, q21, [{src}], #32",
            "ldp q22, q23, [{src}], #32",
            "ldp q24, q25, [{src}], #32",
            "ldp q26, q27, [{src}], #32",
            "ldp q28, q29, [{src}], #32",
            "ldp q30, q31, [{src}], #32",
            ".arch_extension nofp",
            src = in(reg) regs.as_ptr(),
        );
    }
}

/// The secure world side of the SIMD context switch test.
fn test_simd_context_switch_helper(
    ns_world_request: TestHelperRequest,
) -> Result<TestHelperResponse, ()> {
    assert_eq!(
        read_simd(),
        EMPTY_SIMD_REGS,
        "Initial state of SWd SIMD regs should be all zeros."
    );

    // SWd overwrites SIMD vector registers with 0, 2, 4, .. (consecutive even numbers).
    let swd_simd_state: SimdVectors = core::array::from_fn(|i| 2 * i as u128);
    overwrite_simd(&swd_simd_state);
    assert_eq!(
        read_simd(),
        swd_simd_state,
        "SWd failed to overwrite SIMD registers."
    );

    // SWd side of the test passed. NSWd ignores the returned values.
    Ok([0, 0, 0, 0])
}

normal_world_test!(
    test_simd_context_switch,
    helper = test_simd_context_switch_helper
);
/// Checks if SIMD vector registers' state is preserved across world switches.
fn test_simd_context_switch(helper: &TestHelperProxy) -> Result<(), ()> {
    let simd_state = read_simd();
    assert_eq!(
        simd_state, EMPTY_SIMD_REGS,
        "Initial state of NSWd SIMD regs should be all zeros."
    );

    // NSWd overwrites SIMD vector registers with 1, 3, 5, .. (consecutive odd numbers).
    let nswd_clobber: SimdVectors = core::array::from_fn(|i| (2 * i + 1) as u128);
    overwrite_simd(&nswd_clobber);
    assert_eq!(
        read_simd(),
        nswd_clobber,
        "NSWd failed to overwrite SIMD registers."
    );

    // Switch to the SWd side of the test. The input argument is ignored.
    helper([0, 0, 0])?;

    // Make sure the world switch did not destroy NSWd SIMD state.
    assert_eq!(
        read_simd(),
        nswd_clobber,
        "NSWd SIMD should be preserved across world switches."
    );

    Ok(())
}
