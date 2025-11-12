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

const SUPPORTED_RMM_VERSION: RmmBootManifestVersion = RmmBootManifestVersion { major: 0, minor: 8 };
const SUPPORTED_RMM_MANIFEST_VERSION: RmmBootManifestVersion =
    RmmBootManifestVersion { major: 0, minor: 5 };

const RMM_BOOT_COMPLETE: u64 = 0xC400_01CF;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RmmBootManifestVersion {
    pub(crate) major: u16,
    pub(crate) minor: u16,
}

impl TryFrom<u32> for RmmBootManifestVersion {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        let major = (value >> 16) as u16;

        if major & 0x7fff != major {
            return Err(());
        }

        Ok(Self {
            major,
            minor: (value & 0xFFFF) as u16,
        })
    }
}

impl From<RmmBootManifestVersion> for u32 {
    fn from(value: RmmBootManifestVersion) -> Self {
        (value.major as u32) << 16 | value.minor as u32
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
    smc64(RMM_BOOT_COMPLETE as u32, args);

    // TODO: handle RMI calls originating from NS world.
    todo!()
}

fn validate_args(pe_idx: u64, version: u64, core_count: u64, shared_buffer_addr: u64) {
    if pe_idx >= core_count {
        complete_boot(RmmBootReturn::CpuIdOutOfRange);
    }

    let Ok(version) = RmmBootManifestVersion::try_from(version as u32) else {
        complete_boot(RmmBootReturn::VersionNotValid)
    };

    if version.major != SUPPORTED_RMM_VERSION.major {
        complete_boot(RmmBootReturn::VersionNotValid)
    }

    if shared_buffer_addr == 0 {
        complete_boot(RmmBootReturn::InvalidSharedBuffer);
    }

    // Shared buffer must be paged-aligned.
    if !shared_buffer_addr.is_multiple_of(0x1000) {
        complete_boot(RmmBootReturn::InvalidSharedBuffer);
    }

    if core_count > PlatformImpl::CORE_COUNT as u64 {
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

    let Ok(manifest_version) = RmmBootManifestVersion::try_from(manifest_buf[0]) else {
        complete_boot(RmmBootReturn::VersionNotValid);
    };

    info!(
        "Received manifest with version v{}.{}",
        manifest_version.major, manifest_version.minor
    );

    if manifest_version.major != SUPPORTED_RMM_MANIFEST_VERSION.major {
        error!("Unsupported manifest version: 0x{manifest_version:x?}");
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
