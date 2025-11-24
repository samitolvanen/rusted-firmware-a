// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use core::slice::from_raw_parts_mut;

use crate::{
    context::World,
    info,
    platform::{Platform, PlatformImpl},
    services::{
        Service, owns,
        rmmd::smc::{
            EccCurve, RecAttestGetRealmKeyResponse, RecCall, RecCommandReturnCode,
            RecEl3FeaturesResponse,
        },
    },
    smccc::{FunctionId, NOT_SUPPORTED, OwningEntityNumber, SetFrom, SmcReturn},
};

/// Size in bytes of the EL3 - RMM shared area.
pub const RMM_SHARED_BUFFER_SIZE: usize = 0x1000;

pub mod manifest;
mod smc;

/// Returns a mutable reference to the shared buffer used for communication between R-EL2 and EL3.
///
/// # Safety
///
/// Calling this function is always safe, but using its return value is safe if all the conditions
/// below are met:
///
/// - It can only be called after the shared buffer is mapped into the page table.
/// - After calling `get_shared_buffer`, the return reference must be dropped before any other call
///   to it is made.
/// - The reference must be dropped before switching to Realm World.
unsafe fn get_shared_buffer() -> &'static mut [u8; RMM_SHARED_BUFFER_SIZE] {
    // Safety: (relative to [`slice::from_raw_parts_mut`][https://doc.rust-lang.org/stable/core/slice/fn.from_raw_parts_mut.html])
    // - The first condition of `get_shared_buffer()` ensures that the location is valid, and as it
    //   occupies exactly one page, it will always be aligned.
    // - `u8` is properly initialized regardless of the initial value.
    // - The second condition ensures that the buffer is never accessed through multiple reference
    //   within EL3. As it can only be accessed by EL3 and Realm World, it follows from the third
    //   condition that no other pointers can be used to access the buffer while a reference exists.
    // - Follows from the soundness of the layout defined in `layout.rs`.
    unsafe {
        from_raw_parts_mut(
            PlatformImpl::RMM_SHARED_BUFFER_START as *mut u8,
            RMM_SHARED_BUFFER_SIZE,
        )
        .try_into()
        .unwrap()
    }
}

const RMM_BOOT_COMPLETE: u32 = 0xC400_01CF;

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

    fn handle_non_secure_smc(&self, _: &mut SmcReturn) -> World {
        World::Realm
    }

    fn handle_realm_smc(&self, regs: &mut SmcReturn) -> World {
        let in_regs = regs.values();
        let mut function = FunctionId(in_regs[0] as u32);
        function.clear_sve_hint();

        if function.0 == RMM_BOOT_COMPLETE {
            info!("Realm boot completed with code 0x{:x}", regs.values()[1]);

            if regs.values()[1] != 0 {
                panic!()
            }

            rmm_boot_complete(regs);
            return World::NonSecure;
        }

        let Ok(command) = RecCall::from_regs(regs.values()) else {
            regs.set_from(NOT_SUPPORTED);
            return World::Realm;
        };

        let sb_address = PlatformImpl::RMM_SHARED_BUFFER_START
            ..PlatformImpl::RMM_SHARED_BUFFER_START + RMM_SHARED_BUFFER_SIZE;
        match command {
            RecCall::RmiReqComplete { .. } => {
                // Only x1-x6 are used for RMI return values, the remaining ones MBZ.
                regs.values_mut().copy_within(1..7, 0);
                regs.values_mut()[6..].fill(0);
                World::NonSecure
            }
            RecCall::GtsiDelegate { .. } => todo!(),
            RecCall::GtsiUndelegate { .. } => todo!(),
            // TODO(firme): equivalent to MFI_ATTEST_RAK_GET, will have to take into account the
            // write offset and continued request.
            RecCall::AttestGetRealmKey {
                buf_pa,
                buf_size,
                ecc_curve,
            } => {
                let buf_pa = buf_pa as usize;
                let buf_size = buf_size as usize;

                // Perform sanity checks on the received buffer range.
                if !sb_address.contains(&buf_pa) {
                    regs.set_from(RecCommandReturnCode::BadAddress);
                    return World::Realm;
                }
                if !sb_address.contains(&(buf_pa + buf_size - 1)) {
                    regs.set_from(RecCommandReturnCode::InvalidValue);
                    return World::Realm;
                }

                // Safety:
                // - This function can only be reached after having setup the Realm World, which
                //   requires the MMU and pagetables to be setup.
                // - This function never calls again `get_shared_buffer()`, thus the reference will
                //   be dropped upon return, before another call is made.
                // - Similarly to the above, this function does not switch to the Realm World.
                let shared_buffer = unsafe { get_shared_buffer() };

                let key_size = match ecc_curve {
                    EccCurve::EccSecp384r1 => {
                        match PlatformImpl::write_attestion_key_ecc_secp384r1(shared_buffer, 0) {
                            Ok(size) => size,
                            Err(_) => {
                                regs.set_from(RecCommandReturnCode::Unk);
                                return World::Realm;
                            }
                        }
                    }
                };

                regs.set_from(RecAttestGetRealmKeyResponse {
                    key_size: key_size as u64,
                });
                World::Realm
            }
            RecCall::AttestGetPlatToken { .. } => todo!(),
            RecCall::El3Features { .. } => {
                regs.set_from(RecEl3FeaturesResponse { feat_reg: 0 });
                World::Realm
            }
            RecCall::El3TokenSign { .. } => todo!(),
            // Hacky trick to avoid TF-RMM from enabling encryption (not implemented yet).
            RecCall::MecRefresh { .. } => {
                regs.set_from(NOT_SUPPORTED);
                World::Realm
            }
            RecCall::IdeKeyProg { .. } => todo!(),
            RecCall::IdeKeySetGo { .. } => todo!(),
            RecCall::IdeKeySetStop { .. } => todo!(),
            RecCall::IdeKmPullResponse { .. } => todo!(),
            RecCall::ReserveMemory { .. } => todo!(),
        }
    }
}

impl Rmmd {
    pub(super) fn new() -> Self {
        // Safety:
        // - This function is called after initializing the MMU and pagetable.
        // - This function never calls again `get_shared_buffer()`, thus the reference will be dropped
        //   upon return, before another call is made.
        // - Similarly to the above, this function does not switch to the Realm World.
        let buf = unsafe { get_shared_buffer() };

        PlatformImpl::rme_prepare_manifest(buf);

        info!("RMM Boot Manifest ready");

        Self
    }
}

fn rmm_boot_complete(regs: &mut SmcReturn) {
    let ret = regs.values()[1] as i32;
    regs.set_from(ret);
}
