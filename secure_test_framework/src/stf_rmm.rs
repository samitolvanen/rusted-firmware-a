// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Fake RMM component of RF-A Secure Test Framework.

#![no_main]
#![no_std]

extern crate alloc;

mod exceptions;
mod ffa;
mod framework;
mod gicv3;
mod heap;
mod logger;
mod pagetable;
mod platform;
mod tests;
mod util;

use core::{arch::asm, panic::PanicInfo};

use aarch64_rt::{enable_mmu, entry};
use log::{error, info};
use smccc::psci;

use crate::{
    platform::{Platform, PlatformImpl, RMM_IDMAP},
    util::current_el,
};

const RMM_BOOT_COMPLETE: u64 = 0xC400_01CF;

enable_mmu!(RMM_IDMAP);

entry!(realm_main, 4);
fn realm_main(x0: u64, x1: u64, x2: u64, x3: u64) -> ! {
    let log_sink = PlatformImpl::make_log_sink();
    logger::init(log_sink).unwrap();

    info!(
        "Fake RMM starting at EL {} with args {:#x}, {:#x}, {:#x}, {:#x}",
        current_el(),
        x0,
        x1,
        x2,
        x3,
    );

    let fid = RMM_BOOT_COMPLETE;
    let ret = 0u32;

    // Sends a RMM_BOOT_COMPLETE SMC to notify the Root World that RMM has booted.
    //
    // Safety:
    // Marking the boot as successfull is safe as the RMM will never be called again by other
    // components.
    unsafe {
        asm!("smc #0", in("x0") fid, in("x1") ret);
    }

    // The `smc` instruction in the previous asm section never returns.
    unreachable!()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    error!("{info}");
    loop {}
}

fn call_test_helper(_: usize, _: [u64; 3]) -> Result<[u64; 4], ()> {
    panic!("call_test_helper shouldn't be called from realm world tests");
}

/// Not supported in RMM.
pub fn start_secondary(psci_mpidr: u64, _entry: fn(u64) -> !, arg: u64) -> Result<(), psci::Error> {
    panic!("start_secondary({psci_mpidr:#}, .., {arg}) called in RMM");
}
