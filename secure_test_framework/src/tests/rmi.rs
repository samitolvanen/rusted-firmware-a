// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use smccc::smc64;

use crate::framework::normal_world_test;

normal_world_test!(test_rmm_version);
fn test_rmm_version() -> Result<(), ()> {
    const REQUESTED_VERSION: u64 = 0x8;

    let mut args = [0; 17];
    args[0] = REQUESTED_VERSION;

    let ret = smc64(0xC400_0150u32, args);

    // Call not supported, i.e. there is no RMMD.
    if ret[0] == u64::MAX {
        return Ok(());
    }

    let lower = ret[1];
    let higher = ret[2];

    assert_eq!(lower >> 32, 0);
    assert_eq!(higher >> 32, 0);
    assert!(lower <= higher);
    assert!(ret[3..].iter().all(|r| *r == 0));

    match ret[0] {
        0 => {
            assert_eq!(lower, REQUESTED_VERSION);
        }
        1 => assert_ne!(lower, REQUESTED_VERSION),
        _ => panic!("Unexpected return code: 0x{:x}", ret[0]),
    }

    Ok(())
}
