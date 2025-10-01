// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use core::ptr::slice_from_raw_parts_mut;

use crate::{
    context::World,
    info,
    layout::{rmm_shared_end, rmm_shared_start},
    platform::{Platform, PlatformImpl},
    services::{Service, owns},
    smccc::{FunctionId, NOT_SUPPORTED, OwningEntityNumber, SmcReturn},
};

pub mod manifest;

/// Returns a mutable reference to the shared buffer used for communication between R-EL2 and EL3.
///
/// ## Safety
///
/// Calling this function is always safe, but using its return value is safe if all the conditions
/// below are met:
///
/// - It can only be called after the shared buffer is mapped into the page table.
/// - After calling `get_shared_buffer`, the return reference must be dropped before any other call
///   to it is made.
/// - The reference must be dropped before switching to Realm World.
unsafe fn get_shared_buffer() -> &'static mut [u8] {
    // Safety: (relative to [`slice::from_raw_parts_mut`][https://doc.rust-lang.org/stable/core/slice/fn.from_raw_parts_mut.html])
    // - The first condition of `get_shared_buffer()` ensures that the location is valid, and as it
    //   occupies exactly one page, it will always be aligned.
    // - `u8` is properly initialized regardless of the initial value.
    // - The second condition ensures that the buffer is never accessed through multiple reference
    //   within EL3. As it can only be accessed by EL3 and Realm World, it follows from the third
    //   condition that no other pointers can be used to access the buffer while a reference exists.
    // - Follows from the soundness of the layout defined in `layout.rs`.
    unsafe {
        &mut *slice_from_raw_parts_mut(
            rmm_shared_start() as *mut u8,
            rmm_shared_end() - rmm_shared_start(),
        )
    }
}

pub fn rme_prepare() {
    // Safety:
    // - This function is called after initializing the MMU and pagetable.
    // - This function never calls again `get_shared_buffer()`, thus the reference will be dropped
    //   upon return, before another call is made.
    // - Similarly to the above, this function does not switch to the Realm World.
    let buf = unsafe { get_shared_buffer() };

    PlatformImpl::rme_prepare_manifest(buf);

    info!("RME Boot Manifest ready")
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
