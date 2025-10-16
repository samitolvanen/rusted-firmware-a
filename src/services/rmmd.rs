// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use core::{fmt::Debug, ptr::slice_from_raw_parts_mut};

use spin::mutex::SpinMutex;

use crate::{
    context::World,
    info,
    layout::{rmm_shared_end, rmm_shared_start},
    platform::{Platform, PlatformImpl},
    services::{
        Service, owns,
        rmmd::{
            manifest::{ManifestList, RmmBootManifest, RmmConsoleInfo},
            smc::{
                EccCurve, RecAttestGetPlatTokenResponse, RecAttestGetRealmKeyResponse, RecCall,
                RecCommandReturnCode, RecEl3FeaturesResponse,
            },
        },
    },
    smccc::{FunctionId, NOT_SUPPORTED, OwningEntityNumber, SmcReturn},
};

pub mod manifest;
mod smc;

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
const unsafe fn get_shared_buffer() -> &'static mut [u8] {
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

    sort(&mut manifest.plat_dram.as_slice_mut(), |e| e.base);
    sort(&mut manifest.plat_coh_region.as_slice_mut(), |e| e.base);
    sort(&mut manifest.plat_ncoh_region.as_slice_mut(), |e| e.base);

    info!("RME Boot Manifest ready: {manifest:#x?}")
}

fn sort<T, K, F>(slice: &mut [T], f: F)
where
    F: Fn(&T) -> K,
    K: Ord,
{
    if slice.is_empty() {
        return;
    }

    let (first, second) = slice.split_at_mut(1);
    let first_value = f(&mut first[0]);

    if let Some((min_value, min)) = second
        .iter_mut()
        .map(|e| (f(e), e))
        .min_by(|a, b| a.0.cmp(&b.0))
    {
        if first_value > min_value {
            core::mem::swap(&mut first[0], min);
        }

        sort(&mut slice[1..], f);
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

    fn handle_non_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        ((*regs).into(), World::Realm)
    }

    fn handle_realm_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        let mut function = FunctionId(regs[0] as u32);
        function.clear_sve_hint();

        if function.0 == RMM_BOOT_COMPLETE {
            info!("Realm boot completed with code 0x{:x}", regs[1]);

            if regs[1] != 0 {
                panic!()
            }

            return (rmm_boot_complete(regs[1] as i32), World::NonSecure);
        }

        let Ok(command) = RecCall::from_regs(regs) else {
            return (NOT_SUPPORTED.into(), World::Realm);
        };

        let sb_address = rmm_shared_start()..rmm_shared_end();
        match command {
            RecCall::RmiReqComplete { regs } => (regs.into(), World::NonSecure),
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
                    return RecCommandReturnCode::BadAddress.into();
                }
                if !sb_address.contains(&(buf_pa + buf_size - 1)) {
                    return RecCommandReturnCode::InvalidValue.into();
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
                            Err(_) => return RecCommandReturnCode::Unk.into(),
                        }
                    }
                };

                RecAttestGetRealmKeyResponse {
                    key_size: key_size as u64,
                }
                .into()
            }
            // TODO(firme): equivalent to MFI_ATTEST_PAT_GET, will have to take into accoun the
            // write offset.
            RecCall::AttestGetPlatToken {
                buf_pa,
                buf_size,
                c_size,
            } => {
                let buf_pa = buf_pa as usize;
                let buf_size = buf_size as usize;
                let c_size = c_size as usize;

                // Perform sanity checks on the received buffer range.
                if !sb_address.contains(&buf_pa) {
                    return RecCommandReturnCode::BadAddress.into();
                }
                if !sb_address.contains(&(buf_pa + buf_size - 1)) {
                    return RecCommandReturnCode::InvalidValue.into();
                }

                // Safety:
                // - This function can only be reached after having setup the Realm World, which
                //   requires the MMU and pagetables to be setup.
                // - This function never calls again `get_shared_buffer()`, thus the reference will
                //   be dropped upon return, before another call is made.
                // - Similarly to the above, this function does not switch to the Realm World.
                let shared_buffer = unsafe { get_shared_buffer() };

                static INDEX: SpinMutex<usize> = SpinMutex::new(0);

                let mut idx = INDEX.lock();

                let write_res = if c_size == 0 {
                    PlatformImpl::write_attestation_token(shared_buffer, &[], *idx)
                } else {
                    let mut hash_print = [0; 256];
                    fn to_hex(val: u8) -> u8 {
                        char::from_digit(val as u32, 16)
                            .unwrap()
                            .try_into()
                            .unwrap()
                    }

                    for i in 0..c_size {
                        hash_print[2 * i] = to_hex(shared_buffer[i] >> 4);
                        hash_print[2 * i + 1] = to_hex(shared_buffer[i] & 0xF);
                    }

                    PlatformImpl::write_attestation_token(shared_buffer, &hash_print, *idx)
                };

                let Ok((size, rem)) = write_res else {
                    return RecCommandReturnCode::Unk.into();
                };

                *idx += size;

                RecAttestGetPlatTokenResponse {
                    token_hunk_size: size as u64,
                    remaining_size: rem as u64,
                }
                .into()
            }
            RecCall::El3Features { .. } => RecEl3FeaturesResponse { feat_reg: 0 }.into(),
            RecCall::El3TokenSign { .. } => todo!(),
            // Hacky trick to avoid TF-RMM from enabling encryption (not implemented yet).
            RecCall::MecRefresh { .. } => (NOT_SUPPORTED.into(), World::Realm),
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
        Self
    }
}

fn rmm_boot_complete(ret: i32) -> SmcReturn {
    ret.into()
}
