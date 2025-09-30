// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use smccc::smc64;

use crate::framework::normal_world_test;

normal_world_test!(test_rmm_version);
fn test_rmm_version() -> Result<(), ()> {
    let mut args = [0; 17];
    args[0] = 0x8;

    let ret = smc64(0xC400_0150u32, args);

    assert_eq!(ret[0], 0);
    assert_eq!(ret[1], 8);
    assert_eq!(ret[2], 8);
    assert!(ret[3..].iter().all(|r| *r == 0));

    Ok(())
}
