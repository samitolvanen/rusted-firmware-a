// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use core::ptr::slice_from_raw_parts_mut;

use crate::{
    context::World,
    info,
    layout::{rmm_shared_end, rmm_shared_start},
    platform::{Platform, PlatformImpl},
    services::{
        Service, owns,
        rmmd::manifest::{ManifestList, RmmBootManifest, RmmConsoleInfo},
    },
    smccc::{FunctionId, NOT_SUPPORTED, OwningEntityNumber, SmcReturn},
};

pub mod manifest;

pub fn rme_prepare() {
    let shared_base = rmm_shared_start();
    let shared_end = rmm_shared_end();

    // Safety: this memory region was allocated during page table initialization. It is not accessed
    // outside of RMMD or the R-EL2 payload, thus it is ensured that this reference is unique.
    let buf =
        unsafe { &mut *slice_from_raw_parts_mut(shared_base as *mut u8, shared_end - shared_base) };

    let manifest = RmmBootManifest::new(
        buf,
        PlatformImpl::RMM_NS_DRAM_COUNT,
        PlatformImpl::RMM_CONSOLE_COUNT + 1,
        PlatformImpl::RMM_NCOH_REGION_COUNT,
        PlatformImpl::RMM_COH_REGION_COUNT,
        PlatformImpl::RMM_SMMU_COUNT,
        PlatformImpl::RMM_ROOT_COMPLEX,
    );

    PlatformImpl::rme_prepare_manifest(manifest);

    manifest.plat_console.as_slice_mut()[PlatformImpl::RMM_CONSOLE_COUNT] = RmmConsoleInfo {
        // Value from the pl011_uart crate.
        base: 0x1C09_0000,

        // Values from TF-A.
        map_pages: 0x1,
        name: [0x70, 0x6c, 0x30, 0x31, 0x31, 0x0, 0x0, 0x0], // "pl011"
        clk_in_hz: 0x00e1_0000,
        baud_rate: 0x1c200,
        flags: 0,
    };

    info!("RME Boot Manifest ready: {manifest:#x?}")
}

const RMM_BOOT_COMPLETE: u32 = 0xC400_01CF;
const RMM_RMI_REQ_COMPLETE: u32 = 0xC400_018F;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RmmBootReturn {
    Success = 0,
    Unknown = -1,
    VersionMismatch = -2,
    CpusOutOfRange = -3,
    CpuIdOutOfRange = -4,
    InvalidSharedBuffer = -5,
    ManifestVersionNotSupported = -6,
    ManifestDataError = -7,
}

impl From<i32> for RmmBootReturn {
    fn from(value: i32) -> Self {
        match value {
            0 => RmmBootReturn::Success,
            -1 => RmmBootReturn::Unknown,
            -2 => RmmBootReturn::VersionMismatch,
            -3 => RmmBootReturn::CpusOutOfRange,
            -4 => RmmBootReturn::CpuIdOutOfRange,
            -5 => RmmBootReturn::InvalidSharedBuffer,
            -6 => RmmBootReturn::ManifestVersionNotSupported,
            -7 => RmmBootReturn::ManifestDataError,
            _ => RmmBootReturn::Unknown,
        }
    }
}

/// Arm CCA SMCs, for communication between RF-A and TF-RMM.
///
/// This is described at
/// https://trustedfirmware-a.readthedocs.io/en/latest/components/rmm-el3-comms-spec.html.
pub struct Rmmd;

impl Service for Rmmd {
    owns! {OwningEntityNumber::STANDARD_SECURE, 0x0150..=0x01CF}

    fn handle_non_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        ((*regs).into(), World::Realm)
    }

    fn handle_realm_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        let mut function = FunctionId(regs[0] as u32);
        function.clear_sve_hint();

        match function.0 {
            RMM_BOOT_COMPLETE => {
                info!("Realm boot completed with code 0x{:x}", regs[1]);
                (rmm_boot_complete(regs[1] as i32), World::NonSecure)
            }
            RMM_RMI_REQ_COMPLETE => {
                // Only x1-x6 are used for RMI return values, the remaining ones MBZ.
                let forwarded_regs: [u64; 6] = regs[1..7].try_into().unwrap();
                (forwarded_regs.into(), World::NonSecure)
            }
            _ => (NOT_SUPPORTED.into(), World::Realm),
        }
    }
}

impl Rmmd {
    pub(super) fn new() -> Self {
        Self
    }
}

fn rmm_boot_complete(ret: i32) -> SmcReturn {
    ret.into()
}
