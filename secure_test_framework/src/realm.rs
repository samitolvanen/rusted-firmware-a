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
mod platform;
mod tests;
mod util;

use core::{panic::PanicInfo, ptr::slice_from_raw_parts_mut};

use aarch64_rt::entry;
use log::{error, info};
use smccc::smc64;

use crate::{
    platform::{Platform, PlatformImpl},
    util::current_el,
};

const PLATFORM_CORE_COUNT: usize = 8;
const SUPPORTED_RMM_VERSION: RmmBootManifestVersion = RmmBootManifestVersion::new(0, 8);
const SUPPORTED_RMM_MANIFEST_VERSION: RmmBootManifestVersion = RmmBootManifestVersion::new(0, 8);

const RMM_BOOT_COMPLETE: u64 = 0xC400_01CF;
const RMM_RMI_REQ_COMPLETE: u64 = 0xC400_018F;
const RMM_RMI_REQ_VERSION: u64 = 0xC400_0150;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RmmBootManifestVersion(u32);
impl RmmBootManifestVersion {
    pub(crate) const fn new(major: u16, minor: u16) -> Self {
        Self((major as u32 & 0x8fff) << 16 | minor as u32)
    }

    pub(crate) const fn major(&self) -> u16 {
        (self.0 >> 16) as u16
    }

    pub(crate) const fn minor(&self) -> u16 {
        (self.0 & 0xffff) as u16
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum RmmBootReturn {
    Success = 0,
    Unknown = -1,
    VersionNotValid = -2,
    CpusOutOfRange = -3,
    CpuIdOutOfRange = -4,
    InvalidSharedBuffer = -5,
    ManifestVersionNotSupported = -6,
    ManifestDataError = -7,
}

fn complete_boot(ret: RmmBootReturn) -> ! {
    let ret = ret as u64;

    // Sends a RMM_BOOT_COMPLETED SMC to notify the Root World that RMM has booted.
    let mut args: [u64; 17] = [0; 17];
    args[0] = ret;
    let regs = smc64(RMM_BOOT_COMPLETE as u32, args);

    handle_incoming_calls(regs)
}

/// Infinite loop to handle RMI calls coming from NS World.
fn handle_incoming_calls(mut regs: [u64; 18]) -> ! {
    info!("Received RMI call for FID 0x{:X}", regs[0]);

    loop {
        let ret = handle_rmi_call(&regs);

        regs = smc64(RMM_RMI_REQ_COMPLETE as u32, ret);
    }
}

/// Handles a single RMI call from NS world.
fn handle_rmi_call(regs: &[u64]) -> [u64; 17] {
    match regs[0] {
        RMM_RMI_REQ_VERSION => rmi_version(regs),
        _ => {
            let mut ret = [0; 17];
            ret[0] = u64::MAX;
            ret
        }
    }
}

fn rmi_version(args: &[u64]) -> [u64; 17] {
    let mut ret = [0; 17];

    let requested = RmmBootManifestVersion(args[1] as u32);
    info!(
        "Received RMM_RMI_REQ_VERSION for v{}.{}",
        requested.major(),
        requested.minor()
    );

    ret[0] = if args[1] == SUPPORTED_RMM_VERSION.0 as u64 {
        0
    } else {
        1
    };
    ret[1] = SUPPORTED_RMM_VERSION.0 as u64;
    ret[2] = SUPPORTED_RMM_VERSION.0 as u64;

    ret
}

fn validate_args(pe_idx: u64, version: u64, core_count: u64, shared_buffer_addr: u64) {
    if pe_idx > core_count {
        complete_boot(RmmBootReturn::CpuIdOutOfRange);
    }

    let version = RmmBootManifestVersion(version as u32);
    if version.major() != SUPPORTED_RMM_VERSION.major() {
        complete_boot(RmmBootReturn::VersionNotValid)
    }

    if shared_buffer_addr == 0 {
        complete_boot(RmmBootReturn::InvalidSharedBuffer);
    }

    if core_count > PLATFORM_CORE_COUNT as u64 {
        complete_boot(RmmBootReturn::CpusOutOfRange);
    }
}

entry!(realm_main, 4);
fn realm_main(x0: u64, x1: u64, x2: u64, x3: u64) -> ! {
    validate_args(x0, x1, x2, x3);

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

    // Safety: the specification states that the `x3` register of the RMM is a pointer to a 4KB
    // page mapped into the Realm World.
    let manifest_buf = unsafe { &mut *slice_from_raw_parts_mut(x3 as *mut u32, 0x400) };

    info!("Received manifest with version 0x{:x}", manifest_buf[0]);

    let manifest_version = RmmBootManifestVersion(manifest_buf[0]);
    if manifest_version.major() != SUPPORTED_RMM_MANIFEST_VERSION.major() {
        error!("Unsupported manifest version: 0x{:x}", manifest_version.0);
        complete_boot(RmmBootReturn::ManifestVersionNotSupported)
    }

    complete_boot(RmmBootReturn::Success)
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    error!("{info}");
    loop {}
}

fn call_test_helper(_: usize, _: [u64; 3]) -> Result<[u64; 4], ()> {
    panic!("call_test_helper shouldn't be called from realm world tests");
}
